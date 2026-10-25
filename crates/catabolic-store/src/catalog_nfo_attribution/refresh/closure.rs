//! Native-owned refresh closure. Neither the requested subject list nor an
//! observed empty source is evidence of completeness or prior absence.
use super::*;
use catabolic_core::catalog_metadata::nfo_refresh::{FileKey, IdentityClaim, SidecarObservation};
use rusqlite::params;

pub(super) struct Closure {
    pub states: BTreeMap<String, state::CurrentNfoState>,
    pub claims: BTreeMap<String, IdentityClaim>,
}

/// Expand every prior file owner before accepting per-subject grants. The
/// caller supplies independently authorized aggregate stamps for exactly this
/// closure; an omitted hidden owner is a refusal, never a partial refresh.
pub(super) fn read_in(
    store: &Store,
    command: &CommandEnvelope,
    request: &RefreshRequestV2,
    aggregate_revisions: &BTreeMap<String, i64>,
    budget: &mut ProofBudget,
) -> Result<Closure> {
    let mut subjects = BTreeSet::from([request.subject.clone()]);
    let mut files = BTreeSet::<FileKey>::new();
    for observation in &request.observations {
        budget.serialized(&observation.expected_file.file, 64)?;
        files.insert(observation.expected_file.file.clone());
    }
    let mut query = store.db.prepare(
        "SELECT item_id FROM catalog_nfo_file_attributions WHERE profile=? AND file_id=? ORDER BY item_id",
    )?;
    for file in &files {
        let mut rows = query.query(params![file.profile, file.file_id])?;
        while let Some(row) = rows.next()? {
            let subject = text(row, 0, 256, budget)?;
            subjects.insert(subject);
            if subjects.len() > MAX_METADATA_JSON_NODES {
                return Err(refused("complete prior-owner closure exceeds node budget"));
            }
        }
    }
    if subjects.len() != aggregate_revisions.len()
        || subjects
            .iter()
            .any(|subject| !aggregate_revisions.contains_key(subject))
    {
        return Err(refused(
            "complete affected-work authorization/aggregate closure differs",
        ));
    }
    let mut states = BTreeMap::new();
    let mut identities = BTreeSet::new();
    for subject in subjects {
        let current = state::read_in(
            store,
            &subject,
            command.expected_generation(),
            aggregate_revisions[&subject],
        )?;
        budget.serialized(current.presence(), 64)?;
        if let Some(identity) = current
            .head()
            .attribution
            .as_ref()
            .and_then(|head| head.active_identity.as_ref())
        {
            budget.serialized(identity, 64)?;
            identities.insert(identity.clone());
        }
        states.insert(subject, current);
    }
    for observation in &request.observations {
        if let SidecarObservation::Parsed {
            external_identity: Some(identity),
            ..
        } = &observation.sidecar
        {
            budget.serialized(identity, 64)?;
            identities.insert(identity.clone());
        }
    }
    let mut claims = BTreeMap::new();
    for identity in identities {
        // Both indexes are authoritative. Do not collapse a pin into an
        // automatic attribution or disclose an outside-closure subject.
        charge(budget, identity.len(), 2)?;
        let automatic = identity_owner(
            store,
            "SELECT item_id FROM catalog_nfo_identity_claims WHERE value=?",
            &identity,
            budget,
        )?;
        let manual = identity_owner(
            store,
            "SELECT item_id FROM catalog_identity_pins WHERE namespace='nfo' AND value=?",
            &identity,
            budget,
        )?;
        if let (Some(a), Some(m)) = (&automatic, &manual)
            && a != m
        {
            return Err(refused("automatic/manual identity indexes contradict"));
        }
        // The core Subject claim means an actual automatic attribution. A
        // manual-only claim must not be presented as one. A pin owned by the
        // target allows that exact incoming identity; any other manual owner
        // remains an opaque collision even when independently in the closure.
        let claim = match automatic {
            Some(subject) if states.contains_key(&subject) => IdentityClaim::Subject { subject },
            Some(_) => IdentityClaim::OutsideClosure,
            None if manual
                .as_deref()
                .is_some_and(|owner| owner != request.subject) =>
            {
                IdentityClaim::OutsideClosure
            }
            None => IdentityClaim::Available,
        };
        budget.serialized(&claim, 64)?;
        claims.insert(identity, claim);
    }
    Ok(Closure { states, claims })
}

fn identity_owner(
    store: &Store,
    sql: &str,
    identity: &str,
    budget: &mut ProofBudget,
) -> Result<Option<String>> {
    let mut query = store.db.prepare(sql)?;
    let mut rows = query.query([identity])?;
    let value = rows
        .next()?
        .map(|row| text(row, 0, 256, budget))
        .transpose()?;
    if rows.next()?.is_some() {
        return Err(refused("identity index is not unique"));
    }
    Ok(value)
}
