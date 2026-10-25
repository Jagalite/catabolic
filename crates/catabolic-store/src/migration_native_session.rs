//! Retained migration custody. This is not schema admission or a Store escape.
//! Public migration wrappers drop this owner as before; the private offline
//! bootstrap corridor can retain the exact locks and connection across commit.
use super::*;
#[path = "migration_native_session/allocation.rs"]
pub(super) mod allocation;
pub(crate) use allocation::AllocationSink as OfflineAllocationSink;
pub use allocation::{OfflineAllocationReservation, OfflineControlWorkspaceBacking};
#[path = "migration_native_session/bootstrap_session.rs"]
mod bootstrap_session;
pub(in crate::migration) use bootstrap_session::BootstrapRecovery;
pub use bootstrap_session::host_evidence::{
    HostBaselineEvidence, HostImportAudit, HostImportOrigin, HostImportPart, HostSourceEvidence,
};
pub(in crate::migration) use bootstrap_session::{
    BootstrapConversion, CatalogCompletionStorage, NativeApplyStorage,
};
pub use bootstrap_session::{
    CompletedCatalogSession, CompletedFileEndpoint, CompletedLibraryScope, CompletedOfflineImage,
    CompletedSourceWithdrawal, CurrentCatalogImage, CurrentImageCaptureFailure,
    CurrentImageCaptureOperation, CurrentImageCapturePreparationFailure, CurrentImageCaptureWorkspace,
    CurrentRestoreHostAttestation, CurrentRotationFailure, CurrentRotationOperation,
    CurrentRotationPlanFacts, CurrentRotationPlanningOperation, CurrentRotationPreparationFailure,
    OfflineTransition, RotatedCurrentCatalogImage, RotatedOfflineImage,
};
pub(crate) use bootstrap_session::{
    CompletedCurrentImageAdmission, CompletedSourceAdmission, CompletedSourcePreparationAdmission,
    CompletedSourceWithdrawalAdmission, CurrentImageProof, CurrentImageRotationAdmission,
};
pub(crate) use bootstrap_session::{BootstrapWorkspace, CurrentImageInventory};
pub use bootstrap_session::{OfflineOwnedTransition, OfflineRetainedResult, OfflineWorkspaceAdmissionRefused};
pub(crate) use bootstrap_session::{
    OfflineBootstrapComplete, OfflineBootstrapExecution, OfflineBootstrapIdentity,
    OfflineBootstrapSession, OfflineImageAllocation,
};
#[path = "migration_native_session/backup.rs"]
mod backup;
#[path = "migration_native_session/closed41.rs"]
pub(super) mod closed41;
#[path = "migration_native_session/pending.rs"]
mod pending;
#[path = "migration_native_session/preparation.rs"]
mod preparation;
pub(crate) use pending::AdmittedPending41;
pub(in crate::migration) use preparation::{NativePreparation, NativePreparedBootstrap};
#[path = "migration_native_session/preservation.rs"]
mod preservation;
#[path = "migration_native_session/transition.rs"]
mod transition;

pub(super) struct RetainedNativeUpgrade {
    // Field drop order closes SQLite before releasing writer then lifecycle.
    pub(super) db: Connection,
    pub(super) writer: crate::WriterLock,
    pub(super) admission: crate::NativeAdmissionLock,
    pub(super) path: PathBuf,
    verified_backup: Option<backup::VerifiedBackup>,
    retained_preservation: Option<preservation::SourcePreservation>,
    allocation: Option<allocation::AllocationContext>,
}
impl RetainedNativeUpgrade {
    pub(in crate::migration) fn control_workspace_loan<'a>(
        &self,
        backing: &'a mut OfflineControlWorkspaceBacking,
    ) -> Result<allocation::ControlWorkspaceLoan<'a>> {
        let context = self.allocation.as_ref().ok_or_else(|| Error("offline allocation context missing".into()))?;
        backing.loan(context, &self.db)
    }
    pub(super) fn acquire(path: &Path, quiescence: NativeUpgradeQuiescence) -> Result<Self> {
        quiescence.require()?;
        let path = database_path(path)?;
        let admission = crate::native_admission_exclusive(&path)?;
        let writer = writer_lock(&path)?;
        let db = connect(&path, true)?;
        Ok(Self {
            path,
            admission,
            writer,
            db,
            verified_backup: None,
            retained_preservation: None,
            allocation: None,
        })
    }
    pub(super) fn acquire_reserved(
        path: &Path,
        quiescence: NativeUpgradeQuiescence,
        reservation: OfflineAllocationReservation,
    ) -> Result<Self> {
        // Refuse impossible host retention BEFORE locks/open/plan allocations.
        let context = allocation::AllocationContext::new(reservation)?;
        context.check(None, allocation::MIGRATION_SCRATCH)?;
        let mut owner = Self::acquire(path, quiescence)?;
        context.bind_source(&owner.db)?;
        context.check(Some(&owner.db), allocation::MIGRATION_SCRATCH)?;
        owner.allocation = Some(context);
        Ok(owner)
    }
    fn ensure_allocation(&mut self) -> Result<allocation::AllocationContext> {
        if self.allocation.is_none() {
            self.allocation = Some(allocation::AllocationContext::default());
        }
        let context = self
            .allocation
            .as_ref()
            .expect("private allocation initialized")
            .clone();
        context.bind_source(&self.db)?;
        context.check(Some(&self.db), 0)?;
        Ok(context)
    }
    pub(super) fn refresh_external_reservation(
        &self,
        reservation: OfflineAllocationReservation,
    ) -> Result<()> {
        self.allocation
            .as_ref()
            .ok_or_else(|| Error("offline allocation context missing".into()))?
            .refresh(reservation, Some(&self.db))
    }
    pub(super) fn apply(
        self,
        expected_plan: &Value,
        parent: Option<&Path>,
        checkpoint: &Checkpoint<'_>,
        target: u32,
    ) -> Result<(Self, Value)> {
        self.apply_mode(expected_plan, parent, checkpoint, target, None)
    }
    pub(super) fn plan_closed41(mut self) -> Result<(Self, Value)> {
        let allocation = self.ensure_allocation()?;
        let _scope = allocation.enter()?;
        let _scratch = allocation.reserve(&self.db, allocation::MIGRATION_SCRATCH)?;
        self.db.execute_batch("BEGIN")?;
        let plan = closed41::Closed41Target.plan(&self.db, &self.path)?;
        allocation.retain_plan(&plan, &self.db)?;
        self.db.execute_batch("ROLLBACK")?;
        Ok((self, plan))
    }
    pub(super) fn apply_closed41(
        self,
        expected_plan: &Value,
        parent: Option<&Path>,
        checkpoint: &Checkpoint<'_>,
    ) -> Result<(Self, Value)> {
        self.apply_mode(
            expected_plan,
            parent,
            checkpoint,
            41,
            Some(closed41::Closed41Target),
        )
    }
    fn apply_mode(
        mut self,
        expected_plan: &Value,
        parent: Option<&Path>,
        checkpoint: &Checkpoint<'_>,
        target: u32,
        closed: Option<closed41::Closed41Target>,
    ) -> Result<(Self, Value)> {
        let value = self.apply_mode_borrowed(expected_plan, parent, checkpoint, target, closed, None, None)?;
        Ok((self, value))
    }
    /// The fixed public operation retains this exact owner while producer work
    /// borrows it. Legacy consuming wrappers above intentionally keep their old
    /// close-on-error behavior; only the operation owns terminal failed custody.
    pub(in crate::migration) fn apply_closed41_borrowed(
        &mut self,
        expected_plan: &Value,
        parent: Option<&Path>,
        checkpoint: &Checkpoint<'_>,
    ) -> Result<Value> {
        self.apply_closed41_with_control_workspace(expected_plan, parent, checkpoint, None)
    }
    pub(in crate::migration) fn apply_closed41_with_control_workspace(
        &mut self,
        expected_plan: &Value,
        parent: Option<&Path>,
        checkpoint: &Checkpoint<'_>,
        control: Option<&mut allocation::ControlWorkspaceLoan<'_>>,
    ) -> Result<Value> {
        self.apply_mode_borrowed(
            expected_plan, parent, checkpoint, 41, Some(closed41::Closed41Target), control, None,
        )
    }
    /// Only the fixed public controlled operation can supply this private
    /// borrowed witness to its actual output guard and preallocated fallback.
    pub(in crate::migration) fn apply_closed41_with_admitted_control_output(
        &mut self,
        expected_plan: &Value,
        parent: Option<&Path>,
        checkpoint: &Checkpoint<'_>,
        control: &mut allocation::ControlWorkspaceLoan<'_>,
        output: &super::offline_bootstrap::AdmittedControlApplyOutput<'_>,
    ) -> Result<Value> {
        output.require_owner(self)?;
        self.apply_mode_borrowed(
            expected_plan, parent, checkpoint, 41, Some(closed41::Closed41Target),
            Some(control), Some(output),
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn apply_mode_borrowed(
        &mut self,
        expected_plan: &Value,
        parent: Option<&Path>,
        checkpoint: &Checkpoint<'_>,
        target: u32,
        closed: Option<closed41::Closed41Target>,
        control: Option<&mut allocation::ControlWorkspaceLoan<'_>>,
        output: Option<&super::offline_bootstrap::AdmittedControlApplyOutput<'_>>,
    ) -> Result<Value> {
        let allocation = if closed.is_some() {
            Some(self.ensure_allocation()?)
        } else {
            None
        };
        let _allocation_scope = allocation
            .as_ref()
            .map(|context| context.enter())
            .transpose()?;
        // Declared before every surviving receipt/result: either phase guard
        // outlives those actual values on success, error, and unwind.
        let mut allocation_copies = allocation.as_ref()
            .map(|context| allocation::ApplyCopyReservation::new(context, &self.db, expected_plan))
            .transpose()?;
        let mut metadata_workspace = allocation
            .as_ref()
            .map(|context| match control {
                Some(control) => allocation::ApplyMetadataWorkspace::with_control_workspace(context, &self.db, Some(control)),
                None => allocation::ApplyMetadataWorkspace::new(context, &self.db),
            })
            .transpose()?;
        let path = &self.path;
        let _admission = &self.admission;
        // Keep the writer lease owned for the entire call and afterwards.
        let _writer = &self.writer;
        let db = &self.db;
        let mut saved: Option<(PathBuf, Value)> = None;
        // Install acquired custody directly on the borrowed original owner.
        // A later error or unwind must not drop preservation or backup locally.
        let verified_backup = &mut self.verified_backup;
        let retained_preservation = &mut self.retained_preservation;
        let mut migration_committed = false;
        let result: Result<Value> = (|| {
            db.execute_batch("BEGIN")?;
            let current = {
                let observed = if let Some(owner) = &closed {
                    owner.plan(db, path)?
                } else {
                    native_plan(&db, &path, target)?
                };
                let current = observed["from_schema"].as_u64().expect("validated schema") as u32;
                require_native_plan(&observed, expected_plan)?;
                // Full equality has just been checked under the original read
                // transaction. Keep borrowing the immutable approved owner;
                // this fresh whole-plan tree ends before backup/image work.
                current
            };
            let plan = expected_plan;
            if current == target {
                // A retained owner cannot rely on Connection drop to close this
                // read transaction before the next receipt corridor.
                db.execute_batch("ROLLBACK")?;
                return Ok(
                    json!({"database":path,"schema":current,"upgraded":false,"backup":null}),
                );
            }
            require_native_journal(&db)?;
            let database_id = {
                let mut info = validate(&db, true)?;
                info["pending_migrations"] = plan["migrations"].clone();
                let before = &plan["input"]["tables"];
                *retained_preservation = if target == 41 {
                    Some(preservation::SourcePreservation::capture(db, &plan)?)
                } else {
                    None
                };
                if let (Some(context), Some(proof)) = (&allocation, retained_preservation.as_ref()) {
                    context.retain_preservation(proof.allocated_bytes()?, db)?;
                }
                let default_parent = PathBuf::from(format!("{}.backups", path.display()));
                if closed.is_some() {
                    checkpoint("upgrade:before_backup", db, current)?;
                }
                saved = Some(backup(
                    &db,
                    &path,
                    parent.unwrap_or(&default_parent),
                    &info,
                    before,
                    target,
                    Some(&plan),
                )?);
                // Move the sole later-used projection, without cloning it.
                // Info, pending-migration copy and default path end here;
                // the table snapshot was borrowed, never separately cloned.
                info["database_id"].take()
            };
            let transition = retained_preservation.as_ref();
            db.execute_batch("ROLLBACK")?;
            checkpoint("upgrade:after_backup", &db, current)?;
            let (directory, receipt) = saved.as_mut().expect("verified backup");
            if let Some(transition) = &transition {
                let mut workspace = metadata_workspace
                    .as_mut()
                    .expect("private apply metadata workspace")
                    .loan_retained(
                        allocation.as_ref().expect("private transition allocation"),
                        db,
                        allocation_copies.as_ref().expect("private apply copies"),
                        allocation::ApplyRetainedOuter { receipt, database_id: &database_id, directory },
                    )?;
                *verified_backup = Some(
                    transition.verify_backup_with_workspace(
                        &mut workspace,
                        Path::new(
                            receipt["backup"]
                                .as_str()
                                .ok_or_else(|| Error("verified backup path missing".into()))?,
                        ),
                        receipt["sha256"]
                            .as_str()
                            .ok_or_else(|| Error("verified backup SHA missing".into()))?,
                    )?,
                );
            }
            {
                let rehearse = |rehearsal: &Connection, image: Option<&allocation::PaidImageSqliteLoan<'_>>| -> Result<()> {
                    rehearsal.execute_batch("BEGIN EXCLUSIVE")?;
                    let rehearsal_checkpoint =
                        |stage: &str, connection: &Connection, version: u32| {
                            checkpoint(stage, connection, version)?;
                            if stage == "rehearsal:before_commit" {
                                if let Some(transition) = &transition {
                                    let held = verified_backup.as_ref().ok_or_else(|| {
                                        Error("retained backup custody missing".into())
                                    })?;
                                    held.revalidate()?;
                                    transition.append_in_with_image(connection, held.sha256(), image)?;
                                }
                                if closed.is_some() {
                                    checkpoint("rehearsal:commit_ready", connection, version)?;
                                }
                            }
                            Ok(())
                        };
                    apply_target_with_image(
                        rehearsal,
                        current,
                        target,
                        "rehearsal",
                        &rehearsal_checkpoint,
                        closed.as_ref(),
                        image,
                    )
                    .map(|_| ())
                };
                if closed.is_some() {
                    // Private41 rehearsal reads only the pinned backup descriptor.
                    // This isolated writable image is not live-file admission;
                    // the retained source still requires DELETE journal custody.
                    let copies = allocation_copies.as_mut().expect("private apply copies");
                    let held = verified_backup.as_ref()
                        .ok_or_else(|| Error("retained backup custody missing".into()))?;
                    // A live control loan alone is insufficient: only the fixed
                    // operation's actual admitted output/fallback witness permits
                    // this reduced phase. Ordinary/direct callers keep full6P.
                    if output.is_some() {
                        copies.retain_image_outputs(
                            allocation::ApplyRetainedOuter { receipt, database_id: &database_id, directory },
                            held,
                        )?;
                    }
                    let image_result = (|| {
                        let mut workspace = metadata_workspace
                            .as_mut()
                            .expect("private apply metadata workspace")
                            .loan_retained(
                                allocation.as_ref().expect("private rehearsal allocation"),
                                db,
                                copies,
                                allocation::ApplyRetainedOuter { receipt, database_id: &database_id, directory },
                            )?;
                        held.with_verified_image_workspace(true, &mut workspace, |db, image| rehearse(db, Some(image)))
                    })();
                    // The image and all callback scratch have actually closed.
                    // Restore the original producer allowance BEFORE any receipt
                    // mutation, serialization, source replan or source effect.
                    let restored = copies.restore_construction();
                    // If both failed, preserve the original image error. Failed
                    // restoration retains the exact immutable survivor guard.
                    image_result.and(restored)?;
                } else {
                    // Preserve the registered migration rehearsal path unchanged.
                    let temporary = tempfile::Builder::new()
                        .prefix("rehearsal-")
                        .tempdir_in(&*directory)?;
                    let source = connect(
                        Path::new(receipt["backup"].as_str().expect("backup path")),
                        false,
                    )?;
                    let rehearsal_path = temporary.path().join("rehearsal.sqlite3");
                    copy(&source, &rehearsal_path)?;
                    let rehearsal = connect(&rehearsal_path, true)?;
                    rehearse(&rehearsal, None)?;
                }
            }
            receipt["status"] = json!("rehearsed");
            manifest(directory, receipt)?;
            checkpoint("upgrade:after_rehearsal", &db, current)?;
            require_native_journal(&db)?;
            // This is an actual SQLite EXCLUSIVE database lock. In WAL mode the same
            // SQL would not exclude readers, which is why WAL is rejected above.
            db.execute_batch("BEGIN EXCLUSIVE")?;
            require_native_journal(&db)?;
            require_native_plan(
                &if let Some(owner) = &closed {
                    owner.plan(db, path)?
                } else {
                    native_plan(&db, &path, target)?
                },
                &plan,
            )?;
            let guarded_checkpoint = |stage: &str, connection: &Connection, version: u32| {
                if let Some(context) = &allocation {
                    context.check(Some(connection), 0)?;
                }
                checkpoint(stage, connection, version)?;
                _admission.validate_path()?;
                use std::os::unix::fs::MetadataExt;
                let metadata = fs::symlink_metadata(&path)?;
                let identity = json!({"device":metadata.dev(),"inode":metadata.ino()});
                if !metadata.is_file()
                    || metadata.nlink() != 1
                    || identity != plan["input"]["file_identity"]
                {
                    return Err(Error("native catalog replaced during upgrade".into()));
                }
                if stage == "upgrade:before_commit" {
                    if let Some(transition) = &transition {
                        let held = verified_backup
                            .as_ref()
                            .ok_or_else(|| Error("retained backup custody missing".into()))?;
                        held.revalidate()?;
                        transition.append_in(connection, held.sha256())?;
                    }
                }
                if let Some(context) = &allocation {
                    context.check(Some(connection), 0)?;
                }
                if closed.is_some() && stage == "upgrade:before_commit" {
                    // Preservation verification may be expensive. The fixed
                    // apply operation rechecks its original deadline AFTER it,
                    // immediately before the driver's COMMIT, not only before.
                    checkpoint("upgrade:commit_ready", connection, version)?;
                }
                Ok(())
            };
            _admission.validate_path()?;
            let mut warnings = apply_target(
                &db,
                current,
                target,
                "upgrade",
                &guarded_checkpoint,
                closed.as_ref(),
            )?;
            migration_committed = true;
            let transition_digest = if let Some(transition) = &transition {
                _admission.validate_path()?;
                use std::os::unix::fs::MetadataExt;
                let metadata = fs::symlink_metadata(path)?;
                if !metadata.is_file()
                    || metadata.nlink() != 1
                    || json!({"device":metadata.dev(),"inode":metadata.ino()})
                        != plan["input"]["file_identity"]
                {
                    return Err(Error(
                        "postcommit native migration path identity differs".into(),
                    ));
                }
                let held = verified_backup
                    .as_ref()
                    .ok_or_else(|| Error("postcommit backup custody missing".into()))?;
                held.revalidate()?;
                Some(transition.verify_committed_in(db, held.sha256())?)
            } else {
                None
            };
            receipt["status"] = json!("committed");
            if let Err(error) = manifest(directory, receipt) {
                warnings.push(format!(
                    "upgrade committed, but backup manifest could not be updated: {error}"
                ));
            }
            let mut result = json!({"database":path,"database_id":database_id,"from_schema":current,"schema":target,"upgraded":true,"backup":receipt["backup"],"applied":plan["migrations"],"plan_fingerprint":plan["plan_fingerprint"],"warnings":warnings});
            if let Some(digest) = transition_digest {
                result["bootstrap_phase"] = json!("pending");
                result["transition_digest"] = json!(digest);
            }
            Ok(result)
        })();
        if let Err(error) = &result {
            if !db.is_autocommit() {
                let _ = db.execute_batch("ROLLBACK");
            }
            // Failure-manifest recording was always best effort. If the image
            // phase could not reacquire its original construction allowance,
            // skip that optional diagnostic BEFORE mutation/format/serialization.
            // Preserve the original error and all real custody, never claim a
            // successful migration or failure receipt that was not written.
            if allocation_copies.as_ref().is_none_or(|copies| copies.construction_is_held()) {
                if let Some((directory, receipt)) = &mut saved {
                    receipt["status"] = json!(if migration_committed {
                        "committed_unverified"
                    } else {
                        "failed"
                    });
                    receipt["error"] = json!(error.to_string());
                    let _ = manifest(directory, receipt);
                }
            }
        }
        // Errors retain the actual connection (including any unsettled
        // transaction), both leases, and every acquired preservation owner.
        // The public operation is terminal before entering this method.
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn quiescence() -> NativeUpgradeQuiescence {
        NativeUpgradeQuiescence {
            admission_stopped: true,
            readers_and_workers_stopped: true,
            exclusive_operational_ownership: true,
        }
    }
    #[test]
    fn committed_and_noop_native_upgrade_retain_connection_and_both_leases() {
        for from in [39, 40] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("catalog.sqlite3");
            initialize_native_at(&path, from).unwrap();
            let plan = plan_native_39_to_40(&path).unwrap();
            let session = RetainedNativeUpgrade::acquire(&path, quiescence()).unwrap();
            let (session, result) = session.apply(&plan, None, &|_, _, _| Ok(()), 40).unwrap();
            assert_eq!(result["schema"], 40);
            assert_eq!(result["upgraded"], from == 39);
            assert!(session.db.is_autocommit());
            session.admission.validate_path().unwrap();
            assert!(writer_lock(&path).is_err());
            assert!(crate::native_admission_exclusive(&path).is_err());
            assert_eq!(
                session
                    .db
                    .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
                    .unwrap(),
                40
            );
            drop(session);
            let writer = writer_lock(&path).unwrap();
            drop(writer);
            let admission = crate::native_admission_exclusive(&path).unwrap();
            admission.validate_path().unwrap();
        }
    }
    #[test]
    fn changed_plan_refusal_consumes_connection_before_releasing_custody() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.sqlite3");
        initialize_native_at(&path, 40).unwrap();
        let mut plan = plan_native_39_to_40(&path).unwrap();
        plan["plan_fingerprint"] = json!("wrong");
        let session = RetainedNativeUpgrade::acquire(&path, quiescence()).unwrap();
        assert!(session.apply(&plan, None, &|_, _, _| Ok(()), 40).is_err());
        let writer = writer_lock(&path).unwrap();
        drop(writer);
        let admission = crate::native_admission_exclusive(&path).unwrap();
        admission.validate_path().unwrap();
        let db = connect(&path, true).unwrap();
        validate(&db, true).unwrap();
    }
    #[test]
    fn checkpoint_unwind_consumes_connection_and_does_not_return_custody() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.sqlite3");
        initialize_native_at(&path, 39).unwrap();
        let plan = plan_native_39_to_40(&path).unwrap();
        let session = RetainedNativeUpgrade::acquire(&path, quiescence()).unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            session.apply(
                &plan,
                None,
                &|_, _, _| panic!("injected checkpoint unwind"),
                40,
            )
        }));
        assert!(outcome.is_err());
        let writer = writer_lock(&path).unwrap();
        drop(writer);
        let admission = crate::native_admission_exclusive(&path).unwrap();
        admission.validate_path().unwrap();
        let db = connect(&path, true).unwrap();
        assert_eq!(
            db.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
                .unwrap(),
            39
        );
        validate(&db, true).unwrap();
    }
}

#[cfg(test)]
#[path = "migration_native_session/closed41_tests.rs"]
mod closed41_tests;

#[cfg(test)]
pub(crate) fn assert_live_pending41_for_test(path: &Path) -> Result<Value> {
    let owner = RetainedNativeUpgrade::acquire(
        path,
        NativeUpgradeQuiescence {
            admission_stopped: true,
            readers_and_workers_stopped: true,
            exclusive_operational_ownership: true,
        },
    )?;
    let (owner, plan) = owner.plan_closed41()?;
    let (owner, _) = owner.apply_closed41(&plan, None, &|_, _, _| Ok(()))?;
    let (_, inventory) = owner.pending_inventory()?;
    Ok(inventory)
}

pub(crate) use bootstrap_session::EndpointFreshFacts;

pub(crate) use bootstrap_session::OwnedImageAllocation;

#[cfg(test)]
#[path = "migration_native_session/apply_plan_lifetime_tests.rs"]
mod apply_plan_lifetime_tests;
