// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Ordered evidence for publishing a scan batch. The adapter obtains each fact
//! from the catalog or filesystem; this machine does not perform I/O.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fence {
    Catalog,
    Claim,
    Policy,
    Root,
    Ready,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Evidence {
    Catalog(bool),
    Claim(bool),
    Policy(bool),
    Root(bool),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    OutOfOrder,
    CatalogChanged,
    StaleClaim,
    PolicyChanged,
    RootChanged,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeEffect {
    RecordOnly,
    PublishMissing,
}
/// Evidence must be fresh and supplied in order on every publication attempt.
pub fn advance(fence: Fence, evidence: Evidence) -> Result<Fence, Rejection> {
    match (fence, evidence) {
        (Fence::Catalog, Evidence::Catalog(true)) => Ok(Fence::Claim),
        (Fence::Catalog, Evidence::Catalog(false)) => Err(Rejection::CatalogChanged),
        (Fence::Claim, Evidence::Claim(true)) => Ok(Fence::Policy),
        (Fence::Claim, Evidence::Claim(false)) => Err(Rejection::StaleClaim),
        (Fence::Policy, Evidence::Policy(true)) => Ok(Fence::Root),
        (Fence::Policy, Evidence::Policy(false)) => Err(Rejection::PolicyChanged),
        (Fence::Root, Evidence::Root(true)) => Ok(Fence::Ready),
        (Fence::Root, Evidence::Root(false)) => Err(Rejection::RootChanged),
        _ => Err(Rejection::OutOfOrder),
    }
}
/// Completed scopes may authorize absence only after the adapter verifies their
/// directory identity and version; other scopes retain existing observations.
pub fn scope_effect(
    fence: Fence,
    complete: bool,
    directory_verified: bool,
) -> Result<ScopeEffect, Rejection> {
    if fence != Fence::Ready {
        return Err(Rejection::OutOfOrder);
    }
    Ok(if complete && directory_verified {
        ScopeEffect::PublishMissing
    } else {
        ScopeEffect::RecordOnly
    })
}
/// Continuation must revalidate the completed parent, policy and dirty watermark
/// in order before it can reuse the parent's scope evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinuationFence {
    Parent,
    Policy,
    Dirty,
    Ready,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinuationEvidence {
    ParentFinished(bool),
    PolicyMatches(bool),
    DirtyUnchanged(bool),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinuationRejection {
    ParentActive,
    PolicyChanged,
    SourceChanged,
    OutOfOrder,
}
pub fn advance_continuation(
    fence: ContinuationFence,
    evidence: ContinuationEvidence,
) -> Result<ContinuationFence, ContinuationRejection> {
    match (fence, evidence) {
        (ContinuationFence::Parent, ContinuationEvidence::ParentFinished(true)) => {
            Ok(ContinuationFence::Policy)
        }
        (ContinuationFence::Parent, ContinuationEvidence::ParentFinished(false)) => {
            Err(ContinuationRejection::ParentActive)
        }
        (ContinuationFence::Policy, ContinuationEvidence::PolicyMatches(true)) => {
            Ok(ContinuationFence::Dirty)
        }
        (ContinuationFence::Policy, ContinuationEvidence::PolicyMatches(false)) => {
            Err(ContinuationRejection::PolicyChanged)
        }
        (ContinuationFence::Dirty, ContinuationEvidence::DirtyUnchanged(true)) => {
            Ok(ContinuationFence::Ready)
        }
        (ContinuationFence::Dirty, ContinuationEvidence::DirtyUnchanged(false)) => {
            Err(ContinuationRejection::SourceChanged)
        }
        _ => Err(ContinuationRejection::OutOfOrder),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_scope_can_publish_before_all_evidence_or_with_incomplete_directory() {
        for fence in [Fence::Catalog, Fence::Claim, Fence::Policy, Fence::Root] {
            assert_eq!(scope_effect(fence, true, true), Err(Rejection::OutOfOrder));
        }
        assert_eq!(
            scope_effect(Fence::Ready, false, true),
            Ok(ScopeEffect::RecordOnly)
        );
        assert_eq!(
            scope_effect(Fence::Ready, true, false),
            Ok(ScopeEffect::RecordOnly)
        );
        assert_eq!(
            scope_effect(Fence::Ready, true, true),
            Ok(ScopeEffect::PublishMissing)
        );
        assert_eq!(
            advance(Fence::Claim, Evidence::Root(true)),
            Err(Rejection::OutOfOrder)
        );
        assert_eq!(
            advance(Fence::Claim, Evidence::Claim(false)),
            Err(Rejection::StaleClaim)
        );
        assert_eq!(
            advance(Fence::Policy, Evidence::Policy(false)),
            Err(Rejection::PolicyChanged)
        );
        assert_eq!(
            advance(Fence::Root, Evidence::Root(false)),
            Err(Rejection::RootChanged)
        );
    }
    #[test]
    fn continuation_requires_ordered_parent_policy_and_dirty_evidence() {
        use ContinuationEvidence as E;
        use ContinuationFence as F;
        use ContinuationRejection as R;
        assert_eq!(
            advance_continuation(F::Parent, E::ParentFinished(false)),
            Err(R::ParentActive)
        );
        assert_eq!(
            advance_continuation(F::Policy, E::PolicyMatches(false)),
            Err(R::PolicyChanged)
        );
        assert_eq!(
            advance_continuation(F::Dirty, E::DirtyUnchanged(false)),
            Err(R::SourceChanged)
        );
        assert_eq!(
            advance_continuation(F::Parent, E::DirtyUnchanged(true)),
            Err(R::OutOfOrder)
        );
        let policy = advance_continuation(F::Parent, E::ParentFinished(true)).unwrap();
        let dirty = advance_continuation(policy, E::PolicyMatches(true)).unwrap();
        assert_eq!(
            advance_continuation(dirty, E::DirtyUnchanged(true)),
            Ok(F::Ready)
        );
    }
}
