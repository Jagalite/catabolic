// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded competing-debit model. SQL compare-and-update remains authoritative.
use catabolic_core::{
    content_ticket::{self, DebitEffect, DebitEvidence},
    encode, sha256, transfer_completion,
};
use serde::{Deserialize, Serialize};
use stateless::automatic::Auto;
use stateless::execution::{ReplayOptions, ReplayOutcome, record, replay};
use stateless::explore::{SearchConfig, SearchTermination, ShrinkConfig, enumerate, shrink};
use stateless::trace::{ReadLimits, RunConfig, Termination, Trace};
use stateless::{Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition};
use std::{error::Error, fs::File, path::Path};
// One ticket with allowance two, two clients, full two-chunk or partial
// one-chunk responses, one authority/revision change. Authentication, identity
// comparisons and transactional compare/update are adapter abstractions; the
// production ticket tests exercise committed SQLite owners independently.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Client {
    phase: u8, // idle, prepared, active, completed, interrupted/refused
    length: u8,
    observed_remaining: u8,
    delivered: u8,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Audited {
    remaining: u8,
    spent: u8,
    clients: [Client; 2],
    revoked: bool,
    expired: bool,
    revision_changed: bool,
    authority: u8, // current, grant revoked, token revoked, owner changed
    violation: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Caller {
    A,
    B,
}
impl Caller {
    fn index(self) -> usize {
        match self {
            Self::A => 0,
            Self::B => 1,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Digest {
    Match,
    Mismatch,
    Missing,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Event {
    Prepare(Caller, bool),
    Commit(Caller),
    Interrupt(Caller),
    Read(Caller, Digest),
    Revoke,
    Expire,
    ChangeRevision,
    RevokeGrant,
    RevokeToken,
    ChangeOwner,
}
struct Adapter {
    fault: bool,
}
fn authorized(s: &Audited) -> bool {
    !s.revoked && !s.expired && !s.revision_changed && s.authority == 0
}
fn step_state(before: &Audited, event: &Event, fault: bool) -> Audited {
    let mut after = before.clone();
    match *event {
        Event::Revoke => after.revoked = true,
        Event::Expire => after.expired = true,
        Event::ChangeRevision => after.revision_changed = true,
        Event::RevokeGrant if before.authority == 0 => after.authority = 1,
        Event::RevokeToken if before.authority == 0 => after.authority = 2,
        Event::ChangeOwner if before.authority == 0 => after.authority = 3,
        Event::Prepare(caller, full) => {
            let i = caller.index();
            if before.clients[i].phase == 0 && authorized(before) {
                after.clients[i] = Client {
                    phase: 1,
                    length: if full { 2 } else { 1 },
                    observed_remaining: before.remaining,
                    delivered: 0,
                };
            }
        }
        Event::Interrupt(caller) => {
            let i = caller.index();
            if matches!(before.clients[i].phase, 1 | 2) {
                // Pre-commit rollback does not spend; after commit no refund is
                // invented merely because the transport was interrupted.
                after.clients[i].phase = 4;
            }
        }
        Event::Commit(caller) => {
            let i = caller.index();
            let client = before.clients[i];
            if client.phase == 1 {
                let result = if authorized(before) {
                    content_ticket::debit(DebitEvidence {
                        remaining: i64::from(if fault {
                            client.observed_remaining
                        } else {
                            before.remaining
                        }),
                        revoked: before.revoked,
                        expires: if before.expired { 1.0 } else { 2.0 },
                        now: 1.0,
                        requested: u64::from(client.length),
                    })
                } else {
                    DebitEffect::Refuse
                };
                let expected = authorized(before) && client.length <= before.remaining;
                let required = if expected {
                    DebitEffect::Debit {
                        bytes: i64::from(client.length),
                        remaining: i64::from(before.remaining - client.length),
                    }
                } else {
                    DebitEffect::Refuse
                };
                after.violation |= result != required;
                match result {
                    DebitEffect::Debit { bytes, remaining } => {
                        after.remaining = remaining as u8;
                        after.spent += bytes as u8;
                        after.clients[i].phase = 2;
                    }
                    DebitEffect::Refuse => after.clients[i].phase = 4,
                }
                if expected {
                    after.violation |=
                        after.clients[i].phase != 2 || after.spent != before.spent + client.length;
                } else {
                    after.violation |= after.clients[i].phase != 4
                        || after.spent != before.spent
                        || after.remaining != before.remaining;
                }
            }
        }
        Event::Read(caller, digest) => {
            let i = caller.index();
            let client = before.clients[i];
            if client.phase == 2 {
                let final_chunk = client.delivered + 1 == client.length;
                let verification =
                    transfer_completion::verification(true, 0, u64::from(client.length), 2);
                let expected_verification = if client.length == 2 {
                    transfer_completion::Verification::Required
                } else {
                    transfer_completion::Verification::NotRequired
                };
                after.violation |= verification != expected_verification;
                let evidence = match digest {
                    Digest::Match => Some(true),
                    Digest::Mismatch => Some(false),
                    Digest::Missing => None,
                };
                let release = authorized(before)
                    && (!final_chunk
                        || transfer_completion::finish(verification, evidence)
                            == transfer_completion::Effect::Release);
                let expected = authorized(before)
                    && (!final_chunk || client.length == 1 || digest == Digest::Match);
                after.violation |= release != expected;
                if release {
                    after.clients[i].delivered += 1;
                    if final_chunk {
                        after.clients[i].phase = 3;
                    }
                } else {
                    after.clients[i].phase = 4;
                }
                after.violation |=
                    after.clients[i].delivered != client.delivered + u8::from(expected);
                if expected && final_chunk {
                    after.violation |= after.clients[i].phase != 3;
                }
                if !expected {
                    after.violation |= after.clients[i].phase != 4;
                }
            }
        }
        _ => {}
    }
    if !matches!(event, Event::Commit(_)) {
        // Transport failure, interruption and authority changes never debit or
        // refund the ticket; only a successful owning transaction can debit.
        after.violation |= after.spent != before.spent || after.remaining != before.remaining;
    }
    after.violation |= after.spent + after.remaining != 2;
    after.violation |= after
        .clients
        .iter()
        .map(|c| u16::from(c.delivered))
        .sum::<u16>()
        > u16::from(after.spent);
    after
}
impl Model for Adapter {
    type State = Audited;
    type Input = Event;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "catabolic-content-ticket".into(),
            model_version: 2,
            properties_version: 2,
            codec_version: 2,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/content_ticket.rs").as_slice(),
                        include_bytes!("../../catabolic-core/src/transfer_completion.rs")
                            .as_slice(),
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
            clients: [Client::default(); 2],
            revoked: false,
            expired: false,
            revision_changed: false,
            authority: 0,
            violation: false,
        })
    }
    fn step(
        &self,
        before: &Audited,
        event: &Event,
    ) -> Result<Transition<Audited, String>, ModelError> {
        Ok(Transition::accepted(
            step_state(before, event, self.fault),
            vec![format!("{event:?}")],
        ))
    }
    fn check_state(&self, s: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.violation {
            Check::failed(
                "ticket.allowance",
                "competing debit, authority or transfer completion obligation violated",
            )
        } else {
            Check::passed("ticket.allowance")
        }])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        let mut events = vec![
            Event::Revoke,
            Event::Expire,
            Event::ChangeRevision,
            Event::RevokeGrant,
            Event::RevokeToken,
            Event::ChangeOwner,
        ];
        for caller in [Caller::A, Caller::B] {
            events.extend([
                Event::Prepare(caller, false),
                Event::Prepare(caller, true),
                Event::Commit(caller),
                Event::Interrupt(caller),
                Event::Read(caller, Digest::Match),
                Event::Read(caller, Digest::Mismatch),
                Event::Read(caller, Digest::Missing),
            ]);
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
        let state: Audited =
            serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))?;
        if state.remaining > 2
            || state.spent > 4
            || state.authority > 3
            || state.clients.iter().any(|c| {
                c.phase > 4 || c.length > 2 || c.observed_remaining > 2 || c.delivered > c.length
            })
        {
            return Err(ModelError::new("ticket state exceeds documented bounds"));
        }
        Ok(state)
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
            ("clients".into(), "2; full or partial response".into()),
            ("authority".into(), "grant, token, owner, revision".into()),
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

#[cfg(test)]
mod tests {
    use super::*;
    fn run(events: &[Event], fault: bool) -> Audited {
        let mut state = Adapter { fault }.initial_state().unwrap();
        for event in events {
            state = step_state(&state, event, fault);
        }
        state
    }
    #[test]
    fn zero_and_oversized_debits_preserve_the_prior_boundary_domain() {
        for remaining in 0..=2 {
            for requested in 0..=3 {
                for revoked in [false, true] {
                    for expired in [false, true] {
                        let actual = content_ticket::debit(DebitEvidence {
                            remaining,
                            revoked,
                            expires: if expired { 1.0 } else { 2.0 },
                            now: 1.0,
                            requested,
                        });
                        let expected = if !revoked && !expired && requested <= remaining as u64 {
                            DebitEffect::Debit {
                                bytes: requested as i64,
                                remaining: remaining - requested as i64,
                            }
                        } else {
                            DebitEffect::Refuse
                        };
                        assert_eq!(actual, expected);
                    }
                }
            }
        }
    }

    #[test]
    fn concurrent_preparation_does_not_reserve_the_same_allowance_twice() {
        let events = [
            Event::Prepare(Caller::A, true),
            Event::Prepare(Caller::B, true),
            Event::Commit(Caller::A),
            Event::Commit(Caller::B),
        ];
        let state = run(&events, false);
        assert!(!state.violation);
        assert_eq!(state.spent, 2);
        assert_eq!(state.clients[1].phase, 4);
        assert!(run(&events, true).violation);
    }
    #[test]
    fn failed_digest_and_authority_change_withhold_final_chunk_without_refund() {
        for ending in [
            Event::Read(Caller::A, Digest::Mismatch),
            Event::Read(Caller::A, Digest::Missing),
        ] {
            let state = run(
                &[
                    Event::Prepare(Caller::A, true),
                    Event::Commit(Caller::A),
                    Event::Read(Caller::A, Digest::Match),
                    ending,
                ],
                false,
            );
            assert!(!state.violation);
            assert_eq!(state.clients[0].delivered, 1);
            assert_eq!(state.clients[0].phase, 4);
            assert_eq!(state.spent, 2);
        }
        let state = run(
            &[
                Event::Prepare(Caller::A, true),
                Event::Commit(Caller::A),
                Event::Read(Caller::A, Digest::Match),
                Event::ChangeRevision,
                Event::Read(Caller::A, Digest::Match),
            ],
            false,
        );
        assert!(!state.violation);
        assert_eq!(state.clients[0].delivered, 1);
        assert_eq!(state.clients[0].phase, 4);
    }
}
