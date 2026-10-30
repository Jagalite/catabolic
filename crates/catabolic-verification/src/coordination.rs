// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded adapter for the production coordination lease reducer.
use catabolic_core::{
    coordination::{self, Effect, Generation, Input, Lease, State},
    encode, sha256,
};
use serde::{Deserialize, Serialize};
use stateless::automatic::Auto;
use stateless::execution::{ReplayOptions, ReplayOutcome, record, replay};
use stateless::explore::{SearchConfig, SearchTermination, ShrinkConfig, enumerate, shrink};
use stateless::trace::{ReadLimits, RunConfig, Termination, Trace};
use stateless::{Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition};
use std::{error::Error, fs::File, path::Path};

// All times, generations and identities here are deliberately finite. The
// store remains responsible for applying an acquired lease transactionally.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Audited {
    generation: i64,
    lease: Option<BoundedLease>,
    violation: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct BoundedLease {
    generation: i64,
    token: u8,
    identity: u8,
    until: u8,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Event {
    Acquire {
        now: u8,
        token: u8,
        identity: u8,
        alive: bool,
        resume: bool,
    },
    Fence {
        now: u8,
        generation: i64,
        token: u8,
        identity: u8,
    },
    Cancel {
        active: bool,
    },
}
struct Adapter {
    fault: bool,
}
fn text(n: u8) -> String {
    format!("id-{n}")
}
impl Model for Adapter {
    type State = Audited;
    type Input = Event;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "catabolic-coordination".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/coordination.rs").as_slice(),
                        include_bytes!("coordination.rs").as_slice(),
                        include_bytes!("../../../Cargo.lock").as_slice(),
                    ]
                    .concat(),
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
        let state = State {
            generation: before.generation,
            lease: before.lease.as_ref().map(|l| Lease {
                generation: l.generation,
                token: text(l.token),
                identity: text(l.identity),
                until: f64::from(l.until),
            }),
        };
        let input = match *event {
            Event::Acquire {
                now,
                token,
                identity,
                alive,
                resume,
            } => Input::Acquire {
                now: f64::from(now),
                duration: 1.0,
                generation: if resume {
                    Generation::Resume
                } else {
                    Generation::Next
                },
                token: text(token),
                identity: text(identity),
                owner_alive: alive,
            },
            Event::Fence {
                now,
                generation,
                token,
                identity,
            } => Input::Fence {
                now: f64::from(now),
                generation,
                token: text(token),
                identity: text(identity),
            },
            Event::Cancel { active } => Input::CancelCaller { active },
        };
        let mut result = coordination::reduce(&state, &input);
        // Fault only the adapter: let an obsolete claim pass its fence.
        if self.fault
            && before.lease.is_some()
            && matches!(event, Event::Fence { .. })
            && result.is_err()
        {
            result = Ok(Effect::Fenced);
        }
        let mut after = before.clone();
        match *event {
            Event::Acquire {
                now,
                token,
                identity,
                alive,
                resume,
            } => {
                let live = alive && before.lease.as_ref().is_some_and(|lease| lease.until > now);
                let valid = !live && (!resume || before.generation > 0);
                let expected_gen = if resume {
                    before.generation
                } else {
                    before.generation + 1
                };
                let expected = Effect::Acquire(Lease {
                    generation: expected_gen,
                    token: text(token),
                    identity: text(identity),
                    until: f64::from(now) + 1.0,
                });
                if (valid && result.as_ref() != Ok(&expected)) || (!valid && result.is_ok()) {
                    after.violation = Some("coordination.acquire".into());
                }
                if let Ok(Effect::Acquire(lease)) = &result {
                    // Stop advancing beyond the bounded two-generation domain.
                    if lease.generation <= 2 {
                        after.generation = lease.generation;
                        after.lease = Some(BoundedLease {
                            generation: lease.generation,
                            token,
                            identity,
                            until: now + 1,
                        });
                    }
                }
            }
            Event::Fence {
                now,
                generation,
                token,
                identity,
            } => {
                let valid = generation > 0
                    && generation == before.generation
                    && before.lease.as_ref().is_some_and(|lease| {
                        lease.generation == generation
                            && lease.token == token
                            && lease.identity == identity
                            && lease.until > now
                    });
                if (result == Ok(Effect::Fenced)) != valid {
                    after.violation = Some("coordination.fence".into());
                }
            }
            Event::Cancel { active } => {
                if result != Ok(Effect::CancelCaller { changed: active }) {
                    after.violation = Some("coordination.cancel".into());
                }
            }
        }
        Ok(Transition::accepted(after, vec![format!("{result:?}")]))
    }
    fn check_state(&self, state: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok([
            "coordination.acquire",
            "coordination.fence",
            "coordination.cancel",
        ]
        .iter()
        .map(|id| {
            if state.violation.as_deref() == Some(*id) {
                Check::failed(
                    *id,
                    "coordination reducer violated independent lease obligation",
                )
            } else {
                Check::passed(*id)
            }
        })
        .collect())
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, state: &Audited) -> Result<Vec<Event>, ModelError> {
        let mut events = vec![
            Event::Cancel { active: false },
            Event::Cancel { active: true },
        ];
        for now in 0..=2 {
            for token in 0..=1 {
                for identity in 0..=1 {
                    for generation in 0..=2 {
                        events.push(Event::Fence {
                            now,
                            generation,
                            token,
                            identity,
                        });
                    }
                    for alive in [false, true] {
                        for resume in [false, true] {
                            if resume || state.generation < 2 {
                                events.push(Event::Acquire {
                                    now,
                                    token,
                                    identity,
                                    alive,
                                    resume,
                                });
                            }
                        }
                    }
                }
            }
        }
        Ok(events)
    }
}
fn codec(value: &impl Serialize) -> Result<Vec<u8>, ModelError> {
    Ok(
        encode(&serde_json::to_value(value).map_err(|e| ModelError::new(e.to_string()))?)
            .into_bytes(),
    )
}
impl ModelCodec for Adapter {
    fn encode_state(&self, value: &Audited) -> Result<Vec<u8>, ModelError> {
        codec(value)
    }
    fn decode_state(&self, value: &[u8]) -> Result<Audited, ModelError> {
        serde_json::from_slice(value).map_err(|e| ModelError::new(e.to_string()))
    }
    fn encode_input(&self, value: &Event) -> Result<Vec<u8>, ModelError> {
        codec(value)
    }
    fn decode_input(&self, value: &[u8]) -> Result<Event, ModelError> {
        serde_json::from_slice(value).map_err(|e| ModelError::new(e.to_string()))
    }
    fn encode_output(&self, value: &String) -> Result<Vec<u8>, ModelError> {
        codec(value)
    }
}
pub fn check() -> Result<(), Box<dyn Error>> {
    let report = enumerate(&Adapter { fault: false }, SearchConfig::default())?;
    if report.failure.is_some()
        || report.termination != SearchTermination::GraphExhausted
        || report.skipped_checks != 0
    {
        return Err(format!(
            "coordination verification failed/incomplete: {:?}",
            report.termination
        )
        .into());
    }
    println!(
        "production coordination graph exhausted: {} states, {} transitions, zero skipped checks",
        report.states, report.transitions
    );
    Ok(())
}

pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let report = enumerate(&model, SearchConfig::default())?;
    let failure = report
        .failure
        .ok_or("injected coordination fault not found")?;
    let minimized = shrink(&model, &failure, ShrinkConfig::default())?;
    if !minimized.validated_original {
        return Err("coordination shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-coordination".into(),
        seed: None,
        parameters: vec![
            ("generations".into(), "0..2".into()),
            ("times".into(), "0..2".into()),
            ("tokens-and-identities".into(), "0..1".into()),
        ],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &failure.inputs),
        ("minimized", &minimized.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("coordination trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "coordination injected fault found and shrunk: {} -> {} inputs",
        failure.inputs.len(),
        minimized.minimized.inputs.len()
    );
    Ok(())
}
pub fn replay_trace(path: &str) -> Result<(), Box<dyn Error>> {
    let trace = Trace::read_from(File::open(path)?, &ReadLimits::default())?;
    let report = replay(
        &Adapter { fault: true },
        &trace,
        ReplayOptions {
            allow_build_mismatch: false,
        },
    )?;
    if report.outcome != ReplayOutcome::Exact || !report.failure_reproduced || !report.build_matches
    {
        return Err("coordination fresh replay did not match".into());
    }
    println!("coordination fresh-process replay exact, failed property reproduced, matching build");
    Ok(())
}
