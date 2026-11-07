// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded watcher reaction retries, malformed links and deduplicated delivery.
use catabolic_core::{
    encode, sha256,
    watcher_schedule::{self, ReactionStep},
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
    pending: bool,
    depth: u8,
    corrupt_at: Option<u8>,
    delivered: bool,
    violation: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Event {
    Change,
    Retry,
    Corrupt,
    Deliver,
    Complete,
}
struct Adapter {
    fault: bool,
}
// Model three records: an original change and up to two unchanged retries.
// Durable lookup, arbitrary malformed graphs, and transactional admission are
// covered separately by the store crash tests, not this bounded adapter.
fn chain_valid(s: &Audited, fault: bool) -> bool {
    let mut current = s.depth;
    loop {
        let is_corrupt = s.corrupt_at == Some(current);
        let previous = if current == 0 { None } else { Some("previous") };
        let link_type = if is_corrupt && !fault {
            Some("integer")
        } else if previous.is_some() {
            Some("text")
        } else {
            Some("null")
        };
        match watcher_schedule::reaction_step(
            Some("planned"),
            Some(if current == 0 { "true" } else { "false" }),
            link_type,
            previous,
        ) {
            ReactionStep::Changed => return true,
            ReactionStep::Previous(_) if current > 0 => current -= 1,
            ReactionStep::NoChange | ReactionStep::Reject | ReactionStep::Previous(_) => {
                return false;
            }
        }
    }
}
impl Model for Adapter {
    type State = Audited;
    type Input = Event;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "catabolic-watcher-reaction".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/watcher_schedule.rs").as_slice(),
                        include_bytes!("watcher_reaction.rs").as_slice(),
                        include_bytes!("../../../Cargo.lock").as_slice(),
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
        let mut after = before.clone();
        match event {
            Event::Change if !before.pending => {
                after.pending = true;
                after.depth = 0;
                after.corrupt_at = None;
                after.delivered = false;
            }
            Event::Retry if before.pending && before.depth < 2 => after.depth += 1,
            Event::Corrupt if before.pending && before.corrupt_at.is_none() => {
                after.corrupt_at = Some(before.depth)
            }
            Event::Deliver if before.pending => {
                let valid = chain_valid(before, self.fault);
                let expected_valid = before.corrupt_at.is_none();
                let admitted = valid && !before.delivered;
                let expected_admitted = expected_valid && !before.delivered;
                if valid != expected_valid || admitted != expected_admitted {
                    after.violation = true;
                }
                if admitted {
                    after.delivered = true;
                }
            }
            Event::Complete if before.pending && chain_valid(before, false) => {
                after.pending = false
            }
            _ => {}
        }
        Ok(Transition::accepted(after, vec![format!("{event:?}")]))
    }
    fn check_state(&self, s: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.violation {
            Check::failed(
                "watcher.reaction",
                "invalid chain accepted or retry delivery duplicated",
            )
        } else {
            Check::passed("watcher.reaction")
        }])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        Ok(vec![
            Event::Change,
            Event::Retry,
            Event::Corrupt,
            Event::Deliver,
            Event::Complete,
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
            "watcher reaction check failed/incomplete: {:?}",
            r.termination
        )
        .into());
    }
    println!(
        "watcher reaction graph exhausted: {} states, {} transitions, zero skipped checks",
        r.states, r.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let f = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected watcher reaction fault not found")?;
    let reduced = shrink(&model, &f, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("watcher reaction shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-watcher-reaction".into(),
        seed: None,
        parameters: vec![("malformed-link".into(), "boolean".into())],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &f.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("watcher reaction trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "watcher reaction injected fault found and shrunk: {} -> {} inputs",
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
        return Err("watcher reaction fresh replay did not match".into());
    }
    println!(
        "watcher reaction fresh-process replay exact, failed property reproduced, matching build"
    );
    Ok(())
}
