//! One private refresh receipt transaction. Every fallible post-effect check is
//! guarded by the complete subject barrier set; a dropped/error result never
//! releases a partial batch. This module neither commits nor writes receipts.
use super::*;

pub(in crate::catalog_nfo_attribution) fn apply_in(
    store: &Store,
    prepared: PreparedRefresh<'_>,
    admitted_at_us: i64,
) -> Result<Value> {
    apply_inner(
        store,
        prepared,
        admitted_at_us,
        #[cfg(test)]
        None,
        #[cfg(test)]
        None,
    )
}
#[cfg(test)]
pub(super) type PendingProbe<'a> =
    dyn FnMut(&Store, &RefreshReadPermit, &mut ProofBudget) -> Result<()> + 'a;
#[cfg(test)]
pub(super) fn apply_with_probe(
    store: &Store,
    prepared: PreparedRefresh<'_>,
    admitted_at_us: i64,
    probe: &mut PendingProbe<'_>,
) -> Result<Value> {
    apply_inner(store, prepared, admitted_at_us, Some(probe), None)
}
#[cfg(test)]
pub(super) type FirstInitProbe<'a> = dyn FnMut(&Store) -> Result<()> + 'a;
#[cfg(test)]
pub(super) fn apply_with_first_init_probe(
    store: &Store,
    prepared: PreparedRefresh<'_>,
    admitted_at_us: i64,
    probe: &mut FirstInitProbe<'_>,
) -> Result<Value> {
    apply_inner(store, prepared, admitted_at_us, None, Some(probe))
}
fn apply_inner(
    store: &Store,
    mut prepared: PreparedRefresh<'_>,
    admitted_at_us: i64,
    #[cfg(test)] mut probe: Option<&mut PendingProbe<'_>>,
    #[cfg(test)] mut first_init_probe: Option<&mut FirstInitProbe<'_>>,
) -> Result<Value> {
    if admitted_at_us < 0
        || store.schema_version != 41
        || store.db.is_autocommit()
        || !store.writable
        || prepared.outer.key().catalog_id() != store.database_id
        || Some(prepared.outer.admission_incarnation()) != store.catalog_incarnation.as_deref()
        || current_generation(store)? != prepared.outer.expected_generation()
    {
        return Err(refused("prepared refresh writer/fence changed"));
    }
    require_refresh_completion(prepared.authority.workflow.mode(), &prepared.plan.counts)
        .map_err(refused)?;
    prepared
        .observations
        .revalidate_custody_in(store, &BTreeMap::new(), &mut prepared.budget)?;
    for first in prepared.authority.first_sources.values() {
        first.validate_in(store, &prepared.outer)?;
    }
    for (subject, original) in &prepared.original {
        let actual = state::read_in(
            store,
            subject,
            prepared.outer.expected_generation(),
            prepared.request.expected_aggregates[subject],
        )?;
        if actual.presence() != original.presence()
            || actual.native_source() != original.native_source()
            || actual.origin_physical() != original.origin_physical()
            || actual.origin_proof_digest() != original.origin_proof_digest()
            || actual.source_proof_digest() != original.source_proof_digest()
            || actual.attribution_audit_digest() != original.attribution_audit_digest()
        {
            return Err(refused("prepared affected subject changed before effects"));
        }
        prepared.budget.serialized(actual.presence(), 64)?;
    }
    let subjects: BTreeSet<String> = prepared
        .steps
        .iter()
        .map(|step| step.effect.subject.clone())
        .collect();
    prepared.budget.serialized(&subjects, 64)?;
    let empty:bool=store.db.query_row("SELECT NOT EXISTS(SELECT 1 FROM catalog_metadata_permits) AND NOT EXISTS(SELECT 1 FROM catalog_commit_barrier WHERE id=1)",[],|r|r.get(0))?;
    if !empty {
        return Err(refused("another pending metadata batch exists"));
    }
    // No source/schema/index effect can precede this complete barrier set.
    for subject in &subjects {
        store.db.execute(
            "INSERT INTO catalog_metadata_permits(item_id,incarnation) VALUES (?,?)",
            params![subject, prepared.outer.admission_incarnation()],
        )?;
    }
    let mut pending = RefreshReadPermit {
        outer: prepared.outer.clone(),
        expected_generation: current_generation(store)?,
        subjects,
        written: BTreeMap::new(),
    };
    pending.validate_in(store, pending.expected_generation, &mut prepared.budget)?;
    let mut current = prepared.original;
    let mut written_work_heads = BTreeMap::new();
    for (index, step) in prepared.steps.iter().enumerate() {
        let before = current
            .get(&step.effect.subject)
            .ok_or_else(|| refused("step subject outside closure"))?;
        let expected_before = step.effect.expected.clone();
        // Pure planning keeps the original structural stamp. Only this batch's
        // exact metadata steps may advance the work head/schema in between.
        let original = &prepared.plan.fence;
        if original.catalog_id != store.database_id
            || original.incarnation != prepared.outer.admission_incarnation()
        {
            return Err(refused("step catalog identity changed"));
        }
        if before.head().source.as_ref().map(|s| s.revision) != expected_before.source_revision
            || before.head().attribution.as_ref().map(|a| a.revision)
                != expected_before.attribution_revision
            || before.head().pin != expected_before.pin
        {
            return Err(refused("step current source/attribution/pin differs"));
        }
        if let Some(plan) = &step.metadata {
            let permit = RefreshStepPermit {
                outer: prepared.outer.clone(),
                subject: step.effect.subject.clone(),
                expected_generation: current_generation(store)?,
                raw_request: plan.raw_request.clone(),
                first_proof_digest: if plan.first.is_some() {
                    Some(
                        prepared
                            .authority
                            .first_sources
                            .get(&step.effect.subject)
                            .ok_or_else(|| {
                                refused("first source capability missing before initialization")
                            })?
                            .proof_digest()
                            .to_owned(),
                    )
                } else {
                    None
                },
                metadata_digest: step
                    .metadata_digest
                    .clone()
                    .ok_or_else(|| refused("source plan digest absent"))?,
            };
            let grants = &prepared.authority.subjects[&step.effect.subject];
            #[cfg(test)]
            let applied = if let Some(probe) = first_init_probe.as_deref_mut() {
                metadata::apply_refresh_source_with_probe(
                    store,
                    &prepared.outer,
                    plan,
                    &grants.source,
                    &grants.initialization,
                    &grants.extension,
                    &grants.tags,
                    &permit,
                    admitted_at_us,
                    probe,
                )
            } else {
                metadata::apply_refresh_source(
                    store,
                    &prepared.outer,
                    plan,
                    &grants.source,
                    &grants.initialization,
                    &grants.extension,
                    &grants.tags,
                    &permit,
                    admitted_at_us,
                )
            };
            #[cfg(not(test))]
            let applied = metadata::apply_refresh_source(
                store,
                &prepared.outer,
                plan,
                &grants.source,
                &grants.initialization,
                &grants.extension,
                &grants.tags,
                &permit,
                admitted_at_us,
            );
            applied.map_err(refused)?;
        }
        publish_state(
            store,
            &prepared.outer,
            &prepared.digest,
            index,
            step,
            before,
            prepared.observations.proof_digest(),
            prepared
                .authority
                .first_sources
                .get(&step.effect.subject)
                .copied(),
            &mut prepared.budget,
        )?;
        let source_revision = step
            .presence
            .final_snapshot
            .head
            .source
            .as_ref()
            .ok_or_else(|| refused("published source head absent"))?
            .revision;
        let source_written = step.metadata.is_some()
            || pending
                .written
                .get(&step.effect.subject)
                .is_some_and(|head| head.source_written);
        pending.written.insert(
            step.effect.subject.clone(),
            WrittenHead {
                state_revision: step.effect.attribution.revision,
                source_revision,
                source_written,
            },
        );
        pending.expected_generation = current_generation(store)?;
        let actual = state::read_pending_in(
            store,
            &step.effect.subject,
            pending.expected_generation,
            prepared.request.expected_aggregates[&step.effect.subject],
            &pending,
        )?;
        verify_step(step, before, &actual)?;
        #[cfg(test)]
        if let Some(probe) = probe.as_mut() {
            // Test observation after real owner writes. The hook supplies no
            // source/observation capability and has no successful bypass path.
            probe(store, &pending, &mut prepared.budget)?;
        }

        if step.metadata.is_some() {
            if let SubjectState::Active { item_revision } = &actual.head().subject_state {
                written_work_heads.insert(step.effect.subject.clone(), *item_revision);
            }
        }
        current.insert(step.effect.subject.clone(), actual);
    }
    // Reobserve actual retained physical custody before permitting commit. This
    // does not turn historical imported mappings into current byte authority.
    prepared.observations.revalidate_custody_in(
        store,
        &written_work_heads,
        &mut prepared.budget,
    )?;
    pending.expected_generation = current_generation(store)?;
    pending.validate_in(store, pending.expected_generation, &mut prepared.budget)?;
    let mut states = BTreeMap::new();
    for (subject, expected) in &current {
        let actual = state::read_pending_in(
            store,
            subject,
            pending.expected_generation,
            prepared.request.expected_aggregates[subject],
            &pending,
        )?;
        if actual.head() != expected.head()
            || actual.presence().presence != expected.presence().presence
            || actual.source_proof_digest() != expected.source_proof_digest()
            || actual.attribution_audit_digest() != expected.attribution_audit_digest()
            || actual.origin_physical() != expected.origin_physical()
            || actual.origin_proof_digest() != expected.origin_proof_digest()
        {
            return Err(refused("terminal affected state changed"));
        }
        prepared.budget.serialized(actual.presence(), 64)?;
        // This owner does not issue Motion aggregate tokens. Return exact
        // native/public heads only, never the caller-supplied aggregate stamp.
        states.insert(
            subject.clone(),
            json!({"head":actual.head(),
            "presence":actual.presence().presence}),
        );
    }
    // The plan and every effect were independently admitted during prepare.
    // Admit only this exact borrowed response before allocating its Value; a
    // second traversal of the entire plan would charge non-response effects
    // again, while still not describe the actual response envelope.
    let response = RefreshResponse {
        contract_version: 1,
        workflow: prepared.request.workflow,
        progress: &prepared.plan.progress,
        counts: &prepared.plan.counts,
        observations: &prepared.plan.observations,
        states: &states,
        plan_digest: &prepared.digest,
        generation: pending.expected_generation,
    };
    let result = admitted_response(&mut prepared.budget, &response)?;
    // The receipt owner will encode this already validated value. No fallible
    // proof, filesystem operation or source publication follows barrier release.
    let subject_json = serde_json::to_string(&pending.subjects)?;
    let expected = pending.subjects.len();
    pending.validate_in(store, pending.expected_generation, &mut prepared.budget)?;
    for (subject, written) in &pending.written {
        if written.source_written {
            super::super::source_effect::finish_publication_in(
                store,
                &prepared.outer,
                subject,
                written.source_revision,
            )?;
        }
    }
    let removed=store.db.execute("DELETE FROM catalog_metadata_permits WHERE incarnation=? AND barrier_id=1 AND item_id IN(SELECT value FROM json_each(?))",
        params![prepared.outer.admission_incarnation(),subject_json])?;
    if removed != expected {
        return Err(refused("complete batch barrier release differs"));
    }
    Ok(result)
}

#[derive(Serialize)]
struct RefreshResponse<'a, P: Serialize, C: Serialize, O: Serialize> {
    contract_version: u32,
    workflow: Workflow,
    progress: &'a P,
    counts: &'a C,
    observations: &'a O,
    states: &'a BTreeMap<String, Value>,
    plan_digest: &'a str,
    generation: i64,
}

// Both passes serialize the same immutable borrowed value. Streaming admission
// charges its complete bytes/nodes/work before the allocating Value pass. No
// budget reset or independent response budget is introduced.
pub(super) fn admitted_response(
    budget: &mut ProofBudget,
    response: &impl Serialize,
) -> Result<Value> {
    budget.serialized(response, 64)?;
    Ok(serde_json::to_value(response)?)
}

#[cfg(test)]
mod response_tests {
    use super::*;
    #[test]
    fn borrowed_refresh_response_preserves_exact_wire_shape() {
        let progress = json!({"observed": 2, "applied": 1});
        let counts = json!({"skipped": 1});
        let observations = json!([{"outcome": "applied"}, {"outcome": "skipped"}]);
        let states: BTreeMap<String, Value> = [(
            "work".into(),
            json!({"head": {"revision": 2}, "presence": null}),
        )]
        .into();
        let digest = "a".repeat(64);
        for workflow in [Workflow::DirectRefresh, Workflow::MetadataJob] {
            let response = RefreshResponse {
                contract_version: 1,
                workflow,
                progress: &progress,
                counts: &counts,
                observations: &observations,
                states: &states,
                plan_digest: &digest,
                generation: 123,
            };
            let actual = serde_json::to_value(&response).unwrap();
            let previous = json!({"contract_version":1,"workflow":workflow,
                "progress":progress,"counts":counts,"observations":observations,
                "states":states,"plan_digest":digest,"generation":123});
            assert_eq!(actual, previous);
            assert_eq!(
                serde_json::to_vec(&actual).unwrap(),
                serde_json::to_vec(&previous).unwrap()
            );
        }
    }
}

fn verify_step(
    step: &Step,
    before: &state::CurrentNfoState,
    actual: &state::CurrentNfoState,
) -> Result<()> {
    let expected = &step.presence.final_snapshot;
    if actual.head().source != expected.head.source
        || actual.head().attribution != expected.head.attribution
        || actual.head().pin != before.head().pin
        || actual.presence().presence != expected.presence
        || &actual.head().system_fields
            != step
                .metadata
                .as_ref()
                .map(|p| p.after.schema.system_fields())
                .unwrap_or(&before.head().system_fields)
    {
        return Err(refused("normalized persisted refresh effect differs"));
    }
    match (
        &before.head().subject_state,
        &actual.head().subject_state,
        step.metadata.is_some(),
    ) {
        (
            SubjectState::Active { item_revision: old },
            SubjectState::Active { item_revision: new },
            true,
        ) if new >= old => {}
        (old, new, false) if old == new => {}
        _ => return Err(refused("refresh changed unsupported work lifecycle")),
    }
    if let Some(plan) = &step.metadata {
        if actual.native_source() != Some(&plan.source)
            || actual.schema().map(|s| s.fingerprint()) != Some(plan.after.schema.fingerprint())
        {
            return Err(refused("persisted native source/schema differs"));
        }
    } else if actual.native_source() != before.native_source()
        || actual.schema().map(|s| s.fingerprint()) != before.schema().map(|s| s.fingerprint())
    {
        return Err(refused("retained source/schema changed"));
    }
    Ok(())
}

fn publish_state(
    store: &Store,
    outer: &CommandEnvelope,
    batch_digest: &str,
    index: usize,
    step: &Step,
    before: &state::CurrentNfoState,
    observation_digest: &str,
    first: Option<&super::super::first_source::VerifiedFirstNfoAbsence>,
    budget: &mut ProofBudget,
) -> Result<()> {
    let subject = &step.effect.subject;
    let prior = before.head().attribution.as_ref();
    if prior.is_none()
        && (first.is_none() || !step.metadata.as_ref().is_some_and(|p| p.first.is_some()))
    {
        return Err(refused(
            "first publication lacks independently retained absence capability",
        ));
    }
    let next = &step.effect.attribution;
    let presence = &step.presence.final_snapshot.presence;
    let source = step
        .presence
        .final_snapshot
        .head
        .source
        .as_ref()
        .ok_or_else(|| refused("source absent"))?;
    if prior.map_or(Some(1), |p| p.revision.checked_add(1)) != Some(next.revision)
        || source.revision != next.document_revision
        || source.revision != presence.revision
    {
        return Err(refused("publication revision sequence differs"));
    }
    let identity = match &source.document {
        DocumentState::Present {
            external_identity, ..
        } => external_identity.as_deref(),
        DocumentState::Withdrawn => None,
    };
    let (physical, origin_proof) = match next.origin.as_ref().map(|origin| &origin.provenance) {
        Some(OriginProvenance::Imported { .. })
            if prior.is_some_and(|p| next.origin == p.origin) =>
        {
            (before.origin_physical(), before.origin_proof_digest())
        }
        Some(OriginProvenance::Imported { .. }) => {
            return Err(refused("refresh cannot invent imported origin"));
        }
        _ => (None, None),
    };
    let origin_commitment =
        commitment::AdmittedCommitment::capture(budget, &(&next.origin, &physical))?;
    let origin_json = next
        .origin
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let physical_json = physical.map(serde_json::to_string).transpose()?;
    let previous_proof = before
        .source_proof_digest()
        .or_else(|| first.filter(|_| prior.is_none()).map(|f| f.proof_digest()))
        .ok_or_else(|| refused("prior source proof absent"))?;
    let previous_audit = before
        .attribution_audit_digest()
        .or_else(|| first.filter(|_| prior.is_none()).map(|f| f.audit_digest()))
        .ok_or_else(|| refused("prior attribution audit absent"))?;
    let source_proof = budget.serialized(
        &(
            "catabolic.nfo.refresh.state.v3",
            previous_proof,
            previous_audit,
            batch_digest,
            index,
            // The owner-produced v2 step commitment already binds the complete
            // admitted presence plan. Do not traverse/hash that value again.
            &step.digest,
            outer.key(),
            outer.digest().as_str(),
            origin_commitment.digest(),
            origin_proof,
        ),
        64,
    )?;
    let audit = budget.serialized(
        &(
            "catabolic.nfo.refresh.audit.v2",
            previous_audit,
            observation_digest,
            batch_digest,
            index,
            subject,
            origin_commitment.digest(),
        ),
        64,
    )?;
    let row = PersistedStateRow {
        item_id: subject,
        revision: next.revision,
        source_revision: source.revision,
        source_namespace: "motion.nfo",
        source_record_id: "document",
        public_incarnation: presence.incarnation,
        public_revision: presence.public_revision,
        document_identity: identity,
        active_identity: next.active_identity.as_deref(),
        origin_json: origin_json.as_deref(),
        origin_physical_json: physical_json.as_deref(),
        origin_proof_digest: origin_proof,
        source_proof_digest: &source_proof,
        attribution_audit_digest: &audit,
        command_catalog_id: outer.key().catalog_id(),
        command_host_namespace: outer.key().host_namespace(),
        command_operation_id: outer.key().operation_id(),
    };
    // Every persisted scalar/string, including constants, is admitted once.
    // Full document/presence plans were already admitted and committed by step;
    // they are not a second SQL row or allocation at this publication boundary.
    budget.serialized(&row, 64)?;
    store.db.execute("INSERT INTO catalog_nfo_state_revisions(item_id,revision,source_revision,source_namespace,source_record_id,public_incarnation,public_revision,document_identity,active_identity,origin_json,origin_physical_json,origin_proof_digest,source_proof_digest,attribution_audit_digest,command_catalog_id,command_host_namespace,command_operation_id) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        params![row.item_id,row.revision,row.source_revision,row.source_namespace,row.source_record_id,
            row.public_incarnation,row.public_revision,row.document_identity,row.active_identity,
            row.origin_json,row.origin_physical_json,row.origin_proof_digest,row.source_proof_digest,
            row.attribution_audit_digest,row.command_catalog_id,row.command_host_namespace,row.command_operation_id])?;
    if let Some(prior) = prior {
        if store.db.execute(
        "UPDATE catalog_nfo_states SET current_revision=? WHERE item_id=? AND current_revision=?",
        params![next.revision, subject, prior.revision],
    )? != 1
    {
        return Err(refused("refresh state CAS failed"));
    }
        if store.db.execute(
            "DELETE FROM catalog_nfo_identity_claims WHERE item_id=? AND state_revision=?",
            params![subject, prior.revision],
        )? != usize::from(prior.active_identity.is_some())
        {
            return Err(refused("prior automatic claim closure differs"));
        }
        if store.db.execute(
            "DELETE FROM catalog_nfo_file_attributions WHERE item_id=? AND state_revision=?",
            params![subject, prior.revision],
        )? != usize::from(prior.origin.is_some())
        {
            return Err(refused("prior origin closure differs"));
        }
    } else {
        store.db.execute(
            "INSERT INTO catalog_nfo_states(item_id,current_revision) VALUES (?,?)",
            params![subject, next.revision],
        )?;
    }
    if let Some(identity) = &next.active_identity {
        store.db.execute(
            "INSERT INTO catalog_nfo_identity_claims(value,item_id,state_revision) VALUES(?,?,?)",
            params![identity, subject, next.revision],
        )?;
    }
    if let Some(origin) = &next.origin {
        store.db.execute("INSERT INTO catalog_nfo_file_attributions(item_id,profile,file_id,state_revision) VALUES(?,?,?,?)",
            params![subject,origin.file.profile,origin.file.file_id,next.revision])?;
    }
    Ok(())
}

/// Borrowed exact INSERT representation. The serializer and SQL bindings consume
/// these same seventeen fields; no source document or presence plan is omitted
/// from its earlier admission, cloned here, or written as an extra SQL column.
#[derive(Serialize)]
struct PersistedStateRow<'a> {
    item_id: &'a str,
    revision: i64,
    source_revision: i64,
    source_namespace: &'a str,
    source_record_id: &'a str,
    public_incarnation: i64,
    public_revision: Option<i64>,
    document_identity: Option<&'a str>,
    active_identity: Option<&'a str>,
    origin_json: Option<&'a str>,
    origin_physical_json: Option<&'a str>,
    origin_proof_digest: Option<&'a str>,
    source_proof_digest: &'a str,
    attribution_audit_digest: &'a str,
    command_catalog_id: &'a str,
    command_host_namespace: &'a str,
    command_operation_id: &'a str,
}

#[cfg(test)]
mod persisted_row_tests {
    use super::*;
    #[test]
    fn borrowed_state_row_matches_all_persisted_fields_and_binds_head_identity_origin() {
        let mut row = PersistedStateRow {
            item_id: "work",
            revision: 3,
            source_revision: 2,
            source_namespace: "motion.nfo",
            source_record_id: "document",
            public_incarnation: 1,
            public_revision: Some(2),
            document_identity: Some("external"),
            active_identity: Some("external"),
            origin_json: Some("{\"file\":\"one\"}"),
            origin_physical_json: None,
            origin_proof_digest: None,
            source_proof_digest: "source-proof",
            attribution_audit_digest: "audit-proof",
            command_catalog_id: "catalog",
            command_host_namespace: "host",
            command_operation_id: "operation",
        };
        let actual = serde_json::to_value(&row).unwrap();
        let exact = json!({"item_id":"work","revision":3,"source_revision":2,
            "source_namespace":"motion.nfo","source_record_id":"document",
            "public_incarnation":1,"public_revision":2,"document_identity":"external",
            "active_identity":"external","origin_json":"{\"file\":\"one\"}",
            "origin_physical_json":null,"origin_proof_digest":null,
            "source_proof_digest":"source-proof","attribution_audit_digest":"audit-proof",
            "command_catalog_id":"catalog","command_host_namespace":"host","command_operation_id":"operation"});
        assert_eq!(actual, exact);
        assert_eq!(actual.as_object().unwrap().len(), 17);
        assert_eq!(
            serde_json::to_vec(&actual).unwrap(),
            serde_json::to_vec(&exact).unwrap()
        );
        let original = catabolic_core::sha256(serde_json::to_vec(&row).unwrap());
        row.revision += 1;
        assert_ne!(
            original,
            catabolic_core::sha256(serde_json::to_vec(&row).unwrap())
        );
        row.revision -= 1;
        row.active_identity = None;
        assert_ne!(
            original,
            catabolic_core::sha256(serde_json::to_vec(&row).unwrap())
        );
        row.active_identity = Some("external");
        row.origin_json = Some("{\"file\":\"two\"}");
        assert_ne!(
            original,
            catabolic_core::sha256(serde_json::to_vec(&row).unwrap())
        );
        row.origin_json = None;
        row.public_revision = None;
        assert!(serde_json::to_value(&row).unwrap()["origin_json"].is_null());
        assert!(serde_json::to_value(&row).unwrap()["public_revision"].is_null());
    }
}
