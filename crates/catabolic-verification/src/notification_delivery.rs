// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded notification acknowledgement and backoff decisions.
use catabolic_core::{
    encode,
    notification_delivery::{self, Outcome, State},
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
    violation: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Event {
    outcome: u8,
    attempts: u32,
    now: u8,
}
struct Adapter {
    fault: bool,
}
impl Model for Adapter {
    type State = Audited;
    type Input = Event;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "catabolic-notification-ack".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/notification_delivery.rs")
                            .as_slice(),
                        include_bytes!("notification_delivery.rs").as_slice(),
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
        let outcome = [
            Outcome::Complete,
            Outcome::TemporaryFailure,
            Outcome::Repair,
        ][usize::from(e.outcome)];
        let mut result = notification_delivery::acknowledge(outcome, e.attempts, f64::from(e.now));
        if self.fault
            && e.outcome == 1
            && e.attempts == 4
            && let Ok(decision) = &mut result
        {
            decision.state = State::Retry;
        }
        let wanted = match e.outcome {
            0 => State::Complete,
            1 if e.attempts >= 4 => State::Exhausted,
            1 => State::Retry,
            _ => State::Repair,
        };
        let delay = [
            5., 10., 20., 40., 80., 160., 320., 640., 1280., 2560., 3600.,
        ][usize::try_from(e.attempts.min(10)).unwrap()];
        let good = result
            .as_ref()
            .is_ok_and(|d| d.state == wanted && d.due_at == f64::from(e.now) + delay);
        Ok(Transition::accepted(
            Audited {
                violation: before.violation || !good,
            },
            vec![format!("{result:?}")],
        ))
    }
    fn check_state(&self, s: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.violation {
            Check::failed(
                "notification.acknowledgement",
                "acknowledgement or backoff obligation violated",
            )
        } else {
            Check::passed("notification.acknowledgement")
        }])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        let mut v = Vec::new();
        for outcome in 0..3 {
            for attempts in [0, 1, 2, 3, 4, 5, 9, 10, u32::MAX] {
                for now in [0, 1] {
                    v.push(Event {
                        outcome,
                        attempts,
                        now,
                    });
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
        return Err(format!("notification check failed/incomplete: {:?}", r.termination).into());
    }
    println!(
        "notification acknowledgement graph exhausted: {} states, {} transitions, zero skipped checks",
        r.states, r.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let f = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected notification fault not found")?;
    let reduced = shrink(&model, &f, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("notification shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-notification-ack".into(),
        seed: None,
        parameters: vec![
            ("outcomes".into(), "3".into()),
            ("attempt-values".into(), "9".into()),
        ],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &f.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("notification trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "notification injected fault found and shrunk: {} -> {} inputs",
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
        return Err("notification fresh replay did not match".into());
    }
    println!("notification fresh-process replay exact, failed property reproduced, matching build");
    Ok(())
}
