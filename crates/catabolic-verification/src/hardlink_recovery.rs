// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! One create/replace intent with predecessor, desired and foreign inode classes.
//! Durable state includes the journal, staged/retained/output references and
//! committed ownership. Crashes discard the worker but preserve these references;
//! recovery resumes from them. The filesystem stage effects are abstracted,
//! while acknowledgement and cancellation call production core decisions.
//!
//! Bounds: one intent, one predecessor, one desired target and one foreign class
//! per name; no unbounded counters. A stable desired intent must progress in at
//! most four active steps. Arbitrary crashes/external mutation provide no
//! fairness guarantee. The final observation/SQLite acknowledgement is one
//! modeled boundary: this does not claim atomic exclusion of an external writer
//! between the real final filesystem check and database commit. See the store's
//! publication_failpoints process-crash matrix and hardlink reopen regressions
//! for real effects; this model does not itself execute those adapters.
use catabolic_core::{
    encode,
    publication_recovery::{self, Effect, HardlinkEvidence},
    sha256,
};
use serde::{Deserialize, Serialize};
use stateless::automatic::Auto;
use stateless::execution::{ReplayOptions, ReplayOutcome, record, replay};
use stateless::explore::{SearchConfig, SearchTermination, ShrinkConfig, enumerate, shrink};
use stateless::trace::{ReadLimits, RunConfig, Termination, Trace};
use stateless::{Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition};
use std::{error::Error, fs::File, path::Path};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Reference {
    #[default]
    Absent,
    Previous,
    Desired,
    Foreign,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Audited {
    started: bool,
    active: bool,
    owner: bool,
    desired: bool,
    destination: Reference,
    staged: Reference,
    retained: Reference,
    owned: Reference,
    retention_required: bool,
    retention_recorded: bool,
    journal: bool,
    acknowledgements: u8,
    cancelled: bool,
    safety_violation: bool,
    progress_violation: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Event {
    Start(bool),
    Advance,
    Crash,
    Recover,
    Obsolete,
    Cancel(bool),
    LoseOwner,
    ChangeDestination,
    ChangeRetained,
    ChangeStaged,
}
struct Adapter {
    fault: bool,
}

// Independent progress measure for a stable, desired, uncontended intent.
// Each successful adapter invocation must eliminate one remaining boundary.
// Arbitrarily many crashes or a persistent foreign writer imply no liveness
// promise; blocked work must retain its journal instead.
fn remaining(s: &Audited) -> Option<u8> {
    use Reference::*;
    if !s.owner || !s.desired || !s.journal {
        return None;
    }
    if !matches!(s.staged, Absent | Desired) || !matches!(s.retained, Absent | Previous) {
        return None;
    }
    match (s.retention_required, s.retained, s.destination, s.staged) {
        (true, Absent, Previous, Absent) => Some(4),
        (false, Absent, Absent, Absent) | (true, Previous, Absent, Absent) => Some(3),
        (false, Absent, Absent, Desired) | (true, Previous, Absent, Desired) => Some(2),
        (false, Absent, Desired, Absent) | (true, Previous, Desired, Absent) => Some(1),
        _ => None,
    }
}
impl Adapter {
    fn advance(&self, s: &mut Audited) -> &'static str {
        use Reference::*;
        if !s.active || !s.owner || !s.journal {
            return "blocked";
        }
        // These three effects abstract the adapter's exclusive rename/link
        // operations. They are not claimed to be a second implementation of
        // the filesystem owner. Real effects are checked by publication's
        // process-crash matrix and durable reopen tests.
        if s.retention_required && s.retained == Absent && s.destination == Previous {
            if !s.desired {
                return "blocked";
            }
            s.retained = Previous;
            s.destination = Absent;
            return "retain";
        }
        if s.destination == Absent && (!s.retention_required || s.retained == Previous) {
            if !s.desired {
                return "blocked";
            }
            if s.staged == Absent {
                s.staged = Desired;
                return "stage";
            }
            if s.staged == Desired {
                s.destination = Desired;
                s.staged = Absent;
                return "publish";
            }
            return "blocked";
        }
        let evidence = HardlinkEvidence {
            owner_matches: s.owner,
            destination_matches: s.destination == Desired,
            retention_required: s.retention_required,
            retention_matches: s.retained == Previous,
        };
        let mut effect = publication_recovery::acknowledge_hardlink(evidence);
        if self.fault && s.destination == Desired && s.retention_required {
            // Deliberately omit the retained-reference requirement at the
            // acknowledgement boundary; a changed retained inode must expose it.
            effect = Effect::Acknowledge;
        }
        if effect == Effect::Acknowledge {
            s.owned = Desired;
            s.retention_recorded = s.retention_required;
            s.journal = false;
            s.acknowledgements += 1;
            "acknowledge"
        } else {
            "blocked"
        }
    }
    fn cancel(s: &mut Audited, force: bool) -> &'static str {
        use Reference::*;
        use publication_recovery::HardlinkCancellation;
        if !s.owner || !s.journal {
            return "blocked";
        }
        let untouched = if s.retention_required {
            s.destination == Previous && s.retained == Absent
        } else {
            s.destination == Absent
        };
        if !publication_recovery::hardlink_cancellable_layout(
            untouched,
            s.destination == Absent,
            s.retained == Previous,
        ) {
            return "blocked";
        }
        match publication_recovery::hardlink_cancellation(force, !s.desired, s.staged != Absent) {
            HardlinkCancellation::Cancel => {
                if s.retained == Previous {
                    s.retention_recorded = true;
                    s.owned = Absent;
                }
                s.journal = false;
                s.cancelled = true;
                "cancel"
            }
            HardlinkCancellation::KeepIntent | HardlinkCancellation::PreserveStaged => "blocked",
        }
    }
}
impl Adapter {
    fn evolve(&self, before: &Audited, event: &Event) -> (Audited, String) {
        use Reference::*;
        let mut after = before.clone();
        let action = match event {
            Event::Start(required) if !before.started => {
                after.started = true;
                after.active = true;
                after.owner = true;
                after.desired = true;
                after.destination = if *required { Previous } else { Absent };
                after.owned = after.destination;
                after.retention_required = *required;
                after.journal = true;
                "start"
            }
            Event::Advance => self.advance(&mut after),
            Event::Crash => {
                after.active = false;
                "crash"
            }
            Event::Recover if before.started && before.journal => {
                after.active = true;
                "recover"
            }
            Event::Obsolete if before.started => {
                after.desired = false;
                "obsolete"
            }
            Event::Cancel(force) => Self::cancel(&mut after, *force),
            Event::LoseOwner if before.started => {
                after.owner = false;
                "lose-owner"
            }
            Event::ChangeDestination if before.started => {
                after.destination = Foreign;
                "foreign-destination"
            }
            Event::ChangeRetained if before.retained != Absent => {
                after.retained = Foreign;
                "foreign-retained"
            }
            Event::ChangeStaged if before.staged != Absent => {
                after.staged = Foreign;
                "foreign-staged"
            }
            _ => "unchanged",
        };
        // Independent transition obligations inspect durable preconditions,
        // not the reducer's returned effect or its evidence-construction code.
        if before.journal
            && !after.journal
            && !after.cancelled
            && (!before.owner
                || before.destination != Desired
                || before.staged != Absent
                || (before.retention_required && before.retained != Previous)
                || after.owned != Desired
                || after.retention_recorded != before.retention_required
                || after.acknowledgements != before.acknowledgements + 1)
        {
            after.safety_violation = true;
        }
        if before.journal && !after.journal && after.cancelled {
            let allowed = matches!(event, Event::Cancel(true)) || !before.desired;
            let untouched = if before.retention_required {
                before.destination == Previous && before.retained == Absent
            } else {
                before.destination == Absent
            };
            if !allowed
                || !before.owner
                || before.staged != Absent
                || !(untouched || (before.destination == Absent && before.retained == Previous))
                || after.acknowledgements != before.acknowledgements
                || (before.retained == Previous
                    && (!after.retention_recorded || after.owned != Absent))
            {
                after.safety_violation = true;
            }
        }
        if matches!(event, Event::Advance | Event::Cancel(_))
            && ((before.destination == Foreign && after.destination != Foreign)
                || (before.retained == Foreign && after.retained != Foreign)
                || (before.staged == Foreign && after.staged != Foreign)
                || (before.retained == Previous && after.retained != Previous)
                || (before.staged != Absent && !after.journal))
        {
            after.safety_violation = true;
        }
        if matches!(event, Event::Advance)
            && before.active
            && let Some(rank) = remaining(before)
        {
            let progressed = if rank == 1 {
                !after.journal && after.acknowledgements == before.acknowledgements + 1
            } else {
                remaining(&after) == Some(rank - 1)
            };
            after.progress_violation |= !progressed;
        }
        (after, format!("{event:?}:{action}"))
    }
}

impl Model for Adapter {
    type State = Audited;
    type Input = Event;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "catabolic-hardlink-acknowledgement".into(),
            model_version: 2,
            properties_version: 2,
            codec_version: 2,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/publication_recovery.rs")
                            .as_slice(),
                        include_bytes!("hardlink_recovery.rs").as_slice(),
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
        let (after, action) = self.evolve(before, event);
        Ok(Transition::accepted(after, vec![action]))
    }

    fn check_state(&self, s: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![
            if s.safety_violation
                || s.acknowledgements > 1
                || (s.cancelled && s.acknowledgements != 0)
            {
                Check::failed(
                    "hardlink.safety",
                    "foreign/staged/retained reference or durable acknowledgement obligation violated",
                )
            } else {
                Check::passed("hardlink.safety")
            },
            if s.progress_violation {
                Check::failed(
                    "hardlink.progress",
                    "stable recovery omitted a required stage or acknowledgement",
                )
            } else {
                Check::passed("hardlink.progress")
            },
        ])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        Ok(vec![
            Event::Start(false),
            Event::Start(true),
            Event::Advance,
            Event::Crash,
            Event::Recover,
            Event::Obsolete,
            Event::Cancel(false),
            Event::Cancel(true),
            Event::LoseOwner,
            Event::ChangeDestination,
            Event::ChangeRetained,
            Event::ChangeStaged,
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
            "hardlink lifecycle check failed/incomplete: {:?}",
            r.termination
        )
        .into());
    }
    println!(
        "hardlink lifecycle graph exhausted: {} states, {} transitions, zero skipped checks",
        r.states, r.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let f = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected hardlink fault not found")?;
    let reduced = shrink(&model, &f, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("hardlink shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-hardlink-acknowledgement".into(),
        seed: None,
        parameters: vec![
            ("intents".into(), "one create or replace".into()),
            ("stable-recovery-steps".into(), "4".into()),
        ],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &f.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("hardlink trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "hardlink injected fault found and shrunk: {} -> {} inputs",
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
        return Err("hardlink fresh replay did not match".into());
    }
    println!("hardlink fresh-process replay exact, failed property reproduced, matching build");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(fault: bool, inputs: &[Event]) -> Audited {
        let adapter = Adapter { fault };
        let mut state = adapter.initial_state().unwrap();
        for input in inputs {
            state = adapter.evolve(&state, input).0;
        }
        state
    }

    #[test]
    fn stable_create_and_replace_recover_after_every_boundary() {
        for required in [false, true] {
            let steps = if required { 4 } else { 3 };
            for boundary in 0..=steps {
                let mut events = vec![Event::Start(required)];
                events.extend(std::iter::repeat_n(Event::Advance, boundary));
                events.extend([Event::Crash, Event::Recover]);
                events.extend(std::iter::repeat_n(Event::Advance, steps));
                events.extend([Event::Crash, Event::Recover, Event::Advance]);
                let state = run(false, &events);
                assert!(!state.journal);
                assert_eq!(state.acknowledgements, 1);
                assert_eq!(state.owned, Reference::Desired);
                assert_eq!(state.retention_recorded, required);
                assert!(!state.safety_violation);
                assert!(!state.progress_violation);
            }
        }
    }

    #[test]
    fn retained_reference_fault_requires_a_staged_publication_trace() {
        let events = [
            Event::Start(true),
            Event::Advance,
            Event::Advance,
            Event::Crash,
            Event::Recover,
            Event::Advance,
            Event::ChangeRetained,
            Event::Advance,
        ];
        let correct = run(false, &events);
        assert!(correct.journal);
        assert!(!correct.safety_violation);
        let faulty = run(true, &events);
        assert!(!faulty.journal);
        assert!(faulty.safety_violation);
    }

    #[test]
    fn foreign_output_and_staged_reference_survive_repeated_recovery() {
        let state = run(
            false,
            &[
                Event::Start(true),
                Event::Advance,
                Event::Advance,
                Event::ChangeDestination,
                Event::Crash,
                Event::Recover,
                Event::Advance,
                Event::Obsolete,
                Event::Cancel(true),
                Event::Crash,
                Event::Recover,
                Event::Advance,
            ],
        );
        assert_eq!(state.destination, Reference::Foreign);
        assert_eq!(state.retained, Reference::Previous);
        assert_eq!(state.staged, Reference::Desired);
        assert!(state.journal);
        assert_eq!(state.acknowledgements, 0);
        assert!(!state.safety_violation);
        assert!(!state.progress_violation);
    }
}
