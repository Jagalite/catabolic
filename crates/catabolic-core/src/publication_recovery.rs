// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Decisions for acknowledging a journaled symlink publication.
//! The store supplies fresh filesystem observations and commits the effects;
//! checks here cannot make filesystem and SQLite operations atomic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Prepare,
    Create,
    Replace,
    Remove,
    Forget,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observed {
    Absent,
    DesiredLink,
    Other,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Evidence {
    pub kind: Kind,
    pub owner_matches: bool,
    pub observed: Observed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Acknowledge,
    RetainJournal,
}
/// Preserve the journal if ownership or the observable publication changed.
/// `DesiredLink` means the adapter observed exactly this journal's target.
pub fn acknowledge(evidence: Evidence) -> Effect {
    let output_matches = match evidence.kind {
        Kind::Prepare => true,
        Kind::Create | Kind::Replace => evidence.observed == Observed::DesiredLink,
        Kind::Remove | Kind::Forget => evidence.observed == Observed::Absent,
    };
    if evidence.owner_matches && output_matches {
        Effect::Acknowledge
    } else {
        Effect::RetainJournal
    }
}
/// Hardlink acknowledgement also requires the retained predecessor when the
/// operation moved one. The adapter proves identity without following links.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HardlinkEvidence {
    pub owner_matches: bool,
    pub destination_matches: bool,
    pub retention_required: bool,
    pub retention_matches: bool,
}
pub fn acknowledge_hardlink(e: HardlinkEvidence) -> Effect {
    if e.owner_matches && e.destination_matches && (!e.retention_required || e.retention_matches) {
        Effect::Acknowledge
    } else {
        Effect::RetainJournal
    }
}
/// Only an untouched destination or a completed retention with an absent
/// destination can safely cancel an obsolete hardlink intent.
pub fn hardlink_cancellable_layout(
    untouched: bool,
    destination_absent: bool,
    retained: bool,
) -> bool {
    untouched || (destination_absent && retained)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HardlinkCancellation {
    KeepIntent,
    PreserveStaged,
    Cancel,
}
/// Staged links must remain named and recoverable even when the intent is
/// otherwise obsolete or forced to cancel.
pub fn hardlink_cancellation(force: bool, obsolete: bool, staged: bool) -> HardlinkCancellation {
    if !force && !obsolete {
        HardlinkCancellation::KeepIntent
    } else if staged {
        HardlinkCancellation::PreserveStaged
    } else {
        HardlinkCancellation::Cancel
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hardlink_cancellation_preserves_in_progress_and_staged_work() {
        assert!(!hardlink_cancellable_layout(false, false, true));
        assert!(hardlink_cancellable_layout(false, true, true));
        assert!(hardlink_cancellable_layout(true, false, false));
        assert_eq!(
            hardlink_cancellation(false, false, true),
            HardlinkCancellation::KeepIntent
        );
        assert_eq!(
            hardlink_cancellation(true, false, true),
            HardlinkCancellation::PreserveStaged
        );
        assert_eq!(
            hardlink_cancellation(false, true, false),
            HardlinkCancellation::Cancel
        );
    }
    #[test]
    fn acknowledgement_requires_owned_expected_output() {
        for kind in [
            Kind::Prepare,
            Kind::Create,
            Kind::Replace,
            Kind::Remove,
            Kind::Forget,
        ] {
            for observed in [Observed::Absent, Observed::DesiredLink, Observed::Other] {
                assert_eq!(
                    acknowledge(Evidence {
                        kind,
                        owner_matches: false,
                        observed
                    }),
                    Effect::RetainJournal
                );
                let expected = match kind {
                    Kind::Prepare => true,
                    Kind::Create | Kind::Replace => observed == Observed::DesiredLink,
                    Kind::Remove | Kind::Forget => observed == Observed::Absent,
                };
                assert_eq!(
                    acknowledge(Evidence {
                        kind,
                        owner_matches: true,
                        observed
                    }) == Effect::Acknowledge,
                    expected
                );
            }
        }
    }
    #[test]
    fn hardlink_ack_requires_output_and_retained_predecessor() {
        let e = HardlinkEvidence {
            owner_matches: true,
            destination_matches: true,
            retention_required: true,
            retention_matches: true,
        };
        assert_eq!(acknowledge_hardlink(e), Effect::Acknowledge);
        assert_eq!(
            acknowledge_hardlink(HardlinkEvidence {
                owner_matches: false,
                ..e
            }),
            Effect::RetainJournal
        );
        assert_eq!(
            acknowledge_hardlink(HardlinkEvidence {
                destination_matches: false,
                ..e
            }),
            Effect::RetainJournal
        );
        assert_eq!(
            acknowledge_hardlink(HardlinkEvidence {
                retention_matches: false,
                ..e
            }),
            Effect::RetainJournal
        );
        assert_eq!(
            acknowledge_hardlink(HardlinkEvidence {
                retention_required: false,
                retention_matches: false,
                ..e
            }),
            Effect::Acknowledge
        );
    }
}
