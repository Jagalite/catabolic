// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Publication verification is tied to the generation actually observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Evidence {
    pub observed_generation: Option<i64>,
    pub current_generation: i64,
    pub verified_generation: i64,
    pub binding_matches: bool,
    pub healthy: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Verify { generation: i64 },
    RetainPending,
}
/// A successful verification cannot acknowledge a newer publication that was
/// admitted after evidence was captured, or a changed output binding.
pub fn decide(e: Evidence) -> Effect {
    if e.healthy
        && e.binding_matches
        && e.observed_generation == Some(e.current_generation)
        && e.current_generation > e.verified_generation
    {
        Effect::Verify {
            generation: e.current_generation,
        }
    } else {
        Effect::RetainPending
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn new_generation_and_changed_binding_are_not_acknowledged() {
        let e = Evidence {
            observed_generation: Some(3),
            current_generation: 3,
            verified_generation: 2,
            binding_matches: true,
            healthy: true,
        };
        assert_eq!(decide(e), Effect::Verify { generation: 3 });
        assert_eq!(
            decide(Evidence {
                current_generation: 4,
                ..e
            }),
            Effect::RetainPending
        );
        assert_eq!(
            decide(Evidence {
                binding_matches: false,
                ..e
            }),
            Effect::RetainPending
        );
        assert_eq!(
            decide(Evidence {
                healthy: false,
                ..e
            }),
            Effect::RetainPending
        );
        assert_eq!(
            decide(Evidence {
                observed_generation: None,
                ..e
            }),
            Effect::RetainPending
        );
        assert_eq!(
            decide(Evidence {
                verified_generation: 3,
                ..e
            }),
            Effect::RetainPending
        );
    }
}
