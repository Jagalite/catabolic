//! Private schema41 logical-retirement companion. No survivor transfer, source
//! mutation, receipt or transaction is created here. The reviewed logical owner
//! retains its deferred barrier until every terminal attribution check succeeds.
use super::{proof_budget::ProofBudget, state};
use crate::{Error, Result, Store, catalog_logical_edits::ReviewedLogicalEdit};
use catabolic_core::{
    catalog_command::{CommandEnvelope, CommandKey},
    catalog_metadata::nfo_refresh::{
        DocumentEffect, DocumentState, PRESENCE_CONTRACT_VERSION, PresenceChange, PresenceEffect,
        PresenceHeadEffect, PresenceRequest, PresenceSnapshot, SubjectState, plan_presence,
    },
};
use rusqlite::params;
use std::collections::{BTreeMap, BTreeSet};

fn refused(reason: impl std::fmt::Display) -> Error {
    Error(format!("catalog_nfo_retirement_refused: {reason}"))
}
fn generation(store: &Store) -> Result<i64> {
    Ok(store
        .db
        .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
            r.get(0)
        })?)
}
fn scope(store: &Store, outer: &CommandEnvelope) -> Result<()> {
    if store.schema_version != 41
        || !store.writable
        || store.db.is_autocommit()
        || outer.key().catalog_id() != store.database_id
        || Some(outer.admission_incarnation()) != store.catalog_incarnation.as_deref()
    {
        return Err(refused("exact retained native41 writer required"));
    }
    store.transaction_available()?;
    Ok(())
}

pub(crate) struct RetirementPlan {
    outer: CommandEnvelope,
    before: BTreeMap<String, state::CurrentNfoState>,
    budget: ProofBudget,
}

/// Called only after the logical owner has independently validated the exact
/// reviewed graph and authorization, but before its first effect.
pub(crate) fn prepare_in(
    store: &Store,
    outer: &CommandEnvelope,
    reviewed: &ReviewedLogicalEdit,
    subjects: &BTreeSet<String>,
) -> Result<RetirementPlan> {
    scope(store, outer)?;
    if generation(store)? != outer.expected_generation() || subjects.is_empty() {
        return Err(refused(
            "retirement admission generation or endpoints differ",
        ));
    }
    let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
    budget.serialized(&(reviewed, subjects, outer.key(), outer.payload()), 64)?;
    if outer.payload() != &serde_json::json!({"operation":"logical_edit","reviewed":reviewed}) {
        return Err(refused("exact reviewed logical command absent"));
    }
    let mut before = BTreeMap::new();
    for subject in subjects {
        let current = state::read_in(store, subject, outer.expected_generation(), 0)?;
        if !matches!(current.head().subject_state, SubjectState::Active { .. }) {
            return Err(refused("retirement predecessor is not active"));
        }
        budget.serialized(&(subject, current.presence()), 64)?;
        before.insert(subject.clone(), current);
    }
    Ok(RetirementPlan {
        outer: outer.clone(),
        before,
        budget,
    })
}
impl RetirementPlan {
    /// Identity/association changes must not silently change the independently
    /// captured source, public presence, pins or suppression before retirement.
    pub(crate) fn revalidate_before_retirement(&mut self, store: &Store) -> Result<()> {
        scope(store, &self.outer)?;
        let at = generation(store)?;
        self.check_barrier(store, false)?;
        for (subject, before) in &self.before {
            let actual = state::read_in(store, subject, at, 0)?;
            if !same_retained(before, &actual) || before.head() != actual.head() {
                return Err(refused("logical effects changed retained NFO predecessor"));
            }
        }
        Ok(())
    }
    fn check_barrier(&self, store: &Store, retired: bool) -> Result<()> {
        check_logical_barrier(store, &self.outer)?;
        if retired {
            let count: i64 = store.db.query_row(
                "SELECT count(*) FROM catalog_logical_retirements WHERE command_catalog_id=? AND command_host_namespace=? AND command_operation_id=?",
                params![self.outer.key().catalog_id(), self.outer.key().host_namespace(), self.outer.key().operation_id()], |r|r.get(0))?;
            if usize::try_from(count).ok() != Some(self.before.len()) {
                return Err(refused("retirement endpoint set differs"));
            }
            for subject in self.before.keys() {
                check_retirement(store, &self.outer, subject)?;
            }
        }
        Ok(())
    }
    /// All actual retirement rows already exist under the same logical barrier.
    /// A failure leaves that barrier in place, including when an error is caught.
    pub(crate) fn publish_in(mut self, store: &Store) -> Result<()> {
        scope(store, &self.outer)?;
        self.check_barrier(store, true)?;
        let mut expected = BTreeMap::new();
        let mut written = BTreeMap::new();
        for (subject, before) in &self.before {
            self.budget.serialized(before.presence(), 64)?;
            let mut snapshot = before.presence().clone();
            snapshot.fence.generation = generation(store)?;
            snapshot.head.subject_state = state::subject_state(store, subject, &mut self.budget)?;
            if !matches!(snapshot.head.subject_state, SubjectState::Retired { .. }) {
                return Err(refused("actual retirement proof absent"));
            }
            let request = PresenceRequest {
                contract_version: PRESENCE_CONTRACT_VERSION,
                fence: snapshot.fence.clone(),
                subject: subject.clone(),
                expected: snapshot.stamp(),
                change: PresenceChange::ReleaseRetiredAttribution,
            };
            let plan = plan_presence(&snapshot, &request).map_err(refused)?;
            self.budget.serialized(&plan, 64)?;
            if let PresenceEffect::Changed {
                document,
                presence,
                attribution,
            } = &plan.effect
            {
                if !matches!(document, DocumentEffect::Retain)
                    || !matches!(presence, PresenceHeadEffect::Retain)
                    || attribution.active_identity.is_some()
                    || attribution.origin.is_some()
                {
                    return Err(refused(
                        "retirement attempted document mutation or attribution transfer",
                    ));
                }
                let prior = before
                    .head()
                    .attribution
                    .as_ref()
                    .ok_or_else(|| refused("prior attribution absent"))?;
                let source = before
                    .head()
                    .source
                    .as_ref()
                    .ok_or_else(|| refused("prior source absent"))?;
                let identity = match &source.document {
                    DocumentState::Present {
                        external_identity, ..
                    } => external_identity.as_deref(),
                    DocumentState::Withdrawn => None,
                };
                let proof = self.budget.serialized(
                    &(
                        "catabolic.nfo.logical-retirement.v1",
                        before.source_proof_digest(),
                        before.attribution_audit_digest(),
                        self.outer.key(),
                        self.outer.digest().as_str(),
                        &plan,
                    ),
                    64,
                )?;
                let audit = before
                    .attribution_audit_digest()
                    .ok_or_else(|| refused("prior audit absent"))?;
                store.db.execute("INSERT INTO catalog_nfo_state_revisions(item_id,revision,source_revision,public_incarnation,public_revision,document_identity,active_identity,origin_json,origin_physical_json,origin_proof_digest,source_proof_digest,attribution_audit_digest,command_catalog_id,command_host_namespace,command_operation_id) VALUES(?,?,?,?,?,?,NULL,NULL,NULL,NULL,?,?,?,?,?)",
                    params![subject,attribution.revision,source.revision,before.presence().presence.incarnation,before.presence().presence.public_revision,identity,proof,audit,self.outer.key().catalog_id(),self.outer.key().host_namespace(),self.outer.key().operation_id()])?;
                if store.db.execute("UPDATE catalog_nfo_states SET current_revision=? WHERE item_id=? AND current_revision=?",params![attribution.revision,subject,prior.revision])? != 1 {
                    return Err(refused("retirement attribution CAS failed"));
                }
                if store.db.execute("DELETE FROM catalog_nfo_identity_claims WHERE item_id=? AND state_revision=?",params![subject,prior.revision])? != usize::from(prior.active_identity.is_some())
                    || store.db.execute("DELETE FROM catalog_nfo_file_attributions WHERE item_id=? AND state_revision=?",params![subject,prior.revision])? != usize::from(prior.origin.is_some())
                { return Err(refused("retirement prior indexes differ")); }
                written.insert(subject.clone(), (attribution.revision, source.revision));
            }
            expected.insert(subject.clone(), plan.final_snapshot);
        }
        let pending = PendingRetirementRead {
            outer: self.outer,
            generation: generation(store)?,
            subjects: self.before.keys().cloned().collect(),
            written,
        };
        for (subject, expected) in &expected {
            let actual = state::read_retirement_in(
                store,
                subject,
                pending.generation,
                expected.aggregate_revision,
                &pending,
            )?;
            let before = &self.before[subject];
            if actual.head() != &expected.head
                || actual.presence().presence != expected.presence
                || actual.native_source() != before.native_source()
                || actual.schema() != before.schema()
                || actual.origin_physical().is_some()
                    && expected
                        .head
                        .attribution
                        .as_ref()
                        .is_some_and(|a| a.origin.is_none())
            {
                return Err(refused(
                    "terminal retirement source/presence/index state differs",
                ));
            }
        }
        pending.validate_in(store, pending.generation, &mut self.budget)?;
        // The logical owner still holds and alone releases its barrier.
        Ok(())
    }
}
fn same_retained(a: &state::CurrentNfoState, b: &state::CurrentNfoState) -> bool {
    a.presence().presence == b.presence().presence
        && a.native_source() == b.native_source()
        && a.schema() == b.schema()
        && a.origin_physical() == b.origin_physical()
        && a.origin_proof_digest() == b.origin_proof_digest()
        && a.source_proof_digest() == b.source_proof_digest()
        && a.attribution_audit_digest() == b.attribution_audit_digest()
}
fn check_logical_barrier(store: &Store, outer: &CommandEnvelope) -> Result<()> {
    let valid: bool = store.db.query_row(
        "SELECT (SELECT count(*) FROM catalog_logical_edit_permits)=1 AND EXISTS(SELECT 1 FROM catalog_logical_edit_permits WHERE operation_id=? AND incarnation=? AND barrier_id=1) AND NOT EXISTS(SELECT 1 FROM catalog_commit_barrier WHERE id=1) AND NOT EXISTS(SELECT 1 FROM catalog_command_receipts WHERE catalog_id=? AND host_namespace=? AND operation_id=?)",
        params![outer.key().operation_id(),outer.admission_incarnation(),outer.key().catalog_id(),outer.key().host_namespace(),outer.key().operation_id()], |r|r.get(0))?;
    if !valid {
        return Err(refused("exact retained logical barrier/receipt differs"));
    }
    Ok(())
}
fn check_retirement(store: &Store, outer: &CommandEnvelope, subject: &str) -> Result<()> {
    let valid: bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id=? AND command_catalog_id=? AND command_host_namespace=? AND command_operation_id=?)",params![subject,outer.key().catalog_id(),outer.key().host_namespace(),outer.key().operation_id()],|r|r.get(0))?;
    if !valid {
        return Err(refused("retirement row is not owned by exact command"));
    }
    Ok(())
}
/// A distinct, nonserialized read token. It can bypass only its just-written
/// attribution rows, never metadata source/tag/bootstrap/pin receipt evidence.
pub(super) struct PendingRetirementRead {
    outer: CommandEnvelope,
    generation: i64,
    subjects: BTreeSet<String>,
    written: BTreeMap<String, (i64, i64)>,
}
impl PendingRetirementRead {
    pub(super) fn key(&self) -> &CommandKey {
        self.outer.key()
    }
    fn validate_in(
        &self,
        store: &Store,
        generation_expected: i64,
        budget: &mut ProofBudget,
    ) -> Result<()> {
        scope(store, &self.outer)?;
        if generation_expected != self.generation || generation(store)? != self.generation {
            return Err(refused("pending retirement generation differs"));
        }
        budget.serialized(&(&self.subjects, &self.written), 64)?;
        check_logical_barrier(store, &self.outer)?;
        let count:i64=store.db.query_row("SELECT count(*) FROM catalog_logical_retirements WHERE command_catalog_id=? AND command_host_namespace=? AND command_operation_id=?",params![self.outer.key().catalog_id(),self.outer.key().host_namespace(),self.outer.key().operation_id()],|r|r.get(0))?;
        if usize::try_from(count).ok() != Some(self.subjects.len()) {
            return Err(refused("pending retirement endpoint set differs"));
        }
        for subject in &self.subjects {
            check_retirement(store, &self.outer, subject)?;
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
        if self.written.get(subject) != Some(&(revision, source_revision)) {
            return Err(refused("pending retirement revision not written by owner"));
        }
        Ok(())
    }
}
