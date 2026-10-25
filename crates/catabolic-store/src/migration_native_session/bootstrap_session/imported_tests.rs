//! Existing genuine original40 import/closed-scan archive, actual sealed upgrade.
//! Requires the explicitly qualified CATABOLIC_HOST_TEST_HELPER artifact.
use super::*;
use crate::catalog_nfo_attribution::{
    bootstrap_entry::{BaselineEvidence, ImportOriginEvidence},
    with_imported40_bootstrap_fixture,
};
use catabolic_core::catalog_command::CommandKey;
use std::time::Duration;
fn acquire(path: &Path) -> RetainedNativeUpgrade {
    RetainedNativeUpgrade::acquire(
        path,
        NativeUpgradeQuiescence {
            admission_stopped: true,
            readers_and_workers_stopped: true,
            exclusive_operational_ownership: true,
        },
    )
    .unwrap()
}
fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::from_secs(3600),
        max_receipts: 100,
        max_hold_records: 100,
        max_logical_bytes: 16 * 1024 * 1024,
    }
}
#[test]
fn actual_imported_identity_and_physical_origin_complete_and_recover_receipt_only() {
    with_imported40_bootstrap_fixture(|_root, original, host, evidence| {
        let path = original.path.clone();
        let BaselineEvidence::Imported {
            subject,
            metadata,
            origin: ImportOriginEvidence::Physical { audit, .. },
            ..
        } = evidence[0].baseline
        else {
            panic!("fixture must retain complete imported physical archive");
        };
        let identity = metadata.batches[0].typed.as_ref().unwrap().metadata[0].contributions[0]
            .evidence["external_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let physical = audit.physical.clone();
        let prior_sources = rows(
            &original.db,
            "SELECT * FROM catalog_metadata_sources ORDER BY item_id,namespace,record_id",
            &[],
        )
        .unwrap();
        let prior_history = rows(
            &original.db,
            "SELECT * FROM catalog_metadata_history ORDER BY item_id,namespace,record_id,revision",
            &[],
        )
        .unwrap();
        let prior_receipts = rows(&original.db, "SELECT * FROM catalog_command_receipts ORDER BY catalog_id,host_namespace,operation_id", &[]).unwrap();
        drop(original);
        let (owner, plan) = acquire(&path).plan_closed41().unwrap();
        let (owner, _) = owner
            .apply_closed41(&plan, None, &|_, _, _| Ok(()))
            .unwrap();
        let session = owner.into_bootstrap().unwrap();
        let payload = session.prepare(host, evidence).unwrap();
        let command = CommandEnvelope::new(
            1,
            CommandKey::new(session.identity().0, host, "offline-imported-bootstrap").unwrap(),
            session.identity().1,
            session.generation().unwrap(),
            &payload.to_string(),
        )
        .unwrap();
        assert!(matches!(
            session.execute(&command, host, evidence, policy()).unwrap(),
            CommandOutcome::Committed { .. }
        ));
        let complete = session.complete(&command).unwrap();
        assert_eq!(
            complete.source_count(),
            i64::try_from(evidence.len()).unwrap()
        );
        let digest = complete.state_digest().to_owned();
        let committed_generation = complete.committed_generation();
        drop(complete);
        let claim: (String, String, i64) = session
            .store
            .db
            .query_row(
                "SELECT value,item_id,state_revision FROM catalog_nfo_identity_claims",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(claim, (identity.clone(), subject.to_owned(), 1));
        let attribution: (String, String, String, i64) = session
            .store
            .db
            .query_row(
                "SELECT item_id,profile,file_id,state_revision FROM catalog_nfo_file_attributions",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            attribution,
            (
                subject.to_owned(),
                physical.profile.clone(),
                physical.exact.file_id.clone(),
                1
            )
        );
        let retained_physical: String = session.store.db.query_row("SELECT origin_physical_json FROM catalog_nfo_state_revisions WHERE item_id=? AND revision=1", [subject], |r| r.get(0)).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&retained_physical).unwrap(),
            serde_json::to_value(&physical).unwrap()
        );
        let inserted_heads: i64 = session.store.db.query_row("SELECT count(*) FROM catalog_entity_revisions WHERE entity_kind IN ('catalog_nfo_states','catalog_nfo_state_revisions','catalog_nfo_identity_claims','catalog_nfo_file_attributions','catalog_nfo_bootstrap')", [], |r| r.get(0)).unwrap();
        let inserted_rows: i64 = session.store.db.query_row("SELECT (SELECT count(*) FROM catalog_nfo_states)+(SELECT count(*) FROM catalog_nfo_state_revisions)+(SELECT count(*) FROM catalog_nfo_identity_claims)+(SELECT count(*) FROM catalog_nfo_file_attributions)+(SELECT count(*) FROM catalog_nfo_bootstrap)", [], |r| r.get(0)).unwrap();
        assert_eq!(inserted_heads, inserted_rows);
        assert_eq!(
            committed_generation,
            command.expected_generation() + inserted_rows
        );
        let check_preserved = |session: &OfflineBootstrapSession| {
            assert_eq!(
                rows(
                    &session.store.db,
                    "SELECT * FROM catalog_metadata_sources ORDER BY item_id,namespace,record_id",
                    &[]
                )
                .unwrap(),
                prior_sources
            );
            assert_eq!(rows(&session.store.db, "SELECT * FROM catalog_metadata_history ORDER BY item_id,namespace,record_id,revision", &[]).unwrap(), prior_history);
            assert_eq!(rows(&session.store.db, "SELECT * FROM catalog_command_receipts WHERE NOT(catalog_id=? AND host_namespace=? AND operation_id=?) ORDER BY catalog_id,host_namespace,operation_id", &[
                command.key().catalog_id().to_owned().into(), host.to_owned().into(), command.key().operation_id().to_owned().into()]).unwrap(), prior_receipts);
        };
        check_preserved(&session);
        let backup_path = match &session.mode {
            BootstrapMode::Pending { backup } => backup.path_for_test().to_owned(),
            _ => panic!("new migration must retain backup"),
        };
        drop(session);
        fs::rename(&backup_path, backup_path.with_extension("unavailable")).unwrap();
        let recovered = acquire(&path)
            .recover_bootstrap(
                &plan,
                BootstrapRecovery::CompletedReceiptOnly { command: &command },
            )
            .unwrap();
        assert!(recovered.prepare(host, evidence).is_err());
        assert!(matches!(
            recovered.execute(&command, host, &[], policy()).unwrap(),
            CommandOutcome::Replay { .. }
        ));
        assert_eq!(recovered.complete(&command).unwrap().state_digest(), digest);
        check_preserved(&recovered);
        assert!(recovered.store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_nfo_identity_claims c JOIN catalog_nfo_file_attributions f ON f.item_id=c.item_id WHERE c.value=? AND f.profile=? AND f.file_id=?)", rusqlite::params![identity,physical.profile,physical.exact.file_id], |r| r.get::<_,bool>(0)).unwrap());
    });
}

#[test]
fn actual_imported_physical_host_adapter_rejects_bad_descriptions_then_commits_and_replays() {
    use super::host_evidence::{
        HostBaselineEvidence, HostImportAudit, HostImportOrigin, HostImportPart, HostSourceEvidence,
    };
    with_imported40_bootstrap_fixture(|_root, original, host, evidence| {
        assert_eq!(evidence.len(), 1);
        let BaselineEvidence::Imported {
            subject,
            metadata,
            audit,
            origin:
                ImportOriginEvidence::Physical {
                    part,
                    audit: physical_audit,
                },
        } = evidence[0].baseline
        else {
            panic!("fixture must retain complete imported physical archive");
        };
        assert_eq!(
            audit.root_manifest_digest,
            physical_audit.root_manifest_digest
        );
        assert_eq!(
            audit.origin_audit_digest,
            physical_audit.origin_audit_digest
        );
        assert!(audit.no_origin_subjects.is_empty());
        // The fixture host computed both audit commitments from its complete
        // constructed archive. These are independent trusted host assertions,
        // not authenticity claims inferred from current native JSON. All roots,
        // batches, commands, and the positive mapping remain borrowed here.
        let metadata_part = HostImportPart {
            manifest: metadata.manifest,
            batches: metadata.batches,
            activation: metadata.activation,
        };
        let physical_part = HostImportPart {
            manifest: part.manifest,
            batches: part.batches,
            activation: part.activation,
        };
        let host_audit = HostImportAudit {
            root_manifest_digest: audit.root_manifest_digest.as_deref(),
            origin_audit_digest: &audit.origin_audit_digest,
            no_origin_subjects: &audit.no_origin_subjects,
        };
        let physical_origin = HostImportOrigin::Physical {
            part: physical_part,
            motion_file_id: &physical_audit.motion_file_id,
            physical: &physical_audit.physical,
        };
        let description = |audit, origin| HostSourceEvidence {
            baseline: HostBaselineEvidence::Imported {
                subject,
                metadata: metadata_part,
                audit,
                origin,
            },
            later_commands: evidence[0].later_commands,
        };
        let good = [description(host_audit, physical_origin)];
        let path = original.path.clone();
        let prior_sources = rows(
            &original.db,
            "SELECT * FROM catalog_metadata_sources ORDER BY item_id,namespace,record_id",
            &[],
        )
        .unwrap();
        let prior_history = rows(
            &original.db,
            "SELECT * FROM catalog_metadata_history ORDER BY item_id,namespace,record_id,revision",
            &[],
        )
        .unwrap();
        let prior_receipts = rows(&original.db, "SELECT * FROM catalog_command_receipts ORDER BY catalog_id,host_namespace,operation_id", &[]).unwrap();
        drop(original);
        let (owner, plan) = acquire(&path).plan_closed41().unwrap();
        let (owner, _) = owner
            .apply_closed41(&plan, None, &|_, _, _| Ok(()))
            .unwrap();
        let session = owner.into_bootstrap().unwrap();
        let before = snapshot(&session.store.db).unwrap();
        let payload = session.prepare_host(host, &good).unwrap();
        assert_eq!(snapshot(&session.store.db).unwrap(), before);
        let command = CommandEnvelope::new(
            1,
            CommandKey::new(
                session.identity().0,
                host,
                "offline-imported-host-bootstrap",
            )
            .unwrap(),
            session.identity().1,
            session.generation().unwrap(),
            &payload.to_string(),
        )
        .unwrap();
        let contradictory_absence = [subject.to_owned()].into_iter().collect();
        let mut wrong_mapping = physical_audit.physical.clone();
        wrong_mapping.exact.revision = "different-archived-file-revision".into();
        // A different well-formed opaque audit hash alone is NOT natively
        // falsifiable. Test missing/malformed audit, contradictory origin cohort,
        // and exact mapping disagreement with the independently activated part.
        let bad = [
            description(
                HostImportAudit {
                    origin_audit_digest: "",
                    ..host_audit
                },
                physical_origin,
            ),
            description(
                HostImportAudit {
                    origin_audit_digest: "not-a-digest",
                    ..host_audit
                },
                physical_origin,
            ),
            description(
                HostImportAudit {
                    no_origin_subjects: &contradictory_absence,
                    ..host_audit
                },
                physical_origin,
            ),
            description(
                host_audit,
                HostImportOrigin::Physical {
                    part: physical_part,
                    motion_file_id: &physical_audit.motion_file_id,
                    physical: &wrong_mapping,
                },
            ),
        ];
        for (case, bad) in bad.into_iter().enumerate() {
            let bad = [bad];
            assert!(
                session.prepare_host(host, &bad).is_err(),
                "prepare case {case}"
            );
            assert_eq!(
                snapshot(&session.store.db).unwrap(),
                before,
                "prepare case {case}"
            );
            assert!(
                session
                    .execute_host(&command, host, &bad, policy())
                    .is_err(),
                "execute case {case}"
            );
            assert_eq!(
                snapshot(&session.store.db).unwrap(),
                before,
                "execute case {case}"
            );
            assert!(session.reconcile(&command, host).unwrap().is_none());
        }
        assert!(matches!(
            session
                .execute_host(&command, host, &good, policy())
                .unwrap(),
            CommandOutcome::Committed { .. }
        ));
        let complete = session.complete(&command).unwrap();
        assert_eq!(complete.source_count(), 1);
        assert_eq!(
            complete.inventory_digest(),
            command.payload()["inventory_digest"].as_str().unwrap()
        );
        let digest = complete.state_digest().to_owned();
        let generation = complete.committed_generation();
        drop(complete);
        let claim: (String, String, i64) = session
            .store
            .db
            .query_row(
                "SELECT value,item_id,state_revision FROM catalog_nfo_identity_claims",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(claim, ("tmdb:42".into(), subject.to_owned(), 1));
        let attribution: (String, String, String, i64) = session
            .store
            .db
            .query_row(
                "SELECT item_id,profile,file_id,state_revision FROM catalog_nfo_file_attributions",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            attribution,
            (
                subject.to_owned(),
                physical_audit.physical.profile.clone(),
                physical_audit.physical.exact.file_id.clone(),
                1
            )
        );
        let retained: String = session.store.db.query_row("SELECT origin_physical_json FROM catalog_nfo_state_revisions WHERE item_id=? AND revision=1", [subject], |r| r.get(0)).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&retained).unwrap(),
            serde_json::to_value(&physical_audit.physical).unwrap()
        );
        assert_eq!(
            rows(
                &session.store.db,
                "SELECT * FROM catalog_metadata_sources ORDER BY item_id,namespace,record_id",
                &[]
            )
            .unwrap(),
            prior_sources
        );
        assert_eq!(rows(&session.store.db, "SELECT * FROM catalog_metadata_history ORDER BY item_id,namespace,record_id,revision", &[]).unwrap(), prior_history);
        assert_eq!(rows(&session.store.db, "SELECT * FROM catalog_command_receipts WHERE NOT(catalog_id=? AND host_namespace=? AND operation_id=?) ORDER BY catalog_id,host_namespace,operation_id", &[
            command.key().catalog_id().to_owned().into(), host.to_owned().into(), command.key().operation_id().to_owned().into()
        ]).unwrap(), prior_receipts);
        let after = snapshot(&session.store.db).unwrap();
        // Empty evidence cannot reconstruct this nonempty inventory. Replay must
        // resolve the committed receipt before entering the host adapter.
        assert!(matches!(
            session.execute_host(&command, host, &[], policy()).unwrap(),
            CommandOutcome::Replay { .. }
        ));
        assert_eq!(session.complete(&command).unwrap().state_digest(), digest);
        assert_eq!(session.generation().unwrap(), generation);
        assert_eq!(snapshot(&session.store.db).unwrap(), after);
    });
}
