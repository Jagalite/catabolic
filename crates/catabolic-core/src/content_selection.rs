// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Ordered content selection. The adapter supplies candidate, definition and
//! openability evidence in source-then-generated order and performs I/O.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Candidate,
    Definition,
    Open,
    Selected,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    Candidate { requested_file_matches: bool },
    Definition { matches: bool },
    Opened { usable: bool },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Continue,
    CheckDefinition,
    Open,
    Select,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    OutOfOrder,
}
/// Every accepted step produces one explicit next action; once a candidate is
/// selected, no later candidate can be probed in the same selection.
pub fn advance(state: State, input: Input) -> Result<(State, Effect), Rejection> {
    match (state, input) {
        (
            State::Candidate,
            Input::Candidate {
                requested_file_matches: false,
            },
        ) => Ok((State::Candidate, Effect::Continue)),
        (
            State::Candidate,
            Input::Candidate {
                requested_file_matches: true,
            },
        ) => Ok((State::Definition, Effect::CheckDefinition)),
        (State::Definition, Input::Definition { matches: false }) => {
            Ok((State::Candidate, Effect::Continue))
        }
        (State::Definition, Input::Definition { matches: true }) => Ok((State::Open, Effect::Open)),
        (State::Open, Input::Opened { usable: false }) => Ok((State::Candidate, Effect::Continue)),
        (State::Open, Input::Opened { usable: true }) => Ok((State::Selected, Effect::Select)),
        _ => Err(Rejection::OutOfOrder),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unusable_candidates_cannot_be_selected_and_selection_is_terminal() {
        let (s, e) = advance(
            State::Candidate,
            Input::Candidate {
                requested_file_matches: true,
            },
        )
        .unwrap();
        assert_eq!(e, Effect::CheckDefinition);
        let (s, e) = advance(s, Input::Definition { matches: true }).unwrap();
        assert_eq!(e, Effect::Open);
        let (s, e) = advance(s, Input::Opened { usable: false }).unwrap();
        assert_eq!((s, e), (State::Candidate, Effect::Continue));
        let (s, _) = advance(
            s,
            Input::Candidate {
                requested_file_matches: true,
            },
        )
        .unwrap();
        let (s, _) = advance(s, Input::Definition { matches: true }).unwrap();
        let (s, e) = advance(s, Input::Opened { usable: true }).unwrap();
        assert_eq!((s, e), (State::Selected, Effect::Select));
        assert_eq!(
            advance(
                s,
                Input::Candidate {
                    requested_file_matches: true
                }
            ),
            Err(Rejection::OutOfOrder)
        );
    }
    #[test]
    fn ordered_candidates_match_independent_first_eligible_oracle() {
        // Four candidates cover mismatches and failures before and after a winner.
        for bits in 0_u16..(1 << 12) {
            let candidates: Vec<_> = (0..4).map(|i| (bits >> (3 * i)) & 7).collect();
            let expected = candidates.iter().position(|candidate| *candidate == 7);
            let mut state = State::Candidate;
            let mut chosen = None;
            let mut probes = Vec::new();
            for (i, candidate) in candidates.iter().copied().enumerate() {
                let (next, effect) = advance(
                    state,
                    Input::Candidate {
                        requested_file_matches: candidate & 1 != 0,
                    },
                )
                .unwrap();
                state = next;
                if effect == Effect::Continue {
                    continue;
                }
                probes.push((i, "definition"));
                let (next, effect) = advance(
                    state,
                    Input::Definition {
                        matches: candidate & 2 != 0,
                    },
                )
                .unwrap();
                state = next;
                if effect == Effect::Continue {
                    continue;
                }
                probes.push((i, "open"));
                let (next, effect) = advance(
                    state,
                    Input::Opened {
                        usable: candidate & 4 != 0,
                    },
                )
                .unwrap();
                state = next;
                if effect == Effect::Select {
                    chosen = Some(i);
                    break;
                }
            }
            assert_eq!(chosen, expected, "bits={bits}");
            let expected_probes: Vec<_> = candidates
                .iter()
                .copied()
                .enumerate()
                .take(expected.map_or(4, |i| i + 1))
                .flat_map(|(i, candidate)| {
                    let mut probes = Vec::new();
                    if candidate & 1 != 0 {
                        probes.push((i, "definition"));
                    }
                    if candidate & 3 == 3 {
                        probes.push((i, "open"));
                    }
                    probes
                })
                .collect();
            assert_eq!(probes, expected_probes, "bits={bits}");
        }
    }
}
