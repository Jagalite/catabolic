//! Borrow-scoped current source facts for a fixed host withdrawal. This proof
//! does not authorize a native effect or grant a caller access to source bytes.
use super::*;
use crate::catalog_source_succession::CapturedSourceWithdrawal;

pub(crate) struct CompletedSourceWithdrawalAdmission<'a> {
    owner: &'a CompletedCatalogSession,
    profile: &'a str,
    source: &'a str,
    motion_source_id: &'a str,
}
impl CompletedSourceWithdrawalAdmission<'_> {
    pub(crate) fn check(
        &self,
        store: &Store,
        profile: &str,
        source: &str,
        motion_source_id: &str,
    ) -> Result<()> {
        if !std::ptr::eq(store, &self.owner.session.store)
            || profile != self.profile
            || source != self.source
            || motion_source_id != self.motion_source_id
        {
            return Err(Error(
                "completed source withdrawal owner/scope differs".into(),
            ));
        }
        self.owner.validate_current()
    }
}

/// Retain this borrow through the host's withdrawal COMMIT. While it lives the
/// completed session cannot execute a succession or transfer its native locks.
/// Missing filesystem roots are deliberately not a withdrawal prerequisite.
pub struct CompletedSourceWithdrawal<'a> {
    owner: &'a CompletedCatalogSession,
    facts: CapturedSourceWithdrawal,
}
impl CompletedCatalogSession {
    pub fn source_withdrawal(
        &self,
        profile: &str,
        source: &str,
        motion_source_id: &str,
    ) -> Result<CompletedSourceWithdrawal<'_>> {
        let admission = CompletedSourceWithdrawalAdmission {
            owner: self,
            profile,
            source,
            motion_source_id,
        };
        let facts = crate::catalog_source_succession::capture_completed_withdrawal(
            &self.session.store,
            &admission,
            profile,
            source,
            motion_source_id,
        )?;
        Ok(CompletedSourceWithdrawal { owner: self, facts })
    }
}
impl CompletedSourceWithdrawal<'_> {
    pub fn catalog_id(&self) -> &str {
        self.facts.catalog_id()
    }
    pub fn incarnation(&self) -> &str {
        self.facts.incarnation()
    }
    pub fn generation(&self) -> i64 {
        self.facts.generation()
    }
    pub fn profile(&self) -> &str {
        self.facts.profile()
    }
    pub fn native_source(&self) -> &str {
        self.facts.native_source()
    }
    pub fn motion_source_id(&self) -> &str {
        self.facts.motion_source_id()
    }
    pub fn binding(&self) -> &serde_json::Value {
        self.facts.binding()
    }
    pub fn policy(&self) -> &serde_json::Value {
        self.facts.policy()
    }
    pub fn retained_files(&self) -> u64 {
        self.facts.retained_files()
    }
    pub fn logical_memberships(&self) -> u64 {
        self.facts.logical_memberships()
    }
    pub fn native_ownership_unresolved(&self) -> bool {
        self.facts.native_ownership_unresolved()
    }

    /// Repeat fixed current identity, complete source and worker facts while
    /// retaining the same native owner. No generic callback or Store is exposed.
    pub fn revalidate(&self) -> Result<()> {
        let admission = CompletedSourceWithdrawalAdmission {
            owner: self.owner,
            profile: self.facts.profile(),
            source: self.facts.native_source(),
            motion_source_id: self.facts.motion_source_id(),
        };
        crate::catalog_source_succession::revalidate_completed_withdrawal(
            &self.owner.session.store,
            &admission,
            &self.facts,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn withdrawal_admission_binds_real_completed_owner_and_all_source_selectors() {
        let (_root, session, command) = super::super::tests::committed(Duration::from_secs(3600));
        let owner = session.into_completed_catalog(&command).unwrap();
        let (_other_root, session, command) =
            super::super::tests::committed(Duration::from_secs(3600));
        let other = session.into_completed_catalog(&command).unwrap();
        let admission = CompletedSourceWithdrawalAdmission {
            owner: &owner,
            profile: "profile",
            source: "source",
            motion_source_id: "motion-id",
        };
        admission
            .check(&owner.session.store, "profile", "source", "motion-id")
            .unwrap();
        owner
            .session
            .store
            .transaction_detailed::<_, Error>(|_| {
                admission.check(&owner.session.store, "profile", "source", "motion-id")?;
                for (profile, source, id) in [
                    ("other", "source", "motion-id"),
                    ("profile", "other", "motion-id"),
                    ("profile", "source", "other"),
                ] {
                    assert!(
                        admission
                            .check(&owner.session.store, profile, source, id)
                            .is_err()
                    );
                }
                assert!(
                    admission
                        .check(&other.session.store, "profile", "source", "motion-id")
                        .is_err()
                );
                Ok(())
            })
            .unwrap();
        // Completed catalog admission does not invent a source binding.
        assert!(
            owner
                .source_withdrawal("profile", "source", "motion-id")
                .is_err()
        );
    }

    #[test]
    fn withdrawal_admission_accepts_real_expired_bootstrap_without_reconstructing_result() {
        let (_root, session, command) = super::super::tests::committed(Duration::ZERO);
        assert_eq!(
            session
                .compact_receipt_for_test(&command, super::super::super::tests::HOST)
                .unwrap(),
            1
        );
        let owner = session.into_completed_catalog(&command).unwrap();
        let admission = CompletedSourceWithdrawalAdmission {
            owner: &owner,
            profile: "profile",
            source: "source",
            motion_source_id: "motion-id",
        };
        owner
            .session
            .store
            .transaction_detailed::<_, Error>(|_| {
                admission.check(&owner.session.store, "profile", "source", "motion-id")
            })
            .unwrap();
    }
    #[test]
    fn genuine_completed_library_membership_survives_missing_source_root() {
        use super::super::super::tests::{HOST, command, fixture_with_registered_library, policy};
        let (root, session) = fixture_with_registered_library();
        let payload = session.prepare(HOST, &[]).unwrap();
        let command = command(&session, payload);
        session.execute(&command, HOST, &[], policy()).unwrap();
        let owner = session.into_completed_catalog(&command).unwrap();
        let proof = owner
            .source_withdrawal("default", "native-source", "motion-source")
            .unwrap();
        assert_eq!(proof.retained_files(), 0);
        assert_eq!(proof.logical_memberships(), 1);
        assert!(!proof.native_ownership_unresolved());
        std::fs::rename(
            root.path().join("source"),
            root.path().join("source-unavailable"),
        )
        .unwrap();
        proof.revalidate().unwrap();
        // Native facts only: the joined Motion withdrawal refusal has its own fixture.
    }
}
