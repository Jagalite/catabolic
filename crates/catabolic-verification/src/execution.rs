// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded production execution-decision audit; SQL effects remain in the store.
use catabolic_core::{
    encode,
    execution::{self, Effect, Input, Lease, Phase, Proof, Rejection, State},
    sha256,
};
use serde::{Deserialize, Serialize};
use stateless::automatic::Auto;
use stateless::execution::{ReplayOptions, ReplayOutcome, record, replay};
use stateless::explore::{SearchConfig, SearchTermination, ShrinkConfig, enumerate, shrink};
use stateless::trace::{ReadLimits, RunConfig, Termination, Trace};
use stateless::{Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition};
use std::{error::Error, fs::File, path::Path};
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Audited {
    violation: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Event {
    phase: u8,
    attempts: u8,
    has_claim: bool,
    alive: bool,
    expired: bool,
    proof_change: u8,
    operation: u8,
    pending: bool,
}
struct Adapter {
    fault: bool,
}
fn proof(attempt: i64) -> Proof {
    Proof {
        generation: 1,
        attempt,
        token_digest: "token".into(),
        snapshot_digest: "snapshot".into(),
        options_digest: "options".into(),
    }
}
impl Model for Adapter {
    type State = Audited;
    type Input = Event;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "catabolic-execution".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/execution.rs").as_slice(),
                        include_bytes!("execution.rs").as_slice(),
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
    fn step(&self, before: &Audited, e: &Event) -> Result<Transition<Audited, String>, ModelError> {
        let phase = [
            Phase::Queued,
            Phase::Running,
            Phase::Complete,
            Phase::Failed,
            Phase::Timeout,
            Phase::Cancelled,
        ][usize::from(e.phase)];
        let attempts = i64::from(e.attempts);
        let mut supplied = proof(attempts);
        match e.proof_change {
            1 => supplied.generation = 2,
            2 => supplied.attempt += 1,
            3 => supplied.token_digest = "other".into(),
            4 => supplied.snapshot_digest = "other".into(),
            5 => supplied.options_digest = "other".into(),
            _ => {}
        }
        let state = State {
            phase,
            attempts,
            snapshot_digest: "snapshot".into(),
            options_digest: "options".into(),
            claim: e.has_claim.then(|| Lease {
                proof: proof(attempts),
                until: if e.expired { 1.0 } else { 2.0 },
                worker_alive: e.alive,
            }),
        };
        let input = match e.operation {
            0 => Input::Start {
                now: 1.0,
                duration: 1.0,
            },
            1 => Input::Recover {
                now: 1.0,
                duration: 1.0,
                attempt: attempts,
            },
            2 => Input::Fence {
                now: 1.0,
                proof: supplied,
            },
            3 => Input::Renew {
                now: 1.0,
                duration: 1.0,
                proof: supplied,
            },
            4 => Input::Finish {
                now: 1.0,
                proof: supplied,
                outcome: Phase::Complete,
            },
            5 => Input::Finish {
                now: 1.0,
                proof: supplied,
                outcome: Phase::Running,
            },
            6 => Input::Cancel {
                publication_intent: e.pending,
            },
            7 => Input::Retry {
                pending_artifact: e.pending,
            },
            _ => Input::Reclaim { now: 1.0 },
        };
        let mut result = execution::reduce(&state, &input);
        if self.fault && e.operation == 4 && result == Err(Rejection::StaleClaim) {
            result = Ok(Effect::Finish {
                outcome: Phase::Complete,
            });
        }
        let live = e.has_claim && e.alive && !e.expired;
        let fence = phase == Phase::Running && attempts > 0 && live && e.proof_change == 0;
        let expected = match e.operation {
            0 if live => Err(Rejection::LiveOwner),
            0 if !matches!(phase, Phase::Queued | Phase::Running) => Err(Rejection::NotRunnable),
            0 => Ok(Effect::Acquire {
                generation: if e.has_claim { 2 } else { 1 },
                attempt: attempts + 1,
                until: 2.0,
            }),
            1 if live => Err(Rejection::LiveOwner),
            1 if phase != Phase::Running || attempts == 0 => Err(Rejection::NotRunnable),
            1 => Ok(Effect::Acquire {
                generation: if e.has_claim { 2 } else { 1 },
                attempt: attempts,
                until: 2.0,
            }),
            2 if fence => Ok(Effect::Fenced),
            2 => Err(Rejection::StaleClaim),
            3 if fence => Ok(Effect::Renew { until: 2.0 }),
            3 => Err(Rejection::StaleClaim),
            4 if fence => Ok(Effect::Finish {
                outcome: Phase::Complete,
            }),
            4 => Err(Rejection::StaleClaim),
            5 if fence => Err(Rejection::InvalidOutcome),
            5 => Err(Rejection::StaleClaim),
            6 if e.pending => Err(Rejection::PublicationPending),
            6 => Ok(Effect::Cancel {
                changed: matches!(phase, Phase::Queued | Phase::Running),
            }),
            7 if e.pending => Err(Rejection::PublicationPending),
            7 if matches!(phase, Phase::Failed | Phase::Timeout | Phase::Cancelled) => {
                Ok(Effect::Requeue)
            }
            7 => Err(Rejection::NotRetryable),
            _ if live => Err(Rejection::LiveOwner),
            _ if phase == Phase::Running => Ok(Effect::Requeue),
            _ => Err(Rejection::NotRunnable),
        };
        let mut after = before.clone();
        if result != expected {
            after.violation = Some("execution.decision".into());
        }
        Ok(Transition::accepted(after, vec![format!("{result:?}")]))
    }
    fn check_state(&self, state: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if state.violation.is_some() {
            Check::failed(
                "execution.decision",
                "execution decision violated the independent claim and publication obligations",
            )
        } else {
            Check::passed("execution.decision")
        }])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        let mut v = Vec::new();
        for phase in 0..6 {
            for attempts in 0..=1 {
                for has_claim in [false, true] {
                    for alive in [false, true] {
                        for expired in [false, true] {
                            for proof_change in 0..6 {
                                for operation in 0..9 {
                                    for pending in [false, true] {
                                        v.push(Event {
                                            phase,
                                            attempts,
                                            has_claim,
                                            alive,
                                            expired,
                                            proof_change,
                                            operation,
                                            pending,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(v)
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
            "execution verification failed/incomplete: {:?}",
            r.termination
        )
        .into());
    }
    println!(
        "production execution graph exhausted: {} states, {} transitions, zero skipped checks",
        r.states, r.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let failure = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected execution fault not found")?;
    let reduced = shrink(&model, &failure, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("execution shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-execution".into(),
        seed: None,
        parameters: vec![
            ("phases".into(), "6".into()),
            ("proof-variants".into(), "6".into()),
            ("operations".into(), "9".into()),
        ],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &failure.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("execution trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "execution injected fault found and shrunk: {} -> {} inputs",
        failure.inputs.len(),
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
        return Err("execution fresh replay did not match".into());
    }
    println!("execution fresh-process replay exact, failed property reproduced, matching build");
    Ok(())
}
