// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded hardlink post-effect acknowledgement and retained-reference checks.
use catabolic_core::{
    encode,
    publication_recovery::{self, Effect, HardlinkEvidence},
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
    started: bool,
    owner: bool,
    destination: bool,
    retained: bool,
    retention_required: bool,
    journal: bool,
    violation: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Event {
    Start(bool),
    LoseOwner,
    ChangeDestination,
    ChangeRetained,
    Recover,
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
            name: "catabolic-hardlink-acknowledgement".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/publication_recovery.rs")
                            .as_slice(),
                        include_bytes!("hardlink_recovery.rs").as_slice(),
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
        match event {
            Event::Start(required) if !before.started => {
                after.started = true;
                after.owner = true;
                after.destination = true;
                after.retained = *required;
                after.retention_required = *required;
                after.journal = true;
            }
            Event::LoseOwner if before.started => after.owner = false,
            Event::ChangeDestination if before.started => after.destination = false,
            Event::ChangeRetained if before.started => after.retained = false,
            Event::Recover if before.started && before.journal => {
                let evidence = HardlinkEvidence {
                    owner_matches: before.owner,
                    destination_matches: before.destination,
                    retention_required: before.retention_required,
                    retention_matches: before.retained,
                };
                let mut effect = publication_recovery::acknowledge_hardlink(evidence);
                if self.fault
                    && before.owner
                    && before.destination
                    && before.retention_required
                    && !before.retained
                {
                    effect = Effect::Acknowledge;
                }
                let expected = before.owner
                    && before.destination
                    && (!before.retention_required || before.retained);
                if (effect == Effect::Acknowledge) != expected {
                    after.violation = true;
                }
                if effect == Effect::Acknowledge {
                    after.journal = false;
                }
            }
            _ => {}
        }
        Ok(Transition::accepted(after, vec![format!("{event:?}")]))
    }
    fn check_state(&self, s: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.violation {
            Check::failed(
                "hardlink.acknowledgement",
                "journal cleared without expected output or retained predecessor",
            )
        } else {
            Check::passed("hardlink.acknowledgement")
        }])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        Ok(vec![
            Event::Start(false),
            Event::Start(true),
            Event::LoseOwner,
            Event::ChangeDestination,
            Event::ChangeRetained,
            Event::Recover,
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
            "hardlink acknowledgement check failed/incomplete: {:?}",
            r.termination
        )
        .into());
    }
    println!(
        "hardlink acknowledgement graph exhausted: {} states, {} transitions, zero skipped checks",
        r.states, r.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let f = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected hardlink fault not found")?;
    let reduced = shrink(&model, &f, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("hardlink shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-hardlink-acknowledgement".into(),
        seed: None,
        parameters: vec![("retention-required".into(), "both".into())],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &f.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("hardlink trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "hardlink injected fault found and shrunk: {} -> {} inputs",
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
        return Err("hardlink fresh replay did not match".into());
    }
    println!("hardlink fresh-process replay exact, failed property reproduced, matching build");
    Ok(())
}
