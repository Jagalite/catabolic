// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded audit of production fallback selection, failback, and admission.
use catabolic_core::{
    encode,
    resolution::{
        self, Candidate, Failback, FailbackEffect, FailbackMode, Observation, ProjectionAdmission,
        TierEffect,
    },
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
enum Event {
    Tier {
        observations: [u8; 3],
        same_rank: bool,
        tie_break: bool,
        package: bool,
    },
    Failback {
        mode: u8,
        manual: bool,
        improving: bool,
        live: bool,
        checks: u8,
        age: u8,
    },
    Projection {
        epoch_matches: bool,
        generation_matches: bool,
        safe: bool,
        changes: u8,
        removals: u8,
    },
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
            name: "catabolic-resolution".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/resolution.rs").as_slice(),
                        include_bytes!("resolution.rs").as_slice(),
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
        let output = match *event {
            Event::Tier {
                observations,
                same_rank,
                tie_break,
                package,
            } => {
                let candidates: Vec<_> = observations
                    .iter()
                    .enumerate()
                    .map(|(i, o)| Candidate {
                        file_id: format!("file-{i}"),
                        rank: vec![(0, if same_rank || i < 2 { 0.0 } else { 1.0 })],
                        observation: match o {
                            0 => Observation::Unchecked,
                            1 => Observation::Usable,
                            _ => Observation::Unavailable,
                        },
                    })
                    .collect();
                let mut effect = resolution::tier(&candidates, tie_break, package);
                if self.fault && observations[0] == 1 && !same_rank && observations[2] == 0 {
                    effect = TierEffect::Probe(2);
                }
                // Oracle: inspect candidates in rank groups, stopping at the first group with a usable item.
                let groups: Vec<Vec<usize>> = if same_rank {
                    vec![vec![0, 1, 2]]
                } else {
                    vec![vec![0, 1], vec![2]]
                };
                let mut expected = TierEffect::Unavailable;
                'group: for group in groups {
                    let mut usable = Vec::new();
                    for i in group {
                        match observations[i] {
                            0 => {
                                expected = TierEffect::Probe(i);
                                break 'group;
                            }
                            1 => {
                                usable.push(i);
                                if tie_break && !package {
                                    expected = TierEffect::Select(i);
                                    break 'group;
                                }
                            }
                            _ => {}
                        }
                    }
                    if !usable.is_empty() {
                        expected = if usable.len() == 1 || (tie_break && package) {
                            TierEffect::Select(usable[0])
                        } else {
                            TierEffect::Ambiguous
                        };
                        break;
                    }
                }
                if effect != expected {
                    after.violation = Some("resolution.tier".into());
                }
                format!("{effect:?}")
            }
            Event::Failback {
                mode,
                manual,
                improving,
                live,
                checks,
                age,
            } => {
                let mode = [
                    FailbackMode::Immediate,
                    FailbackMode::Stable,
                    FailbackMode::Manual,
                ][usize::from(mode)];
                let value = Failback {
                    mode,
                    manual_override: manual,
                    improving_tier: improving,
                    previous_eligible_and_live: live,
                    successful_checks: u64::from(checks),
                    minimum_successful_checks: 1,
                    healthy_since: 0.0,
                    now: f64::from(age),
                    minimum_healthy_seconds: 1,
                };
                let effect = resolution::failback(value);
                let retain = improving
                    && live
                    && !manual
                    && (mode == FailbackMode::Manual
                        || (mode == FailbackMode::Stable && (checks < 1 || age < 1)));
                if (effect == FailbackEffect::RetainPrevious) != retain {
                    after.violation = Some("resolution.failback".into());
                }
                format!("{effect:?}")
            }
            Event::Projection {
                epoch_matches,
                generation_matches,
                safe,
                changes,
                removals,
            } => {
                let input = ProjectionAdmission {
                    expected_epoch: 1,
                    current_epoch: if epoch_matches { 1 } else { 2 },
                    expected_generation: 1,
                    current_generation: if generation_matches { 1 } else { 2 },
                    safe,
                    changes: usize::from(changes),
                    removals: usize::from(removals),
                    max_changes: 1,
                    max_removals: 1,
                };
                let effect = resolution::projection(input);
                let expected_error = if !epoch_matches {
                    Some("stale_resolution_plan")
                } else if !generation_matches {
                    Some("stale_resolution_generation")
                } else if safe && (changes > 1 || removals > 1) {
                    Some("fallback_change_or_removal_budget")
                } else {
                    None
                };
                let ok = match expected_error {
                    Some(error) => effect == Err(error),
                    None => effect
                        .as_ref()
                        .is_ok_and(|e| e.generation == 2 && e.retain != safe),
                };
                if !ok {
                    after.violation = Some("resolution.projection".into());
                }
                format!("{effect:?}")
            }
        };
        Ok(Transition::accepted(after, vec![output]))
    }
    fn check_state(&self, state: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok([
            "resolution.tier",
            "resolution.failback",
            "resolution.projection",
        ]
        .iter()
        .map(|id| {
            if state.violation.as_deref() == Some(*id) {
                Check::failed(*id, "fallback decision obligation violated")
            } else {
                Check::passed(*id)
            }
        })
        .collect())
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        let mut events = Vec::new();
        for a in 0..3 {
            for b in 0..3 {
                for c in 0..3 {
                    for same_rank in [false, true] {
                        for tie_break in [false, true] {
                            for package in [false, true] {
                                events.push(Event::Tier {
                                    observations: [a, b, c],
                                    same_rank,
                                    tie_break,
                                    package,
                                });
                            }
                        }
                    }
                }
            }
        }
        for mode in 0..3 {
            for manual in [false, true] {
                for improving in [false, true] {
                    for live in [false, true] {
                        for checks in 0..2 {
                            for age in 0..2 {
                                events.push(Event::Failback {
                                    mode,
                                    manual,
                                    improving,
                                    live,
                                    checks,
                                    age,
                                });
                            }
                        }
                    }
                }
            }
        }
        for epoch_matches in [false, true] {
            for generation_matches in [false, true] {
                for safe in [false, true] {
                    for changes in 0..3 {
                        for removals in 0..3 {
                            events.push(Event::Projection {
                                epoch_matches,
                                generation_matches,
                                safe,
                                changes,
                                removals,
                            });
                        }
                    }
                }
            }
        }
        Ok(events)
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
    let report = enumerate(&Adapter { fault: false }, SearchConfig::default())?;
    if report.failure.is_some()
        || report.termination != SearchTermination::GraphExhausted
        || report.skipped_checks != 0
    {
        return Err(format!(
            "resolution verification failed/incomplete: {:?}",
            report.termination
        )
        .into());
    }
    println!(
        "production resolution graph exhausted: {} states, {} transitions, zero skipped checks",
        report.states, report.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let failure = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected resolution fault not found")?;
    let reduced = shrink(&model, &failure, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("resolution shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-resolution".into(),
        seed: None,
        parameters: vec![
            ("candidates".into(), "3".into()),
            ("observations".into(), "3".into()),
        ],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &failure.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("resolution trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "resolution injected fault found and shrunk: {} -> {} inputs",
        failure.inputs.len(),
        reduced.minimized.inputs.len()
    );
    Ok(())
}
pub fn replay_trace(path: &str) -> Result<(), Box<dyn Error>> {
    let trace = Trace::read_from(File::open(path)?, &ReadLimits::default())?;
    let result = replay(
        &Adapter { fault: true },
        &trace,
        ReplayOptions {
            allow_build_mismatch: false,
        },
    )?;
    if result.outcome != ReplayOutcome::Exact || !result.failure_reproduced || !result.build_matches
    {
        return Err("resolution fresh replay did not match".into());
    }
    println!("resolution fresh-process replay exact, failed property reproduced, matching build");
    Ok(())
}
