// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded delivery lease, replacement and acknowledgement sequences.
use catabolic_core::{
    encode,
    notification_delivery::{self, LeaseEvidence, Outcome},
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
    now: u8,
    token: u8,
    revision: u8,
    lease_revision: u8,
    until: u8,
    leased: bool,
    cancelled: bool,
    complete: bool,
    violation: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Event {
    Claim,
    Tick,
    Edit,
    AcknowledgeOld,
    AcknowledgeCurrent,
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
            name: "catabolic-notification-lease".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/notification_delivery.rs")
                            .as_slice(),
                        include_bytes!("notification_lease.rs").as_slice(),
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
            Event::Claim
                if !before.complete
                    && !before.cancelled
                    && before.token < 2
                    && (!before.leased || before.until <= before.now) =>
            {
                after.token += 1;
                after.lease_revision = before.revision;
                after.until = before.now + 1;
                after.leased = true;
            }
            Event::Tick if before.now < 2 => after.now += 1,
            Event::Edit if !before.complete && !before.cancelled => {
                after.revision += 1;
                after.cancelled = true;
                after.leased = false;
            }
            Event::AcknowledgeOld | Event::AcknowledgeCurrent => {
                let token = if matches!(event, Event::AcknowledgeOld) {
                    1
                } else {
                    before.token
                };
                let evidence = LeaseEvidence {
                    leased: before.leased,
                    token_matches: token != 0 && token == before.token,
                    unexpired: before.until > before.now,
                    destination_enabled: !before.cancelled,
                    revision_matches: before.lease_revision == before.revision,
                };
                let expected = before.leased
                    && token != 0
                    && token == before.token
                    && before.until > before.now
                    && !before.cancelled
                    && before.lease_revision == before.revision;
                let mut actual = notification_delivery::lease_current(evidence);
                if self.fault
                    && before.leased
                    && before.token == 2
                    && token == 1
                    && before.until > before.now
                {
                    actual = true;
                }
                if actual != expected {
                    after.violation = true;
                }
                if actual {
                    if notification_delivery::acknowledge(
                        Outcome::Complete,
                        0,
                        f64::from(before.now),
                    )
                    .is_err()
                    {
                        after.violation = true;
                    }
                    after.complete = true;
                    after.leased = false;
                }
            }
            _ => {}
        }
        Ok(Transition::accepted(after, vec![format!("{event:?}")]))
    }
    fn check_state(&self, s: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.violation {
            Check::failed(
                "notification.lease",
                "stale or superseded lease was accepted",
            )
        } else {
            Check::passed("notification.lease")
        }])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        Ok(vec![
            Event::Claim,
            Event::Tick,
            Event::Edit,
            Event::AcknowledgeOld,
            Event::AcknowledgeCurrent,
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
            "notification lease check failed/incomplete: {:?}",
            r.termination
        )
        .into());
    }
    println!(
        "notification lease graph exhausted: {} states, {} transitions, zero skipped checks",
        r.states, r.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let f = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected notification lease fault not found")?;
    let reduced = shrink(&model, &f, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("notification lease shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-notification-lease".into(),
        seed: None,
        parameters: vec![("maximum-claims".into(), "2".into())],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &f.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("notification lease trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "notification lease injected fault found and shrunk: {} -> {} inputs",
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
        return Err("notification lease fresh replay did not match".into());
    }
    println!(
        "notification lease fresh-process replay exact, failed property reproduced, matching build"
    );
    Ok(())
}
