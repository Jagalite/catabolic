// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded continuation admission under changing parent, policy and source evidence.
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
    parent_finished: bool,
    policy_matches: bool,
    dirty_unchanged: bool,
    admitted: bool,
    violation: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Event {
    FinishParent,
    ChangePolicy,
    MarkDirty,
    Continue,
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
            name: "catabolic-scan-continuation".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
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
            parent_finished: false,
            policy_matches: true,
            dirty_unchanged: true,
            admitted: false,
            violation: false,
        })
    }
    fn step(
        &self,
        before: &Audited,
        event: &Event,
    ) -> Result<Transition<Audited, String>, ModelError> {
        let mut after = before.clone();
        match event {
            Event::FinishParent => after.parent_finished = true,
            Event::ChangePolicy => after.policy_matches = false,
            Event::MarkDirty => after.dirty_unchanged = false,
            Event::Continue if !before.admitted => {
                let dirty = before.dirty_unchanged || self.fault;
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
                    scan_publication::advance_continuation(f, Evidence::DirtyUnchanged(dirty))
                });
                let accepted = result == Ok(Fence::Ready);
                let expected =
                    before.parent_finished && before.policy_matches && before.dirty_unchanged;
                if accepted != expected {
                    after.violation = true;
                }
                if accepted {
                    after.admitted = true;
                }
            }
            _ => {}
        }
        Ok(Transition::accepted(after, vec![format!("{event:?}")]))
    }
    fn check_state(&self, s: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.violation {
            Check::failed(
                "scan.continuation",
                "continuation admitted stale or incomplete evidence",
            )
        } else {
            Check::passed("scan.continuation")
        }])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        Ok(vec![
            Event::FinishParent,
            Event::ChangePolicy,
            Event::MarkDirty,
            Event::Continue,
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
        parameters: vec![("source-dirty".into(), "boolean".into())],
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
