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
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportedSourceKind {
    Local,
    Provider,
    Scan,
}
impl ImportedSourceKind {
    fn owner(self) -> SourceKind {
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedMetadata {
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
        self.metadata.len()
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
        let mut subjects = BTreeSet::new();
        let mut pairs = BTreeSet::new();
        for m in &self.metadata {
            if !id(&m.item_id) || !subjects.insert(&m.item_id) {
                return Err(invalid("invalid or duplicate metadata subject"));
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
                    (MetadataAccess::Initialize, None) => true,
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
    metadata::apply_metadata(&store.db, &mutation, &command, grant, time).map_err(|e| {
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
    if batches.is_empty() {
        return Ok(vec![]);
    }
    let grants = grants.ok_or_else(|| {
        error(
            ImportErrorCode::AuthorityDenied,
            "typed import requires independently established owner grants",
        )
    })?;
    let mut results = Vec::new();
    let mut subjects = BTreeSet::new();
    let mut pin_pairs = BTreeSet::new();
    for batch in batches {
        batch.validate()?;
        for m in &batch.metadata {
            if !items.contains(&m.item_id) || !subjects.insert(&m.item_id) {
                return Err(invalid(
                    "metadata import can initialize each newly imported item once",
                ));
            }
            let policies: Vec<PolicyField> = serde_json::from_value(m.policies.clone())
                .map_err(|_| invalid("invalid metadata policies"))?;
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
            )?;
            let fingerprint: String = store
                .db
                .query_row(
                    "SELECT schema_fingerprint FROM catalog_metadata_subjects WHERE item_id=?1",
                    [&m.item_id],
                    |r| r.get(0),
                )
                .map_err(|e| error(ImportErrorCode::StoreFailure, &e.to_string()))?;
            for c in &m.contributions {
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
                )?;
                results.push(json!({"owner":"metadata","item_id":m.item_id,"namespace":c.namespace,"record_id":c.record_id,"source_revision":c.source_revision,"catalog_revision":1,"schema_fingerprint":fingerprint}));
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
    let native: Vec<_> = batches.iter().filter_map(|b| b.native.as_ref()).collect();
    results.extend(apply_native(store, outer, &native, items, grants, time)?);
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
    if batches.is_empty() {
        return Ok(vec![]);
    }
    let imported = items.iter().map(|i| i.item_id.clone()).collect();
    let result: std::result::Result<(), Rehearsal> = store.transaction_detailed(|_| {
        crate::catalog_import::create_import_items(store, items).map_err(Rehearsal::Failed)?;
        let mapping =
            apply(store, command, batches, &imported, grants, time).map_err(Rehearsal::Failed)?;
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
            if mutation.context.expected_revision.is_some()
                || mutation
                    .timelines
                    .iter()
                    .any(|t| t.expected_revision.is_some())
                || mutation.versions.iter().any(|v| {
                    v.expected_revision.is_some()
                        || v.expected_binding_revisions.values().any(Option::is_some)
                })
            {
                return Err(invalid(
                    "import only creates new typed identities; existing identities require separate reviewed mutation",
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
) -> Result<Vec<Value>> {
    use crate::{catalog_identity_mutation, curation};
    let mut occurrences = BTreeMap::new();
    let mut destinations = BTreeSet::new();
    let mut associations = BTreeMap::new();
    let mut results = Vec::new();
    let mut used_associations = BTreeSet::new();
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
            let target=curation::associate_in_transaction(store,&m.profile,&store.db,&json!({"file_id":m.exact.file_id,"item_id":a.item_id,"role":"primary","part":a.part,"metadata":{}}),"migration").map_err(|e|error(ImportErrorCode::SnapshotConflict,&e.to_string()))?;
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
            for t in &mutation.timelines {
                if !items.contains(&t.edition_item_id) {
                    return Err(invalid(
                        "timeline must belong to a new imported edition item",
                    ));
                }
            }
            for version in &mut mutation.versions {
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
