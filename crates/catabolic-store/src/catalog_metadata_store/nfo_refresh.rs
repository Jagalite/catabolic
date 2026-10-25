//! Private schema41 refresh metadata plans. Ordinary40 entrypoints are unchanged.
//! Planning never writes; the refresh batch owns deferred barriers and the sole
//! receipt. These typed values cannot be deserialized from a caller payload.
pub(super) mod classification;
use super::*;
use catabolic_core::catalog_metadata::{
    ContributionReplacement, plan_replace, plan_schema_extension,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub(crate) struct RefreshMetadataSnapshot {
    pub(crate) schema: FieldSchema,
    pub(crate) documents: Vec<ContributionDocument>,
    pub(super) classification: Option<std::sync::Arc<classification::ClassificationProof>>,
}
impl RefreshMetadataSnapshot {
    /// Exact captured lineage binding, never a caller-created authority marker.
    pub(crate) fn classification_digest(&self) -> Option<&str> {
        self.classification.as_deref().map(|proof| proof.digest())
    }
}
#[derive(Debug)]
pub(crate) struct RefreshSourcePlan {
    pub(crate) before: RefreshMetadataSnapshot,
    pub(crate) after: RefreshMetadataSnapshot,
    pub(crate) source: ContributionDocument,
    pub(crate) resolution: MetadataResolution,
    pub(crate) request: ContributionReplace,
    pub(crate) raw_request: String,
    pub(crate) classification_fields: BTreeSet<String>,
    pub(crate) structural: Vec<ContributionDocument>,
    pub(crate) first: Option<super::nfo_refresh_initialization::RefreshFirstMetadataPlan>,
    /// Ordered intermediate revisions, never deduplicated by source key.
    pub(crate) origin: Vec<ContributionDocument>,
}
fn budget_error(error: impl std::fmt::Display) -> MetadataStoreError {
    fail(
        MetadataStoreErrorCode::StoreFailure,
        format!("refresh metadata budget: {error}"),
    )
}

fn evaluation_budget_error(error: crate::selection::EvaluationFailure) -> MetadataStoreError {
    budget_error(format!("{error:?}"))
}

pub(crate) fn read_refresh_metadata(
    store: &crate::Store,
    subject: &str,
) -> Result<RefreshMetadataSnapshot> {
    if store.schema_version != 41 || store.db.is_autocommit() {
        return Err(fail(
            MetadataStoreErrorCode::UnsupportedContract,
            "private refresh metadata requires schema41 retained writer",
        ));
    }
    let budget = store.catalog_operation_budget().map_err(budget_error)?;
    schema_registry::charge_retained_schemas(&store.db, &budget, subject)?;
    let schema = read_schema(&store.db, subject)?;
    // Charge before read_documents allocates retained values. SQL aggregates
    // are decoded as signed SQLite integers and converted without truncation.
    let (bytes, fields, count): (i64,i64,i64) = store.db.query_row(
        "SELECT coalesce(sum(length(CAST(fields_json AS BLOB))+length(CAST(evidence_json AS BLOB))),0),(SELECT count(*) FROM catalog_metadata_sources s,json_each(s.fields_json) WHERE s.item_id=?1),count(*) FROM catalog_metadata_sources WHERE item_id=?1",
        [subject], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let bytes = usize::try_from(bytes).map_err(budget_error)?;
    let fields = usize::try_from(fields).map_err(budget_error)?;
    let count = usize::try_from(count).map_err(budget_error)?;
    if bytes > MAX_METADATA_SNAPSHOT_BYTES || count > MAX_METADATA_DOCUMENTS {
        return Err(MetadataError::SnapshotBudget.into());
    }
    budget
        .charge(crate::operation_budget::BudgetCharge {
            bytes,
            values: fields
                .checked_add(count)
                .ok_or(MetadataError::SnapshotBudget)?,
            work: bytes / 64 + fields + count + 1,
            ..Default::default()
        })
        .map_err(evaluation_budget_error)?;
    let documents = read_documents(&store.db, &schema, subject)?;
    let mut json_budget = crate::catalog_nfo_attribution::ProofBudget::new(
        store.catalog_operation_budget().map_err(budget_error)?,
    );
    json_budget
        .serialized(&documents, 64)
        .map_err(budget_error)?;
    // Managed projection must still correspond to these exact sources. A
    // generic item write cannot become current metadata ownership authority.
    let resolution = resolve(&schema, subject, &documents)?;
    json_budget
        .serialized(&resolution, 64)
        .map_err(budget_error)?;
    let effective = Value::Object(
        resolution
            .fields
            .iter()
            .filter_map(|(key, field)| {
                if let FieldResolution::Resolved { value, .. } = field {
                    Some((key.clone(), value.clone()))
                } else {
                    None
                }
            })
            .collect(),
    );
    let mut query = store.db.prepare("SELECT p.effective_json,i.metadata FROM catalog_metadata_projection p JOIN items i ON i.id=p.item_id WHERE p.item_id=? AND p.schema_fingerprint=?")?;
    let mut rows = query.query(params![subject, schema.fingerprint()])?;
    let row = rows
        .next()?
        .ok_or_else(|| corrupt("managed refresh projection absent"))?;
    for index in 0..2 {
        let rusqlite::types::ValueRef::Text(bytes) = row.get_ref(index)? else {
            return Err(corrupt("managed refresh projection is not text"));
        };
        if bytes.len() > MAX_METADATA_SNAPSHOT_BYTES {
            return Err(MetadataError::SnapshotBudget.into());
        }
        budget
            .charge(crate::operation_budget::BudgetCharge {
                bytes: bytes.len(),
                values: 1,
                work: bytes.len() / 64 + 1,
                ..Default::default()
            })
            .map_err(evaluation_budget_error)?;
        let raw = std::str::from_utf8(bytes).map_err(|_| corrupt("invalid projection UTF-8"))?;
        json_budget
            .raw_json(
                raw,
                catabolic_core::catalog_metadata::MAX_METADATA_JSON_DEPTH,
            )
            .map_err(budget_error)?;
        if decode_json(raw).map_err(|_| corrupt("invalid managed projection JSON"))? != effective {
            return Err(corrupt("refresh source/projection/item ownership diverged"));
        }
    }
    let classification = if schema.fields().contains_key("motion_catalog")
        && !schema.system_fields().contains("motion_catalog")
    {
        classification::capture_in(&store.db, subject, &schema)?
    } else {
        None
    };
    Ok(RefreshMetadataSnapshot {
        schema,
        documents,
        classification,
    })
}

/// Normalize without advancing any revision. In particular an identical
/// refresh at i64::MAX must not fail merely because an explicit PUT would.
pub(crate) fn normalize_refresh_fields(
    snapshot: &RefreshMetadataSnapshot,
    subject: &str,
    fields_json: &str,
) -> Result<BTreeMap<String, Value>> {
    let schema = extended_schema(snapshot, fields_json, None)?;
    let replacement = ContributionReplacement::from_json(
        &schema,
        ContributionKey::new(subject, "motion.nfo", "document")?,
        SourceKind::Provider,
        0,
        fields_json,
        "{}",
    )?;
    Ok(replacement.fields().clone())
}
/// Only the already-owned Motion structural field may gain classification on
/// a historical v1 schema. This predicate grants no source-write authority.
pub(super) fn valid_classification(
    snapshot: &RefreshMetadataSnapshot,
    requested: &BTreeSet<String>,
) -> bool {
    let old = &snapshot.schema;
    requested.iter().all(|field| {
        old.system_fields().contains(field)
            || (field == "motion_catalog"
                && snapshot
                    .classification
                    .as_ref()
                    .is_some_and(|proof| proof.permits(old)))
    })
}

fn extended_schema(
    snapshot: &RefreshMetadataSnapshot,
    fields_json: &str,
    classification: Option<&BTreeSet<String>>,
) -> Result<FieldSchema> {
    let old = &snapshot.schema;
    let fields = catabolic_core::catalog_metadata::replacement_fields(fields_json)?;
    if fields.keys().any(|key| key.starts_with("motion_")) {
        return Err(MetadataError::ReservedField.into());
    }
    let additions: Vec<(String, FieldPolicy)> = fields
        .keys()
        .filter(|key| !old.fields().contains_key(*key))
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
    let system: Vec<(String, FieldPolicy)> = old
        .fields()
        .iter()
        .filter(|(key, _)| {
            old.system_fields().contains(*key)
                || classification.is_some_and(|fields| fields.contains(*key))
        })
        .map(|(key, policy)| (key.clone(), *policy))
        .collect();
    if old.contract_version() == 2
        && classification.is_some_and(|fields| fields.contains("motion_catalog"))
        && !old.system_fields().contains("motion_catalog")
    {
        let proof = snapshot.classification.as_deref().ok_or_else(|| {
            fail(
                MetadataStoreErrorCode::AuthorityDenied,
                "exact original-v1 classification ancestry absent",
            )
        })?;
        let external = old
            .fields()
            .iter()
            .filter(|(field, _)| {
                !old.system_fields().contains(*field) && field.as_str() != "motion_catalog"
            })
            .map(|(field, policy)| (field.clone(), *policy))
            .chain(additions)
            .collect();
        let next = FieldSchema::new_v2(external, system)?;
        classification::validate_planned(proof, old, &next)?;
        Ok(next)
    } else {
        Ok(plan_schema_extension(old, additions, system)?)
    }
}

pub(crate) fn plan_refresh_source(
    snapshot: &RefreshMetadataSnapshot,
    request: &ContributionReplace,
    command: &CommandEnvelope,
    prior_identity: Option<&str>,
    pin: Option<&str>,
    next_identity: Option<&str>,
    structural_grants: &super::nfo_refresh_projection::ProjectionGrants<'_>,
    initialization: &MetadataAuthority,
) -> Result<RefreshSourcePlan> {
    plan_source(
        snapshot,
        None,
        request,
        command,
        prior_identity,
        pin,
        next_identity,
        structural_grants,
        initialization,
    )
}

/// First-source planning is descriptive. Only a separately attested batch step
/// permit can apply this plan; an absent source in a snapshot supplies no grant.
pub(crate) fn plan_first_refresh_source(
    first: &super::nfo_refresh_initialization::RefreshFirstMetadataPlan,
    request: &ContributionReplace,
    command: &CommandEnvelope,
    prior_identity: Option<&str>,
    pin: Option<&str>,
    next_identity: Option<&str>,
    structural_grants: &super::nfo_refresh_projection::ProjectionGrants<'_>,
    initialization: &MetadataAuthority,
) -> Result<RefreshSourcePlan> {
    if request.expected_revision != 0 || prior_identity.is_some() {
        return Err(fail(
            MetadataStoreErrorCode::InvalidInput,
            "first refresh source is not revision zero without prior identity",
        ));
    }
    plan_source(
        first.snapshot(),
        Some(first),
        request,
        command,
        prior_identity,
        pin,
        next_identity,
        structural_grants,
        initialization,
    )
}

fn plan_source(
    snapshot: &RefreshMetadataSnapshot,
    first: Option<&super::nfo_refresh_initialization::RefreshFirstMetadataPlan>,
    request: &ContributionReplace,
    command: &CommandEnvelope,
    prior_identity: Option<&str>,
    pin: Option<&str>,
    next_identity: Option<&str>,
    structural_grants: &super::nfo_refresh_projection::ProjectionGrants<'_>,
    initialization: &MetadataAuthority,
) -> Result<RefreshSourcePlan> {
    let expected_fingerprint = match first {
        Some(first) => first.expected_schema_fingerprint(),
        None => Some(snapshot.schema.fingerprint()),
    };
    let expected_intent = first.and_then(|first| first.origin_creation_intent());
    if request.contract_version != 2
        || request.namespace != "motion.nfo"
        || request.record_id != "document"
        || !request.excluded_tags.is_empty()
        || request.origin_creation_intent.as_deref() != expected_intent
        || request.expected_schema_fingerprint.as_deref() != expected_fingerprint
    {
        return Err(fail(
            MetadataStoreErrorCode::InvalidInput,
            "refresh source plan does not match exact NFO predecessor",
        ));
    }
    let raw_request = serde_json::to_string(request)?;
    if raw_request.len() > MAX_RAW_MUTATION_BYTES {
        return Err(MetadataError::DocumentBudget.into());
    }
    let MetadataAccess::InitializeV2 { system_fields } = &initialization.access else {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "explicit refresh classification grant required",
        ));
    };
    if initialization.subject != request.subject
        || initialization.host_namespace != command.key().host_namespace()
        || !valid_classification(snapshot, system_fields)
    {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "refresh classification scope differs",
        ));
    }
    if !system_fields.is_subset(snapshot.schema.system_fields()) {
        super::nfo_refresh_projection::require_classification(
            &snapshot.documents,
            command,
            &request.subject,
            structural_grants,
        )?;
    }
    // Only exact original-v1 ancestry permits delayed marker classification.
    // Current per-contributor grants remain independent of that policy proof.
    let schema = extended_schema(snapshot, &request.fields_json, Some(system_fields))?;
    let mut documents = snapshot
        .documents
        .iter()
        .map(|document| {
            classification::revalidate_planned(
                &schema,
                &snapshot.schema,
                document,
                snapshot.classification.as_deref(),
            )
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let key = ContributionKey::new(&request.subject, &request.namespace, &request.record_id)?;
    let current = documents.iter().find(|document| document.key() == &key);
    if first.is_some() {
        if documents
            .iter()
            .any(|document| document.key().namespace() == "motion.nfo")
        {
            return Err(corrupt(
                "first refresh predecessor already has an NFO source",
            ));
        }
    } else if current.is_none() {
        return Err(corrupt(
            "refresh source requires independently proven existing baseline",
        ));
    }
    let replacement = ContributionReplacement::from_json(
        &schema,
        key,
        SourceKind::Provider,
        request.expected_revision,
        &request.fields_json,
        &request.evidence_json,
    )?;
    let source = plan_replace(&schema, current, &replacement)?;
    documents.retain(|document| document.key() != source.key());
    documents.push(source.clone());
    let structural = super::nfo_refresh_projection::plan(
        &schema,
        &documents,
        command,
        &request.subject,
        prior_identity,
        pin,
        next_identity,
        structural_grants,
    )?;
    for document in &structural {
        documents.retain(|current| current.key() != document.key());
        documents.push(document.clone());
    }
    documents.sort_by(|a, b| a.key().cmp(b.key()));
    let resolution = resolve(&schema, &request.subject, &documents)?;
    let origin = first
        .map(|first| first.origin())
        .unwrap_or_default()
        .iter()
        .map(|document| {
            classification::revalidate_planned(
                &schema,
                &snapshot.schema,
                document,
                snapshot.classification.as_deref(),
            )
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(RefreshSourcePlan {
        before: first
            .map(|first| first.before())
            .unwrap_or(snapshot)
            .clone(),
        after: RefreshMetadataSnapshot {
            schema,
            documents,
            classification: None,
        },
        source,
        resolution,
        request: request.clone(),
        raw_request,
        classification_fields: system_fields.clone(),
        structural,
        first: first.cloned(),
        origin,
    })
}

/// Apply only a batch-owner-issued step permit. This keeps the deferred barrier
/// installed even if the caller swallows a later tag/projection/publication
/// error. The receipt owner, not this helper, commits the transaction.
pub(crate) fn apply_refresh_source(
    store: &crate::Store,
    command: &CommandEnvelope,
    plan: &RefreshSourcePlan,
    source: &MetadataAuthority,
    initialization: &MetadataAuthority,
    extension: &MetadataAuthority,
    tags: &crate::catalog_tag_mutation::TagAuthority,
    permit: &crate::catalog_nfo_attribution::refresh::RefreshStepPermit,
    admitted_at_us: i64,
) -> Result<()> {
    apply_source_inner(
        store,
        command,
        plan,
        source,
        initialization,
        extension,
        tags,
        permit,
        admitted_at_us,
        #[cfg(test)]
        None,
    )
}

#[cfg(test)]
pub(crate) fn apply_refresh_source_with_probe(
    store: &crate::Store,
    command: &CommandEnvelope,
    plan: &RefreshSourcePlan,
    source: &MetadataAuthority,
    initialization: &MetadataAuthority,
    extension: &MetadataAuthority,
    tags: &crate::catalog_tag_mutation::TagAuthority,
    permit: &crate::catalog_nfo_attribution::refresh::RefreshStepPermit,
    admitted_at_us: i64,
    probe: &mut dyn FnMut(&crate::Store) -> crate::Result<()>,
) -> Result<()> {
    apply_source_inner(
        store,
        command,
        plan,
        source,
        initialization,
        extension,
        tags,
        permit,
        admitted_at_us,
        Some(probe),
    )
}

fn apply_source_inner(
    store: &crate::Store,
    command: &CommandEnvelope,
    plan: &RefreshSourcePlan,
    source: &MetadataAuthority,
    initialization: &MetadataAuthority,
    extension: &MetadataAuthority,
    tags: &crate::catalog_tag_mutation::TagAuthority,
    permit: &crate::catalog_nfo_attribution::refresh::RefreshStepPermit,
    admitted_at_us: i64,
    #[cfg(test)] mut probe: Option<&mut dyn FnMut(&crate::Store) -> crate::Result<()>>,
) -> Result<()> {
    let source_effect = permit
        .begin_source_effect_in(store, command, plan)
        .map_err(|error| fail(MetadataStoreErrorCode::AuthorityDenied, error.to_string()))?;
    if store.schema_version != 41 || store.db.is_autocommit() || admitted_at_us < 0 {
        return Err(fail(
            MetadataStoreErrorCode::UnsupportedContract,
            "refresh source effect requires schema41 retained writer",
        ));
    }
    let request = &plan.request;
    for grant in [source, initialization, extension] {
        if grant.host_namespace != command.key().host_namespace()
            || grant.subject != request.subject
        {
            return Err(fail(
                MetadataStoreErrorCode::AuthorityDenied,
                "refresh source grant identity differs",
            ));
        }
    }
    if !matches!(&source.access,MetadataAccess::Source {namespace,record_id,kind}
        if namespace=="motion.nfo" && record_id=="document" && *kind==SourceKind::Provider)
        || !matches!(&initialization.access,MetadataAccess::InitializeV2{system_fields}
            if system_fields == &plan.classification_fields
                && valid_classification(&plan.before, system_fields)
                && system_fields.is_subset(plan.after.schema.system_fields()))
        || !matches!(extension.access, MetadataAccess::ExtendSchema)
    {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "independent exact refresh source/schema grants required",
        ));
    }
    if plan.first.is_some() {
        super::nfo_refresh_initialization::apply_initialization(
            store,
            command,
            plan,
            initialization,
            permit,
            admitted_at_us,
        )?;
        #[cfg(test)]
        if plan.first.as_ref().is_some_and(|first| first.initializes()) {
            if let Some(probe) = probe.as_mut() {
                probe(store).map_err(|error| {
                    fail(MetadataStoreErrorCode::StoreFailure, error.to_string())
                })?;
            }
        }
    }
    let actual = read_refresh_metadata(store, &request.subject)?;
    if actual.schema.fingerprint() != plan.before.schema.fingerprint()
        || actual.documents != plan.before.documents
        || actual.classification != plan.before.classification
    {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "refresh metadata plan predecessor changed",
        ));
    }
    let budget = store.catalog_operation_budget().map_err(budget_error)?;
    let mut proof_budget = crate::catalog_nfo_attribution::ProofBudget::new(
        store.catalog_operation_budget().map_err(budget_error)?,
    );
    proof_budget
        .serialized(
            &(
                plan.source.fields(),
                plan.source.evidence(),
                &plan.raw_request,
                &plan.resolution,
                &plan.structural,
                &plan.origin,
            ),
            64,
        )
        .map_err(budget_error)?;
    let fields = serde_json::to_string(plan.source.fields())?;
    let evidence = encode(plan.source.evidence());
    let output = serde_json::to_vec(&plan.resolution)?;
    let bytes = fields
        .len()
        .checked_add(evidence.len())
        .and_then(|n| n.checked_add(plan.raw_request.len()))
        .and_then(|n| n.checked_add(output.len()))
        .ok_or(MetadataError::SnapshotBudget)?;
    budget
        .charge(crate::operation_budget::BudgetCharge {
            bytes,
            values: plan.after.schema.fields().len(),
            work: bytes / 64 + plan.after.schema.fields().len() + 1,
            ..Default::default()
        })
        .map_err(evaluation_budget_error)?;
    let schema = &plan.after.schema;
    let old = &plan.before.schema;
    if schema.fingerprint() != old.fingerprint() {
        store.db.execute("INSERT INTO catalog_metadata_schemas(item_id,schema_fingerprint,contract_version,policies_json,system_fields_json,prior_fingerprint,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us) VALUES (?,?,2,?,?,?,?,?,?,?)",params![request.subject,schema.fingerprint(),serde_json::to_string(&schema_registry::policies(schema))?,serde_json::to_string(schema.system_fields())?,old.fingerprint(),command.key().catalog_id(),command.key().host_namespace(),command.key().operation_id(),admitted_at_us])?;
        if store.db.execute("UPDATE catalog_metadata_schema_heads SET schema_fingerprint=? WHERE item_id=? AND schema_fingerprint=?",params![schema.fingerprint(),request.subject,old.fingerprint()])?!=1 {
            return Err(corrupt("refresh metadata schema compare-and-swap failed"));
        }
    }
    // Keep origin reconciliation before structural reconciliation: the same
    // legacy key can advance from root1 to origin2 and then structural3.
    for document in &plan.origin {
        proof_budget
            .serialized(document, 64)
            .map_err(budget_error)?;
        proof_budget
            .canonical_serialized(document.evidence(), 64)
            .map_err(budget_error)?;
        proof_budget
            .serialized(&plan.raw_request, 64)
            .map_err(budget_error)?;
        write_revision(
            &store.db,
            document,
            &plan.raw_request,
            command,
            admitted_at_us,
        )?;
    }
    write_revision(
        &store.db,
        &plan.source,
        &plan.raw_request,
        command,
        admitted_at_us,
    )?;
    for document in &plan.structural {
        let bytes = serde_json::to_vec(document.fields())?.len()
            + encode(document.evidence()).len()
            + plan.raw_request.len();
        budget
            .charge(crate::operation_budget::BudgetCharge {
                bytes,
                values: document.fields().len(),
                work: bytes / 64 + document.fields().len() + 1,
                ..Default::default()
            })
            .map_err(evaluation_budget_error)?;
        write_revision(
            &store.db,
            document,
            &plan.raw_request,
            command,
            admitted_at_us,
        )?;
    }
    crate::tagging::replace_source_authorized(
        store,
        command,
        tags,
        &request.subject,
        &request.namespace,
        &request.record_id,
        plan.source.revision(),
        &request.tags,
        &[],
        admitted_at_us,
        Some(&source_effect),
    )
    .map_err(|error| fail(MetadataStoreErrorCode::StoreFailure, error.to_string()))?;
    write_projection(&store.db, &plan.resolution)?;
    // Deliberately no DELETE catalog_metadata_permits. Only terminal batch
    // publication may release every subject barrier after complete verification.
    Ok(())
}
