//! Historical native-created NFO revision one, never current mutation authority.
mod v1;
mod v1_initialize;
use super::{VerifiedNfoBaseline, proof_budget::ProofBudget, verify_committed_command_in};
use crate::{
    Error, Result, Store,
    catalog_commands::TrustedCommandAuthority,
    catalog_metadata_store::{self as metadata, ContributionReplace},
    operation_budget::BudgetCharge,
};
use catabolic_core::{
    catalog_command::{CommandEnvelope, CommandKey, MAX_COMMAND_JSON_DEPTH},
    catalog_metadata::{
        ContributionDocument, ContributionKey, ContributionReplacement, FieldPolicy, FieldSchema,
        MAX_METADATA_JSON_DEPTH, Normalization, SourceKind, ValueType, plan_legacy_reconciliation,
        plan_replace, plan_schema_extension, revalidate_document,
    },
};
use rusqlite::params;
use serde_json::{Value, json};

/// Prepared from the complete retained Motion source/identity/origin inventory
/// and exact validated HTTP intent chain. The audit positively establishes BOTH
/// absent automatic NFO identity and absent sidecar attribution before this first
/// API contribution. Neither omitted request fields nor current JSON prove it.
#[derive(Debug)]
pub(crate) struct NfoCreatedSourceAuthority {
    pub host_namespace: String,
    pub subject: String,
    pub request_digest: String,
    pub complete_absence_audit_digest: String,
}
fn refused(message: impl std::fmt::Display) -> Error {
    Error(format!("catalog_nfo_created_provenance_refused: {message}"))
}
fn sql_budget_count(value: i64, maximum: usize) -> Result<usize> {
    let value = usize::try_from(value).map_err(|_| refused("negative SQL resource count"))?;
    if value > maximum {
        return Err(refused("SQL resource count exceeds owner budget"));
    }
    Ok(value)
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
struct Schema {
    value: FieldSchema,
    prior: Option<String>,
    command: CommandKey,
}
fn schema(
    store: &Store,
    subject: &str,
    fingerprint: &str,
    budget: &mut ProofBudget,
) -> Result<Schema> {
    let (bytes,nodes):(i64,i64)=store.db.query_row(
        "SELECT length(CAST(policies_json AS BLOB))+length(CAST(system_fields_json AS BLOB))+1024,5*json_array_length(policies_json)+json_array_length(system_fields_json)+8 FROM catalog_metadata_schemas WHERE item_id=? AND schema_fingerprint=?",
        params![subject,fingerprint],|r| Ok((r.get(0)?,r.get(1)?)))?;
    let bytes = sql_budget_count(
        bytes,
        catabolic_core::catalog_metadata::MAX_METADATA_SNAPSHOT_BYTES,
    )?;
    let nodes = sql_budget_count(
        nodes,
        catabolic_core::catalog_metadata::MAX_METADATA_JSON_NODES,
    )?;
    budget.charge(BudgetCharge {
        bytes,
        values: nodes,
        work: bytes / 64 + nodes + 1,
        ..Default::default()
    })?;
    let (prior,catalog,host,operation):(Option<String>,String,String,String)=store.db.query_row(
        "SELECT prior_fingerprint,command_catalog_id,command_host_namespace,command_operation_id FROM catalog_metadata_schemas WHERE item_id=? AND schema_fingerprint=?",
        params![subject,fingerprint],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    Ok(Schema {
        value: metadata::read_registered_schema(&store.db, subject, fingerprint)
            .map_err(refused)?,
        prior,
        command: CommandKey::new(catalog, host, operation).map_err(refused)?,
    })
}
struct Row {
    revision: i64,
    kind: String,
    fingerprint: String,
    fields: String,
    evidence: String,
    raw: String,
    command: CommandKey,
}
fn row(
    store: &Store,
    subject: &str,
    namespace: &str,
    record: &str,
    revision: i64,
    budget: &mut ProofBudget,
) -> Result<Row> {
    let bytes:i64=store.db.query_row(
        "SELECT length(CAST(fields_json AS BLOB))+length(CAST(evidence_json AS BLOB))+length(CAST(raw_mutation_json AS BLOB))+1024 FROM catalog_metadata_history WHERE item_id=? AND namespace=? AND record_id=? AND revision=?",
        params![subject,namespace,record,revision],|r|r.get(0))?;
    let bytes = sql_budget_count(
        bytes,
        catabolic_core::catalog_metadata::MAX_METADATA_SNAPSHOT_BYTES,
    )?;
    budget.charge(BudgetCharge {
        bytes,
        values: 9,
        work: bytes / 64 + 10,
        ..Default::default()
    })?;
    let (kind,fingerprint,fields,evidence,raw,catalog,host,operation):(String,String,String,String,String,String,String,String)=store.db.query_row(
        "SELECT source_kind,schema_fingerprint,fields_json,evidence_json,raw_mutation_json,command_catalog_id,command_host_namespace,command_operation_id FROM catalog_metadata_history WHERE item_id=? AND namespace=? AND record_id=? AND revision=?",
        params![subject,namespace,record,revision],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?)))?;
    budget.raw_json(&fields, MAX_METADATA_JSON_DEPTH)?;
    budget.raw_json(&evidence, MAX_METADATA_JSON_DEPTH)?;
    Ok(Row {
        revision,
        kind,
        fingerprint,
        fields,
        evidence,
        raw,
        command: CommandKey::new(catalog, host, operation).map_err(refused)?,
    })
}
fn compare(
    row: &Row,
    expected: &ContributionDocument,
    raw: &str,
    command: &CommandEnvelope,
    budget: &mut ProofBudget,
) -> Result<()> {
    budget.serialized(expected.fields(), MAX_METADATA_JSON_DEPTH)?;
    budget.canonical_json(expected.evidence())?;
    if row.revision != expected.revision()
        || row.kind
            != match expected.kind() {
                SourceKind::Local => "local",
                SourceKind::Provider => "provider",
                SourceKind::Scan => "scan",
                SourceKind::LegacyImport => "legacy_import",
            }
        || row.fingerprint != expected.schema_fingerprint()
        || row.fields != serde_json::to_string(expected.fields())?
        || row.evidence != catabolic_core::encode(expected.evidence())
        || row.raw != raw
        || row.command != *command.key()
    {
        return Err(refused(
            "immutable source row contradicts reconstructed first effect",
        ));
    }
    Ok(())
}
fn root_initialization(
    store: &Store,
    subject: &str,
    first: &CommandEnvelope,
    budget: &mut ProofBudget,
) -> Result<Schema> {
    let bytes:i64=store.db.query_row("SELECT length(CAST(legacy_raw_json AS BLOB))+length(CAST(policies_json AS BLOB))+1024 FROM catalog_metadata_subjects WHERE item_id=?",[subject],|r|r.get(0))?;
    let bytes = sql_budget_count(
        bytes,
        catabolic_core::catalog_metadata::MAX_METADATA_SNAPSHOT_BYTES,
    )?;
    budget.charge(BudgetCharge {
        bytes,
        values: 8,
        work: bytes / 64 + 9,
        ..Default::default()
    })?;
    let (version,fingerprint,legacy,catalog,host,operation):(u32,String,String,String,String,String)=store.db.query_row(
        "SELECT contract_version,schema_fingerprint,legacy_raw_json,command_catalog_id,command_host_namespace,command_operation_id FROM catalog_metadata_subjects WHERE item_id=?",[subject],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
    if version != 2 || CommandKey::new(catalog, host, operation).map_err(refused)? != *first.key() {
        return Err(refused(
            "first initialization is not owned by creation command",
        ));
    }
    budget.raw_json(&legacy, MAX_METADATA_JSON_DEPTH)?;
    let values = catabolic_core::json::parse(&legacy, true).map_err(refused)?;
    let values = values
        .as_object()
        .ok_or_else(|| refused("initial legacy value is not an object"))?;
    let root = schema(store, subject, &fingerprint, budget)?;
    if root.prior.is_some()
        || root.command != *first.key()
        || root.value.fields().len() != values.len()
        || values.keys().any(|key| {
            root.value.fields().get(key)
                != Some(&FieldPolicy {
                    value_type: ValueType::Json,
                    nullable: false,
                    normalization: Normalization::Exact,
                })
        })
    {
        return Err(refused(
            "first initialization policy differs from exact legacy baseline",
        ));
    }
    let request = metadata::MetadataInitializeV2 {
        contract_version: 2,
        subject: subject.into(),
        policies: values
            .keys()
            .map(|field| metadata::PolicyField {
                field: field.clone(),
                value_type: metadata::PolicyValueType::Json,
                nullable: false,
                normalization: metadata::PolicyNormalization::Exact,
            })
            .collect(),
        system_fields: root.value.system_fields().clone(),
    };
    budget.serialized(&request, MAX_COMMAND_JSON_DEPTH)?;
    let expected = ContributionDocument::from_json_with_system_fields(
        &root.value,
        ContributionKey::new(
            subject,
            metadata::LEGACY_NAMESPACE,
            metadata::LEGACY_RECORD_ID,
        )
        .map_err(refused)?,
        SourceKind::LegacyImport,
        1,
        &legacy,
        r#"{"origin":"unknown","import":"items.metadata"}"#,
    )
    .map_err(refused)?;
    let actual = row(
        store,
        subject,
        metadata::LEGACY_NAMESPACE,
        metadata::LEGACY_RECORD_ID,
        1,
        budget,
    )?;
    compare(
        &actual,
        &expected,
        &serde_json::to_string(&request)?,
        first,
        budget,
    )?;
    Ok(root)
}

pub(crate) fn verify_created_no_origin_baseline_in(
    store: &Store,
    first: &CommandEnvelope,
    grant: &NfoCreatedSourceAuthority,
    authority: TrustedCommandAuthority<'_>,
    expected_generation: i64,
) -> Result<VerifiedNfoBaseline> {
    if first.payload()["operation"] == "metadata_initialize_and_patch" {
        return v1_initialize::verify_initialize_and_patch_in(
            store,
            first,
            grant,
            authority,
            expected_generation,
        );
    }
    if first.payload()["operation"] == "metadata" {
        return v1::verify_first_patch_in(store, first, grant, authority, expected_generation);
    }
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
    let request: ContributionReplace = serde_json::from_value(first.payload()["request"].clone())?;
    if first.payload().as_object().is_none_or(|o| o.len() != 2)
        || first.payload()["operation"] != "metadata_contribution_replace"
        || request.contract_version != 2
        || request.subject != grant.subject
        || request.namespace != "motion.nfo"
        || request.record_id != "document"
        || request.expected_revision != 0
    {
        return Err(refused("not an exact first native40 NFO contribution"));
    }
    budget.raw_json(&request.fields_json, MAX_METADATA_JSON_DEPTH)?;
    budget.raw_json(&request.evidence_json, MAX_METADATA_JSON_DEPTH)?;
    let evidence = catabolic_core::json::parse(&request.evidence_json, true).map_err(refused)?;
    if evidence.as_object().is_none_or(|o| o.len() != 3)
        || evidence["owner"] != "motion.metadata.replace"
        || evidence["request_digest"].as_str() != Some(&grant.request_digest)
        || evidence["expected_aggregate_revision"].as_u64().is_none()
    {
        return Err(refused(
            "first source is not exact independently audited Motion intent",
        ));
    }
    let actual = row(
        store,
        &request.subject,
        "motion.nfo",
        "document",
        1,
        &mut budget,
    )?;
    let before = match request.expected_schema_fingerprint.as_deref() {
        Some(fingerprint) => schema(store, &request.subject, fingerprint, &mut budget)?,
        None => root_initialization(store, &request.subject, first, &mut budget)?,
    };
    let after = schema(store, &request.subject, &actual.fingerprint, &mut budget)?;
    if before.value.fingerprint() != after.value.fingerprint()
        && (after.prior.as_deref() != Some(before.value.fingerprint())
            || after.command != *first.key())
    {
        return Err(refused(
            "first source schema step is not exact command-owned successor",
        ));
    }
    let values = catabolic_core::catalog_metadata::replacement_fields(&request.fields_json)
        .map_err(refused)?;
    if values.keys().any(|name| name.starts_with("motion_")) {
        return Err(refused("reserved source fields"));
    }
    let additions = values
        .keys()
        .filter(|key| !before.value.fields().contains_key(*key))
        .map(|key| {
            (
                key.clone(),
                FieldPolicy {
                    value_type: ValueType::Json,
                    nullable: false,
                    normalization: Normalization::Exact,
                },
            )
        })
        .collect();
    let system = before
        .value
        .fields()
        .iter()
        .filter(|(key, _)| after.value.system_fields().contains(*key))
        .map(|(key, policy)| (key.clone(), *policy))
        .collect();
    if plan_schema_extension(&before.value, additions, system).map_err(refused)? != after.value {
        return Err(refused("first source schema does not reconstruct"));
    }
    let replacement = ContributionReplacement::from_json(
        &after.value,
        ContributionKey::new(&request.subject, "motion.nfo", "document").map_err(refused)?,
        SourceKind::Provider,
        0,
        &request.fields_json,
        &request.evidence_json,
    )
    .map_err(refused)?;
    let expected = plan_replace(&after.value, None, &replacement).map_err(refused)?;
    budget.serialized(&request, MAX_COMMAND_JSON_DEPTH)?;
    let raw = serde_json::to_string(&request)?;
    compare(&actual, &expected, &raw, first, &mut budget)?;
    if let Some(intent) = request.origin_creation_intent.as_deref() {
        verify_title_origin(
            store,
            &request.subject,
            intent,
            first,
            &before.value,
            &after.value,
            &raw,
            authority,
            expected_generation,
            &mut budget,
        )?;
    }
    // Only the independently complete absence audit plus the first-source
    // operation contract establish absence. No present projection is consulted.
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

fn verify_title_origin(
    store: &Store,
    subject: &str,
    intent: &str,
    first: &CommandEnvelope,
    before: &FieldSchema,
    after: &FieldSchema,
    raw: &str,
    authority: TrustedCommandAuthority<'_>,
    generation: i64,
    budget: &mut ProofBudget,
) -> Result<()> {
    budget.raw_json(intent, MAX_COMMAND_JSON_DEPTH)?;
    let saved = catabolic_core::json::parse(intent, true).map_err(refused)?;
    let creation = CommandEnvelope::new(
        1,
        serde_json::from_value(saved["key"].clone())?,
        saved["admission_incarnation"]
            .as_str()
            .ok_or_else(|| refused("creation incarnation absent"))?,
        saved["expected_generation"]
            .as_i64()
            .ok_or_else(|| refused("creation generation absent"))?,
        &saved["payload"].to_string(),
    )
    .map_err(refused)?;
    if creation.canonical_payload() != intent
        || creation.payload()["operation"] != "item_create_batch"
    {
        return Err(refused(
            "historical origin is not canonical create-only intent",
        ));
    }
    let committed = verify_committed_command_in(store, &creation, authority, generation)?;
    if committed.committed_generation() > first.expected_generation() {
        return Err(refused("creation follows first source"));
    }
    let items = creation.payload()["items"]
        .as_array()
        .ok_or_else(|| refused("creation items absent"))?;
    let mut matching = items
        .iter()
        .filter(|item| item["id"].as_str() == Some(subject));
    let item = matching
        .next()
        .ok_or_else(|| refused("creation subject absent"))?;
    if matching.next().is_some() || !item["identities"].is_null() {
        return Err(refused("ambiguous creation subject"));
    }
    let original = item["metadata"]
        .as_object()
        .ok_or_else(|| refused("creation metadata absent"))?;
    if original.len() != 2
        || !original.get("title").is_some_and(Value::is_string)
        || original
            .get("motion_catalog")
            .is_none_or(|m| m["contract_version"] != 1 || m["item_id"].as_str() != Some(subject))
    {
        return Err(refused(
            "creation is not exact Motion title/structure origin",
        ));
    }
    let mut query=store.db.prepare("SELECT revision FROM catalog_metadata_history WHERE item_id=? AND namespace=? AND record_id=? AND command_catalog_id=? AND command_host_namespace=? AND command_operation_id=? ORDER BY revision DESC LIMIT 3")?;
    let revisions = query
        .query_map(
            params![
                subject,
                metadata::LEGACY_NAMESPACE,
                metadata::LEGACY_RECORD_ID,
                first.key().catalog_id(),
                first.key().host_namespace(),
                first.key().operation_id()
            ],
            |r| r.get::<_, i64>(0),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if revisions.is_empty()
        || revisions.len() > 2
        || revisions[0] < 2
        || (revisions.len() == 2 && revisions != [2, 1])
    {
        return Err(refused(
            "title reconciliation history is incomplete or ambiguous",
        ));
    }
    let previous = row(
        store,
        subject,
        metadata::LEGACY_NAMESPACE,
        metadata::LEGACY_RECORD_ID,
        revisions[0] - 1,
        budget,
    )?;
    let previous_schema = schema(store, subject, &previous.fingerprint, budget)?;
    if previous.kind != "legacy_import" {
        return Err(refused("title origin source kind changed"));
    }
    let previous_doc = ContributionDocument::from_json_with_system_fields(
        &previous_schema.value,
        ContributionKey::new(
            subject,
            metadata::LEGACY_NAMESPACE,
            metadata::LEGACY_RECORD_ID,
        )
        .map_err(refused)?,
        SourceKind::LegacyImport,
        previous.revision,
        &previous.fields,
        &previous.evidence,
    )
    .map_err(refused)?;
    let previous_doc =
        revalidate_document(before, &previous_schema.value, &previous_doc).map_err(refused)?;
    if previous_doc.fields().get("title") != original.get("title") {
        return Err(refused(
            "historical legacy title differs from exact creation",
        ));
    }
    let reconciled = plan_legacy_reconciliation(
        before,
        &previous_doc,
        previous_doc.revision(),
        &["title".into()],
    )
    .map_err(refused)?;
    let reconciled = revalidate_document(after, before, &reconciled).map_err(refused)?;
    let actual = row(
        store,
        subject,
        metadata::LEGACY_NAMESPACE,
        metadata::LEGACY_RECORD_ID,
        revisions[0],
        budget,
    )?;
    compare(&actual, &reconciled, raw, first, budget)?;
    let scan=ContributionDocument::from_json(before,ContributionKey::new(subject,"motion.scan","origin").map_err(refused)?,SourceKind::Scan,1,
        &json!({"title":original["title"]}).to_string(),
        &json!({"owner":"motion.receipt-origin.v1","creation_intent_digest":catabolic_core::sha256(intent)}).to_string()).map_err(refused)?;
    let scan = revalidate_document(after, before, &scan).map_err(refused)?;
    let actual = row(store, subject, "motion.scan", "origin", 1, budget)?;
    compare(&actual, &scan, raw, first, budget)
}

#[cfg(test)]
mod tests;
