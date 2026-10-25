//! Pure first-refresh metadata preparation and capability-bound root persistence.
//! Missing rows and these plans are descriptive, never positive NFO absence
//! authority. The refresh coordinator must retain its independently verified
//! first-source token and attest it through the exact batch step permit.
use super::nfo_refresh::{RefreshMetadataSnapshot, RefreshSourcePlan};
use super::*;
use crate::catalog_nfo_attribution::ProofBudget;
use std::sync::Arc;

#[derive(Clone, Debug)]
struct Initialization {
    request: MetadataInitializeV2,
    raw: String,
    policies_json: String,
    system_fields_json: String,
    raw_mutation: String,
    legacy: ContributionDocument,
    resolution: MetadataResolution,
}

/// A virtual predecessor, not an initialized database or a source-write grant.
/// `before` retains legacy revision1; `snapshot` includes the ordered origin
/// reconciliation used for normalization and later structural planning.
#[derive(Clone, Debug)]
pub(crate) struct RefreshFirstMetadataPlan {
    // Clones share admitted immutable bytes; they do not allocate a second
    // raw baseline and both virtual document snapshots per observation.
    inner: Arc<FirstMetadata>,
}
#[derive(Debug)]
struct FirstMetadata {
    before: RefreshMetadataSnapshot,
    snapshot: RefreshMetadataSnapshot,
    initialization: Option<Initialization>,
    origin: Vec<ContributionDocument>,
    expected_schema_fingerprint: Option<String>,
    origin_creation_intent: Option<String>,
    digest: String,
}
impl RefreshFirstMetadataPlan {
    pub(crate) fn snapshot(&self) -> &RefreshMetadataSnapshot {
        &self.inner.snapshot
    }
    pub(crate) fn expected_schema_fingerprint(&self) -> Option<&str> {
        self.inner.expected_schema_fingerprint.as_deref()
    }
    pub(crate) fn origin_creation_intent(&self) -> Option<&str> {
        self.inner.origin_creation_intent.as_deref()
    }
    pub(crate) fn digest(&self) -> &str {
        &self.inner.digest
    }
    pub(super) fn before(&self) -> &RefreshMetadataSnapshot {
        &self.inner.before
    }
    pub(super) fn origin(&self) -> &[ContributionDocument] {
        &self.inner.origin
    }
    #[cfg(test)]
    pub(super) fn initializes(&self) -> bool {
        self.inner.initialization.is_some()
    }
}
fn budget_error(error: impl std::fmt::Display) -> MetadataStoreError {
    fail(
        MetadataStoreErrorCode::StoreFailure,
        format!("first refresh metadata budget: {error}"),
    )
}
fn invalid(message: &str) -> MetadataStoreError {
    fail(MetadataStoreErrorCode::AuthorityDenied, message)
}
fn charge_snapshot(snapshot: &RefreshMetadataSnapshot, budget: &mut ProofBudget) -> Result<()> {
    budget
        .serialized(
            &(
                snapshot.schema.fields(),
                snapshot.schema.system_fields(),
                snapshot.schema.fingerprint(),
                &snapshot.documents,
            ),
            64,
        )
        .map_err(budget_error)?;
    Ok(())
}

/// The coordinator supplies only inputs borrowed from its validated
/// VerifiedFirstNfoAbsence. This read-only function deliberately cannot attest
/// that absence and cannot write, even if caller-chosen raw bytes are supplied.
pub(crate) fn plan_first_refresh_metadata(
    subject: &str,
    managed: Option<&RefreshMetadataSnapshot>,
    legacy_raw: Option<&str>,
    origin: Option<&VerifiedCreatedOrigin>,
    initialization: &MetadataAuthority,
    outer: &CommandEnvelope,
    budget: &mut ProofBudget,
) -> Result<RefreshFirstMetadataPlan> {
    let MetadataAccess::InitializeV2 { system_fields } = &initialization.access else {
        return Err(invalid(
            "first refresh needs explicit initialization classification",
        ));
    };
    if initialization.subject != subject
        || initialization.host_namespace != outer.key().host_namespace()
    {
        return Err(invalid("first refresh initialization scope differs"));
    }
    legacy_key(subject)?;
    budget
        .serialized(&(subject, system_fields), 64)
        .map_err(budget_error)?;
    let (before, initialization, expected_schema_fingerprint, intent) =
        if let Some(snapshot) = managed {
            if legacy_raw.is_some()
                || origin.is_some()
                || !super::nfo_refresh::valid_classification(snapshot, system_fields)
                || snapshot
                    .documents
                    .iter()
                    .any(|document| document.key().namespace() == "motion.nfo")
            {
                return Err(invalid("managed first refresh predecessor differs"));
            }
            charge_snapshot(snapshot, budget)?;
            (
                snapshot.clone(),
                None,
                Some(snapshot.schema.fingerprint().to_owned()),
                None,
            )
        } else {
            let raw = legacy_raw.ok_or_else(|| invalid("audited initial metadata bytes absent"))?;
            let origin =
                origin.ok_or_else(|| invalid("verified fresh creation title origin absent"))?;
            if raw.len() > MAX_METADATA_DOCUMENT_BYTES {
                return Err(MetadataError::DocumentBudget.into());
            }
            budget
                .raw_json(
                    raw,
                    catabolic_core::catalog_metadata::MAX_METADATA_JSON_DEPTH,
                )
                .map_err(budget_error)?;
            let value = catabolic_core::json::parse(raw, true)
                .map_err(|_| corrupt("invalid initial metadata JSON"))?;
            let fields = value.as_object().ok_or(MetadataError::ExpectedObject)?;
            if !system_fields.iter().all(|field| fields.contains_key(field)) {
                return Err(invalid("initial system policy has no original field"));
            }
            let mut policies = Vec::new();
            for field in fields.keys() {
                #[derive(Serialize)]
                struct Policy<'a> {
                    field: &'a str,
                    value_type: PolicyValueType,
                    nullable: bool,
                    normalization: PolicyNormalization,
                }
                budget
                    .serialized(
                        &Policy {
                            field,
                            value_type: PolicyValueType::Json,
                            nullable: false,
                            normalization: PolicyNormalization::Exact,
                        },
                        64,
                    )
                    .map_err(budget_error)?;
                policies.push(PolicyField {
                    field: field.clone(),
                    value_type: PolicyValueType::Json,
                    nullable: false,
                    normalization: PolicyNormalization::Exact,
                });
            }
            let request = MetadataInitializeV2 {
                contract_version: 2,
                subject: subject.to_owned(),
                policies,
                system_fields: system_fields.clone(),
            };
            budget.serialized(&request, 64).map_err(budget_error)?;
            let mut external = Vec::new();
            let mut internal = Vec::new();
            for policy in &request.policies {
                let pair = (
                    policy.field.clone(),
                    schema_registry::convert_policy(policy),
                );
                if request.system_fields.contains(&policy.field) {
                    internal.push(pair);
                } else {
                    external.push(pair);
                }
            }
            let schema = FieldSchema::new_v2(external, internal)?;
            budget
                .raw_json(
                    raw,
                    catabolic_core::catalog_metadata::MAX_METADATA_JSON_DEPTH,
                )
                .map_err(budget_error)?;
            let legacy = ContributionDocument::from_json_with_system_fields(
                &schema,
                legacy_key(subject)?,
                SourceKind::LegacyImport,
                1,
                raw,
                r#"{"origin":"unknown","import":"items.metadata"}"#,
            )?;
            budget.serialized(&legacy, 64).map_err(budget_error)?;
            let resolution = resolve(&schema, subject, &[legacy.clone()])?;
            budget.serialized(&resolution, 64).map_err(budget_error)?;
            budget
                .serialized(&request.policies, 64)
                .map_err(budget_error)?;
            let policies_json = serde_json::to_string(&request.policies)?;
            budget
                .serialized(&request.system_fields, 64)
                .map_err(budget_error)?;
            let system_fields_json = serde_json::to_string(&request.system_fields)?;
            budget.serialized(&request, 64).map_err(budget_error)?;
            let raw_mutation = serde_json::to_string(&request)?;
            if raw_mutation.len() > MAX_RAW_MUTATION_BYTES {
                return Err(MetadataError::DocumentBudget.into());
            }
            budget
                .serialized(&(raw, &origin.intent, &legacy), 64)
                .map_err(budget_error)?;
            let before = RefreshMetadataSnapshot {
                schema,
                documents: vec![legacy.clone()],
                classification: None,
            };
            (
                before,
                Some(Initialization {
                    request,
                    raw: raw.to_owned(),
                    policies_json,
                    system_fields_json,
                    raw_mutation,
                    legacy,
                    resolution,
                }),
                None,
                Some(origin.intent.clone()),
            )
        };
    charge_snapshot(&before, budget)?;
    let mut snapshot = before.clone();
    budget
        .serialized(&(&snapshot.documents, &intent), 64)
        .map_err(budget_error)?;
    let origin_documents = super::origin::plan_origin(
        &snapshot.schema,
        &mut snapshot.documents,
        subject,
        intent.as_deref(),
        outer,
        origin,
    )?;
    snapshot.documents.sort_by(|a, b| a.key().cmp(b.key()));
    let root_wire = initialization.as_ref().map(|root| {
        (
            &root.request,
            &root.raw,
            &root.policies_json,
            &root.system_fields_json,
            &root.raw_mutation,
            &root.legacy,
            &root.resolution,
        )
    });
    let digest = budget
        .serialized(
            &(
                "catabolic.nfo.refresh.first-metadata.v1",
                subject,
                outer.key(),
                outer.digest().as_str(),
                &expected_schema_fingerprint,
                &intent,
                before.schema.fingerprint(),
                &before.documents,
                &snapshot.documents,
                &origin_documents,
                root_wire,
            ),
            64,
        )
        .map_err(budget_error)?;
    Ok(RefreshFirstMetadataPlan {
        inner: Arc::new(FirstMetadata {
            before,
            snapshot,
            initialization,
            origin: origin_documents,
            expected_schema_fingerprint,
            origin_creation_intent: intent,
            digest,
        }),
    })
}

/// Only the batch's exact first-step permit admits the precomputed root. The
/// coordinator already validated the positive absence token at the original
/// fence; unrelated earlier batch effects may now have advanced generation.
/// This helper never releases or replaces the already acquired batch barrier.
pub(super) fn apply_initialization(
    store: &crate::Store,
    outer: &CommandEnvelope,
    plan: &RefreshSourcePlan,
    authority: &MetadataAuthority,
    permit: &crate::catalog_nfo_attribution::refresh::RefreshStepPermit,
    admitted_at_us: i64,
) -> Result<()> {
    permit
        .authorize_first_in(store, outer, plan)
        .map_err(|error| invalid(&error.to_string()))?;
    let first = plan
        .first
        .as_ref()
        .ok_or_else(|| invalid("exact first metadata plan absent"))?;
    let Some(root) = &first.inner.initialization else {
        return Ok(());
    };
    if store.schema_version != 41
        || store.db.is_autocommit()
        || admitted_at_us < 0
        || authority.subject != root.request.subject
        || authority.host_namespace != outer.key().host_namespace()
        || !matches!(&authority.access,MetadataAccess::InitializeV2 { system_fields } if system_fields==&root.request.system_fields)
        || plan.request.expected_schema_fingerprint.is_some()
        || plan.request.expected_revision != 0
    {
        return Err(invalid("retained first initialization scope differs"));
    }
    let mut budget = ProofBudget::new(store.catalog_operation_budget().map_err(budget_error)?);
    let mut query=store.db.prepare("SELECT i.metadata,r.revision FROM items i JOIN catalog_entity_revisions r ON r.entity_kind='items' AND r.entity_key=json_array(i.id) AND r.deleted=0 WHERE i.id=?")?;
    let mut rows = query.query([&root.request.subject])?;
    let row = rows
        .next()?
        .ok_or_else(|| corrupt("fresh initialization item absent"))?;
    let raw = row
        .get_ref(0)?
        .as_str()
        .map_err(|_| corrupt("fresh initialization raw is not text"))?;
    budget
        .raw_json(
            raw,
            catabolic_core::catalog_metadata::MAX_METADATA_JSON_DEPTH,
        )
        .map_err(budget_error)?;
    if raw != root.raw || row.get::<_, i64>(1)? != 1 {
        return Err(invalid("audited fresh initialization baseline changed"));
    }
    drop(rows);
    drop(query);
    let absent:bool=store.db.query_row("SELECT NOT EXISTS(SELECT 1 FROM catalog_metadata_subjects WHERE item_id=?1) AND NOT EXISTS(SELECT 1 FROM catalog_metadata_schema_heads WHERE item_id=?1) AND NOT EXISTS(SELECT 1 FROM catalog_metadata_schemas WHERE item_id=?1) AND NOT EXISTS(SELECT 1 FROM catalog_metadata_sources WHERE item_id=?1) AND NOT EXISTS(SELECT 1 FROM catalog_metadata_history WHERE item_id=?1) AND NOT EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id=?1)",[&root.request.subject],|row|row.get(0))?;
    if !absent {
        return Err(invalid(
            "first initialization predecessor already managed or retired",
        ));
    }
    budget
        .serialized(
            &(
                &root.request,
                &root.raw,
                &root.policies_json,
                &root.system_fields_json,
                &root.raw_mutation,
                &root.legacy,
                &root.resolution,
            ),
            64,
        )
        .map_err(budget_error)?;
    budget
        .canonical_serialized(root.legacy.evidence(), 64)
        .map_err(budget_error)?;
    budget
        .canonical_serialized(root.legacy.fields(), 64)
        .map_err(budget_error)?;
    // Match initialize_v2's exact root/history wire and order, but retain the
    // preinstalled batch permit and outer key. No synthetic new receipt/fence.
    let schema = &first.inner.before.schema;
    store.db.execute("INSERT INTO catalog_metadata_schemas(item_id,schema_fingerprint,contract_version,policies_json,system_fields_json,prior_fingerprint,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us) VALUES (?,?,2,?,?,NULL,?,?,?,?)",params![root.request.subject,schema.fingerprint(),root.policies_json,root.system_fields_json,outer.key().catalog_id(),outer.key().host_namespace(),outer.key().operation_id(),admitted_at_us])?;
    store.db.execute("INSERT INTO catalog_metadata_subjects(item_id,contract_version,schema_fingerprint,policies_json,legacy_raw_json,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us) VALUES (?,2,?,?,?,?,?,?,?)",params![root.request.subject,schema.fingerprint(),root.policies_json,root.raw,outer.key().catalog_id(),outer.key().host_namespace(),outer.key().operation_id(),admitted_at_us])?;
    write_revision(
        &store.db,
        &root.legacy,
        &root.raw_mutation,
        outer,
        admitted_at_us,
    )?;
    write_projection(&store.db, &root.resolution)?;
    Ok(())
}
