//! The preinitialized, public-only first Patch used by Motion's native39 path.
//! Verification occurs after the official native40 upgrade, whose registry keeps
//! the original v1 schema and whose history/receipts retain their exact bytes.
//! This proves the operation contract, not an otherwise unrecorded engine version.
use super::{
    NfoCreatedSourceAuthority, ProofBudget, VerifiedNfoBaseline, compare, digest, refused, row,
    schema, verify_committed_command_in,
};
use crate::{
    Result, Store,
    catalog_commands::TrustedCommandAuthority,
    catalog_metadata_store::{MetadataChange, MetadataMutation},
};
use catabolic_core::{
    catalog_command::{CommandEnvelope, MAX_COMMAND_JSON_DEPTH},
    catalog_metadata::{
        ContributionKey, ContributionPatch, FieldEdit, MAX_METADATA_DOCUMENT_BYTES,
        MAX_METADATA_EVIDENCE_BYTES, MAX_METADATA_JSON_DEPTH, SourceKind, plan_update,
    },
};

pub(super) fn verify_first_patch_in(
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
    if first
        .payload()
        .as_object()
        .is_none_or(|fields| fields.len() != 2)
        || first.payload()["operation"] != "metadata"
    {
        return Err(refused("not an exact standalone first metadata Patch"));
    }
    let mutation: MetadataMutation = serde_json::from_value(first.payload()["mutation"].clone())?;
    let MetadataChange::Patch {
        subject,
        namespace,
        record_id,
        schema_fingerprint,
        expected_revision,
        edits_json,
        evidence_json,
    } = &mutation.change
    else {
        return Err(refused("first v1 source is not Patch"));
    };
    if mutation.contract_version != 1
        || subject != &grant.subject
        || namespace != "motion.nfo"
        || record_id != "document"
        || *expected_revision != 0
        || edits_json.len() > MAX_METADATA_DOCUMENT_BYTES
        || evidence_json.len() > MAX_METADATA_EVIDENCE_BYTES
    {
        return Err(refused("not an exact bounded first Motion NFO Patch"));
    }
    budget.raw_json(evidence_json, MAX_METADATA_JSON_DEPTH)?;
    let evidence = catabolic_core::json::parse(evidence_json, true).map_err(refused)?;
    if evidence.as_object().is_none_or(|fields| fields.len() != 3)
        || evidence["owner"] != "motion.metadata.replace"
        || evidence["request_digest"].as_str() != Some(grant.request_digest.as_str())
        || evidence["expected_aggregate_revision"].as_u64().is_none()
    {
        return Err(refused(
            "first Patch lacks exact independently audited Motion evidence",
        ));
    }
    let registered = schema(store, subject, schema_fingerprint, &mut budget)?;
    if registered.value.contract_version() != 1
        || !registered.value.system_fields().is_empty()
        || registered.prior.is_some()
        || registered.command == *first.key()
    {
        return Err(refused(
            "standalone first Patch requires a preinitialized v1 root",
        ));
    }
    // A 39→40 migration registers the original immutable subject schema exactly;
    // later v2 policy extensions do not replace this root or source fingerprint.
    let matches_subject: bool = store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM catalog_metadata_subjects WHERE item_id=? AND contract_version=1 AND schema_fingerprint=?)",
        rusqlite::params![subject, schema_fingerprint], |r| r.get(0),
    )?;
    if !matches_subject {
        return Err(refused(
            "first Patch schema is not the retained v1 subject root",
        ));
    }
    budget.raw_json(edits_json, MAX_METADATA_JSON_DEPTH)?;
    // The parser below builds a second evidence tree, independently of the
    // closed evidence inspection above; admit that decode before constructing it.
    budget.raw_json(evidence_json, MAX_METADATA_JSON_DEPTH)?;
    let patch = ContributionPatch::from_json(
        &registered.value,
        ContributionKey::new(subject, namespace, record_id).map_err(refused)?,
        SourceKind::Provider,
        0,
        edits_json,
        evidence_json,
    )
    .map_err(refused)?;
    // With no prior source, Motion's full-replacement adapter emits only Set.
    // An empty first contribution is valid; v1 still requires a nonempty schema.
    if patch
        .edits()
        .iter()
        .any(|(field, edit)| field.starts_with("motion_") || !matches!(edit, FieldEdit::Set(_)))
    {
        return Err(refused(
            "first Motion Patch is not public Set-only replacement",
        ));
    }
    let expected = plan_update(&registered.value, None, &patch).map_err(refused)?;
    let actual = row(store, subject, namespace, record_id, 1, &mut budget)?;
    budget.serialized(&mutation, MAX_COMMAND_JSON_DEPTH)?;
    let raw = serde_json::to_string(&mutation)?;
    compare(&actual, &expected, &raw, first, &mut budget)?;
    budget.serialized(first.payload(), MAX_COMMAND_JSON_DEPTH)?;
    budget.raw_json(first.canonical_payload(), MAX_COMMAND_JSON_DEPTH)?;
    Ok(VerifiedNfoBaseline {
        subject: subject.clone(),
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
