//! Exact trusted system-source replacement. Public contribution callers cannot
//! obtain SourceV2 system grants; no field name or request marker grants access.
use super::*;
use catabolic_core::catalog_metadata::{
    MAX_METADATA_JSON_DEPTH, plan_schema_extension, revalidate_document,
};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataSystemReplace {
    pub contract_version: u32,
    pub subject: String,
    pub namespace: String,
    pub record_id: String,
    pub expected_schema_fingerprint: Option<String>,
    pub expected_revision: i64,
    pub fields_json: String,
    pub evidence_json: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_creation_intent: Option<String>,
}

pub(crate) fn replace_system(
    store: &crate::Store,
    request: &MetadataSystemReplace,
    command: &CommandEnvelope,
    source: &MetadataAuthority,
    initialization: &MetadataAuthority,
    extension: &MetadataAuthority,
    origin: Option<&VerifiedCreatedOrigin>,
    time: i64,
) -> Result<Value> {
    let db = &store.db;
    if !matches!(store.schema_version, 40 | 41)
        || request.contract_version != 1
        || db.is_autocommit()
        || time < 0
    {
        return Err(fail(
            MetadataStoreErrorCode::UnsupportedContract,
            "system replacement requires native40 or managed native41 writer",
        ));
    }
    for grant in [source, initialization, extension] {
        if grant.subject != request.subject
            || grant.host_namespace != command.key().host_namespace()
        {
            return Err(fail(
                MetadataStoreErrorCode::AuthorityDenied,
                "system replacement grant identity mismatch",
            ));
        }
    }
    let (kind, allowed) = match &source.access {
        MetadataAccess::SourceV2 {
            namespace,
            record_id,
            kind,
            system_fields,
        } if namespace == &request.namespace
            && record_id == &request.record_id
            && *kind != SourceKind::LegacyImport
            && !system_fields.is_empty() =>
        {
            (*kind, system_fields)
        }
        _ => {
            return Err(fail(
                MetadataStoreErrorCode::AuthorityDenied,
                "explicit system-source grant required",
            ));
        }
    };
    if !matches!(initialization.access, MetadataAccess::InitializeV2 { .. })
        || !matches!(extension.access, MetadataAccess::ExtendSchema)
    {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "explicit initialization/schema grants required",
        ));
    }
    if command.payload() != &json!({"operation":"metadata_system_replace","request":request}) {
        return Err(fail(
            MetadataStoreErrorCode::InvalidInput,
            "system replacement payload mismatch",
        ));
    }
    let raw = serde_json::to_string(request)?;
    if raw.len() > MAX_RAW_MUTATION_BYTES || request.fields_json.len() > MAX_METADATA_DOCUMENT_BYTES
    {
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
                    "system replacement budget exhausted",
                )
            })
    };
    charge(raw.len(), allowed.len())?;
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
            "retired system replacement subject",
        ));
    }
    let fingerprint = schema_registry::current_fingerprint(db, &request.subject)?;
    if fingerprint != request.expected_schema_fingerprint {
        return Err(MetadataError::SchemaMismatch.into());
    }
    if store.schema_version == 41
        && (fingerprint.is_none()
            || request.namespace == "motion.nfo"
            || allowed.contains("motion_catalog")
            || request.origin_creation_intent.is_some()
            || origin.is_some())
    {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "native41 system replacement requires managed non-NFO source without identity projection or origin effects",
        ));
    }
    let fields = catabolic_core::catalog_metadata::replacement_fields(&request.fields_json)?;
    if fields.keys().any(|field| !allowed.contains(field)) {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "system replacement exceeds exact field grant",
        ));
    }
    if fingerprint.is_none() {
        if request.expected_revision != 0 {
            return Err(MetadataError::StaleSourceRevision {
                expected: request.expected_revision,
                actual: 0,
            }
            .into());
        }
        let baseline: String = db.query_row(
            "SELECT metadata FROM items WHERE id=? AND length(CAST(metadata AS BLOB))<=65536",
            [&request.subject],
            |r| r.get(0),
        )?;
        charge(baseline.len(), 1)?;
        let value =
            decode_json(&baseline).map_err(|_| corrupt("invalid system-source baseline"))?;
        let policies = value
            .as_object()
            .ok_or_else(|| corrupt("system-source baseline is not object"))?
            .keys()
            .map(|field| PolicyField {
                field: field.clone(),
                value_type: PolicyValueType::Json,
                nullable: false,
                normalization: PolicyNormalization::Exact,
            })
            .collect();
        let MetadataAccess::InitializeV2 { system_fields } = &initialization.access else {
            unreachable!()
        };
        let initialize = MetadataInitializeV2 {
            contract_version: 2,
            subject: request.subject.clone(),
            policies,
            system_fields: system_fields.clone(),
        };
        let inner = CommandEnvelope::new(
            command.contract_version(),
            command.key().clone(),
            command.admission_incarnation(),
            command.expected_generation(),
            &json!({"operation":"metadata_initialize_v2","request":initialize}).to_string(),
        )
        .map_err(|e| fail(MetadataStoreErrorCode::InvalidInput, e.to_string()))?;
        initialize_v2(store, &initialize, &inner, initialization, time)?;
    }
    schema_registry::charge_retained_schemas(db, &budget, &request.subject)?;
    let old = read_schema(db, &request.subject)?;
    let (source_bytes, source_fields): (i64, i64) = db.query_row(
        "SELECT coalesce(sum(length(CAST(fields_json AS BLOB))+length(CAST(evidence_json AS BLOB))),0),(SELECT count(*) FROM catalog_metadata_sources s,json_each(s.fields_json) WHERE s.item_id=?1) FROM catalog_metadata_sources WHERE item_id=?1",
        [&request.subject], |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    charge(
        source_bytes as usize,
        source_fields as usize + old.fields().len(),
    )?;
    let mut documents = read_documents(db, &old, &request.subject)?;
    let key = if store.schema_version == 41 {
        Some(ContributionKey::new(
            &request.subject,
            &request.namespace,
            &request.record_id,
        )?)
    } else {
        None
    };
    let mut retained = if let Some(key) = key.as_ref() {
        if old.fields().contains_key("motion_catalog")
            && !old.system_fields().contains("motion_catalog")
        {
            return Err(fail(
                MetadataStoreErrorCode::AuthorityDenied,
                "unclassified identity marker requires exact protected-classification composition before system replacement",
            ));
        }
        let current = documents.iter().find(|document| document.key() == key);
        if fields.contains_key("motion_catalog")
            || current.is_some_and(|document| document.fields().contains_key("motion_catalog"))
        {
            return Err(fail(
                MetadataStoreErrorCode::AuthorityDenied,
                "identity-bearing system source requires the exact compound owner",
            ));
        }
        let MetadataAccess::InitializeV2 { system_fields } = &initialization.access else {
            unreachable!()
        };
        if system_fields.iter().any(|field| {
            !old.system_fields().contains(field)
                && (!allowed.contains(field)
                    || !(fields.contains_key(field)
                        || current.is_some_and(|document| document.fields().contains_key(field))))
        }) || allowed.iter().any(|field| {
            !old.system_fields().contains(field)
                && !(fields.contains_key(field)
                    || current.is_some_and(|document| document.fields().contains_key(field)))
        }) {
            return Err(fail(
                MetadataStoreErrorCode::AuthorityDenied,
                "native41 classification must be limited to the independently granted replacement fields",
            ));
        }
        let mut proof = crate::catalog_nfo_attribution::ProofBudget::new(budget.clone());
        let digest = retained_digest(db, key, &mut proof)?;
        Some((proof, digest))
    } else {
        None
    };
    let origin_documents = super::origin::plan_origin(
        &old,
        &mut documents,
        &request.subject,
        request.origin_creation_intent.as_deref(),
        command,
        origin,
    )?;
    let key = match key {
        Some(key) => key,
        None => ContributionKey::new(&request.subject, &request.namespace, &request.record_id)?,
    };
    for document in &documents {
        if document.key() != &key
            && document
                .fields()
                .keys()
                .any(|field| allowed.contains(field) && !old.system_fields().contains(field))
        {
            return Err(fail(
                MetadataStoreErrorCode::AuthorityDenied,
                "cannot reclassify another source's public field as system ownership",
            ));
        }
    }
    let mut system: BTreeMap<String, FieldPolicy> = old
        .fields()
        .iter()
        .filter(|(field, _)| old.system_fields().contains(*field))
        .map(|(field, policy)| (field.clone(), *policy))
        .collect();
    // This non-serialized grant is the host's explicit classification decision,
    // not an inference from a field name or a contributor's source kind. The
    // host must establish ownership before minting it. A v1 transition changes
    // only classification: retain each exact policy and every source document.
    // An already-v2 external policy must never be promoted by a later grant.
    let MetadataAccess::InitializeV2 { system_fields } = &initialization.access else {
        unreachable!()
    };
    for field in system_fields {
        let policy = old.fields().get(field).ok_or(MetadataError::UnknownField)?;
        if old.contract_version() != 1 && !old.system_fields().contains(field) {
            return Err(MetadataError::SchemaMismatch.into());
        }
        system.insert(field.clone(), *policy);
    }
    for field in allowed {
        system.insert(
            field.clone(),
            old.fields().get(field).copied().unwrap_or(FieldPolicy {
                value_type: ValueType::Json,
                nullable: false,
                normalization: Normalization::Exact,
            }),
        );
    }
    let schema = plan_schema_extension(&old, Vec::new(), system.into_iter().collect())?;
    let mut proposed = documents
        .iter()
        .map(|document| revalidate_document(&schema, &old, document))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let current = proposed.iter().find(|document| document.key() == &key);
    if current.is_some_and(|document| {
        document
            .fields()
            .keys()
            .any(|field| !allowed.contains(field))
    }) {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "system replacement cannot discard ungranted source fields",
        ));
    }
    let mut edits: BTreeMap<String, Value> = fields
        .into_iter()
        .map(|(field, value)| (field, json!({"op":"set","value":value})))
        .collect();
    for field in current
        .into_iter()
        .flat_map(|document| document.fields().keys())
    {
        edits
            .entry(field.clone())
            .or_insert_with(|| json!({"op":"remove"}));
    }
    let patch = ContributionPatch::from_json_with_system_fields(
        &schema,
        key,
        kind,
        request.expected_revision,
        &serde_json::to_string(&edits)?,
        &request.evidence_json,
    )?;
    let next = plan_update(&schema, current, &patch)?;
    proposed.retain(|document| document.key() != next.key());
    proposed.push(next.clone());
    let resolution = resolve(&schema, &request.subject, &proposed)?;
    charge(
        serde_json::to_vec(&resolution)?.len() + raw.len(),
        schema.fields().len(),
    )?;
    db.execute(
        "INSERT INTO catalog_metadata_permits(item_id,incarnation) VALUES (?,?)",
        params![request.subject, command.admission_incarnation()],
    )?;
    if schema.fingerprint() != old.fingerprint() {
        db.execute("INSERT INTO catalog_metadata_schemas(item_id,schema_fingerprint,contract_version,policies_json,system_fields_json,prior_fingerprint,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us) VALUES (?,?,2,?,?,?,?,?,?,?)",params![request.subject,schema.fingerprint(),serde_json::to_string(&schema_registry::policies(&schema))?,serde_json::to_string(schema.system_fields())?,old.fingerprint(),command.key().catalog_id(),command.key().host_namespace(),command.key().operation_id(),time])?;
        db.execute("UPDATE catalog_metadata_schema_heads SET schema_fingerprint=? WHERE item_id=? AND schema_fingerprint=?",params![schema.fingerprint(),request.subject,old.fingerprint()])?;
    }
    for document in &origin_documents {
        charge(
            serde_json::to_vec(document.fields())?.len()
                + encode(document.evidence()).len()
                + raw.len(),
            document.fields().len(),
        )?;
        let document = revalidate_document(&schema, &old, document)?;
        write_revision(db, &document, &raw, command, time)?;
    }
    write_revision(db, &next, &raw, command, time)?;
    write_projection(db, &resolution)?;
    if let Some((proof, before)) = retained.as_mut() {
        schema_registry::charge_retained_schemas(db, &budget, &request.subject)?;
        let actual_schema = read_schema(db, &request.subject)?;
        if actual_schema != schema || retained_digest(db, next.key(), proof)? != *before {
            return Err(corrupt(
                "system replacement changed retained NFO/source closure or classification",
            ));
        }
        proof.serialized(next.fields(), 64)?;
        proof.canonical_serialized(next.evidence(), MAX_METADATA_JSON_DEPTH)?;
        let fields = serde_json::to_string(next.fields())?;
        let evidence = encode(next.evidence());
        let exact: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM catalog_metadata_sources s JOIN catalog_metadata_history h ON h.item_id=s.item_id AND h.namespace=s.namespace AND h.record_id=s.record_id AND h.revision=s.revision WHERE s.item_id=?1 AND s.namespace=?2 AND s.record_id=?3 AND s.revision=?4 AND s.source_kind=?5 AND s.schema_fingerprint=?6 AND s.fields_json=?7 AND s.evidence_json=?8 AND h.source_kind=s.source_kind AND h.schema_fingerprint=s.schema_fingerprint AND h.fields_json=s.fields_json AND h.evidence_json=s.evidence_json AND h.raw_mutation_json=?9 AND h.command_catalog_id=?10 AND h.command_host_namespace=?11 AND h.command_operation_id=?12 AND h.admitted_at_us=?13)",
            params![request.subject,request.namespace,request.record_id,next.revision(),kind_sql(next.kind()),schema.fingerprint(),fields,evidence,raw,command.key().catalog_id(),command.key().host_namespace(),command.key().operation_id(),time],|r|r.get(0))?;
        if !exact {
            return Err(corrupt(
                "system replacement terminal source/history differs",
            ));
        }
    }
    db.execute(
        "DELETE FROM catalog_metadata_permits WHERE item_id=?",
        [&request.subject],
    )?;
    Ok(
        json!({"operation":"metadata_system_replace","subject":request.subject,"schema_fingerprint":schema.fingerprint(),"source":{"namespace":request.namespace,"record_id":request.record_id,"revision":next.revision()},"resolution":resolution}),
    )
}

/// Hash exact retained current bytes under cumulative admission, before copying
/// them. This owner never writes NFO history; immutable historical rows remain
/// protected by their existing triggers, without scanning unbounded lifetimes.
fn retained_digest(
    db: &Connection,
    key: &ContributionKey,
    budget: &mut crate::catalog_nfo_attribution::ProofBudget,
) -> Result<String> {
    struct Snapshot<'a> {
        db: &'a Connection,
        key: &'a ContributionKey,
    }
    struct Rows<'a> {
        db: &'a Connection,
        key: &'a ContributionKey,
        sql: &'static str,
        other_sources: bool,
    }
    struct Row<'a, 's>(&'a rusqlite::Row<'s>);
    struct Cell<'a>(rusqlite::types::ValueRef<'a>);
    impl Serialize for Cell<'_> {
        fn serialize<S: serde::Serializer>(
            &self,
            serializer: S,
        ) -> std::result::Result<S::Ok, S::Error> {
            use rusqlite::types::ValueRef;
            match self.0 {
                ValueRef::Null => serializer.serialize_none(),
                ValueRef::Integer(value) => serializer.serialize_i64(value),
                ValueRef::Real(value) => serializer.serialize_f64(value),
                ValueRef::Text(value) => serializer
                    .serialize_str(std::str::from_utf8(value).map_err(serde::ser::Error::custom)?),
                ValueRef::Blob(value) => serializer.serialize_bytes(value),
            }
        }
    }
    impl Serialize for Row<'_, '_> {
        fn serialize<S: serde::Serializer>(
            &self,
            serializer: S,
        ) -> std::result::Result<S::Ok, S::Error> {
            use serde::ser::SerializeSeq;
            let mut seq = serializer.serialize_seq(Some(self.0.as_ref().column_count()))?;
            for column in 0..self.0.as_ref().column_count() {
                seq.serialize_element(&Cell(
                    self.0.get_ref(column).map_err(serde::ser::Error::custom)?,
                ))?;
            }
            seq.end()
        }
    }
    impl Serialize for Rows<'_> {
        fn serialize<S: serde::Serializer>(
            &self,
            serializer: S,
        ) -> std::result::Result<S::Ok, S::Error> {
            use serde::ser::SerializeSeq;
            let mut query = self
                .db
                .prepare(self.sql)
                .map_err(serde::ser::Error::custom)?;
            let mut rows = if self.other_sources {
                query.query(params![
                    self.key.subject(),
                    self.key.namespace(),
                    self.key.record_id()
                ])
            } else {
                query.query([self.key.subject()])
            }
            .map_err(serde::ser::Error::custom)?;
            let mut seq = serializer.serialize_seq(None)?;
            while let Some(row) = rows.next().map_err(serde::ser::Error::custom)? {
                seq.serialize_element(&Row(row))?;
            }
            seq.end()
        }
    }
    impl Serialize for Snapshot<'_> {
        fn serialize<S: serde::Serializer>(
            &self,
            serializer: S,
        ) -> std::result::Result<S::Ok, S::Error> {
            use serde::ser::SerializeSeq;
            const QUERIES: [&str; 6] = [
                "SELECT * FROM catalog_metadata_sources WHERE item_id=?1 AND NOT(namespace=?2 AND record_id=?3) ORDER BY namespace,record_id",
                "SELECT * FROM catalog_nfo_states WHERE item_id=?1",
                "SELECT h.* FROM catalog_nfo_states s JOIN catalog_nfo_state_revisions h ON h.item_id=s.item_id AND h.revision=s.current_revision WHERE s.item_id=?1",
                "SELECT * FROM catalog_nfo_identity_claims WHERE item_id=?1 ORDER BY value",
                "SELECT * FROM catalog_nfo_file_attributions WHERE item_id=?1",
                "SELECT * FROM catalog_identity_pins WHERE item_id=?1 AND namespace='nfo'",
            ];
            let mut seq = serializer.serialize_seq(Some(QUERIES.len()))?;
            for (index, sql) in QUERIES.into_iter().enumerate() {
                seq.serialize_element(&Rows {
                    db: self.db,
                    key: self.key,
                    sql,
                    other_sources: index == 0,
                })?;
            }
            seq.end()
        }
    }
    budget
        .serialized(&Snapshot { db, key }, 64)
        .map_err(Into::into)
}
