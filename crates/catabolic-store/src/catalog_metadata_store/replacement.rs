//! Atomic attributed metadata/schema/tag replacement. The durable dispatcher
//! owns admission, replay and the sole transaction; this owner never commits.
use super::*;
use crate::catalog_tag_mutation::TagAuthority;
use catabolic_core::catalog_metadata::{
    ContributionReplacement, plan_replace, plan_schema_extension, revalidate_document,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContributionReplace {
    pub contract_version: u32,
    pub subject: String,
    pub namespace: String,
    pub record_id: String,
    pub expected_schema_fingerprint: Option<String>,
    pub expected_revision: i64,
    pub fields_json: String,
    pub evidence_json: String,
    pub tags: Vec<String>,
    pub excluded_tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_creation_intent: Option<String>,
}

pub(crate) fn replace(
    store: &crate::Store,
    request: &ContributionReplace,
    command: &CommandEnvelope,
    source: &MetadataAuthority,
    initialization: &MetadataAuthority,
    extension: &MetadataAuthority,
    tags: &TagAuthority,
    origin: Option<&VerifiedCreatedOrigin>,
    time: i64,
) -> Result<Value> {
    replace_inner(
        store,
        request,
        command,
        source,
        initialization,
        extension,
        tags,
        origin,
        time,
        None,
        None,
    )
}

/// Private schema41 composition only. The nonserialized NFO permit supplements,
/// never replaces, the ordinary source/schema/tag grants. The outer NFO owner
/// must verify and publish its typed state before the sole transaction commits.
pub(crate) fn replace_nfo(
    store: &crate::Store,
    request: &ContributionReplace,
    command: &CommandEnvelope,
    source: &MetadataAuthority,
    initialization: &MetadataAuthority,
    extension: &MetadataAuthority,
    tags: &TagAuthority,
    time: i64,
    permit: &crate::catalog_nfo_attribution::NfoWritePermit,
) -> Result<Value> {
    let source_effect = permit
        .begin_source_effect_in(store, request, command)
        .map_err(|error| fail(MetadataStoreErrorCode::AuthorityDenied, error.to_string()))?;
    replace_inner(
        store,
        request,
        command,
        source,
        initialization,
        extension,
        tags,
        permit.first_origin(),
        time,
        Some(permit),
        Some(&source_effect),
    )
}

fn replace_inner(
    store: &crate::Store,
    request: &ContributionReplace,
    command: &CommandEnvelope,
    source: &MetadataAuthority,
    initialization: &MetadataAuthority,
    extension: &MetadataAuthority,
    tags: &TagAuthority,
    origin: Option<&VerifiedCreatedOrigin>,
    time: i64,
    nfo_permit: Option<&crate::catalog_nfo_attribution::NfoWritePermit>,
    source_effect: Option<&crate::catalog_nfo_attribution::source_effect::NfoSourceEffect>,
) -> Result<Value> {
    let db = &store.db;
    let admitted_schema = matches!(store.schema_version, 40 | 41)
        && (store.schema_version != 41
            || request.namespace != "motion.nfo"
            || nfo_permit.is_some());
    if !admitted_schema || request.contract_version != 2 || db.is_autocommit() || time < 0 {
        return Err(fail(
            MetadataStoreErrorCode::UnsupportedContract,
            "contribution replacement requires native40 contract2 writer",
        ));
    }
    for grant in [source, initialization, extension] {
        if grant.host_namespace != command.key().host_namespace()
            || grant.subject != request.subject
        {
            return Err(fail(
                MetadataStoreErrorCode::AuthorityDenied,
                "contribution grant subject mismatch",
            ));
        }
    }
    if !matches!(
        initialization.access,
        MetadataAccess::Initialize | MetadataAccess::InitializeV2 { .. }
    ) || !matches!(extension.access, MetadataAccess::ExtendSchema)
    {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "explicit initialization/schema grants required",
        ));
    }
    let kind = match &source.access {
        MetadataAccess::Source {
            namespace,
            record_id,
            kind,
        } if namespace == &request.namespace
            && record_id == &request.record_id
            && *kind != SourceKind::LegacyImport =>
        {
            *kind
        }
        _ => {
            return Err(fail(
                MetadataStoreErrorCode::AuthorityDenied,
                "source grant does not cover replacement",
            ));
        }
    };
    let key = ContributionKey::new(&request.subject, &request.namespace, &request.record_id)?;
    if db.prepare("SELECT 1 FROM catalog_metadata_sources WHERE item_id=? AND namespace=? AND record_id<>? LIMIT 1")?.exists(params![request.subject,request.namespace,request.record_id])? {
        return Err(fail(MetadataStoreErrorCode::InvalidInput, "namespace-scoped tags require one contribution record per source"));
    }

    if nfo_permit.is_none()
        && command.payload()
            != &json!({"operation":"metadata_contribution_replace","request":request})
    {
        return Err(fail(
            MetadataStoreErrorCode::InvalidInput,
            "replacement command payload mismatch",
        ));
    }
    let authority = MetadataReadAuthority {
        catalog_id: command.key().catalog_id().into(),
        incarnation: command.admission_incarnation().into(),
        subject: request.subject.clone(),
        expected_generation: command.expected_generation(),
    };
    check_read_admission(db, &authority)?;
    if db
        .prepare("SELECT 1 FROM catalog_logical_retirements WHERE item_id=?")?
        .exists([&request.subject])?
    {
        return Err(fail(
            MetadataStoreErrorCode::RetiredSubject,
            "retired replacement subject",
        ));
    }
    let current_fingerprint = schema_registry::current_fingerprint(db, &request.subject)?;
    if current_fingerprint != request.expected_schema_fingerprint {
        return Err(MetadataError::SchemaMismatch.into());
    }
    let raw = serde_json::to_string(request)?;
    if raw.len() > MAX_RAW_MUTATION_BYTES {
        return Err(fail(
            MetadataStoreErrorCode::InvalidInput,
            "replacement raw history exceeds budget",
        ));
    }
    let budget = store.catalog_operation_budget()?;
    budget
        .charge(crate::operation_budget::BudgetCharge {
            bytes: raw.len(),
            values: request.tags.len() + request.excluded_tags.len(),
            work: raw.len() / 64 + 1,
            ..Default::default()
        })
        .map_err(|_| {
            fail(
                MetadataStoreErrorCode::StoreFailure,
                "replacement operation budget exhausted",
            )
        })?;
    if current_fingerprint.is_none() {
        if request.expected_revision != 0 {
            return Err(MetadataError::StaleSourceRevision {
                expected: request.expected_revision,
                actual: 0,
            }
            .into());
        }
        let raw: String = db.query_row(
            "SELECT metadata FROM items WHERE id=? AND length(CAST(metadata AS BLOB))<=65536",
            [&request.subject],
            |r| r.get(0),
        )?;
        let existing =
            decode_json(&raw).map_err(|_| corrupt("invalid native metadata baseline"))?;
        let fields = existing
            .as_object()
            .ok_or_else(|| corrupt("native baseline is not an object"))?;
        let policies = fields
            .keys()
            .map(|field| PolicyField {
                field: field.clone(),
                value_type: PolicyValueType::Json,
                nullable: false,
                normalization: PolicyNormalization::Exact,
            })
            .collect();
        let system_fields = match &initialization.access {
            MetadataAccess::InitializeV2 { system_fields } => system_fields.clone(),
            _ => {
                return Err(fail(
                    MetadataStoreErrorCode::AuthorityDenied,
                    "first v2 contribution requires explicit v2 initialization grant",
                ));
            }
        };
        let request = MetadataInitializeV2 {
            contract_version: 2,
            subject: request.subject.clone(),
            policies,
            system_fields,
        };
        let inner = CommandEnvelope::new(
            command.contract_version(),
            command.key().clone(),
            command.admission_incarnation(),
            command.expected_generation(),
            &json!({"operation":"metadata_initialize_v2","request":request}).to_string(),
        )
        .map_err(|e| fail(MetadataStoreErrorCode::InvalidInput, e.to_string()))?;
        if let Some(permit) = nfo_permit {
            super::initialization_v2::initialize_nfo_v2(
                store,
                &request,
                &inner,
                initialization,
                time,
                permit,
                command,
            )?;
        } else {
            initialize_v2(store, &request, &inner, initialization, time)?;
        }
    }
    schema_registry::charge_retained_schemas(db, &budget, &request.subject)?;
    let old_schema = read_schema(db, &request.subject)?;
    let (source_bytes, source_fields): (i64,i64) = db.query_row("SELECT coalesce(sum(length(CAST(fields_json AS BLOB))+length(CAST(evidence_json AS BLOB))),0),(SELECT count(*) FROM catalog_metadata_sources s,json_each(s.fields_json) WHERE s.item_id=?1) FROM catalog_metadata_sources WHERE item_id=?1", [&request.subject], |r|Ok((r.get(0)?,r.get(1)?)))?;
    budget
        .charge(crate::operation_budget::BudgetCharge {
            bytes: source_bytes as usize,
            values: source_fields as usize + old_schema.fields().len(),
            work: source_bytes as usize / 64
                + source_fields as usize
                + old_schema.fields().len()
                + 1,
            ..Default::default()
        })
        .map_err(|_| {
            fail(
                MetadataStoreErrorCode::StoreFailure,
                "retained metadata replacement budget exhausted",
            )
        })?;
    let mut documents = read_documents(db, &old_schema, &request.subject)?;
    let origin_documents = super::origin::plan_origin(
        &old_schema,
        &mut documents,
        &request.subject,
        request.origin_creation_intent.as_deref(),
        command,
        origin,
    )?;
    // Parse under the unchanged bounded JSON parser before selecting policies.
    let values = catabolic_core::catalog_metadata::replacement_fields(&request.fields_json)?;
    if values.keys().any(|field| field.starts_with("motion_")) {
        return Err(MetadataError::ReservedField.into());
    }
    let additions = values
        .keys()
        .filter(|key| !old_schema.fields().contains_key(*key))
        .map(|field| {
            (
                field.clone(),
                FieldPolicy {
                    value_type: ValueType::Json,
                    nullable: false,
                    normalization: Normalization::Exact,
                },
            )
        })
        .collect();
    let system = old_schema
        .fields()
        .iter()
        .filter(|(field, _)| old_schema.system_fields().contains(*field) || matches!(&initialization.access, MetadataAccess::InitializeV2 { system_fields } if system_fields.contains(*field)))
        .map(|(field, p)| (field.clone(), *p))
        .collect();
    let schema = plan_schema_extension(&old_schema, additions, system)?;
    let mut proposed = documents
        .iter()
        .map(|doc| revalidate_document(&schema, &old_schema, doc))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let current = proposed.iter().find(|doc| doc.key() == &key);
    let replacement = ContributionReplacement::from_json(
        &schema,
        key,
        kind,
        request.expected_revision,
        &request.fields_json,
        &request.evidence_json,
    )?;
    let next = plan_replace(&schema, current, &replacement)?;
    proposed.retain(|doc| doc.key() != next.key());
    proposed.push(next.clone());
    let resolution = resolve(&schema, &request.subject, &proposed)?;
    for document in &origin_documents {
        let bytes = serde_json::to_vec(document.fields())?.len()
            + encode(document.evidence()).len()
            + raw.len();
        budget
            .charge(crate::operation_budget::BudgetCharge {
                bytes,
                values: document.fields().len(),
                work: bytes / 64 + document.fields().len() + 1,
                ..Default::default()
            })
            .map_err(|_| {
                fail(
                    MetadataStoreErrorCode::StoreFailure,
                    "origin replacement history budget exhausted",
                )
            })?;
    }
    if let Some(permit) = nfo_permit {
        permit
            .require_retained_barrier_in(store, command)
            .map_err(|error| fail(MetadataStoreErrorCode::AuthorityDenied, error.to_string()))?;
    } else {
        db.execute(
            "INSERT INTO catalog_metadata_permits(item_id,incarnation) VALUES (?,?)",
            params![request.subject, command.admission_incarnation()],
        )?;
    }
    if schema.fingerprint() != old_schema.fingerprint() {
        db.execute("INSERT INTO catalog_metadata_schemas(item_id,schema_fingerprint,contract_version,policies_json,system_fields_json,prior_fingerprint,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us) VALUES (?,?,2,?,?,?,?,?,?,?)",params![request.subject,schema.fingerprint(),serde_json::to_string(&schema_registry::policies(&schema))?,serde_json::to_string(schema.system_fields())?,old_schema.fingerprint(),command.key().catalog_id(),command.key().host_namespace(),command.key().operation_id(),time])?;
        db.execute("UPDATE catalog_metadata_schema_heads SET schema_fingerprint=? WHERE item_id=? AND schema_fingerprint=?",params![schema.fingerprint(),request.subject,old_schema.fingerprint()])?;
    }
    for document in &origin_documents {
        let document = revalidate_document(&schema, &old_schema, document)?;
        write_revision(db, &document, &raw, command, time)?;
    }
    write_revision(db, &next, &raw, command, time)?;
    let tag_state = crate::tagging::replace_source_authorized(
        store,
        command,
        tags,
        &request.subject,
        &request.namespace,
        &request.record_id,
        next.revision(),
        &request.tags,
        &request.excluded_tags,
        time,
        source_effect,
    )?;
    write_projection(db, &resolution)?;
    if nfo_permit.is_none() {
        db.execute(
            "DELETE FROM catalog_metadata_permits WHERE item_id=?",
            [&request.subject],
        )?;
    }
    // Schema41 NFO composition deliberately retains the existing deferred
    // commit-barrier permit. Only verified typed-state publication clears it;
    // dropping the Rust token or swallowing a later error cannot commit just
    // the metadata half of the operation.
    Ok(
        json!({"operation":"metadata_contribution_replace","subject":request.subject,"schema_fingerprint":schema.fingerprint(),"source":{"namespace":request.namespace,"record_id":request.record_id,"revision":next.revision()},"tags":tag_state,"resolution":resolution}),
    )
}
