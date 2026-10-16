// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Private no-child execution ownership. An operation result is not a witness.
use catabolic_store::{Error, Result, Store, media_mutation::Claim};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

thread_local! { static NO_CHILDREN: std::cell::Cell<bool> = const {std::cell::Cell::new(false)}; }
struct NoChildren(bool);
impl NoChildren {
    fn enter() -> Self {
        Self(NO_CHILDREN.with(|current| current.replace(true)))
    }
}
impl Drop for NoChildren {
    fn drop(&mut self) {
        NO_CHILDREN.with(|current| current.set(self.0));
    }
}
/// The existing process owner consults this before any child spawn. It never
/// turns an operation result into cleanup proof or adopts an existing process.
pub(crate) fn no_child_scope_active() -> bool {
    NO_CHILDREN.with(std::cell::Cell::get)
}

/// Only this closed operation set executes without an external process path.
#[derive(Clone, Copy)]
enum Builtin {
    Sniff,
    Hash,
    Verify,
}
impl Builtin {
    fn parse(job: &Value) -> Option<Self> {
        match job["operation"].as_str()? {
            "sniff" => Some(Self::Sniff),
            "hash" => Some(Self::Hash),
            "verify" => Some(Self::Verify),
            _ => None,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Sniff => "sniff",
            Self::Hash => "hash",
            Self::Verify => "verify",
        }
    }
}
struct Identity {
    catalog_id: String,
    incarnation: String,
    owner: String,
    claim: Claim,
    operation: Builtin,
}
/// The work plan moves into exactly the worker thread that executes it. No
/// public constructor, callback, PID or caller-supplied terminal flag can seal it.
pub(crate) struct OwnedBuiltin(Identity);
pub(crate) struct CompletedBuiltin(Identity);
pub(crate) struct ReleasedBuiltin(Identity);
impl OwnedBuiltin {
    pub(crate) fn capture(store: &Store, claim: &Claim, job: &Value) -> Result<Option<Self>> {
        let Some(operation) = Builtin::parse(job) else {
            return Ok(None);
        };
        if job["id"] != claim.job_id
            || job["snapshot"] != catabolic_core::decode_json(&claim.snapshot)?
            || job["options"] != catabolic_core::decode_json(&claim.options)?
        {
            return Err(Error(
                "native builtin cleanup inputs do not match the claim".into(),
            ));
        }
        // Linux source UUID validation uses store::source's direct findmnt
        // child, outside runtime::process. That path has no owned retirement
        // proof and must never receive this no-child capability. Null UUID or
        // an explicit UUID skip is the closed source-validation cohort.
        if cfg!(target_os = "linux")
            && job["snapshot"]["source_policy"]["settings"]["uuid"] != "skip"
            && !job["snapshot"]["volume_uuid"].is_null()
        {
            return Ok(None);
        }
        let Some(incarnation) = store.catalog_incarnation.as_ref() else {
            return Ok(None);
        };
        store.validate_native_identity()?;
        if store.schema_version < catabolic_store::media_mutation::NATIVE_CLEANUP_SCHEMA_VERSION {
            return Ok(None);
        }
        let owner:Option<String>=store.db.query_row("SELECT runtime_owner_nonce FROM native_execution_lifetimes WHERE catalog_id=? AND incarnation=? AND job_id=? AND generation=? AND attempt=? AND claim_token_digest=? AND snapshot_digest=? AND options_digest=? AND state='owned' AND protocol_version=1 AND child_obligations=0 AND verified_retirements=0 AND capacity_released=0",params![store.database_id,incarnation,claim.job_id,claim.generation,claim.attempt,catabolic_core::sha256(&claim.token),catabolic_core::sha256(&claim.snapshot),catabolic_core::sha256(&claim.options)],|row|row.get(0)).optional()?;
        let owner = owner
            .ok_or_else(|| Error("native builtin cleanup owner is missing or stale".into()))?;
        Ok(Some(Self(Identity {
            catalog_id: store.database_id.clone(),
            incarnation: incarnation.clone(),
            owner,
            claim: claim.clone(),
            operation,
        })))
    }
    /// Only Pool's definite Builder::spawn error consumes the retained plan
    /// here. No worker ran, no capacity was reserved, and no child was launched.
    pub(crate) fn without_worker(self) -> CompletedBuiltin {
        CompletedBuiltin(self.0)
    }
    pub(crate) fn execute(
        self,
        job: &Value,
        cancel: &std::sync::atomic::AtomicBool,
        cached: Option<Value>,
    ) -> (Value, CompletedBuiltin) {
        // job is the exact immutable Value moved into this owned worker. The
        // assertion fails closed if an internal caller violates the closed plan.
        assert_eq!(job["operation"], self.0.operation.name());
        let _no_children = NoChildren::enter();
        let result = cached.unwrap_or_else(|| crate::media::process(job, cancel));
        (result, CompletedBuiltin(self.0))
    }
}
impl CompletedBuiltin {
    /// Called only by the private Pool owner after joining the exact worker,
    /// removing Pending and releasing that worker's capacity buckets.
    pub(crate) fn release_capacity(self) -> ReleasedBuiltin {
        ReleasedBuiltin(self.0)
    }
}
impl ReleasedBuiltin {
    pub(crate) fn seal(self, store: &Store) -> Result<()> {
        let Identity {
            catalog_id,
            incarnation,
            owner,
            claim,
            operation,
        } = self.0;
        store.validate_native_identity()?;
        if store.database_id != catalog_id
            || store.catalog_incarnation.as_deref() != Some(&incarnation)
        {
            return Err(Error("native cleanup catalog lifetime changed".into()));
        }
        store.transaction(|db| {
            let exact:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM execution_claims c JOIN processing_jobs j ON j.id=c.job_id WHERE c.job_id=? AND c.generation=? AND c.attempt=? AND c.token=? AND c.snapshot=? AND c.options=? AND j.operation=?)",params![claim.job_id,claim.generation,claim.attempt,claim.token,claim.snapshot,claim.options,operation.name()],|row|row.get(0))?;
            if !exact {return Err(Error("native cleanup execution claim changed".into()));}
            let changed=db.execute("UPDATE native_execution_lifetimes SET state='quiescent',cleanup_kind='builtin_no_child_v1',capacity_released=1,sealed_at=CURRENT_TIMESTAMP WHERE catalog_id=? AND incarnation=? AND job_id=? AND generation=? AND attempt=? AND claim_token_digest=? AND snapshot_digest=? AND options_digest=? AND runtime_owner_nonce=? AND protocol_version=1 AND state='owned'",params![catalog_id,incarnation,claim.job_id,claim.generation,claim.attempt,catabolic_core::sha256(&claim.token),catabolic_core::sha256(&claim.snapshot),catabolic_core::sha256(&claim.options),owner])?;
            if changed!=1 {return Err(Error("native cleanup owner changed or already sealed".into()));}
            db.execute("UPDATE execution_claims SET lease_until=0 WHERE job_id=? AND generation=? AND attempt=? AND token=?",params![claim.job_id,claim.generation,claim.attempt,claim.token])?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use catabolic_store::{media_mutation, migration};
    use serde_json::json;
    use std::sync::atomic::AtomicBool;
    fn fixture() -> (tempfile::TempDir, Store, CompletedBuiltin) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog");
        migration::initialize_native_at(&path, 36).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        store.transaction(|db| {db.execute_batch("INSERT INTO locations VALUES('source');INSERT INTO files VALUES('file','source','file.bin');INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key) VALUES('job','default','file','hash','source','{}','{}','key');")?;Ok(())}).unwrap();
        let claim = media_mutation::start(&store, "default", "job", 1.0).unwrap();
        let job = media_mutation::get_job(&store, "default", "job").unwrap();
        let owner = OwnedBuiltin::capture(&store, &claim, &job)
            .unwrap()
            .unwrap();
        let (_, done) =
            std::thread::spawn(move || owner.execute(&job, &AtomicBool::new(false), None))
                .join()
                .unwrap();
        (root, store, done)
    }
    #[test]
    fn cleanup_sql_failure_rolls_back_witness_generation_and_claim_release() {
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
        let (_root, store, done) = fixture();
        let before = migration::snapshot(&store.db).unwrap();
        store
            .db
            .authorizer(Some(|context: AuthContext<'_>| match context.action {
                AuthAction::Update {
                    table_name: "native_execution_lifetimes",
                    ..
                } => Authorization::Deny,
                _ => Authorization::Allow,
            }))
            .unwrap();
        assert!(done.release_capacity().seal(&store).is_err());
        store
            .db
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
            .unwrap();
        assert_eq!(migration::snapshot(&store.db).unwrap(), before);
        assert_eq!(
            migration::inspect_native(&store.path, false).unwrap()["pending_work"]["native_execution_lifetimes"],
            1
        );
        assert_eq!(
            media_mutation::prepare_run_admitted(&store, "default", None, 2.0, 0, 1)
                .unwrap_err()
                .code,
            media_mutation::AdmissionErrorCode::BusyWorker
        );
    }
    #[test]
    fn wrong_private_nonce_incarnation_or_claim_cannot_seal() {
        for field in [
            "nonce",
            "incarnation",
            "token",
            "snapshot",
            "options",
            "generation",
            "attempt",
            "job_id",
        ] {
            let (_root, store, mut done) = fixture();
            let before = migration::snapshot(&store.db).unwrap();
            match field {
                "nonce" => done.0.owner = "f".repeat(32),
                "incarnation" => done.0.incarnation = "f".repeat(32),
                "token" => done.0.claim.token = "wrong".into(),
                "snapshot" => done.0.claim.snapshot = "[]".into(),
                "options" => done.0.claim.options = "[]".into(),
                "generation" => done.0.claim.generation += 1,
                "attempt" => done.0.claim.attempt += 1,
                "job_id" => done.0.claim.job_id = "other-job".into(),
                _ => unreachable!(),
            }
            assert!(done.release_capacity().seal(&store).is_err());
            assert_eq!(migration::snapshot(&store.db).unwrap(), before);
        }
    }
    #[test]
    fn crash_boundaries_before_the_sql_seal_leave_no_retry_authority() {
        for release_capacity in [false, true] {
            let (_root, store, done) = fixture();
            if release_capacity {
                drop(done.release_capacity());
            } else {
                drop(done);
            }
            assert_eq!(
                store
                    .db
                    .query_row(
                        "SELECT count(*) FROM native_execution_lifetimes WHERE state='quiescent'",
                        [],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
            assert_eq!(
                media_mutation::prepare_run_admitted(&store, "default", None, 2.0, 0, 1)
                    .unwrap_err()
                    .code,
                media_mutation::AdmissionErrorCode::BusyWorker
            );
        }
    }

    #[test]
    fn no_child_scope_refuses_actual_process_launch_and_restores_prior_runner() {
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("must-not-exist");
        {
            let _guard = NoChildren::enter();
            assert!(
                crate::process::command_output(
                    &[
                        "/bin/sh",
                        "-c",
                        &format!("printf bad > '{}'", marker.display())
                    ],
                    Default::default()
                )
                .is_err()
            );
        }
        assert!(!marker.exists());
        // Use the same portable shell as the refused launch above; true is not
        // installed at /bin/true on macOS.
        let restored =
            crate::process::command_output(&["/bin/sh", "-c", "exit 0"], Default::default())
                .unwrap();
        assert_eq!(restored.returncode, 0);
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn uuid_checking_source_cannot_obtain_no_child_cleanup_capability() {
        let (_root, store, done) = fixture();
        for uuid in [json!("enrolled-volume"), json!(42)] {
            let mut claim = done.0.claim.clone();
            let mut job = media_mutation::get_job(&store, "default", &claim.job_id).unwrap();
            job["snapshot"]["volume_uuid"] = uuid;
            claim.snapshot = catabolic_core::encode(&job["snapshot"]);
            assert!(
                OwnedBuiltin::capture(&store, &claim, &job)
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(
            store
                .db
                .query_row("SELECT state FROM native_execution_lifetimes", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
            "owned"
        );
    }

    #[test]
    fn external_operation_cannot_obtain_builtin_cleanup_capability() {
        let (_root, store, done) = fixture();
        let mut job = media_mutation::get_job(&store, "default", "job").unwrap();
        job["operation"] = json!("probe");
        assert!(
            OwnedBuiltin::capture(&store, &done.0.claim, &job)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .db
                .query_row("SELECT state FROM native_execution_lifetimes", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
            "owned"
        );
    }
}
