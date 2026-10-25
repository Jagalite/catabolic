//! Positive host-audited imported-root absence, independently tied to exact
//! committed parts and untouched typed metadata. No missing row, current JSON,
//! expired receipt body or requested marker can supply this capability.
use super::super::{
    proof_budget::ProofBudget, state::CurrentNfoState, verify_committed_command_in,
};
use crate::{
    Error, Result, Store,
    catalog_commands::TrustedCommandAuthority,
    catalog_import::{self, ImportBatch, ImportItem, ImportMutation, ImportStep, SnapshotManifest},
    catalog_import_typed::{ImportedContribution, ImportedMetadata, ImportedSourceKind},
    catalog_metadata_store::{
        LEGACY_NAMESPACE, LEGACY_RECORD_ID, MetadataChange, MetadataInitializeV2, MetadataMutation,
        PolicyField,
    },
    operation_budget::BudgetCharge,
};
use catabolic_core::{
    catalog_command::{CommandEnvelope, MAX_COMMAND_JSON_DEPTH},
    catalog_metadata::{
        ContributionDocument, ContributionKey, FieldResolution, FieldSchema,
        MAX_METADATA_DOCUMENTS, MAX_METADATA_JSON_DEPTH, SourceKind, resolve,
    },
};
use rusqlite::types::ValueRef;
use serde::{
    Deserialize, Serialize, Serializer,
    ser::{SerializeMap, SerializeSeq},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// Complete ordered root parts, not merely the part containing the subject.
#[derive(Debug)]
pub(crate) struct ImportedFirstPart<'a> {
    pub ordinal: u32,
    pub manifest: &'a SnapshotManifest,
    pub batches: &'a [ImportBatch],
    pub activation: &'a CommandEnvelope,
    pub reference_ack_id: &'a str,
    pub mapping_digest: &'a str,
}
#[derive(Debug, Serialize)]
pub(crate) struct ImportedFirstPartCommitment {
    pub ordinal: u32,
    pub import_id: String,
    pub input_digest: String,
    pub activation_digest: String,
    pub reference_ack_id: String,
    pub mapping_digest: String,
}
/// Nonserde grant prepared independently from the complete frozen root. The
/// subject map is an audited absence subset, not a claim about unrelated owners.
/// Values are logical ImportItem.source_id, never a physical source mapping.
#[derive(Debug, Serialize)]
pub(crate) struct ImportedFirstRootAudit {
    pub catalog_id: String,
    pub incarnation: String,
    pub generation: i64,
    pub host_namespace: String,
    pub outer_digest: String,
    pub root_import_id: String,
    pub root_manifest_digest: Option<String>,
    pub archived_audit_digest: String,
    pub ordered_parts: Vec<ImportedFirstPartCommitment>,
    pub subjects: BTreeMap<String, String>,
}
#[derive(Debug)]
pub(crate) struct VerifiedImportedRoot {
    catalog: String,
    incarnation: String,
    generation: i64,
    subject: String,
    outer_digest: String,
    closure_digest: String,
    audit_digest: String,
    proof_digest: String,
}
impl VerifiedImportedRoot {
    pub(super) fn audit_digest(&self) -> &str {
        &self.audit_digest
    }
    pub(super) fn proof_digest(&self) -> &str {
        &self.proof_digest
    }
    pub(super) fn validate_in(
        &self,
        store: &Store,
        outer: &CommandEnvelope,
        current: &CurrentNfoState,
    ) -> Result<()> {
        check_scope(store, outer, current)?;
        let fence = &current.presence().fence;
        if self.catalog != fence.catalog_id
            || self.incarnation != fence.incarnation
            || self.generation != fence.generation
            || self.subject != current.presence().subject
            || self.outer_digest != outer.digest().as_str()
        {
            return Err(refused(
                "imported capability fence or outer envelope changed",
            ));
        }
        let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
        if capture_closure(store, &self.subject, None, &mut budget)? != self.closure_digest {
            return Err(refused("untouched imported metadata closure changed"));
        }
        Ok(())
    }
}
fn refused(message: impl std::fmt::Display) -> Error {
    Error(format!("catalog_nfo_imported_first_refused: {message}"))
}
fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn check_scope(store: &Store, outer: &CommandEnvelope, current: &CurrentNfoState) -> Result<()> {
    let p = current.presence();
    if store.schema_version != 41
        || !store.writable
        || store.db.is_autocommit()
        || current.schema().is_none()
        || p.fence.catalog_id != store.database_id
        || Some(p.fence.incarnation.as_str()) != store.catalog_incarnation.as_deref()
        || outer.key().catalog_id() != p.fence.catalog_id
        || outer.admission_incarnation() != p.fence.incarnation
        || outer.expected_generation() != p.fence.generation
    {
        return Err(refused("managed same-incarnation retained writer required"));
    }
    store.transaction_available()?;
    let exact: bool = store.db.query_row(
        "SELECT catalog_id=?1 AND incarnation=?2 AND generation=?3 FROM catalog_state WHERE id=1",
        rusqlite::params![p.fence.catalog_id, p.fence.incarnation, p.fence.generation],
        |r| r.get(0),
    )?;
    if !exact {
        return Err(refused("current catalog fence changed"));
    }
    Ok(())
}
fn encoded(budget: &mut ProofBudget, value: &impl Serialize) -> Result<String> {
    budget.serialized(value, MAX_COMMAND_JSON_DEPTH)?;
    Ok(serde_json::to_string(value)?)
}
fn marker_absent(fields: &Value, required: bool, subject: &str) -> Result<()> {
    match fields.get("motion_catalog") {
        None if !required => Ok(()),
        Some(Value::Object(marker)) => {
            if marker.get("contract_version").and_then(Value::as_u64) != Some(1)
                || marker.get("item_id").and_then(Value::as_str) != Some(subject)
            {
                return Err(refused(
                    "original structural marker contract or subject differs",
                ));
            }
            let ids = marker
                .get("external_ids")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    refused("original structural external identity inventory not explicit")
                })?;
            for id in ids {
                let provider = id
                    .get("provider")
                    .and_then(Value::as_str)
                    .ok_or_else(|| refused("malformed structural identity entry"))?;
                if provider == "nfo" {
                    return Err(refused("original structural NFO identity obligation"));
                }
            }
            Ok(())
        }
        _ => Err(refused("original structural marker absent or malformed")),
    }
}

pub(super) fn verify_in(
    store: &Store,
    outer: &CommandEnvelope,
    current: &CurrentNfoState,
    parts: &[ImportedFirstPart<'_>],
    audit: &ImportedFirstRootAudit,
    authority: TrustedCommandAuthority<'_>,
) -> Result<VerifiedImportedRoot> {
    check_scope(store, outer, current)?;
    let p = current.presence();
    if audit.catalog_id != p.fence.catalog_id
        || audit.incarnation != p.fence.incarnation
        || audit.generation != p.fence.generation
        || audit.host_namespace != outer.key().host_namespace()
        || audit.host_namespace != authority.host_namespace
        || audit.outer_digest != outer.digest().as_str()
        || audit.root_import_id.is_empty()
        || audit.root_import_id.len() > 256
        || !digest(&audit.archived_audit_digest)
        || audit
            .root_manifest_digest
            .as_deref()
            .is_some_and(|v| !digest(v))
        || parts.is_empty()
        || (audit.root_manifest_digest.is_none()
            && (parts.len() != 1 || audit.root_import_id != parts[0].manifest.import_id))
        || parts.len() != audit.ordered_parts.len()
        || !audit.subjects.contains_key(&p.subject)
    {
        return Err(refused("complete typed root audit or exact scope absent"));
    }
    let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
    // Explicit claims are part of the capability commitment. A caller-supplied
    // SHA alone is not the promise that the whole root was audited.
    let audit_digest = budget.serialized(
        &(
            "catabolic.nfo.imported-first-audit.v1",
            audit,
            [
                "original_nfo_document_absent",
                "automatic_nfo_identity_absent",
                "nfo_tags_and_exclusions_absent",
                "sidecar_origins_absent",
            ],
        ),
        MAX_COMMAND_JSON_DEPTH,
    )?;
    let mut import_ids = BTreeSet::new();
    let mut source_ids = BTreeSet::new();
    let mut item_ids = BTreeSet::new();
    let mut found_subjects = BTreeSet::new();
    let mut metadata_seen = BTreeSet::new();
    let mut target: Option<(&ImportItem, &ImportedMetadata, &CommandEnvelope)> = None;
    for (index, (part, commitment)) in parts.iter().zip(&audit.ordered_parts).enumerate() {
        if usize::try_from(part.ordinal).ok() != Some(index)
            || commitment.ordinal != part.ordinal
            || commitment.import_id != part.manifest.import_id
            || !import_ids.insert(part.manifest.import_id.as_str())
            || commitment.activation_digest != part.activation.digest().as_str()
            || commitment.reference_ack_id != part.reference_ack_id
            || commitment.mapping_digest != part.mapping_digest
            || !digest(part.mapping_digest)
            || part.activation.key().host_namespace() != audit.host_namespace
        {
            return Err(refused(
                "duplicate, omitted, reordered or mismatched root part",
            ));
        }
        budget.canonical_serialized(part.manifest, MAX_COMMAND_JSON_DEPTH)?;
        let input = catalog_import::manifest_digest(part.manifest).map_err(refused)?;
        if input != commitment.input_digest
            || part.batches.len() != part.manifest.batches.len()
            || !matches!(
                part.manifest.source_namespace.as_str(),
                "motion.schema68.export.v1"
                    | "motion.schema68.export.v2"
                    | "motion.schema68.export.v3"
                    | "motion.schema68.export.v4"
            )
        {
            return Err(refused("exact Motion manifest commitment differs"));
        }
        verify_committed_command_in(store, part.activation, authority, p.fence.generation)?;
        budget.serialized(
            &(
                part.reference_ack_id,
                part.mapping_digest,
                &input,
                &part.manifest.import_id,
            ),
            MAX_COMMAND_JSON_DEPTH,
        )?;
        let expected = json!({"operation":"import","mutation":ImportMutation::Activate {
            import_id:part.manifest.import_id.clone(),input_digest:input,
            reference_ack_id:part.reference_ack_id.into(),mapping_digest:part.mapping_digest.into(),
        }});
        if part.activation.payload() != &expected
            || part.activation.key().operation_id()
                != catalog_import::operation_id(&part.manifest.import_id, ImportStep::Activate)
                    .map_err(refused)?
        {
            return Err(refused("exact permanent Activate commitment differs"));
        }
        for (batch, descriptor) in part.batches.iter().zip(&part.manifest.batches) {
            budget.canonical_serialized(batch, MAX_COMMAND_JSON_DEPTH)?;
            if catalog_import::describe_batch(batch).map_err(refused)? != *descriptor
                || !batch.unresolved_evidence.is_empty()
            {
                return Err(refused("complete activated batch descriptor differs"));
            }
            for item in &batch.items {
                if !source_ids.insert(item.source_id.as_str())
                    || !item_ids.insert(item.item_id.as_str())
                {
                    return Err(refused("root logical source/item mapping is not unique"));
                }
                if let Some(source) = audit.subjects.get(&item.item_id) {
                    if source != &item.source_id || !found_subjects.insert(item.item_id.as_str()) {
                        return Err(refused("audited logical source/item mapping differs"));
                    }
                    if item.legacy_metadata.get("motion_catalog").is_some() {
                        return Err(refused(
                            "legacy marker is not the named structural contributor",
                        ));
                    }
                }
            }
            if let Some(typed) = &batch.typed {
                for tags in &typed.tags {
                    if audit.subjects.contains_key(&tags.item_id) && tags.namespace == "motion.nfo"
                    {
                        return Err(refused("original NFO tag/exclusion obligation"));
                    }
                }
                for metadata in &typed.metadata {
                    if !audit.subjects.contains_key(&metadata.item_id) {
                        continue;
                    }
                    if !metadata_seen.insert(metadata.item_id.as_str()) {
                        return Err(refused("duplicate audited metadata across root"));
                    }
                    let mut structural = 0;
                    for source in &metadata.contributions {
                        if source.namespace == "motion.nfo" {
                            return Err(refused("original NFO document obligation in root part"));
                        }
                        let is_structural = source.namespace == "motion.structure"
                            && source.record_id == "source_projection";
                        if is_structural {
                            structural += 1;
                            if source.kind != ImportedSourceKind::Scan {
                                return Err(refused("structural source owner differs"));
                            }
                        }
                        if !is_structural && source.fields.get("motion_catalog").is_some() {
                            return Err(refused("marker outside named structural contributor"));
                        }
                        marker_absent(&source.fields, is_structural, &metadata.item_id)?;
                    }
                    if structural != 1 {
                        return Err(refused("exact original structural source required"));
                    }
                    if metadata.item_id == p.subject {
                        // ImportedMetadata can live in another batch within the
                        // same activated part, but never a different part's item.
                        let mut items = part
                            .batches
                            .iter()
                            .flat_map(|b| &b.items)
                            .filter(|i| i.item_id == p.subject);
                        let item = items
                            .next()
                            .ok_or_else(|| refused("metadata has no same-part logical item"))?;
                        if items.next().is_some()
                            || target.replace((item, metadata, part.activation)).is_some()
                        {
                            return Err(refused("ambiguous original typed metadata"));
                        }
                    }
                }
            }
        }
    }
    if found_subjects.len() != audit.subjects.len() || metadata_seen.len() != audit.subjects.len() {
        return Err(refused("audited root logical subject omitted"));
    }
    let (item, metadata, activation) =
        target.ok_or_else(|| refused("managed original typed subject absent"))?;
    let expected = reconstruct(
        store,
        current.schema().unwrap(),
        item,
        metadata,
        activation,
        &mut budget,
    )?;
    let closure_digest = capture_closure(store, &p.subject, Some(&expected), &mut budget)?;
    let proof_digest = budget.serialized(
        &(
            "catabolic.nfo.imported-first-root.v1",
            &audit_digest,
            &closure_digest,
            &p.fence,
            &p.subject,
            outer.digest().as_str(),
        ),
        MAX_COMMAND_JSON_DEPTH,
    )?;
    budget.serialized(
        &(
            &p.fence.catalog_id,
            &p.fence.incarnation,
            &p.subject,
            outer.digest().as_str(),
            &closure_digest,
            &audit_digest,
            &proof_digest,
        ),
        MAX_COMMAND_JSON_DEPTH,
    )?;
    Ok(VerifiedImportedRoot {
        catalog: p.fence.catalog_id.clone(),
        incarnation: p.fence.incarnation.clone(),
        generation: p.fence.generation,
        subject: p.subject.clone(),
        outer_digest: outer.digest().as_str().into(),
        closure_digest,
        audit_digest,
        proof_digest,
    })
}

// Exact owner wire views. These do not implement policy normalization: the
// existing registered schema and ContributionDocument constructors do that.
#[derive(Serialize)]
struct Evidence<'a> {
    import_operation_id: &'a str,
    source_evidence: &'a Value,
    source_revision: i64,
}
#[derive(Serialize)]
struct RawV2<'a> {
    evidence: &'a Evidence<'a>,
    fields: &'a Value,
    namespace: &'a str,
    operation: &'a str,
    record_id: &'a str,
    subject: &'a str,
}
struct Edits<'a>(&'a serde_json::Map<String, Value>);
impl Serialize for Edits<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Set<'a> {
            op: &'a str,
            value: &'a Value,
        }
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in self.0 {
            m.serialize_entry(
                k,
                &Set {
                    op: "set",
                    value: v,
                },
            )?;
        }
        m.end()
    }
}
#[derive(Serialize)]
struct RawV1<'a> {
    contract_version: u32,
    change: Patch<'a>,
}
#[derive(Serialize)]
struct Patch<'a> {
    action: &'a str,
    subject: &'a str,
    namespace: &'a str,
    record_id: &'a str,
    schema_fingerprint: &'a str,
    expected_revision: i64,
    edits_json: &'a str,
    evidence_json: &'a str,
}
struct Document {
    document: ContributionDocument,
    fields: String,
    evidence: String,
    raw: String,
}
struct Documents<'a>(&'a [Document]);
impl Serialize for Documents<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for document in self.0 {
            seq.serialize_element(&document.document)?;
        }
        seq.end()
    }
}
fn document(
    schema: &FieldSchema,
    subject: &str,
    source: &ImportedContribution,
    command: &CommandEnvelope,
    budget: &mut ProofBudget,
) -> Result<Document> {
    let evidence = Evidence {
        import_operation_id: command.key().operation_id(),
        source_evidence: &source.evidence,
        source_revision: source.source_revision,
    };
    let fields = encoded(budget, &source.fields)?;
    let evidence_json = encoded(budget, &evidence)?;
    let raw = if schema.contract_version() == 2 {
        encoded(
            budget,
            &RawV2 {
                evidence: &evidence,
                fields: &source.fields,
                namespace: &source.namespace,
                operation: "metadata_import_source_v2",
                record_id: &source.record_id,
                subject,
            },
        )?
    } else {
        let edits = encoded(
            budget,
            &Edits(
                source
                    .fields
                    .as_object()
                    .ok_or_else(|| refused("source fields not object"))?,
            ),
        )?;
        encoded(
            budget,
            &RawV1 {
                contract_version: 1,
                change: Patch {
                    action: "patch",
                    subject,
                    namespace: &source.namespace,
                    record_id: &source.record_id,
                    schema_fingerprint: schema.fingerprint(),
                    expected_revision: 0,
                    edits_json: &edits,
                    evidence_json: &evidence_json,
                },
            },
        )?
    };
    budget.raw_json(&fields, MAX_METADATA_JSON_DEPTH)?;
    budget.raw_json(&evidence_json, MAX_METADATA_JSON_DEPTH)?;
    let kind = match source.kind {
        ImportedSourceKind::Local => SourceKind::Local,
        ImportedSourceKind::Provider => SourceKind::Provider,
        ImportedSourceKind::Scan => SourceKind::Scan,
    };
    let doc = ContributionDocument::from_json_with_system_fields(
        schema,
        ContributionKey::new(subject, &source.namespace, &source.record_id).map_err(refused)?,
        kind,
        1,
        &fields,
        &evidence_json,
    )
    .map_err(refused)?;
    retained(doc, raw, budget)
}
fn retained(
    document: ContributionDocument,
    raw: String,
    budget: &mut ProofBudget,
) -> Result<Document> {
    let fields = encoded(budget, document.fields())?;
    budget.canonical_json(document.evidence())?;
    Ok(Document {
        evidence: catabolic_core::encode(document.evidence()),
        document,
        fields,
        raw,
    })
}
fn row_hash(budget: &mut ProofBudget, row: &impl Serialize) -> Result<String> {
    // Retained comparison digest, in addition to the borrowed row traversal.
    budget.charge(BudgetCharge {
        bytes: 64,
        values: 1,
        work: 1,
        ..Default::default()
    })?;
    budget.serialized(row, MAX_COMMAND_JSON_DEPTH)
}
// The query ordering is part of this private digest, not a host wire contract.
const QUERIES: [&str; 8] = [
    "SELECT item_id,contract_version,schema_fingerprint,policies_json,legacy_raw_json,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us FROM catalog_metadata_subjects WHERE item_id=?",
    "SELECT item_id,schema_fingerprint,contract_version,policies_json,system_fields_json,prior_fingerprint,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us FROM catalog_metadata_schemas WHERE item_id=? ORDER BY schema_fingerprint",
    "SELECT item_id,schema_fingerprint FROM catalog_metadata_schema_heads WHERE item_id=?",
    "SELECT item_id,namespace,record_id,source_kind,revision,schema_fingerprint,fields_json,evidence_json FROM catalog_metadata_sources WHERE item_id=? ORDER BY namespace,record_id",
    "SELECT item_id,namespace,record_id,revision,source_kind,schema_fingerprint,fields_json,evidence_json,raw_mutation_json,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us FROM catalog_metadata_history WHERE item_id=? ORDER BY namespace,record_id,revision",
    "SELECT item_id,schema_fingerprint,resolution_json,effective_json FROM catalog_metadata_projection WHERE item_id=?",
    "SELECT id,kind,metadata FROM items WHERE id=?",
    "SELECT entity_kind,entity_key,revision,deleted FROM catalog_entity_revisions WHERE entity_kind IN ('items','catalog_metadata_subjects','catalog_metadata_schemas','catalog_metadata_schema_heads','catalog_metadata_sources','catalog_metadata_history','catalog_metadata_projection') AND json_extract(entity_key,'$[0]')=? ORDER BY entity_kind,entity_key",
];
struct SqlRow<'a, 's>(&'a rusqlite::Row<'s>, usize);
impl Serialize for SqlRow<'_, '_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.1))?;
        for i in 0..self.1 {
            match self.0.get_ref(i).map_err(serde::ser::Error::custom)? {
                ValueRef::Null => seq.serialize_element(&Option::<u8>::None)?,
                ValueRef::Integer(v) => seq.serialize_element(&v)?,
                ValueRef::Text(v) => seq.serialize_element(
                    std::str::from_utf8(v).map_err(serde::ser::Error::custom)?,
                )?,
                _ => {
                    return Err(serde::ser::Error::custom(
                        "unexpected imported closure SQL type",
                    ));
                }
            }
        }
        seq.end()
    }
}
fn capture_closure(
    store: &Store,
    subject: &str,
    expected: Option<&[Vec<String>; 8]>,
    budget: &mut ProofBudget,
) -> Result<String> {
    let mut hash = Sha256::new();
    for (q, sql) in QUERIES.iter().enumerate() {
        hash.update((q as u64).to_le_bytes());
        let mut query = store.db.prepare(sql)?;
        let cols = query.column_count();
        let mut rows = query.query([subject])?;
        let mut n = 0usize;
        while let Some(row) = rows.next()? {
            // Serialize directly from borrowed SQLite cells, before any owned
            // copy. Every row is charged cumulatively, including extra rows.
            let d = budget.serialized(&SqlRow(row, cols), MAX_COMMAND_JSON_DEPTH)?;
            if expected.is_some_and(|e| e[q].get(n) != Some(&d)) {
                return Err(refused(format!(
                    "original imported source/schema bytes or permanent heads evolved (table {q}, row {n})"
                )));
            }
            hash.update(d.as_bytes());
            n = n
                .checked_add(1)
                .ok_or_else(|| refused("row count overflow"))?;
        }
        if expected.is_some_and(|e| e[q].len() != n) {
            return Err(refused("original imported closure row omitted"));
        }
        hash.update((n as u64).to_le_bytes());
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn reconstruct(
    store: &Store,
    schema: &FieldSchema,
    item: &ImportItem,
    metadata: &ImportedMetadata,
    command: &CommandEnvelope,
    budget: &mut ProofBudget,
) -> Result<[Vec<String>; 8]> {
    if schema.contract_version() != metadata.contract_version
        || !matches!(metadata.contract_version, 1 | 2)
        || metadata
            .contributions
            .len()
            .checked_add(1)
            .is_none_or(|n| n > MAX_METADATA_DOCUMENTS)
    {
        return Err(refused("original schema contract or source bound differs"));
    }
    budget.serialized(&metadata.policies, MAX_COMMAND_JSON_DEPTH)?;
    let policies = Vec::<PolicyField>::deserialize(&metadata.policies)?;
    let policy_json = encoded(budget, &policies)?;
    budget.serialized(&metadata.system_fields, MAX_COMMAND_JSON_DEPTH)?;
    let system: BTreeSet<String> = metadata.system_fields.iter().cloned().collect();
    if system.len() != metadata.system_fields.len() || &system != schema.system_fields() {
        return Err(refused("original system classification differs"));
    }
    let system_json = encoded(budget, &system)?;
    budget.canonical_json(&item.legacy_metadata)?;
    let legacy_raw = catabolic_core::encode(&item.legacy_metadata);
    // Exact initializer forms differ; source-history raw bytes remain original.
    let init = if metadata.contract_version == 2 {
        encoded(
            budget,
            &MetadataInitializeV2 {
                contract_version: 2,
                subject: item.item_id.clone(),
                policies,
                system_fields: system,
            },
        )?
    } else {
        encoded(
            budget,
            &MetadataMutation {
                contract_version: 1,
                change: MetadataChange::Initialize {
                    subject: item.item_id.clone(),
                    policies,
                },
            },
        )?
    };
    budget.raw_json(&legacy_raw, MAX_METADATA_JSON_DEPTH)?;
    let legacy = ContributionDocument::from_json_with_system_fields(
        schema,
        ContributionKey::new(&item.item_id, LEGACY_NAMESPACE, LEGACY_RECORD_ID).map_err(refused)?,
        SourceKind::LegacyImport,
        1,
        &legacy_raw,
        r#"{"origin":"unknown","import":"items.metadata"}"#,
    )
    .map_err(refused)?;
    let mut docs = vec![retained(legacy, init, budget)?];
    for source in &metadata.contributions {
        docs.push(document(schema, &item.item_id, source, command, budget)?);
    }
    let mut item_revision = 1i64;
    budget.serialized(&item.legacy_metadata, MAX_COMMAND_JSON_DEPTH)?;
    budget.serialized(&legacy_raw, MAX_COMMAND_JSON_DEPTH)?;
    let mut effective = item.legacy_metadata.clone();
    let mut item_raw = legacy_raw.clone();
    let mut final_resolution = None;
    for end in 1..=docs.len() {
        // This is the original owner's ordered projection sequence. It derives
        // the item counter rather than using a possibly compacted result body.
        budget.serialized(&Documents(&docs[..end]), MAX_COMMAND_JSON_DEPTH)?;
        let resolution = resolve(
            schema,
            &item.item_id,
            &docs[..end]
                .iter()
                .map(|d| d.document.clone())
                .collect::<Vec<_>>(),
        )
        .map_err(refused)?;
        budget.serialized(&resolution, MAX_COMMAND_JSON_DEPTH)?;
        let next = Value::Object(
            resolution
                .fields
                .iter()
                .filter_map(|(k, v)| match v {
                    FieldResolution::Resolved { value, .. } => Some((k.clone(), value.clone())),
                    _ => None,
                })
                .collect(),
        );
        if effective != next {
            item_revision = item_revision
                .checked_add(1)
                .ok_or_else(|| refused("item revision overflow"))?;
            budget.canonical_json(&next)?;
            item_raw = catabolic_core::encode(&next);
            effective = next;
        }
        final_resolution = Some(resolution);
    }
    let resolution = final_resolution.unwrap();
    let projection = encoded(budget, &resolution)?;
    budget.canonical_json(&effective)?;
    let effective_json = catabolic_core::encode(&effective);
    // The schema subject's admission time is immutable. All imported source
    // effects use this same actual typed-owner input, not receipt commit time.
    budget.charge(BudgetCharge {
        values: 1,
        work: 1,
        ..Default::default()
    })?;
    let at: i64 = store.db.query_row(
        "SELECT admitted_at_us FROM catalog_metadata_subjects WHERE item_id=?",
        [&item.item_id],
        |r| r.get(0),
    )?;
    if at < 0 {
        return Err(refused("invalid imported owner time"));
    }
    let id = item.item_id.as_str();
    let fp = schema.fingerprint();
    let c = command.key();
    let mut expected: [Vec<String>; 8] = Default::default();
    expected[0].push(row_hash(
        budget,
        &(
            id,
            metadata.contract_version,
            fp,
            &policy_json,
            &legacy_raw,
            c.catalog_id(),
            c.host_namespace(),
            c.operation_id(),
            at,
        ),
    )?);
    expected[1].push(row_hash(
        budget,
        &(
            id,
            fp,
            metadata.contract_version,
            &policy_json,
            &system_json,
            Option::<String>::None,
            c.catalog_id(),
            c.host_namespace(),
            c.operation_id(),
            at,
        ),
    )?);
    expected[2].push(row_hash(budget, &(id, fp))?);
    let mut heads = BTreeMap::new();
    for (kind, key, revision) in [
        ("items", encoded(budget, &[id])?, item_revision),
        ("catalog_metadata_subjects", encoded(budget, &[id])?, 1),
        ("catalog_metadata_schemas", encoded(budget, &[id, fp])?, 1),
        ("catalog_metadata_schema_heads", encoded(budget, &[id])?, 1),
        (
            "catalog_metadata_projection",
            encoded(budget, &[id])?,
            i64::try_from(docs.len()).map_err(refused)?,
        ),
    ] {
        heads.insert((kind, key), revision);
    }
    docs.sort_by(|a, b| a.document.key().cmp(b.document.key()));
    for d in &docs {
        let key = d.document.key();
        let kind = match d.document.kind() {
            SourceKind::LegacyImport => "legacy_import",
            SourceKind::Local => "local",
            SourceKind::Provider => "provider",
            SourceKind::Scan => "scan",
        };
        expected[3].push(row_hash(
            budget,
            &(
                id,
                key.namespace(),
                key.record_id(),
                kind,
                1i64,
                fp,
                &d.fields,
                &d.evidence,
            ),
        )?);
        expected[4].push(row_hash(
            budget,
            &(
                id,
                key.namespace(),
                key.record_id(),
                1i64,
                kind,
                fp,
                &d.fields,
                &d.evidence,
                &d.raw,
                c.catalog_id(),
                c.host_namespace(),
                c.operation_id(),
                at,
            ),
        )?);
        heads.insert(
            (
                "catalog_metadata_sources",
                encoded(budget, &(id, key.namespace(), key.record_id()))?,
            ),
            1,
        );
        heads.insert(
            (
                "catalog_metadata_history",
                encoded(budget, &(id, key.namespace(), key.record_id(), 1i64))?,
            ),
            1,
        );
    }
    expected[5].push(row_hash(budget, &(id, fp, &projection, &effective_json))?);
    expected[6].push(row_hash(budget, &(id, &item.kind, &item_raw))?);
    for ((kind, key), revision) in heads {
        expected[7].push(row_hash(budget, &(kind, key, revision, 0i64))?);
    }
    Ok(expected)
}

#[cfg(test)]
pub(in crate::catalog_nfo_attribution) mod tests;
