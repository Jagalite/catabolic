// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Two-scope continuation lineage and one dirty-watermark readmission.
//! One file per scope starts present/absent; the single source change reverses
//! both. Scope/seen copying and atomic publication are adapter abstractions,
//! grounded separately by native continuation/reopen tests. No filesystem
//! atomicity or unbounded scanner liveness is claimed. Source changes are assumed
//! to advance the dirty watermark; each observed scope is a complete bounded directory.
use catabolic_core::{
    encode,
    scan_publication::{self, ContinuationEvidence as Evidence, ContinuationFence as Fence},
    sha256,
};
use serde::{Deserialize, Serialize};
use stateless::automatic::Auto;
use stateless::execution::{ReplayOptions, ReplayOutcome, record, replay};
use stateless::explore::{SearchConfig, SearchTermination, ShrinkConfig, enumerate, shrink};
use stateless::trace::{ReadLimits, RunConfig, Termination, Trace};
use stateless::{Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition};
use std::{error::Error, fs::File, path::Path};
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Audited {
    generation: u8,
    parent_finished: bool,
    policy_matches: bool,
    dirty: bool,
    captured_dirty: bool,
    active: bool,
    cancelled: bool,
    selected: [bool; 2],
    complete: [bool; 2],
    seen: [bool; 2],
    verified: [bool; 2],
    missing: [bool; 2],
    violation: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Event {
    Observe(u8),
    Verify(u8),
    Publish(u8),
    FinishParent,
    ChangePolicy,
    MarkDirty,
    Continue(u8),
    Interrupt,
    Resume,
    Cancel,
    Readmit,
}
struct Adapter {
    fault: bool,
}
fn step_state(before: &Audited, event: &Event, fault: bool) -> Audited {
    let mut after = before.clone();
    let current = before.active
        && !before.cancelled
        && before.policy_matches
        && before.dirty == before.captured_dirty;
    match *event {
        Event::Observe(scope) if current && scope < 2 => {
            let scope = usize::from(scope);
            if before.selected[scope] && !before.complete[scope] {
                after.complete[scope] = true;
                after.seen[scope] = (scope == 0) != before.dirty;
            }
        }
        Event::Verify(scope) if current && scope < 2 => {
            let scope = usize::from(scope);
            after.verified[scope] = before.complete[scope];
        }
        Event::Publish(scope) if scope < 2 => {
            let scope = usize::from(scope);
            use scan_publication::{Evidence as E, Fence as F, ScopeEffect};
            let result = scan_publication::advance(F::Catalog, E::Catalog(true))
                .and_then(|f| {
                    scan_publication::advance(
                        f,
                        E::Claim(
                            before.active
                                && !before.cancelled
                                && before.dirty == before.captured_dirty,
                        ),
                    )
                })
                .and_then(|f| scan_publication::advance(f, E::Policy(before.policy_matches)))
                .and_then(|f| scan_publication::advance(f, E::Root(true)))
                .and_then(|f| {
                    scan_publication::scope_effect(
                        f,
                        before.complete[scope],
                        before.verified[scope],
                    )
                });
            let publish = result == Ok(ScopeEffect::PublishMissing);
            let expected = current && before.complete[scope] && before.verified[scope];
            if publish != expected {
                after.violation = true;
            }
            if publish {
                // Expected absence comes from the independent bounded source,
                // never from the copied observation_seen adapter evidence.
                let source_present = (scope == 0) != before.dirty;
                after.missing[scope] = !before.seen[scope];
                if after.missing[scope] == source_present {
                    after.violation = true;
                }
            }
        }
        Event::FinishParent if before.generation == 0 && current => {
            after.parent_finished = true;
            after.active = false;
        }
        Event::ChangePolicy => after.policy_matches = false,
        Event::MarkDirty => after.dirty = true,
        Event::Continue(selection)
            if selection <= 2 && before.generation == 0 && !before.cancelled =>
        {
            let result = scan_publication::advance_continuation(
                Fence::Parent,
                Evidence::ParentFinished(before.parent_finished),
            )
            .and_then(|f| {
                scan_publication::advance_continuation(
                    f,
                    Evidence::PolicyMatches(before.policy_matches),
                )
            })
            .and_then(|f| {
                scan_publication::advance_continuation(
                    f,
                    Evidence::DirtyUnchanged(before.dirty == before.captured_dirty),
                )
            });
            let accepted = result == Ok(Fence::Ready);
            let expected = before.parent_finished
                && before.policy_matches
                && before.dirty == before.captured_dirty;
            if accepted != expected {
                after.violation = true;
            }
            if accepted {
                after.generation = 1;
                after.active = true;
                after.selected = [selection != 1, selection != 0];
                // The real admission transaction copies both completed scopes
                // and seen rows. Negative control omits the latter.
                if fault {
                    after.seen = [false; 2];
                }
                // Completed inherited scopes must be verified afresh before
                // any missing publication in the child.
                after.verified = [false; 2];
            }
        }
        Event::Interrupt => after.active = false,
        Event::Resume if !before.cancelled && !before.parent_finished => {
            after.active = true;
        }
        Event::Resume if before.generation > 0 && !before.cancelled => {
            after.active = true;
        }
        Event::Cancel => {
            after.cancelled = true;
            after.active = false;
        }
        Event::Readmit
            if before.generation == 1
                && before.dirty != before.captured_dirty
                && !before.cancelled
                && before.policy_matches =>
        {
            after.generation = 2;
            after.captured_dirty = before.dirty;
            after.active = true;
            after.selected = [true; 2];
            after.complete = [false; 2];
            after.seen = [false; 2];
            after.verified = [false; 2];
        }
        _ => {}
    }
    // Required transition effects are checked independently as well as
    // publication safety; a no-op adapter must not satisfy admission.
    match *event {
        Event::Continue(selection)
            if selection <= 2
                && before.generation == 0
                && !before.cancelled
                && before.parent_finished
                && before.policy_matches
                && before.dirty == before.captured_dirty =>
        {
            after.violation |= after.generation != 1
                || !after.active
                || after.selected != [selection != 1, selection != 0]
                || after.complete != before.complete
                || after.seen != before.seen
                || after.verified != [false; 2];
        }
        Event::Readmit
            if before.generation == 1
                && before.dirty != before.captured_dirty
                && !before.cancelled
                && before.policy_matches =>
        {
            after.violation |= after.generation != 2
                || !after.active
                || after.captured_dirty != before.dirty
                || after.selected != [true; 2]
                || after.complete != [false; 2]
                || after.seen != [false; 2]
                || after.verified != [false; 2]
                || after.missing != before.missing;
        }
        Event::Cancel => {
            after.violation |= !after.cancelled || after.active || after.missing != before.missing;
        }
        _ => {}
    }
    after
}

impl Model for Adapter {
    type State = Audited;
    type Input = Event;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "catabolic-scan-continuation".into(),
            model_version: 2,
            properties_version: 2,
            codec_version: 2,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/scan_publication.rs").as_slice(),
                        include_bytes!("scan_continuation.rs").as_slice(),
                        include_bytes!("../../../Cargo.lock").as_slice()
                    ]
                    .concat()
                ),
                self.fault
            ),
        }
    }
    fn initial_state(&self) -> Result<Audited, ModelError> {
        Ok(Audited {
            generation: 0,
            parent_finished: false,
            policy_matches: true,
            dirty: false,
            captured_dirty: false,
            active: true,
            cancelled: false,
            selected: [true; 2],
            complete: [false; 2],
            seen: [false; 2],
            verified: [false; 2],
            missing: [false; 2],
            violation: false,
        })
    }
    fn step(
        &self,
        before: &Audited,
        event: &Event,
    ) -> Result<Transition<Audited, String>, ModelError> {
        Ok(Transition::accepted(
            step_state(before, event, self.fault),
            vec![format!("{event:?}")],
        ))
    }
    fn check_state(&self, s: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.violation {
            Check::failed(
                "scan.continuation",
                "continuation admitted stale lineage or published false absence",
            )
        } else {
            Check::passed("scan.continuation")
        }])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        Ok(vec![
            Event::Observe(0),
            Event::Observe(1),
            Event::Verify(0),
            Event::Verify(1),
            Event::Publish(0),
            Event::Publish(1),
            Event::FinishParent,
            Event::ChangePolicy,
            Event::MarkDirty,
            Event::Continue(0),
            Event::Continue(1),
            Event::Continue(2),
            Event::Interrupt,
            Event::Resume,
            Event::Cancel,
            Event::Readmit,
        ])
    }
}
fn codec(v: &impl Serialize) -> Result<Vec<u8>, ModelError> {
    Ok(encode(&serde_json::to_value(v).map_err(|e| ModelError::new(e.to_string()))?).into_bytes())
}
impl ModelCodec for Adapter {
    fn encode_state(&self, v: &Audited) -> Result<Vec<u8>, ModelError> {
        codec(v)
    }
    fn decode_state(&self, v: &[u8]) -> Result<Audited, ModelError> {
        serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))
    }
    fn encode_input(&self, v: &Event) -> Result<Vec<u8>, ModelError> {
        codec(v)
    }
    fn decode_input(&self, v: &[u8]) -> Result<Event, ModelError> {
        serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))
    }
    fn encode_output(&self, v: &String) -> Result<Vec<u8>, ModelError> {
        codec(v)
    }
}
pub fn check() -> Result<(), Box<dyn Error>> {
    let r = enumerate(&Adapter { fault: false }, SearchConfig::default())?;
    if r.failure.is_some()
        || r.termination != SearchTermination::GraphExhausted
        || r.skipped_checks != 0
    {
        return Err(format!(
            "scan continuation check failed/incomplete: {:?}",
            r.termination
        )
        .into());
    }
    println!(
        "scan continuation graph exhausted: {} states, {} transitions, zero skipped checks",
        r.states, r.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let f = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected scan continuation fault not found")?;
    let reduced = shrink(&model, &f, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("scan continuation shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-scan-continuation".into(),
        seed: None,
        parameters: vec![
            ("scopes".into(), "2; one file each".into()),
            (
                "generations".into(),
                "parent, child, dirty readmission".into(),
            ),
            ("source-changes".into(), "at most one".into()),
        ],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &f.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("scan continuation trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "scan continuation injected fault found and shrunk: {} -> {} inputs",
        f.inputs.len(),
        reduced.minimized.inputs.len()
    );
    Ok(())
}
pub fn replay_trace(path: &str) -> Result<(), Box<dyn Error>> {
    let trace = Trace::read_from(File::open(path)?, &ReadLimits::default())?;
    let r = replay(
        &Adapter { fault: true },
        &trace,
        ReplayOptions {
            allow_build_mismatch: false,
        },
    )?;
    if r.outcome != ReplayOutcome::Exact || !r.failure_reproduced || !r.build_matches {
        return Err("scan continuation fresh replay did not match".into());
    }
    println!(
        "scan continuation fresh-process replay exact, failed property reproduced, matching build"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(events: &[Event], fault: bool) -> Audited {
        let model = Adapter { fault };
        let mut state = model.initial_state().unwrap();
        for event in events {
            state = step_state(&state, event, fault);
        }
        state
    }

    #[test]
    fn selected_child_and_dirty_readmission_do_not_reuse_stale_seen_rows() {
        let state = run(
            &[
                Event::Observe(0),
                Event::FinishParent,
                Event::Continue(1),
                Event::Observe(1),
                Event::Verify(1),
                Event::Publish(1),
                Event::Interrupt,
                Event::MarkDirty,
                Event::Readmit,
                Event::Observe(0),
                Event::Verify(0),
                Event::Publish(0),
                Event::Observe(1),
                Event::Verify(1),
                Event::Publish(1),
            ],
            false,
        );
        assert!(!state.violation);
        assert_eq!(state.generation, 2);
        assert_eq!(state.missing, [true, false]);
    }

    #[test]
    fn omitted_seen_copy_has_a_false_absence_witness() {
        let events = [
            Event::Observe(0),
            Event::FinishParent,
            Event::Continue(1),
            Event::Verify(0),
            Event::Publish(0),
        ];
        assert!(!run(&events, false).violation);
        assert!(run(&events, true).violation);
    }
}
