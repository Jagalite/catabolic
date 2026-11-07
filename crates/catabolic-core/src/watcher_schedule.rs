// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Watcher admission from explicit durable generations and clock evidence.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DueEvidence {
    pub enabled: bool,
    pub requested_generation: i64,
    pub completed_generation: i64,
    pub pending_generation: i64,
    pub active_run: bool,
    pub lease_until: f64,
    pub retry_at: f64,
    pub pending_reaction: bool,
    pub manual_schedule: bool,
    pub next_due: f64,
    pub events_enabled: bool,
    pub event_first: Option<f64>,
    pub event_last: f64,
    pub debounce_seconds: Option<f64>,
    pub maximum_delay_seconds: Option<f64>,
    pub now: f64,
}
/// Explicit requests may run while disabled; active leases and retry deadlines
/// always fence admission. The caller revalidates in the claiming transaction.
pub fn due(e: DueEvidence) -> bool {
    let requested = e.requested_generation > e.completed_generation;
    if (!e.enabled && !requested) || (e.active_run && e.lease_until > e.now) || e.retry_at > e.now {
        return false;
    }
    if requested || e.pending_reaction || e.retry_at != 0.0 {
        return true;
    }
    if !e.manual_schedule && e.next_due <= e.now {
        return true;
    }
    e.events_enabled
        && e.pending_generation > e.completed_generation
        && e.event_first.is_some_and(|first| {
            e.now
                >= (e.event_last + e.debounce_seconds.unwrap())
                    .min(first + e.maximum_delay_seconds.unwrap())
        })
}
/// Completion may acknowledge only the generation belonging to the fenced run.
/// Later pending work keeps its event timing evidence for a future run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompletionEvidence {
    pub run_generation: i64,
    pub claimed_generation: i64,
    pub pending_generation: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionEffect {
    Reject,
    Complete { generation: i64, clear_events: bool },
}
pub fn complete(e: CompletionEvidence) -> CompletionEffect {
    if e.run_generation != e.claimed_generation {
        CompletionEffect::Reject
    } else {
        CompletionEffect::Complete {
            generation: e.run_generation,
            clear_events: e.pending_generation <= e.run_generation,
        }
    }
}
/// One durable reaction record either identifies the original change, points
/// to its predecessor, or proves that no change remains to be delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReactionStep<'a> {
    Changed,
    NoChange,
    Previous(&'a str),
    Reject,
}
pub fn reaction_step<'a>(
    phase: Option<&str>,
    changed_type: Option<&str>,
    link_type: Option<&str>,
    link: Option<&'a str>,
) -> ReactionStep<'a> {
    if phase != Some("planned") {
        return ReactionStep::Reject;
    }
    let previous = match (link_type, link) {
        (Some("null"), _) => None,
        (Some("text"), Some(previous)) => Some(previous),
        _ => return ReactionStep::Reject,
    };
    match (changed_type, previous) {
        (Some("true"), _) => ReactionStep::Changed,
        (Some("false"), None) => ReactionStep::NoChange,
        (Some("false"), Some(previous)) => ReactionStep::Previous(previous),
        _ => ReactionStep::Reject,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn base() -> DueEvidence {
        DueEvidence {
            enabled: true,
            requested_generation: 0,
            completed_generation: 0,
            pending_generation: 1,
            active_run: false,
            lease_until: 0.0,
            retry_at: 0.0,
            pending_reaction: false,
            manual_schedule: true,
            next_due: 0.0,
            events_enabled: true,
            event_first: Some(10.0),
            event_last: 12.0,
            debounce_seconds: Some(5.0),
            maximum_delay_seconds: Some(10.0),
            now: 16.0,
        }
    }
    #[test]
    fn reaction_chain_requires_a_valid_planned_record() {
        assert_eq!(
            reaction_step(Some("planned"), Some("true"), Some("null"), None),
            ReactionStep::Changed
        );
        assert_eq!(
            reaction_step(
                Some("planned"),
                Some("false"),
                Some("text"),
                Some("original")
            ),
            ReactionStep::Previous("original")
        );
        assert_eq!(
            reaction_step(Some("planned"), Some("false"), Some("null"), None),
            ReactionStep::NoChange
        );
        assert_eq!(
            reaction_step(Some("planned"), Some("true"), Some("integer"), None),
            ReactionStep::Reject
        );
        assert_eq!(
            reaction_step(Some("finished"), Some("true"), Some("null"), None),
            ReactionStep::Reject
        );
        assert_eq!(
            reaction_step(Some("planned"), Some("integer"), Some("null"), None),
            ReactionStep::Reject
        );
    }
    #[test]
    fn debounce_deadline_and_claim_fences() {
        let e = base();
        assert!(!due(e));
        assert!(due(DueEvidence { now: 17.0, ..e }));
        assert!(due(DueEvidence {
            event_last: 30.0,
            now: 20.0,
            ..e
        }));
        assert!(!due(DueEvidence {
            active_run: true,
            lease_until: 21.0,
            now: 20.0,
            ..e
        }));
        assert!(!due(DueEvidence {
            retry_at: 21.0,
            now: 20.0,
            ..e
        }));
        assert!(due(DueEvidence {
            enabled: false,
            requested_generation: 1,
            ..e
        }));
        assert!(!due(DueEvidence {
            enabled: false,
            ..e
        }));
    }
    #[test]
    fn completion_preserves_newer_events_and_rejects_mismatched_run_generation() {
        let e = CompletionEvidence {
            run_generation: 3,
            claimed_generation: 3,
            pending_generation: 4,
        };
        assert_eq!(
            complete(e),
            CompletionEffect::Complete {
                generation: 3,
                clear_events: false
            }
        );
        assert_eq!(
            complete(CompletionEvidence {
                pending_generation: 3,
                ..e
            }),
            CompletionEffect::Complete {
                generation: 3,
                clear_events: true
            }
        );
        assert_eq!(
            complete(CompletionEvidence {
                claimed_generation: 4,
                ..e
            }),
            CompletionEffect::Reject
        );
    }
}
