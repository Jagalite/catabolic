//! Bounded Statelessness adapter around the application-owned production reducer.
mod content_ticket;
mod coordination;
mod execution;
mod hardlink_cancellation;
mod hardlink_recovery;
mod notification_delivery;
mod notification_lease;
mod playback;
mod playback_admission;
mod playback_retirement;
mod processing_acceptance;
mod publication_progress;
mod publication_recovery;
mod rendition;
mod resolution;
mod scan_continuation;
mod scan_publication;
mod scan_publication_race;
mod shared_job;
mod transfer_completion;
mod watcher_generation;
mod watcher_reaction;
mod watcher_schedule;
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
            properties_version: 2,
            codec_version: 1,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/lifecycle.rs").as_slice(),
                        include_bytes!("main.rs").as_slice(),
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
        if let Some(required) = required_effect_violation(&before.state, input, &effects) {
            violation = Some(required.into());
        }
        Ok(Transition::accepted(Audited { state, violation }, effects))
    }
    fn check_state(&self, state: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok([
            "request.stale_completion",
            "request.cancelled_delivery",
            "request.cancel_stops_sibling",
            "request.required_start",
            "request.required_delivery",
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
fn required_effect_violation(
    before: &State,
    input: &Input,
    effects: &[Effect],
) -> Option<&'static str> {
    let required_start = match input {
        Input::Attach(caller) => *caller < 2 && !before.running && !before.started,
        Input::Advance(generation) => {
            *generation > before.generation && before.callers.contains(&Caller::Waiting)
        }
        _ => false,
    };
    let start_generation = match input {
        Input::Advance(generation) => *generation,
        _ => before.generation,
    };
    if required_start && !effects.contains(&Effect::Start(start_generation)) {
        return Some("request.required_start");
    }
    if let Input::Complete(generation) = input
        && *generation == before.generation
        && before.running
    {
        for (caller, status) in before.callers.iter().enumerate() {
            if *status == Caller::Waiting
                && !effects.contains(&Effect::Deliver {
                    caller: caller as u8,
                    generation: *generation,
                })
            {
                return Some("request.required_delivery");
            }
        }
    }
    None
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
        ["check-publication-progress"] => { publication_progress::check()?; Ok(0) }
        ["find-publication-progress", directory] => { publication_progress::find(directory)?; Ok(0) }
        ["replay-publication-progress", path] => { publication_progress::replay_trace(path)?; Ok(0) }
        ["check-playback-retirement"] => { playback_retirement::check()?; Ok(0) }
        ["find-playback-retirement", directory] => { playback_retirement::find(directory)?; Ok(0) }
        ["replay-playback-retirement", path] => { playback_retirement::replay_trace(path)?; Ok(0) }
        ["check-playback-admission"] => { playback_admission::check()?; Ok(0) }
        ["find-playback-admission", directory] => { playback_admission::find(directory)?; Ok(0) }
        ["replay-playback-admission", path] => { playback_admission::replay_trace(path)?; Ok(0) }
        ["check-watcher-reaction"] => { watcher_reaction::check()?; Ok(0) }
        ["find-watcher-reaction", directory] => { watcher_reaction::find(directory)?; Ok(0) }
        ["replay-watcher-reaction", path] => { watcher_reaction::replay_trace(path)?; Ok(0) }
        ["check-watcher-generation"] => { watcher_generation::check()?; Ok(0) }
        ["find-watcher-generation", directory] => { watcher_generation::find(directory)?; Ok(0) }
        ["replay-watcher-generation", path] => { watcher_generation::replay_trace(path)?; Ok(0) }
        ["check-watcher-schedule"] => { watcher_schedule::check()?; Ok(0) }
        ["find-watcher-schedule", directory] => { watcher_schedule::find(directory)?; Ok(0) }
        ["replay-watcher-schedule", path] => { watcher_schedule::replay_trace(path)?; Ok(0) }
        ["check-transfer-completion"] => { transfer_completion::check()?; Ok(0) }
        ["find-transfer-completion", directory] => { transfer_completion::find(directory)?; Ok(0) }
        ["replay-transfer-completion", path] => { transfer_completion::replay_trace(path)?; Ok(0) }
        ["check-content-ticket"] => { content_ticket::check()?; Ok(0) }
        ["find-content-ticket", directory] => { content_ticket::find(directory)?; Ok(0) }
        ["replay-content-ticket", path] => { content_ticket::replay_trace(path)?; Ok(0) }
        ["check-processing-acceptance"] => { processing_acceptance::check()?; Ok(0) }
        ["find-processing-acceptance", directory] => { processing_acceptance::find(directory)?; Ok(0) }
        ["replay-processing-acceptance", path] => { processing_acceptance::replay_trace(path)?; Ok(0) }
        ["check-notification-lease"] => { notification_lease::check()?; Ok(0) }
        ["find-notification-lease", directory] => { notification_lease::find(directory)?; Ok(0) }
        ["replay-notification-lease", path] => { notification_lease::replay_trace(path)?; Ok(0) }
        ["check-notification"] => { notification_delivery::check()?; Ok(0) }
        ["find-notification", directory] => { notification_delivery::find(directory)?; Ok(0) }
        ["replay-notification", path] => { notification_delivery::replay_trace(path)?; Ok(0) }
        ["check-hardlink-cancellation"] => { hardlink_cancellation::check()?; Ok(0) }
        ["find-hardlink-cancellation", directory] => { hardlink_cancellation::find(directory)?; Ok(0) }
        ["replay-hardlink-cancellation", path] => { hardlink_cancellation::replay_trace(path)?; Ok(0) }
        ["check-hardlink-recovery"] => { hardlink_recovery::check()?; Ok(0) }
        ["find-hardlink-recovery", directory] => { hardlink_recovery::find(directory)?; Ok(0) }
        ["replay-hardlink-recovery", path] => { hardlink_recovery::replay_trace(path)?; Ok(0) }
        ["check-publication-recovery"] => { publication_recovery::check()?; Ok(0) }
        ["find-publication-recovery", directory] => { publication_recovery::find(directory)?; Ok(0) }
        ["replay-publication-recovery", path] => { publication_recovery::replay_trace(path)?; Ok(0) }
        ["check-scan-continuation"] => { scan_continuation::check()?; Ok(0) }
        ["find-scan-continuation", directory] => { scan_continuation::find(directory)?; Ok(0) }
        ["replay-scan-continuation", path] => { scan_continuation::replay_trace(path)?; Ok(0) }
        ["check-scan-publication-race"] => { scan_publication_race::check()?; Ok(0) }
        ["find-scan-publication-race", directory] => { scan_publication_race::find(directory)?; Ok(0) }
        ["replay-scan-publication-race", path] => { scan_publication_race::replay_trace(path)?; Ok(0) }
        ["check-scan-publication"] => { scan_publication::check()?; Ok(0) }
        ["find-scan-publication", directory] => { scan_publication::find(directory)?; Ok(0) }
        ["replay-scan-publication", path] => { scan_publication::replay_trace(path)?; Ok(0) }
        ["check-execution"] => { execution::check()?; Ok(0) }
        ["find-execution", directory] => { execution::find(directory)?; Ok(0) }
        ["replay-execution", path] => { execution::replay_trace(path)?; Ok(0) }
        ["check-resolution"] => { resolution::check()?; Ok(0) }
        ["find-resolution", directory] => { resolution::find(directory)?; Ok(0) }
        ["replay-resolution", path] => { resolution::replay_trace(path)?; Ok(0) }
        ["check-shared-job"] => { shared_job::check()?; Ok(0) }
        ["find-shared-job", directory] => { shared_job::find(directory)?; Ok(0) }
        ["replay-shared-job", path] => { shared_job::replay_trace(path)?; Ok(0) }
        ["check-rendition"] => {
            rendition::check()?;
            Ok(0)
        }
        ["find-rendition", directory] => {
            rendition::find(directory)?;
            Ok(0)
        }
        ["replay-rendition", path] => {
            rendition::replay_trace(path)?;
            Ok(0)
        }
        ["find-coordination", directory] => {
            coordination::find(directory)?;
            Ok(0)
        }
        ["replay-coordination", path] => {
            coordination::replay_trace(path)?;
            Ok(0)
        }
        ["check-coordination"] => {
            coordination::check()?;
            Ok(0)
        }
        ["check-playback"] => {
            playback::check()?;
            Ok(0)
        }
        ["find-playback", directory] => {
            playback::find(directory)?;
            Ok(0)
        }
        ["replay-playback", path] => {
            playback::replay_trace(path)?;
            Ok(0)
        }
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
        _ => Err("usage: catabolic-verification check | check-playback | check-coordination | find NEW_DIRECTORY | replay TRACE".into()),
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

#[cfg(test)]
mod obligation_tests {
    use super::*;

    #[test]
    fn missing_start_and_delivery_are_detected() {
        let initial = State::default();
        assert_eq!(
            required_effect_violation(&initial, &Input::Attach(0), &[]),
            Some("request.required_start")
        );
        assert_eq!(
            required_effect_violation(&initial, &Input::Attach(0), &[Effect::Start(0)]),
            None
        );
        let (running, _) = reduce(&initial, &Input::Attach(0));
        assert_eq!(
            required_effect_violation(&running, &Input::Complete(0), &[]),
            Some("request.required_delivery")
        );
        assert_eq!(
            required_effect_violation(
                &running,
                &Input::Complete(0),
                &[Effect::Deliver {
                    caller: 0,
                    generation: 0
                }]
            ),
            None
        );
        assert_eq!(
            required_effect_violation(&running, &Input::Complete(1), &[]),
            None
        );
        assert_eq!(
            required_effect_violation(&running, &Input::Advance(1), &[]),
            Some("request.required_start")
        );
    }
}
