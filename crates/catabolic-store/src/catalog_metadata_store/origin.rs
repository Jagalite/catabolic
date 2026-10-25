//! Receipt-proven Motion create origin. This token is not deserializable and
//! cannot be minted from an item marker, historical projection or request body.
use super::*;
use crate::catalog_commands::{self, CommandOutcome, TrustedCommandAuthority};

#[derive(Debug)]
pub struct VerifiedCreatedOrigin {
    pub(super) subject: String,
    pub(super) catalog: String,
    pub(super) host_namespace: String,
    pub(super) incarnation: String,
    pub(super) generation: i64,
    pub(super) intent: String,
    pub(super) title: Value,
}
pub fn verify_created_origin(
    store: &crate::Store,
    creation: &CommandEnvelope,
    subject: &str,
    authority: TrustedCommandAuthority<'_>,
) -> Result<VerifiedCreatedOrigin> {
    verify_created_origin_inner(store, creation, subject, authority, None)
}

/// Exact private first-source capability, never a general schema41 origin API.
pub(crate) fn verify_created_origin_nfo(
    store: &crate::Store,
    creation: &CommandEnvelope,
    subject: &str,
    authority: TrustedCommandAuthority<'_>,
    absence: &crate::catalog_nfo_attribution::VerifiedFirstNfoAbsence,
) -> Result<VerifiedCreatedOrigin> {
    absence
        .validate_origin_in(store, creation, subject)
        .map_err(|error| fail(MetadataStoreErrorCode::AuthorityDenied, error.to_string()))?;
    verify_created_origin_inner(store, creation, subject, authority, Some(absence))
}

fn verify_created_origin_inner(
    store: &crate::Store,
    creation: &CommandEnvelope,
    subject: &str,
    authority: TrustedCommandAuthority<'_>,
    first_nfo: Option<&crate::catalog_nfo_attribution::VerifiedFirstNfoAbsence>,
) -> Result<VerifiedCreatedOrigin> {
    if !matches!(
        (store.schema_version, first_nfo.is_some()),
        (40, false) | (41, true)
    ) || authority.allow_new_commands
        || authority.receipt_visibility
            != catabolic_core::catalog_command::ReceiptVisibility::FullResult
    {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "full receipt-only origin verification required",
        ));
    }
    let result = if let Some(absence) = first_nfo {
        // The first-source owner already retains the sole transaction. Do not
        // call the top-level reconciliation API or create a nested transaction.
        absence
            .read_creation_result_in(store, creation, subject, authority)
            .map_err(|e| fail(MetadataStoreErrorCode::AuthorityDenied, e.to_string()))?
    } else {
        let outcome = catalog_commands::reconcile_receipt(store, creation, authority)
            .map_err(|e| fail(MetadataStoreErrorCode::AuthorityDenied, e.to_string()))?;
        let Some(CommandOutcome::Replay { result, .. }) = outcome else {
            return Err(fail(
                MetadataStoreErrorCode::AuthorityDenied,
                "origin requires exact retained full create receipt",
            ));
        };
        result
    };
    if creation.payload()["operation"] != "item_create_batch"
        || result["operation"] != "item_create_batch"
    {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "origin is not a create-only receipt",
        ));
    }
    let items = creation.payload()["items"]
        .as_array()
        .ok_or_else(|| corrupt("origin create items absent"))?;
    let matching: Vec<_> = items
        .iter()
        .filter(|item| item["id"].as_str() == Some(subject))
        .collect();
    if matching.len() != 1
        || !matching[0]["identities"].is_null()
        || !result["items"].as_array().is_some_and(|items| {
            items
                .iter()
                .filter(|item| item["id"].as_str() == Some(subject))
                .count()
                == 1
        })
    {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "origin subject is not exact create-only item",
        ));
    }
    let metadata = matching[0]["metadata"]
        .as_object()
        .ok_or_else(|| corrupt("origin metadata absent"))?;
    if metadata.len() != 2
        || !metadata.contains_key("title")
        || !metadata["title"].is_string()
        || metadata
            .get("motion_catalog")
            .is_none_or(|m| m["contract_version"] != 1 || m["item_id"].as_str() != Some(subject))
    {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "origin lacks exact Motion creation structure",
        ));
    }
    let (item_revision, current_metadata, catalog, incarnation, generation): (i64, String, String, String, i64) = store.db.query_row(
        "SELECT r.revision,i.metadata,c.catalog_id,c.incarnation,c.generation FROM items i JOIN catalog_entity_revisions r ON r.entity_kind='items' AND r.entity_key=json_array(i.id) AND r.deleted=0 CROSS JOIN catalog_state c WHERE i.id=? AND c.id=1 AND length(CAST(i.metadata AS BLOB))<=65536",
        [subject], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
    )?;
    let bytes = current_metadata.len() + creation.canonical_payload().len();
    store
        .catalog_operation_budget()?
        .charge(crate::operation_budget::BudgetCharge {
            bytes,
            values: metadata.len(),
            work: bytes / 64 + metadata.len() + 1,
            ..Default::default()
        })
        .map_err(|_| {
            fail(
                MetadataStoreErrorCode::StoreFailure,
                "origin verification budget exhausted",
            )
        })?;
    if item_revision != 1
        || decode_json(&current_metadata).map_err(|_| corrupt("invalid origin metadata"))?
            != matching[0]["metadata"]
    {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "origin requires unchanged create-only native item revision and metadata",
        ));
    }
    if catalog != creation.key().catalog_id() || incarnation != creation.admission_incarnation() {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "origin catalog incarnation changed",
        ));
    }
    Ok(VerifiedCreatedOrigin {
        subject: subject.into(),
        catalog,
        host_namespace: creation.key().host_namespace().into(),
        incarnation,
        generation,
        intent: creation.canonical_payload().into(),
        title: metadata["title"].clone(),
    })
}

pub(super) fn plan_origin(
    schema: &FieldSchema,
    documents: &mut Vec<ContributionDocument>,
    subject: &str,
    intent: Option<&str>,
    command: &CommandEnvelope,
    grant: Option<&VerifiedCreatedOrigin>,
) -> Result<Vec<ContributionDocument>> {
    let Some(intent) = intent else {
        return Ok(vec![]);
    };
    let grant = grant.ok_or_else(|| {
        fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "verified create origin grant absent",
        )
    })?;
    if grant.subject != subject
        || grant.catalog != command.key().catalog_id()
        || grant.host_namespace != command.key().host_namespace()
        || grant.incarnation != command.admission_incarnation()
        || grant.generation != command.expected_generation()
        || grant.intent != intent
    {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "origin proof differs from exact current command",
        ));
    }
    let legacy = documents
        .iter()
        .find(|doc| {
            doc.key().namespace() == LEGACY_NAMESPACE && doc.key().record_id() == LEGACY_RECORD_ID
        })
        .ok_or_else(|| corrupt("origin legacy baseline absent"))?;
    if legacy.fields().get("title") != Some(&grant.title)
        || documents
            .iter()
            .any(|doc| doc.key().namespace() == "motion.scan")
    {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "origin does not match current exact baseline or scan already exists",
        ));
    }
    let reconciled =
        plan_legacy_reconciliation(schema, legacy, legacy.revision(), &["title".to_owned()])?;
    let scan=ContributionDocument::from_json(schema,ContributionKey::new(subject,"motion.scan","origin")?,SourceKind::Scan,1,&json!({"title":grant.title}).to_string(),&json!({"owner":"motion.receipt-origin.v1","creation_intent_digest":catabolic_core::sha256(intent)}).to_string())?;
    documents.retain(|doc| doc.key() != reconciled.key());
    documents.push(reconciled.clone());
    documents.push(scan.clone());
    Ok(vec![reconciled, scan])
}
