// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Explicit playback worker policy shared by execution and verification.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum State {
    Queued,
    Running,
    Ready,
    Failed,
    Cancelled,
    Stale,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Evidence {
    pub authorized_viewers: u32,
    pub source_current: bool,
    pub tools_current: bool,
    pub destination_current: bool,
    pub within_budget: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Event {
    Start,
    Poll,
    Complete { valid_output: bool },
    Recover,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Decision {
    pub state: State,
    pub error: Option<&'static str>,
    pub stop: bool,
    pub release_reservation: bool,
}
pub fn decide(state: State, evidence: Evidence, event: Event) -> Decision {
    let mut d = Decision {
        state,
        error: None,
        stop: false,
        release_reservation: !matches!(state, State::Queued | State::Running),
    };
    if !matches!(state, State::Queued | State::Running) {
        return d;
    }
    if event == Event::Recover {
        if state == State::Running {
            d.state = State::Failed;
            d.error = Some("playback_worker_interrupted");
        } else {
            return d;
        }
    } else if !evidence.tools_current {
        d.state = State::Failed;
        d.error = Some("playback_tools_changed");
    } else if evidence.authorized_viewers == 0 {
        d.state = State::Cancelled;
        d.error = Some("playback_cancelled");
    } else if !evidence.source_current {
        d.state = State::Failed;
        d.error = Some("playback_source_changed");
    } else if !evidence.destination_current {
        d.state = State::Failed;
        d.error = Some("playback_destination_changed");
    } else if !evidence.within_budget {
        d.state = State::Failed;
        d.error = Some("playback_storage_budget");
    } else {
        match event {
            Event::Start if state == State::Queued => d.state = State::Running,
            Event::Complete { valid_output } if state == State::Running => {
                d.state = if valid_output {
                    State::Ready
                } else {
                    State::Failed
                };
                if !valid_output {
                    d.error = Some("incomplete_playback_output");
                }
            }
            _ => {}
        }
    }
    d.stop = state == State::Running && d.state != State::Running;
    d.release_reservation = !matches!(d.state, State::Queued | State::Running);
    d
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_exhaustive_fences_and_shared_cancellation() {
        for state in [
            State::Queued,
            State::Running,
            State::Ready,
            State::Failed,
            State::Cancelled,
            State::Stale,
        ] {
            for viewers in 0..=2 {
                for mask in 0..16 {
                    for event in [
                        Event::Start,
                        Event::Poll,
                        Event::Complete {
                            valid_output: false,
                        },
                        Event::Complete { valid_output: true },
                        Event::Recover,
                    ] {
                        let e = Evidence {
                            authorized_viewers: viewers,
                            source_current: mask & 1 != 0,
                            tools_current: mask & 2 != 0,
                            destination_current: mask & 4 != 0,
                            within_budget: mask & 8 != 0,
                        };
                        let d = decide(state, e, event);
                        let terminal = matches!(
                            state,
                            State::Ready | State::Failed | State::Cancelled | State::Stale
                        );
                        if terminal {
                            assert_eq!(d.state, state);
                            assert!(!d.stop);
                        }
                        if !terminal && event != Event::Recover && (viewers == 0 || mask != 15) {
                            assert!(matches!(d.state, State::Failed | State::Cancelled));
                        }
                        if state == State::Running
                            && viewers > 0
                            && mask == 15
                            && event == Event::Poll
                        {
                            assert_eq!(d.state, State::Running);
                            assert!(!d.stop);
                        }
                        if d.state == State::Ready && !terminal {
                            assert_eq!(state, State::Running);
                            assert!(viewers > 0 && mask == 15);
                            assert_eq!(event, Event::Complete { valid_output: true });
                        }
                        assert_eq!(
                            d.release_reservation,
                            !matches!(d.state, State::Queued | State::Running)
                        );
                    }
                }
            }
        }
    }
}
