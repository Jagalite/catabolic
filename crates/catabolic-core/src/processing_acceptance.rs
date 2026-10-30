// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Decisions around result acceptance; the adapter observes source and baseline
//! evidence and applies effects after atomically fencing the execution claim.
use crate::execution::Phase;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceAction {
    Validate,
    AcceptReported,
}
/// A successful result or an invalid-media finding must still describe the
/// source that was actually processed.
pub fn source_action(reported: Phase, invalid_media: bool) -> SourceAction {
    if reported == Phase::Complete || invalid_media {
        SourceAction::Validate
    } else {
        SourceAction::AcceptReported
    }
}
/// Compare every recorded inventory field, except ctime (validated against
/// the live file separately), against the transaction's current observation.
pub fn inventory_matches(expected: &serde_json::Value, current: &serde_json::Value) -> bool {
    expected.as_object().is_some_and(|recorded| {
        recorded
            .iter()
            .all(|(key, value)| key == "ctime_ns" || current.get(key) == Some(value))
    })
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaselineAction {
    Preserve,
    Establish,
    Mismatch,
}
/// A mismatching digest never overwrites the durable checksum baseline.
pub fn baseline_action(baseline: Option<&str>, digest: &str) -> BaselineAction {
    match baseline {
        None => BaselineAction::Establish,
        Some(value) if value == digest => BaselineAction::Preserve,
        Some(_) => BaselineAction::Mismatch,
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RetryDecision {
    pub retryable: bool,
    pub due_at: f64,
    pub enqueue: bool,
}
/// Attempt identity and explicit clock come from the fenced execution claim.
pub fn retry_decision(
    outcome: Phase,
    requested: bool,
    attempt: i64,
    delay_seconds: u32,
    now: f64,
) -> RetryDecision {
    let retryable = matches!(outcome, Phase::Failed | Phase::Timeout) && requested;
    let exponent = attempt.saturating_sub(1).clamp(0, 12) as u32;
    let delay = delay_seconds.saturating_mul(1u32 << exponent).min(3600);
    RetryDecision {
        retryable,
        due_at: now + f64::from(delay),
        enqueue: retryable && attempt <= 10,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn baseline_is_never_replaced_and_retry_remains_attempt_bound() {
        let expected = serde_json::json!({"size": 10, "inode": 2, "ctime_ns": 5});
        assert!(inventory_matches(
            &expected,
            &serde_json::json!({"size": 10, "inode": 2, "ctime_ns": 6})
        ));
        assert!(!inventory_matches(
            &expected,
            &serde_json::json!({"size": 11, "inode": 2})
        ));
        assert!(!inventory_matches(
            &expected,
            &serde_json::json!({"size": 10})
        ));
        assert_eq!(baseline_action(None, "new"), BaselineAction::Establish);
        assert_eq!(
            baseline_action(Some("old"), "old"),
            BaselineAction::Preserve
        );
        assert_eq!(
            baseline_action(Some("old"), "new"),
            BaselineAction::Mismatch
        );
        assert_eq!(
            source_action(Phase::Complete, false),
            SourceAction::Validate
        );
        assert_eq!(source_action(Phase::Failed, true), SourceAction::Validate);
        assert_eq!(
            source_action(Phase::Failed, false),
            SourceAction::AcceptReported
        );
        assert_eq!(
            retry_decision(Phase::Timeout, true, 10, 2, 100.0),
            RetryDecision {
                retryable: true,
                due_at: 1124.0,
                enqueue: true
            }
        );
        assert!(!retry_decision(Phase::Timeout, true, 11, 2, 100.0).enqueue);
        assert!(!retry_decision(Phase::Mismatch, true, 1, 2, 100.0).retryable);
    }
}
