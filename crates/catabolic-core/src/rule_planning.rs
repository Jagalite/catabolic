// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Eligibility and admission policy for a rule batch. Catalog reads, arbitrary
//! precision storage arithmetic, and enqueue effects remain with adapters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateState {
    Missing,
    Stale,
    Failed,
    Other,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Eligibility {
    pub state: CandidateState,
    pub external: bool,
    pub retry_failed: bool,
}
pub fn eligible(facts: Eligibility) -> bool {
    matches!(facts.state, CandidateState::Missing)
        || (!facts.external && matches!(facts.state, CandidateState::Stale))
        || (facts.retry_failed && matches!(facts.state, CandidateState::Failed))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetEvidence {
    pub external: bool,
    pub any_candidates: bool,
    pub any_selected: bool,
    pub local_budget_supplied: bool,
    pub exceeds_available: bool,
    pub exceeds_local_budget: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocker {
    ExternalStorageUnknown,
    DestinationSpace,
    NewBytesBudget,
}
pub fn blockers(e: BudgetEvidence) -> Vec<Blocker> {
    let mut blockers = Vec::new();
    if e.external {
        if e.any_candidates && e.local_budget_supplied {
            blockers.push(Blocker::ExternalStorageUnknown);
        }
    } else {
        if e.any_selected && e.exceeds_available {
            blockers.push(Blocker::DestinationSpace);
        }
        if e.local_budget_supplied && e.exceeds_local_budget {
            blockers.push(Blocker::NewBytesBudget);
        }
    }
    blockers
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rule_admission_respects_external_and_retry_policy() {
        for external in [false, true] {
            for retry_failed in [false, true] {
                assert!(eligible(Eligibility {
                    state: CandidateState::Missing,
                    external,
                    retry_failed
                }));
                assert_eq!(
                    eligible(Eligibility {
                        state: CandidateState::Stale,
                        external,
                        retry_failed
                    }),
                    !external
                );
                assert_eq!(
                    eligible(Eligibility {
                        state: CandidateState::Failed,
                        external,
                        retry_failed
                    }),
                    retry_failed
                );
                assert!(!eligible(Eligibility {
                    state: CandidateState::Other,
                    external,
                    retry_failed
                }));
            }
        }
        assert_eq!(
            blockers(BudgetEvidence {
                external: true,
                any_candidates: true,
                any_selected: false,
                local_budget_supplied: true,
                exceeds_available: true,
                exceeds_local_budget: true
            }),
            vec![Blocker::ExternalStorageUnknown]
        );
        assert_eq!(
            blockers(BudgetEvidence {
                external: false,
                any_candidates: true,
                any_selected: true,
                local_budget_supplied: true,
                exceeds_available: true,
                exceeds_local_budget: true
            }),
            vec![Blocker::DestinationSpace, Blocker::NewBytesBudget]
        );
    }
}
