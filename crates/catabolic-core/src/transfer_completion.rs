// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Decisions for releasing a complete transfer after digest verification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verification {
    Required,
    NotRequired,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Release,
    Reject,
}
/// Full responses for recorded generated outputs require a streaming digest.
pub fn verification(recorded_digest: bool, start: u64, length: u64, size: u64) -> Verification {
    if recorded_digest && start == 0 && length == size {
        Verification::Required
    } else {
        Verification::NotRequired
    }
}
/// Missing or failed digest evidence cannot complete a verified response.
pub fn finish(required: Verification, digest_matches: Option<bool>) -> Effect {
    match (required, digest_matches) {
        (Verification::Required, Some(true)) | (Verification::NotRequired, _) => Effect::Release,
        (Verification::Required, _) => Effect::Reject,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_generated_transfer_requires_matching_digest() {
        assert_eq!(verification(true, 0, 10, 10), Verification::Required);
        assert_eq!(verification(true, 1, 9, 10), Verification::NotRequired);
        assert_eq!(verification(false, 0, 10, 10), Verification::NotRequired);
        assert_eq!(finish(Verification::Required, Some(true)), Effect::Release);
        assert_eq!(finish(Verification::Required, Some(false)), Effect::Reject);
        assert_eq!(finish(Verification::Required, None), Effect::Reject);
    }
}
