//! Explicit native40 initialization/import composition. Caller retains the sole
//! dispatcher transaction and receipt; request data never supplies authority.
use super::*;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataInitializeV2 {
    pub contract_version: u32,
    pub subject: String,
    pub policies: Vec<PolicyField>,
    pub system_fields: BTreeSet<String>,
}
pub(crate) fn initialize_v2(
    store: &crate::Store,
    request: &MetadataInitializeV2,
    command: &CommandEnvelope,
    authority: &MetadataAuthority,
    time: i64,
) -> Result<Value> {
    initialize_inner(store, request, command, authority, time, None)
}

/// Only a first-source capability may retain the preinstalled deferred barrier.
/// The synthetic initialization bytes remain unchanged and share the outer key.
pub(super) fn initialize_nfo_v2(
    store: &crate::Store,
    request: &MetadataInitializeV2,
    command: &CommandEnvelope,
    authority: &MetadataAuthority,
    time: i64,
    permit: &crate::catalog_nfo_attribution::NfoWritePermit,
    outer: &CommandEnvelope,
) -> Result<Value> {
    permit
        .authorize_initialization_in(store, request, outer)
        .map_err(|e| fail(MetadataStoreErrorCode::AuthorityDenied, e.to_string()))?;
    if command.key() != outer.key()
        || command.admission_incarnation() != outer.admission_incarnation()
        || command.expected_generation() != outer.expected_generation()
        || command.contract_version() != outer.contract_version()
    {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "initialization outer scope differs",
        ));
    }
    initialize_inner(store, request, command, authority, time, Some(permit))
}
fn initialize_inner(
    store: &crate::Store,
    request: &MetadataInitializeV2,
    command: &CommandEnvelope,
    authority: &MetadataAuthority,
    time: i64,
    retained: Option<&crate::catalog_nfo_attribution::NfoWritePermit>,
) -> Result<Value> {
    let db = &store.db;
    if !matches!(store.schema_version, 40 | 41)
        || request.contract_version != 2
        || db.is_autocommit()
        || time < 0
    {
        return Err(fail(
            MetadataStoreErrorCode::UnsupportedContract,
            "native40 explicit initialization required",
        ));
    }
    if authority.subject != request.subject
        || authority.host_namespace != command.key().host_namespace()
        || !matches!(&authority.access, MetadataAccess::InitializeV2 { system_fields } if system_fields == &request.system_fields)
    {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "explicit system-policy initialization grant mismatch",
        ));
    }
    if command.payload() != &json!({"operation":"metadata_initialize_v2","request":request}) {
        return Err(fail(
            MetadataStoreErrorCode::InvalidInput,
            "initialization payload mismatch",
        ));
    }
    check_read_admission(
        db,
        &MetadataReadAuthority {
            catalog_id: command.key().catalog_id().into(),
            incarnation: command.admission_incarnation().into(),
            subject: request.subject.clone(),
            expected_generation: command.expected_generation(),
        },
    )?;
    legacy_key(&request.subject)?;
    if schema_registry::current_fingerprint(db, &request.subject)?.is_some() {
        return Err(fail(
            MetadataStoreErrorCode::AlreadyInitialized,
            "metadata already initialized",
        ));
    }
    if db
        .prepare("SELECT 1 FROM catalog_logical_retirements WHERE item_id=?")?
        .exists([&request.subject])?
    {
        return Err(fail(
            MetadataStoreErrorCode::RetiredSubject,
            "retired initialization subject",
        ));
    }
    let mut external = Vec::new();
    let mut system = Vec::new();
    for policy in &request.policies {
        let pair = (
            policy.field.clone(),
            schema_registry::convert_policy(policy),
        );
        if request.system_fields.contains(&policy.field) {
            system.push(pair);
        } else {
            external.push(pair);
        }
    }
    if system.len() != request.system_fields.len() {
        return Err(fail(
            MetadataStoreErrorCode::InvalidInput,
            "system policy absent or duplicated",
        ));
    }
    let schema = FieldSchema::new_v2(external, system)?;
    let raw: String = db.query_row(
        "SELECT metadata FROM items WHERE id=? AND length(CAST(metadata AS BLOB))<=?",
        params![request.subject, MAX_METADATA_DOCUMENT_BYTES as i64],
        |r| r.get(0),
    )?;
    let legacy = ContributionDocument::from_json_with_system_fields(
        &schema,
        legacy_key(&request.subject)?,
        SourceKind::LegacyImport,
        1,
        &raw,
        r#"{"origin":"unknown","import":"items.metadata"}"#,
    )?;
    let resolution = resolve(&schema, &request.subject, &[legacy.clone()])?;
    let mutation = serde_json::to_string(request)?;
    if mutation.len() > MAX_RAW_MUTATION_BYTES {
        return Err(MetadataError::DocumentBudget.into());
    }
    store
        .catalog_operation_budget()?
        .charge(crate::operation_budget::BudgetCharge {
            bytes: raw.len() + mutation.len(),
            values: schema.fields().len() + legacy.fields().len(),
            work: mutation.len() / 64 + schema.fields().len() + 1,
            ..Default::default()
        })
        .map_err(|failure| {
            fail(
                MetadataStoreErrorCode::StoreFailure,
                format!("initialization budget exhausted: {failure:?}"),
            )
        })?;
    if retained.is_none() {
        db.execute(
            "INSERT INTO catalog_metadata_permits(item_id,incarnation) VALUES (?,?)",
            params![request.subject, command.admission_incarnation()],
        )?;
    }
    let policies = serde_json::to_string(&request.policies)?;
    db.execute("INSERT INTO catalog_metadata_schemas(item_id,schema_fingerprint,contract_version,policies_json,system_fields_json,prior_fingerprint,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us) VALUES (?,?,2,?,?,NULL,?,?,?,?)",params![request.subject,schema.fingerprint(),policies,serde_json::to_string(&request.system_fields)?,command.key().catalog_id(),command.key().host_namespace(),command.key().operation_id(),time])?;
    db.execute("INSERT INTO catalog_metadata_subjects(item_id,contract_version,schema_fingerprint,policies_json,legacy_raw_json,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us) VALUES (?,2,?,?,?,?,?,?,?)",params![request.subject,schema.fingerprint(),policies,raw,command.key().catalog_id(),command.key().host_namespace(),command.key().operation_id(),time])?;
    write_revision(db, &legacy, &mutation, command, time)?;
    write_projection(db, &resolution)?;
    if retained.is_none() {
        db.execute(
            "DELETE FROM catalog_metadata_permits WHERE item_id=?",
            [&request.subject],
        )?;
    }
    Ok(
        json!({"operation":"metadata_initialize_v2","subject":request.subject,"schema_fingerprint":schema.fingerprint()}),
    )
}

pub(crate) fn import_source_v2(
    store: &crate::Store,
    subject: &str,
    namespace: &str,
    record_id: &str,
    fields: &Value,
    evidence: &Value,
    command: &CommandEnvelope,
    authority: &MetadataAuthority,
    time: i64,
) -> Result<Value> {
    import_source_v2_authorized(
        store, subject, namespace, record_id, fields, evidence, command, authority, time, None,
        None,
    )
}
pub(crate) fn import_source_v2_authorized(
    store: &crate::Store,
    subject: &str,
    namespace: &str,
    record_id: &str,
    fields: &Value,
    evidence: &Value,
    command: &CommandEnvelope,
    authority: &MetadataAuthority,
    time: i64,
    permit: Option<&crate::catalog_nfo_attribution::source_effect::ImportedSourceEffect<'_>>,
    validated: Option<&crate::catalog_import::ValidatedImportSource<'_>>,
) -> Result<Value> {
    if store.schema_version == 41 && fields.get("motion_catalog").is_some() {
        validated
            .ok_or_else(|| {
                fail(
                    MetadataStoreErrorCode::AuthorityDenied,
                    "protected projection import requires validated source",
                )
            })?
            .authorize_v2_in(
                store, command, subject, namespace, record_id, fields, evidence, authority,
            )
            .map_err(|e| fail(MetadataStoreErrorCode::AuthorityDenied, e.to_string()))?;
    }
    if store.schema_version == 41 && namespace == "motion.nfo" {
        permit
            .ok_or_else(|| {
                fail(
                    MetadataStoreErrorCode::AuthorityDenied,
                    "validated NFO import capability required",
                )
            })?
            .authorize_v2_in(
                store, command, subject, namespace, record_id, fields, evidence,
            )
            .map_err(|e| fail(MetadataStoreErrorCode::AuthorityDenied, e.to_string()))?;
    }
    let db = &store.db;
    if !matches!(store.schema_version, 40 | 41)
        || db.is_autocommit()
        || authority.subject != subject
        || authority.host_namespace != command.key().host_namespace()
    {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "v2 import source authority mismatch",
        ));
    }
    let (kind, allowed) = match &authority.access {
        MetadataAccess::SourceV2 {
            namespace: n,
            record_id: r,
            kind,
            system_fields,
        } if n == namespace && r == record_id => (*kind, system_fields.clone()),
        MetadataAccess::Source {
            namespace: n,
            record_id: r,
            kind,
        } if n == namespace && r == record_id => (*kind, BTreeSet::new()),
        _ => {
            return Err(fail(
                MetadataStoreErrorCode::AuthorityDenied,
                "v2 import source grant absent",
            ));
        }
    };
    if kind == SourceKind::LegacyImport {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "import cannot impersonate legacy baseline",
        ));
    }
    check_read_admission(
        db,
        &MetadataReadAuthority {
            catalog_id: command.key().catalog_id().into(),
            incarnation: command.admission_incarnation().into(),
            subject: subject.into(),
            expected_generation: command.expected_generation(),
        },
    )?;
    if command.payload()
        != &json!({"operation":"metadata_import_source_v2","subject":subject,"namespace":namespace,"record_id":record_id,"fields":fields,"evidence":evidence})
    {
        return Err(fail(
            MetadataStoreErrorCode::InvalidInput,
            "source import payload mismatch",
        ));
    }
    schema_registry::charge_retained_schemas(db, &store.catalog_operation_budget()?, subject)?;
    let schema = read_schema(db, subject)?;
    if schema.contract_version() != 2 {
        return Err(fail(
            MetadataStoreErrorCode::UnsupportedContract,
            "v2 source requires v2 schema",
        ));
    }
    let object = fields.as_object().ok_or(MetadataError::ExpectedObject)?;
    if !allowed.is_subset(schema.system_fields())
        || object
            .keys()
            .any(|key| schema.system_fields().contains(key) && !allowed.contains(key))
    {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "source cannot write another system owner field",
        ));
    }
    let key = ContributionKey::new(subject, namespace, record_id)?;
    let mut documents = read_documents(db, &schema, subject)?;
    if documents.iter().any(|doc| doc.key() == &key) {
        return Err(fail(
            MetadataStoreErrorCode::AlreadyInitialized,
            "import source already exists",
        ));
    }
    let next = ContributionDocument::from_json_with_system_fields(
        &schema,
        key,
        kind,
        1,
        &fields.to_string(),
        &evidence.to_string(),
    )?;
    documents.push(next.clone());
    let resolution = resolve(&schema, subject, &documents)?;
    let raw=json!({"operation":"metadata_import_source_v2","subject":subject,"namespace":namespace,"record_id":record_id,"fields":fields,"evidence":evidence}).to_string();
    if raw.len() > MAX_RAW_MUTATION_BYTES {
        return Err(MetadataError::DocumentBudget.into());
    }
    store
        .catalog_operation_budget()?
        .charge(crate::operation_budget::BudgetCharge {
            bytes: raw.len(),
            values: schema.fields().len() + object.len(),
            work: raw.len() / 64 + schema.fields().len() + object.len() + 1,
            ..Default::default()
        })
        .map_err(|_| {
            fail(
                MetadataStoreErrorCode::StoreFailure,
                "source import budget exhausted",
            )
        })?;
    db.execute(
        "INSERT INTO catalog_metadata_permits(item_id,incarnation) VALUES (?,?)",
        params![subject, command.admission_incarnation()],
    )?;
    write_revision(db, &next, &raw, command, time)?;
    write_projection(db, &resolution)?;
    db.execute(
        "DELETE FROM catalog_metadata_permits WHERE item_id=?",
        [subject],
    )?;
    Ok(json!({"schema_fingerprint":schema.fingerprint(),"source_revision":1}))
}
