//! Unregistered schema41 current-state reader. This reads only within an already
//! retained writer; it grants no curation, receipt disclosure or filesystem access.
//! Historical origin mappings remain provenance, never current byte authority.
use super::proof_budget::ProofBudget;
use crate::{
    Error, Result, Store, catalog_import_typed::PhysicalMapping,
    catalog_metadata_store::read_registered_schema, operation_budget::BudgetCharge,
};
use catabolic_core::{
    catalog_command::{COMMAND_CANONICALIZATION_VERSION, COMMAND_CONTRACT_VERSION, CommandKey},
    catalog_metadata::{
        ContributionDocument, ContributionKey, FieldSchema, MAX_METADATA_DOCUMENT_BYTES,
        MAX_METADATA_EVIDENCE_BYTES, MAX_METADATA_JSON_DEPTH, MAX_METADATA_JSON_NODES,
        MAX_METADATA_SNAPSHOT_BYTES, SourceKind,
        nfo_refresh::{
            AttributionHead, Contribution, DocumentHead, DocumentState, Fence, FileOrigin,
            OriginProvenance, PRESENCE_CONTRACT_VERSION, PinHead, PresenceSnapshot, SourcePresence,
            SubjectHead, SubjectState, public_nfo_document,
        },
    },
};
use rusqlite::{OptionalExtension, Row, params, types::ValueRef};
use std::collections::BTreeSet;

const NAMESPACE: &str = "motion.nfo";
const RECORD: &str = "document";

/// Nonserialized descriptive state, scoped to the current native incarnation and
/// generation. Only read_in constructs it. aggregate_revision is supplied by the
/// independently established Motion guard, NOT issued by this native reader.
#[derive(Debug)]
pub(super) struct CurrentNfoState {
    presence: PresenceSnapshot,
    native_source: Option<ContributionDocument>,
    schema: Option<FieldSchema>,
    origin_physical: Option<PhysicalMapping>,
    origin_proof_digest: Option<String>,
    source_proof_digest: Option<String>,
    attribution_audit_digest: Option<String>,
}
impl CurrentNfoState {
    pub(super) fn presence(&self) -> &PresenceSnapshot {
        &self.presence
    }
    pub(super) fn head(&self) -> &SubjectHead {
        &self.presence.head
    }
    pub(super) fn native_source(&self) -> Option<&ContributionDocument> {
        self.native_source.as_ref()
    }
    pub(super) fn schema(&self) -> Option<&FieldSchema> {
        self.schema.as_ref()
    }
    pub(super) fn origin_physical(&self) -> Option<&PhysicalMapping> {
        self.origin_physical.as_ref()
    }
    pub(super) fn origin_proof_digest(&self) -> Option<&str> {
        self.origin_proof_digest.as_deref()
    }
    pub(super) fn source_proof_digest(&self) -> Option<&str> {
        self.source_proof_digest.as_deref()
    }
    pub(super) fn attribution_audit_digest(&self) -> Option<&str> {
        self.attribution_audit_digest.as_deref()
    }
}
/// Only named owner tokens can bypass their exact just-written receipt links.
/// Retirement can never authorize a pending source/tag history row.
#[derive(Clone, Copy)]
enum PendingRead<'a> {
    Refresh(&'a super::refresh::RefreshReadPermit),
    Retirement(&'a super::retirement::PendingRetirementRead),
}
impl PendingRead<'_> {
    fn key(&self) -> &CommandKey {
        match self {
            Self::Refresh(p) => p.key(),
            Self::Retirement(p) => p.key(),
        }
    }
    fn authorize_source_read_in(
        &self,
        store: &Store,
        subject: &str,
        revision: i64,
        generation: i64,
        budget: &mut ProofBudget,
    ) -> Result<()> {
        match self {
            Self::Refresh(p) => {
                p.authorize_source_read_in(store, subject, revision, generation, budget)
            }
            Self::Retirement(_) => Err(refused("retirement cannot bypass source history receipt")),
        }
    }
    fn authorize_state_read_in(
        &self,
        store: &Store,
        subject: &str,
        revision: i64,
        source_revision: i64,
        generation: i64,
        budget: &mut ProofBudget,
    ) -> Result<()> {
        match self {
            Self::Refresh(p) => p.authorize_state_read_in(
                store,
                subject,
                revision,
                source_revision,
                generation,
                budget,
            ),
            Self::Retirement(p) => p.authorize_state_read_in(
                store,
                subject,
                revision,
                source_revision,
                generation,
                budget,
            ),
        }
    }
}

fn refused(reason: impl std::fmt::Display) -> Error {
    Error(format!("catalog_nfo_state_refused: {reason}"))
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.trim() == value
        && !value.chars().any(char::is_control)
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn charge(budget: &mut ProofBudget, bytes: usize, values: usize) -> Result<()> {
    budget.charge(BudgetCharge {
        bytes,
        values,
        work: bytes / 64 + values + 1,
        ..Default::default()
    })
}
fn borrowed_text<'a>(row: &'a Row<'_>, index: usize, maximum: usize) -> Result<&'a str> {
    match row.get_ref(index)? {
        ValueRef::Text(value) if value.len() <= maximum => {
            std::str::from_utf8(value).map_err(refused)
        }
        _ => Err(refused("missing, oversized or non-text stored value")),
    }
}
fn text(row: &Row<'_>, index: usize, maximum: usize, budget: &mut ProofBudget) -> Result<String> {
    let value = borrowed_text(row, index, maximum)?;
    charge(budget, value.len(), 1)?;
    Ok(value.to_owned())
}
fn optional_text(
    row: &Row<'_>,
    index: usize,
    maximum: usize,
    budget: &mut ProofBudget,
) -> Result<Option<String>> {
    if matches!(row.get_ref(index)?, ValueRef::Null) {
        charge(budget, 0, 1)?;
        Ok(None)
    } else {
        text(row, index, maximum, budget).map(Some)
    }
}
fn fence(store: &Store, expected_generation: i64, budget: &mut ProofBudget) -> Result<Fence> {
    if store.schema_version != 41
        || !store.writable
        || store.db.is_autocommit()
        || expected_generation < 0
    {
        return Err(refused("schema41 retained writer required"));
    }
    store.transaction_available()?;
    charge(budget, 0, 3)?;
    let mut query = store
        .db
        .prepare("SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1")?;
    let mut rows = query.query([])?;
    let row = rows
        .next()?
        .ok_or_else(|| refused("catalog fence absent"))?;
    let catalog = text(row, 0, 256, budget)?;
    let incarnation = text(row, 1, 256, budget)?;
    let generation: i64 = row.get(2)?;
    if catalog != store.database_id
        || Some(incarnation.as_str()) != store.catalog_incarnation.as_deref()
        || generation != expected_generation
    {
        return Err(refused("current catalog/incarnation/generation differs"));
    }
    Ok(Fence {
        catalog_id: catalog,
        incarnation,
        generation,
    })
}

/// Historical receipt linkage does not recover its result or confer old-scope
/// authority. Restore may change the current incarnation without rewriting this
/// immutable record, so only an explicitly supplied historical incarnation is
/// compared here; the reader's own fence is checked separately.
fn receipt_link(
    store: &Store,
    key: &CommandKey,
    historical_incarnation: Option<&str>,
    generation: i64,
    budget: &mut ProofBudget,
) -> Result<()> {
    charge(budget, 0, 10)?;
    let mut query=store.db.prepare("SELECT contract_version,canonicalization_version,admission_incarnation,expected_generation,committed_generation,committed_at_us,retain_until_us,result_state,expired_at_us,result_json IS NULL FROM catalog_command_receipts WHERE catalog_id=? AND host_namespace=? AND operation_id=?")?;
    let mut rows = query.query(params![
        key.catalog_id(),
        key.host_namespace(),
        key.operation_id()
    ])?;
    let row = rows
        .next()?
        .ok_or_else(|| refused("state command receipt absent"))?;
    let contract: i64 = row.get(0)?;
    let canonical: i64 = row.get(1)?;
    let incarnation = text(row, 2, 256, budget)?;
    let expected: i64 = row.get(3)?;
    let committed: i64 = row.get(4)?;
    let at: i64 = row.get(5)?;
    let until: i64 = row.get(6)?;
    let state = text(row, 7, 16, budget)?;
    let expired: Option<i64> = row.get(8)?;
    let absent: bool = row.get(9)?;
    let valid = match state.as_str() {
        "full" => !absent && expired.is_none(),
        "expired" => absent && expired.is_some_and(|v| v >= until),
        _ => false,
    };
    if key.catalog_id() != store.database_id
        || contract != i64::from(COMMAND_CONTRACT_VERSION)
        || canonical != i64::from(COMMAND_CANONICALIZATION_VERSION)
        || !identifier(&incarnation)
        || historical_incarnation.is_some_and(|v| v != incarnation)
        || expected < 0
        || committed < expected
        || committed > generation
        || at < 0
        || until < at
        || !valid
    {
        return Err(refused("invalid historical state receipt linkage"));
    }
    Ok(())
}
fn bootstrap_complete(store: &Store, generation: i64, budget: &mut ProofBudget) -> Result<()> {
    charge(budget, 0, 6)?;
    let mut query=store.db.prepare("SELECT catalog_id,incarnation,inventory_digest,source_count,command_catalog_id,command_host_namespace,command_operation_id FROM catalog_nfo_bootstrap WHERE id=1")?;
    let mut rows = query.query([])?;
    let row = rows
        .next()?
        .ok_or_else(|| refused("NFO bootstrap is incomplete"))?;
    let catalog = text(row, 0, 256, budget)?;
    let incarnation = text(row, 1, 256, budget)?;
    let inventory = text(row, 2, 64, budget)?;
    let count: i64 = row.get(3)?;
    let key = CommandKey::new(
        text(row, 4, 256, budget)?,
        text(row, 5, 256, budget)?,
        text(row, 6, 256, budget)?,
    )
    .map_err(refused)?;
    if catalog != store.database_id || !identifier(&incarnation) || !digest(&inventory) || count < 0
    {
        return Err(refused("invalid retained bootstrap completion"));
    }
    receipt_link(store, &key, Some(&incarnation), generation, budget)
}
fn entity_revision(
    store: &Store,
    kind: &str,
    subject: &str,
    budget: &mut ProofBudget,
) -> Result<i64> {
    charge(budget, 0, 2)?;
    let value:Option<(i64,bool)>=store.db.query_row("SELECT revision,deleted FROM catalog_entity_revisions WHERE entity_kind=? AND entity_key=json_array(?)",params![kind,subject],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    match value {
        Some((revision, false)) if revision > 0 => Ok(revision),
        _ => Err(refused("exact live entity revision absent")),
    }
}
pub(super) fn subject_state(
    store: &Store,
    subject: &str,
    budget: &mut ProofBudget,
) -> Result<SubjectState> {
    charge(budget, 0, 2)?;
    let exists: bool = store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM items WHERE id=?)",
        [subject],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(refused("subject item absent"));
    }
    let item_revision = entity_revision(store, "items", subject, budget)?;
    let retired: bool = store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id=?)",
        [subject],
        |r| r.get(0),
    )?;
    Ok(if retired {
        SubjectState::Retired {
            item_revision,
            retirement_revision: entity_revision(
                store,
                "catalog_logical_retirements",
                subject,
                budget,
            )?,
        }
    } else {
        SubjectState::Active { item_revision }
    })
}
fn pin(store: &Store, subject: &str, budget: &mut ProofBudget) -> Result<PinHead> {
    charge(budget, 0, 4)?;
    let mut query=store.db.prepare("SELECT p.current_revision,p.value,h.revision,p.value IS h.value FROM catalog_identity_pins p LEFT JOIN catalog_identity_pin_revisions h ON h.item_id=p.item_id AND h.namespace=p.namespace AND h.revision=p.current_revision WHERE p.item_id=? AND p.namespace='nfo'")?;
    let mut rows = query.query([subject])?;
    let Some(row) = rows.next()? else {
        let history:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_identity_pin_revisions WHERE item_id=? AND namespace='nfo')",[subject],|r|r.get(0))?;
        if history {
            return Err(refused("manual pin head missing retained history"));
        }
        return Ok(PinHead::NeverDecided);
    };
    let revision: i64 = row.get(0)?;
    let value = optional_text(row, 1, 1024, budget)?;
    let historical: Option<i64> = row.get(2)?;
    let equal: bool = row.get(3)?;
    let latest:Option<i64>=store.db.query_row("SELECT max(revision) FROM catalog_identity_pin_revisions WHERE item_id=? AND namespace='nfo'",[subject],|r|r.get(0))?;
    if revision < 1 || historical != Some(revision) || latest != Some(revision) || !equal {
        return Err(refused("manual pin differs from exact history head"));
    }
    Ok(PinHead::Decision { revision, value })
}

fn schema(
    store: &Store,
    subject: &str,
    fingerprint: &str,
    budget: &mut ProofBudget,
) -> Result<FieldSchema> {
    if !digest(fingerprint) {
        return Err(refused("invalid schema fingerprint"));
    }
    charge(budget, 0, 1)?;
    let mut query=store.db.prepare("SELECT policies_json,system_fields_json FROM catalog_metadata_schemas WHERE item_id=? AND schema_fingerprint=?")?;
    let mut rows = query.query(params![subject, fingerprint])?;
    let row = rows
        .next()?
        .ok_or_else(|| refused("registered schema absent"))?;
    // Borrow SQLite's text cells and preflight before the existing owner helper
    // allocates either its raw strings or decoded policy/system collections.
    for column in 0..2 {
        let raw = borrowed_text(row, column, MAX_METADATA_SNAPSHOT_BYTES)?;
        budget.raw_json(raw, MAX_METADATA_JSON_DEPTH)?;
        budget.raw_json(raw, MAX_METADATA_JSON_DEPTH)?;
    }
    read_registered_schema(&store.db, subject, fingerprint).map_err(refused)
}
fn current_schema(
    store: &Store,
    subject: &str,
    budget: &mut ProofBudget,
) -> Result<Option<FieldSchema>> {
    charge(budget, 0, 2)?;
    let mut query = store
        .db
        .prepare("SELECT schema_fingerprint FROM catalog_metadata_schema_heads WHERE item_id=?")?;
    let mut rows = query.query([subject])?;
    let fingerprint = rows
        .next()?
        .map(|row| text(row, 0, 64, budget))
        .transpose()?;
    let subject_exists: bool = store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM catalog_metadata_subjects WHERE item_id=?)",
        [subject],
        |r| r.get(0),
    )?;
    if fingerprint.is_some() != subject_exists {
        return Err(refused("metadata root/head presence differs"));
    }
    fingerprint
        .map(|value| schema(store, subject, &value, budget))
        .transpose()
}
fn ancestor(
    store: &Store,
    subject: &str,
    historical: &str,
    current: &str,
    budget: &mut ProofBudget,
) -> Result<()> {
    charge(budget, current.len(), 1)?;
    let mut cursor = current.to_owned();
    let mut seen = BTreeSet::new();
    while cursor != historical {
        charge(budget, cursor.len(), 1)?;
        if seen.len() >= MAX_METADATA_JSON_NODES || !seen.insert(cursor.clone()) {
            return Err(refused("schema ancestry cycle/overflow"));
        }
        let mut query=store.db.prepare("SELECT prior_fingerprint FROM catalog_metadata_schemas WHERE item_id=? AND schema_fingerprint=?")?;
        let mut rows = query.query(params![subject, cursor])?;
        let row = rows
            .next()?
            .ok_or_else(|| refused("schema ancestry missing"))?;
        cursor = optional_text(row, 0, 64, budget)?
            .ok_or_else(|| refused("source schema is outside current lineage"))?;
        if !digest(&cursor) {
            return Err(refused("invalid schema predecessor"));
        }
    }
    Ok(())
}
fn source(
    store: &Store,
    subject: &str,
    current: Option<&FieldSchema>,
    budget: &mut ProofBudget,
) -> Result<Option<ContributionDocument>> {
    charge(budget, 0, 6)?;
    let mut query=store.db.prepare("SELECT revision,source_kind,schema_fingerprint,fields_json,evidence_json FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.nfo' AND record_id='document'")?;
    let mut rows = query.query([subject])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let revision: i64 = row.get(0)?;
    let kind = text(row, 1, 16, budget)?;
    let fingerprint = text(row, 2, 64, budget)?;
    let fields = text(row, 3, MAX_METADATA_DOCUMENT_BYTES, budget)?;
    let evidence = text(row, 4, MAX_METADATA_EVIDENCE_BYTES, budget)?;
    if revision < 1 || kind != "provider" {
        return Err(refused("NFO source is not a retained provider head"));
    }
    let current = current.ok_or_else(|| refused("NFO source has no current schema"))?;
    ancestor(store, subject, &fingerprint, current.fingerprint(), budget)?;
    let historical = schema(store, subject, &fingerprint, budget)?;
    budget.raw_json(&fields, MAX_METADATA_JSON_DEPTH)?;
    budget.raw_json(&evidence, MAX_METADATA_JSON_DEPTH)?;
    let document = ContributionDocument::from_json_with_system_fields(
        &historical,
        ContributionKey::new(subject, NAMESPACE, RECORD).map_err(refused)?,
        SourceKind::Provider,
        revision,
        &fields,
        &evidence,
    )
    .map_err(refused)?;
    budget.serialized(document.fields(), MAX_METADATA_JSON_DEPTH)?;
    budget.canonical_json(document.evidence())?;
    if serde_json::to_string(document.fields())? != fields
        || catabolic_core::encode(document.evidence()) != evidence
    {
        return Err(refused("source is not exact normalized owner encoding"));
    }
    let exact:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_metadata_history WHERE item_id=?1 AND namespace='motion.nfo' AND record_id='document' AND revision=?2 AND source_kind='provider' AND schema_fingerprint=?3 AND fields_json=?4 AND evidence_json=?5) AND ?2=(SELECT max(revision) FROM catalog_metadata_history WHERE item_id=?1 AND namespace='motion.nfo' AND record_id='document')",params![subject,revision,fingerprint,fields,evidence],|r|r.get(0))?;
    if !exact {
        return Err(refused("source differs from terminal immutable history"));
    }
    budget.serialized(&(document.fields(), document.evidence()), 64)?;
    crate::catalog_metadata_store::revalidate_retained_document(
        &store.db,
        subject,
        current,
        &historical,
        &document,
    )
    .map_err(refused)?;
    Ok(Some(document))
}

/// Current assertions are descriptive native state, never proof of who may
/// change them. Imported assertion/exclusion provenance must be independently
/// covered by the eventual bootstrap owner, not guessed from note/reason JSON.
struct TagState {
    active: BTreeSet<String>,
    has_assertions: bool,
}
fn tags(
    store: &Store,
    subject: &str,
    generation: i64,
    source_revision: Option<i64>,
    budget: &mut ProofBudget,
    pending: Option<PendingRead<'_>>,
) -> Result<TagState> {
    let mut active_tags = BTreeSet::new();
    let mut has_assertions = false;
    // Stream all assertions for this exact item: filtering/sorting a large
    // unrelated assertion set must not hide work before its first charged row.
    let mut query = store
        .db
        .prepare("SELECT id,tag_id,source,active FROM item_tags WHERE item_id=?")?;
    let mut rows = query.query([subject])?;
    while let Some(row) = rows.next()? {
        charge(budget, 0, 4)?;
        let assertion = text(row, 0, 256, budget)?;
        let id = text(row, 1, 256, budget)?;
        let namespace = text(row, 2, 256, budget)?;
        let active: i64 = row.get(3)?;
        if namespace != NAMESPACE {
            continue;
        }
        if !identifier(&assertion) || !identifier(&id) || !matches!(active, 0 | 1) {
            return Err(refused("invalid current NFO tag assertion"));
        }
        has_assertions = true;
        entity_revision(store, "item_tags", &assertion, budget)?;
        entity_revision(store, "tags", &id, budget)?;
        let mut names = store.db.prepare("SELECT name,EXISTS(SELECT 1 FROM tag_names WHERE tag_id=tags.id AND name=tags.name),EXISTS(SELECT 1 FROM catalog_tag_redirects WHERE source_tag_id=tags.id) FROM tags WHERE id=?")?;
        let mut names = names.query([&id])?;
        let row = names
            .next()?
            .ok_or_else(|| refused("NFO tag vocabulary row absent"))?;
        let name = text(row, 0, 100, budget)?;
        let canonical: bool = row.get(1)?;
        let retired: bool = row.get(2)?;
        if !canonical || retired {
            return Err(refused("NFO assertion names missing/retired vocabulary"));
        }
        if active == 1 {
            if active_tags.len() >= 100 || !active_tags.insert(name) {
                return Err(refused("NFO tag count overflow or duplicate name"));
            }
        }
    }
    // Suppressions are independent item-wide decisions. Validate their exact
    // retained heads, but do NOT remove suppressed names from source tags or
    // turn a foreign decision into an NFO-owned exclusion.
    let mut query=store.db.prepare("SELECT s.tag_id,s.revision,h.revision,h.suppressed,h.command_catalog_id,h.command_host_namespace,h.command_operation_id FROM catalog_item_tag_suppressions s LEFT JOIN catalog_tag_suppression_revisions h ON h.subject_type=s.subject_type AND h.subject_id=s.item_id AND h.tag_id=s.tag_id AND h.revision=s.revision WHERE s.item_id=?")?;
    let mut rows = query.query([subject])?;
    while let Some(row) = rows.next()? {
        charge(budget, 0, 7)?;
        let tag = text(row, 0, 256, budget)?;
        let revision: i64 = row.get(1)?;
        let historical: Option<i64> = row.get(2)?;
        let suppressed: Option<i64> = row.get(3)?;
        let command = CommandKey::new(
            text(row, 4, 256, budget)?,
            text(row, 5, 256, budget)?,
            text(row, 6, 256, budget)?,
        )
        .map_err(refused)?;
        let latest:Option<i64>=store.db.query_row("SELECT max(revision) FROM catalog_tag_suppression_revisions WHERE subject_type='item' AND subject_id=? AND tag_id=?",params![subject,tag],|r|r.get(0))?;
        let entity:Option<(i64,bool)>=store.db.query_row("SELECT revision,deleted FROM catalog_entity_revisions WHERE entity_kind='catalog_item_tag_suppressions' AND entity_key=json_array(?,?)",params![subject,tag],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if !identifier(&tag)
            || revision < 1
            || historical != Some(revision)
            || latest != Some(revision)
            || !matches!(suppressed, Some(0 | 1))
            || !matches!(entity,Some((r,false)) if r>0)
        {
            return Err(refused("suppression differs from exact retained head"));
        }
        entity_revision(store, "tags", &tag, budget)?;
        receipt_link(store, &command, None, generation, budget)?;
    }
    // Ownership may legitimately be older than a later manual decision. Its
    // explicit historical pointer must still exist; it is never promoted.
    let mut query=store.db.prepare("SELECT o.tag_id,o.namespace,o.revision,s.revision,h.revision FROM catalog_metadata_tag_suppressions o LEFT JOIN catalog_item_tag_suppressions s ON s.item_id=o.item_id AND s.tag_id=o.tag_id LEFT JOIN catalog_tag_suppression_revisions h ON h.subject_type='item' AND h.subject_id=o.item_id AND h.tag_id=o.tag_id AND h.revision=o.revision WHERE o.item_id=?")?;
    let mut rows = query.query([subject])?;
    while let Some(row) = rows.next()? {
        charge(budget, 0, 5)?;
        let tag = text(row, 0, 256, budget)?;
        let namespace = text(row, 1, 256, budget)?;
        let revision: i64 = row.get(2)?;
        let current: Option<i64> = row.get(3)?;
        let historical: Option<i64> = row.get(4)?;
        if !identifier(&tag)
            || namespace != "motion.local"
            || revision < 1
            || current.is_none_or(|r| r < revision)
            || historical != Some(revision)
        {
            return Err(refused(
                "unsupported or inconsistent source exclusion ownership",
            ));
        }
    }
    // Native40 replacement has immutable attributed tag history. Earlier native
    // imports have no such row, so absence is not invented proof of an empty
    // source. Imported tag provenance remains a bootstrap registration duty.
    let mut query=store.db.prepare("SELECT revision,after_json,command_catalog_id,command_host_namespace,command_operation_id FROM catalog_metadata_tag_history WHERE item_id=? AND namespace='motion.nfo' AND record_id='document' ORDER BY revision DESC LIMIT 1")?;
    let mut rows = query.query([subject])?;
    if let Some(row) = rows.next()? {
        charge(budget, 0, 5)?;
        let revision: i64 = row.get(0)?;
        let raw = text(row, 1, MAX_METADATA_DOCUMENT_BYTES, budget)?;
        let command = CommandKey::new(
            text(row, 2, 256, budget)?,
            text(row, 3, 256, budget)?,
            text(row, 4, 256, budget)?,
        )
        .map_err(refused)?;
        if revision < 1 || source_revision.is_none_or(|r| r < revision) {
            return Err(refused("source tag history exceeds native source head"));
        }
        budget.raw_json(&raw, MAX_METADATA_JSON_DEPTH)?;
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct After {
            tags: Vec<String>,
            excluded_tags: Vec<String>,
        }
        let after: After = serde_json::from_str(&raw)?;
        if !after.excluded_tags.is_empty() || after.tags.len() > 100 {
            return Err(refused("unrepresented NFO exclusions or tag bound"));
        }
        let mut expected = BTreeSet::new();
        let mut requested = BTreeSet::new();
        for tag in after.tags {
            charge(budget, tag.len(), 2)?;
            if tag.is_empty() || tag.len() > 100 || !requested.insert(tag.clone()) {
                return Err(refused("invalid retained source tag history"));
            }
            // Replacement history retains normalized requested names, which
            // may be aliases. Compare their exact current typed vocabulary
            // targets, not the alias spelling against the canonical display.
            let mut query = store.db.prepare(
                "SELECT t.name FROM tag_names n JOIN tags t ON t.id=n.tag_id WHERE n.name=? AND NOT EXISTS(SELECT 1 FROM catalog_tag_redirects r WHERE r.source_tag_id=t.id)",
            )?;
            let mut rows = query.query([&tag])?;
            let row = rows
                .next()?
                .ok_or_else(|| refused("source tag history alias target absent or retired"))?;
            let canonical = text(row, 0, 100, budget)?;
            expected.insert(canonical);
        }
        if expected != active_tags {
            return Err(refused(
                "current NFO tags differ from attributed source history",
            ));
        }
        let linked:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_metadata_history WHERE item_id=? AND namespace='motion.nfo' AND record_id='document' AND revision=? AND command_catalog_id=? AND command_host_namespace=? AND command_operation_id=?)",params![subject,revision,command.catalog_id(),command.host_namespace(),command.operation_id()],|r|r.get(0))?;
        if !linked {
            return Err(refused("tag/source history command linkage differs"));
        }
        if let Some(permit) = pending.filter(|permit| permit.key() == &command) {
            permit.authorize_source_read_in(store, subject, revision, generation, budget)?;
        } else {
            receipt_link(store, &command, None, generation, budget)?;
        }
    }
    Ok(TagState {
        active: active_tags,
        has_assertions,
    })
}

struct OwnerRow {
    revision: i64,
    source_revision: i64,
    public_incarnation: i64,
    public_revision: Option<i64>,
    document_identity: Option<String>,
    active_identity: Option<String>,
    origin: Option<FileOrigin>,
    physical: Option<PhysicalMapping>,
    origin_proof_digest: Option<String>,
    source_proof_digest: String,
    audit_digest: String,
}
fn owner(
    store: &Store,
    subject: &str,
    generation: i64,
    budget: &mut ProofBudget,
    pending: Option<PendingRead<'_>>,
) -> Result<Option<OwnerRow>> {
    charge(budget, 0, 1)?;
    let revision: Option<i64> = store
        .db
        .query_row(
            "SELECT current_revision FROM catalog_nfo_states WHERE item_id=?",
            [subject],
            |r| r.get(0),
        )
        .optional()?;
    let Some(revision) = revision else {
        return Ok(None);
    };
    if revision < 1 || entity_revision(store, "catalog_nfo_states", subject, budget)? != revision {
        return Err(refused("NFO owner head/entity revision differs"));
    }
    charge(budget, 0, 16)?;
    let mut query=store.db.prepare("SELECT source_revision,source_namespace,source_record_id,public_incarnation,public_revision,document_identity,active_identity,origin_json,origin_physical_json,origin_proof_digest,source_proof_digest,attribution_audit_digest,command_catalog_id,command_host_namespace,command_operation_id FROM catalog_nfo_state_revisions WHERE item_id=? AND revision=?")?;
    let mut rows = query.query(params![subject, revision])?;
    let row = rows
        .next()?
        .ok_or_else(|| refused("NFO owner history head absent"))?;
    let source_revision: i64 = row.get(0)?;
    let namespace = text(row, 1, 256, budget)?;
    let record = text(row, 2, 256, budget)?;
    let public_incarnation: i64 = row.get(3)?;
    let public_revision: Option<i64> = row.get(4)?;
    let document_identity = optional_text(row, 5, 1024, budget)?;
    let active_identity = optional_text(row, 6, 1024, budget)?;
    let origin_json = optional_text(row, 7, MAX_METADATA_DOCUMENT_BYTES, budget)?;
    let physical_json = optional_text(row, 8, MAX_METADATA_DOCUMENT_BYTES, budget)?;
    let origin_proof_digest = optional_text(row, 9, 64, budget)?;
    let source_proof_digest = text(row, 10, 64, budget)?;
    let audit_digest = text(row, 11, 64, budget)?;
    let command = CommandKey::new(
        text(row, 12, 256, budget)?,
        text(row, 13, 256, budget)?,
        text(row, 14, 256, budget)?,
    )
    .map_err(refused)?;
    if source_revision < 1
        || namespace != NAMESPACE
        || record != RECORD
        || public_incarnation < 1
        || public_revision.is_some_and(|v| v < 1 || v > source_revision)
        || !digest(&source_proof_digest)
        || !digest(&audit_digest)
        || origin_proof_digest.as_ref().is_some_and(|v| !digest(v))
    {
        return Err(refused("invalid typed NFO owner head"));
    }
    let newer: bool = store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM catalog_nfo_state_revisions WHERE item_id=? AND revision>?)",
        params![subject, revision],
        |r| r.get(0),
    )?;
    if newer {
        return Err(refused("NFO owner head is not terminal history"));
    }
    if let Some(permit) = pending.filter(|permit| permit.key() == &command) {
        permit.authorize_state_read_in(
            store,
            subject,
            revision,
            source_revision,
            generation,
            budget,
        )?;
    } else {
        receipt_link(store, &command, None, generation, budget)?;
    }
    let origin = origin_json
        .map(|raw| {
            budget.raw_json(&raw, MAX_METADATA_JSON_DEPTH)?;
            serde_json::from_str::<FileOrigin>(&raw).map_err(Error::from)
        })
        .transpose()?;
    let physical = physical_json
        .map(|raw| {
            budget.raw_json(&raw, MAX_METADATA_JSON_DEPTH)?;
            serde_json::from_str::<PhysicalMapping>(&raw).map_err(Error::from)
        })
        .transpose()?;
    match origin.as_ref().map(|o| &o.provenance) {
        Some(OriginProvenance::Imported {
            source_occurrence_id,
            ..
        }) => {
            let mapping = physical
                .as_ref()
                .ok_or_else(|| refused("imported origin lost physical mapping"))?;
            let origin = origin.as_ref().expect("matched origin");
            if origin_proof_digest.is_none()
                || mapping.profile != origin.file.profile
                || mapping.exact.file_id != origin.file.file_id
                || &mapping.source_occurrence_id != source_occurrence_id
                || !identifier(&mapping.profile)
                || !identifier(&mapping.source_occurrence_id)
                || !identifier(&mapping.location)
                || mapping.path.is_empty()
                || mapping.path.len() > 4096
                || mapping.exact.validate().is_err()
            {
                return Err(refused(
                    "imported origin contradicts exact historical mapping",
                ));
            }
        }
        _ if physical.is_some() || origin_proof_digest.is_some() => {
            return Err(refused("physical import proof without imported origin"));
        }
        _ => {}
    }
    Ok(Some(OwnerRow {
        revision,
        source_revision,
        public_incarnation,
        public_revision,
        document_identity,
        active_identity,
        origin,
        physical,
        origin_proof_digest,
        source_proof_digest,
        audit_digest,
    }))
}
fn indexes(
    store: &Store,
    subject: &str,
    owner: Option<&OwnerRow>,
    pin: &PinHead,
    budget: &mut ProofBudget,
) -> Result<()> {
    charge(budget, 0, 5)?;
    let mut query = store
        .db
        .prepare("SELECT value,state_revision FROM catalog_nfo_identity_claims WHERE item_id=?")?;
    let mut rows = query.query([subject])?;
    let claim = rows
        .next()?
        .map(|row| Ok::<_, Error>((text(row, 0, 1024, budget)?, row.get::<_, i64>(1)?)))
        .transpose()?;
    let expected = owner.and_then(|o| {
        o.active_identity
            .as_ref()
            .map(|value| (value.as_str(), o.revision))
    });
    if claim.as_ref().map(|(v, r)| (v.as_str(), *r)) != expected {
        return Err(refused("automatic claim index differs from current owner"));
    }
    if let Some((value, _)) = expected {
        let collision:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_identity_pins WHERE namespace='nfo' AND value=? AND item_id<>?)",params![value,subject],|r|r.get(0))?;
        if collision {
            return Err(refused(
                "automatic claim collides with another manual owner",
            ));
        }
    }
    if let PinHead::Decision {
        value: Some(value), ..
    } = pin
    {
        let collision: bool = store.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM catalog_nfo_identity_claims WHERE value=? AND item_id<>?)",
            params![value, subject],
            |r| r.get(0),
        )?;
        if collision {
            return Err(refused("manual pin collides with another automatic owner"));
        }
    }
    let mut query = store.db.prepare(
        "SELECT profile,file_id,state_revision FROM catalog_nfo_file_attributions WHERE item_id=?",
    )?;
    let mut rows = query.query([subject])?;
    let file = rows
        .next()?
        .map(|row| {
            Ok::<_, Error>((
                text(row, 0, 256, budget)?,
                text(row, 1, 256, budget)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .transpose()?;
    let expected = owner.and_then(|o| {
        o.origin.as_ref().map(|origin| {
            (
                origin.file.profile.as_str(),
                origin.file.file_id.as_str(),
                o.revision,
            )
        })
    });
    if file.as_ref().map(|(p, f, r)| (p.as_str(), f.as_str(), *r)) != expected {
        return Err(refused("file attribution index differs from current owner"));
    }
    Ok(())
}

pub(super) fn read_in(
    store: &Store,
    subject: &str,
    expected_generation: i64,
    aggregate_revision: i64,
) -> Result<CurrentNfoState> {
    read_inner(
        store,
        subject,
        expected_generation,
        aggregate_revision,
        None,
    )
}
pub(super) fn read_pending_in(
    store: &Store,
    subject: &str,
    expected_generation: i64,
    aggregate_revision: i64,
    permit: &super::refresh::RefreshReadPermit,
) -> Result<CurrentNfoState> {
    read_inner(
        store,
        subject,
        expected_generation,
        aggregate_revision,
        Some(PendingRead::Refresh(permit)),
    )
}
pub(super) fn read_retirement_in(
    store: &Store,
    subject: &str,
    expected_generation: i64,
    aggregate_revision: i64,
    permit: &super::retirement::PendingRetirementRead,
) -> Result<CurrentNfoState> {
    read_inner(
        store,
        subject,
        expected_generation,
        aggregate_revision,
        Some(PendingRead::Retirement(permit)),
    )
}
fn read_inner(
    store: &Store,
    subject: &str,
    expected_generation: i64,
    aggregate_revision: i64,
    pending: Option<PendingRead<'_>>,
) -> Result<CurrentNfoState> {
    if !identifier(subject) || aggregate_revision < 0 {
        return Err(refused(
            "invalid subject or externally asserted aggregate revision",
        ));
    }
    let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
    let current = fence(store, expected_generation, &mut budget)?;
    bootstrap_complete(store, expected_generation, &mut budget)?;
    let subject_state = subject_state(store, subject, &mut budget)?;
    let pin = pin(store, subject, &mut budget)?;
    let schema = current_schema(store, subject, &mut budget)?;
    let native_source = source(store, subject, schema.as_ref(), &mut budget)?;
    let tag_state = tags(
        store,
        subject,
        expected_generation,
        native_source.as_ref().map(|d| d.revision()),
        &mut budget,
        pending,
    )?;
    let tags = tag_state.active;
    let owner = owner(store, subject, expected_generation, &mut budget, pending)?;
    indexes(store, subject, owner.as_ref(), &pin, &mut budget)?;
    let (
        source,
        attribution,
        presence,
        origin_physical,
        origin_proof_digest,
        source_proof_digest,
        attribution_audit_digest,
    ) = match (owner, native_source.as_ref()) {
        (None, None) => {
            charge(&mut budget, 0, 1)?;
            let history:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_metadata_history WHERE item_id=?1 AND namespace='motion.nfo' AND record_id='document') OR EXISTS(SELECT 1 FROM catalog_nfo_state_revisions WHERE item_id=?1)",[subject],|r|r.get(0))?;
            if history || tag_state.has_assertions {
                return Err(refused(
                    "missing state cannot erase source/history/tag evidence",
                ));
            }
            (
                None,
                None,
                SourcePresence {
                    revision: 0,
                    incarnation: 0,
                    public_revision: None,
                },
                None,
                None,
                None,
                None,
            )
        }
        (Some(row), Some(document)) => {
            if matches!(subject_state, SubjectState::Retired { .. })
                && row.active_identity.is_some()
            {
                return Err(refused("retired subject retains active automatic claim"));
            }
            if row.source_revision != document.revision() {
                return Err(refused("state/source revisions diverge"));
            }
            budget.serialized(
                &(
                    document.fields(),
                    &row.document_identity,
                    &row.origin,
                    &row.active_identity,
                ),
                64,
            )?;
            let document_state = if row.public_revision.is_some() {
                DocumentState::Present {
                    contribution: Contribution {
                        fields: document.fields().clone(),
                        tags,
                    },
                    external_identity: row.document_identity,
                }
            } else {
                if !document.fields().is_empty()
                    || !tags.is_empty()
                    || row.document_identity.is_some()
                    || row.active_identity.is_some()
                    || row.origin.is_some()
                {
                    return Err(refused("withdrawn NFO retains public data or attribution"));
                }
                DocumentState::Withdrawn
            };
            (
                Some(DocumentHead {
                    revision: row.source_revision,
                    document: document_state,
                }),
                Some(AttributionHead {
                    revision: row.revision,
                    document_revision: row.source_revision,
                    origin: row.origin,
                    active_identity: row.active_identity,
                }),
                SourcePresence {
                    revision: row.source_revision,
                    incarnation: row.public_incarnation,
                    public_revision: row.public_revision,
                },
                row.physical,
                row.origin_proof_digest,
                Some(row.source_proof_digest),
                Some(row.audit_digest),
            )
        }
        _ => return Err(refused("source and NFO state presence differ")),
    };
    if let Some(schema) = &schema {
        budget.serialized(&(schema.fingerprint(), schema.system_fields()), 64)?;
    }
    charge(&mut budget, subject.len(), 1)?;
    let presence = PresenceSnapshot {
        contract_version: PRESENCE_CONTRACT_VERSION,
        fence: current,
        subject: subject.into(),
        aggregate_revision,
        head: SubjectHead {
            subject_state,
            schema_fingerprint: schema.as_ref().map(|s| s.fingerprint().to_owned()),
            system_fields: schema
                .as_ref()
                .map(|s| s.system_fields().clone())
                .unwrap_or_default(),
            source,
            attribution,
            pin,
        },
        presence,
    };
    // Reuse the core's precise presence/document/origin validation. Charge its
    // returned public-document copy before it is constructed, then discard it.
    budget.serialized(&presence, 64)?;
    public_nfo_document(&presence).map_err(refused)?;
    let end = fence(store, expected_generation, &mut budget)?;
    if end != presence.fence {
        return Err(refused("current state fence changed"));
    }
    Ok(CurrentNfoState {
        presence,
        native_source,
        schema,
        origin_physical,
        origin_proof_digest,
        source_proof_digest,
        attribution_audit_digest,
    })
}
