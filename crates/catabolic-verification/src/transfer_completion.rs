// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded final-chunk release for full and partial transfers.
use catabolic_core::{
    encode, sha256,
    transfer_completion::{self, Effect, Verification},
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
    started: bool,
    required: bool,
    remaining: u8,
    released: u8,
    complete: bool,
    failed: bool,
    violation: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Event {
    Start { recorded: bool, full: bool },
    Chunk { digest_matches: bool },
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
            name: "catabolic-transfer-completion".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/transfer_completion.rs")
                            .as_slice(),
                        include_bytes!("transfer_completion.rs").as_slice(),
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
        let mut after = before.clone();
        match *event {
            Event::Start { recorded, full } if !before.started => {
                let result = transfer_completion::verification(
                    recorded,
                    if full { 0 } else { 1 },
                    2,
                    if full { 2 } else { 3 },
                );
                after.required = result == Verification::Required;
                if after.required != (recorded && full) {
                    after.violation = true;
                }
                after.started = true;
                after.remaining = 2;
            }
            Event::Chunk { digest_matches }
                if before.started && !before.failed && !before.complete =>
            {
                if before.remaining > 1 {
                    after.remaining -= 1;
                    after.released += 1;
                } else {
                    let required = if before.required {
                        Verification::Required
                    } else {
                        Verification::NotRequired
                    };
                    let evidence = if before.required {
                        Some(digest_matches)
                    } else {
                        None
                    };
                    let mut effect = transfer_completion::finish(required, evidence);
                    if self.fault && before.required && !digest_matches {
                        effect = Effect::Release;
                    }
                    if (effect == Effect::Release) != (!before.required || digest_matches) {
                        after.violation = true;
                    }
                    if effect == Effect::Release {
                        after.released += 1;
                        after.remaining = 0;
                        after.complete = true;
                    } else {
                        after.failed = true;
                    }
                }
            }
            _ => {}
        }
        if after.complete && (after.remaining != 0 || after.released != 2) {
            after.violation = true;
        }
        Ok(Transition::accepted(after, vec![format!("{event:?}")]))
    }
    fn check_state(&self, s: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.violation {
            Check::failed("transfer.final_chunk", "unverified full transfer completed")
        } else {
            Check::passed("transfer.final_chunk")
        }])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        Ok(vec![
            Event::Start {
                recorded: false,
                full: false,
            },
            Event::Start {
                recorded: false,
                full: true,
            },
            Event::Start {
                recorded: true,
                full: false,
            },
            Event::Start {
                recorded: true,
                full: true,
            },
            Event::Chunk {
                digest_matches: false,
            },
            Event::Chunk {
                digest_matches: true,
            },
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
            "transfer completion check failed/incomplete: {:?}",
            r.termination
        )
        .into());
    }
    println!(
        "transfer completion graph exhausted: {} states, {} transitions, zero skipped checks",
        r.states, r.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let f = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected transfer completion fault not found")?;
    let reduced = shrink(&model, &f, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("transfer completion shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-transfer-completion".into(),
        seed: None,
        parameters: vec![("chunks".into(), "2".into())],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &f.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("transfer completion trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "transfer completion injected fault found and shrunk: {} -> {} inputs",
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
        return Err("transfer completion fresh replay did not match".into());
    }
    println!(
        "transfer completion fresh-process replay exact, failed property reproduced, matching build"
    );
    Ok(())
}
