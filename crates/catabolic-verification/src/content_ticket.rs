// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded competing-debit model. SQL compare-and-update remains authoritative.
use catabolic_core::{
    content_ticket::{self, DebitEffect, DebitEvidence},
    encode, sha256,
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
    remaining: u8,
    spent: u8,
    revoked: bool,
    expired: bool,
    violation: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Event {
    Debit(u8),
    Revoke,
    Expire,
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
            name: "catabolic-content-ticket".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/content_ticket.rs").as_slice(),
                        include_bytes!("content_ticket.rs").as_slice(),
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
            remaining: 2,
            spent: 0,
            revoked: false,
            expired: false,
            violation: false,
        })
    }
    fn step(
        &self,
        before: &Audited,
        event: &Event,
    ) -> Result<Transition<Audited, String>, ModelError> {
        let mut after = before.clone();
        let output = match *event {
            Event::Revoke => {
                after.revoked = true;
                "revoked".to_string()
            }
            Event::Expire => {
                after.expired = true;
                "expired".to_string()
            }
            Event::Debit(bytes) => {
                let mut result = content_ticket::debit(DebitEvidence {
                    remaining: i64::from(before.remaining),
                    revoked: before.revoked,
                    expires: if before.expired { 1.0 } else { 2.0 },
                    now: 1.0,
                    requested: u64::from(bytes),
                });
                if self.fault && before.revoked && bytes == 1 && before.remaining >= 1 {
                    result = DebitEffect::Debit {
                        bytes: 1,
                        remaining: i64::from(before.remaining - 1),
                    };
                }
                let expected = !before.revoked && !before.expired && bytes <= before.remaining;
                if let DebitEffect::Debit {
                    bytes: charged,
                    remaining,
                } = result
                {
                    if !expected
                        || charged != i64::from(bytes)
                        || remaining != i64::from(before.remaining) - i64::from(bytes)
                    {
                        after.violation = true;
                    } else {
                        after.remaining = remaining as u8;
                        after.spent += bytes;
                    }
                } else if expected {
                    after.violation = true;
                }
                format!("{result:?}")
            }
        };
        if after.spent + after.remaining != 2 {
            after.violation = true;
        }
        Ok(Transition::accepted(after, vec![output]))
    }
    fn check_state(&self, s: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.violation {
            Check::failed(
                "ticket.allowance",
                "ticket admitted invalid authority or overspent allowance",
            )
        } else {
            Check::passed("ticket.allowance")
        }])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        Ok(vec![
            Event::Debit(0),
            Event::Debit(1),
            Event::Debit(2),
            Event::Debit(3),
            Event::Revoke,
            Event::Expire,
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
        return Err(format!("ticket check failed/incomplete: {:?}", r.termination).into());
    }
    println!(
        "content ticket graph exhausted: {} states, {} transitions, zero skipped checks",
        r.states, r.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let f = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected ticket fault not found")?;
    let reduced = shrink(&model, &f, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("ticket shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-content-ticket".into(),
        seed: None,
        parameters: vec![
            ("initial-allowance".into(), "2".into()),
            ("debit-lengths".into(), "0..3".into()),
        ],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &f.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("ticket trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "ticket injected fault found and shrunk: {} -> {} inputs",
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
        return Err("ticket fresh replay did not match".into());
    }
    println!("ticket fresh-process replay exact, failed property reproduced, matching build");
    Ok(())
}
