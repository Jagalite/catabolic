//! Historical/direct-owner v1 InitializeAndPatch, not the current native39 NFO
//! HTTP admission path. The outer receipt proves the compound command; each
//! immutable source row stores its exact synthetic MetadataMutation instead.
use super::{
    NfoCreatedSourceAuthority, ProofBudget, VerifiedNfoBaseline, compare, digest, refused, row,
    schema, sql_budget_count, verify_committed_command_in,
};
use crate::{
    Result, Store,
    catalog_commands::TrustedCommandAuthority,
    catalog_metadata_store::{
        LEGACY_NAMESPACE, LEGACY_RECORD_ID, PolicyField, PolicyNormalization, PolicyValueType,
    },
    operation_budget::BudgetCharge,
};
use catabolic_core::{
    catalog_command::{CommandEnvelope, MAX_COMMAND_JSON_DEPTH},
    catalog_metadata::{
        ContributionDocument, ContributionKey, ContributionPatch, FieldEdit, FieldPolicy,
        FieldSchema, MAX_METADATA_DOCUMENT_BYTES, MAX_METADATA_EVIDENCE_BYTES, MAX_METADATA_FIELDS,
        MAX_METADATA_JSON_DEPTH, Normalization, SourceKind, ValueType, plan_update,
    },
};
use rusqlite::params;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    operation: String,
    subject: String,
    policies: Vec<PolicyField>,
    namespace: String,
    record_id: String,
    edits_json: String,
    evidence_json: String,
}

// Borrowed views match MetadataMutation's exact declared field order. They
// permit preflight admission before either synthetic raw string is allocated.
#[derive(Serialize)]
struct RawMutation<'a> {
    contract_version: u32,
    change: RawChange<'a>,
}
#[derive(Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum RawChange<'a> {
    Initialize {
        subject: &'a str,
        policies: &'a [PolicyField],
    },
    Patch {
        subject: &'a str,
        namespace: &'a str,
        record_id: &'a str,
        schema_fingerprint: &'a str,
        expected_revision: i64,
        edits_json: &'a str,
        evidence_json: &'a str,
    },
}
fn encoded(raw: &RawMutation<'_>, budget: &mut ProofBudget) -> Result<String> {
    budget.serialized(raw, MAX_COMMAND_JSON_DEPTH)?;
    Ok(serde_json::to_string(raw)?)
}

pub(super) fn verify_initialize_and_patch_in(
    store: &Store,
    first: &CommandEnvelope,
    grant: &NfoCreatedSourceAuthority,
    authority: TrustedCommandAuthority<'_>,
    expected_generation: i64,
) -> Result<VerifiedNfoBaseline> {
    verify_committed_command_in(store, first, authority, expected_generation)?;
    if grant.host_namespace != authority.host_namespace
        || !digest(&grant.request_digest)
        || !digest(&grant.complete_absence_audit_digest)
    {
        return Err(refused(
            "independent complete identity/origin absence audit required",
        ));
    }
    let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
    budget.serialized(first.payload(), MAX_COMMAND_JSON_DEPTH)?;
    // Deserialize the borrowed Value directly, without an uncharged Value clone.
    let request = Request::deserialize(first.payload())?;
    if request.operation != "metadata_initialize_and_patch"
        || request.subject != grant.subject
        || request.namespace != "motion.nfo"
        || request.record_id != "document"
        || request.policies.is_empty()
        || request.policies.len() > MAX_METADATA_FIELDS
        || request.edits_json.len() > MAX_METADATA_DOCUMENT_BYTES
        || request.evidence_json.len() > MAX_METADATA_EVIDENCE_BYTES
    {
        return Err(refused("not an exact bounded first compound NFO Patch"));
    }
    budget.raw_json(&request.evidence_json, MAX_METADATA_JSON_DEPTH)?;
    let evidence = catabolic_core::json::parse(&request.evidence_json, true).map_err(refused)?;
    if evidence.as_object().is_none_or(|fields| fields.len() != 3)
        || evidence["owner"] != "motion.metadata.replace"
        || evidence["request_digest"].as_str() != Some(grant.request_digest.as_str())
        || evidence["expected_aggregate_revision"].as_u64().is_none()
    {
        return Err(refused(
            "compound first Patch lacks independently audited Motion evidence",
        ));
    }

    // Reconstruct the exact explicit policies, rather than infer policies from
    // either today's projection or an assumed historical HTTP adapter version.
    budget.serialized(&request.policies, MAX_COMMAND_JSON_DEPTH)?;
    let supplied = FieldSchema::new(
        request
            .policies
            .iter()
            .map(|field| {
                (
                    field.field.clone(),
                    FieldPolicy {
                        value_type: match field.value_type {
                            PolicyValueType::String => ValueType::String,
                            PolicyValueType::Boolean => ValueType::Boolean,
                            PolicyValueType::Integer => ValueType::Integer,
                            PolicyValueType::Json => ValueType::Json,
                        },
                        nullable: field.nullable,
                        normalization: match field.normalization {
                            PolicyNormalization::Exact => Normalization::Exact,
                            PolicyNormalization::TrimString => Normalization::TrimString,
                            PolicyNormalization::TrimAsciiLowercase => {
                                Normalization::TrimAsciiLowercase
                            }
                        },
                    },
                )
            })
            .collect(),
    )
    .map_err(refused)?;
    let registered = schema(store, &request.subject, supplied.fingerprint(), &mut budget)?;
    if registered.value != supplied
        || registered.prior.is_some()
        || registered.command != *first.key()
    {
        return Err(refused(
            "compound command does not own exact v1 root schema",
        ));
    }
    budget.serialized(&request.policies, MAX_COMMAND_JSON_DEPTH)?;
    let policies_json = serde_json::to_string(&request.policies)?;
    // Check root ownership, policy order/encoding and the original subject row,
    // not only the equivalent fingerprint reconstructed by registry lookup.
    let matches_root: bool = store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM catalog_metadata_subjects s JOIN catalog_metadata_schemas r ON r.item_id=s.item_id AND r.schema_fingerprint=s.schema_fingerprint WHERE s.item_id=?1 AND s.contract_version=1 AND s.schema_fingerprint=?2 AND s.policies_json=?3 AND r.policies_json=?3 AND r.contract_version=1 AND r.system_fields_json='[]' AND r.prior_fingerprint IS NULL AND s.command_catalog_id=?4 AND s.command_host_namespace=?5 AND s.command_operation_id=?6)",
        params![request.subject,supplied.fingerprint(),policies_json,first.key().catalog_id(),first.key().host_namespace(),first.key().operation_id()],
        |r| r.get(0),
    )?;
    if !matches_root {
        return Err(refused("compound immutable subject/root policies differ"));
    }
    let legacy_bytes: i64 = store.db.query_row(
        "SELECT length(CAST(legacy_raw_json AS BLOB)) FROM catalog_metadata_subjects WHERE item_id=?",
        [&request.subject],
        |r| r.get(0),
    )?;
    let legacy_bytes = sql_budget_count(legacy_bytes, MAX_METADATA_DOCUMENT_BYTES)?;
    budget.charge(BudgetCharge {
        bytes: legacy_bytes,
        values: 1,
        work: legacy_bytes / 64 + 1,
        ..Default::default()
    })?;
    let legacy: String = store.db.query_row(
        "SELECT legacy_raw_json FROM catalog_metadata_subjects WHERE item_id=?",
        [&request.subject],
        |r| r.get(0),
    )?;
    budget.raw_json(&legacy, MAX_METADATA_JSON_DEPTH)?;
    const LEGACY_EVIDENCE: &str = r#"{"origin":"unknown","import":"items.metadata"}"#;
    budget.raw_json(LEGACY_EVIDENCE, MAX_METADATA_JSON_DEPTH)?;
    let expected_legacy = ContributionDocument::from_json(
        &supplied,
        ContributionKey::new(&request.subject, LEGACY_NAMESPACE, LEGACY_RECORD_ID)
            .map_err(refused)?,
        SourceKind::LegacyImport,
        1,
        &legacy,
        LEGACY_EVIDENCE,
    )
    .map_err(refused)?;
    let initialize_raw = encoded(
        &RawMutation {
            contract_version: 1,
            change: RawChange::Initialize {
                subject: &request.subject,
                policies: &request.policies,
            },
        },
        &mut budget,
    )?;
    let actual_legacy = row(
        store,
        &request.subject,
        LEGACY_NAMESPACE,
        LEGACY_RECORD_ID,
        1,
        &mut budget,
    )?;
    compare(
        &actual_legacy,
        &expected_legacy,
        &initialize_raw,
        first,
        &mut budget,
    )?;

    budget.raw_json(&request.edits_json, MAX_METADATA_JSON_DEPTH)?;
    budget.raw_json(&request.evidence_json, MAX_METADATA_JSON_DEPTH)?;
    let patch = ContributionPatch::from_json(
        &supplied,
        ContributionKey::new(&request.subject, &request.namespace, &request.record_id)
            .map_err(refused)?,
        SourceKind::Provider,
        0,
        &request.edits_json,
        &request.evidence_json,
    )
    .map_err(refused)?;
    if patch
        .edits()
        .iter()
        .any(|(field, edit)| field.starts_with("motion_") || !matches!(edit, FieldEdit::Set(_)))
    {
        return Err(refused("first compound NFO Patch is not public Set-only"));
    }
    let expected = plan_update(&supplied, None, &patch).map_err(refused)?;
    let raw = encoded(
        &RawMutation {
            contract_version: 1,
            change: RawChange::Patch {
                subject: &request.subject,
                namespace: &request.namespace,
                record_id: &request.record_id,
                schema_fingerprint: supplied.fingerprint(),
                expected_revision: 0,
                edits_json: &request.edits_json,
                evidence_json: &request.evidence_json,
            },
        },
        &mut budget,
    )?;
    let actual = row(
        store,
        &request.subject,
        &request.namespace,
        &request.record_id,
        1,
        &mut budget,
    )?;
    compare(&actual, &expected, &raw, first, &mut budget)?;
    budget.serialized(first.payload(), MAX_COMMAND_JSON_DEPTH)?;
    budget.raw_json(first.canonical_payload(), MAX_COMMAND_JSON_DEPTH)?;
    Ok(VerifiedNfoBaseline {
        subject: request.subject,
        revision: 1,
        fields_json: actual.fields,
        evidence_json: actual.evidence,
        raw_mutation_json: actual.raw,
        schema_fingerprint: actual.fingerprint,
        source_kind: SourceKind::Provider,
        command: first.clone(),
        automatic_identity: None,
        origin: None,
        attribution_audit_digest: grant.complete_absence_audit_digest.clone(),
        origin_proof_digest: None,
        origin_physical: None,
        imported_requested_names: None,
        imported_tag_mapping: None,
    })
}

#[cfg(test)]
mod tests;
