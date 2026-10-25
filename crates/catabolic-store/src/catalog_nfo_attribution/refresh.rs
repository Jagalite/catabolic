//! Private candidate refresh batch. No dispatcher, host export or schema41
//! registration. Complete observation/curation/source grants are independent of
//! the immutable request. One receipt and one retained batch barrier cover every
//! effect; no per-observation PublicPut or transaction is invoked.
mod apply;
pub(super) use apply::apply_in;
mod closure;
mod commitment;
mod observation;

use super::{proof_budget::ProofBudget, state};
use crate::catalog_metadata_store::{self as metadata, ContributionReplace, MetadataAuthority};
use crate::{Error, Result, Store, operation_budget::BudgetCharge};
use catabolic_core::{
    catalog_command::CommandEnvelope,
    catalog_metadata::{MAX_METADATA_JSON_NODES, nfo_refresh::*},
};
use rusqlite::{Row, params, types::ValueRef};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Workflow {
    DirectRefresh,
    MetadataJob,
}
impl Workflow {
    fn mode(self) -> RefreshCompletionMode {
        match self {
            Self::DirectRefresh => RefreshCompletionMode::DirectRefresh,
            Self::MetadataJob => RefreshCompletionMode::MetadataJob,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RefreshRequest {
    pub contract_version: u32,
    /// Descriptive immutable intent only. Authority must independently agree.
    pub workflow: Workflow,
    pub refresh: RefreshRequestV2,
    pub expected_aggregates: BTreeMap<String, i64>,
}
/// Constructed by the retained host after complete all-membership curation.
/// None of these grants may be reconstructed from request subject names.
pub(super) struct SubjectAuthority {
    pub aggregate_revision: i64,
    pub curation_audit_digest: String,
    pub source: MetadataAuthority,
    pub initialization: MetadataAuthority,
    pub extension: MetadataAuthority,
    pub tags: crate::catalog_tag_mutation::TagAuthority,
    pub structural_sources: Vec<MetadataAuthority>,
    pub structural_legacy: Option<MetadataAuthority>,
}
pub(super) struct RefreshAuthority<'a> {
    pub workflow: Workflow,
    pub observations: &'a observation::RefreshObservationAudit<'a>,
    pub subjects: BTreeMap<String, SubjectAuthority>,
    pub first_sources: BTreeMap<String, &'a super::first_source::VerifiedFirstNfoAbsence>,
}
struct Step {
    effect: RefreshEffect,
    presence: PresencePlan,
    metadata: Option<metadata::RefreshSourcePlan>,
    metadata_digest: Option<String>,
    digest: String,
}
pub(super) struct PreparedRefresh<'a> {
    outer: CommandEnvelope,
    authority: &'a RefreshAuthority<'a>,
    request: RefreshRequest,
    observations: observation::VerifiedRefreshObservations<'a>,
    original: BTreeMap<String, state::CurrentNfoState>,
    plan: RefreshPlanV2,
    steps: Vec<Step>,
    digest: String,
    budget: ProofBudget,
}
fn refused(message: impl std::fmt::Display) -> Error {
    Error(format!("catalog_nfo_refresh_refused: {message}"))
}
fn charge(budget: &mut ProofBudget, bytes: usize, values: usize) -> Result<()> {
    budget.charge(BudgetCharge {
        bytes,
        values,
        work: bytes / 64 + values + 1,
        ..Default::default()
    })
}
fn text(row: &Row<'_>, index: usize, maximum: usize, budget: &mut ProofBudget) -> Result<String> {
    let ValueRef::Text(bytes) = row.get_ref(index)? else {
        return Err(refused("stored text absent"));
    };
    if bytes.len() > maximum {
        return Err(refused("stored text budget"));
    }
    charge(budget, bytes.len(), 1)?;
    Ok(std::str::from_utf8(bytes).map_err(refused)?.to_owned())
}
fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn current_generation(store: &Store) -> Result<i64> {
    Ok(store
        .db
        .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
            r.get(0)
        })?)
}

pub(super) fn prepare_in<'a>(
    store: &Store,
    outer: &CommandEnvelope,
    request: &RefreshRequest,
    authority: &'a RefreshAuthority<'a>,
) -> Result<PreparedRefresh<'a>> {
    if store.schema_version != 41
        || store.db.is_autocommit()
        || !store.writable
        || request.contract_version != 1
        || request.workflow != authority.workflow
        || outer.key().catalog_id() != store.database_id
        || Some(outer.admission_incarnation()) != store.catalog_incarnation.as_deref()
        || outer.expected_generation() != current_generation(store)?
    {
        return Err(refused("exact native41 writer/workflow scope required"));
    }
    let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
    budget.serialized(request, 64)?;
    budget.canonical_serialized(request, 64)?;
    if outer.payload() != &json!({"operation":"nfo_refresh","request":request}) {
        return Err(refused("outer refresh payload differs"));
    }
    for (subject, grant) in &authority.subjects {
        if !is_digest(&grant.curation_audit_digest)
            || grant.aggregate_revision < 0
            || [&grant.source, &grant.initialization, &grant.extension]
                .iter()
                .any(|g| g.subject != *subject || g.host_namespace != outer.key().host_namespace())
            || grant.tags.host_namespace != outer.key().host_namespace()
            || !grant.tags.allowed_subjects.contains(
                &crate::catalog_tag_mutation::TagSubject::Item(subject.clone()),
            )
        {
            return Err(refused("independent affected-subject grant differs"));
        }
        budget.serialized(
            &(
                subject,
                &grant.curation_audit_digest,
                grant.aggregate_revision,
            ),
            64,
        )?;
    }
    let aggregates = authority
        .subjects
        .iter()
        .map(|(id, g)| (id.clone(), g.aggregate_revision))
        .collect();
    if aggregates != request.expected_aggregates {
        return Err(refused(
            "immutable aggregate preconditions differ from admitted grants",
        ));
    }
    let closure = closure::read_in(store, outer, &request.refresh, &aggregates, &mut budget)?;
    let heads: BTreeMap<_, _> = closure
        .states
        .iter()
        .map(|(id, state)| (id.clone(), state.head().clone()))
        .collect();
    let observations = observation::verify_in(
        store,
        outer,
        &request.refresh,
        authority.observations,
        &heads,
        &closure.claims,
        &mut budget,
    )?;
    let snapshot = RefreshSnapshotV2 {
        contract_version: CONTRACT_VERSION_V2,
        fence: request.refresh.fence.clone(),
        subject: request.refresh.subject.clone(),
        files: observations.revalidated_files().to_vec(),
        subjects: closure
            .states
            .iter()
            .map(|(id, s)| (id.clone(), s.head().clone()))
            .collect(),
        identity_claims: closure.claims,
    };
    if snapshot.fence.catalog_id != store.database_id
        || Some(snapshot.fence.incarnation.as_str()) != store.catalog_incarnation.as_deref()
        || snapshot.fence.generation != outer.expected_generation()
    {
        return Err(refused("refresh input fence differs"));
    }
    budget.serialized(&snapshot, 64)?;
    let eligibility = classify_refresh_v2(&snapshot, &request.refresh).map_err(refused)?;
    // Positive absence is independently verified before any batch effect.
    // Native missing rows below are consistency only, never authority.
    let mut first_plans = BTreeMap::new();
    for (subject, first) in &authority.first_sources {
        first.validate_in(store, outer)?;
        if !authority.subjects.contains_key(subject)
            || first.current().presence().subject != *subject
            || closure.states.get(subject).map(|s| s.presence()) != Some(first.current().presence())
        {
            return Err(refused("first-source capability outside exact closure"));
        }
    }
    let mut metadata_snapshots = BTreeMap::new();
    for (subject, state) in &closure.states {
        if state.native_source().is_some() && state.schema().is_some() {
            metadata_snapshots.insert(
                subject.clone(),
                metadata::read_refresh_metadata(store, subject).map_err(refused)?,
            );
        }
    }
    for (subject, first) in &authority.first_sources {
        let managed = if first.current().schema().is_some() {
            Some(metadata::read_refresh_metadata(store, subject).map_err(refused)?)
        } else {
            None
        };
        let first_plan = metadata::plan_first_refresh_metadata(
            subject,
            managed.as_ref(),
            first.legacy_raw_json(),
            first.origin(),
            &authority.subjects[subject].initialization,
            outer,
            &mut budget,
        )
        .map_err(refused)?;
        metadata_snapshots.insert(subject.clone(), first_plan.snapshot().clone());
        first_plans.insert(subject.clone(), first_plan);
    }
    budget.serialized(&request.refresh, 64)?;
    let mut normalized = request.refresh.clone();
    for (entry, eligibility) in normalized.observations.iter_mut().zip(&eligibility) {
        if eligibility.disposition != EligibilityDispositionV2::Parsed {
            continue;
        }
        let SidecarObservation::Parsed { contribution, .. } = &mut entry.sidecar else {
            return Err(refused("eligible parsed observation changed"));
        };
        budget.serialized(contribution, 64)?;
        let fields = serde_json::to_string(&contribution.fields)?;
        contribution.fields = metadata::normalize_refresh_fields(
            metadata_snapshots
                .get(&request.refresh.subject)
                .ok_or_else(|| {
                    refused("refresh first publication requires positive absence composition")
                })?,
            &request.refresh.subject,
            &fields,
        )
        .map_err(refused)?;
        contribution.tags = canonical_tags(store, &contribution.tags, &mut budget)?;
    }
    let plan = plan_refresh_v2(&snapshot, &normalized).map_err(refused)?;
    require_refresh_completion(authority.workflow.mode(), &plan.counts).map_err(refused)?;
    let plan_commitment = commitment::AdmittedCommitment::capture(&mut budget, &plan)?;
    let mut current_presence: BTreeMap<_, _> = closure
        .states
        .iter()
        .map(|(id, s)| (id.clone(), s.presence().clone()))
        .collect();
    let mut steps = Vec::new();
    for (index, effect) in plan.effects.iter().enumerate() {
        let before = &current_presence[&effect.subject];
        let change = presence_change(effect, before)?;
        let presence = plan_presence(
            before,
            &PresenceRequest {
                contract_version: PRESENCE_CONTRACT_VERSION,
                fence: before.fence.clone(),
                subject: effect.subject.clone(),
                expected: before.stamp(),
                change,
            },
        )
        .map_err(refused)?;
        if presence.final_snapshot.head.source
            != match &effect.document {
                DocumentEffect::Retain => before.head.source.clone(),
                DocumentEffect::Replace { head } => Some(head.clone()),
            }
            || presence.final_snapshot.head.attribution.as_ref() != Some(&effect.attribution)
        {
            return Err(refused("refresh and presence transition disagree"));
        }
        let metadata = if let DocumentEffect::Replace { head } = &effect.document {
            let (contribution, identity) = match &head.document {
                DocumentState::Present {
                    contribution,
                    external_identity,
                } => (contribution.clone(), external_identity.as_deref()),
                DocumentState::Withdrawn => (Contribution::default(), None),
            };
            budget.serialized(&contribution, 64)?;
            let old = metadata_snapshots
                .get(&effect.subject)
                .ok_or_else(|| refused("metadata predecessor absent"))?;
            let prior_identity = match before.head.source.as_ref().map(|s| &s.document) {
                Some(DocumentState::Present {
                    external_identity, ..
                }) => external_identity.as_deref(),
                _ => None,
            };
            let pin = match &before.head.pin {
                PinHead::Decision { value, .. } => value.as_deref(),
                _ => None,
            };
            let request = ContributionReplace {
                contract_version: 2,
                subject: effect.subject.clone(),
                namespace: "motion.nfo".into(),
                record_id: "document".into(),
                expected_schema_fingerprint: first_plans
                    .get(&effect.subject)
                    .map(|p| p.expected_schema_fingerprint().map(str::to_owned))
                    .unwrap_or_else(|| Some(old.schema.fingerprint().into())),
                expected_revision: before.head.source.as_ref().map_or(0, |s| s.revision),
                fields_json: serde_json::to_string(&contribution.fields)?,
                evidence_json: serde_json::to_string(
                    &json!({"owner":"nfo_refresh","outer_digest":outer.digest().as_str(),"effect_index":index,"observation_proof":observations.proof_digest()}),
                )?,
                tags: contribution.tags.iter().cloned().collect(),
                excluded_tags: vec![],
                origin_creation_intent: first_plans
                    .get(&effect.subject)
                    .and_then(|p| p.origin_creation_intent())
                    .map(str::to_owned),
            };
            let grants = &authority.subjects[&effect.subject];
            let structural = metadata::RefreshProjectionGrants {
                sources: &grants.structural_sources,
                legacy_reconciliation: grants.structural_legacy.as_ref(),
            };
            // Admit the retained classification set before the metadata plan
            // clones it; later plan-digest admission does not cover allocation.
            if let metadata::MetadataAccess::InitializeV2 { system_fields } =
                &grants.initialization.access
            {
                budget.serialized(system_fields, 64)?;
            }
            budget.serialized(&(&old.documents, &request), 64)?;
            let prepared = if let Some(first) = first_plans.remove(&effect.subject) {
                metadata::plan_first_refresh_source(
                    &first,
                    &request,
                    outer,
                    prior_identity,
                    pin,
                    identity,
                    &structural,
                    &grants.initialization,
                )
            } else {
                metadata::plan_refresh_source(
                    old,
                    &request,
                    outer,
                    prior_identity,
                    pin,
                    identity,
                    &structural,
                    &grants.initialization,
                )
            }
            .map_err(refused)?;
            if prepared.source.revision() != head.revision
                || prepared.source.fields() != &contribution.fields
            {
                return Err(refused(
                    "normalized source plan differs from refresh effect",
                ));
            }
            metadata_snapshots.insert(effect.subject.clone(), prepared.after.clone());
            Some(prepared)
        } else {
            None
        };
        let presence_commitment = commitment::AdmittedCommitment::capture(&mut budget, &presence)?;
        let metadata_digest = metadata
            .as_ref()
            .map(|plan| source_plan_digest(plan, &mut budget))
            .transpose()?;
        let digest = commitment::step(
            &mut budget,
            index,
            effect,
            &presence_commitment,
            &metadata_digest,
            metadata.as_ref().filter(|p| p.first.is_some()).map(|_| {
                let first = authority.first_sources[&effect.subject];
                (first.proof_digest(), first.audit_digest())
            }),
            outer.digest().as_str(),
        )?;
        current_presence.insert(effect.subject.clone(), presence.final_snapshot.clone());
        steps.push(Step {
            effect: effect.clone(),
            presence,
            metadata,
            metadata_digest,
            digest,
        });
    }
    let digest = commitment::batch(
        &mut budget,
        outer.digest().as_str(),
        observations.proof_digest(),
        &plan_commitment,
        &steps
            .iter()
            .map(|step| step.digest.as_str())
            .collect::<Vec<_>>(),
    )?;
    Ok(PreparedRefresh {
        outer: outer.clone(),
        authority,
        request: request.clone(),
        observations,
        original: closure.states,
        plan,
        steps,
        digest,
        budget,
    })
}

fn presence_change(effect: &RefreshEffect, before: &PresenceSnapshot) -> Result<PresenceChange> {
    match effect.reason {
        EffectReason::Withdraw => Ok(PresenceChange::Withdraw),
        EffectReason::ReleaseRetiredAttribution => Ok(PresenceChange::ReleaseRetiredAttribution),
        EffectReason::Publish => {
            let head =
                match &effect.document {
                    DocumentEffect::Replace { head } => head,
                    DocumentEffect::Retain => before.head.source.as_ref().ok_or_else(|| {
                        refused("origin-only publication lacks retained document")
                    })?,
                };
            let DocumentState::Present {
                contribution,
                external_identity,
            } = &head.document
            else {
                return Err(refused("publication document is not present"));
            };
            Ok(PresenceChange::Publish {
                contribution: contribution.clone(),
                external_identity: external_identity.clone(),
                origin: effect
                    .attribution
                    .origin
                    .clone()
                    .ok_or_else(|| refused("publish origin absent"))?,
            })
        }
    }
}
fn canonical_tags(
    store: &Store,
    names: &BTreeSet<String>,
    budget: &mut ProofBudget,
) -> Result<BTreeSet<String>> {
    let mut result = BTreeSet::new();
    for name in names {
        budget.serialized(name, 64)?;
        if crate::tagging::tag_name(&json!(name))? != *name {
            return Err(refused("lossy source tag normalization"));
        }
        let mut query=store.db.prepare("SELECT t.name,EXISTS(SELECT 1 FROM catalog_tag_redirects r WHERE r.source_tag_id=t.id) FROM tag_names n JOIN tags t ON t.id=n.tag_id WHERE n.name=?")?;
        let mut rows = query.query([name])?;
        let mapped = rows
            .next()?
            .map(|row| -> Result<_> {
                Ok((
                    text(
                        row,
                        0,
                        catabolic_core::catalog_metadata::MAX_METADATA_SNAPSHOT_BYTES,
                        budget,
                    )?,
                    row.get::<_, bool>(1)?,
                ))
            })
            .transpose()?;
        match mapped {
            Some((_, true)) => return Err(refused("source tag resolves to retired vocabulary")),
            Some((name, false)) => {
                budget.serialized(&name, 64)?;
                result.insert(name);
            }
            None => {
                result.insert(name.clone());
            }
        }
    }
    Ok(result)
}

/// Constructible only while the batch owner advances one admitted step. A
/// source grant alone cannot mint a schema41 refresh write capability.
pub(crate) struct RefreshStepPermit {
    outer: CommandEnvelope,
    subject: String,
    expected_generation: i64,
    raw_request: String,
    metadata_digest: String,
    first_proof_digest: Option<String>,
}
impl RefreshStepPermit {
    pub(crate) fn begin_source_effect_in(
        &self,
        store: &Store,
        outer: &CommandEnvelope,
        plan: &metadata::RefreshSourcePlan,
    ) -> Result<super::source_effect::NfoSourceEffect> {
        self.authorize_source_in(store, outer, plan)?;
        super::source_effect::begin_publication_in(
            store,
            outer,
            &self.subject,
            plan.source.revision(),
            &plan.raw_request,
        )
    }

    pub(crate) fn authorize_first_in(
        &self,
        store: &Store,
        outer: &CommandEnvelope,
        plan: &metadata::RefreshSourcePlan,
    ) -> Result<()> {
        if plan.first.is_none() || !self.first_proof_digest.as_deref().is_some_and(is_digest) {
            return Err(refused("positive first-source batch capability absent"));
        }
        self.authorize_source_in(store, outer, plan)
    }
    pub(crate) fn authorize_source_in(
        &self,
        store: &Store,
        outer: &CommandEnvelope,
        plan: &metadata::RefreshSourcePlan,
    ) -> Result<()> {
        if store.schema_version != 41
            || store.db.is_autocommit()
            || outer != &self.outer
            || plan.request.subject != self.subject
            || plan.raw_request != self.raw_request
            || current_generation(store)? != self.expected_generation
        {
            return Err(refused("refresh source step capability mismatch"));
        }
        let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
        if source_plan_digest(plan, &mut budget)? != self.metadata_digest {
            return Err(refused("batch source/structural effect digest differs"));
        }
        let barrier:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_metadata_permits WHERE item_id=? AND incarnation=? AND barrier_id=1) AND NOT EXISTS(SELECT 1 FROM catalog_commit_barrier WHERE id=1)",params![self.subject,outer.admission_incarnation()],|r|r.get(0))?;
        if !barrier {
            return Err(refused("batch deferred barrier absent"));
        }
        Ok(())
    }
}

#[derive(Clone)]
struct WrittenHead {
    state_revision: i64,
    source_revision: i64,
    source_written: bool,
}
/// Sole private exception to historical receipt reads: exact revisions already
/// written by THIS pending batch, protected by its COMPLETE deferred barrier set.
/// Existing historical rows never use this capability. Dropping it releases
/// nothing and cannot make an incomplete transaction committable.
pub(super) struct RefreshReadPermit {
    outer: CommandEnvelope,
    expected_generation: i64,
    subjects: BTreeSet<String>,
    written: BTreeMap<String, WrittenHead>,
}
impl RefreshReadPermit {
    pub(super) fn key(&self) -> &catabolic_core::catalog_command::CommandKey {
        self.outer.key()
    }
    fn validate_in(&self, store: &Store, generation: i64, budget: &mut ProofBudget) -> Result<()> {
        if store.schema_version != 41
            || store.db.is_autocommit()
            || self.outer.key().catalog_id() != store.database_id
            || Some(self.outer.admission_incarnation()) != store.catalog_incarnation.as_deref()
            || generation != self.expected_generation
            || current_generation(store)? != generation
        {
            return Err(refused("pending read scope/generation differs"));
        }
        budget.serialized(&self.subjects, 64)?;
        let ids = serde_json::to_string(&self.subjects)?;
        let count = i64::try_from(self.subjects.len()).map_err(refused)?;
        let exact:bool=store.db.query_row("SELECT (SELECT count(*) FROM catalog_metadata_permits)=?1 AND (SELECT count(*) FROM catalog_metadata_permits WHERE item_id IN(SELECT value FROM json_each(?2)) AND incarnation=?3 AND barrier_id=1)=?1 AND NOT EXISTS(SELECT 1 FROM catalog_commit_barrier WHERE id=1) AND NOT EXISTS(SELECT 1 FROM catalog_command_receipts WHERE catalog_id=?4 AND host_namespace=?5 AND operation_id=?6)",params![count,ids,self.outer.admission_incarnation(),self.outer.key().catalog_id(),self.outer.key().host_namespace(),self.outer.key().operation_id()],|r|r.get(0))?;
        if !exact {
            return Err(refused("complete pending batch barrier/receipt differs"));
        }
        Ok(())
    }
    pub(super) fn authorize_source_read_in(
        &self,
        store: &Store,
        subject: &str,
        revision: i64,
        generation: i64,
        budget: &mut ProofBudget,
    ) -> Result<()> {
        self.validate_in(store, generation, budget)?;
        if !self.subjects.contains(subject)
            || !self
                .written
                .get(subject)
                .is_some_and(|head| head.source_written && head.source_revision == revision)
        {
            return Err(refused("pending source revision was not written by batch"));
        }
        Ok(())
    }
    pub(super) fn authorize_state_read_in(
        &self,
        store: &Store,
        subject: &str,
        revision: i64,
        source_revision: i64,
        generation: i64,
        budget: &mut ProofBudget,
    ) -> Result<()> {
        self.validate_in(store, generation, budget)?;
        if !self.subjects.contains(subject)
            || !self.written.get(subject).is_some_and(|head| {
                head.state_revision == revision && head.source_revision == source_revision
            })
        {
            return Err(refused(
                "pending attribution revision was not written by batch",
            ));
        }
        Ok(())
    }
}

fn source_plan_digest(
    plan: &metadata::RefreshSourcePlan,
    budget: &mut ProofBudget,
) -> Result<String> {
    budget.serialized(
        &(
            "catabolic.nfo.refresh.metadata.v2",
            &plan.request,
            &plan.classification_fields,
            plan.before.schema.fingerprint(),
            plan.before.classification_digest(),
            &plan.before.documents,
            plan.after.schema.fingerprint(),
            &plan.after.documents,
            &plan.source,
            &plan.structural,
            &plan.origin,
            plan.first.as_ref().map(|p| p.digest()),
            &plan.resolution,
        ),
        64,
    )
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod batch_tests;
