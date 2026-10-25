//! Private existing-work Unmatch composite. Releasing an accepted manual lock
//! does not release automatic identity ownership or rewrite provider evidence.
//! The initial cohort requires every projected identity and live Pin to be
//! covered by the independently verified NFO claim. Other providers remain an
//! explicit unsupported ownership obligation, never a partial unpin.
use super::{proof_budget::ProofBudget, state};
use crate::{
    Error, Result, Store,
    catalog_metadata_store::{self as metadata, MetadataAuthority},
    catalog_pins::{self, PinAuthority, PinBatch, PinChange, PinDecision, PinMutation},
    operation_budget::BudgetCharge,
};
use catabolic_core::catalog_command::CommandEnvelope;
use catabolic_core::catalog_metadata::nfo_refresh::{DocumentState, PinHead, SubjectState};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UnmatchRequest {
    pub contract_version: u32,
    pub subject: String,
    pub expected_item_revision: i64,
    pub expected_pin_revision: i64,
    pub expected_schema_fingerprint: String,
}
/// Independently established complete work curation, not inferred from a Pin or
/// a protected field grant. No public constructor/dispatcher is registered.
pub(crate) struct UnmatchAuthority<'a> {
    pub host_namespace: &'a str,
    pub mutable_subjects: &'a BTreeSet<String>,
    pub pins: &'a PinAuthority,
    pub projection: &'a [MetadataAuthority],
}
fn refused(reason: impl std::fmt::Display) -> Error {
    Error(format!("catalog_unmatch_refused: {reason}"))
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
        return Err(refused("exact native41 retained writer required"));
    }
    store.transaction_available()?;
    Ok(())
}
/// Minted only after complete current ownership/grant checks and the exact Pin
/// owner effect. A generic permit table row or request spelling cannot mint it.
pub(crate) struct UnmatchProjectionPermit {
    outer: CommandEnvelope,
    subject: String,
    plan_digest: String,
    after_pins_generation: i64,
}
impl UnmatchProjectionPermit {
    pub(crate) fn authorize_projection_in(
        &self,
        store: &Store,
        outer: &CommandEnvelope,
        plan: &metadata::UnmatchProjectionPlan,
    ) -> Result<()> {
        scope(store, outer)?;
        if outer != &self.outer
            || plan.subject() != self.subject
            || plan.digest() != self.plan_digest
            || generation(store)? != self.after_pins_generation
        {
            return Err(refused("projection permit identity/plan/fence differs"));
        }
        let exact:bool=store.db.query_row("SELECT (SELECT count(*) FROM catalog_metadata_permits)=1 AND EXISTS(SELECT 1 FROM catalog_metadata_permits WHERE item_id=? AND incarnation=?) AND NOT EXISTS(SELECT 1 FROM catalog_command_receipts WHERE catalog_id=? AND host_namespace=? AND operation_id=?)",params![self.subject,outer.admission_incarnation(),outer.key().catalog_id(),outer.key().host_namespace(),outer.key().operation_id()],|r|r.get(0))?;
        if !exact {
            return Err(refused("exact deferred barrier absent"));
        }
        Ok(())
    }
}

pub(crate) fn apply_unmatch_in(
    store: &Store,
    outer: &CommandEnvelope,
    request: &UnmatchRequest,
    authority: &UnmatchAuthority<'_>,
    admitted_at_us: i64,
) -> Result<Value> {
    apply_inner(
        store,
        outer,
        request,
        authority,
        admitted_at_us,
        #[cfg(test)]
        None,
    )
}
#[cfg(test)]
pub(super) fn apply_unmatch_with_probe_in(
    store: &Store,
    outer: &CommandEnvelope,
    request: &UnmatchRequest,
    authority: &UnmatchAuthority<'_>,
    time: i64,
    probe: &mut dyn FnMut(&Store) -> Result<()>,
) -> Result<Value> {
    apply_inner(store, outer, request, authority, time, Some(probe))
}
fn apply_inner(
    store: &Store,
    outer: &CommandEnvelope,
    request: &UnmatchRequest,
    authority: &UnmatchAuthority<'_>,
    time: i64,
    #[cfg(test)] mut probe: Option<&mut dyn FnMut(&Store) -> Result<()>>,
) -> Result<Value> {
    scope(store, outer)?;
    let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
    budget.serialized(&(request, outer.payload()), 64)?;
    if request.contract_version != 1
        || time < 0
        || generation(store)? != outer.expected_generation()
        || authority.host_namespace != outer.key().host_namespace()
        || !authority.mutable_subjects.contains(&request.subject)
        || outer.payload() != &json!({"operation":"nfo_unmatch","request":request})
    {
        return Err(refused(
            "exact command, current curation or generation differs",
        ));
    }
    let before = state::read_in(store, &request.subject, outer.expected_generation(), 0)?;
    if before.head().subject_state
        != (SubjectState::Active {
            item_revision: request.expected_item_revision,
        })
        || before.head().schema_fingerprint.as_deref()
            != Some(request.expected_schema_fingerprint.as_str())
    {
        return Err(refused("current item/schema differs"));
    }
    let (pin_revision, pin_value) = match &before.head().pin {
        PinHead::NeverDecided => (0, None),
        PinHead::Decision { revision, value } => (*revision, value.as_deref()),
    };
    if pin_revision != request.expected_pin_revision {
        return Err(refused("current Pin revision differs"));
    }
    let identity = match before.head().source.as_ref().map(|s| &s.document) {
        Some(DocumentState::Present {
            external_identity, ..
        }) => external_identity.as_deref(),
        _ => None,
    };
    let claim = before
        .head()
        .attribution
        .as_ref()
        .and_then(|a| a.active_identity.as_deref());
    if identity != claim || pin_value.is_some_and(|p| Some(p) != claim) {
        return Err(refused(
            "retained identity lacks exact automatic collision ownership",
        ));
    }
    let plan =
        metadata::plan_unmatch_projection_in(store, outer, &request.subject, authority.projection)
            .map_err(refused)?;
    // Every marker contributor must have the same exact identity array; the
    // projection owner validates this even for shadowed/unchanged contributors.
    let ids = plan
        .effective_before()
        .get("external_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| refused("complete provider identity array absent"))?;
    if ids.len() != usize::from(identity.is_some())
        || ids.iter().any(|id| {
            id.as_object().is_none_or(|o| o.len() != 3)
                || id["provider"] != "nfo"
                || id["namespace_version"] != ""
                || id["value"].as_str() != identity
        })
    {
        return Err(refused("uncovered provider identity ownership"));
    }
    // Check the COMPLETE live Pin set before any effects, not just requested
    // namespaces. Missing independent provider ownership refuses the whole op.
    let (count,bytes):(i64,i64)=store.db.query_row("SELECT count(*),coalesce(sum(length(CAST(namespace AS BLOB))+length(CAST(value AS BLOB))),0) FROM catalog_identity_pins WHERE item_id=? AND value IS NOT NULL",[&request.subject],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let count = usize::try_from(count).map_err(refused)?;
    let bytes = usize::try_from(bytes).map_err(refused)?;
    budget.charge(BudgetCharge {
        bytes,
        values: count,
        work: count.saturating_add(bytes / 64).saturating_add(1),
        ..Default::default()
    })?;
    if count != usize::from(pin_value.is_some()) {
        return Err(refused("uncovered provider Pin set"));
    }
    let mutation = if pin_value.is_some() {
        let batch = PinBatch {
            contract_version: 1,
            changes: vec![PinChange {
                item_id: request.subject.clone(),
                namespace: "nfo".into(),
                expected_revision: pin_revision,
                decision: PinDecision::Remove,
            }],
        };
        budget.serialized(&batch, 64)?;
        let preview = catalog_pins::preview_pins_in_transaction(store, &batch, authority.pins)
            .map_err(refused)?;
        Some(PinMutation {
            batch,
            preview_digest: preview.digest,
        })
    } else {
        None
    };
    store.db.execute(
        "INSERT INTO catalog_metadata_permits(item_id,incarnation) VALUES(?,?)",
        params![request.subject, outer.admission_incarnation()],
    )?;
    if let Some(mutation) = &mutation {
        catalog_pins::apply_pins(store, mutation, outer, authority.pins, time).map_err(refused)?;
    }
    budget.serialized(
        &(
            outer.key(),
            outer.admission_incarnation(),
            outer.expected_generation(),
            outer.payload(),
            plan.digest(),
            &request.subject,
        ),
        64,
    )?;
    let permit = UnmatchProjectionPermit {
        outer: outer.clone(),
        subject: request.subject.clone(),
        plan_digest: plan.digest().to_owned(),
        after_pins_generation: generation(store)?,
    };
    metadata::apply_unmatch_projection_in(store, outer, &plan, &permit, time).map_err(refused)?;
    #[cfg(test)]
    if let Some(probe) = probe.as_mut() {
        probe(store)?;
    }
    let after = state::read_in(store, &request.subject, generation(store)?, 0)?;
    let expected_pin = if mutation.is_some() {
        PinHead::Decision {
            revision: pin_revision
                .checked_add(1)
                .ok_or_else(|| refused("Pin revision exhausted"))?,
            value: None,
        }
    } else {
        before.head().pin.clone()
    };
    if after.head().pin != expected_pin
        || before.head().source != after.head().source
        || before.head().attribution != after.head().attribution
        || before.presence().presence != after.presence().presence
        || before.native_source() != after.native_source()
        || before.schema() != after.schema()
        || before.origin_physical() != after.origin_physical()
        || before.origin_proof_digest() != after.origin_proof_digest()
        || before.source_proof_digest() != after.source_proof_digest()
        || before.attribution_audit_digest() != after.attribution_audit_digest()
    {
        return Err(refused(
            "Unmatch changed retained contribution/identity/attribution",
        ));
    }
    let result = json!({"operation":"nfo_unmatch","subject":request.subject,"pin":expected_pin,"item":after.head().subject_state,"observed_generation":generation(store)?,"projection_digest":plan.digest()});
    budget.serialized(&result, 64)?;
    if store.db.execute(
        "DELETE FROM catalog_metadata_permits WHERE item_id=? AND incarnation=?",
        params![request.subject, outer.admission_incarnation()],
    )? != 1
    {
        return Err(refused("publication barrier changed"));
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
