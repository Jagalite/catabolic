//! Terminal native observation evidence, not a filesystem or mutation grant.
//! Original selection and original-file provenance are independently audited by
//! the host; immutable request strings cannot manufacture either. Sidecar/root
//! custody is retained by a borrowed host callback for the whole batch lifetime.
use super::super::proof_budget::ProofBudget;
use crate::{
    Error, Result, Store,
    catalog_identity_mutation::{
        StructuralContext, StructuralReadBudget, load_structural_context_in,
        revalidate_structural_heads_in,
    },
    catalog_projection::{TrustedProjectionAuthority, VersionLineageRequest, version_lineage_in},
    operation_budget::BudgetCharge,
};
use catabolic_core::{
    catalog_command::CommandEnvelope,
    catalog_metadata::{
        MAX_METADATA_JSON_NODES, MAX_METADATA_SNAPSHOT_BYTES,
        nfo_refresh::{
            EligibilityDispositionV2, Fence, FileAvailability, FileKey, FileRevalidation,
            FileSnapshot, IdentityClaim, MissingEvidence, Observation, RefreshRequestV2,
            RefreshSnapshotV2, RevalidatedFile, SourceState, SubjectHead, classify_refresh_v2,
        },
    },
};
use rusqlite::{OptionalExtension, params, types::ValueRef};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// The embedding host retains its Motion writer and exact sidecar descriptors
/// (or independently checked absence/unusable evidence) behind this callback.
/// The embedding host independently validates Motion lifecycle/enabled state,
/// curation and complete-original-selection facts under its retained writer.
/// The callback retains that immutable audit and exclusive writer custody; it
/// must not perform nested asynchronous SQL or acquire a second connection.
/// Wall-clock credential/signin-family expiry is not frozen by the writer and
/// must be rechecked alongside filesystem custody. Native policy/head checks
/// below are not proof of those host facts (including Motion93 lifecycle).
/// A digest is never a substitute for calling it again before barrier release.
pub(super) trait RetainedRefreshCustody {
    fn revalidate(&self) -> Result<()>;
}
impl<F: Fn() -> Result<()>> RetainedRefreshCustody for F {
    fn revalidate(&self) -> Result<()> {
        self()
    }
}

pub(super) struct RefreshObservationAudit<'a> {
    pub catalog_id: String,
    pub incarnation: String,
    pub generation: i64,
    pub host_namespace: String,
    pub subject: String,
    pub outer_digest: String,
    pub request_digest: String,
    pub complete_original_selection_digest: String,
    pub observations: Vec<ObservationAudit>,
    pub custody: &'a dyn RetainedRefreshCustody,
}

pub(super) struct ObservationAudit {
    pub observed_file: FileSnapshot,
    pub original_lineage: VersionLineageRequest,
    /// Positive audited original (not generated) file provenance. No inference
    /// from absent output markers or from current projection JSON is permitted.
    pub original_file_provenance_digest: String,
    pub observation_digest: String,
    pub terminal: TerminalObservationAudit,
}

pub(super) enum TerminalObservationAudit {
    Present {
        selection: CurrentFileSelection,
        current: FileSnapshot,
        availability: FileAvailability,
        source_state: SourceState,
        native_binding: Value,
        root_sidecar_audit_digest: String,
    },
    Missing {
        complete_lookup_digest: String,
    },
    Unproven {
        reason: MissingEvidence,
    },
}

/// Exact named structural selector, not permission to read any source. The host
/// independently establishes this candidate and the native owner verifies it.
pub(super) struct CurrentFileSelection {
    pub file: FileKey,
    pub order_key: String,
    pub lineage: VersionLineageRequest,
    pub binding_id: String,
    pub association_id: String,
}

pub(super) struct CapturedCurrentObservation {
    current: FileSnapshot,
    availability: FileAvailability,
    native_binding: Value,
    occurrence: Value,
    binding_file_revision: String,
    full_proof_digest: String,
    work_item_revision: i64,
    structure: Arc<InitialStructure>,
    physical_heads: Vec<(&'static str, Value, Option<(i64, bool)>)>,
}
impl CapturedCurrentObservation {
    pub(super) fn current(&self) -> &FileSnapshot {
        &self.current
    }
    pub(super) fn availability(&self) -> &FileAvailability {
        &self.availability
    }
    pub(super) fn native_binding(&self) -> &Value {
        &self.native_binding
    }
}

pub(super) struct VerifiedRefreshObservations<'a> {
    audit: &'a RefreshObservationAudit<'a>,
    files: Vec<RevalidatedFile>,
    captures: Vec<Option<CapturedCurrentObservation>>,
    initial_work_heads: BTreeMap<String, i64>,
    live_files: BTreeSet<FileKey>,
    proof_digest: String,
}
impl VerifiedRefreshObservations<'_> {
    pub(super) fn revalidated_files(&self) -> &[RevalidatedFile] {
        &self.files
    }
    pub(super) fn proof_digest(&self) -> &str {
        &self.proof_digest
    }

    /// Same retained writer, current actual generation. No synthetic envelope or
    /// renewed external grant is issued. Only work-item metadata revision changes
    /// are excluded from structural fingerprint comparison; every identity edge,
    /// binding, edition/version/context/timeline fact remains exact. The batch
    /// owner admits metadata effects only, never native structural mutations.
    pub(super) fn revalidate_custody_in(
        &self,
        store: &Store,
        expected_work_item_revisions: &BTreeMap<String, i64>,
        budget: &mut ProofBudget,
    ) -> Result<()> {
        budget.serialized(expected_work_item_revisions, 64)?;
        if expected_work_item_revisions
            .values()
            .any(|revision| *revision < 1)
        {
            return Err(refused("invalid actual metadata outcome head"));
        }
        let scope = current_fence(
            store,
            &self.audit.catalog_id,
            &self.audit.incarnation,
            budget,
        )?;
        if scope.generation < self.audit.generation {
            return Err(refused("generation moved backwards"));
        }
        self.audit.custody.revalidate()?;
        for (index, audit) in self.audit.observations.iter().enumerate() {
            match &audit.terminal {
                TerminalObservationAudit::Present { selection, .. } => {
                    let captured = self.captures[index]
                        .as_ref()
                        .ok_or_else(|| refused("retained initial native capture missing"))?;
                    let expected_work_head = expected_work_item_revisions
                        .get(&selection.lineage.work_item_id)
                        .or_else(|| self.initial_work_heads.get(&selection.lineage.work_item_id))
                        .copied()
                        .ok_or_else(|| refused("retained initial work head missing"))?;
                    if entity_head(
                        store,
                        "items",
                        &json!([selection.lineage.work_item_id]),
                        true,
                        budget,
                    )? != Some((expected_work_head, false))
                    {
                        return Err(refused(
                            "terminal native occurrence/structural/source facts changed",
                        ));
                    }
                    charge(budget, 0, 1)?;
                    let retired: bool = store.db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id=?)",
                        [&selection.lineage.work_item_id],
                        |r| r.get(0),
                    )?;
                    if retired {
                        return Err(refused("terminal observed work retired"));
                    }
                    revalidate_structural_heads_in(
                        store,
                        captured.structure.context.context_id(),
                        captured.structure.context.heads(),
                        budget,
                    )?;
                    for (kind, key, expected) in &captured.physical_heads {
                        // Absence and tombstones are exact independent facts too;
                        // a new row or away-and-back mutation cannot match them.
                        let present = expected.is_some_and(|(_, deleted)| !deleted);
                        if entity_head(store, kind, key, present, budget)? != *expected {
                            return Err(refused("terminal physical/source/policy head changed"));
                        }
                    }
                    require_original_consistency(
                        store,
                        selection,
                        &captured.current.source_id,
                        budget,
                    )?;
                    if self.live_files.contains(&selection.file) {
                        validate_live(store, captured, budget)?;
                    }
                }
                TerminalObservationAudit::Missing { .. } => {
                    require_missing(store, &audit.observed_file.file, budget)?
                }
                TerminalObservationAudit::Unproven { .. } => {
                    return Err(refused("unproven terminal token"));
                }
            }
        }
        self.audit.custody.revalidate()?;
        let after = current_fence(
            store,
            &self.audit.catalog_id,
            &self.audit.incarnation,
            budget,
        )?;
        if after != scope {
            return Err(refused(
                "native fence changed during terminal custody validation",
            ));
        }
        Ok(())
    }
}

pub(super) fn verify_in<'a>(
    store: &Store,
    outer: &CommandEnvelope,
    request: &RefreshRequestV2,
    audit: &'a RefreshObservationAudit<'a>,
    subjects: &BTreeMap<String, SubjectHead>,
    identity_claims: &BTreeMap<String, IdentityClaim>,
    budget: &mut ProofBudget,
) -> Result<VerifiedRefreshObservations<'a>> {
    let scope = current_fence(store, &audit.catalog_id, &audit.incarnation, budget)?;
    if scope != request.fence
        || scope.generation != audit.generation
        || outer.key().catalog_id() != scope.catalog_id
        || outer.admission_incarnation() != scope.incarnation
        || outer.expected_generation() != scope.generation
        || outer.key().host_namespace() != audit.host_namespace
        || audit.outer_digest != outer.digest().as_str()
        || audit.subject != request.subject
        || audit.request_digest != budget.serialized(request, 64)?
        || !digest(&audit.complete_original_selection_digest)
        || audit.observations.len() != request.observations.len()
        || audit.observations.len() > MAX_METADATA_JSON_NODES
    {
        return Err(refused(
            "complete original selection/command audit scope differs",
        ));
    }
    budget.serialized(
        &(
            &audit.catalog_id,
            &audit.incarnation,
            audit.generation,
            &audit.host_namespace,
            &audit.subject,
            &audit.outer_digest,
            &audit.request_digest,
            &audit.complete_original_selection_digest,
        ),
        64,
    )?;
    audit.custody.revalidate()?;
    let mut files = Vec::with_capacity(audit.observations.len());
    let mut captures = Vec::with_capacity(audit.observations.len());
    let mut proof_parts = Vec::with_capacity(audit.observations.len());
    let mut initial_work_heads = BTreeMap::new();
    let mut pass = InitialStructurePass::new(store, &scope, budget)?;
    for (audit, observation) in audit.observations.iter().zip(&request.observations) {
        validate_original_audit(audit, observation, budget)?;
        let (current, captured, proof) = match &audit.terminal {
            TerminalObservationAudit::Unproven { reason } => {
                return Err(refused(format!("unproven observation: {reason:?}")));
            }
            TerminalObservationAudit::Missing {
                complete_lookup_digest,
            } => {
                if !digest(complete_lookup_digest) {
                    return Err(refused("missing complete lookup audit"));
                }
                require_missing(store, &audit.observed_file.file, budget)?;
                let proof = budget.serialized(
                    &(
                        "complete_missing",
                        &audit.observed_file.file,
                        complete_lookup_digest,
                    ),
                    64,
                )?;
                (
                    FileRevalidation::Missing {
                        lookup_digest: complete_lookup_digest.clone(),
                    },
                    None,
                    proof,
                )
            }
            TerminalObservationAudit::Present {
                selection,
                current,
                availability,
                source_state,
                native_binding,
                root_sidecar_audit_digest,
            } => {
                if selection.file != audit.observed_file.file
                    || selection.order_key != audit.observed_file.order_key
                    || !digest(root_sidecar_audit_digest)
                {
                    return Err(refused("current observation selector/custody differs"));
                }
                budget.serialized(
                    &(
                        current,
                        availability,
                        source_state,
                        native_binding,
                        root_sidecar_audit_digest,
                    ),
                    64,
                )?;
                let captured = capture_current_with_pass(&mut pass, selection, budget)?;
                if captured.current != *current
                    || captured.availability != *availability
                    || captured.native_binding != *native_binding
                {
                    return Err(refused(
                        "audited current observation differs from native owner facts",
                    ));
                }
                budget.serialized(
                    &(&selection.lineage.work_item_id, captured.work_item_revision),
                    64,
                )?;
                if initial_work_heads
                    .insert(
                        selection.lineage.work_item_id.clone(),
                        captured.work_item_revision,
                    )
                    .is_some_and(|old| old != captured.work_item_revision)
                {
                    return Err(refused("inconsistent exact initial work heads"));
                }
                let proof = budget.serialized(
                    &(
                        &captured.full_proof_digest,
                        root_sidecar_audit_digest,
                        &audit.original_file_provenance_digest,
                        &audit.observation_digest,
                    ),
                    64,
                )?;
                (
                    FileRevalidation::Present {
                        current: current.clone(),
                        availability: availability.clone(),
                        source_state: source_state.clone(),
                    },
                    Some(captured),
                    proof,
                )
            }
        };
        let file = RevalidatedFile {
            observed_file: audit.observed_file.clone(),
            current,
        };
        budget.serialized(&file, 64)?;
        files.push(file);
        captures.push(captured);
        proof_parts.push(proof);
    }
    // No cached structural read escapes this callback-free read-only pass.
    pass.check(budget)?;
    drop(pass);
    // Use the same complete pure validation and eligibility rules as planning.
    // No local approximation of stale/disabled/unusable/pin ordering is accepted.
    budget.serialized(
        &(&scope, &request.subject, &files, subjects, identity_claims),
        64,
    )?;
    let snapshot = RefreshSnapshotV2 {
        contract_version: request.contract_version,
        fence: scope.clone(),
        subject: request.subject.clone(),
        files: files.clone(),
        subjects: subjects.clone(),
        identity_claims: identity_claims.clone(),
    };
    let classified = classify_refresh_v2(&snapshot, request).map_err(refused)?;
    let mut live_files = BTreeSet::new();
    for (classification, capture) in classified.iter().zip(&captures) {
        if matches!(
            classification.disposition,
            EligibilityDispositionV2::Parsed | EligibilityDispositionV2::Absent
        ) {
            let captured = capture
                .as_ref()
                .ok_or_else(|| refused("eligible observation lacks native capture"))?;
            validate_live(store, captured, budget)?;
            budget.serialized(&classification.file, 64)?;
            live_files.insert(classification.file.clone());
        }
    }
    audit.custody.revalidate()?;
    if current_fence(store, &audit.catalog_id, &audit.incarnation, budget)? != scope {
        return Err(refused(
            "native fence changed during observation validation",
        ));
    }
    let proof_digest = budget.serialized(
        &(
            "native_nfo_terminal_observations_v1",
            &scope,
            outer.digest(),
            &audit.request_digest,
            &audit.complete_original_selection_digest,
            &proof_parts,
            &files,
            &live_files,
        ),
        64,
    )?;
    Ok(VerifiedRefreshObservations {
        audit,
        files,
        captures,
        initial_work_heads,
        live_files,
        proof_digest,
    })
}

fn validate_original_audit(
    audit: &ObservationAudit,
    observation: &Observation,
    budget: &mut ProofBudget,
) -> Result<()> {
    validate_lineage_request(&audit.original_lineage)?;
    if audit.observed_file != observation.expected_file
        || audit.original_lineage.work_item_id != audit.observed_file.subject
        || !digest(&audit.original_file_provenance_digest)
        || audit.observation_digest != budget.serialized(observation, 64)?
    {
        return Err(refused("original file/sidecar/provenance audit differs"));
    }
    budget.serialized(
        &(
            &audit.observed_file,
            lineage_ids(&audit.original_lineage),
            &audit.original_file_provenance_digest,
            &audit.observation_digest,
        ),
        64,
    )?;
    Ok(())
}

fn refused(reason: impl std::fmt::Display) -> Error {
    Error(format!("catalog_nfo_observation_refused: {reason}"))
}
fn identifier(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && s.trim() == s && !s.chars().any(char::is_control)
}
fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn charge(budget: &mut ProofBudget, bytes: usize, values: usize) -> Result<()> {
    budget.charge(BudgetCharge {
        bytes,
        values,
        work: bytes / 64 + values + 1,
        ..Default::default()
    })
}
fn lineage_ids(request: &VersionLineageRequest) -> [&str; 4] {
    [
        &request.work_item_id,
        &request.edition_item_id,
        &request.version_item_id,
        &request.version_id,
    ]
}
fn validate_lineage_request(request: &VersionLineageRequest) -> Result<()> {
    if lineage_ids(request).iter().any(|id| !identifier(id)) {
        return Err(refused("invalid exact lineage selector"));
    }
    Ok(())
}
fn current_fence(
    store: &Store,
    catalog: &str,
    incarnation: &str,
    budget: &mut ProofBudget,
) -> Result<Fence> {
    if store.schema_version != 41
        || !store.writable
        || store.db.is_autocommit()
        || !identifier(catalog)
        || !identifier(incarnation)
    {
        return Err(refused("schema41 retained writer required"));
    }
    store.transaction_available()?;
    charge(budget, catalog.len() + incarnation.len(), 3)?;
    let (matches, generation): (bool, i64) = store.db.query_row(
        "SELECT catalog_id=?1 AND incarnation=?2,generation FROM catalog_state WHERE id=1",
        params![catalog, incarnation],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if !matches
        || catalog != store.database_id
        || Some(incarnation) != store.catalog_incarnation.as_deref()
        || generation < 0
    {
        return Err(refused("catalog/incarnation scope differs"));
    }
    Ok(Fence {
        catalog_id: catalog.into(),
        incarnation: incarnation.into(),
        generation,
    })
}
fn require_missing(store: &Store, file: &FileKey, budget: &mut ProofBudget) -> Result<()> {
    if !identifier(&file.profile) || !identifier(&file.file_id) {
        return Err(refused("invalid missing-file selector"));
    }
    charge(budget, 0, 1)?;
    let exists: bool = store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM files WHERE id=?)",
        [&file.file_id],
        |r| r.get(0),
    )?;
    if exists {
        return Err(refused(
            "complete missing lookup contradicts retained native file",
        ));
    }
    Ok(())
}

impl StructuralReadBudget for ProofBudget {
    #[cfg(test)]
    fn trace_phase(&self, phase: &str) {
        ProofBudget::trace_phase(self, phase);
    }
    fn charge(&mut self, charge: BudgetCharge) -> Result<()> {
        ProofBudget::charge(self, charge)
    }
    fn raw_json(&mut self, raw: &str) -> Result<()> {
        ProofBudget::raw_json(self, raw, 64)
    }
}

/// Reusable native capture definition used both when constructing the trusted
/// original observation and when verifying its terminal successor. It does not
/// attest original/generated provenance or establish sidecar/root custody.
// Private owned materialization, never a caller-supplied commitment or grant.
// Reuse is restricted to one retained, callback-free read-only verification pass.
struct InitialStructure {
    context: StructuralContext,
    structural_lineage: Value,
    full_lineage_digest: String,
    work_item_revision: i64,
}
struct InitialStructurePass<'a> {
    store: &'a Store,
    scope: &'a Fence,
    changes: u64,
    schema_cookie: i64,
    records: BTreeMap<(String, VersionLineageRequest), Arc<InitialStructure>>,
}
impl<'a> InitialStructurePass<'a> {
    fn new(store: &'a Store, scope: &'a Fence, budget: &mut ProofBudget) -> Result<Self> {
        charge(budget, 0, 2)?;
        if store.db.is_autocommit() {
            return Err(refused("initial structural pass requires retained writer"));
        }
        let schema_cookie = store
            .db
            .query_row("PRAGMA schema_version", [], |r| r.get(0))?;
        Ok(Self {
            store,
            scope,
            changes: store.db.total_changes(),
            schema_cookie,
            records: BTreeMap::new(),
        })
    }
    fn check(&self, budget: &mut ProofBudget) -> Result<()> {
        charge(budget, 0, 3)?;
        self.store.transaction_available()?;
        let cookie: i64 = self
            .store
            .db
            .query_row("PRAGMA schema_version", [], |r| r.get(0))?;
        if self.store.db.is_autocommit()
            || self.store.db.total_changes() != self.changes
            || cookie != self.schema_cookie
        {
            return Err(refused(
                "initial structural pass changed during read-only capture",
            ));
        }
        Ok(())
    }
    fn capture(
        &mut self,
        selection: &CurrentFileSelection,
        budget: &mut ProofBudget,
    ) -> Result<Arc<InitialStructure>> {
        self.check(budget)?;
        // Admission precedes owned lookup-key allocation and ordered comparisons.
        budget.serialized(
            &(&selection.file.profile, lineage_ids(&selection.lineage)),
            64,
        )?;
        charge(
            budget,
            std::mem::size_of::<(String, VersionLineageRequest)>(),
            self.records.len().saturating_add(1),
        )?;
        let key = (selection.file.profile.clone(), selection.lineage.clone());
        if let Some(record) = self.records.get(&key) {
            // Resolve the native context independently on every use. Full selector
            // equality plus unchanged transaction binds the retained complete heads.
            charge(budget, 0, 2)?;
            let mut statement = self
                .store
                .db
                .prepare("SELECT segmentation_context_id FROM catalog_live_versions WHERE id=?")?;
            let mut rows = statement.query([&selection.lineage.version_id])?;
            let row = rows
                .next()?
                .ok_or_else(|| refused("cached structural version missing"))?;
            let context = row
                .get_ref(0)?
                .as_str()
                .map_err(|_| refused("cached structural context is not text"))?;
            charge(budget, context.len(), 1)?;
            if context != record.context.context_id() || rows.next()?.is_some() {
                return Err(refused("cached structural context differs"));
            }
            self.check(budget)?;
            charge(budget, std::mem::size_of::<Arc<InitialStructure>>(), 1)?;
            return Ok(Arc::clone(record));
        }
        let record = load_initial_structure(self.store, self.scope, selection, budget)?;
        self.check(budget)?;
        charge(
            budget,
            std::mem::size_of::<InitialStructure>()
                + std::mem::size_of::<Arc<InitialStructure>>() * 2,
            2,
        )?;
        let record = Arc::new(record);
        self.records.insert(key, Arc::clone(&record));
        Ok(record)
    }
}
fn load_initial_structure(
    store: &Store,
    scope: &Fence,
    selection: &CurrentFileSelection,
    budget: &mut ProofBudget,
) -> Result<InitialStructure> {
    precharge_lineage(store, &selection.lineage, budget)?;
    let authority = TrustedProjectionAuthority {
        catalog_id: scope.catalog_id.clone(),
        incarnation: scope.incarnation.clone(),
        expected_generation: scope.generation,
    };
    let lineage = version_lineage_in(store, &authority, std::slice::from_ref(&selection.lineage))?;
    budget.serialized(&lineage, 64)?;
    let rows = lineage["rows"]
        .as_array()
        .filter(|rows| rows.len() == 1)
        .ok_or_else(|| refused("complete named lineage missing"))?;
    if rows[0]["status"] != "active" {
        return Err(refused("current lineage is retired, detached or ambiguous"));
    }
    let work_item_revision = rows[0]["owners"]
        .as_array()
        .and_then(|owners| {
            owners
                .iter()
                .find(|owner| owner["id"] == selection.lineage.work_item_id)
        })
        .and_then(|owner| owner["item_revision"].as_i64())
        .filter(|revision| *revision > 0)
        .ok_or_else(|| refused("exact initial work item head missing"))?;
    let full_lineage_digest = budget.serialized(&rows[0], 64)?;
    // Work metadata is the only item row this batch may change. Keep every
    // structural identity/head, including edition/version item revisions, exact.
    budget.serialized(&rows[0], 64)?;
    let mut structural_lineage = rows[0].clone();
    let owners = structural_lineage["owners"]
        .as_array_mut()
        .ok_or_else(|| refused("lineage owner set missing"))?;
    for owner in owners {
        if owner["id"] == selection.lineage.work_item_id {
            owner
                .as_object_mut()
                .ok_or_else(|| refused("lineage owner object missing"))?
                .remove("item_revision");
        }
    }
    let context = load_structural_context_in(
        store,
        &selection.file.profile,
        &selection.lineage.version_id,
        budget,
    )?;
    budget.serialized(
        &(
            context.context_id(),
            context.versions(),
            context.segmentation_sets(),
            context.heads(),
        ),
        64,
    )?;
    Ok(InitialStructure {
        context,
        structural_lineage,
        full_lineage_digest,
        work_item_revision,
    })
}

pub(super) fn capture_current_in(
    store: &Store,
    scope: &Fence,
    selection: &CurrentFileSelection,
    budget: &mut ProofBudget,
) -> Result<CapturedCurrentObservation> {
    let mut pass = InitialStructurePass::new(store, scope, budget)?;
    capture_current_with_pass(&mut pass, selection, budget)
}

fn capture_current_with_pass(
    pass: &mut InitialStructurePass<'_>,
    selection: &CurrentFileSelection,
    budget: &mut ProofBudget,
) -> Result<CapturedCurrentObservation> {
    let store = pass.store;
    let scope = pass.scope;
    validate_lineage_request(&selection.lineage)?;
    if [
        &selection.file.profile,
        &selection.file.file_id,
        &selection.order_key,
        &selection.binding_id,
        &selection.association_id,
    ]
    .iter()
    .any(|id| !identifier(id))
    {
        return Err(refused("invalid current exact binding selector"));
    }
    if current_fence(store, &scope.catalog_id, &scope.incarnation, budget)? != *scope {
        return Err(refused("capture fence differs"));
    }
    budget.serialized(
        &(
            lineage_ids(&selection.lineage),
            &selection.file,
            &selection.order_key,
            &selection.binding_id,
            &selection.association_id,
        ),
        64,
    )?;
    let structure = pass.capture(selection, budget)?;
    let context = &structure.context;
    let structural_lineage = &structure.structural_lineage;
    let full_lineage_digest = &structure.full_lineage_digest;
    let work_item_revision = structure.work_item_revision;
    let version = context
        .versions()
        .iter()
        .find(|v| v.version.version_id == selection.lineage.version_id)
        .ok_or_else(|| refused("selected structural version missing"))?;
    if version.version.association_item_id != selection.lineage.version_item_id
        || version.version.edition_item_id != selection.lineage.edition_item_id
    {
        return Err(refused("selected structural owners differ"));
    }
    let binding = version
        .version
        .bindings
        .iter()
        .find(|b| b.binding_id == selection.binding_id)
        .ok_or_else(|| refused("selected structural binding missing"))?;
    if binding.association_id != selection.association_id
        || binding.file.file_id != selection.file.file_id
    {
        return Err(refused("selected exact association/file differs"));
    }
    charge(budget, 0, 6)?;
    let (binding_revision,version_revision,context_revision):(i64,i64,i64)=store.db.query_row(
        "SELECT b.binding_revision,b.version_revision,c.current_revision FROM catalog_active_binding_revisions b JOIN catalog_segmentation_contexts c ON c.id=b.segmentation_context_id WHERE b.version_id=?1 AND b.binding_id=?2 AND b.association_id=?3 AND b.profile=?4 AND b.file_id=?5",
        params![selection.lineage.version_id,selection.binding_id,selection.association_id,selection.file.profile,selection.file.file_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    if binding_revision < 1 || version_revision < 1 || context_revision < 1 {
        return Err(refused("invalid binding/version/context head"));
    }
    let mut heads = Vec::new();
    for (kind, key) in [
        ("catalog_bindings", json!([selection.binding_id])),
        (
            "catalog_binding_revisions",
            json!([selection.binding_id, binding_revision]),
        ),
        (
            "catalog_version_bindings",
            json!([
                selection.lineage.version_id,
                version_revision,
                selection.binding_id
            ]),
        ),
        (
            "catalog_segmentation_contexts",
            json!([context.context_id()]),
        ),
        (
            "catalog_segmentation_revisions",
            json!([context.context_id(), context_revision]),
        ),
        ("item_files", json!([selection.association_id])),
    ] {
        heads.push((kind, entity_head(store, kind, &key, true, budget)?));
    }
    let structural_digest = budget.serialized(
        &(
            "nfo_file_ownership_v1",
            &structural_lineage,
            context.context_id(),
            context.versions(),
            context.segmentation_sets(),
            context.heads(),
            &heads,
            binding_revision,
        ),
        64,
    )?;
    precharge_occurrence(store, &selection.file, budget)?;
    let occurrence =
        crate::components::occurrence(&store.db, &selection.file.profile, &selection.file.file_id)?;
    budget.serialized(&occurrence, 64)?;
    let source = occurrence["location"]
        .as_str()
        .filter(|id| identifier(id))
        .ok_or_else(|| refused("native source identity missing"))?;
    let path = occurrence["path"]
        .as_str()
        .ok_or_else(|| refused("native source path missing"))?;
    if path.is_empty() || path.len() > 4096 || crate::layout::relative_path(path).is_err() {
        return Err(refused("native relative path bound"));
    }
    let availability = match occurrence["status"].as_str() {
        Some("present") => FileAvailability::Available,
        Some("missing") => FileAvailability::Unavailable,
        _ => return Err(refused("current native availability is unproven")),
    };
    require_original_consistency(store, selection, source, budget)?;
    let native_binding =
        crate::observation::binding(store, &selection.file.profile, "source", source)?;
    budget.serialized(&native_binding, 64)?;
    let observation_policy = raw_policy(store, &selection.file.profile, source, budget)?;
    charge(budget, 0, 1)?;
    let has_ctime: bool = store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM native_source_ctimes WHERE profile=? AND file_id=?)",
        params![selection.file.profile, selection.file.file_id],
        |r| r.get(0),
    )?;
    let mut physical_heads = Vec::new();
    for (kind, key, present) in [
        ("files", json!([selection.file.file_id]), true),
        (
            "observations",
            json!([selection.file.profile, selection.file.file_id]),
            true,
        ),
        (
            "native_source_ctimes",
            json!([selection.file.profile, selection.file.file_id]),
            has_ctime,
        ),
        (
            "bindings",
            json!([selection.file.profile, "source", source]),
            true,
        ),
        (
            "source_identity_policies",
            json!([selection.file.profile, source]),
            occurrence["source_policy"]["revision"]
                .as_i64()
                .is_some_and(|v| v > 0),
        ),
        (
            "source_observation_policies",
            json!([selection.file.profile, source]),
            observation_policy.is_some(),
        ),
        (
            "binding_volumes",
            json!([selection.file.profile, "source", source]),
            !occurrence["volume_uuid"].is_null(),
        ),
    ] {
        let head = entity_head(store, kind, &key, present, budget)?;
        physical_heads.push((kind, key, head));
    }
    let binding_fingerprint = budget.serialized(
        &(
            "nfo_source_binding_v1",
            &selection.file.profile,
            source,
            &native_binding,
            &occurrence["source_policy"],
            &occurrence["volume_uuid"],
            &observation_policy,
            &physical_heads,
        ),
        64,
    )?;
    // Components revision excludes ctime by its established contract. Keep that
    // contract unchanged; live publication separately verifies exact native ctime.
    budget.canonical_json(&occurrence)?;
    let exact_revision = crate::components::revision(&occurrence);
    let current = FileSnapshot {
        order_key: selection.order_key.clone(),
        file: selection.file.clone(),
        subject: selection.lineage.work_item_id.clone(),
        source_id: source.into(),
        relative_path: path.into(),
        exact_revision,
        binding_fingerprint,
        ownership_fingerprint: structural_digest.clone(),
    };
    budget.serialized(&current, 64)?;
    budget.serialized(&binding.file.revision, 64)?;
    let binding_file_revision = binding.file.revision.clone();
    let full_proof_digest = budget.serialized(
        &(
            "nfo_native_capture_v1",
            scope,
            &full_lineage_digest,
            &structural_digest,
            &current,
            &binding_file_revision,
            &observation_policy,
            &physical_heads,
        ),
        64,
    )?;
    pass.check(budget)?;
    Ok(CapturedCurrentObservation {
        current,
        availability,
        native_binding,
        occurrence,
        binding_file_revision,
        full_proof_digest,
        work_item_revision,
        structure,
        physical_heads,
    })
}

fn entity_head(
    store: &Store,
    kind: &str,
    key: &Value,
    present: bool,
    budget: &mut ProofBudget,
) -> Result<Option<(i64, bool)>> {
    budget.serialized(key, 64)?;
    let raw = serde_json::to_string(key)?;
    charge(budget, 0, 2)?;
    let head:Option<(i64,bool)>=store.db.query_row("SELECT revision,deleted FROM catalog_entity_revisions WHERE entity_kind=? AND entity_key=?",params![kind,raw],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if head.is_some_and(|(revision, deleted)| revision < 1 || deleted == present)
        || (present && head.is_none())
    {
        return Err(refused(format!(
            "exact permanent {kind} head missing/inconsistent"
        )));
    }
    Ok(head)
}

fn validate_live(
    store: &Store,
    captured: &CapturedCurrentObservation,
    budget: &mut ProofBudget,
) -> Result<()> {
    if captured.availability != FileAvailability::Available
        || captured.binding_file_revision != captured.current.exact_revision
    {
        return Err(refused(
            "publication occurrence differs from selected typed binding",
        ));
    }
    let file = &captured.current.file;
    charge(budget, 0, 2)?;
    let ctime:Option<i64>=store.db.query_row("SELECT c.ctime_ns FROM native_source_ctimes c JOIN observations o ON o.profile=c.profile AND o.file_id=c.file_id AND o.size=c.size AND o.mtime_ns=c.mtime_ns AND o.device=c.device AND o.inode=c.inode WHERE c.profile=? AND c.file_id=? AND o.status='present'",params![file.profile,file.file_id],|r|r.get(0)).optional()?;
    let ctime = ctime.ok_or_else(|| refused("selected exact ctime proof missing"))?;
    entity_head(
        store,
        "native_source_ctimes",
        &json!([file.profile, file.file_id]),
        true,
        budget,
    )?;
    admit_live_validation(&captured.occurrence, ctime, budget)?;
    crate::source::validate_expected_ctime(&captured.occurrence, ctime)
}

/// Admit the actual borrowed validator input and its internal allocations before
/// filesystem use. The complete occurrence was already admitted when captured;
/// terminal validation neither clones it nor constructs a returned stat object.
fn admit_live_validation(snapshot: &Value, ctime: i64, budget: &mut ProofBudget) -> Result<()> {
    let settings = &snapshot["source_policy"]["settings"];
    let inputs = [
        &snapshot["status"],
        &snapshot["root"],
        &snapshot["path"],
        &snapshot["root_device"],
        &snapshot["root_inode"],
        &snapshot["volume_uuid"],
        &snapshot["size"],
        &snapshot["mtime_ns"],
        &snapshot["device"],
        &snapshot["inode"],
        &settings["device"],
        &settings["inode"],
        &settings["uuid"],
    ];
    budget.serialized(&(inputs, ctime), 64)?;
    // Linux's legacy UUID probe collects child stdout before its size check.
    // Until a separately owned bounded probe exists, this candidate refuses
    // before filesystem I/O rather than skipping required UUID validation.
    #[cfg(target_os = "linux")]
    if settings["uuid"] != "skip" && !snapshot["volume_uuid"].is_null() {
        return Err(refused("bounded terminal volume UUID probe unavailable"));
    }
    #[cfg(target_os = "macos")]
    if settings["uuid"] != "skip" && !snapshot["volume_uuid"].is_null() {
        charge(budget, 2 * 36, 2)?;
    }
    let root = snapshot["root"].as_str().unwrap_or("");
    let path = snapshot["path"].as_str().unwrap_or("");
    let roots = root.split('/').filter(|part| !part.is_empty()).count();
    let parts = path.split('/').count();
    let overflow = || refused("live validation allocation bound overflow");
    // Two complete root/path walks allocate each CString including its NUL;
    // the path-component vector retains borrowed &str pairs, not path copies.
    let strings = root
        .len()
        .checked_add(path.len())
        .and_then(|n| n.checked_add(roots))
        .and_then(|n| n.checked_add(parts))
        .and_then(|n| n.checked_mul(2))
        .ok_or_else(overflow)?;
    let vector = parts
        .checked_mul(std::mem::size_of::<&str>())
        .ok_or_else(overflow)?;
    // Three internal revision objects (before, held, named), each five integer
    // fields. 40 bytes bounds any signed i128 token; 64 bounds all five keys,
    // punctuation and braces. No fourth operation-result object is created.
    let revisions = 3 * (5 * 40 + 64);
    let bytes = strings
        .checked_add(vector)
        .and_then(|n| n.checked_add(revisions))
        .ok_or_else(overflow)?;
    let walks = roots
        .checked_add(parts)
        .and_then(|n| n.checked_mul(2))
        .ok_or_else(overflow)?;
    // Account all fixed descriptor/metadata/identity checks in addition to each
    // component open and the 33 JSON scalar/container nodes retained internally.
    let work = walks
        .checked_add(20 + 33)
        .and_then(|n| n.checked_add(bytes / 64))
        .ok_or_else(overflow)?;
    budget.charge(BudgetCharge {
        bytes,
        values: 33,
        work,
        ..Default::default()
    })
}

fn require_original_consistency(
    store: &Store,
    selection: &CurrentFileSelection,
    source: &str,
    budget: &mut ProofBudget,
) -> Result<()> {
    charge(budget, 0, 1)?;
    let generated:bool=store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM generated_locations WHERE profile=?1 AND location=?2) OR EXISTS(SELECT 1 FROM processing_artifacts WHERE profile=?1 AND file_id=?3) OR EXISTS(SELECT 1 FROM items WHERE id=?4 AND json_type(metadata,'$.motion_generated_output') IS NOT NULL)",
        params![selection.file.profile,source,selection.file.file_id,selection.lineage.version_item_id],|r|r.get(0))?;
    if generated {
        return Err(refused(
            "native generated provenance contradicts original-file audit",
        ));
    }
    Ok(())
}

fn raw_policy(
    store: &Store,
    profile: &str,
    source: &str,
    budget: &mut ProofBudget,
) -> Result<Option<(i64, String)>> {
    let mut query = store.db.prepare(
        "SELECT revision,policy FROM source_observation_policies WHERE profile=? AND source=?",
    )?;
    let mut rows = query.query(params![profile, source])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let revision: i64 = row.get(0)?;
    let raw = row
        .get_ref(1)?
        .as_str()
        .map_err(|_| refused("stored observation policy is not text"))?;
    if revision < 1 {
        return Err(refused("stored observation policy revision invalid"));
    }
    budget.raw_json(raw, 64)?;
    charge(budget, raw.len(), 2)?;
    Ok(Some((revision, raw.into())))
}

fn admit_sql_rows(budget: &mut ProofBudget, count: i64, bytes: i64, max_rows: i64) -> Result<()> {
    if count < 0 || count > max_rows || bytes < 0 {
        return Err(refused("native complete row bound"));
    }
    let values = usize::try_from(count)
        .ok()
        .and_then(|n| n.checked_mul(32))
        .ok_or_else(|| refused("native row count overflow"))?;
    let bytes = usize::try_from(bytes)
        .ok()
        .and_then(|n| n.checked_mul(6))
        .and_then(|n| n.checked_add(usize::try_from(count).ok()?.checked_mul(1024)?))
        .filter(|n| *n <= MAX_METADATA_SNAPSHOT_BYTES)
        .ok_or_else(|| refused("native complete row byte bound"))?;
    charge(budget, bytes, values)
}
fn precharge_lineage(
    store: &Store,
    request: &VersionLineageRequest,
    budget: &mut ProofBudget,
) -> Result<()> {
    let (count,bytes):(i64,i64)=store.db.query_row("SELECT count(*),coalesce(sum(length(CAST(id AS BLOB))+length(CAST(kind AS BLOB))),0) FROM items WHERE id IN (?1,?2,?3)",params![request.work_item_id,request.edition_item_id,request.version_item_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    admit_sql_rows(budget, count, bytes, 3)?;
    let (count,bytes):(i64,i64)=store.db.query_row("SELECT count(*),coalesce(sum(length(CAST(v.id AS BLOB))+length(CAST(v.association_item_id AS BLOB))+length(CAST(r.timeline_id AS BLOB))+length(CAST(t.edition_item_id AS BLOB))),0) FROM catalog_versions v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision JOIN catalog_timelines t ON t.id=r.timeline_id WHERE v.id=?",[&request.version_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    admit_sql_rows(budget, count, bytes, 1)?;
    let (count,bytes):(i64,i64)=store.db.query_row("SELECT count(*),coalesce(sum(length(CAST(id AS BLOB))+length(CAST(source_id AS BLOB))+length(CAST(target_id AS BLOB))+length(CAST(kind AS BLOB))),0) FROM item_relationships WHERE active=1 AND ((target_id=?1 AND kind='custom:motion_edition') OR (target_id=?2 AND kind='custom:motion_version'))",params![request.edition_item_id,request.version_item_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    admit_sql_rows(budget, count, bytes, 1000)
}
fn precharge_occurrence(store: &Store, file: &FileKey, budget: &mut ProofBudget) -> Result<()> {
    let (count,bytes):(i64,i64)=store.db.query_row("SELECT count(*),coalesce(sum(length(CAST(f.id AS BLOB))+length(CAST(f.location AS BLOB))+length(CAST(f.path AS BLOB))+coalesce(length(CAST(b.root AS BLOB)),0)+coalesce(length(CAST(o.status AS BLOB)),0)),0) FROM files f LEFT JOIN observations o ON o.profile=?1 AND o.file_id=f.id LEFT JOIN bindings b ON b.profile=?1 AND b.kind='source' AND b.owner=f.location WHERE f.id=?2",params![file.profile,file.file_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    if count != 1 {
        return Err(refused("native file lookup incomplete"));
    }
    admit_sql_rows(budget, count, bytes, 1)?;
    // These rows are decoded by the existing occurrence owner. Inspect borrowed
    // text and charge exact raw JSON before any owned string or Value allocation.
    let mut policies=store.db.prepare("SELECT p.identity_policy,p.settings FROM source_identity_policies p JOIN files f ON f.location=p.location WHERE p.profile=? AND f.id=?")?;
    let mut rows = policies.query(params![file.profile, file.file_id])?;
    while let Some(row) = rows.next()? {
        for index in 0..2 {
            match row.get_ref(index)? {
                ValueRef::Text(raw) => {
                    let raw = std::str::from_utf8(raw).map_err(refused)?;
                    if index == 1 && !raw.is_empty() {
                        budget.raw_json(raw, 64)?;
                    }
                    charge(
                        budget,
                        raw.len()
                            .checked_mul(6)
                            .and_then(|n| n.checked_add(1024))
                            .ok_or_else(|| refused("source policy byte overflow"))?,
                        4,
                    )?;
                }
                ValueRef::Null if index == 1 => {}
                _ => return Err(refused("invalid stored source identity policy")),
            }
        }
    }
    let bytes:i64=store.db.query_row("SELECT coalesce(sum(length(CAST(v.volume_uuid AS BLOB))),0) FROM binding_volumes v JOIN files f ON f.location=v.owner WHERE v.profile=? AND v.kind='source' AND f.id=?",params![file.profile,file.file_id],|r|r.get(0))?;
    admit_sql_rows(budget, 1, bytes, 1)
}

#[cfg(test)]
pub(super) mod tests;

#[cfg(test)]
mod live_admission_tests {
    use super::*;
    use crate::operation_budget::{BudgetLimits, OperationBudget};
    fn budget() -> (OperationBudget, ProofBudget) {
        let shared = OperationBudget::new(
            std::time::Duration::from_secs(10),
            BudgetLimits {
                items: 100,
                values: 100_000,
                bytes: 10_000_000,
                work: 100_000,
            },
        )
        .unwrap();
        (shared.clone(), ProofBudget::new(shared))
    }
    #[test]
    fn terminal_live_admission_charges_consumed_fields_not_unused_clone() {
        let mut snapshot = json!({"status":"present", "root":"/tmp/root", "path":"a/b",
            "root_device":1,"root_inode":2,"volume_uuid":null,"size":3,"mtime_ns":4,
            "device":5,"inode":6,"source_policy":{"settings":{"device":"check","inode":"check","uuid":"skip"}}});
        let (first, mut first_budget) = budget();
        admit_live_validation(&snapshot, 7, &mut first_budget).unwrap();
        snapshot["unused_retained_payload"] = json!("x".repeat(20_000));
        let (second, mut second_budget) = budget();
        admit_live_validation(&snapshot, 7, &mut second_budget).unwrap();
        assert_eq!(first.usage(), second.usage());
        assert!(first.usage().bytes >= 3 * (5 * 40 + 64));
        assert!(first.usage().values >= 33);
        snapshot["path"] = json!("a/b/c/d");
        let (third, mut third_budget) = budget();
        admit_live_validation(&snapshot, 7, &mut third_budget).unwrap();
        assert!(third.usage().bytes > second.usage().bytes);
        assert!(third.usage().work > second.usage().work);
    }
    #[test]
    fn terminal_live_admission_refuses_huge_consumed_cell_and_cumulative_overflow() {
        let (_, mut huge) = budget();
        let snapshot = json!({"root":"x".repeat(MAX_METADATA_SNAPSHOT_BYTES + 1)});
        assert!(admit_live_validation(&snapshot, 1, &mut huge).is_err());
        let (_, mut cumulative) = budget();
        cumulative
            .charge(BudgetCharge {
                work: MAX_METADATA_JSON_NODES - 1,
                ..Default::default()
            })
            .unwrap();
        assert!(admit_live_validation(&json!({}), 1, &mut cumulative).is_err());
        #[cfg(target_os = "linux")]
        {
            let (_, mut uuid) = budget();
            let error =
                admit_live_validation(&json!({"volume_uuid":"required-volume"}), 1, &mut uuid)
                    .unwrap_err();
            assert!(
                error
                    .0
                    .contains("bounded terminal volume UUID probe unavailable")
            );
        }
    }
}
