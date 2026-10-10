//! Application-owned shared request reducer. Every fence and observation is
//! explicit input; this module performs no I/O and reads no implicit clock.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Caller {
    #[default]
    Idle,
    Waiting,
    Cancelled,
    Delivered,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct State {
    pub generation: u8,
    pub running: bool,
    pub started: bool,
    pub callers: [Caller; 2],
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Input {
    Attach(u8),
    Cancel(u8),
    Advance(u8),
    Complete(u8),
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Effect {
    Start(u8),
    Stop(u8),
    Deliver { caller: u8, generation: u8 },
}
/// One caller cancelling cannot stop work needed by another caller. A completed
/// old generation cannot deliver into the current request, even after reattach.
pub fn reduce(before: &State, input: &Input) -> (State, Vec<Effect>) {
    let mut state = before.clone();
    let mut effects = vec![];
    match *input {
        Input::Attach(caller) if caller < 2 => {
            state.callers[usize::from(caller)] = Caller::Waiting;
            if !state.running && !state.started {
                state.running = true;
                state.started = true;
                effects.push(Effect::Start(state.generation));
            }
        }
        Input::Cancel(caller) if caller < 2 => {
            if state.callers[usize::from(caller)] == Caller::Waiting {
                state.callers[usize::from(caller)] = Caller::Cancelled;
            }
            if state.running && !state.callers.contains(&Caller::Waiting) {
                state.running = false;
                effects.push(Effect::Stop(state.generation));
            }
        }
        Input::Advance(generation) if generation > state.generation => {
            if state.running {
                effects.push(Effect::Stop(state.generation));
            }
            state.generation = generation;
            state.running = state.callers.contains(&Caller::Waiting);
            state.started = state.running;
            if state.running {
                effects.push(Effect::Start(generation));
            }
        }
        Input::Complete(generation) if generation == state.generation && state.running => {
            for (caller, status) in state.callers.iter_mut().enumerate() {
                if *status == Caller::Waiting {
                    *status = Caller::Delivered;
                    effects.push(Effect::Deliver {
                        caller: caller as u8,
                        generation,
                    });
                }
            }
            state.running = false;
        }
        _ => {}
    }
    (state, effects)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_is_per_caller_and_stale_completion_is_fenced() {
        let (state, _) = reduce(&State::default(), &Input::Attach(0));
        let (state, _) = reduce(&state, &Input::Attach(1));
        let (state, effects) = reduce(&state, &Input::Cancel(0));
        assert!(state.running);
        assert!(effects.is_empty());
        let (state, _) = reduce(&state, &Input::Advance(1));
        assert!(reduce(&state, &Input::Complete(0)).1.is_empty());
        let (state, effects) = reduce(&state, &Input::Complete(1));
        assert_eq!(state.callers, [Caller::Cancelled, Caller::Delivered]);
        assert_eq!(
            effects,
            vec![Effect::Deliver {
                caller: 1,
                generation: 1
            }]
        );
    }
}
