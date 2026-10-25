// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Schema34 reviewed, DB-only logical replacements. The durable dispatcher owns
//! the writer transaction, authority/replay checks, receipt-last write and commit.
//! This owner never rehomes a timeline/version or rewrites historical pins.
use crate::{
    Error as StoreError, Store,
    catalog_identity_mutation::{self, IdentityAuthority, IdentityMutation},
    migration::rows,
};
use catabolic_core::{
    catalog_command::CommandEnvelope,
    catalog_logical_edit::{ExactReference, ReferenceKind, ReferenceMap, ReferenceReplacement},
    encode, sha256,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const MAX_ROWS: usize = 8192;
const MAX_BYTES: usize = 2 * 1024 * 1024;
#[derive(Clone, Debug)]
pub struct LogicalEditAuthority {
    pub host_namespace: String,
    /// Exact existing logical/association item scope, including unchanged
    /// context members whose snapshots appear in the preview/receipt.
    pub item_ids: BTreeSet<String>,
    pub new_item_ids: BTreeSet<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LogicalChange {
    Merge {
        sources: Vec<String>,
        survivor: String,
    },
    Split {
        source: String,
        children: Vec<String>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewItem {
    pub id: String,
    pub kind: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssociationClone {
    pub source_association_id: String,
    pub new_association_id: String,
    pub new_owner_item_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionReplacement {
    pub version_id: String,
    pub expected_revision: i64,
    pub replacement_version_ids: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextReplacement {
    pub retired_versions: Vec<VersionReplacement>,
    pub mutation: IdentityMutation,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalEditRequest {
    pub contract_version: u32,
    pub change: LogicalChange,
    /// New child editions and dedicated association owners. Their metadata is
    /// explicitly empty. No source/provider metadata or pin is copied.
    pub new_items: Vec<NewItem>,
    /// Exact association copies preserve file, role, part, metadata and origin.
    /// Only association ID/owner change. Source records remain historical.
    pub association_clones: Vec<AssociationClone>,
    pub contexts: Vec<ContextReplacement>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewedLogicalEdit {
    pub request: LogicalEditRequest,
    pub catalog_id: String,
    pub incarnation: String,
    pub expected_generation: i64,
    pub affected_digest: String,
    pub reference_map: ReferenceMap,
}
#[derive(Clone, Debug, Serialize)]
pub struct LogicalEditPreview {
    pub reviewed: ReviewedLogicalEdit,
    /// Complete bounded rows used for validation, including unchanged context
    /// membership, metadata/pin history and relevant projections. Never truncated.
    pub affected_graph: BTreeMap<String, Vec<Value>>,
    pub disposition: Value,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogicalEditErrorCode {
    InvalidInput,
    AuthorityDenied,
    Conflict,
    Unsupported,
    StoreFailure,
}
#[derive(Debug)]
pub struct LogicalEditError {
    pub code: LogicalEditErrorCode,
    pub error: StoreError,
}
impl std::fmt::Display for LogicalEditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for LogicalEditError {}
impl From<StoreError> for LogicalEditError {
    fn from(error: StoreError) -> Self {
        Self {
            code: LogicalEditErrorCode::StoreFailure,
            error,
        }
    }
}
impl From<rusqlite::Error> for LogicalEditError {
    fn from(error: rusqlite::Error) -> Self {
        StoreError::from(error).into()
    }
}
impl From<serde_json::Error> for LogicalEditError {
    fn from(error: serde_json::Error) -> Self {
        StoreError::from(error).into()
    }
}
pub type Result<T> = std::result::Result<T, LogicalEditError>;
fn failure(code: LogicalEditErrorCode, value: &str) -> LogicalEditError {
    LogicalEditError {
        code,
        error: StoreError(value.into()),
    }
}
fn invalid(value: &str) -> LogicalEditError {
    failure(LogicalEditErrorCode::InvalidInput, value)
}
fn conflict(value: &str) -> LogicalEditError {
    failure(LogicalEditErrorCode::Conflict, value)
}
fn unsupported(value: &str) -> LogicalEditError {
    failure(LogicalEditErrorCode::Unsupported, value)
}
fn denied() -> LogicalEditError {
    failure(
        LogicalEditErrorCode::AuthorityDenied,
        "catalog_logical_authority_denied",
    )
}
fn identifier(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 256
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        Err(invalid("catalog_logical_invalid_identifier"))
    } else {
        Ok(())
    }
}
fn unique(values: &[String]) -> Result<BTreeSet<String>> {
    let set: BTreeSet<_> = values.iter().cloned().collect();
    for v in values {
        identifier(v)?;
    }
    if set.len() != values.len() {
        return Err(invalid("catalog_logical_duplicate_identifier"));
    }
    Ok(set)
}
fn table_exists(db: &Connection, table: &str) -> Result<bool> {
    Ok(db
        .prepare("SELECT 1 FROM sqlite_schema WHERE type IN ('table','view') AND name=?")?
        .exists([table])?)
}
fn get_rows(db: &Connection, sql: &str, args: &[String]) -> Result<Vec<Value>> {
    let args: Vec<rusqlite::types::Value> = args.iter().cloned().map(Into::into).collect();
    let records = rows(db, &format!("{sql} LIMIT {}", MAX_ROWS + 1), &args)?;
    if records.len() > MAX_ROWS {
        return Err(unsupported("catalog_logical_affected_graph_budget"));
    }
    Ok(records)
}
fn one(db: &Connection, table: &str, id: &str) -> Result<Value> {
    get_rows(
        db,
        &format!("SELECT * FROM {table} WHERE id=?"),
        &[id.into()],
    )?
    .pop()
    .ok_or_else(|| conflict("catalog_logical_missing_record"))
}
fn text<'a>(r: &'a Value, key: &str) -> Result<&'a str> {
    r[key]
        .as_str()
        .ok_or_else(|| invalid("catalog_logical_invalid_record"))
}
fn number(r: &Value, key: &str) -> Result<i64> {
    r[key]
        .as_i64()
        .ok_or_else(|| invalid("catalog_logical_invalid_record"))
}
fn state(db: &Connection) -> Result<(String, String, i64)> {
    Ok(db.query_row(
        "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?)
}
fn add(graph: &mut BTreeMap<String, Vec<Value>>, table: &str, records: Vec<Value>) -> Result<()> {
    let dest = graph.entry(table.into()).or_default();
    for row in records {
        if !dest.contains(&row) {
            dest.push(row);
        }
    }
    dest.sort_by_key(encode);
    if graph.values().map(Vec::len).sum::<usize>() > MAX_ROWS
        || encode(&json!(graph)).len() > MAX_BYTES
    {
        return Err(unsupported("catalog_logical_affected_graph_budget"));
    }
    Ok(())
}
fn exact(kind: ReferenceKind, id: &str, revision: i64) -> ExactReference {
    ExactReference {
        kind,
        id: id.into(),
        revision,
    }
}
fn add_map(
    maps: &mut BTreeMap<ExactReference, BTreeSet<ExactReference>>,
    source: ExactReference,
    target: ExactReference,
) {
    maps.entry(source).or_default().insert(target);
}

impl LogicalEditRequest {
    fn endpoints(&self) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
        if self.contract_version != 1 {
            return Err(unsupported("catalog_logical_unsupported_contract"));
        }
        let (sources, targets) = match &self.change {
            LogicalChange::Merge { sources, survivor }
                if !sources.is_empty() && sources.len() <= 16 =>
            {
                (unique(sources)?, unique(std::slice::from_ref(survivor))?)
            }
            LogicalChange::Split { source, children } if (2..=16).contains(&children.len()) => {
                (unique(std::slice::from_ref(source))?, unique(children)?)
            }
            _ => return Err(invalid("catalog_logical_invalid_change")),
        };
        if !sources.is_disjoint(&targets)
            || self.new_items.len() > 128
            || self.contexts.len() > 16
            || self.association_clones.len() > 1024
        {
            return Err(invalid("catalog_logical_request_budget"));
        }
        let mut new = BTreeSet::new();
        for item in &self.new_items {
            identifier(&item.id)?;
            identifier(&item.kind)?;
            if !new.insert(&item.id) {
                return Err(invalid("catalog_logical_duplicate_new_item"));
            }
        }
        Ok((sources, targets))
    }
}

/// Read-only preview. The coarse generation is checked before and after all
/// reads, so a concurrently introduced related row cannot escape the graph.
pub fn preview_logical_edit(
    store: &Store,
    request: LogicalEditRequest,
    authority: &LogicalEditAuthority,
    identity_authority: Option<&IdentityAuthority>,
) -> Result<LogicalEditPreview> {
    store.transaction_available()?;
    if !store.db.is_autocommit() {
        return Err(unsupported("catalog_logical_ambient_transaction"));
    }
    let preview = preview_logical_edit_inner(store, request, authority, identity_authority)?;
    store.transaction_available()?;
    let after = state(&store.db)?;
    if after.0 != preview.reviewed.catalog_id
        || after.1 != preview.reviewed.incarnation
        || after.2 != preview.reviewed.expected_generation
    {
        return Err(conflict("catalog_logical_stale_preview"));
    }
    Ok(preview)
}

// Called only inside this owner's dispatcher-controlled writer transaction or
// by the lifecycle-fenced public wrapper above. Never a public ambient read API.
fn preview_logical_edit_inner(
    store: &Store,
    request: LogicalEditRequest,
    authority: &LogicalEditAuthority,
    identity_authority: Option<&IdentityAuthority>,
) -> Result<LogicalEditPreview> {
    if store.schema_version < 34 || !table_exists(&store.db, "catalog_logical_retirements")? {
        return Err(unsupported("catalog_logical_schema34_required"));
    }
    let db = &store.db;
    let before = state(db)?;
    let (sources, targets) = request.endpoints()?;
    identifier(&authority.host_namespace)?;
    let new: BTreeMap<_, _> = request
        .new_items
        .iter()
        .map(|v| (v.id.clone(), v))
        .collect();
    let mut item_ids = sources.clone();
    if matches!(request.change, LogicalChange::Merge { .. }) {
        item_ids.extend(targets.iter().cloned());
    }
    for item in new.keys() {
        if !authority.new_item_ids.contains(item) {
            return Err(denied());
        }
        if db
            .prepare("SELECT 1 FROM items WHERE id=?")?
            .exists([item])?
        {
            return Err(conflict("catalog_logical_new_item_exists"));
        }
    }
    if matches!(&request.change, LogicalChange::Split { .. })
        && !targets.iter().all(|v| new.contains_key(v))
    {
        return Err(invalid("catalog_logical_missing_child"));
    }
    if matches!(&request.change, LogicalChange::Merge { .. })
        && targets.iter().any(|v| new.contains_key(v))
    {
        return Err(invalid("catalog_logical_survivor_must_exist"));
    }
    let mut graph = BTreeMap::new();
    let mut retired = BTreeMap::<String, (i64, String)>::new();
    let mut replacement_versions = BTreeMap::new();
    let mut used_new = targets
        .iter()
        .filter(|v| new.contains_key(*v))
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut clone_targets = BTreeMap::new();
    for clone in &request.association_clones {
        for id in [
            &clone.source_association_id,
            &clone.new_association_id,
            &clone.new_owner_item_id,
        ] {
            identifier(id)?;
        }
        if clone_targets
            .insert(clone.new_association_id.clone(), clone)
            .is_some()
            || !new.contains_key(&clone.new_owner_item_id)
        {
            return Err(invalid("catalog_logical_invalid_clone"));
        }
        if db
            .prepare("SELECT 1 FROM item_files WHERE id=?")?
            .exists([&clone.new_association_id])?
        {
            return Err(conflict("catalog_logical_clone_id_exists"));
        }
    }
    let mut contexts = BTreeSet::new();
    let mut maps = BTreeMap::new();
    let mut timelines = BTreeSet::new();
    let mut referenced_clones = BTreeSet::new();
    for context in &request.contexts {
        let m = &context.mutation;
        m.validate().map_err(|e| invalid(&e.error.0))?;
        let ia = identity_authority.ok_or_else(denied)?;
        if ia.authority_namespace != authority.host_namespace
            || m.context.authority_namespace != ia.authority_namespace
            || !ia.context_keys.contains(&m.context.context_key)
        {
            return Err(denied());
        }
        if !contexts.insert(m.context.id.clone())
            || context.retired_versions.is_empty()
            || context.retired_versions.len() > 64
        {
            return Err(invalid("catalog_logical_invalid_context_set"));
        }
        for basis in m.evidence.iter().map(|e| e.basis).chain(
            m.versions
                .iter()
                .filter_map(|v| v.equivalence.as_ref().map(|e| e.basis)),
        ) {
            if !match basis {
                catabolic_core::catalog_identity::EquivalenceBasis::Declared => {
                    ia.allow_declared_evidence
                }
                catabolic_core::catalog_identity::EquivalenceBasis::Verified => {
                    ia.allow_verified_evidence
                }
            } {
                return Err(denied());
            }
        }
        let old = one(db, "catalog_segmentation_contexts", &m.context.id)?;
        if old["authority_namespace"] != m.context.authority_namespace
            || old["context_key"] != m.context.context_key
        {
            return Err(denied());
        }
        if old["current_revision"].as_i64() != m.context.expected_revision {
            return Err(conflict("catalog_logical_stale_context"));
        }
        add(&mut graph, "catalog_segmentation_contexts", vec![old])?;
        for table in [
            "catalog_segmentation_revisions",
            "catalog_segmentation_sets",
            "catalog_segment_members",
        ] {
            add(
                &mut graph,
                table,
                get_rows(
                    db,
                    &format!("SELECT * FROM {table} WHERE context_id=?"),
                    std::slice::from_ref(&m.context.id),
                )?,
            )?;
        }
        let members = get_rows(
            db,
            "SELECT * FROM catalog_live_versions WHERE segmentation_context_id=?",
            std::slice::from_ref(&m.context.id),
        )?;
        for member in &members {
            item_ids.insert(text(member, "association_item_id")?.into());
            let vid = text(member, "id")?;
            for table in [
                "catalog_version_revisions",
                "catalog_version_bindings",
                "catalog_bindings",
            ] {
                add(
                    &mut graph,
                    table,
                    get_rows(
                        db,
                        &format!("SELECT * FROM {table} WHERE version_id=?"),
                        &[vid.into()],
                    )?,
                )?;
            }
            for row in get_rows(
                db,
                "SELECT r.* FROM catalog_binding_revisions r JOIN catalog_bindings b ON b.id=r.binding_id WHERE b.version_id=?",
                &[vid.into()],
            )? {
                add(&mut graph, "catalog_binding_revisions", vec![row])?;
            }
            let tr = get_rows(
                db,
                "SELECT t.* FROM catalog_timelines t JOIN catalog_version_revisions r ON r.timeline_id=t.id WHERE r.version_id=?",
                &[vid.into()],
            )?;
            for t in tr {
                item_ids.insert(text(&t, "edition_item_id")?.into());
                timelines.insert(text(&t, "id")?.to_owned());
                add(&mut graph, "catalog_timelines", vec![t])?;
            }
        }
        add(&mut graph, "catalog_versions", members.clone())?;
        for r in &context.retired_versions {
            identifier(&r.version_id)?;
            let old = members
                .iter()
                .find(|v| v["id"] == r.version_id)
                .ok_or_else(|| conflict("catalog_logical_missing_context_version"))?;
            if old["current_revision"].as_i64() != Some(r.expected_revision)
                || retired
                    .insert(
                        r.version_id.clone(),
                        (r.expected_revision, m.context.id.clone()),
                    )
                    .is_some()
            {
                return Err(conflict("catalog_logical_stale_version"));
            }
            let old_v=get_rows(db,"SELECT r.*,t.edition_item_id FROM catalog_version_revisions r JOIN catalog_timelines t ON t.id=r.timeline_id WHERE r.version_id=? AND r.revision=(SELECT current_revision FROM catalog_versions WHERE id=?)",&[r.version_id.clone(),r.version_id.clone()])?.pop().ok_or_else(||conflict("catalog_logical_missing_version"))?;
            if !sources.contains(text(&old_v, "edition_item_id")?) {
                return Err(invalid("catalog_logical_unrelated_retirement"));
            }
            let new_ids = unique(&r.replacement_version_ids)?;
            if new_ids.is_empty() {
                return Err(invalid("catalog_logical_missing_replacement"));
            }
            for new_id in new_ids {
                let change = m
                    .versions
                    .iter()
                    .find(|c| c.version.version_id == new_id)
                    .ok_or_else(|| invalid("catalog_logical_missing_replacement"))?;
                if replacement_versions
                    .insert(new_id.clone(), r.version_id.clone())
                    .is_some()
                {
                    return Err(invalid("catalog_logical_ambiguous_version_mapping"));
                }
                let v = &change.version;
                if !targets.contains(&v.edition_item_id)
                    || change.expected_revision.is_some()
                    || change
                        .expected_binding_revisions
                        .values()
                        .any(Option::is_some)
                    || !new.contains_key(&v.association_item_id)
                    || !used_new.insert(v.association_item_id.clone())
                {
                    return Err(invalid("catalog_logical_replacement_owner_must_be_new"));
                }
                if db
                    .prepare("SELECT 1 FROM catalog_versions WHERE id=?")?
                    .exists([&v.version_id])?
                {
                    return Err(conflict("catalog_logical_replacement_version_exists"));
                }
                let t = m
                    .timelines
                    .iter()
                    .find(|t| t.timeline_id == v.timeline_id)
                    .ok_or_else(|| invalid("catalog_logical_replacement_timeline_must_be_new"))?;
                if t.expected_revision.is_some()
                    || t.edition_item_id != v.edition_item_id
                    || change.expected_timeline_revision != 1
                    || db
                        .prepare("SELECT 1 FROM catalog_timelines WHERE id=?")?
                        .exists([&t.timeline_id])?
                {
                    return Err(invalid("catalog_logical_replacement_timeline_must_be_new"));
                }
                add_map(
                    &mut maps,
                    exact(ReferenceKind::Version, &r.version_id, r.expected_revision),
                    exact(ReferenceKind::Version, &v.version_id, 1),
                );
                add_map(
                    &mut maps,
                    exact(
                        ReferenceKind::Timeline,
                        text(&old_v, "timeline_id")?,
                        number(&old_v, "timeline_revision")?,
                    ),
                    exact(ReferenceKind::Timeline, &v.timeline_id, 1),
                );
                add_map(
                    &mut maps,
                    exact(
                        ReferenceKind::Item,
                        text(old, "association_item_id")?,
                        entity_revision(db, "items", text(old, "association_item_id")?)?,
                    ),
                    exact(ReferenceKind::Item, &v.association_item_id, 1),
                );
                for binding in &v.bindings {
                    let clone = clone_targets
                        .get(&binding.association_id)
                        .ok_or_else(|| invalid("catalog_logical_binding_clone_missing"))?;
                    if clone.new_owner_item_id != v.association_item_id
                        || !referenced_clones.insert(clone.new_association_id.clone())
                    {
                        return Err(invalid("catalog_logical_clone_owner_mismatch"));
                    }
                    let source = one(db, "item_files", &clone.source_association_id)?;
                    if source["item_id"] != old["association_item_id"]
                        || source["file_id"] != binding.file.file_id
                        || source["part"].as_u64() != Some(binding.part as u64)
                        || source["role"] != "primary"
                        || source["active"].as_i64() != Some(1)
                    {
                        return Err(invalid("catalog_logical_clone_source_mismatch"));
                    }
                    let source_binding=get_rows(db,"SELECT br.* FROM catalog_bindings b JOIN catalog_version_bindings vb ON vb.binding_id=b.id JOIN catalog_binding_revisions br ON br.binding_id=vb.binding_id AND br.revision=vb.binding_revision WHERE b.association_id=? AND vb.version_id=? AND vb.version_revision=?",&[clone.source_association_id.clone(),r.version_id.clone(),r.expected_revision.to_string()])?.pop().ok_or_else(||invalid("catalog_logical_unmapped_source_binding"))?;
                    if source_binding["file_revision"] != binding.file.revision {
                        return Err(conflict("catalog_logical_clone_file_revision_mismatch"));
                    }
                    add_map(
                        &mut maps,
                        exact(
                            ReferenceKind::Binding,
                            text(&source_binding, "binding_id")?,
                            number(&source_binding, "revision")?,
                        ),
                        exact(ReferenceKind::Binding, &binding.binding_id, 1),
                    );
                    add_map(
                        &mut maps,
                        exact(
                            ReferenceKind::Association,
                            &clone.source_association_id,
                            entity_revision(db, "item_files", &clone.source_association_id)?,
                        ),
                        exact(ReferenceKind::Association, &clone.new_association_id, 1),
                    );
                }
            }
        }
        if m.versions.len()
            != context
                .retired_versions
                .iter()
                .map(|r| r.replacement_version_ids.len())
                .sum::<usize>()
            || m.timelines.iter().any(|t| {
                !m.versions
                    .iter()
                    .any(|v| v.version.timeline_id == t.timeline_id)
            })
        {
            return Err(invalid("catalog_logical_extraneous_replacement"));
        }
    }
    if used_new != new.keys().cloned().collect()
        || referenced_clones != clone_targets.keys().cloned().collect()
    {
        return Err(invalid("catalog_logical_unused_new_records"));
    }
    // A survivor can already own versions in another context. Those unchanged
    // contexts remain part of the affected logical graph and require their own
    // current read scope; a merge never hides them or assumes equivalence.
    for target in &targets {
        if new.contains_key(target) {
            continue;
        }
        for row in get_rows(
            db,
            "SELECT DISTINCT v.segmentation_context_id FROM catalog_live_versions v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision JOIN catalog_timelines t ON t.id=r.timeline_id WHERE t.edition_item_id=?",
            std::slice::from_ref(target),
        )? {
            let context_id = text(&row, "segmentation_context_id")?;
            if !contexts.contains(context_id) {
                snapshot_unchanged_context(
                    db,
                    context_id,
                    authority,
                    identity_authority,
                    &mut graph,
                    &mut item_ids,
                    &mut timelines,
                )?;
                contexts.insert(context_id.into());
                if contexts.len() > 16 {
                    return Err(unsupported("catalog_logical_affected_context_budget"));
                }
            }
        }
    }
    // Source-owned active versions must be exhaustively retired, including any
    // newly introduced version that an earlier preview could not have seen.
    let mut expected_retirements = BTreeSet::new();
    for source in &sources {
        if db
            .prepare("SELECT 1 FROM catalog_versions WHERE association_item_id=?")?
            .exists([source])?
        {
            return Err(unsupported("catalog_logical_source_is_version_owner"));
        }
        for row in get_rows(
            db,
            "SELECT v.id FROM catalog_live_versions v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision JOIN catalog_timelines t ON t.id=r.timeline_id WHERE t.edition_item_id=?",
            std::slice::from_ref(source),
        )? {
            expected_retirements.insert(text(&row, "id")?.to_owned());
        }
        for t in get_rows(
            db,
            "SELECT * FROM catalog_timelines WHERE edition_item_id=?",
            std::slice::from_ref(source),
        )? {
            timelines.insert(text(&t, "id")?.into());
            add(&mut graph, "catalog_timelines", vec![t])?;
        }
        if db
            .prepare("SELECT 1 FROM item_files WHERE item_id=? AND active=1")?
            .exists([source])?
        {
            return Err(unsupported(
                "catalog_logical_untyped_associations_require_explicit_policy",
            ));
        }
    }
    if expected_retirements != retired.keys().cloned().collect() {
        return Err(conflict("catalog_logical_incomplete_version_membership"));
    }
    // Existing relationship/publication meaning cannot be guessed from an alias.
    // A later contract must provide explicit relationship/mapping replacements.
    let mut retiring_items = sources.clone();
    for vid in retired.keys() {
        retiring_items
            .insert(text(&one(db, "catalog_versions", vid)?, "association_item_id")?.into());
    }
    for item in &retiring_items {
        for (table, predicate) in [
            (
                "item_relationships",
                "(source_id=?1 OR target_id=?1) AND active=1",
            ),
            ("mappings", "item_id=?1 AND active=1"),
            ("expected_sets", "collection_id=?1"),
        ] {
            if db
                .prepare(&format!("SELECT 1 FROM {table} WHERE {predicate}"))?
                .exists([item])?
            {
                return Err(unsupported(
                    "catalog_logical_related_records_require_explicit_policy",
                ));
            }
        }
    }
    let mut kind = None;
    for item in &item_ids {
        if !authority.item_ids.contains(item) {
            return Err(denied());
        }
        if !db
            .prepare("SELECT 1 FROM catalog_live_items WHERE id=?")?
            .exists([item])?
        {
            return Err(conflict("catalog_logical_item_retired_or_missing"));
        }
        let row = one(db, "items", item)?;
        if sources.contains(item) || targets.contains(item) {
            let actual = text(&row, "kind")?.to_owned();
            if kind.as_ref().is_some_and(|k| k != &actual) {
                return Err(invalid("catalog_logical_kind_mismatch"));
            }
            kind = Some(actual);
        }
        add(&mut graph, "items", vec![row])?;
        for table in [
            "identities",
            "item_files",
            "catalog_logical_retirements",
            "catalog_metadata_subjects",
            "catalog_metadata_history",
            "catalog_metadata_sources",
            "catalog_metadata_projection",
            "catalog_identity_pins",
            "catalog_identity_pin_revisions",
            "catalog_legacy_identity_evidence",
            "catalog_legacy_item_metadata",
            "item_tags",
            "catalog_item_tag_suppressions",
        ] {
            if table_exists(db, table)? {
                add(
                    &mut graph,
                    table,
                    get_rows(
                        db,
                        &format!("SELECT * FROM {table} WHERE item_id=?"),
                        std::slice::from_ref(item),
                    )?,
                )?;
            }
        }
        if table_exists(db, "catalog_tag_suppression_revisions")? {
            add(
                &mut graph,
                "catalog_tag_suppression_revisions",
                get_rows(
                    db,
                    "SELECT * FROM catalog_tag_suppression_revisions WHERE subject_type='item' AND subject_id=?",
                    std::slice::from_ref(item),
                )?,
            )?;
        }
        for (table, predicate) in [
            ("item_relationships", "source_id=?1 OR target_id=?1"),
            ("mappings", "item_id=?1"),
            ("expected_sets", "collection_id=?1"),
        ] {
            add(
                &mut graph,
                table,
                get_rows(
                    db,
                    &format!("SELECT * FROM {table} WHERE {predicate}"),
                    std::slice::from_ref(item),
                )?,
            )?;
        }
        add(
            &mut graph,
            "catalog_entity_revisions",
            get_rows(
                db,
                "SELECT * FROM catalog_entity_revisions WHERE entity_kind='items' AND entity_key=json_array(?)",
                std::slice::from_ref(item),
            )?,
        )?;
    }
    for target in &targets {
        if let Some(item) = new.get(target)
            && kind.as_ref() != Some(&item.kind)
        {
            return Err(invalid("catalog_logical_kind_mismatch"));
        }
    }
    for t in &timelines {
        add(
            &mut graph,
            "catalog_timeline_revisions",
            get_rows(
                db,
                "SELECT * FROM catalog_timeline_revisions WHERE timeline_id=?",
                std::slice::from_ref(t),
            )?,
        )?;
    }
    // Exact retained file snapshots and equivalence evidence are part of the
    // graph; their immutable values are never repurposed for new timelines.
    let binding_rows = graph
        .get("catalog_binding_revisions")
        .cloned()
        .unwrap_or_default();
    for row in &binding_rows {
        add(
            &mut graph,
            "catalog_exact_file_revisions",
            get_rows(
                db,
                "SELECT * FROM catalog_exact_file_revisions WHERE profile=? AND file_id=? AND revision=?",
                &[
                    text(row, "profile")?.into(),
                    text(row, "file_id")?.into(),
                    text(row, "file_revision")?.into(),
                ],
            )?,
        )?;
    }
    let version_rows = graph
        .get("catalog_version_revisions")
        .cloned()
        .unwrap_or_default();
    for row in version_rows {
        if let Some(e) = row["equivalence_evidence_id"].as_str() {
            add(
                &mut graph,
                "catalog_identity_evidence",
                get_rows(
                    db,
                    "SELECT * FROM catalog_identity_evidence WHERE id=?",
                    &[e.into()],
                )?,
            )?;
        }
    }
    let associations = graph.get("item_files").cloned().unwrap_or_default();
    for row in &associations {
        let id = text(row, "id")?;
        add(
            &mut graph,
            "catalog_entity_revisions",
            get_rows(
                db,
                "SELECT * FROM catalog_entity_revisions WHERE entity_kind='item_files' AND entity_key=json_array(?)",
                &[id.into()],
            )?,
        )?;
        add(
            &mut graph,
            "catalog_legacy_associations",
            get_rows(
                db,
                "SELECT * FROM catalog_legacy_associations WHERE association_id=?",
                &[id.into()],
            )?,
        )?;
    }
    for (version, (revision, _)) in &retired {
        let members = get_rows(
            db,
            "SELECT binding_id,binding_revision FROM catalog_version_bindings WHERE version_id=? AND version_revision=?",
            &[version.clone(), revision.to_string()],
        )?;
        for member in members {
            let reference = exact(
                ReferenceKind::Binding,
                text(&member, "binding_id")?,
                number(&member, "binding_revision")?,
            );
            if !maps.contains_key(&reference) {
                return Err(invalid("catalog_logical_incomplete_binding_mapping"));
            }
        }
    }
    validate_binding_partitions(&request, &maps, &binding_rows)?;
    for source in &sources {
        for target in &targets {
            add_map(
                &mut maps,
                exact(
                    ReferenceKind::Item,
                    source,
                    entity_revision(db, "items", source)?,
                ),
                exact(
                    ReferenceKind::Item,
                    target,
                    if new.contains_key(target) {
                        1
                    } else {
                        entity_revision(db, "items", target)?
                    },
                ),
            );
        }
    }
    let reference_map = ReferenceMap {
        contract_version: 1,
        retain_historical_references: true,
        implies_timeline_equivalence: false,
        entries: maps
            .into_iter()
            .map(|(source, replacements)| ReferenceReplacement {
                source,
                replacements: replacements.into_iter().collect(),
            })
            .collect(),
    };
    reference_map.validate().map_err(invalid)?;
    let after = state(db)?;
    if before != after {
        return Err(conflict("catalog_logical_stale_preview"));
    }
    let disposition = json!({"retained_historical_items":retiring_items,"retired_versions":retired.keys().collect::<Vec<_>>(),"new_items":request.new_items,"cloned_associations":request.association_clones,"metadata":"retained_on_original_ids_no_copy","identity_pins":"retained_on_original_ids_no_remap","legacy_identities":"retained_as_history","timeline_equivalence":"not_implied","relationships_and_publication_mappings":"no_active_records_on_retiring_items"});
    Ok(LogicalEditPreview {
        reviewed: ReviewedLogicalEdit {
            request,
            catalog_id: before.0,
            incarnation: before.1,
            expected_generation: before.2,
            affected_digest: sha256(encode(&json!(graph))),
            reference_map,
        },
        affected_graph: graph,
        disposition,
    })
}

fn snapshot_unchanged_context(
    db: &Connection,
    context_id: &str,
    authority: &LogicalEditAuthority,
    identity_authority: Option<&IdentityAuthority>,
    graph: &mut BTreeMap<String, Vec<Value>>,
    item_ids: &mut BTreeSet<String>,
    timelines: &mut BTreeSet<String>,
) -> Result<()> {
    let row = one(db, "catalog_segmentation_contexts", context_id)?;
    let grant = identity_authority.ok_or_else(denied)?;
    if grant.authority_namespace != authority.host_namespace
        || row["authority_namespace"] != grant.authority_namespace
        || !grant.context_keys.contains(text(&row, "context_key")?)
    {
        return Err(denied());
    }
    add(graph, "catalog_segmentation_contexts", vec![row])?;
    for table in [
        "catalog_segmentation_revisions",
        "catalog_segmentation_sets",
        "catalog_segment_members",
    ] {
        add(
            graph,
            table,
            get_rows(
                db,
                &format!("SELECT * FROM {table} WHERE context_id=?"),
                &[context_id.into()],
            )?,
        )?;
    }
    let versions = get_rows(
        db,
        "SELECT * FROM catalog_live_versions WHERE segmentation_context_id=?",
        &[context_id.into()],
    )?;
    for version in &versions {
        item_ids.insert(text(version, "association_item_id")?.into());
        let vid = text(version, "id")?;
        for table in [
            "catalog_version_revisions",
            "catalog_version_bindings",
            "catalog_bindings",
        ] {
            add(
                graph,
                table,
                get_rows(
                    db,
                    &format!("SELECT * FROM {table} WHERE version_id=?"),
                    &[vid.into()],
                )?,
            )?;
        }
        add(
            graph,
            "catalog_binding_revisions",
            get_rows(
                db,
                "SELECT r.* FROM catalog_binding_revisions r JOIN catalog_bindings b ON b.id=r.binding_id WHERE b.version_id=?",
                &[vid.into()],
            )?,
        )?;
        for timeline in get_rows(
            db,
            "SELECT t.* FROM catalog_timelines t JOIN catalog_version_revisions r ON r.timeline_id=t.id WHERE r.version_id=?",
            &[vid.into()],
        )? {
            timelines.insert(text(&timeline, "id")?.into());
            item_ids.insert(text(&timeline, "edition_item_id")?.into());
            add(graph, "catalog_timelines", vec![timeline])?;
        }
    }
    add(graph, "catalog_versions", versions)
}

fn entity_revision(db: &Connection, kind: &str, id: &str) -> Result<i64> {
    let value:Option<i64>=db.query_row("SELECT revision FROM catalog_entity_revisions WHERE entity_kind=? AND entity_key=json_array(?) AND deleted=0",params![kind,id],|r|r.get(0)).optional()?;
    value
        .filter(|v| *v > 0)
        .ok_or_else(|| conflict("catalog_logical_missing_entity_revision"))
}

fn validate_binding_partitions(
    request: &LogicalEditRequest,
    maps: &BTreeMap<ExactReference, BTreeSet<ExactReference>>,
    old_rows: &[Value],
) -> Result<()> {
    use catabolic_core::catalog_identity::SourceExtent;
    for context in &request.contexts {
        for retiring in &context.retired_versions {
            // The original current version membership is checked separately through
            // clone source ownership. Every bound association must get exact coverage.
            let replacement_bindings: Vec<_> = context
                .mutation
                .versions
                .iter()
                .filter(|v| {
                    retiring
                        .replacement_version_ids
                        .contains(&v.version.version_id)
                })
                .flat_map(|v| v.version.bindings.iter())
                .collect();
            for (source, targets) in maps
                .iter()
                .filter(|(s, _)| s.kind == ReferenceKind::Binding)
            {
                let replacements: Vec<_> = replacement_bindings
                    .iter()
                    .filter(|b| targets.iter().any(|t| t.id == b.binding_id))
                    .collect();
                if replacements.is_empty() {
                    continue;
                }
                let old = old_rows
                    .iter()
                    .find(|r| {
                        r["binding_id"] == source.id
                            && r["revision"].as_i64() == Some(source.revision)
                    })
                    .ok_or_else(|| invalid("catalog_logical_missing_binding_snapshot"))?;
                if old["source_start_us"].is_null() {
                    if replacements.len() != 1
                        || replacements[0].source != (SourceExtent::WholeFile { duration_us: None })
                    {
                        return Err(invalid(
                            "catalog_logical_unknown_extent_requires_exact_copy",
                        ));
                    }
                } else {
                    let start = number(old, "source_start_us")?;
                    let end = number(old, "source_end_us")?;
                    let mut intervals = Vec::new();
                    for binding in replacements {
                        let interval = binding
                            .source
                            .interval()
                            .map_err(|_| invalid("catalog_logical_invalid_extent"))?
                            .ok_or_else(|| invalid("catalog_logical_unknown_replacement_extent"))?;
                        intervals.push((interval.start_us(), interval.end_us()));
                    }
                    intervals.sort();
                    let mut cursor = start;
                    for (a, b) in intervals {
                        if a != cursor || b > end {
                            return Err(invalid("catalog_logical_nonpartitioning_replacement"));
                        }
                        cursor = b;
                    }
                    if cursor != end {
                        return Err(invalid("catalog_logical_incomplete_replacement_coverage"));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Only the central durable dispatcher calls this seam. No independent commit,
/// receipt engine, arbitrary callback, network or filesystem effect exists here.
pub(crate) fn apply_logical_edit(
    store: &Store,
    reviewed: &ReviewedLogicalEdit,
    command: &CommandEnvelope,
    authority: &LogicalEditAuthority,
    identity_authority: Option<&IdentityAuthority>,
    now: i64,
) -> Result<Value> {
    apply_logical_edit_inner(
        store,
        reviewed,
        command,
        authority,
        identity_authority,
        now,
        #[cfg(test)]
        None,
    )
}
#[cfg(test)]
pub(crate) fn apply_logical_edit_with_probe(
    store: &Store,
    reviewed: &ReviewedLogicalEdit,
    command: &CommandEnvelope,
    authority: &LogicalEditAuthority,
    identity_authority: Option<&IdentityAuthority>,
    now: i64,
    probe: &mut dyn FnMut(&Store) -> crate::Result<()>,
) -> Result<Value> {
    apply_logical_edit_inner(
        store,
        reviewed,
        command,
        authority,
        identity_authority,
        now,
        Some(probe),
    )
}
fn apply_logical_edit_inner(
    store: &Store,
    reviewed: &ReviewedLogicalEdit,
    command: &CommandEnvelope,
    authority: &LogicalEditAuthority,
    identity_authority: Option<&IdentityAuthority>,
    now: i64,
    #[cfg(test)] mut probe: Option<&mut dyn FnMut(&Store) -> crate::Result<()>>,
) -> Result<Value> {
    if store.db.is_autocommit() {
        return Err(unsupported("catalog_logical_writer_transaction_required"));
    }
    if authority.host_namespace != command.key().host_namespace() {
        return Err(denied());
    }
    let current = preview_logical_edit_inner(
        store,
        reviewed.request.clone(),
        authority,
        identity_authority,
    )?;
    if command.key().catalog_id() != reviewed.catalog_id
        || command.admission_incarnation() != reviewed.incarnation
        || command.expected_generation() != reviewed.expected_generation
        || encode(&json!(current.reviewed)) != encode(&json!(reviewed))
    {
        return Err(conflict("catalog_logical_stale_review"));
    }
    let mut nfo_retirement = if store.schema_version == 41 {
        let (sources, _) = reviewed.request.endpoints()?;
        Some(crate::catalog_nfo_attribution::retirement::prepare_in(
            store, command, reviewed, &sources,
        )?)
    } else {
        None
    };
    let db = &store.db;
    db.execute(
        "INSERT INTO catalog_logical_edit_permits(operation_id,incarnation) VALUES(?,?)",
        params![command.key().operation_id(), reviewed.incarnation],
    )?;
    for item in &reviewed.request.new_items {
        crate::curation::item_put(store, &json!({"id":item.id,"kind":item.kind,"metadata":{}}))?;
    }
    for clone in &reviewed.request.association_clones {
        let count=db.execute("INSERT INTO item_files(id,file_id,item_id,role,part,metadata,origin,active) SELECT ?,file_id,?,role,part,metadata,origin,active FROM item_files WHERE id=?",params![clone.new_association_id,clone.new_owner_item_id,clone.source_association_id])?;
        if count != 1 {
            return Err(conflict("catalog_logical_clone_source_missing"));
        }
    }
    let mut replacements = Vec::new();
    for context in &reviewed.request.contexts {
        let retirements: Vec<_> = context
            .retired_versions
            .iter()
            .map(|r| (r.version_id.clone(), r.expected_revision))
            .collect();
        replacements.push(
            catalog_identity_mutation::apply_identity_replacement(
                store,
                &context.mutation,
                command,
                identity_authority.ok_or_else(denied)?,
                now,
                &retirements,
            )
            .map_err(|e| LogicalEditError {
                code: match e.code {
                    catalog_identity_mutation::IdentityErrorCode::InvalidInput => {
                        LogicalEditErrorCode::InvalidInput
                    }
                    catalog_identity_mutation::IdentityErrorCode::AuthorityDenied => {
                        LogicalEditErrorCode::AuthorityDenied
                    }
                    catalog_identity_mutation::IdentityErrorCode::Conflict => {
                        LogicalEditErrorCode::Conflict
                    }
                    catalog_identity_mutation::IdentityErrorCode::Unsupported => {
                        LogicalEditErrorCode::Unsupported
                    }
                    catalog_identity_mutation::IdentityErrorCode::StoreFailure => {
                        LogicalEditErrorCode::StoreFailure
                    }
                },
                error: e.error,
            })?,
        );
    }
    if let Some(nfo) = nfo_retirement.as_mut() {
        nfo.revalidate_before_retirement(store)?;
    }
    let (sources, _) = reviewed.request.endpoints()?;
    let (kind, survivor, children) = match &reviewed.request.change {
        LogicalChange::Merge { survivor, .. } => ("merge", Some(survivor.as_str()), json!([])),
        LogicalChange::Split { children, .. } => ("split", None, json!(children)),
    };
    for source in sources {
        db.execute(
            "INSERT INTO catalog_logical_retirements VALUES(?,?,?,?,?,?,?,?,?)",
            params![
                source,
                kind,
                survivor,
                encode(&children),
                encode(&json!(reviewed.reference_map)),
                command.key().catalog_id(),
                command.key().host_namespace(),
                command.key().operation_id(),
                now
            ],
        )?;
    }
    if let Some(nfo) = nfo_retirement {
        nfo.publish_in(store)?;
    }
    #[cfg(test)]
    if let Some(probe) = probe.as_deref_mut() {
        probe(store)?;
    }
    db.execute(
        "DELETE FROM catalog_logical_edit_permits WHERE operation_id=?",
        [command.key().operation_id()],
    )?;
    Ok(
        json!({"operation":"logical_edit","contract_version":1,"reviewed":reviewed,"disposition":current.disposition,"identity_results":replacements,"reference_map":reviewed.reference_map}),
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LogicalResolution {
    Active {
        item_id: String,
    },
    Merged {
        historical_item_id: String,
        item_id: String,
    },
    Split {
        historical_item_id: String,
        children: Vec<String>,
    },
}
/// Read-only logical resolution. Split never returns a chosen child. Retired
/// version/timeline references are not passed through this item-only resolver.
pub fn resolve_logical_item(
    store: &Store,
    item: &str,
    authority: &LogicalEditAuthority,
) -> Result<LogicalResolution> {
    store.transaction_available()?;
    if !store.db.is_autocommit() {
        return Err(unsupported("catalog_logical_ambient_transaction"));
    }
    identifier(item)?;
    if store.schema_version < 34 {
        return Err(unsupported("catalog_logical_schema34_required"));
    }
    let before = state(&store.db)?;
    let mut current = item.to_owned();
    let mut seen = BTreeSet::new();
    loop {
        if !authority.item_ids.contains(&current) {
            return Err(denied());
        }
        if !seen.insert(current.clone()) || seen.len() > 64 {
            return Err(unsupported("catalog_logical_alias_budget"));
        }
        let retirement = get_rows(
            &store.db,
            "SELECT * FROM catalog_logical_retirements WHERE item_id=?",
            std::slice::from_ref(&current),
        )?
        .pop();
        match retirement {
            None => {
                one(&store.db, "catalog_live_items", &current)?;
                store.transaction_available()?;
                if state(&store.db)? != before {
                    return Err(conflict("catalog_logical_stale_resolution"));
                }
                return Ok(if current == item {
                    LogicalResolution::Active { item_id: current }
                } else {
                    LogicalResolution::Merged {
                        historical_item_id: item.into(),
                        item_id: current,
                    }
                });
            }
            Some(row) => match text(&row, "kind")? {
                "merge" => current = text(&row, "survivor_item_id")?.into(),
                "split" => {
                    let children: Vec<String> = serde_json::from_str(text(&row, "children_json")?)?;
                    if !children.iter().all(|c| authority.item_ids.contains(c)) {
                        return Err(denied());
                    }
                    store.transaction_available()?;
                    if state(&store.db)? != before {
                        return Err(conflict("catalog_logical_stale_resolution"));
                    }
                    return Ok(LogicalResolution::Split {
                        historical_item_id: item.into(),
                        children,
                    });
                }
                _ => return Err(invalid("catalog_logical_invalid_retirement")),
            },
        }
    }
}

/// Import composition is already inside the dispatcher writer transaction. It
/// must execute the same exhaustive graph review and mutation as a normal merge;
/// only the generation-specific preview is omitted from the stable import map.
pub(crate) fn import_reviewed_alias(
    store: &Store,
    alias: &str,
    survivor: &str,
    command: &CommandEnvelope,
    authority: &LogicalEditAuthority,
    identity_authority: Option<&IdentityAuthority>,
    now: i64,
) -> Result<Value> {
    if store.db.is_autocommit() {
        return Err(unsupported("catalog_logical_writer_transaction_required"));
    }
    let preview = preview_logical_edit_inner(
        store,
        LogicalEditRequest {
            contract_version: 1,
            change: LogicalChange::Merge {
                sources: vec![alias.into()],
                survivor: survivor.into(),
            },
            new_items: vec![],
            association_clones: vec![],
            contexts: vec![],
        },
        authority,
        identity_authority,
    )?;
    apply_logical_edit(
        store,
        &preview.reviewed,
        command,
        authority,
        identity_authority,
        now,
    )
}
