//! Reconstruct imported revision-one identity from an exact activated manifest.
//! Archived origin absence is a separate trusted host audit, never inferred from
//! a missing JSON field. Origin-bearing subjects additionally require the
//! independently verified exact activated physical-part proof.
mod imported_tag_mapping;
mod imported_tags;
pub(super) use imported_tag_mapping::{
    ImportedTagAdmission, ImportedTagMapping, accounted_in as verify_recorded_tags_in,
};
pub(super) use imported_tags::{ImportedRequestedNames, reserve_normalization};

use super::{VerifiedNfoBaseline, proof_budget::ProofBudget, verify_committed_command_in};
use crate::{
    Error, Result, Store,
    catalog_commands::TrustedCommandAuthority,
    catalog_import::{self, ImportBatch, ImportMutation, ImportStep, SnapshotManifest},
    catalog_import_typed::{ImportedMetadata, ImportedSourceKind},
    catalog_metadata_store,
};
use catabolic_core::{
    catalog_command::{CommandEnvelope, MAX_COMMAND_JSON_DEPTH},
    catalog_metadata::{
        ContributionDocument, ContributionKey, MAX_METADATA_JSON_DEPTH, SourceKind,
    },
};
use serde::{Serialize, Serializer, ser::SerializeMap};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Independently prepared by the embedding host from its immutable frozen
/// export, complete cutover set, exact part and complete archived-origin audit.
/// Never deserialize this grant or derive it from request namespace/markers.
#[derive(Debug)]
pub(crate) struct NfoImportAuthority {
    pub host_namespace: String,
    pub root_manifest_digest: Option<String>,
    pub import_id: String,
    pub input_digest: String,
    pub source_namespace: String,
    pub reference_ack_id: String,
    pub mapping_digest: String,
    pub origin_audit_digest: String,
    pub no_origin_subjects: BTreeSet<String>,
}
fn refused(message: &str) -> Error {
    Error(format!("catalog_nfo_import_provenance_refused: {message}"))
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

// Borrowed serialization views preserve the existing exact raw wire bytes while
// permitting admission before allocating strings or cloning JSON trees.
#[derive(Serialize)]
struct ImportEvidence<'a> {
    import_operation_id: &'a str,
    source_evidence: &'a Value,
    source_revision: i64,
}
#[derive(Serialize)]
struct ImportRawV2<'a> {
    evidence: &'a ImportEvidence<'a>,
    fields: &'a Value,
    namespace: &'a str,
    operation: &'a str,
    record_id: &'a str,
    subject: &'a str,
}
struct ImportEdits<'a>(&'a serde_json::Map<String, Value>);
impl Serialize for ImportEdits<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Set<'a> {
            op: &'a str,
            value: &'a Value,
        }
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in self.0 {
            map.serialize_entry(key, &Set { op: "set", value })?;
        }
        map.end()
    }
}
#[derive(Serialize)]
struct ImportRawV1<'a> {
    contract_version: u32,
    change: ImportPatch<'a>,
}
#[derive(Serialize)]
struct ImportPatch<'a> {
    action: &'a str,
    subject: &'a str,
    namespace: &'a str,
    record_id: &'a str,
    schema_fingerprint: &'a str,
    expected_revision: i64,
    edits_json: &'a str,
    evidence_json: &'a str,
}
fn admitted_json(budget: &mut ProofBudget, value: &impl Serialize) -> Result<String> {
    budget.serialized(value, MAX_COMMAND_JSON_DEPTH)?;
    Ok(serde_json::to_string(value)?)
}

pub(crate) fn verify_imported_no_origin_baseline_in(
    store: &Store,
    subject: &str,
    manifest: &SnapshotManifest,
    batches: &[ImportBatch],
    activation: &CommandEnvelope,
    grant: &NfoImportAuthority,
    authority: TrustedCommandAuthority<'_>,
    expected_generation: i64,
) -> Result<VerifiedNfoBaseline> {
    if !grant.no_origin_subjects.contains(subject) {
        return Err(refused(
            "complete archived origin absence must be independently proved",
        ));
    }
    verify_imported_baseline_in(
        store,
        subject,
        manifest,
        batches,
        activation,
        grant,
        authority,
        expected_generation,
        None,
        None,
        None,
    )
}

pub(crate) fn verify_imported_origin_baseline_in(
    store: &Store,
    subject: &str,
    manifest: &SnapshotManifest,
    batches: &[ImportBatch],
    activation: &CommandEnvelope,
    grant: &NfoImportAuthority,
    origin: &super::VerifiedImportedOrigin,
    authority: TrustedCommandAuthority<'_>,
    expected_generation: i64,
) -> Result<VerifiedNfoBaseline> {
    if grant.no_origin_subjects.contains(subject)
        || origin.subject() != subject
        || origin.metadata_command().canonical_payload() != activation.canonical_payload()
        || origin.root_manifest_digest() != grant.root_manifest_digest.as_deref()
        || origin.origin_audit_digest() != grant.origin_audit_digest
        || origin.observed_generation() != expected_generation
    {
        return Err(refused(
            "historical physical origin differs from exact metadata part/audit",
        ));
    }
    // The proof remains descriptive and must still belong to this admitted
    // native owner; it is not a transferable receipt or current file grant.
    verify_committed_command_in(
        store,
        origin.physical_command(),
        authority,
        expected_generation,
    )?;
    verify_imported_baseline_in(
        store,
        subject,
        manifest,
        batches,
        activation,
        grant,
        authority,
        expected_generation,
        Some(origin.origin().clone()),
        Some(origin.proof_digest().to_owned()),
        Some(origin.physical().clone()),
    )
}

fn verify_imported_baseline_in(
    store: &Store,
    subject: &str,
    manifest: &SnapshotManifest,
    batches: &[ImportBatch],
    activation: &CommandEnvelope,
    grant: &NfoImportAuthority,
    authority: TrustedCommandAuthority<'_>,
    expected_generation: i64,
    origin: Option<catabolic_core::catalog_metadata::nfo_refresh::FileOrigin>,
    origin_proof_digest: Option<String>,
    origin_physical: Option<crate::catalog_import_typed::PhysicalMapping>,
) -> Result<VerifiedNfoBaseline> {
    // This native commit proof is descriptive, not a replayed Activated result.
    verify_committed_command_in(store, activation, authority, expected_generation)?;
    if grant.host_namespace != authority.host_namespace
        || grant.import_id != manifest.import_id
        || grant.source_namespace != manifest.source_namespace
        || !matches!(
            grant.source_namespace.as_str(),
            "motion.schema68.export.v1"
                | "motion.schema68.export.v2"
                | "motion.schema68.export.v3"
                | "motion.schema68.export.v4"
        )
        || !digest(&grant.input_digest)
        || !digest(&grant.mapping_digest)
        || !digest(&grant.origin_audit_digest)
        || grant
            .root_manifest_digest
            .as_ref()
            .is_some_and(|value| !digest(value))
    {
        return Err(refused(
            "independent exact import/complete origin-audit grant required",
        ));
    }
    let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
    budget.serialized(&(manifest, batches), MAX_COMMAND_JSON_DEPTH)?;
    // Compare this independently supplied value by reference before constructing
    // the expected envelope; a grant is not an unbounded allocation allowance.
    if grant.reference_ack_id.is_empty()
        || grant.reference_ack_id.len() > 256
        || grant.reference_ack_id.chars().any(char::is_control)
    {
        return Err(refused("invalid reference acknowledgment identity"));
    }
    let input = catalog_import::manifest_digest(manifest).map_err(|e| refused(&e.to_string()))?;
    if input != grant.input_digest || batches.len() != manifest.batches.len() {
        return Err(refused("frozen manifest identity or batch count changed"));
    }
    let expected = json!({"operation":"import","mutation":ImportMutation::Activate {
        import_id:grant.import_id.clone(),input_digest:grant.input_digest.clone(),
        reference_ack_id:grant.reference_ack_id.clone(),mapping_digest:grant.mapping_digest.clone(),
    }});
    if activation.payload() != &expected
        || activation.key().operation_id()
            != catalog_import::operation_id(&grant.import_id, ImportStep::Activate)
                .map_err(|e| refused(&e.to_string()))?
    {
        return Err(refused("exact part activation envelope changed"));
    }
    let mut found: Option<&ImportedMetadata> = None;
    for (batch, descriptor) in batches.iter().zip(&manifest.batches) {
        if &catalog_import::describe_batch(batch).map_err(|e| refused(&e.to_string()))?
            != descriptor
        {
            return Err(refused("frozen batch descriptor changed"));
        }
        if let Some(typed) = &batch.typed {
            for metadata in &typed.metadata {
                if metadata.item_id == subject {
                    if found.replace(metadata).is_some() {
                        return Err(refused("duplicate imported subject"));
                    }
                }
            }
        }
    }
    let metadata = found.ok_or_else(|| refused("subject absent from activated part"))?;
    let mut sources = metadata
        .contributions
        .iter()
        .filter(|source| source.namespace == "motion.nfo" && source.record_id == "document");
    let source = sources
        .next()
        .ok_or_else(|| refused("NFO source absent from activated part"))?;
    if sources.next().is_some()
        || source.kind != ImportedSourceKind::Provider
        || source.source_revision < 1
        || source.evidence["source_table"] != "metadata_documents"
        || source.evidence["source"] != "nfo"
    {
        return Err(refused("ambiguous or unrecognized original NFO source"));
    }
    let automatic_identity = match source.evidence.get("external_id") {
        Some(Value::Null) => None,
        // Historical PUT identity bytes are opaque (up to 256 bytes); refresh
        // has a separate 1024-byte bound. Do not trim or normalize retained IDs.
        Some(Value::String(value)) if !value.is_empty() && value.len() <= 1024 => {
            Some(value.clone())
        }
        _ => return Err(refused("original automatic identity is not explicit")),
    };
    let original_document = source.evidence["document_json"]
        .as_str()
        .ok_or_else(|| refused("original source document bytes absent"))?;
    budget.raw_json(original_document, MAX_METADATA_JSON_DEPTH)?;
    let original: Value = catabolic_core::json::parse(original_document, true)
        .map_err(|e| refused(&e.to_string()))?;
    if original != source.evidence["document"] || original["values"] != source.fields {
        return Err(refused(
            "original source fields disagree with frozen document",
        ));
    }
    // Every descriptor above was checked before this complete-activation scan.
    // This establishes archived requested names only, not current assertions or
    // the IDs chosen by activation's vocabulary owner.
    let imported_requested_names = imported_tags::verify_requested_names(
        subject,
        source.source_revision,
        &original,
        batches,
        &mut budget,
    )?;
    let imported_tag_mapping = imported_tag_mapping::read_mapping_in(
        store,
        &imported_requested_names,
        manifest,
        activation,
        grant,
        authority,
        expected_generation,
        &mut budget,
    )?;
    // Subject initialization is immutable. Later compatible schema heads never
    // replace this original fingerprint or its exact typed import provenance.
    let (schema_bytes, schema_values): (i64, i64) = store.db.query_row(
        "SELECT length(CAST(s.policies_json AS BLOB))+length(CAST(r.policies_json AS BLOB))+length(CAST(r.system_fields_json AS BLOB))+1024,5*json_array_length(s.policies_json)+5*json_array_length(r.policies_json)+json_array_length(r.system_fields_json)+3 FROM catalog_metadata_subjects s JOIN catalog_metadata_schemas r ON r.item_id=s.item_id AND r.schema_fingerprint=s.schema_fingerprint WHERE s.item_id=? AND length(CAST(s.policies_json AS BLOB))<=2097152 AND length(CAST(r.policies_json AS BLOB))<=2097152 AND length(CAST(r.system_fields_json AS BLOB))<=32768",
        [subject], |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let schema_bytes =
        usize::try_from(schema_bytes).map_err(|_| refused("invalid schema proof size"))?;
    let schema_values =
        usize::try_from(schema_values).map_err(|_| refused("invalid schema proof count"))?;
    budget
        .charge(crate::operation_budget::BudgetCharge {
            bytes: schema_bytes,
            values: schema_values,
            work: schema_bytes / 64 + schema_values + 1,
            ..Default::default()
        })
        .map_err(|_| refused("schema proof budget exceeded before allocation"))?;
    type Row = (u32, String, String, String, String, String);
    let (version,fingerprint,policies,catalog,host,operation):Row=store.db.query_row(
        "SELECT contract_version,schema_fingerprint,policies_json,command_catalog_id,command_host_namespace,command_operation_id FROM catalog_metadata_subjects WHERE item_id=? AND length(CAST(policies_json AS BLOB))<=2097152",[subject],
        |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
    if version != metadata.contract_version
        || serde_json::from_str::<Value>(&policies)? != metadata.policies
        || catalog != activation.key().catalog_id()
        || host != activation.key().host_namespace()
        || operation != activation.key().operation_id()
    {
        return Err(refused(
            "original schema does not belong to exact activated part",
        ));
    }
    let schema = catalog_metadata_store::read_registered_schema(&store.db, subject, &fingerprint)
        .map_err(|e| refused(&e.to_string()))?;
    if schema.contract_version() != metadata.contract_version
        || schema.system_fields()
            != &metadata
                .system_fields
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
    {
        return Err(refused("original schema system-field policy differs"));
    }
    let evidence = ImportEvidence {
        import_operation_id: activation.key().operation_id(),
        source_evidence: &source.evidence,
        source_revision: source.source_revision,
    };
    let fields_json = admitted_json(&mut budget, &source.fields)?;
    let evidence_json = admitted_json(&mut budget, &evidence)?;
    let raw = match metadata.contract_version {
        2 => admitted_json(
            &mut budget,
            &ImportRawV2 {
                evidence: &evidence,
                fields: &source.fields,
                namespace: "motion.nfo",
                operation: "metadata_import_source_v2",
                record_id: "document",
                subject,
            },
        )?,
        1 => {
            let fields = source
                .fields
                .as_object()
                .ok_or_else(|| refused("import fields are not an object"))?;
            let edits_json = admitted_json(&mut budget, &ImportEdits(fields))?;
            admitted_json(
                &mut budget,
                &ImportRawV1 {
                    contract_version: 1,
                    change: ImportPatch {
                        action: "patch",
                        subject,
                        namespace: "motion.nfo",
                        record_id: "document",
                        schema_fingerprint: &fingerprint,
                        expected_revision: 0,
                        edits_json: &edits_json,
                        evidence_json: &evidence_json,
                    },
                },
            )?
        }
        _ => return Err(refused("unsupported imported metadata contract")),
    };
    // Parsing creates retained value trees in addition to the already admitted
    // encoded strings. Charge that representation before decoding it.
    budget.raw_json(&fields_json, MAX_METADATA_JSON_DEPTH)?;
    budget.raw_json(&evidence_json, MAX_METADATA_JSON_DEPTH)?;
    let document = ContributionDocument::from_json(
        &schema,
        ContributionKey::new(subject, "motion.nfo", "document")
            .map_err(|e| refused(&e.to_string()))?,
        SourceKind::Provider,
        1,
        &fields_json,
        &evidence_json,
    )
    .map_err(|e| refused(&e.to_string()))?;
    budget.canonical_json(document.evidence())?;
    let retained_fields = admitted_json(&mut budget, document.fields())?;
    budget.serialized(activation.payload(), MAX_COMMAND_JSON_DEPTH)?;
    Ok(VerifiedNfoBaseline {
        subject: subject.into(),
        revision: 1,
        fields_json: retained_fields,
        evidence_json: catabolic_core::encode(document.evidence()),
        raw_mutation_json: raw,
        schema_fingerprint: fingerprint,
        source_kind: SourceKind::Provider,
        command: activation.clone(),
        automatic_identity,
        attribution_audit_digest: grant.origin_audit_digest.clone(),
        origin,
        origin_proof_digest,
        origin_physical,
        imported_requested_names: Some(imported_requested_names),
        imported_tag_mapping,
    })
}

#[cfg(test)]
mod tests;
