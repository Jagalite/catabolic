//! Bounded Statelessness adapter around the application-owned production reducer.
use catabolic_core::{
    encode,
    lifecycle::{Caller, Effect, Input, State, reduce},
    sha256,
};
use serde::{Deserialize, Serialize};
use stateless::{Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition};
use stateless::{
    automatic::Auto,
    execution::{ReplayOptions, ReplayOutcome, record, replay},
    explore::{FuzzConfig, SearchConfig, SearchTermination, ShrinkConfig, enumerate, fuzz, shrink},
    trace::{ReadLimits, RunConfig, Termination, Trace},
};
use std::{error::Error, fs::File, path::Path, process::ExitCode};
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
struct Audited {
    state: State,
    violation: Option<String>,
}
struct Adapter {
    fault: bool,
}
impl Model for Adapter {
    type State = Audited;
    type Input = Input;
    type Output = Effect;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "catabolic-shared-request".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/lifecycle.rs").as_slice(),
                        include_bytes!("main.rs").as_slice()
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
    ) -> Result<Transition<Audited, Effect>, ModelError> {
        let (state, mut effects) = reduce(&before.state, input);
        // Deliberately injected adapter fault, outside the production reducer:
        // a stale callback delivers to the first caller despite its fence/state.
        if self.fault
            && before.state.started
            && let Input::Complete(generation) = input
            && (*generation != before.state.generation || !before.state.running)
        {
            effects.push(Effect::Deliver {
                caller: 0,
                generation: *generation,
            });
        }
        let mut violation = before.violation.clone();
        for effect in &effects {
            match effect {
                Effect::Deliver {
                    caller: _,
                    generation,
                } if *generation != before.state.generation => {
                    violation = Some("request.stale_completion".into())
                }
                Effect::Deliver {
                    caller,
                    generation: _,
                } if before.state.callers[usize::from(*caller)] != Caller::Waiting => {
                    violation = Some("request.cancelled_delivery".into())
                }
                Effect::Stop(_)
                    if matches!(input, Input::Cancel(_))
                        && state.callers.contains(&Caller::Waiting) =>
                {
                    violation = Some("request.cancel_stops_sibling".into())
                }
                _ => {}
            }
        }
        Ok(Transition::accepted(Audited { state, violation }, effects))
    }
    fn check_state(&self, state: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok([
            "request.stale_completion",
            "request.cancelled_delivery",
            "request.cancel_stops_sibling",
        ]
        .iter()
        .map(|property| {
            if state.violation.as_deref() == Some(property) {
                Check::failed(
                    *property,
                    "effect violates the independent caller/fence audit",
                )
            } else {
                Check::passed(*property)
            }
        })
        .collect())
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Input>, ModelError> {
        Ok(vec![
            Input::Attach(0),
            Input::Attach(1),
            Input::Cancel(0),
            Input::Cancel(1),
            Input::Advance(1),
            Input::Complete(0),
            Input::Complete(1),
        ])
    }
}
impl ModelCodec for Adapter {
    fn encode_state(&self, value: &Audited) -> Result<Vec<u8>, ModelError> {
        codec(value)
    }
    fn decode_state(&self, value: &[u8]) -> Result<Audited, ModelError> {
        serde_json::from_slice(value).map_err(|e| ModelError::new(e.to_string()))
    }
    fn encode_input(&self, value: &Input) -> Result<Vec<u8>, ModelError> {
        codec(value)
    }
    fn decode_input(&self, value: &[u8]) -> Result<Input, ModelError> {
        serde_json::from_slice(value).map_err(|e| ModelError::new(e.to_string()))
    }
    fn encode_output(&self, value: &Effect) -> Result<Vec<u8>, ModelError> {
        codec(value)
    }
}
fn codec(value: &impl Serialize) -> Result<Vec<u8>, ModelError> {
    let value = serde_json::to_value(value).map_err(|e| ModelError::new(e.to_string()))?;
    Ok(encode(&value).into_bytes())
}
fn save(path: &Path, trace: &Trace) -> Result<(), Box<dyn Error>> {
    let mut file = File::create_new(path)?;
    trace.write_to(&mut file)?;
    file.sync_all()?;
    Ok(())
}
fn run(args: &[String]) -> Result<u8, Box<dyn Error>> {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["check"] => {
            let report = enumerate(&Adapter { fault: false }, SearchConfig::default())?;
            if report.failure.is_some()
                || report.termination != SearchTermination::GraphExhausted
                || report.skipped_checks != 0
            {
                return Err(
                    format!("incomplete/failed verification: {:?}", report.termination).into(),
                );
            }
            println!(
                "graph exhausted: {} states, {} transitions, zero skipped checks",
                report.states, report.transitions
            );
            Ok(0)
        }
        ["find", directory] => {
            let model = Auto::new(Adapter { fault: true });
            let config = FuzzConfig {
                seed: 453,
                cases: 100,
                max_steps: 30,
                ..Default::default()
            };
            let report = fuzz(&model, config.clone())?;
            let failure = report.failure.ok_or("injected fault not found")?;
            std::fs::create_dir(directory)?;
            let run = RunConfig {
                strategy: "catabolic-shared-request".into(),
                seed: Some(config.seed),
                parameters: vec![
                    ("cases".into(), "100".into()),
                    ("max_steps".into(), "30".into()),
                    ("callers".into(), "2".into()),
                    ("generations".into(), "2".into()),
                ],
            };
            let original = record(
                &model,
                failure.inputs.clone(),
                run.clone(),
                failure.inputs.len(),
            )?;
            if original.termination != Termination::PropertyFailed {
                return Err("original trace did not reproduce".into());
            }
            save(&Path::new(directory).join("original.sttrace"), &original)?;
            let minimized = shrink(&model, &failure, ShrinkConfig::default())?;
            if !minimized.validated_original {
                return Err("shrinker did not validate original".into());
            }
            let trace = record(
                &model,
                minimized.minimized.inputs.clone(),
                run,
                minimized.minimized.inputs.len(),
            )?;
            if trace.termination != Termination::PropertyFailed {
                return Err("minimized trace did not reproduce".into());
            }
            save(&Path::new(directory).join("minimized.sttrace"), &trace)?;
            println!(
                "injected fault found: {} -> {} inputs, shrink {:?}",
                failure.inputs.len(),
                minimized.minimized.inputs.len(),
                minimized.termination
            );
            Ok(0)
        }
        ["replay", path] => {
            let trace = Trace::read_from(File::open(path)?, &ReadLimits::default())?;
            let report = replay(
                &Adapter { fault: true },
                &trace,
                ReplayOptions {
                    allow_build_mismatch: false,
                },
            )?;
            println!(
                "{:?}: failure={}, matching_build={}",
                report.outcome, report.failure_reproduced, report.build_matches
            );
            if report.outcome != ReplayOutcome::Exact
                || !report.failure_reproduced
                || !report.build_matches
            {
                return Err("fresh process replay did not reproduce exact failure".into());
            }
            Ok(0)
        }
        _ => Err("usage: catabolic-verification check | find NEW_DIRECTORY | replay TRACE".into()),
    }
}
fn main() -> ExitCode {
    match run(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(2)
        }
    }
}
