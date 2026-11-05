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
    let history = enumerate(&HistoryAdapter { fault: false }, SearchConfig::default())?;
    if history.failure.is_some()
        || history.termination != SearchTermination::GraphExhausted
        || history.skipped_checks != 0
    {
        return Err(format!(
            "resolution history failed/incomplete: {:?}",
            history.termination
        )
        .into());
    }
    println!(
        "resolution history graph exhausted: {} states, {} transitions, zero skipped checks",
        history.states, history.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(HistoryAdapter { fault: true });
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
            ("candidates".into(), "2".into()),
            ("revisions".into(), "2 per candidate".into()),
            ("admissions".into(), "3".into()),
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
        &HistoryAdapter { fault: true },
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

// Two candidate identities, two revisions each, three accepted plans, time 0..2.
// Probe, SQL key lookup and health-set replacement are adapter abstractions.
// Core owns accumulation, failback, admission and verification decisions. A
// restart preserves durable history while dropping uncommitted observations.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct HealthSlot {
    revision: u8,
    count: u8,
    since: u8,
    last: u8,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Probe {
    candidate: u8,
    revision: u8,
    generation: u8,
    stale: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct History {
    revisions: [u8; 2],
    health: [Option<HealthSlot>; 2],
    now: u8,
    generation: u8,
    verified: u8,
    probe: Option<Probe>,
    verification: Option<u8>,
    violation: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum HistoryEvent {
    Begin(u8),
    Result(bool),
    Revise(u8),
    Tick,
    CompetingAdmission,
    Choose { candidate: u8, previous_live: bool },
    BeginVerification,
    Verify { healthy: bool, binding: bool },
    Restart,
}
struct HistoryAdapter {
    fault: bool,
}
impl HistoryAdapter {
    fn apply(&self, before: &History, event: &HistoryEvent) -> Result<History, ModelError> {
        use catabolic_core::publication_progress::{self, Effect, Evidence};
        let mut after = before.clone();
        match *event {
            HistoryEvent::Tick if before.now < 2 => after.now += 1,
            HistoryEvent::Begin(candidate) if before.generation < 3 => {
                after.probe = Some(Probe {
                    candidate,
                    revision: before.revisions[usize::from(candidate)],
                    generation: before.generation,
                    stale: false,
                });
            }
            HistoryEvent::Revise(candidate) if before.revisions[usize::from(candidate)] == 0 => {
                after.revisions[usize::from(candidate)] = 1;
                if let Some(p) = &mut after.probe {
                    p.stale = true;
                }
            }
            HistoryEvent::CompetingAdmission if before.generation < 3 => after.generation += 1,
            HistoryEvent::Result(success) if before.generation < 3 => {
                if let Some(p) = &before.probe {
                    let index = usize::from(p.candidate);
                    let eligible = !p.stale
                        && p.generation == before.generation
                        && p.revision == before.revisions[index];
                    let effect = resolution::projection(ProjectionAdmission {
                        expected_epoch: 0,
                        current_epoch: i64::from(p.stale),
                        expected_generation: i64::from(p.generation),
                        current_generation: i64::from(before.generation),
                        safe: true,
                        changes: 0,
                        removals: 0,
                        max_changes: 1,
                        max_removals: 1,
                    });
                    after.violation |= effect.is_ok() != eligible;
                    if let Ok(effect) = effect {
                        after.violation |=
                            effect.generation != i64::from(before.generation + 1) || effect.retain;
                        after.generation = effect.generation as u8;
                        after.health[index] = if success {
                            let old = before.health[index]
                                .as_ref()
                                .filter(|h| h.revision == p.revision || self.fault);
                            let record = resolution::health_success(
                                old.map(|h| resolution::Health {
                                    healthy_since: f64::from(h.since),
                                    successes: i64::from(h.count),
                                    last_check: f64::from(h.last),
                                }),
                                f64::from(before.now),
                            )
                            .map_err(ModelError::new)?;
                            Some(HealthSlot {
                                revision: p.revision,
                                count: record.successes as u8,
                                since: record.healthy_since as u8,
                                last: record.last_check as u8,
                            })
                        } else {
                            None
                        };
                    }
                    // Independent accepted-history oracle: candidate identity,
                    // exact revision, failure reset, and no early verification.
                    let expected = if !eligible {
                        before.health[index].clone()
                    } else if !success {
                        None
                    } else {
                        let old = before.health[index]
                            .as_ref()
                            .filter(|h| h.revision == p.revision);
                        Some(HealthSlot {
                            revision: p.revision,
                            count: old.map_or(1, |h| h.count + 1),
                            since: old.map_or(before.now, |h| h.since),
                            last: before.now,
                        })
                    };
                    after.violation |= after.health[index] != expected
                        || after.health[1 - index] != before.health[1 - index]
                        || after.generation != before.generation + u8::from(eligible)
                        || after.verified != before.verified;
                    after.probe = None;
                }
            }
            HistoryEvent::Choose {
                candidate,
                previous_live,
            } => {
                let i = usize::from(candidate);
                let h = before.health[i]
                    .as_ref()
                    .filter(|h| h.revision == before.revisions[i]);
                let count = h.map_or(0, |h| h.count);
                let since = h.map_or(before.now, |h| h.since);
                let effect = resolution::failback(Failback {
                    mode: FailbackMode::Stable,
                    manual_override: false,
                    improving_tier: true,
                    previous_eligible_and_live: previous_live,
                    successful_checks: u64::from(count),
                    minimum_successful_checks: 2,
                    healthy_since: f64::from(since),
                    now: f64::from(before.now),
                    minimum_healthy_seconds: 1,
                });
                let expected = !previous_live || (count >= 2 && before.now > since);
                after.violation |= (effect == FailbackEffect::Preferred) != expected;
            }
            HistoryEvent::BeginVerification => after.verification = Some(before.generation),
            HistoryEvent::Verify { healthy, binding } => {
                let effect = publication_progress::decide(Evidence {
                    observed_generation: before.verification.map(i64::from),
                    current_generation: i64::from(before.generation),
                    verified_generation: i64::from(before.verified),
                    binding_matches: binding,
                    healthy,
                });
                let expected = healthy
                    && binding
                    && before.verification == Some(before.generation)
                    && before.generation > before.verified;
                match effect {
                    Effect::Verify { generation } => {
                        after.violation |= !expected || generation != i64::from(before.generation);
                        after.verified = generation as u8;
                    }
                    Effect::RetainPending => after.violation |= expected,
                }
            }
            HistoryEvent::Restart => {
                after = serde_json::from_slice(
                    &serde_json::to_vec(before).map_err(|e| ModelError::new(e.to_string()))?,
                )
                .map_err(|e| ModelError::new(e.to_string()))?;
                after.probe = None;
                after.verification = None;
                after.violation |= after.health != before.health
                    || after.generation != before.generation
                    || after.verified != before.verified;
            }
            _ => {}
        }
        after.violation |= after.verified > after.generation;
        Ok(after)
    }
}
impl Model for HistoryAdapter {
    type State = History;
    type Input = HistoryEvent;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "catabolic-resolution".into(),
            model_version: 2,
            properties_version: 2,
            codec_version: 2,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/resolution.rs").as_slice(),
                        include_bytes!("../../catabolic-core/src/publication_progress.rs")
                            .as_slice(),
                        include_bytes!("resolution.rs").as_slice(),
                        include_bytes!("../../../Cargo.lock").as_slice()
                    ]
                    .concat()
                ),
                self.fault
            ),
        }
    }
    fn initial_state(&self) -> Result<History, ModelError> {
        Ok(History::default())
    }
    fn step(
        &self,
        before: &History,
        input: &HistoryEvent,
    ) -> Result<Transition<History, String>, ModelError> {
        Ok(Transition::accepted(
            self.apply(before, input)?,
            vec![format!("{input:?}")],
        ))
    }
    fn check_state(&self, s: &History) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.violation {
            Check::failed(
                "resolution.history",
                "candidate/revision health, stable failback or verified-generation obligation violated",
            )
        } else {
            Check::passed("resolution.history")
        }])
    }
}
impl Enumerate for HistoryAdapter {
    fn inputs(&self, _: &History) -> Result<Vec<HistoryEvent>, ModelError> {
        let mut inputs = vec![
            HistoryEvent::Tick,
            HistoryEvent::Result(false),
            HistoryEvent::Result(true),
            HistoryEvent::CompetingAdmission,
            HistoryEvent::BeginVerification,
            HistoryEvent::Restart,
        ];
        for candidate in 0..2 {
            inputs.extend([
                HistoryEvent::Begin(candidate),
                HistoryEvent::Revise(candidate),
                HistoryEvent::Choose {
                    candidate,
                    previous_live: true,
                },
                HistoryEvent::Choose {
                    candidate,
                    previous_live: false,
                },
            ]);
        }
        for healthy in [false, true] {
            for binding in [false, true] {
                inputs.push(HistoryEvent::Verify { healthy, binding });
            }
        }
        Ok(inputs)
    }
}
impl ModelCodec for HistoryAdapter {
    fn encode_state(&self, v: &History) -> Result<Vec<u8>, ModelError> {
        codec(v)
    }
    fn decode_state(&self, v: &[u8]) -> Result<History, ModelError> {
        let s: History = serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))?;
        if s.now > 2
            || s.generation > 3
            || s.verified > s.generation
            || s.revisions.iter().any(|r| *r > 1)
            || s.verification.is_some_and(|g| g > 3)
            || s.probe
                .as_ref()
                .is_some_and(|p| p.candidate > 1 || p.revision > 1 || p.generation > 3)
            || s.health.iter().flatten().any(|h| {
                h.revision > 1 || h.count == 0 || h.count > 3 || h.since > h.last || h.last > s.now
            })
        {
            return Err(ModelError::new("resolution history state outside bounds"));
        }
        Ok(s)
    }
    fn encode_input(&self, v: &HistoryEvent) -> Result<Vec<u8>, ModelError> {
        codec(v)
    }
    fn decode_input(&self, v: &[u8]) -> Result<HistoryEvent, ModelError> {
        let e: HistoryEvent =
            serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))?;
        if matches!(e,HistoryEvent::Begin(c)|HistoryEvent::Revise(c)|HistoryEvent::Choose{candidate:c,..} if c>1)
        {
            return Err(ModelError::new("resolution candidate outside bounds"));
        }
        Ok(e)
    }
    fn encode_output(&self, v: &String) -> Result<Vec<u8>, ModelError> {
        codec(v)
    }
}
#[cfg(test)]
mod history_tests {
    use super::*;
    fn run(fault: bool, events: &[HistoryEvent]) -> History {
        let model = HistoryAdapter { fault };
        events
            .iter()
            .fold(History::default(), |s, e| model.apply(&s, e).unwrap())
    }
    #[test]
    fn revision_change_cannot_inherit_health_even_after_restart() {
        use HistoryEvent::*;
        let events = [
            Begin(0),
            Result(true),
            Tick,
            Revise(0),
            Restart,
            Begin(0),
            Result(true),
        ];
        let good = run(false, &events);
        assert_eq!(good.health[0].as_ref().unwrap().count, 1);
        assert_eq!(good.health[0].as_ref().unwrap().since, 1);
        assert!(!good.violation);
        assert!(run(true, &events).violation);
    }
    #[test]
    fn failed_and_stale_evidence_do_not_advance_health_or_publication() {
        use HistoryEvent::*;
        let stale = run(false, &[Begin(0), Revise(0), Result(true)]);
        assert_eq!(stale.generation, 0);
        assert!(stale.health[0].is_none());
        let failed = run(
            false,
            &[
                Begin(0),
                Result(true),
                Begin(1),
                Result(true),
                Begin(0),
                Result(false),
            ],
        );
        assert!(failed.health[0].is_none());
        assert_eq!(failed.health[1].as_ref().unwrap().count, 1);
        assert_eq!(failed.verified, 0);
        assert!(!failed.violation);
        let stale_verify = run(
            false,
            &[
                Begin(0),
                Result(true),
                BeginVerification,
                CompetingAdmission,
                Verify {
                    healthy: true,
                    binding: true,
                },
                Restart,
            ],
        );
        assert_eq!(stale_verify.generation, 2);
        assert_eq!(stale_verify.verified, 0);
        assert!(!stale_verify.violation);
    }
    #[test]
    fn count_and_age_thresholds_are_both_required() {
        use HistoryEvent::*;
        let s = run(
            false,
            &[
                Begin(0),
                Result(true),
                Choose {
                    candidate: 0,
                    previous_live: true,
                },
                Begin(0),
                Result(true),
                Choose {
                    candidate: 0,
                    previous_live: true,
                },
                Tick,
                Choose {
                    candidate: 0,
                    previous_live: true,
                },
                BeginVerification,
                Verify {
                    healthy: true,
                    binding: true,
                },
            ],
        );
        assert_eq!(s.health[0].as_ref().unwrap().count, 2);
        assert_eq!(s.verified, 2);
        assert!(!s.violation);
    }
}
