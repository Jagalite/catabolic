// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded decision audit for shared physical rendition demand.
use catabolic_core::{
    encode,
    rendition::{self, Capacity, DemandPhase, Effect, Input, Rejection},
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
struct Adapter {
    fault: bool,
}
impl Model for Adapter {
    type State = Audited;
    type Input = Input;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "catabolic-rendition".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/rendition.rs").as_slice(),
                        include_bytes!("rendition.rs").as_slice(),
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
        input: &Input,
    ) -> Result<Transition<Audited, String>, ModelError> {
        let mut result = rendition::reduce(input);
        // Simulate reserving the same physical job a second time.
        if self.fault
            && let Input::Admit {
                existing_body_matches: None,
                capacity,
                ..
            } = input
            && capacity.job_reserved
            && let Ok(Effect::Attach { reserve, .. }) = &mut result
        {
            *reserve = true;
        }
        let mut after = before.clone();
        let violation = match (input, &result) {
            (Input::Cancel, result) if *result != Ok(Effect::CancelDemand) => {
                Some("rendition.caller_cancel")
            }
            (
                Input::Admit {
                    existing_body_matches: Some(true),
                    ..
                },
                result,
            ) if *result != Ok(Effect::ReuseDemand) => Some("rendition.idempotent_reuse"),
            (
                Input::Admit {
                    existing_body_matches: Some(false),
                    ..
                },
                result,
            ) if *result != Err(Rejection::IdempotencyConflict) => {
                Some("rendition.idempotent_conflict")
            }
            (
                Input::Admit {
                    existing_body_matches: None,
                    job_phase,
                    capacity,
                },
                Ok(Effect::Attach { demand, reserve }),
            ) => {
                let wanted = if job_phase == "complete" {
                    DemandPhase::Ready
                } else {
                    DemandPhase::parse(job_phase)
                };
                if *demand != wanted
                    || *reserve != (job_phase != "complete" && !capacity.job_reserved)
                {
                    Some("rendition.reservation")
                } else if capacity.principal_pending >= capacity.principal_limit
                    || capacity.global_pending >= capacity.global_limit
                    || (*reserve
                        && capacity
                            .available
                            .checked_sub(capacity.reserved)
                            .and_then(|v| v.checked_sub(u128::from(capacity.output_budget)))
                            .is_none_or(|v| v < u128::from(capacity.reserve_floor)))
                {
                    Some("rendition.capacity")
                } else {
                    None
                }
            }
            (
                Input::Admit {
                    existing_body_matches: None,
                    capacity,
                    ..
                },
                Err(Rejection::RequestLimit),
            ) if capacity.principal_pending < capacity.principal_limit
                && capacity.global_pending < capacity.global_limit =>
            {
                Some("rendition.capacity")
            }
            (
                Input::Status {
                    demand: DemandPhase::Cancelled,
                    ..
                },
                result,
            ) if *result != Ok(Effect::Status(DemandPhase::Cancelled)) => {
                Some("rendition.cancelled_status")
            }
            (
                Input::Status {
                    demand: DemandPhase::Ready,
                    output_present: false,
                    ..
                },
                result,
            ) if *result != Ok(Effect::Status(DemandPhase::Stale)) => {
                Some("rendition.stale_output")
            }
            (
                Input::Worker {
                    demand,
                    authorized: false,
                    ..
                },
                result,
            ) if *result
                != Ok(if *demand == DemandPhase::Cancelled {
                    Effect::Ignore
                } else {
                    Effect::BlockDemand
                }) =>
            {
                Some("rendition.authorization")
            }
            (Input::Retry { demand, .. }, result)
                if !matches!(
                    demand,
                    DemandPhase::Failed | DemandPhase::Cancelled | DemandPhase::Blocked
                ) && *result != Err(Rejection::NotRetryable) =>
            {
                Some("rendition.retry")
            }
            _ => None,
        };
        if let Some(id) = violation {
            after.violation = Some(id.into());
        }
        Ok(Transition::accepted(after, vec![format!("{result:?}")]))
    }
    fn check_state(&self, state: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok([
            "rendition.caller_cancel",
            "rendition.idempotent_reuse",
            "rendition.idempotent_conflict",
            "rendition.reservation",
            "rendition.capacity",
            "rendition.cancelled_status",
            "rendition.stale_output",
            "rendition.authorization",
            "rendition.retry",
        ]
        .iter()
        .map(|id| {
            if state.violation.as_deref() == Some(*id) {
                Check::failed(*id, "shared demand obligation violated")
            } else {
                Check::passed(*id)
            }
        })
        .collect())
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Input>, ModelError> {
        let mut inputs = vec![Input::Cancel];
        let demands = [
            DemandPhase::Queued,
            DemandPhase::Running,
            DemandPhase::Validating,
            DemandPhase::Ready,
            DemandPhase::Failed,
            DemandPhase::Cancelled,
            DemandPhase::Blocked,
            DemandPhase::Stale,
            DemandPhase::Other,
        ];
        for job in [
            "queued",
            "running",
            "complete",
            "failed",
            "timeout",
            "cancelled",
            "other",
        ] {
            for reserved in [false, true] {
                for full in [false, true] {
                    for available in [0, 1, 2, 3] {
                        let capacity = Capacity {
                            principal_pending: u64::from(full),
                            global_pending: 0,
                            principal_limit: 1,
                            global_limit: 1,
                            available,
                            reserved: 1,
                            output_budget: 1,
                            reserve_floor: 1,
                            job_reserved: reserved,
                        };
                        for existing in [None, Some(false), Some(true)] {
                            inputs.push(Input::Admit {
                                existing_body_matches: existing,
                                job_phase: job.into(),
                                capacity,
                            });
                        }
                        for demand in demands {
                            inputs.push(Input::Retry {
                                demand,
                                job_phase: job.into(),
                                capacity,
                            });
                        }
                    }
                }
            }
            for demand in demands {
                for authorized in [false, true] {
                    for operation_enabled in [false, true] {
                        for source_current in [false, true] {
                            inputs.push(Input::Worker {
                                demand,
                                job_phase: job.into(),
                                authorized,
                                operation_enabled,
                                source_current,
                            });
                        }
                    }
                }
            }
        }
        for demand in demands {
            for source_current in [false, true] {
                for output_present in [false, true] {
                    inputs.push(Input::Status {
                        demand,
                        source_current,
                        output_present,
                    });
                }
            }
        }
        Ok(inputs)
    }
}
fn codec(value: &impl Serialize) -> Result<Vec<u8>, ModelError> {
    Ok(
        encode(&serde_json::to_value(value).map_err(|e| ModelError::new(e.to_string()))?)
            .into_bytes(),
    )
}
impl ModelCodec for Adapter {
    fn encode_state(&self, v: &Audited) -> Result<Vec<u8>, ModelError> {
        codec(v)
    }
    fn decode_state(&self, v: &[u8]) -> Result<Audited, ModelError> {
        serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))
    }
    fn encode_input(&self, v: &Input) -> Result<Vec<u8>, ModelError> {
        codec(v)
    }
    fn decode_input(&self, v: &[u8]) -> Result<Input, ModelError> {
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
            "rendition verification failed/incomplete: {:?}",
            report.termination
        )
        .into());
    }
    println!(
        "production rendition graph exhausted: {} states, {} transitions, zero skipped checks",
        report.states, report.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let failure = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected rendition fault not found")?;
    let reduced = shrink(&model, &failure, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("rendition shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-rendition".into(),
        seed: None,
        parameters: vec![
            ("demand-phases".into(), "9".into()),
            ("job-phases".into(), "7".into()),
            ("available-bytes".into(), "0..3".into()),
        ],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &failure.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("rendition trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "rendition injected fault found and shrunk: {} -> {} inputs",
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
        return Err("rendition fresh replay did not match".into());
    }
    println!("rendition fresh-process replay exact, failed property reproduced, matching build");
    Ok(())
}
