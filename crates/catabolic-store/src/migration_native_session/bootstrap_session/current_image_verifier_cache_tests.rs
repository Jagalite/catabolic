//! Genuine qualified capture of the retained original owner. Retirement means
//! release of actual verifier cache pages, never a replacement buffer, an
//! accounting discount, or permission to omit the final original-source scan.
use super::*;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::Ordering;

fn run_retirement_capture(
    operation: &mut CurrentImageCaptureOperation,
    backing: &mut allocation::OfflineControlWorkspaceBacking,
    cancel: &AtomicBool,
    fault: u8,
) -> OfflineRetainedResult<()> {
    let CurrentImageCaptureWorkspace { result, loan } = operation.borrow_control_workspace(backing).unwrap();
    let control = BootstrapWorkspace::from_capture_loan(&operation.owner.session.store, loan);
    let outcome = CurrentImageCaptureOperation::capture_parts(&operation.owner,
        &operation.admission, &mut operation.state, cancel, fault, Some(&control));
    result.finish(outcome)
}

fn source_settings(db: &Connection) -> (i64, i64, i64, i64, i64, i64, String) {
    (
        db.query_row("PRAGMA main.cache_size", [], |row| row.get(0)).unwrap(),
        db.query_row("PRAGMA foreign_keys", [], |row| row.get(0)).unwrap(),
        db.query_row("PRAGMA recursive_triggers", [], |row| row.get(0)).unwrap(),
        db.query_row("PRAGMA read_uncommitted", [], |row| row.get(0)).unwrap(),
        db.query_row("PRAGMA main.schema_version", [], |row| row.get(0)).unwrap(),
        db.query_row("PRAGMA main.user_version", [], |row| row.get(0)).unwrap(),
        db.query_row("PRAGMA main.journal_mode", [], |row| row.get(0)).unwrap(),
    )
}

fn assert_original_available(store: &Store, source: usize) {
    assert_eq!(unsafe { store.db.handle() } as usize, source);
    assert!(store.owns_writer_lock());
    assert!(crate::writer_lock(&store.path).is_err());
    assert!(!store.transaction_poisoned.get());
    assert!(store.db.is_autocommit());
    assert_eq!(store.db.transaction_state(Some("main")).unwrap(), rusqlite::TransactionState::None);
}

fn trace(context: &allocation::AllocationContext) -> Vec<allocation::CapturePhaseForTest> {
    let slots = context.capture_trace_for_test().unwrap();
    // The final handoff must not be silently lost to a filled diagnostic array.
    let phases: Vec<_> = slots.into_iter().flatten().collect();
    assert!(phases.len() < slots.len());
    phases
}

fn phase<'a>(phases: &'a [allocation::CapturePhaseForTest], name: &str) -> &'a allocation::CapturePhaseForTest {
    let mut matches = phases.iter().filter(|phase| phase.name == name);
    let found = matches.next().unwrap_or_else(|| panic!("missing genuine capture phase {name}"));
    assert!(matches.next().is_none(), "unexpected repeated phase {name}");
    found
}

fn assert_additive_phases(phases: &[allocation::CapturePhaseForTest], external: u64) {
    for phase in phases {
        assert_eq!(phase.external, external, "{} rebased external retention", phase.name);
        assert_eq!(phase.temporary, phase.native[4]);
        assert_eq!(phase.total, phase.external + phase.native.into_iter().sum::<u64>());
        assert!(phase.total <= allocation::LIMIT, "{} exceeded the unchanged cap", phase.name);
        assert_eq!(phase.metadata, 0, "qualified capture must use its original metadata loan");
        if phase.image_bytes != 0 {
            assert!(phase.copy_sqlite != 0 || phase.name == "copier_phase");
            assert_eq!(phase.requested, phase.image_bytes + phase.copy_sqlite);
        }
    }
}

fn assert_retirement_keeps_full_construction_coverage(phases: &[allocation::CapturePhaseForTest]) {
    let entered = phase(phases, "verifier_retirement_enter");
    let before = phase(phases, "verifier_retirement_prechecked");
    let after = phase(phases, "verifier_retirement_accounted");
    assert!(before.image_bytes != 0);
    assert_eq!(before.image_bytes, after.image_bytes, "the owned serialized backing must remain live");
    assert_eq!(entered.image_bytes, before.image_bytes);
    assert_eq!(entered.copy_sqlite, before.copy_sqlite);
    assert!(after.copy_sqlite < before.copy_sqlite,
        "retirement must reduce actual verifier DBSTATUS: {} -> {}", before.copy_sqlite, after.copy_sqlite);
    assert_eq!(before.requested - after.requested, before.copy_sqlite - after.copy_sqlite);
    // The production construction guard is a high-water, not the freshly
    // observed smaller requested payload. Neither it nor another category is
    // subtracted during release; the unchanged exact-output seal comes later.
    assert_eq!(entered.temporary, before.temporary);
    assert_eq!(before.temporary, after.temporary);
    for index in [0, 1, 2, 3, 4, 6] {
        assert_eq!(before.native[index], after.native[index], "retirement changed accounting category {index}");
    }
    // SQLite's public release can also evict clean pages from its shared LRU.
    // Only a freshly measured original-source DBSTATUS reduction may lower
    // this full total; construction and all retained categories stay intact.
    assert!(after.native[5] <= before.native[5]);
    assert_eq!(before.total - after.total, before.native[5] - after.native[5]);
    assert!(after.temporary >= after.requested);
}

#[test]
fn verifier_cache_retirement_preserves_full_accounting_final_source_scan_and_original_custody() {
    let (_root, owner, mut backing) = workspace_fixture();
    let mut operation = typed_operation(owner);
    operation.owner.session.store.db.pragma_update(Some("main"), "cache_size", -2000).unwrap();
    let store = &operation.owner.session.store;
    let source = unsafe { store.db.handle() } as usize;
    let path = store.path.clone();
    let original = snapshot(&store.db).unwrap();
    let file_before = std::fs::read(&path).unwrap();
    let settings = source_settings(&store.db);
    assert_eq!(settings.0, -2000);
    let expected = crate::catalog_backup::inventory::capture(&store.db,
        &AtomicBool::new(false), Instant::now() + Duration::from_secs(60)).unwrap();
    let context = store.offline_bootstrap.as_ref().unwrap().allocation.clone();
    let identity = context.preparation_state_for_test().unwrap();
    let external = context.external_retained_bytes().unwrap();
    let retained_before = context.retained_components_for_test().unwrap();
    let deadline = operation.deadline();
    let result = run_retirement_capture(&mut operation, &mut backing, &AtomicBool::new(false), 0);
    assert!(result.result().is_ok(), "{result:?}");
    assert!(matches!(operation.state, CaptureState::Captured(..)));
    assert_eq!(operation.deadline(), deadline);
    let phases = trace(&context);
    assert_additive_phases(&phases, external);
    assert_retirement_keeps_full_construction_coverage(&phases);
    let ordered = ["verifier_retirement_enter", "verifier_retirement_prechecked",
        "verifier_retirement_accounted", "copier_output_installed", "final_source_validated",
        "copier_return", "persistent_handoff"];
    let positions: Vec<_> = ordered.iter().map(|name| {
        phase(&phases, name);
        phases.iter().position(|phase| phase.name == *name).unwrap()
    }).collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    let accounted = phase(&phases, "verifier_retirement_accounted");
    let sealed = phase(&phases, "copier_output_installed");
    let final_source = phase(&phases, "final_source_validated");
    let committed = phase(&phases, "persistent_handoff");
    assert!(sealed.temporary < accounted.temporary,
        "obsolete construction coverage may retire only at the later exact-output seal");
    assert_eq!(sealed.temporary, final_source.temporary,
        "image/history coverage must survive the final full original-source scan");
    assert_eq!(&sealed.native[..4], &final_source.native[..4]);
    assert!(final_source.native[5] != 0, "final original SQLite must remain additive");
    assert!(final_source.temporary > committed.temporary);
    assert!(committed.native[3] > retained_before.1, "persistent image/history coverage must be installed");
    for name in ordered {
        let phase = phase(&phases, name);
        eprintln!("verifier_retirement_phase={} K={} total={} temporary={} image={} copy_sqlite={} requested={} native={:?}",
            phase.name, phase.external, phase.total, phase.temporary, phase.image_bytes,
            phase.copy_sqlite, phase.requested, phase.native);
    }
    assert_original_available(&operation.owner.session.store, source);
    assert_eq!(source_settings(&operation.owner.session.store.db), settings);
    assert_eq!(snapshot(&operation.owner.session.store.db).unwrap(), original);
    assert_eq!(std::fs::read(&path).unwrap(), file_before);
    assert_eq!(context.preparation_state_for_test().unwrap(), identity);
    assert_eq!(context.external_retained_bytes().unwrap(), external);
    assert_eq!(backing.state_for_test(), (external, false, false));
    let image = operation.finish().unwrap();
    assert_eq!(image.current_inventory_sha256(), expected.complete);
    assert_eq!(image.source_effects().len(), 1);
    assert_additive_retention(&image);
    assert_original_available(&image.owner.session.store, source);
    // Actual verifier/source payloads and operation slots can now drop, while
    // the independently retained result still owns its paid original output.
    assert!(context.temporary_bytes_for_test().unwrap() >= 2 * 64 * 1024);
    drop(image);
    assert!(crate::writer_lock(&path).is_ok());
    assert!(result.result().is_ok());
    assert!(context.temporary_bytes_for_test().unwrap() >= 2 * 64 * 1024);
    drop(result);
    assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
}

#[test]
fn verifier_cache_retirement_cancel_unwind_and_real_pressure_preserve_original_failure_custody() {
    for fault in 20..=24 {
        let (_root, owner, mut backing) = workspace_fixture();
        let mut operation = typed_operation(owner);
        operation.owner.session.store.db.pragma_update(Some("main"), "cache_size", -2000).unwrap();
        let store = &operation.owner.session.store;
        let source = unsafe { store.db.handle() } as usize;
        let path = store.path.clone();
        let original = snapshot(&store.db).unwrap();
        let file_before = std::fs::read(&path).unwrap();
        let settings = source_settings(&store.db);
        let context = store.offline_bootstrap.as_ref().unwrap().allocation.clone();
        let identity = context.preparation_state_for_test().unwrap();
        let external = context.external_retained_bytes().unwrap();
        let retained_before = context.retained_components_for_test().unwrap();
        let deadline = operation.deadline();
        let cancel = AtomicBool::new(false);
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            run_retirement_capture(&mut operation, &mut backing, &cancel, fault)
        }));
        if fault == 21 || fault == 23 {
            assert!(outcome.is_err(), "actual retirement unwind {fault} was not reached");
        } else {
            let result = outcome.as_ref().unwrap();
            assert!(result.result().is_err(), "retirement fault {fault} published success");
            if fault == 24 {
                assert!(result.result().as_ref().unwrap_err().to_string().contains("allocation"),
                    "pressure must fail through unchanged aggregate admission: {result:?}");
            }
        }
        assert_eq!(cancel.load(Ordering::Acquire), fault == 20 || fault == 22);
        let phases = trace(&context);
        assert_additive_phases(&phases, external);
        phase(&phases, "verifier_retirement_enter");
        if fault == 22 || fault == 23 {
            assert_retirement_keeps_full_construction_coverage(&phases);
        } else {
            assert!(!phases.iter().any(|phase| phase.name == "verifier_retirement_prechecked"));
            assert!(!phases.iter().any(|phase| phase.name == "verifier_retirement_accounted"),
                "cancel/unwind/over-limit state must refuse before verifier cache release");
        }
        assert!(!phases.iter().any(|phase| matches!(phase.name,
            "copier_output_installed" | "final_source_validated" | "copier_return" | "persistent_handoff")));
        assert!(matches!(operation.state, CaptureState::Failed));
        assert!(operation.capture_attempted());
        assert_eq!(operation.deadline(), deadline);
        assert_original_available(&operation.owner.session.store, source);
        assert_eq!(source_settings(&operation.owner.session.store.db), settings);
        assert_eq!(snapshot(&operation.owner.session.store.db).unwrap(), original);
        assert_eq!(std::fs::read(&path).unwrap(), file_before);
        assert!(operation.owner.failed_capture.borrow().is_none());
        assert_eq!(context.preparation_state_for_test().unwrap(), identity);
        assert_eq!(context.external_retained_bytes().unwrap(), external);
        assert_eq!(context.retained_components_for_test().unwrap(), retained_before);
        assert_eq!(backing.state_for_test(), (external, false, false));
        assert!(operation.borrow_control_workspace(&mut backing).is_err());
        assert!(operation.capture(&AtomicBool::new(false)).is_err());
        let owner = match operation.finish() {
            Err(failure) => failure.into_session(),
            Ok(_) => panic!("retirement fault {fault} returned an image"),
        };
        assert_original_available(&owner.session.store, source);
        owner.validate_current().unwrap();
        assert_eq!(snapshot(&owner.session.store.db).unwrap(), original);
        // In particular, the genuine pressure guard and live refusal VM from24
        // must have died before exact source-policy restoration, without poison.
        context.check(Some(&owner.session.store.db), 0).unwrap();
        drop(outcome);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 0,
            "retirement fault {fault} retained a copy, pressure, or output guard");
        drop(owner);
        assert!(crate::writer_lock(&path).is_ok());
    }
}

#[test]
fn verifier_cache_retirement_is_not_applied_to_the_ordinary_six_image_path() {
    let (_root, owner, _backing) = workspace_fixture();
    let mut operation = admitted_capture(owner, Instant::now() + Duration::from_secs(60));
    operation.owner.session.store.db.pragma_update(Some("main"), "cache_size", -2000).unwrap();
    let settings = source_settings(&operation.owner.session.store.db);
    let context = operation.owner.session.store.offline_bootstrap.as_ref().unwrap().allocation.clone();
    let source = unsafe { operation.owner.session.store.db.handle() } as usize;
    let original = snapshot(&operation.owner.session.store.db).unwrap();
    operation.capture(&AtomicBool::new(false)).unwrap();
    let phases = trace(&context);
    assert!(!phases.iter().any(|phase| phase.name.starts_with("verifier_retirement_")));
    phase(&phases, "final_source_validated");
    phase(&phases, "persistent_handoff");
    assert_original_available(&operation.owner.session.store, source);
    assert_eq!(source_settings(&operation.owner.session.store.db), settings);
    assert_eq!(snapshot(&operation.owner.session.store.db).unwrap(), original);
    let image = operation.finish().unwrap();
    assert_eq!(image.source_effects().len(), 1);
    assert_additive_retention(&image);
}
