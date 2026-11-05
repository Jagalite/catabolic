// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded watcher admission and debounce audit.
//! Time 0..=4 crosses the fixed maximum delay of 3. The last event is either
//! fixed at 1 or refreshed to now, independently exercising both deadlines.
//! This decision table does not model durable claims or elapsed-time fairness.
use catabolic_core::{
    encode, sha256,
    watcher_schedule::{self, DueEvidence},
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
    enabled: bool,
    requested: bool,
    pending: bool,
    active: bool,
    lease_live: bool,
    retry: u8,
    reaction: bool,
    manual: bool,
    scheduled: bool,
    events: bool,
    first: bool,
    now: u8,
    refreshed: bool,
}
struct Adapter {
    fault: bool,
}
fn facts(e: &Event) -> DueEvidence {
    let now = f64::from(e.now);
    let retry = match e.retry {
        0 => 0.0,
        1 => now,
        _ => now + 1.0,
    };
    DueEvidence {
        enabled: e.enabled,
        requested_generation: i64::from(e.requested),
        completed_generation: 0,
        pending_generation: i64::from(e.pending),
        active_run: e.active,
        lease_until: if e.lease_live { now + 1.0 } else { now },
        retry_at: retry,
        pending_reaction: e.reaction,
        manual_schedule: e.manual,
        next_due: if e.scheduled { now } else { now + 1.0 },
        events_enabled: e.events,
        event_first: e.first.then_some(0.0),
        event_last: if e.refreshed { now } else { 1.0 },
        debounce_seconds: Some(1.0),
        maximum_delay_seconds: Some(3.0),
        now,
    }
}
fn evaluate(e: &Event, fault: bool) -> (bool, bool) {
    let mut actual = watcher_schedule::due(facts(e));
    if fault && e.retry == 2 && e.requested {
        actual = true;
    }
    let fenced = (!e.enabled && !e.requested) || (e.active && e.lease_live) || e.retry == 2;
    let expected = !fenced
        && (e.requested
            || e.reaction
            || (e.retry == 1 && e.now > 0)
            || (!e.manual && e.scheduled)
            || (e.events && e.pending && e.first && ((!e.refreshed && e.now >= 2) || e.now >= 3)));
    (actual, expected)
}
impl Model for Adapter {
    type State = Audited;
    type Input = Event;
    type Output = bool;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "catabolic-watcher-schedule".into(),
            model_version: 2,
            properties_version: 2,
            codec_version: 2,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/watcher_schedule.rs").as_slice(),
                        include_bytes!("watcher_schedule.rs").as_slice(),
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
    fn step(&self, before: &Audited, e: &Event) -> Result<Transition<Audited, bool>, ModelError> {
        let (actual, expected) = evaluate(e, self.fault);
        Ok(Transition::accepted(
            Audited {
                violation: before.violation || actual != expected,
            },
            vec![actual],
        ))
    }
    fn check_state(&self, s: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.violation {
            Check::failed(
                "watcher.due",
                "watcher admitted or suppressed work contrary to deadline and generation evidence",
            )
        } else {
            Check::passed("watcher.due")
        }])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        let mut v = Vec::new();
        for mask in 0..1024 {
            for retry in 0..3 {
                for now in 0..=4 {
                    for refreshed in [false, true] {
                        v.push(Event {
                            enabled: mask & 1 != 0,
                            requested: mask & 2 != 0,
                            pending: mask & 4 != 0,
                            active: mask & 8 != 0,
                            lease_live: mask & 16 != 0,
                            reaction: mask & 32 != 0,
                            manual: mask & 64 != 0,
                            scheduled: mask & 128 != 0,
                            events: mask & 256 != 0,
                            first: mask & 512 != 0,
                            retry,
                            now,
                            refreshed,
                        });
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
        let event: Event = serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))?;
        if event.now > 4 || event.retry > 2 {
            return Err(ModelError::new("watcher evidence outside declared bounds"));
        }
        Ok(event)
    }
    fn encode_output(&self, v: &bool) -> Result<Vec<u8>, ModelError> {
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
            "watcher schedule check failed/incomplete: {:?}",
            r.termination
        )
        .into());
    }
    println!(
        "watcher schedule graph exhausted: {} states, {} transitions, zero skipped checks",
        r.states, r.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let f = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected watcher fault not found")?;
    let reduced = shrink(&model, &f, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("watcher shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-watcher-schedule".into(),
        seed: None,
        parameters: vec![
            ("boolean-facts".into(), "10".into()),
            ("retry-cases".into(), "3".into()),
            ("times".into(), "0..4".into()),
            ("last-event".into(), "fixed-or-refreshed".into()),
        ],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &f.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("watcher trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "watcher injected fault found and shrunk: {} -> {} inputs",
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
        return Err("watcher fresh replay did not match".into());
    }
    println!("watcher fresh-process replay exact, failed property reproduced, matching build");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(now: u8, refreshed: bool) -> Event {
        Event {
            enabled: true,
            requested: false,
            pending: true,
            active: false,
            lease_live: false,
            retry: 0,
            reaction: false,
            manual: true,
            scheduled: false,
            events: true,
            first: true,
            now,
            refreshed,
        }
    }

    #[test]
    fn trace_codec_rejects_time_or_retry_outside_the_enumerated_domain() {
        let model = Adapter { fault: false };
        let mut e = event(5, true);
        assert!(model.decode_input(&codec(&e).unwrap()).is_err());
        e.now = 3;
        e.retry = 3;
        assert!(model.decode_input(&codec(&e).unwrap()).is_err());
    }

    #[test]
    fn refreshed_events_cannot_defer_beyond_maximum_delay() {
        for now in 0..=4 {
            let e = event(now, true);
            assert_eq!(evaluate(&e, false), (now >= 3, now >= 3));
        }
        let e = event(3, true);
        let mut omission = facts(&e);
        omission.maximum_delay_seconds = Some(100.0);
        assert!(!watcher_schedule::due(omission));
        assert!(evaluate(&e, false).1);
    }

    #[test]
    fn quiet_debounce_and_retry_or_lease_fences_remain_independent() {
        for now in 0..=4 {
            assert_eq!(evaluate(&event(now, false), false), (now >= 2, now >= 2));
        }
        let mut e = event(3, true);
        e.retry = 2;
        assert_eq!(evaluate(&e, false), (false, false));
        e.requested = true;
        assert_eq!(evaluate(&e, true), (true, false));
        e.retry = 0;
        e.active = true;
        e.lease_live = true;
        assert_eq!(evaluate(&e, false), (false, false));
    }
}
