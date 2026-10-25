//! Atomic reference association, not a byte service. The embedding independently
//! verifies Motion-owned bytes and grants exactly one immutable descriptor. This
//! owner reads current references under the dispatcher writer, merges them with
//! the shared pure planner, and never opens a file or grants access to bytes.
use super::*;
use catabolic_core::artwork::native::{
    AssetReference, AssociationChange, ReferenceState, plan_associate_upload,
};

const NAMESPACE: &str = "motion.artwork";
const RECORD_ID: &str = "source_projection";
const FIELD: &str = "motion_artwork";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataArtworkAttach {
    pub contract_version: u32,
    pub subject: String,
    pub asset: AssetReference,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_creation_intent: Option<String>,
}

/// Non-serializable host capability for one reference association. The host must
/// independently authorize this subject and verify this exact descriptor against
/// its immutable asset service. This is NOT byte-read or filesystem authority.
#[derive(Clone, Debug)]
pub struct ArtworkAttachAuthority {
    pub host_namespace: String,
    pub subject: String,
    pub asset: AssetReference,
}

pub(crate) fn attach_artwork(
    store: &crate::Store,
    request: &MetadataArtworkAttach,
    command: &CommandEnvelope,
    attach: &ArtworkAttachAuthority,
    source: &MetadataAuthority,
    initialization: &MetadataAuthority,
    extension: &MetadataAuthority,
    origin: Option<&VerifiedCreatedOrigin>,
    time: i64,
) -> Result<Value> {
    if store.schema_version != 40
        || request.contract_version != 1
        || store.db.is_autocommit()
        || time < 0
    {
        return Err(fail(
            MetadataStoreErrorCode::UnsupportedContract,
            "artwork attachment requires native40 writer",
        ));
    }
    if attach.host_namespace != command.key().host_namespace()
        || attach.subject != request.subject
        || attach.asset != request.asset
    {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "artwork attachment requires the exact independently verified descriptor grant",
        ));
    }
    for grant in [source, initialization, extension] {
        if grant.host_namespace != command.key().host_namespace()
            || grant.subject != request.subject
        {
            return Err(fail(
                MetadataStoreErrorCode::AuthorityDenied,
                "artwork attachment grant identity mismatch",
            ));
        }
    }
    if !matches!(&source.access, MetadataAccess::SourceV2 {
        namespace, record_id, kind: SourceKind::Scan, system_fields,
    } if namespace == NAMESPACE && record_id == RECORD_ID
        && system_fields.len() == 1 && system_fields.contains(FIELD))
        || !matches!(initialization.access, MetadataAccess::InitializeV2 { .. })
        || !matches!(extension.access, MetadataAccess::ExtendSchema)
    {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "artwork attachment requires exact source, initialization and schema grants",
        ));
    }
    if command.payload() != &json!({"operation":"metadata_artwork_attach","request":request}) {
        return Err(fail(
            MetadataStoreErrorCode::InvalidInput,
            "artwork attachment payload mismatch",
        ));
    }
    let raw = serde_json::to_string(request)?;
    if raw.len() > MAX_RAW_MUTATION_BYTES {
        return Err(MetadataError::DocumentBudget.into());
    }
    let budget = store.catalog_operation_budget()?;
    let charge = |bytes: usize, values: usize| {
        budget
            .charge(crate::operation_budget::BudgetCharge {
                bytes,
                values,
                work: bytes / 64 + values + 1,
                ..Default::default()
            })
            .map_err(|_| {
                fail(
                    MetadataStoreErrorCode::StoreFailure,
                    "artwork attachment budget exhausted",
                )
            })
    };
    charge(raw.len(), 8)?;
    let db = &store.db;
    check_read_admission(
        db,
        &MetadataReadAuthority {
            catalog_id: command.key().catalog_id().into(),
            incarnation: command.admission_incarnation().into(),
            subject: request.subject.clone(),
            expected_generation: command.expected_generation(),
        },
    )?;
    if db
        .prepare("SELECT 1 FROM catalog_logical_retirements WHERE item_id=?")?
        .exists([&request.subject])?
    {
        return Err(fail(
            MetadataStoreErrorCode::RetiredSubject,
            "retired artwork attachment subject",
        ));
    }
    let bytes: Option<i64> = db
        .query_row(
            "SELECT length(CAST(metadata AS BLOB)) FROM items WHERE id=?",
            [&request.subject],
            |row| row.get(0),
        )
        .optional()?;
    let bytes = bytes.ok_or_else(|| {
        fail(
            MetadataStoreErrorCode::SubjectMissing,
            "artwork subject absent",
        )
    })?;
    let bytes = usize::try_from(bytes).map_err(|_| corrupt("negative metadata size"))?;
    if bytes > MAX_METADATA_SNAPSHOT_BYTES {
        return Err(MetadataError::SnapshotBudget.into());
    }
    charge(bytes, 1)?;
    let raw_metadata: String = db.query_row(
        "SELECT metadata FROM items WHERE id=?",
        [&request.subject],
        |row| row.get(0),
    )?;
    let metadata = decode_json(&raw_metadata).map_err(|_| corrupt("invalid artwork projection"))?;
    let fingerprint = schema_registry::current_fingerprint(db, &request.subject)?;
    let mut revision = 0;
    let mut owned_value = None;
    if fingerprint.is_some() {
        schema_registry::charge_retained_schemas(db, &budget, &request.subject)?;
        let schema = read_schema(db, &request.subject)?;
        let (source_bytes, source_fields): (i64, i64) = db.query_row(
            "SELECT coalesce(sum(length(CAST(fields_json AS BLOB))+length(CAST(evidence_json AS BLOB))),0),(SELECT count(*) FROM catalog_metadata_sources s,json_each(s.fields_json) WHERE s.item_id=?1) FROM catalog_metadata_sources WHERE item_id=?1",
            [&request.subject], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        charge(
            usize::try_from(source_bytes).map_err(|_| corrupt("negative source size"))?,
            usize::try_from(source_fields).map_err(|_| corrupt("negative source fields"))?
                + schema.fields().len(),
        )?;
        let documents = read_documents(db, &schema, &request.subject)?;
        let resolution = resolve(&schema, &request.subject, &documents)?;
        let MetadataAccess::InitializeV2 { system_fields } = &initialization.access else {
            unreachable!()
        };
        for field in system_fields {
            if !schema.fields().contains_key(field)
                || (schema.contract_version() != 1 && !schema.system_fields().contains(field))
            {
                return Err(MetadataError::SchemaMismatch.into());
            }
        }
        for document in &documents {
            if document.key().namespace() == NAMESPACE && document.key().record_id() == RECORD_ID {
                if document.kind() != SourceKind::Scan
                    || document.fields().keys().any(|field| field != FIELD)
                {
                    return Err(fail(
                        MetadataStoreErrorCode::AuthorityDenied,
                        "artwork source has an unexpected kind or extra fields",
                    ));
                }
                revision = document.revision();
                owned_value = document.fields().get(FIELD).cloned();
            } else if document.fields().contains_key(FIELD) {
                return Err(fail(
                    MetadataStoreErrorCode::AuthorityDenied,
                    "artwork field has another current source owner",
                ));
            }
        }
        if owned_value.is_some()
            && schema.contract_version() != 1
            && !schema.system_fields().contains(FIELD)
        {
            return Err(MetadataError::SchemaMismatch.into());
        }
        let effective = match resolution.fields.get(FIELD) {
            Some(FieldResolution::Resolved { value, .. }) => Some(value),
            Some(FieldResolution::Absent) | None => None,
            _ => return Err(corrupt("artwork source is not uniquely resolved")),
        };
        if effective != owned_value.as_ref() {
            return Err(corrupt("artwork source and resolver diverged"));
        }
    }
    if metadata.get(FIELD) != owned_value.as_ref() {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "artwork effective field lacks exact current source ownership",
        ));
    }
    let state = match owned_value {
        Some(value) => ReferenceState::parse_json(&value.to_string(), &request.subject),
        None => ReferenceState::empty(&request.subject),
    }
    .map_err(|error| corrupt(format!("invalid current artwork references: {error}")))?;
    let plan = plan_associate_upload(&state, &request.asset)
        .map_err(|error| fail(MetadataStoreErrorCode::InvalidInput, error.to_string()))?;
    if plan.change == AssociationChange::Unchanged {
        return Ok(json!({
            "operation":"metadata_artwork_attach", "subject":request.subject,
            "asset":request.asset, "change":plan.change,
            "source_revision":revision, "schema_fingerprint":fingerprint,
        }));
    }
    let next_json = plan
        .state
        .to_json()
        .map_err(|error| fail(MetadataStoreErrorCode::InvalidInput, error.to_string()))?;
    charge(next_json.len(), 1)?;
    let replacement = MetadataSystemReplace {
        contract_version: 1,
        subject: request.subject.clone(),
        namespace: NAMESPACE.into(),
        record_id: RECORD_ID.into(),
        expected_schema_fingerprint: fingerprint,
        expected_revision: revision,
        fields_json: json!({"motion_artwork":serde_json::from_str::<Value>(&next_json)?})
            .to_string(),
        evidence_json: json!({
            "owner":"motion.artwork.attach.v1",
            "outer_command_digest":command.digest().as_str(),
            "asset_id":request.asset.asset_id,
            "content_sha256":request.asset.content_sha256,
        })
        .to_string(),
        origin_creation_intent: request.origin_creation_intent.clone(),
    };
    let inner = CommandEnvelope::new(
        command.contract_version(),
        command.key().clone(),
        command.admission_incarnation(),
        command.expected_generation(),
        &json!({"operation":"metadata_system_replace","request":replacement}).to_string(),
    )
    .map_err(|error| fail(MetadataStoreErrorCode::InvalidInput, error.to_string()))?;
    let result = replace_system(
        store,
        &replacement,
        &inner,
        source,
        initialization,
        extension,
        origin,
        time,
    )?;
    Ok(json!({
        "operation":"metadata_artwork_attach", "subject":request.subject,
        "asset":request.asset, "change":plan.change,
        "source_revision":result["source"]["revision"],
        "schema_fingerprint":result["schema_fingerprint"],
    }))
}
