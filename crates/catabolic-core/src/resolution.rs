// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Production fallback selection and failback decisions. Candidate ordering,
//! bounded probe results, old eligibility and time are explicit inputs. No I/O.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Observation {
    Unchecked,
    Usable,
    Unavailable,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub file_id: String,
    pub rank: Vec<(u8, f64)>,
    pub observation: Observation,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TierEffect {
    Probe(usize),
    Select(usize),
    Ambiguous,
    Unavailable,
}
/// The store passes candidates in deterministic (rank,file_id) order. Exactly
/// one next probe is requested, so lower-ranked candidates cannot consume a
/// budget after a usable top rank or leak extra filesystem observations.
pub fn tier(candidates: &[Candidate], tie_break: bool, package: bool) -> TierEffect {
    let mut usable: Vec<usize> = Vec::new();
    for (index, candidate) in candidates.iter().enumerate() {
        if let Some(&first) = usable.first()
            && candidate.rank != candidates[first].rank
        {
            break;
        }
        match candidate.observation {
            Observation::Unchecked => return TierEffect::Probe(index),
            Observation::Unavailable => {}
            Observation::Usable => {
                usable.push(index);
                if tie_break && !package {
                    return TierEffect::Select(index);
                }
            }
        }
    }
    if tie_break && package && !usable.is_empty() {
        let first = usable
            .iter()
            .map(|&i| &candidates[i].file_id)
            .min()
            .unwrap();
        usable.retain(|&i| &candidates[i].file_id == first);
    }
    match usable.as_slice() {
        [] => TierEffect::Unavailable,
        [index] => TierEffect::Select(*index),
        _ => TierEffect::Ambiguous,
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailbackMode {
    Immediate,
    Stable,
    Manual,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Failback {
    pub mode: FailbackMode,
    pub manual_override: bool,
    pub improving_tier: bool,
    pub previous_eligible_and_live: bool,
    pub successful_checks: u64,
    pub minimum_successful_checks: u64,
    pub healthy_since: f64,
    pub now: f64,
    pub minimum_healthy_seconds: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailbackEffect {
    Preferred,
    RetainPrevious,
}
pub fn failback(value: Failback) -> FailbackEffect {
    if !value.improving_tier
        || !value.previous_eligible_and_live
        || value.manual_override
        || value.mode == FailbackMode::Immediate
    {
        return FailbackEffect::Preferred;
    }
    if value.mode == FailbackMode::Manual
        || value.successful_checks < value.minimum_successful_checks
        || !value.now.is_finite()
        || !value.healthy_since.is_finite()
        || value.now - value.healthy_since < value.minimum_healthy_seconds as f64
    {
        FailbackEffect::RetainPrevious
    } else {
        FailbackEffect::Preferred
    }
}
/// Admission fences are pure; SQL owns their capture and applies the effect in
/// the same write transaction as the new projection generation.
#[derive(Clone, Copy, Debug)]
pub struct ProjectionAdmission {
    pub expected_epoch: i64,
    pub current_epoch: i64,
    pub expected_generation: i64,
    pub current_generation: i64,
    pub safe: bool,
    pub changes: usize,
    pub removals: usize,
    pub max_changes: usize,
    pub max_removals: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectionEffect {
    pub generation: i64,
    pub retain: bool,
}
pub fn projection(input: ProjectionAdmission) -> Result<ProjectionEffect, &'static str> {
    if input.expected_epoch != input.current_epoch {
        return Err("stale_resolution_plan");
    }
    if input.expected_generation != input.current_generation {
        return Err("stale_resolution_generation");
    }
    if input.safe && (input.changes > input.max_changes || input.removals > input.max_removals) {
        return Err("fallback_change_or_removal_budget");
    }
    let generation = input
        .current_generation
        .checked_add(1)
        .ok_or("fallback_generation_overflow")?;
    Ok(ProjectionEffect {
        generation,
        retain: !input.safe,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_failed_top_rank_does_not_suppress_live_lower_rank() {
        let mut c = vec![
            Candidate {
                file_id: "a".into(),
                rank: vec![(0, 0.0)],
                observation: Observation::Unavailable,
            },
            Candidate {
                file_id: "b".into(),
                rank: vec![(0, 1.0)],
                observation: Observation::Unchecked,
            },
        ];
        assert_eq!(tier(&c, false, false), TierEffect::Probe(1));
        c[1].observation = Observation::Usable;
        assert_eq!(tier(&c, false, false), TierEffect::Select(1));
    }
    #[test]
    fn multiple_component_choices_in_one_file_remain_ambiguous() {
        let c = vec![
            Candidate {
                file_id: "a".into(),
                rank: vec![],
                observation: Observation::Usable
            };
            2
        ];
        assert_eq!(tier(&c, true, true), TierEffect::Ambiguous);
        assert_eq!(tier(&c, true, false), TierEffect::Select(0));
    }
    #[test]
    fn manual_failback_never_holds_a_failed_previous_candidate() {
        let f = Failback {
            mode: FailbackMode::Manual,
            manual_override: false,
            improving_tier: true,
            previous_eligible_and_live: false,
            successful_checks: 0,
            minimum_successful_checks: 2,
            healthy_since: 0.,
            now: 1.,
            minimum_healthy_seconds: 300,
        };
        assert_eq!(failback(f), FailbackEffect::Preferred);
        assert_eq!(
            failback(Failback {
                previous_eligible_and_live: true,
                ..f
            }),
            FailbackEffect::RetainPrevious
        );
    }
}
