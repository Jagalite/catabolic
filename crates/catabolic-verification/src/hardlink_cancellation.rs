// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded hardlink cancellation and staged-reference preservation.
use catabolic_core::{
    encode,
    publication_recovery::{self, HardlinkCancellation},
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
struct Evidence {
    untouched: bool,
    absent: bool,
    retained: bool,
    force: bool,
    obsolete: bool,
    staged: bool,
}
struct Adapter {
    fault: bool,
}
impl Model for Adapter {
    type State = Audited;
    type Input = Evidence;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "catabolic-hardlink-cancellation".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/publication_recovery.rs")
                            .as_slice(),
                        include_bytes!("hardlink_cancellation.rs").as_slice(),
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
        e: &Evidence,
    ) -> Result<Transition<Audited, String>, ModelError> {
        let mut after = before.clone();
        let layout =
            publication_recovery::hardlink_cancellable_layout(e.untouched, e.absent, e.retained);
        let effect = if layout {
            publication_recovery::hardlink_cancellation(
                e.force,
                e.obsolete,
                e.staged && !self.fault,
            )
        } else {
            HardlinkCancellation::KeepIntent
        };
        let expected = if !(e.untouched || (e.absent && e.retained)) || !(e.force || e.obsolete) {
            HardlinkCancellation::KeepIntent
        } else if e.staged {
            HardlinkCancellation::PreserveStaged
        } else {
            HardlinkCancellation::Cancel
        };
        if effect != expected {
            after.violation = true;
        }
        Ok(Transition::accepted(after, vec![format!("{effect:?}")]))
    }
    fn check_state(&self, s: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.violation {
            Check::failed(
                "hardlink.cancellation",
                "staged link or live intent was not preserved",
            )
        } else {
            Check::passed("hardlink.cancellation")
        }])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Evidence>, ModelError> {
        let mut inputs = Vec::new();
        for mask in 0..64 {
            inputs.push(Evidence {
                untouched: mask & 1 != 0,
                absent: mask & 2 != 0,
                retained: mask & 4 != 0,
                force: mask & 8 != 0,
                obsolete: mask & 16 != 0,
                staged: mask & 32 != 0,
            });
        }
        Ok(inputs)
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
    fn encode_input(&self, v: &Evidence) -> Result<Vec<u8>, ModelError> {
        codec(v)
    }
    fn decode_input(&self, v: &[u8]) -> Result<Evidence, ModelError> {
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
            "hardlink cancellation check failed/incomplete: {:?}",
            r.termination
        )
        .into());
    }
    println!(
        "hardlink cancellation graph exhausted: {} states, {} transitions, zero skipped checks",
        r.states, r.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let f = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected hardlink cancellation fault not found")?;
    let reduced = shrink(&model, &f, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("hardlink cancellation shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-hardlink-cancellation".into(),
        seed: None,
        parameters: vec![("staged-link".into(), "boolean".into())],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &f.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("hardlink cancellation trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "hardlink cancellation injected fault found and shrunk: {} -> {} inputs",
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
        return Err("hardlink cancellation fresh replay did not match".into());
    }
    println!(
        "hardlink cancellation fresh-process replay exact, failed property reproduced, matching build"
    );
    Ok(())
}
