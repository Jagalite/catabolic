// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Native import v2 adapters into existing typed owners. No independent metadata
//! resolver, identity engine, receipt transaction, grant inference or effects.
use crate::{
    Error, Store,
    catalog_import::{ImportError, ImportErrorCode, Result},
    catalog_metadata_store::{
        self as metadata, MetadataAccess, MetadataAuthority, MetadataChange, MetadataMutation,
        PolicyField,
    },
    catalog_pins::{self as pins, PinAuthority, PinBatch, PinChange, PinDecision, PinMutation},
};
use catabolic_core::{catalog_command::CommandEnvelope, catalog_metadata::SourceKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default)]
pub struct TypedAuthority {
    pub metadata: Vec<MetadataAuthority>,
    pub pins: Option<PinAuthority>,
    pub physical_files: BTreeMap<String, BTreeSet<String>>,
    pub identity: Option<crate::catalog_identity_mutation::IdentityAuthority>,
    pub tag_sources: Vec<TagImportAuthority>,
    pub relationships: Option<RelationshipImportAuthority>,
    pub existing_items: BTreeMap<String, ExistingItemGrant>,
    pub existing_contexts: BTreeMap<String, ExistingContextGrant>,
    pub existing_timelines: BTreeMap<String, ExistingTimelineGrant>,
    pub logical_aliases: Option<LogicalAliasImportAuthority>,
}
/// Exact independently reviewed alias pairs and affected logical graph scope.
#[derive(Clone, Debug)]
pub struct LogicalAliasImportAuthority {
    pub host_namespace: String,
    pub aliases: BTreeSet<(String, String)>,
    pub item_ids: BTreeSet<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedLogicalAlias {
    pub alias_item_id: String,
    pub survivor_item_id: String,
    pub source_receipt_id: String,
    /// Original serialized host merge receipt, retained without re-encoding.
    pub source_receipt_json: String,
}
/// Prior activated context authority for append-only import continuation. A
/// continuation creates new versions/timelines/bindings only; the native owner
/// loads and revalidates all existing members, never replacing them from input.
#[derive(Clone, Debug)]
pub struct ExistingContextGrant {
    pub host_namespace: String,
    pub catalog_id: String,
    pub incarnation: String,
    pub authority_namespace: String,
    pub context_key: String,
    pub revision: i64,
}
pub fn capture_existing_context(
    store: &Store,
    host_namespace: &str,
    context_id: &str,
    independently_allowed_contexts: &BTreeSet<String>,
) -> Result<ExistingContextGrant> {
    if !id(host_namespace)
        || !id(context_id)
        || !independently_allowed_contexts.contains(context_id)
        || !store.db.is_autocommit()
    {
        return Err(error(
            ImportErrorCode::AuthorityDenied,
            "existing context is outside trusted import scope or snapshot",
        ));
    }
    let (authority_namespace,context_key,revision) = store.transaction(|db| {
        Ok(db.query_row("SELECT authority_namespace,context_key,current_revision FROM catalog_segmentation_contexts WHERE id=?",[context_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?)))?)
    })?;
    Ok(ExistingContextGrant {
        host_namespace: host_namespace.into(),
        catalog_id: store.database_id.clone(),
        incarnation: store
            .catalog_incarnation
            .clone()
            .ok_or_else(|| invalid("missing catalog incarnation"))?,
        authority_namespace,
        context_key,
        revision,
    })
}
#[derive(Clone, Debug)]
pub struct ExistingTimelineGrant {
    pub host_namespace: String,
    pub catalog_id: String,
    pub incarnation: String,
    pub edition_item_id: String,
    pub revision: i64,
}
pub fn capture_existing_timeline(
    store: &Store,
    host_namespace: &str,
    timeline_id: &str,
    independently_allowed_timelines: &BTreeSet<String>,
) -> Result<ExistingTimelineGrant> {
    if !id(host_namespace)
        || !id(timeline_id)
        || !independently_allowed_timelines.contains(timeline_id)
        || !store.db.is_autocommit()
    {
        return Err(error(
            ImportErrorCode::AuthorityDenied,
            "existing timeline is outside trusted import scope or snapshot",
        ));
    }
    let (edition_item_id, revision) = store.transaction(|db| {
        Ok(db.query_row(
            "SELECT edition_item_id,current_revision FROM catalog_timelines WHERE id=?",
            [timeline_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )?)
    })?;
    Ok(ExistingTimelineGrant {
        host_namespace: host_namespace.into(),
        catalog_id: store.database_id.clone(),
        incarnation: store
            .catalog_incarnation
            .clone()
            .ok_or_else(|| invalid("missing catalog incarnation"))?,
        edition_item_id,
        revision,
    })
}
/// Exact existing endpoint authority for a later import partition. Never
/// deserialize this from a staged snapshot. Capture only IDs the trusted host
/// has independently reconciled to earlier activated partition receipts.
#[derive(Clone, Debug)]
pub struct ExistingItemGrant {
    pub host_namespace: String,
    pub catalog_id: String,
    pub incarnation: String,
    pub item_revision: i64,
}
pub fn capture_existing_item(
    store: &Store,
    host_namespace: &str,
    item_id: &str,
    independently_allowed_items: &BTreeSet<String>,
) -> Result<ExistingItemGrant> {
    if !id(host_namespace) || !independently_allowed_items.contains(item_id) {
        return Err(error(
            ImportErrorCode::AuthorityDenied,
            "existing item is outside trusted import scope",
        ));
    }
    let revision = existing_item_revision(store, item_id)?;
    Ok(ExistingItemGrant {
        host_namespace: host_namespace.into(),
        catalog_id: store.database_id.clone(),
        incarnation: store
            .catalog_incarnation
            .clone()
            .ok_or_else(|| invalid("missing catalog incarnation"))?,
        item_revision: revision,
    })
}
fn existing_item_revision(store: &Store, item_id: &str) -> Result<i64> {
    use rusqlite::OptionalExtension;
    let revision = store.db.query_row("SELECT r.revision FROM catalog_entity_revisions r JOIN items i ON r.entity_key=json_array(i.id) WHERE r.entity_kind='items' AND i.id=? AND r.deleted=0 AND NOT EXISTS(SELECT 1 FROM catalog_logical_retirements x WHERE x.item_id=i.id)", [item_id], |r| r.get::<_,i64>(0)).optional()?;
    revision.ok_or_else(|| {
        error(
            ImportErrorCode::SnapshotConflict,
            "existing import endpoint missing or retired",
        )
    })
}
#[derive(Clone, Debug)]
pub struct RelationshipImportAuthority {
    pub host_namespace: String,
    /// Exact directed edge scopes. Neither endpoint membership nor a source
    /// snapshot alone grants authority to create arbitrary relationships.
    pub edges: BTreeSet<(String, String, String)>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedRelationship {
    pub source_relationship_id: String,
    pub source_revision: i64,
    pub source_item_id: String,
    pub target_item_id: String,
    pub kind: String,
    pub position: Option<u32>,
    pub metadata: Value,
}
/// Independently established namespace/subject grants. Suppression is a local
/// editorial decision, never authority inferred from an imported source name.
#[derive(Clone, Debug)]
pub struct TagImportAuthority {
    pub host_namespace: String,
    pub namespace: String,
    pub item_ids: BTreeSet<String>,
    pub allow_suppressions: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedTagSet {
    pub item_id: String,
    pub namespace: String,
    pub source_revision: i64,
    pub tags: Vec<String>,
    pub excluded_tags: Vec<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportedSourceKind {
    Local,
    Provider,
    Scan,
}
impl ImportedSourceKind {
    pub(crate) fn owner(self) -> SourceKind {
        match self {
            Self::Local => SourceKind::Local,
            Self::Provider => SourceKind::Provider,
            Self::Scan => SourceKind::Scan,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedContribution {
    pub namespace: String,
    pub record_id: String,
    pub source_revision: i64,
    pub kind: ImportedSourceKind,
    pub fields: Value,
    pub evidence: Value,
}
fn metadata_contract_v1() -> u32 {
    1
}
fn metadata_contract_is_v1(value: &u32) -> bool {
    *value == 1
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedMetadata {
    #[serde(
        default = "metadata_contract_v1",
        skip_serializing_if = "metadata_contract_is_v1"
    )]
    pub contract_version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub system_fields: Vec<String>,
    pub item_id: String,
    pub policies: Value,
    pub contributions: Vec<ImportedContribution>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedPinRevision {
    pub source_revision: i64,
    pub decision: PinDecision,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedPin {
    pub item_id: String,
    pub namespace: String,
    pub history: Vec<ImportedPinRevision>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypedBatch {
    pub metadata: Vec<ImportedMetadata>,
    pub pins: Vec<ImportedPin>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<ImportedTagSet>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relationships: Vec<ImportedRelationship>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub logical_aliases: Vec<ImportedLogicalAlias>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<NativeIdentityBatch>,
}
fn error(code: ImportErrorCode, message: &str) -> ImportError {
    ImportError {
        code,
        error: Error(message.into()),
    }
}
fn invalid(message: &str) -> ImportError {
    error(ImportErrorCode::InvalidInput, message)
}
fn id(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
impl TypedBatch {
    pub fn record_count(&self) -> usize {
        self.logical_aliases.len()
            + self.relationships.len()
            + self
                .tags
                .iter()
                .map(|t| 1 + t.tags.len() + t.excluded_tags.len())
                .sum::<usize>()
            + self.metadata.len()
            + self
                .metadata
                .iter()
                .map(|m| m.contributions.len())
                .sum::<usize>()
            + self.pins.iter().map(|p| p.history.len()).sum::<usize>()
            + self.native.as_ref().map_or(0, |n| {
                n.occurrences.len() + n.associations.len() + n.mutations.len()
            })
    }
    pub(crate) fn validate(&self) -> Result<()> {
        if !(1..=64).contains(&self.record_count()) {
            return Err(invalid("typed batch must contain 1..=64 owner records"));
        }
        if let Some(native) = &self.native {
            native.validate()?;
        }
        let mut aliases = BTreeSet::new();
        for alias in &self.logical_aliases {
            if !id(&alias.alias_item_id)
                || !id(&alias.survivor_item_id)
                || !id(&alias.source_receipt_id)
                || alias.alias_item_id == alias.survivor_item_id
                || !aliases.insert(&alias.alias_item_id)
                || !serde_json::from_str::<Value>(&alias.source_receipt_json)
                    .is_ok_and(|v| v.is_object())
            {
                return Err(invalid("invalid imported logical alias or source receipt"));
            }
        }
        let mut relationships = BTreeSet::new();
        for relationship in &self.relationships {
            if !id(&relationship.source_relationship_id)
                || relationship.source_revision < 1
                || !id(&relationship.source_item_id)
                || !id(&relationship.target_item_id)
                || relationship.source_item_id == relationship.target_item_id
                || !relationship.metadata.is_object()
                || !relationships.insert(&relationship.source_relationship_id)
            {
                return Err(invalid("invalid or duplicate imported relationship"));
            }
            crate::curation::validate_relationship_read_kind(&relationship.kind)
                .map_err(|_| invalid("invalid imported relationship kind"))?;
        }
        let mut tag_subjects = BTreeSet::new();
        for tags in &self.tags {
            if !id(&tags.item_id)
                || !id(&tags.namespace)
                || tags.source_revision < 1
                || !tag_subjects.insert((&tags.item_id, &tags.namespace))
            {
                return Err(invalid("invalid or duplicate imported tag source"));
            }
            for names in [&tags.tags, &tags.excluded_tags] {
                let mut canonical = BTreeSet::new();
                for name in names {
                    let normalized = crate::tagging::tag_name(&json!(name))
                        .map_err(|_| invalid("invalid imported tag name"))?;
                    if !canonical.insert(normalized) {
                        return Err(invalid("duplicate canonical imported tag"));
                    }
                }
            }
        }
        let mut subjects = BTreeSet::new();
        let mut pairs = BTreeSet::new();
        for m in &self.metadata {
            if !id(&m.item_id) || !subjects.insert(&m.item_id) {
                return Err(invalid("invalid or duplicate metadata subject"));
            }
            if !matches!(m.contract_version, 1 | 2)
                || (m.contract_version == 1 && !m.system_fields.is_empty())
                || m.system_fields.iter().collect::<BTreeSet<_>>().len() != m.system_fields.len()
            {
                return Err(invalid("invalid metadata import contract/system policy"));
            }
            let _: Vec<PolicyField> = serde_json::from_value(m.policies.clone())
                .map_err(|_| invalid("invalid metadata policies"))?;
            let mut sources = BTreeSet::new();
            for c in &m.contributions {
                if !id(&c.namespace)
                    || !id(&c.record_id)
                    || c.source_revision < 1
                    || !c.fields.is_object()
                    || !c.evidence.is_object()
                    || !sources.insert((&c.namespace, &c.record_id))
                {
                    return Err(invalid("invalid or duplicate imported contribution"));
                }
            }
        }
        for p in &self.pins {
            if !id(&p.item_id)
                || !id(&p.namespace)
                || p.history.is_empty()
                || !pairs.insert((&p.item_id, &p.namespace))
            {
                return Err(invalid("invalid or duplicate imported decision"));
            }
            let mut last = 0;
            for (i, h) in p.history.iter().enumerate() {
                if h.source_revision <= last || i == 0 && matches!(h.decision, PinDecision::Remove)
                {
                    return Err(invalid(
                        "decision history needs ordered source revisions and an evidenced initial set",
                    ));
                }
                last = h.source_revision;
            }
        }
        Ok(())
    }
}
fn prepare_nfo_import<'a>(
    store: &Store,
    outer: &CommandEnvelope,
    batches: &'a [TypedBatch],
    items: &BTreeSet<String>,
    grants: &TypedAuthority,
    validated: Option<&crate::catalog_import::ValidatedNfoImport<'a>>,
) -> Result<
    BTreeMap<&'a str, crate::catalog_nfo_attribution::source_effect::ImportedSourceEffect<'a>>,
> {
    let mut subjects = BTreeSet::new();
    if store.schema_version != 41 {
        return Ok(BTreeMap::new());
    }
    let budget = store.catalog_operation_budget()?;
    for batch in batches {
        batch.validate()?;
        for m in &batch.metadata {
            for c in &m.contributions {
                budget
                    .charge(crate::operation_budget::BudgetCharge {
                        work: 1,
                        ..Default::default()
                    })
                    .map_err(|_| invalid("NFO import traversal budget"))?;
                if c.namespace != "motion.nfo" {
                    continue;
                }
                budget
                    .charge(crate::operation_budget::BudgetCharge {
                        bytes: std::mem::size_of::<&str>()
                            + std::mem::size_of::<
                                crate::catalog_nfo_attribution::source_effect::ImportedSourceEffect<
                                    '_,
                                >,
                            >(),
                        values: 2,
                        work: 1,
                        ..Default::default()
                    })
                    .map_err(|_| invalid("NFO import retained capability budget"))?;
                if !items.contains(&m.item_id)
                    || !subjects.insert(m.item_id.as_str())
                    || c.record_id != "document"
                    || c.kind != ImportedSourceKind::Provider
                {
                    return Err(invalid(
                        "NFO import requires one complete provider document per subject",
                    ));
                }
                metadata_grant(grants, outer.key().host_namespace(), &m.item_id, None)?;
                metadata_grant(grants, outer.key().host_namespace(), &m.item_id, Some(c))?;
            }
        }
    }
    for tags in batches
        .iter()
        .flat_map(|b| &b.tags)
        .filter(|t| t.namespace == "motion.nfo")
    {
        if !subjects.contains(tags.item_id.as_str())
            || !tags.excluded_tags.is_empty()
            || !grants.tag_sources.iter().any(|g| {
                g.host_namespace == outer.key().host_namespace()
                    && g.namespace == tags.namespace
                    && g.item_ids.contains(&tags.item_id)
            })
        {
            return Err(error(
                ImportErrorCode::AuthorityDenied,
                "NFO tag-only/import exclusion or owner grant unsupported",
            ));
        }
    }
    if subjects.is_empty() {
        return Ok(BTreeMap::new());
    }
    let validated = validated.ok_or_else(|| {
        error(
            ImportErrorCode::AuthorityDenied,
            "complete descriptor-validated NFO import capability required",
        )
    })?;
    validated.check(outer, batches)?;
    // Validate the complete paired set before any permit/vocabulary/init write.
    for subject in &subjects {
        let (_, source, tags) = validated.source(subject)?;
        if tags.is_some_and(|t| {
            t.source_revision != source.source_revision || !t.excluded_tags.is_empty()
        }) {
            return Err(invalid("NFO imported source/tag revision differs"));
        }
    }
    let mut effects = BTreeMap::new();
    for subject in subjects {
        effects.insert(
            subject,
            crate::catalog_nfo_attribution::source_effect::begin_import_in(
                store, validated, subject,
            )?,
        );
    }
    Ok(effects)
}

fn context(store: &Store, outer: &CommandEnvelope, payload: Value) -> Result<CommandEnvelope> {
    let generation = store
        .db
        .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
            r.get(0)
        })
        .map_err(|e| error(ImportErrorCode::StoreFailure, &e.to_string()))?;
    CommandEnvelope::new(
        outer.contract_version(),
        outer.key().clone(),
        outer.admission_incarnation(),
        generation,
        &payload.to_string(),
    )
    .map_err(|e| invalid(&e.to_string()))
}
fn metadata_grant<'a>(
    grants: &'a TypedAuthority,
    host: &str,
    item: &str,
    source: Option<&ImportedContribution>,
) -> Result<&'a MetadataAuthority> {
    grants
        .metadata
        .iter()
        .find(|g| {
            g.host_namespace == host
                && g.subject == item
                && match (&g.access, source) {
                    (MetadataAccess::Initialize | MetadataAccess::InitializeV2 { .. }, None) => {
                        true
                    }
                    (
                        MetadataAccess::Source {
                            namespace,
                            record_id,
                            kind,
                        },
                        Some(s),
                    ) => {
                        namespace == &s.namespace
                            && record_id == &s.record_id
                            && kind == &s.kind.owner()
                    }
                    (
                        MetadataAccess::SourceV2 {
                            namespace,
                            record_id,
                            kind,
                            ..
                        },
                        Some(s),
                    ) => {
                        namespace == &s.namespace
                            && record_id == &s.record_id
                            && kind == &s.kind.owner()
                    }
                    _ => false,
                }
        })
        .ok_or_else(|| {
            error(
                ImportErrorCode::AuthorityDenied,
                "missing independent metadata owner grant",
            )
        })
}
fn apply_metadata(
    store: &Store,
    outer: &CommandEnvelope,
    mutation: MetadataMutation,
    grant: &MetadataAuthority,
    time: i64,
    nfo: Option<&crate::catalog_nfo_attribution::source_effect::ImportedSourceEffect<'_>>,
    validated: Option<&crate::catalog_import::ValidatedImportSource<'_>>,
) -> Result<Value> {
    // This internal envelope retains the ONE outer receipt key/incarnation. Its
    // current generation and exact owner payload describe this composition step,
    // after the outer snapshot was fenced and earlier writes advanced generation.
    // It is not admitted separately and never inserts an independent receipt.
    let command = context(
        store,
        outer,
        json!({"operation":"metadata","mutation":mutation}),
    )?;
    let applied = if nfo.is_some() || validated.is_some() {
        metadata::apply_metadata_import(store, &mutation, &command, grant, time, nfo, validated)
    } else {
        metadata::apply_metadata(&store.db, &mutation, &command, grant, time)
    };
    applied.map_err(|e| {
        error(
            if e.code == metadata::MetadataStoreErrorCode::AuthorityDenied {
                ImportErrorCode::AuthorityDenied
            } else {
                ImportErrorCode::SnapshotConflict
            },
            &e.to_string(),
        )
    })
}
pub(crate) fn apply(
    store: &Store,
    outer: &CommandEnvelope,
    batches: &[TypedBatch],
    items: &BTreeSet<String>,
    grants: Option<&TypedAuthority>,
    time: i64,
) -> Result<Vec<Value>> {
    apply_with_origin(store, outer, batches, items, grants, time, "migration")
}
pub(crate) fn apply_validated<'a>(
    store: &Store,
    outer: &CommandEnvelope,
    batches: &'a [TypedBatch],
    items: &BTreeSet<String>,
    grants: Option<&TypedAuthority>,
    time: i64,
    validated: &crate::catalog_import::ValidatedNfoImport<'a>,
) -> Result<Vec<Value>> {
    validated.check(outer, batches)?;
    apply_with_origin_inner(
        store,
        outer,
        batches,
        items,
        grants,
        time,
        "migration",
        Some(validated),
    )
}
pub(crate) fn apply_with_origin(
    store: &Store,
    outer: &CommandEnvelope,
    batches: &[TypedBatch],
    items: &BTreeSet<String>,
    grants: Option<&TypedAuthority>,
    time: i64,
    association_origin: &str,
) -> Result<Vec<Value>> {
    apply_with_origin_inner(
        store,
        outer,
        batches,
        items,
        grants,
        time,
        association_origin,
        None,
    )
}
fn apply_with_origin_inner<'a>(
    store: &Store,
    outer: &CommandEnvelope,
    batches: &'a [TypedBatch],
    items: &BTreeSet<String>,
    grants: Option<&TypedAuthority>,
    time: i64,
    association_origin: &str,
    validated: Option<&crate::catalog_import::ValidatedNfoImport<'a>>,
) -> Result<Vec<Value>> {
    if batches.is_empty() {
        return Ok(vec![]);
    }
    let grants = grants.ok_or_else(|| {
        error(
            ImportErrorCode::AuthorityDenied,
            "typed import requires independently established owner grants",
        )
    })?;
    let mut all_items = items.clone();
    for (item_id, grant) in &grants.existing_items {
        if grant.host_namespace != outer.key().host_namespace()
            || grant.catalog_id != outer.key().catalog_id()
            || grant.incarnation != outer.admission_incarnation()
            || items.contains(item_id)
        {
            return Err(error(
                ImportErrorCode::AuthorityDenied,
                "existing import endpoint identity/scope mismatch",
            ));
        }
        if existing_item_revision(store, item_id)? != grant.item_revision {
            return Err(error(
                ImportErrorCode::SnapshotConflict,
                "existing import endpoint revision changed",
            ));
        }
        all_items.insert(item_id.clone());
    }
    let items = &all_items;
    let nfo_effects = prepare_nfo_import(store, outer, batches, items, grants, validated)?;

    let mut results = Vec::new();
    let mut subjects = BTreeSet::new();
    let mut pin_pairs = BTreeSet::new();
    let mut tag_pairs = BTreeSet::new();
    for batch in batches {
        batch.validate()?;
        for tags in &batch.tags {
            if !items.contains(&tags.item_id) || !tag_pairs.insert((&tags.item_id, &tags.namespace))
            {
                return Err(invalid(
                    "tag import addresses each newly imported item/source once",
                ));
            }
            let grant = grants
                .tag_sources
                .iter()
                .find(|g| {
                    g.host_namespace == outer.key().host_namespace()
                        && g.namespace == tags.namespace
                        && g.item_ids.contains(&tags.item_id)
                        && (tags.excluded_tags.is_empty() || g.allow_suppressions)
                })
                .ok_or_else(|| {
                    error(
                        ImportErrorCode::AuthorityDenied,
                        "missing independent tag source/suppression grant",
                    )
                })?;
            results.extend(crate::tagging::import_tag_set_authorized(
                store,
                outer,
                tags,
                grant,
                time,
                if tags.namespace == "motion.nfo" {
                    nfo_effects.get(tags.item_id.as_str())
                } else {
                    None
                },
            )?);
        }
        for m in &batch.metadata {
            if !items.contains(&m.item_id) || !subjects.insert(&m.item_id) {
                return Err(invalid(
                    "metadata import can initialize each newly imported item once",
                ));
            }
            let policies: Vec<PolicyField> = serde_json::from_value(m.policies.clone())
                .map_err(|_| invalid("invalid metadata policies"))?;
            if m.contract_version == 2 {
                let request = metadata::MetadataInitializeV2 {
                    contract_version: 2,
                    subject: m.item_id.clone(),
                    policies,
                    system_fields: m.system_fields.iter().cloned().collect(),
                };
                let command = context(
                    store,
                    outer,
                    json!({"operation":"metadata_initialize_v2","request":request}),
                )?;
                metadata::initialize_v2(
                    store,
                    &request,
                    &command,
                    metadata_grant(grants, outer.key().host_namespace(), &m.item_id, None)?,
                    time,
                )
                .map_err(|e| error(ImportErrorCode::SnapshotConflict, &e.to_string()))?;
            } else {
                apply_metadata(
                    store,
                    outer,
                    MetadataMutation {
                        contract_version: 1,
                        change: MetadataChange::Initialize {
                            subject: m.item_id.clone(),
                            policies,
                        },
                    },
                    metadata_grant(grants, outer.key().host_namespace(), &m.item_id, None)?,
                    time,
                    None,
                    None,
                )?;
            }
            let fingerprint: String = store
                .db
                .query_row(
                    "SELECT schema_fingerprint FROM catalog_metadata_subjects WHERE item_id=?1",
                    [&m.item_id],
                    |r| r.get(0),
                )
                .map_err(|e| error(ImportErrorCode::StoreFailure, &e.to_string()))?;
            for c in &m.contributions {
                let validated_source =
                    if store.schema_version == 41 && c.fields.get("motion_catalog").is_some() {
                        Some(
                            validated
                                .ok_or_else(|| {
                                    error(
                                        ImportErrorCode::AuthorityDenied,
                                        "protected import requires complete validated batch",
                                    )
                                })?
                                .source_entry(&m.item_id, &c.namespace, &c.record_id)?,
                        )
                    } else {
                        None
                    };
                if m.contract_version == 2 {
                    let evidence = json!({"import_operation_id":outer.key().operation_id(),"source_revision":c.source_revision,"source_evidence":c.evidence});
                    let command = context(
                        store,
                        outer,
                        json!({"operation":"metadata_import_source_v2","subject":m.item_id,"namespace":c.namespace,"record_id":c.record_id,"fields":c.fields,"evidence":evidence}),
                    )?;
                    metadata::import_source_v2_authorized(
                        store,
                        &m.item_id,
                        &c.namespace,
                        &c.record_id,
                        &c.fields,
                        &evidence,
                        &command,
                        metadata_grant(grants, outer.key().host_namespace(), &m.item_id, Some(c))?,
                        time,
                        if c.namespace == "motion.nfo" {
                            nfo_effects.get(m.item_id.as_str())
                        } else {
                            None
                        },
                        validated_source.as_ref(),
                    )
                    .map_err(|e| error(ImportErrorCode::SnapshotConflict, &e.to_string()))?;
                    results.push(json!({"owner":"metadata","item_id":m.item_id,"namespace":c.namespace,"record_id":c.record_id,"source_revision":c.source_revision,"catalog_revision":1,"schema_fingerprint":fingerprint}));
                    continue;
                }
                let edits: serde_json::Map<String, Value> = c
                    .fields
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.clone(), json!({"op":"set","value":v})))
                    .collect();
                let evidence = json!({"import_operation_id":outer.key().operation_id(),"source_revision":c.source_revision,"source_evidence":c.evidence});
                let mutation = MetadataMutation {
                    contract_version: 1,
                    change: MetadataChange::Patch {
                        subject: m.item_id.clone(),
                        namespace: c.namespace.clone(),
                        record_id: c.record_id.clone(),
                        schema_fingerprint: fingerprint.clone(),
                        expected_revision: 0,
                        edits_json: Value::Object(edits).to_string(),
                        evidence_json: evidence.to_string(),
                    },
                };
                let _applied = apply_metadata(
                    store,
                    outer,
                    mutation,
                    metadata_grant(grants, outer.key().host_namespace(), &m.item_id, Some(c))?,
                    time,
                    if c.namespace == "motion.nfo" {
                        nfo_effects.get(m.item_id.as_str())
                    } else {
                        None
                    },
                    validated_source.as_ref(),
                )?;
                results.push(json!({"owner":"metadata","item_id":m.item_id,"namespace":c.namespace,"record_id":c.record_id,"source_revision":c.source_revision,"catalog_revision":1,"schema_fingerprint":fingerprint}));
            }
        }
        if matches!(store.schema_version, 40 | 41) {
            // Establish suppression ownership only from this exact import's
            // native tag decision and source provenance, never from names alone.
            for m in &batch.metadata {
                store
                    .db
                    .execute(
                        "INSERT INTO catalog_metadata_permits(item_id,incarnation) VALUES (?,?)",
                        rusqlite::params![m.item_id, outer.admission_incarnation()],
                    )
                    .map_err(|e| error(ImportErrorCode::StoreFailure, &e.to_string()))?;
                store.db.execute("INSERT INTO catalog_metadata_tag_suppressions(item_id,tag_id,namespace,revision) SELECT s.item_id,s.tag_id,'motion.local',s.revision FROM catalog_item_tag_suppressions s JOIN catalog_tag_suppression_revisions h ON h.subject_type='item' AND h.subject_id=s.item_id AND h.tag_id=s.tag_id AND h.revision=s.revision JOIN catalog_metadata_sources m ON m.item_id=s.item_id AND m.namespace='motion.local' AND m.record_id='document' WHERE s.item_id=?1 AND json_extract(h.reason,'$.import_operation_id')=?2 AND json_extract(h.reason,'$.import_operation_id')=json_extract(m.evidence_json,'$.import_operation_id') AND json_extract(h.reason,'$.source_revision')=json_extract(m.evidence_json,'$.source_revision')",rusqlite::params![m.item_id,outer.key().operation_id()]).map_err(|e|error(ImportErrorCode::StoreFailure,&e.to_string()))?;
                store
                    .db
                    .execute(
                        "DELETE FROM catalog_metadata_permits WHERE item_id=?",
                        [&m.item_id],
                    )
                    .map_err(|e| error(ImportErrorCode::StoreFailure, &e.to_string()))?;
            }
        }
        for p in &batch.pins {
            if !items.contains(&p.item_id) || !pin_pairs.insert((&p.item_id, &p.namespace)) {
                return Err(invalid(
                    "pin import can address each new item/namespace once",
                ));
            }
            let grant = grants.pins.as_ref().ok_or_else(|| {
                error(
                    ImportErrorCode::AuthorityDenied,
                    "missing independent pin owner grant",
                )
            })?;
            for (index, h) in p.history.iter().enumerate() {
                let batch = PinBatch {
                    contract_version: 1,
                    changes: vec![PinChange {
                        item_id: p.item_id.clone(),
                        namespace: p.namespace.clone(),
                        expected_revision: index as i64,
                        decision: h.decision.clone(),
                    }],
                };
                let preview = pins::preview_pins_in_transaction(store, &batch, grant)
                    .map_err(|e| error(ImportErrorCode::SnapshotConflict, &e.to_string()))?;
                let mutation = PinMutation {
                    batch,
                    preview_digest: preview.digest,
                };
                let command = context(
                    store,
                    outer,
                    json!({"operation":"pins","mutation":mutation}),
                )?;
                let _applied =
                    pins::apply_pins(store, &mutation, &command, grant, time).map_err(|e| {
                        error(
                            if e.code == pins::PinErrorCode::AuthorityDenied {
                                ImportErrorCode::AuthorityDenied
                            } else {
                                ImportErrorCode::SnapshotConflict
                            },
                            &e.to_string(),
                        )
                    })?;
                results.push(json!({"owner":"pins","item_id":p.item_id,"namespace":p.namespace,"source_revision":h.source_revision,"catalog_revision":index+1}));
            }
        }
    }
    let mut relationship_ids = BTreeSet::new();
    let mut relationship_edges = BTreeSet::new();
    for relationship in batches.iter().flat_map(|batch| &batch.relationships) {
        let edge = (
            relationship.source_item_id.clone(),
            relationship.target_item_id.clone(),
            relationship.kind.clone(),
        );
        if !items.contains(&relationship.source_item_id)
            || !items.contains(&relationship.target_item_id)
            || !relationship_ids.insert(&relationship.source_relationship_id)
            || !relationship_edges.insert(edge.clone())
        {
            return Err(invalid(
                "relationship endpoints must be imported and edges unique",
            ));
        }
        let grant = grants
            .relationships
            .as_ref()
            .filter(|g| g.host_namespace == outer.key().host_namespace() && g.edges.contains(&edge))
            .ok_or_else(|| {
                error(
                    ImportErrorCode::AuthorityDenied,
                    "missing independent relationship owner grant",
                )
            })?;
        let _ = grant;
        if store
            .db
            .prepare(
                "SELECT 1 FROM item_relationships WHERE source_id=? AND target_id=? AND kind=?",
            )?
            .exists(rusqlite::params![
                relationship.source_item_id,
                relationship.target_item_id,
                relationship.kind
            ])?
        {
            return Err(error(
                ImportErrorCode::SnapshotConflict,
                "import relationship edge already exists",
            ));
        }
        let result = crate::curation::relate(store, &json!({"source":relationship.source_item_id,
            "target":relationship.target_item_id,"kind":relationship.kind,"position":relationship.position,
            "metadata":relationship.metadata}))
            .map_err(|e| error(ImportErrorCode::SnapshotConflict, &e.to_string()))?;
        results.push(json!({"owner":"relationships","source_relationship_id":relationship.source_relationship_id,
            "source_revision":relationship.source_revision,"relationship_id":result["id"],
            "source_item_id":relationship.source_item_id,"target_item_id":relationship.target_item_id,
            "kind":relationship.kind,"position":relationship.position}));
    }
    let native: Vec<_> = batches.iter().filter_map(|b| b.native.as_ref()).collect();
    results.extend(apply_native(
        store,
        outer,
        &native,
        items,
        grants,
        time,
        association_origin,
    )?);
    let mut alias_sources = BTreeSet::new();
    for alias in batches.iter().flat_map(|batch| &batch.logical_aliases) {
        if !items.contains(&alias.alias_item_id)
            || !items.contains(&alias.survivor_item_id)
            || !alias_sources.insert(&alias.alias_item_id)
        {
            return Err(invalid(
                "logical alias endpoints must be in exact import scope and sources unique",
            ));
        }
        let grant = grants
            .logical_aliases
            .as_ref()
            .filter(|g| {
                g.host_namespace == outer.key().host_namespace()
                    && g.aliases
                        .contains(&(alias.alias_item_id.clone(), alias.survivor_item_id.clone()))
            })
            .ok_or_else(|| {
                error(
                    ImportErrorCode::AuthorityDenied,
                    "missing independent logical alias grant",
                )
            })?;
        let authority = crate::catalog_logical_edits::LogicalEditAuthority {
            host_namespace: grant.host_namespace.clone(),
            item_ids: grant.item_ids.clone(),
            new_item_ids: BTreeSet::new(),
        };
        let command = context(store, outer, json!({"imported_logical_alias":alias}))?;
        let result = crate::catalog_logical_edits::import_reviewed_alias(
            store,
            &alias.alias_item_id,
            &alias.survivor_item_id,
            &command,
            &authority,
            grants.identity.as_ref(),
            time,
        )
        .map_err(|e| error(ImportErrorCode::SnapshotConflict, &e.to_string()))?;
        results.push(json!({"owner":"logical_aliases","alias_item_id":alias.alias_item_id,
            "survivor_item_id":alias.survivor_item_id,"source_receipt_id":alias.source_receipt_id,
            "source_receipt_json":alias.source_receipt_json,"reference_map":result["reference_map"]}));
    }

    for (_, effect) in nfo_effects {
        effect.finish_in(store, time)?;
    }
    Ok(results)
}

// The validate command rehearses the exact existing owners inside a nested
// savepoint and deliberately rolls it back. Only the resulting mapping proposal
// survives in the outer validation receipt; no staged row becomes active.
enum Rehearsal {
    Complete(Vec<Value>),
    Failed(ImportError),
}
impl std::fmt::Display for Rehearsal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Complete(_) => f.write_str("import rehearsal rollback"),
            Self::Failed(e) => e.fmt(f),
        }
    }
}
impl From<Error> for Rehearsal {
    fn from(error: Error) -> Self {
        Self::Failed(ImportError {
            code: ImportErrorCode::StoreFailure,
            error,
        })
    }
}
pub(crate) fn rehearse(
    store: &Store,
    command: &CommandEnvelope,
    items: &[crate::catalog_import::ImportItem],
    batches: &[TypedBatch],
    grants: Option<&TypedAuthority>,
    time: i64,
) -> Result<Vec<Value>> {
    rehearse_inner(store, command, items, batches, grants, time, None)
}
pub(crate) fn rehearse_validated<'a>(
    store: &Store,
    command: &CommandEnvelope,
    items: &[crate::catalog_import::ImportItem],
    batches: &'a [TypedBatch],
    grants: Option<&TypedAuthority>,
    time: i64,
    validated: &crate::catalog_import::ValidatedNfoImport<'a>,
) -> Result<Vec<Value>> {
    validated.check(command, batches)?;
    rehearse_inner(
        store,
        command,
        items,
        batches,
        grants,
        time,
        Some(validated),
    )
}
fn rehearse_inner<'a>(
    store: &Store,
    command: &CommandEnvelope,
    items: &[crate::catalog_import::ImportItem],
    batches: &'a [TypedBatch],
    grants: Option<&TypedAuthority>,
    time: i64,
    validated: Option<&crate::catalog_import::ValidatedNfoImport<'a>>,
) -> Result<Vec<Value>> {
    if batches.is_empty() {
        return Ok(vec![]);
    }
    let imported = items.iter().map(|i| i.item_id.clone()).collect();
    let result: std::result::Result<(), Rehearsal> = store.transaction_detailed(|_| {
        crate::catalog_import::create_import_items(store, items).map_err(Rehearsal::Failed)?;
        let mapping = apply_with_origin_inner(
            store,
            command,
            batches,
            &imported,
            grants,
            time,
            "migration",
            validated,
        )
        .map_err(Rehearsal::Failed)?;
        Err(Rehearsal::Complete(mapping))
    });
    match result {
        Err(Rehearsal::Complete(mapping)) => Ok(mapping),
        Err(Rehearsal::Failed(error)) => Err(error),
        Ok(()) => Err(error(
            ImportErrorCode::StoreFailure,
            "rehearsal unexpectedly committed",
        )),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhysicalMapping {
    pub source_occurrence_id: String,
    pub profile: String,
    pub location: String,
    pub path: String,
    pub exact: catabolic_core::catalog_identity::ExactFileRevision,
    pub observed_status: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedAssociation {
    pub source_association_id: String,
    pub source_occurrence_id: String,
    pub item_id: String,
    pub part: u32,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeIdentityBatch {
    pub occurrences: Vec<PhysicalMapping>,
    pub associations: Vec<ImportedAssociation>,
    /// Strictly parsed by the existing IdentityMutation type/validator. Binding
    /// association IDs name explicit source_association_id records in this batch;
    /// only those exact references are replaced with the curation owner's IDs.
    pub mutations: Vec<Value>,
}
impl NativeIdentityBatch {
    fn validate(&self) -> Result<()> {
        for m in &self.occurrences {
            if !id(&m.source_occurrence_id)
                || !id(&m.profile)
                || !id(&m.location)
                || m.path.is_empty()
                || m.path.len() > 4096
                || m.exact.validate().is_err()
            {
                return Err(invalid("invalid exact occurrence mapping"));
            }
        }
        for a in &self.associations {
            if !id(&a.source_association_id)
                || !id(&a.source_occurrence_id)
                || !id(&a.item_id)
                || a.part == 0
            {
                return Err(invalid("invalid imported association"));
            }
        }
        for raw in &self.mutations {
            let mutation: crate::catalog_identity_mutation::IdentityMutation =
                serde_json::from_value(raw.clone())
                    .map_err(|_| invalid("invalid native identity mutation"))?;
            mutation.validate().map_err(|e| invalid(&e.to_string()))?;
            if mutation
                .timelines
                .iter()
                .any(|t| t.expected_revision.is_some())
                || mutation.versions.iter().any(|v| {
                    v.expected_revision.is_some()
                        || v.expected_binding_revisions.values().any(Option::is_some)
                })
            {
                return Err(invalid(
                    "import only creates new timelines/versions/bindings; replacing existing identities requires separate reviewed mutation",
                ));
            }
        }
        Ok(())
    }
}
fn physical_grant(grant: &TypedAuthority, profile: &str, file: &str) -> Result<()> {
    if !grant
        .physical_files
        .get(profile)
        .is_some_and(|ids| ids.contains(file))
    {
        return Err(error(
            ImportErrorCode::AuthorityDenied,
            "exact occurrence mapping requires current physical read grant",
        ));
    }
    Ok(())
}
/// Privileged bounded read through the existing occurrence/revision owner. No
/// filesystem scan, source registration, digest-based matching or availability
/// claim is performed. The host supplies an independently admitted exact file set.
pub fn capture_existing_occurrence(
    store: &Store,
    source_occurrence_id: &str,
    profile: &str,
    file: &str,
    grant: &TypedAuthority,
) -> Result<PhysicalMapping> {
    if !id(source_occurrence_id) || !store.db.is_autocommit() {
        return Err(invalid(
            "occurrence capture needs a bounded source ID and top-level snapshot",
        ));
    }
    physical_grant(grant, profile, file)?;
    let snapshot = store
        .transaction(|db| crate::components::occurrence(db, profile, file))
        .map_err(|e| error(ImportErrorCode::StoreFailure, &e.to_string()))?;
    Ok(PhysicalMapping {
        source_occurrence_id: source_occurrence_id.into(),
        profile: profile.into(),
        location: snapshot["location"]
            .as_str()
            .ok_or_else(|| invalid("invalid occurrence location"))?
            .into(),
        path: snapshot["path"]
            .as_str()
            .ok_or_else(|| invalid("invalid occurrence path"))?
            .into(),
        exact: catabolic_core::catalog_identity::ExactFileRevision {
            file_id: file.into(),
            revision: crate::components::revision(&snapshot),
        },
        observed_status: snapshot["status"].as_str().map(str::to_owned),
    })
}
fn apply_native(
    store: &Store,
    outer: &CommandEnvelope,
    batches: &[&NativeIdentityBatch],
    items: &BTreeSet<String>,
    grant: &TypedAuthority,
    time: i64,
    association_origin: &str,
) -> Result<Vec<Value>> {
    use crate::{catalog_identity_mutation, curation};
    let mut occurrences = BTreeMap::new();
    let mut destinations = BTreeSet::new();
    let mut associations = BTreeMap::new();
    let mut results = Vec::new();
    let mut used_associations = BTreeSet::new();
    let mut new_timelines = BTreeSet::new();
    for batch in batches {
        for raw in &batch.mutations {
            let mutation: catalog_identity_mutation::IdentityMutation =
                serde_json::from_value(raw.clone())
                    .map_err(|_| invalid("invalid identity mutation"))?;
            for timeline in mutation.timelines {
                new_timelines.insert(timeline.timeline_id);
            }
        }
    }
    for batch in batches {
        for m in &batch.occurrences {
            physical_grant(grant, &m.profile, &m.exact.file_id)?;
            if occurrences
                .insert(m.source_occurrence_id.clone(), m)
                .is_some()
                || !destinations.insert((&m.profile, &m.exact.file_id))
            {
                return Err(invalid(
                    "duplicate occurrence source or many-to-one physical mapping",
                ));
            }
            let current = crate::components::occurrence(&store.db, &m.profile, &m.exact.file_id)
                .map_err(|e| error(ImportErrorCode::SnapshotConflict, &e.to_string()))?;
            if current["location"] != m.location
                || current["path"] != m.path
                || current["status"].as_str() != m.observed_status.as_deref()
                || crate::components::revision(&current) != m.exact.revision
            {
                return Err(error(
                    ImportErrorCode::SnapshotConflict,
                    "source/path/exact revision changed; rename or replacement cannot be inferred",
                ));
            }
            results.push(json!({"owner":"occurrence","source_occurrence_id":m.source_occurrence_id,"profile":m.profile,"file_id":m.exact.file_id,"file_revision":m.exact.revision,"location":m.location,"path":m.path,"observed_status":m.observed_status,"source_availability":"unknown"}));
        }
    }
    for batch in batches {
        for a in &batch.associations {
            if !items.contains(&a.item_id) {
                return Err(invalid("association can only target a newly imported item"));
            }
            let m = occurrences
                .get(&a.source_occurrence_id)
                .ok_or_else(|| invalid("association requires explicit physical mapping"))?;
            let target=curation::associate_in_transaction(store,&m.profile,&store.db,&json!({"file_id":m.exact.file_id,"item_id":a.item_id,"role":"primary","part":a.part,"metadata":{}}),association_origin).map_err(|e|error(ImportErrorCode::SnapshotConflict,&e.to_string()))?;
            if associations
                .insert(a.source_association_id.clone(), (target.clone(), *m, a))
                .is_some()
            {
                return Err(invalid("duplicate source association ID"));
            }
            results.push(json!({"owner":"association","source_association_id":a.source_association_id,"association_id":target,"item_id":a.item_id,"file_id":m.exact.file_id,"file_revision":m.exact.revision,"part":a.part}));
        }
    }
    for batch in batches {
        for raw in &batch.mutations {
            let mut mutation: catalog_identity_mutation::IdentityMutation =
                serde_json::from_value(raw.clone())
                    .map_err(|_| invalid("invalid identity mutation"))?;
            if let Some(expected) = mutation.context.expected_revision {
                let prior = grant
                    .existing_contexts
                    .get(&mutation.context.id)
                    .filter(|prior| {
                        prior.host_namespace == outer.key().host_namespace()
                            && prior.catalog_id == outer.key().catalog_id()
                            && prior.incarnation == outer.admission_incarnation()
                            && prior.authority_namespace == mutation.context.authority_namespace
                            && prior.context_key == mutation.context.context_key
                            && prior.revision == expected
                    })
                    .ok_or_else(|| {
                        error(
                            ImportErrorCode::AuthorityDenied,
                            "missing exact prior context continuation grant",
                        )
                    })?;
                let _ = prior;
                // apply_identity checks the actual head and complete retained
                // membership again under this same writer transaction.
            }
            for t in &mutation.timelines {
                if !items.contains(&t.edition_item_id) {
                    return Err(invalid(
                        "timeline must belong to a new imported edition item",
                    ));
                }
            }
            for version in &mut mutation.versions {
                if !new_timelines.contains(&version.version.timeline_id) {
                    grant
                        .existing_timelines
                        .get(&version.version.timeline_id)
                        .filter(|prior| {
                            prior.host_namespace == outer.key().host_namespace()
                                && prior.catalog_id == outer.key().catalog_id()
                                && prior.incarnation == outer.admission_incarnation()
                                && prior.edition_item_id == version.version.edition_item_id
                                && prior.revision == version.expected_timeline_revision
                        })
                        .ok_or_else(|| {
                            error(
                                ImportErrorCode::AuthorityDenied,
                                "missing exact existing timeline grant",
                            )
                        })?;
                }
                if !items.contains(&version.version.association_item_id)
                    || !items.contains(&version.version.edition_item_id)
                {
                    return Err(invalid("version owners must be new imported items"));
                }
                for binding in &mut version.version.bindings {
                    let (target, m, a) =
                        associations.get(&binding.association_id).ok_or_else(|| {
                            invalid("binding must use an explicit imported association mapping")
                        })?;
                    if m.profile != mutation.profile
                        || m.exact != binding.file
                        || a.item_id != version.version.association_item_id
                        || a.part != binding.part
                    {
                        return Err(error(
                            ImportErrorCode::SnapshotConflict,
                            "binding does not match exact physical/association mapping",
                        ));
                    }
                    if !used_associations.insert(binding.association_id.clone()) {
                        return Err(invalid(
                            "each imported association requires exactly one typed binding",
                        ));
                    }
                    binding.association_id = target.clone();
                }
            }
            let identity_grant = grant.identity.as_ref().ok_or_else(|| {
                error(
                    ImportErrorCode::AuthorityDenied,
                    "missing independent editorial identity grant",
                )
            })?;
            let payload = serde_json::to_value(&mutation)
                .map_err(|_| invalid("cannot encode identity mutation"))?;
            let command = context(
                store,
                outer,
                json!({"operation":"identity","mutation":payload}),
            )?;
            let applied = catalog_identity_mutation::apply_identity(
                store,
                &payload,
                &command,
                identity_grant,
                time,
            )
            .map_err(|e| {
                error(
                    if e.code == catalog_identity_mutation::IdentityErrorCode::AuthorityDenied {
                        ImportErrorCode::AuthorityDenied
                    } else {
                        ImportErrorCode::SnapshotConflict
                    },
                    &e.to_string(),
                )
            })?;
            results.push(json!({"owner":"identity","result":applied}));
        }
    }
    if used_associations.len() != associations.len() {
        return Err(invalid(
            "every imported association requires an exact-revision typed binding",
        ));
    }
    Ok(results)
}

/// Bounded same-owner physical snapshot for a trusted host adapter. Paths select
/// observations, but confer no identity or mutation grant. All returned exact
/// revisions, binding, policy and generation belong to this single transaction.
pub fn capture_source_occurrences(
    store: &Store,
    profile: &str,
    source: &str,
) -> crate::Result<Value> {
    crate::query::name(source)?;
    crate::query::name(profile)?;
    if !store.db.is_autocommit() {
        return Err(crate::Error(
            "source occurrence capture requires top-level snapshot".into(),
        ));
    }
    store.transaction(|_| capture_source_occurrences_in(store, profile, source))
}
/// Same complete occurrence owner under the caller's retained snapshot.
pub(crate) fn capture_source_occurrences_in(
    store: &Store,
    profile: &str,
    source: &str,
) -> crate::Result<Value> {
    crate::query::name(source)?;
    crate::query::name(profile)?;
    if store.db.is_autocommit() {
        return Err(crate::Error(
            "source occurrence capture requires retained snapshot".into(),
        ));
    }
    let db = &store.db;

    let binding = crate::observation::binding(store, profile, "source", source)?;
    let policy =
        crate::observation::execute(store, profile, "scan_policy", &json!({"source":source}))?;
    let occurrences = capture_source_occurrence_rows(db, profile, source)?;
    let generation: i64 =
        db.query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
            r.get(0)
        })?;
    Ok(
        json!({"catalog_id":store.database_id,"catalog_incarnation":store.catalog_incarnation,"schema_version":store.schema_version,"profile":profile,"generation":generation,"binding":binding,"policy":policy,"occurrences":occurrences}),
    )
}

// The fixed endpoint owner borrows only the already-fresh source header. Every
// file, tuple, identity policy, volume UUID, exact revision and ctime is still
// captured by the same complete loop as the ordinary owner.
pub(crate) fn capture_source_occurrence_rows_for_endpoint_in(
    store: &Store,
    facts: &crate::migration::EndpointFreshFacts<'_, '_>,
    source: &str,
    budget: &mut crate::catalog_nfo_attribution::ProofBudget,
) -> crate::Result<Vec<Value>> {
    facts.require_source(store, source, budget)?;
    let profile = facts.scope().profile();
    crate::query::name(source)?;
    crate::query::name(profile)?;
    let rows = capture_source_occurrence_rows(&store.db, profile, source)?;
    facts.check(store, budget)?;
    Ok(rows)
}
fn capture_source_occurrence_rows(
    db: &rusqlite::Connection,
    profile: &str,
    source: &str,
) -> crate::Result<Vec<Value>> {
    use rusqlite::OptionalExtension;
    let count: i64 = db.query_row(
        "SELECT count(*) FROM files WHERE location=?",
        [source],
        |r| r.get(0),
    )?;
    if count > 100_000 {
        return Err(crate::Error(
            "source occurrence snapshot exceeds 100000; explicit paging required".into(),
        ));
    }
    let ids = db
        .prepare("SELECT id FROM files WHERE location=? ORDER BY id")?
        .query_map([source], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut occurrences = Vec::with_capacity(ids.len());
    for file in ids {
        let mut observed = crate::components::occurrence(db, profile, &file)?;
        let revision = crate::components::revision(&observed);
        let ctime: Option<i64> = db.query_row("SELECT c.ctime_ns FROM native_source_ctimes c JOIN observations o ON o.profile=c.profile AND o.file_id=c.file_id AND o.size=c.size AND o.mtime_ns=c.mtime_ns AND o.device=c.device AND o.inode=c.inode WHERE c.profile=? AND c.file_id=? AND o.status='present'", [profile,&file], |r| r.get(0)).optional()?;
        observed["ctime_ns"] = json!(ctime);
        observed["exact_revision"] = json!(revision);
        occurrences.push(observed);
    }
    Ok(occurrences)
}

/// Exact source registration for the retained native host owner. Replays can
/// adopt the identical root and policy, never replace an existing binding.
pub fn register_host_source(
    store: &Store,
    profile: &str,
    source: &str,
    root: &std::path::Path,
    exclusions: &[String],
) -> crate::Result<Value> {
    use std::os::unix::fs::MetadataExt;
    crate::query::name(profile)?;
    crate::query::name(source)?;
    let root = root.canonicalize()?;
    let metadata = std::fs::metadata(&root)?;
    if !metadata.is_dir() {
        return Err(crate::Error("source root must be a directory".into()));
    }
    let root = root
        .to_str()
        .ok_or_else(|| crate::Error("source root must be UTF-8".into()))?;
    store.transaction(|_| {
        let exists=store.db.prepare("SELECT 1 FROM bindings WHERE profile=? AND kind='source' AND owner=?")?.exists([profile,source])?;
        let binding=if exists {crate::observation::binding(store,profile,"source",source)?} else {crate::observation::execute(store,profile,"bind",&json!({"kind":"source","owner":source,"root":root}))?};
        if binding["root"].as_str()!=Some(root) || binding["device"].as_u64()!=Some(metadata.dev()) || binding["inode"].as_u64()!=Some(metadata.ino()) {return Err(crate::Error("source binding differs from exact requested root".into()));}
        let before=crate::observation::execute(store,profile,"scan_policy",&json!({"source":source}))?;
        let compatible=before["exclusions"]==json!(exclusions)&&before["extensions"].is_null()&&before["include_regex"]==json!([])&&before["exclude_regex"]==json!([]);
        if !compatible {
            if before["revision"].as_i64()!=Some(0) {return Err(crate::Error("source policy differs from exact requested scope".into()));}
            crate::observation::execute(store,profile,"scan_policy",&json!({"source":source,"apply":true,"policy":{"exclusions":exclusions,"extensions":null,"include_regex":[],"exclude_regex":[]}}))?;
        }
        let policy=crate::observation::execute(store,profile,"scan_policy",&json!({"source":source}))?;
        Ok(json!({"binding":binding,"policy":policy}))
    })
}
