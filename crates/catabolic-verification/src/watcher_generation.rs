// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded watcher generation and event retention sequences.
use catabolic_core::{
    encode, sha256,
    watcher_schedule::{self, CompletionEffect, CompletionEvidence},
};
use serde::{Deserialize, Serialize};
use stateless::automatic::Auto;
use stateless::execution::{ReplayOptions, ReplayOutcome, record, replay};
use stateless::explore::{SearchConfig, SearchTermination, ShrinkConfig, enumerate, shrink};
use stateless::trace::{ReadLimits, RunConfig, Termination, Trace};
use stateless::{Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition};
use std::{error::Error, fs::File, path::Path};
// Two immutable run identities, at most three demand generations and time
// 0..=2. Planning and event admission abstract durable lookup/transactions;
// watcher_event_crash exercises the corresponding committed production owners.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Run {
    generation: u8,
    planned: bool,
    changed: bool,
    previous: Option<u8>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Audited {
    pending: u8,
    requested: u8,
    completed: u8,
    active: Option<u8>,
    until: u8,
    now: u8,
    runs: [Option<Run>; 2],
    reaction: Option<u8>,
    delivered: u8,
    notifications: [u8; 2],
    event: bool,
    violation: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Event {
    Request,
    Observe,
    Tick,
    Claim(u8),
    Plan(bool),
    LegacyDeliver(u8),
    Complete(u8),
    WrongGeneration(u8),
}
struct Adapter {
    fault: bool,
}
fn token(owner: u8) -> String {
    format!("run-{owner}")
}
fn reaction_root(state: &Audited, owner: u8) -> Option<u8> {
    let mut current = owner;
    for _ in 0..2 {
        let run = state.runs[current as usize].as_ref()?;
        match watcher_schedule::reaction_step(
            Some(if run.planned { "planned" } else { "observing" }),
            Some(if run.changed { "true" } else { "false" }),
            Some(if run.previous.is_some() {
                "text"
            } else {
                "null"
            }),
            run.previous.map(|_| "previous"),
        ) {
            watcher_schedule::ReactionStep::Changed => return Some(current),
            watcher_schedule::ReactionStep::Previous(_) => current = run.previous?,
            watcher_schedule::ReactionStep::NoChange | watcher_schedule::ReactionStep::Reject => {
                return None;
            }
        }
    }
    None
}
impl Model for Adapter {
    type State = Audited;
    type Input = Event;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "catabolic-watcher-generation".into(),
            model_version: 2,
            properties_version: 2,
            codec_version: 2,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/watcher_schedule.rs").as_slice(),
                        include_bytes!("../../catabolic-core/src/coordination.rs").as_slice(),
                        include_bytes!("watcher_generation.rs").as_slice(),
                        include_bytes!("../../../Cargo.lock").as_slice()
                    ]
                    .concat()
                ),
                self.fault
            ),
        }
    }
    fn initial_state(&self) -> Result<Audited, ModelError> {
        Ok(Audited::default())
    }
    fn step(
        &self,
        before: &Audited,
        event: &Event,
    ) -> Result<Transition<Audited, String>, ModelError> {
        use catabolic_core::coordination::{self, Effect, Generation, Input, Lease, State};
        let mut after = before.clone();
        match event {
            Event::Request if before.pending < 3 => {
                after.pending += 1;
                after.requested = after.pending;
            }
            Event::Observe if before.pending < 3 => {
                after.pending += 1;
                after.event = true;
            }
            Event::Tick if before.now < 2 => after.now += 1,
            Event::Claim(owner)
                if before.runs[*owner as usize].is_none() && before.completed < 3 =>
            {
                let due = watcher_schedule::due(watcher_schedule::DueEvidence {
                    enabled: true,
                    requested_generation: before.requested.into(),
                    completed_generation: before.completed.into(),
                    pending_generation: before.pending.into(),
                    active_run: before.active.is_some(),
                    lease_until: before.until.into(),
                    retry_at: 0.0,
                    pending_reaction: before.reaction.is_some(),
                    manual_schedule: true,
                    next_due: 0.0,
                    events_enabled: true,
                    event_first: before.event.then_some(0.0),
                    event_last: 0.0,
                    debounce_seconds: Some(0.0),
                    maximum_delay_seconds: Some(0.0),
                    now: before.now.into(),
                });
                let expected_due = (before.active.is_none() || before.until <= before.now)
                    && (before.requested > before.completed
                        || before.reaction.is_some()
                        || (before.event && before.pending > before.completed));
                after.violation |= due != expected_due;
                if due {
                    let state = State {
                        generation: before.completed.into(),
                        lease: before.active.map(|active| Lease {
                            generation: before.pending.into(),
                            token: token(active),
                            identity: "definition".into(),
                            until: before.until.into(),
                        }),
                    };
                    let result = coordination::reduce(
                        &state,
                        &Input::Acquire {
                            now: before.now.into(),
                            duration: 1.0,
                            generation: Generation::AtLeast(before.pending.into()),
                            token: token(*owner),
                            identity: "definition".into(),
                            owner_alive: true,
                        },
                    );
                    let allowed = before.active.is_none() || before.until <= before.now;
                    match result {
                        Ok(Effect::Acquire(lease)) => {
                            let expected = (before.completed + 1).max(before.pending);
                            after.violation |= !allowed || lease.generation != i64::from(expected);
                            after.pending = lease.generation as u8;
                            after.until = lease.until as u8;
                            after.active = Some(*owner);
                            after.runs[*owner as usize] = Some(Run {
                                generation: expected,
                                ..Run::default()
                            });
                        }
                        _ => after.violation |= allowed,
                    }
                }
            }
            Event::Plan(changed) if before.active.is_some() && before.until > before.now => {
                let owner = before.active.unwrap();
                if !before.runs[owner as usize].as_ref().unwrap().planned {
                    let run = after.runs[owner as usize].as_mut().unwrap();
                    run.planned = true;
                    run.changed = *changed;
                    run.previous = before.reaction;
                    after.reaction = Some(owner);
                }
            }
            Event::LegacyDeliver(owner) => {
                // A frozen split-protocol writer may have committed under the
                // original or retry identity before losing its process. Bound
                // the environment to one such commit per logical change.
                if let Some(root) = reaction_root(before, *owner)
                    && before.notifications[root as usize] == 0
                {
                    after.delivered |= 1 << owner;
                    after.notifications[root as usize] = 1;
                }
            }
            Event::Complete(owner) | Event::WrongGeneration(owner)
                if before.runs[*owner as usize]
                    .as_ref()
                    .is_some_and(|r| r.planned) =>
            {
                let run = before.runs[*owner as usize].as_ref().unwrap();
                let claimed = run.generation + u8::from(matches!(event, Event::WrongGeneration(_)));
                // Match production require_run: historical run generation plus
                // current active token and lease. Same-generation takeover is
                // safe only if the token fence survives composition.
                let state = State {
                    generation: run.generation.into(),
                    lease: before.active.map(|active| Lease {
                        generation: run.generation.into(),
                        token: token(active),
                        identity: "definition".into(),
                        until: before.until.into(),
                    }),
                };
                let fenced = coordination::reduce(
                    &state,
                    &Input::Fence {
                        now: before.now.into(),
                        generation: run.generation.into(),
                        token: token(*owner),
                        identity: "definition".into(),
                    },
                ) == Ok(Effect::Fenced);
                let effect = watcher_schedule::complete(CompletionEvidence {
                    run_generation: run.generation.into(),
                    claimed_generation: claimed.into(),
                    pending_generation: before.pending.into(),
                });
                let accepted =
                    (fenced || self.fault) && matches!(effect, CompletionEffect::Complete { .. });
                let expected = before.active == Some(*owner)
                    && before.until > before.now
                    && claimed == run.generation;
                after.violation |= accepted != expected;
                if accepted {
                    let CompletionEffect::Complete {
                        generation,
                        clear_events,
                    } = effect
                    else {
                        unreachable!()
                    };
                    after.violation |= generation != i64::from(run.generation)
                        || clear_events != (before.pending <= run.generation);
                    if let Some(root) = reaction_root(before, *owner) {
                        // Independent root: a changed run owns its identity;
                        // an unchanged retry retains the prior changed run.
                        let expected_root = if run.changed {
                            Some(*owner)
                        } else {
                            run.previous.filter(|p| {
                                before.runs[*p as usize].as_ref().is_some_and(|r| r.changed)
                            })
                        };
                        after.violation |= Some(root) != expected_root;
                        let chain = (1 << owner)
                            | if run.changed {
                                0
                            } else {
                                run.previous.map_or(0, |previous| 1 << previous)
                            };
                        if before.delivered & chain == 0 {
                            after.delivered |= 1 << root;
                            after.notifications[root as usize] += 1;
                        }
                        after.violation |= after.notifications[root as usize] != 1;
                    } else if run.changed
                        || run.previous.is_some_and(|p| {
                            before.runs[p as usize].as_ref().is_some_and(|r| r.changed)
                        })
                    {
                        after.violation = true;
                    }
                    after.completed = generation as u8;
                    after.active = None;
                    after.reaction = None;
                    if clear_events {
                        after.event = false;
                    }
                }
            }
            _ => {}
        }
        if after.completed > after.pending || after.requested > after.pending {
            after.violation = true;
        }
        Ok(Transition::accepted(after, vec![format!("{event:?}")]))
    }
    fn check_state(&self, s: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.violation {
            Check::failed(
                "watcher.generation",
                "completion lost newer work, bypassed owner/lease fencing, or changed reaction identity",
            )
        } else {
            Check::passed("watcher.generation")
        }])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        Ok(vec![
            Event::Request,
            Event::Observe,
            Event::Tick,
            Event::Claim(0),
            Event::Claim(1),
            Event::Plan(false),
            Event::Plan(true),
            Event::LegacyDeliver(0),
            Event::LegacyDeliver(1),
            Event::Complete(0),
            Event::Complete(1),
            Event::WrongGeneration(0),
            Event::WrongGeneration(1),
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
            "watcher generation check failed/incomplete: {:?}",
            r.termination
        )
        .into());
    }
    println!(
        "watcher generation graph exhausted: {} states, {} transitions, zero skipped checks",
        r.states, r.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let f = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected watcher generation fault not found")?;
    let reduced = shrink(&model, &f, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("watcher generation shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-watcher-generation".into(),
        seed: None,
        parameters: vec![
            ("maximum-generation".into(), "3".into()),
            ("owners".into(), "2".into()),
            ("maximum-time".into(), "2".into()),
        ],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &f.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("watcher generation trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "watcher generation injected fault found and shrunk: {} -> {} inputs",
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
        return Err("watcher generation fresh replay did not match".into());
    }
    println!(
        "watcher generation fresh-process replay exact, failed property reproduced, matching build"
    );
    Ok(())
}
