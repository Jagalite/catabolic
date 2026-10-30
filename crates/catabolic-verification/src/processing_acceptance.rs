// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded acceptance, baseline and retry decision audit.
use catabolic_core::{
    encode,
    execution::Phase,
    processing_acceptance::{self, BaselineAction, SourceAction},
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
    phase: u8,
    invalid: bool,
    baseline: u8,
    retry: bool,
    attempt: i64,
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
            name: "catabolic-processing-acceptance".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/processing_acceptance.rs")
                            .as_slice(),
                        include_bytes!("processing_acceptance.rs").as_slice(),
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
            Phase::Complete,
            Phase::Failed,
            Phase::Timeout,
            Phase::Mismatch,
            Phase::Changed,
        ][usize::from(e.phase)];
        let source = processing_acceptance::source_action(phase, e.invalid);
        let baseline = [None, Some("same"), Some("old")][usize::from(e.baseline)];
        let mut checksum = processing_acceptance::baseline_action(baseline, "same");
        if self.fault && e.baseline == 2 {
            checksum = BaselineAction::Establish;
        }
        let retry = processing_acceptance::retry_decision(phase, e.retry, e.attempt, 5, 100.0);
        let expected_baseline = [
            BaselineAction::Establish,
            BaselineAction::Preserve,
            BaselineAction::Mismatch,
        ][usize::from(e.baseline)];
        let retryable = matches!(phase, Phase::Failed | Phase::Timeout) && e.retry;
        let delay = 5u32
            .saturating_mul(1u32 << e.attempt.saturating_sub(1).clamp(0, 12) as u32)
            .min(3600);
        let good = (source == SourceAction::Validate) == (phase == Phase::Complete || e.invalid)
            && checksum == expected_baseline
            && retry.retryable == retryable
            && retry.enqueue == (retryable && e.attempt <= 10)
            && retry.due_at == 100.0 + f64::from(delay);
        Ok(Transition::accepted(
            Audited {
                violation: before.violation || !good,
            },
            vec![format!("{source:?}/{checksum:?}/{retry:?}")],
        ))
    }
    fn check_state(&self, s: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.violation {
            Check::failed(
                "processing.acceptance",
                "acceptance decision violated baseline or retry obligations",
            )
        } else {
            Check::passed("processing.acceptance")
        }])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        let mut v = Vec::new();
        for phase in 0..5 {
            for invalid in [false, true] {
                for baseline in 0..3 {
                    for retry in [false, true] {
                        for attempt in [1, 2, 10, 11] {
                            v.push(Event {
                                phase,
                                invalid,
                                baseline,
                                retry,
                                attempt,
                            });
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
            "processing acceptance check failed/incomplete: {:?}",
            r.termination
        )
        .into());
    }
    println!(
        "processing acceptance graph exhausted: {} states, {} transitions, zero skipped checks",
        r.states, r.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let f = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected processing fault not found")?;
    let reduced = shrink(&model, &f, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("processing shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-processing-acceptance".into(),
        seed: None,
        parameters: vec![
            ("phases".into(), "5".into()),
            ("baselines".into(), "3".into()),
            ("attempts".into(), "1,2,10,11".into()),
        ],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &f.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("processing trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "processing injected fault found and shrunk: {} -> {} inputs",
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
        return Err("processing fresh replay did not match".into());
    }
    println!("processing fresh-process replay exact, failed property reproduced, matching build");
    Ok(())
}
