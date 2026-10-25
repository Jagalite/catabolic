//! Scoped policy probes use the genuine original40 -> completed41 fixture.
//! No replacement Store, fabricated source admission, lowered accounting cap,
//! or synthetic close/SQL success is used here.
use super::*;
use control_workspace::SourceCachePolicy;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}};

fn cache_size(db: &Connection) -> i64 {
    db.query_row("PRAGMA main.cache_size", [], |row| row.get(0)).unwrap()
}
fn set_cache_size(db: &Connection, value: i32) {
    db.pragma_update(Some("main"), "cache_size", value).unwrap();
    assert_eq!(cache_size(db), i64::from(value));
}
fn transaction_none(db: &Connection) -> bool {
    db.is_autocommit()
        && db.transaction_state(Some("main")).unwrap() == rusqlite::TransactionState::None
}
fn install_cache_log(db: &Connection) -> Arc<Mutex<Vec<Option<String>>>> {
    let log = Arc::new(Mutex::new(Vec::new()));
    let observed = log.clone();
    db.authorizer(Some(move |context: AuthContext<'_>| {
        if let AuthAction::Pragma { pragma_name: "cache_size", pragma_value } = context.action {
            observed.lock().unwrap().push(pragma_value.map(str::to_owned));
        }
        Authorization::Allow
    })).unwrap();
    log
}
fn writes(log: &Arc<Mutex<Vec<Option<String>>>>) -> Vec<String> {
    log.lock().unwrap().iter().filter_map(Clone::clone).collect()
}
fn capture_fault(operation: &mut CurrentImageCaptureOperation,
    backing: &mut allocation::OfflineControlWorkspaceBacking, cancel: &AtomicBool, fault: u8,
) -> OfflineRetainedResult<()> {
    let CurrentImageCaptureWorkspace { result, loan } = operation.borrow_control_workspace(backing).unwrap();
    let control = BootstrapWorkspace::from_capture_loan(&operation.owner.session.store, loan);
    let outcome = CurrentImageCaptureOperation::capture_parts(&operation.owner,
        &operation.admission, &mut operation.state, cancel, fault, Some(&control));
    result.finish(outcome)
}

#[test]
fn source_cache_policy_exact_signed_scalar_and_smaller_targets_are_preserved() {
    let (_root, owner, mut backing) = workspace_fixture();
    let mut operation = typed_operation(owner);
    let db = &operation.owner.session.store.db;
    let page_size: i64 = db.query_row("PRAGMA main.page_size", [], |row| row.get(0)).unwrap();
    assert!(page_size > 0 && 1048576 % page_size == 0);
    let boundary = i32::try_from(1048576 / page_size).unwrap();
    let before = snapshot(db).unwrap();
    let file_before = std::fs::read(&operation.owner.session.store.path).unwrap();
    let source = unsafe { db.handle() } as usize;
    let context = operation.owner.session.store.offline_bootstrap.as_ref().unwrap().allocation.clone();
    let identity = context.preparation_state_for_test().unwrap();
    let external = operation.owner.external_retained_reservation().unwrap();
    let CurrentImageCaptureWorkspace { result, loan } = operation.borrow_control_workspace(&mut backing).unwrap();
    let store = &operation.owner.session.store;
    let control = BootstrapWorkspace::from_capture_loan(store, loan);
    for old in [0, 1, boundary, boundary + 1, -1, -512, -1024, -8192, i32::MIN, i32::MAX] {
        set_cache_size(&store.db, old);
        let log = install_cache_log(&store.db);
        let should_lower = if old < 0 { -i64::from(old) > 1024 }
            else { i64::from(old) * page_size > 1048576 };
        let mut policy = SourceCachePolicy::begin(store, &operation.admission._slots, &control).unwrap();
        assert_eq!(policy.is_some(), should_lower, "old target {old}");
        assert_eq!(cache_size(&store.db), if should_lower { -1024 } else { i64::from(old) });
        if let Some(policy) = policy.as_mut() {
            assert_eq!(policy.restoration_attempts_for_test(), 0);
            policy.finish().unwrap();
            assert_eq!(policy.restoration_attempts_for_test(), 1);
            let calls = log.lock().unwrap().len();
            assert!(policy.finish().is_err(), "restoration obligation is one-shot");
            assert_eq!(policy.restoration_attempts_for_test(), 1);
            assert_eq!(log.lock().unwrap().len(), calls, "second finish must run no SQL");
        }
        let calls = log.lock().unwrap().len();
        drop(policy);
        assert_eq!(log.lock().unwrap().len(), calls, "successful explicit restore must not retry in Drop");
        assert_eq!(cache_size(&store.db), i64::from(old));
        assert_eq!(writes(&log), if should_lower { vec!["-1024".to_owned(), old.to_string()] } else { vec![] });
        assert!(transaction_none(&store.db));
        assert!(!store.transaction_poisoned.get());
        assert!(store.owns_writer_lock());
        assert_eq!(unsafe { store.db.handle() } as usize, source);
        assert_eq!(context.preparation_state_for_test().unwrap(), identity);
        assert_eq!(context.external_retained_bytes().unwrap(), external);
        assert_eq!(snapshot(&store.db).unwrap(), before);
        assert_eq!(std::fs::read(&store.path).unwrap(), file_before);
        context.check(Some(&store.db), 0).unwrap();
    }
    drop(control);
    drop(result);
    assert_eq!(backing.state_for_test(), (external, false, false));
}

#[test]
fn source_cache_policy_qualified_success_restores_before_publishing_original_history() {
    for old in [4096, -8192, 0, -512] {
        let (_root, owner, mut backing) = workspace_fixture();
        let mut operation = typed_operation(owner);
        set_cache_size(&operation.owner.session.store.db, old);
        let source = unsafe { operation.owner.session.store.db.handle() } as usize;
        let before = snapshot(&operation.owner.session.store.db).unwrap();
        let file_before = std::fs::read(&operation.owner.session.store.path).unwrap();
        let deadline = operation.deadline();
        let context = operation.owner.session.store.offline_bootstrap.as_ref().unwrap().allocation.clone();
        let identity = context.preparation_state_for_test().unwrap();
        let callbacks = Arc::new(AtomicUsize::new(0));
        let observed = callbacks.clone();
        operation.owner.session.store.db.progress_handler(1, Some(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            false
        })).unwrap();
        let result = capture_fault(&mut operation, &mut backing, &AtomicBool::new(false), 0);
        assert!(result.result().is_ok(), "{result:?}");
        assert!(matches!(operation.state, CaptureState::Captured(..)));
        assert_eq!(operation.deadline(), deadline);
        assert_eq!(cache_size(&operation.owner.session.store.db), i64::from(old));
        let before_poll = callbacks.load(Ordering::SeqCst);
        operation.owner.session.store.db.query_row("SELECT 1", [], |_| Ok(())).unwrap();
        assert!(callbacks.load(Ordering::SeqCst) > before_poll, "source progress handler was replaced or removed");
        // The existing full validator installs its own closed41 authorizer.
        // Policy hook preservation is tested separately without that validator.
        let image = operation.finish().unwrap();
        assert_eq!(image.source_effects().len(), 1);
        assert_additive_retention(&image);
        assert_eq!(unsafe { image.owner.session.store.db.handle() } as usize, source);
        assert_eq!(context.preparation_state_for_test().unwrap(), identity);
        assert_eq!(snapshot(&image.owner.session.store.db).unwrap(), before);
        assert_eq!(std::fs::read(&image.owner.session.store.path).unwrap(), file_before);
        assert!(image.owner.session.store.owns_writer_lock());
        assert!(transaction_none(&image.owner.session.store.db));
        assert!(!image.owner.session.store.transaction_poisoned.get());
        assert!(result.result().is_ok());
    }
}

#[test]
fn source_cache_policy_ordinary_capture_never_changes_the_source_setting() {
    let (_root, owner, _backing) = workspace_fixture();
    let mut operation = admitted_capture(owner, Instant::now() + Duration::from_secs(60));
    set_cache_size(&operation.owner.session.store.db, -8192);
    let log = install_cache_log(&operation.owner.session.store.db);
    operation.capture(&AtomicBool::new(false)).unwrap();
    assert_eq!(cache_size(&operation.owner.session.store.db), -8192);
    assert!(writes(&log).is_empty(), "ordinary six-image capture must not opt into qualified cache policy");
    let image = operation.finish().unwrap();
    assert_eq!(image.source_effects().len(), 1);
    assert_additive_retention(&image);
}

#[test]
fn source_cache_policy_drop_and_main_unwind_restore_exactly_once() {
    for unwind in [false, true] {
        let (_root, owner, mut backing) = workspace_fixture();
        let mut operation = typed_operation(owner);
        set_cache_size(&operation.owner.session.store.db, 4096);
        let before = snapshot(&operation.owner.session.store.db).unwrap();
        let log = install_cache_log(&operation.owner.session.store.db);
        let CurrentImageCaptureWorkspace { result, loan } = operation.borrow_control_workspace(&mut backing).unwrap();
        let store = &operation.owner.session.store;
        let control = BootstrapWorkspace::from_capture_loan(store, loan);
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let _policy = SourceCachePolicy::begin(store, &operation.admission._slots, &control).unwrap().unwrap();
            assert_eq!(cache_size(&store.db), -1024);
            if unwind { panic!("original scoped-cache unwind"); }
        }));
        if unwind {
            let panic = outcome.unwrap_err();
            assert_eq!(panic.downcast_ref::<&str>(), Some(&"original scoped-cache unwind"));
        } else { outcome.unwrap(); }
        assert_eq!(cache_size(&store.db), 4096);
        assert_eq!(writes(&log), ["-1024", "4096"]);
        assert!(!store.transaction_poisoned.get());
        assert!(transaction_none(&store.db));
        assert!(store.owns_writer_lock());
        assert_eq!(snapshot(&store.db).unwrap(), before);
        drop(control);
        drop(result);
        operation.owner.validate_current().unwrap();
    }
}

#[test]
fn source_cache_policy_denied_assignment_or_real_readback_poisons_without_retry() {
    for deny_readback in [false, true] {
        let (_root, owner, mut backing) = workspace_fixture();
        let mut operation = typed_operation(owner);
        set_cache_size(&operation.owner.session.store.db, -8192);
        let source = unsafe { operation.owner.session.store.db.handle() } as usize;
        let file_before = std::fs::read(&operation.owner.session.store.path).unwrap();
        let calls = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
        let observed = calls.clone();
        let restoring = Arc::new(AtomicBool::new(false));
        let seen_restore = restoring.clone();
        operation.owner.session.store.db.authorizer(Some(move |context: AuthContext<'_>| {
            if let AuthAction::Pragma { pragma_name: "cache_size", pragma_value } = context.action {
                observed.lock().unwrap().push(pragma_value.map(str::to_owned));
                if pragma_value == Some("-8192") {
                    seen_restore.store(true, Ordering::Release);
                    if !deny_readback { return Authorization::Deny; }
                }
                if deny_readback && pragma_value.is_none() && seen_restore.load(Ordering::Acquire) {
                    return Authorization::Deny;
                }
            }
            Authorization::Allow
        })).unwrap();
        let CurrentImageCaptureWorkspace { result, loan } = operation.borrow_control_workspace(&mut backing).unwrap();
        let store = &operation.owner.session.store;
        let control = BootstrapWorkspace::from_capture_loan(store, loan);
        let mut policy = SourceCachePolicy::begin(store, &operation.admission._slots, &control).unwrap().unwrap();
        assert!(policy.finish().is_err());
        assert_eq!(policy.restoration_attempts_for_test(), 1);
        assert!(store.transaction_poisoned.get());
        let expected = if deny_readback {
            vec![None, Some("-1024".into()), None, Some("-8192".into()), None]
        } else { vec![None, Some("-1024".into()), None, Some("-8192".into())] };
        assert_eq!(*calls.lock().unwrap(), expected, "denial must occur in the real restore/readback SQL");
        assert!(policy.finish().is_err());
        assert_eq!(policy.restoration_attempts_for_test(), 1);
        drop(policy);
        assert_eq!(*calls.lock().unwrap(), expected);
        // Never clear poison's authorizer to inspect or heal the source.
        assert!(store.db.query_row("SELECT 1", [], |_| Ok(())).is_err());
        assert!(store.owns_writer_lock());
        assert!(transaction_none(&store.db));
        assert_eq!(unsafe { store.db.handle() } as usize, source);
        assert_eq!(std::fs::read(&store.path).unwrap(), file_before);
        drop(control);
        drop(result);
        assert!(operation.owner.validate_current().is_err());
        assert!(operation.finish().is_err());
    }
}

#[test]
fn source_cache_policy_failed_drop_cannot_replace_the_original_unwind_or_heal_owner() {
    let (_root, owner, mut backing) = workspace_fixture();
    let mut operation = typed_operation(owner);
    set_cache_size(&operation.owner.session.store.db, -8192);
    let restored = Arc::new(AtomicUsize::new(0));
    let observed = restored.clone();
    operation.owner.session.store.db.authorizer(Some(move |context: AuthContext<'_>| {
        if let AuthAction::Pragma { pragma_name: "cache_size", pragma_value: Some("-8192") } = context.action {
            observed.fetch_add(1, Ordering::SeqCst);
            return Authorization::Deny;
        }
        Authorization::Allow
    })).unwrap();
    let path = operation.owner.session.store.path.clone();
    let file_before = std::fs::read(&path).unwrap();
    let CurrentImageCaptureWorkspace { result, loan } = operation.borrow_control_workspace(&mut backing).unwrap();
    let store = &operation.owner.session.store;
    let control = BootstrapWorkspace::from_capture_loan(store, loan);
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let _policy = SourceCachePolicy::begin(store, &operation.admission._slots, &control).unwrap().unwrap();
        panic!("retain this original panic payload");
    }));
    let panic = outcome.unwrap_err();
    assert_eq!(panic.downcast_ref::<&str>(), Some(&"retain this original panic payload"));
    assert_eq!(restored.load(Ordering::SeqCst), 1);
    assert!(store.transaction_poisoned.get());
    assert!(store.db.query_row("SELECT 1", [], |_| Ok(())).is_err());
    assert!(store.owns_writer_lock());
    assert!(crate::writer_lock(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), file_before);
    drop(control);
    drop(result);
    assert!(operation.finish().is_err());
}

#[test]
fn source_cache_policy_progress_interrupt_is_not_suppressed_for_cleanup() {
    for persistent in [false, true] {
        let (_root, owner, mut backing) = workspace_fixture();
        let mut operation = typed_operation(owner);
        set_cache_size(&operation.owner.session.store.db, -8192);
        let interrupt = Arc::new(AtomicBool::new(false));
        let armed = interrupt.clone();
        operation.owner.session.store.db.authorizer(Some(move |context: AuthContext<'_>| {
            if let AuthAction::Pragma { pragma_name: "cache_size", pragma_value: Some("-1024") } = context.action {
                armed.store(true, Ordering::Release);
            }
            Authorization::Allow
        })).unwrap();
        let polls = Arc::new(AtomicUsize::new(0));
        let observed = polls.clone();
        let cancelling = interrupt.clone();
        operation.owner.session.store.db.progress_handler(1, Some(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            if persistent { cancelling.load(Ordering::Acquire) }
            else { cancelling.swap(false, Ordering::AcqRel) }
        })).unwrap();
        let CurrentImageCaptureWorkspace { result, loan } = operation.borrow_control_workspace(&mut backing).unwrap();
        let store = &operation.owner.session.store;
        let control = BootstrapWorkspace::from_capture_loan(store, loan);
        assert!(SourceCachePolicy::begin(store, &operation.admission._slots, &control).is_err());
        assert!(polls.load(Ordering::SeqCst) > 0);
        assert_eq!(store.transaction_poisoned.get(), persistent);
        assert!(store.owns_writer_lock());
        assert!(transaction_none(&store.db));
        if !persistent {
            assert_eq!(cache_size(&store.db), -8192);
            let before = polls.load(Ordering::SeqCst);
            store.db.query_row("SELECT 1", [], |_| Ok(())).unwrap();
            assert!(polls.load(Ordering::SeqCst) > before, "policy must leave the actual progress handler installed");
        } else {
            assert!(store.db.query_row("SELECT 1", [], |_| Ok(())).is_err());
        }
        drop(control);
        drop(result);
    }
}

#[test]
fn source_cache_policy_fresh_prechange_pressure_refuses_without_rehabilitating_source() {
    let (_root, owner, mut backing) = workspace_fixture();
    let mut operation = typed_operation(owner);
    set_cache_size(&operation.owner.session.store.db, -8192);
    let log = install_cache_log(&operation.owner.session.store.db);
    let context = operation.owner.session.store.offline_bootstrap.as_ref().unwrap().allocation.clone();
    let CurrentImageCaptureWorkspace { result, loan } = operation.borrow_control_workspace(&mut backing).unwrap();
    let store = &operation.owner.session.store;
    let control = BootstrapWorkspace::from_capture_loan(store, loan);
    let external = context.external_retained_bytes().unwrap();
    let used = context.retained_native_bytes(&store.db).unwrap() + external;
    let pressure = context.reserve(&store.db, allocation::LIMIT - used).unwrap();
    // A real, still-live SQLite VM makes the fresh source measurement exceed
    // the unchanged cap; its payload is not hidden by a zeroed test scalar.
    let live_vm = store.db.prepare("SELECT 1, 2, 3, 4, 5, 6, 7, 8").unwrap();
    context.check(None, 0).unwrap();
    assert!(context.check(Some(&store.db), 0).is_err());
    assert!(SourceCachePolicy::begin(store, &operation.admission._slots, &control).is_err());
    assert!(log.lock().unwrap().is_empty(), "over-budget source must be refused before preparing policy SQL");
    assert!(!store.transaction_poisoned.get());
    assert!(store.owns_writer_lock());
    drop(live_vm);
    drop(pressure);
    context.check(Some(&store.db), 0).unwrap();
    assert_eq!(cache_size(&store.db), -8192);
    drop(control);
    drop(result);
    operation.owner.validate_current().unwrap();
}

#[test]
fn source_cache_policy_foreign_admission_and_ambient_transaction_never_mutate() {
    let (_root, owner, mut backing) = workspace_fixture();
    let (_other_root, other, _other_backing) = workspace_fixture();
    let mut operation = typed_operation(owner);
    let foreign = typed_operation(other);
    set_cache_size(&operation.owner.session.store.db, -8192);
    let log = install_cache_log(&operation.owner.session.store.db);
    let CurrentImageCaptureWorkspace { result, loan } = operation.borrow_control_workspace(&mut backing).unwrap();
    let store = &operation.owner.session.store;
    let control = BootstrapWorkspace::from_capture_loan(store, loan);
    assert!(SourceCachePolicy::begin(store, &foreign.admission._slots, &control).is_err());
    assert!(SourceCachePolicy::begin(&foreign.owner.session.store, &operation.admission._slots, &control).is_err());
    store.db.execute_batch("BEGIN").unwrap();
    assert!(SourceCachePolicy::begin(store, &operation.admission._slots, &control).is_err());
    store.db.execute_batch("ROLLBACK").unwrap();
    assert!(writes(&log).is_empty());
    assert_eq!(cache_size(&store.db), -8192);
    assert!(!store.transaction_poisoned.get());
    assert!(transaction_none(&store.db));
    drop(control);
    drop(result);
}

#[test]
fn source_cache_policy_capture_cancel_and_unwind_matrix_keeps_original_custody() {
    // Entry, proof/copy returns, serialize/real-close/attach, after policy
    // installation, and after the unchanged outer availability check.
    for fault in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18] {
        let (_root, owner, mut backing) = workspace_fixture();
        let mut operation = typed_operation(owner);
        set_cache_size(&operation.owner.session.store.db, -8192);
        let source = unsafe { operation.owner.session.store.db.handle() } as usize;
        let path = operation.owner.session.store.path.clone();
        let before = snapshot(&operation.owner.session.store.db).unwrap();
        let file_before = std::fs::read(&path).unwrap();
        let context = operation.owner.session.store.offline_bootstrap.as_ref().unwrap().allocation.clone();
        let identity = context.preparation_state_for_test().unwrap();
        let deadline = operation.deadline();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            capture_fault(&mut operation, &mut backing, &AtomicBool::new(false), fault)
        }));
        if [1, 2, 4, 6, 8, 10, 12, 15, 17, 18].contains(&fault) { assert!(outcome.is_err(), "fault {fault}"); }
        else { assert!(outcome.as_ref().unwrap().result().is_err(), "fault {fault}"); }
        assert!(matches!(operation.state, CaptureState::Failed));
        assert_eq!(operation.deadline(), deadline);
        assert_eq!(unsafe { operation.owner.session.store.db.handle() } as usize, source);
        assert_eq!(context.preparation_state_for_test().unwrap(), identity);
        assert_eq!(std::fs::read(&path).unwrap(), file_before);
        assert!(operation.owner.session.store.owns_writer_lock());
        assert!(crate::writer_lock(&path).is_err());
        assert!(transaction_none(&operation.owner.session.store.db));
        assert!(!backing.state_for_test().1 && !backing.state_for_test().2);
        let poisoned = fault == 11 || fault == 17;
        assert_eq!(operation.owner.session.store.transaction_poisoned.get(), poisoned, "fault {fault}");
        if !poisoned {
            assert_eq!(cache_size(&operation.owner.session.store.db), -8192, "fault {fault}");
            assert_eq!(snapshot(&operation.owner.session.store.db).unwrap(), before);
        } else {
            assert!(operation.owner.session.store.db.query_row("SELECT 1", [], |_| Ok(())).is_err());
        }
        if fault == 11 {
            assert!(operation.owner.failed_capture.borrow().as_ref().unwrap().retained_busy_copy_for_test());
        }
        assert!(operation.capture(&AtomicBool::new(false)).is_err());
        let owner = match operation.finish() {
            Err(failure) => failure.into_session(),
            Ok(_) => panic!("failed capture published an image for fault {fault}"),
        };
        assert!(owner.session.store.owns_writer_lock());
        assert_eq!(unsafe { owner.session.store.db.handle() } as usize, source);
        assert_eq!(owner.validate_current().is_err(), poisoned);
        drop(owner);
        drop(outcome);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 0, "fault {fault} leaked a payload guard");
        assert!(crate::writer_lock(&path).is_ok());
    }
}

#[test]
fn source_cache_policy_cancel_in_real_restore_vm_prevents_publication_after_readback() {
    let (_root, owner, mut backing) = workspace_fixture();
    let mut operation = typed_operation(owner);
    set_cache_size(&operation.owner.session.store.db, -8192);
    let cancel = Arc::new(AtomicBool::new(false));
    let cancelling = cancel.clone();
    let observed = Arc::new(AtomicUsize::new(0));
    let flags = observed.clone();
    let source = unsafe { operation.owner.session.store.db.handle() } as usize;
    let before = snapshot(&operation.owner.session.store.db).unwrap();
    let path = operation.owner.session.store.path.clone();
    let file_before = std::fs::read(&path).unwrap();
    operation.owner.session.store.db.progress_handler(1, Some(move || {
        // SAFETY: this callback is installed on, and owned by, the exact live
        // source whose handle was captured above. It is removed below before
        // that owner can move or close. These three introspection calls neither
        // step/prepare SQL nor replace hooks, change settings, or grant access.
        // A statement and its immutable SQL remain live throughout this callback.
        unsafe {
            let db = source as *mut rusqlite::ffi::sqlite3;
            let mut statement = rusqlite::ffi::sqlite3_next_stmt(db, std::ptr::null_mut());
            while !statement.is_null() {
                let sql = rusqlite::ffi::sqlite3_sql(statement);
                if !sql.is_null() && rusqlite::ffi::sqlite3_stmt_busy(statement) != 0 {
                    match std::ffi::CStr::from_ptr(sql).to_bytes() {
                        b"PRAGMA main.cache_size=-8192" => {
                            flags.fetch_or(1, Ordering::SeqCst);
                            cancelling.store(true, Ordering::Release);
                        }
                        b"PRAGMA main.cache_size" if cancelling.load(Ordering::Acquire) => {
                            flags.fetch_or(2, Ordering::SeqCst);
                        }
                        _ => (),
                    }
                }
                statement = rusqlite::ffi::sqlite3_next_stmt(db, statement);
            }
        }
        // Do not interrupt or suppress existing cancellation semantics. Set
        // the actual operation's cancellation flag and let restoration finish.
        false
    })).unwrap();
    let result = capture_fault(&mut operation, &mut backing, &cancel, 0);
    assert!(result.result().is_err());
    assert_eq!(observed.load(Ordering::SeqCst), 3,
        "actual restore and exact readback VMs must both execute before the late cancellation refusal");
    assert!(cancel.load(Ordering::Acquire));
    assert!(matches!(operation.state, CaptureState::Failed));
    operation.owner.session.store.db.progress_handler(0, None::<fn() -> bool>).unwrap();
    assert_eq!(cache_size(&operation.owner.session.store.db), -8192);
    assert!(!operation.owner.session.store.transaction_poisoned.get());
    assert!(transaction_none(&operation.owner.session.store.db));
    assert_eq!(unsafe { operation.owner.session.store.db.handle() } as usize, source);
    assert!(operation.owner.session.store.owns_writer_lock());
    assert!(crate::writer_lock(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), file_before);
    assert_eq!(snapshot(&operation.owner.session.store.db).unwrap(), before);
    assert!(operation.finish().is_err());
}

#[test]
fn source_cache_policy_precancel_and_expired_admission_run_no_policy_sql() {
    for expired in [false, true] {
        let (_root, owner, mut backing) = workspace_fixture();
        let mut operation = typed_operation(owner);
        set_cache_size(&operation.owner.session.store.db, -8192);
        if expired { operation.admission.deadline = Instant::now(); }
        let deadline = operation.deadline();
        let log = install_cache_log(&operation.owner.session.store.db);
        let before = snapshot(&operation.owner.session.store.db).unwrap();
        let result = capture_fault(&mut operation, &mut backing, &AtomicBool::new(!expired), 0);
        assert!(result.result().is_err());
        assert!(matches!(operation.state, CaptureState::Failed));
        assert_eq!(operation.deadline(), deadline);
        assert!(log.lock().unwrap().is_empty(), "inherited entry fence must precede all new cache SQL");
        assert_eq!(cache_size(&operation.owner.session.store.db), -8192);
        assert_eq!(snapshot(&operation.owner.session.store.db).unwrap(), before);
        assert!(operation.owner.session.store.owns_writer_lock());
        assert!(!operation.owner.session.store.transaction_poisoned.get());
        assert!(operation.finish().is_err());
    }
}

#[test]
fn source_cache_policy_explicit_restore_unwind_consumes_the_cleanup_obligation() {
    let (_root, owner, mut backing) = workspace_fixture();
    let mut operation = typed_operation(owner);
    set_cache_size(&operation.owner.session.store.db, -8192);
    let log = install_cache_log(&operation.owner.session.store.db);
    let CurrentImageCaptureWorkspace { result, loan } = operation.borrow_control_workspace(&mut backing).unwrap();
    let store = &operation.owner.session.store;
    let control = BootstrapWorkspace::from_capture_loan(store, loan);
    let mut policy = SourceCachePolicy::begin(store, &operation.admission._slots, &control).unwrap().unwrap();
    let outcome = catch_unwind(AssertUnwindSafe(|| policy.finish_with_fault_for_test(true)));
    assert!(outcome.is_err());
    assert_eq!(policy.restoration_attempts_for_test(), 1);
    assert!(store.transaction_poisoned.get());
    assert!(policy.finish().is_err());
    assert_eq!(policy.restoration_attempts_for_test(), 1);
    drop(policy);
    assert_eq!(writes(&log), ["-1024"], "panic before restore SQL must not trigger an ordinary Drop retry");
    assert!(store.db.query_row("SELECT 1", [], |_| Ok(())).is_err());
    assert!(store.owns_writer_lock());
    drop(control);
    drop(result);
}

// Start with a genuine native40 file containing reusable free pages, before
// original source acquisition/admission. No authenticated owner is modified to
// manufacture a later cache footprint, and no extra table survives migration.
fn large_file_fixture() -> (tempfile::TempDir, CompletedCatalogSession, allocation::OfflineControlWorkspaceBacking) {
    use crate::migration::native_session::bootstrap_session::tests::{HOST, command, policy};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("large-original-cache.db");
    initialize_native_at(&path, 40).unwrap();
    {
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE cache_fixture_pages(payload BLOB); INSERT INTO cache_fixture_pages VALUES(zeroblob(4194304)); DROP TABLE cache_fixture_pages;").unwrap();
    }
    let mut backing = allocation::OfflineControlWorkspaceBacking::new();
    let context = backing.admit(OfflineAllocationReservation::default()).unwrap();
    let mut owner = RetainedNativeUpgrade::acquire(&path, NativeUpgradeQuiescence {
        admission_stopped: true, readers_and_workers_stopped: true, exclusive_operational_ownership: true,
    }).unwrap();
    context.bind_source(&owner.db).unwrap();
    owner.allocation = Some(context);
    let (owner, plan) = owner.plan_closed41().unwrap();
    let (owner, _) = owner.apply_closed41(&plan, None, &|_, _, _| Ok(())).unwrap();
    let session = owner.into_bootstrap().unwrap();
    let first = command(&session, session.prepare(HOST, &[]).unwrap());
    assert!(matches!(session.execute(&first, HOST, &[], policy()).unwrap(), CommandOutcome::Committed { .. }));
    let owner = session.into_completed_catalog(&first).unwrap();
    assert!(std::fs::metadata(&path).unwrap().len() > 3 * 1024 * 1024);
    (root, owner, backing)
}
fn sqlite_components(db: &Connection) -> [u64; 3] {
    [rusqlite::ffi::SQLITE_DBSTATUS_CACHE_USED, rusqlite::ffi::SQLITE_DBSTATUS_SCHEMA_USED,
        rusqlite::ffi::SQLITE_DBSTATUS_STMT_USED].map(|operation| {
        let mut current = 0;
        let mut highwater = 0;
        // SAFETY: read-only DBSTATUS on the retained, live source connection;
        // both output pointers are valid and no counter is reset.
        let code = unsafe { rusqlite::ffi::sqlite3_db_status(db.handle(), operation,
            &mut current, &mut highwater, 0) };
        assert_eq!(code, rusqlite::ffi::SQLITE_OK);
        u64::try_from(current).unwrap()
    })
}
fn warm_original_file_cache(store: &Store) -> [u64; 3] {
    let context = &store.offline_bootstrap.as_ref().unwrap().allocation;
    let bytes = std::fs::metadata(&store.path).unwrap().len();
    let buffer = context.reserve(&store.db, bytes).unwrap();
    let serialized = store.db.serialize(rusqlite::MAIN_DB).unwrap();
    assert_eq!(serialized.len() as u64, bytes);
    context.check(Some(&store.db), 0).unwrap();
    drop(serialized);
    drop(buffer);
    let measured = sqlite_components(&store.db);
    assert_eq!(measured.into_iter().sum::<u64>(), crate::catalog_backup::allocations(&store.db).unwrap());
    context.check(Some(&store.db), 0).unwrap();
    measured
}

#[test]
fn source_cache_policy_real_original_cache_reduces_then_regrows_with_all_categories_live() {
    let (_root, owner, mut backing) = large_file_fixture();
    let mut operation = typed_operation(owner);
    set_cache_size(&operation.owner.session.store.db, -8192);
    let file_before = std::fs::read(&operation.owner.session.store.path).unwrap();
    let before = snapshot(&operation.owner.session.store.db).unwrap();
    let source = unsafe { operation.owner.session.store.db.handle() } as usize;
    let context = operation.owner.session.store.offline_bootstrap.as_ref().unwrap().allocation.clone();
    let identity = context.preparation_state_for_test().unwrap();
    let external = context.external_retained_bytes().unwrap();
    let retained = context.retained_components_for_test().unwrap();
    let CurrentImageCaptureWorkspace { result, loan } = operation.borrow_control_workspace(&mut backing).unwrap();
    let store = &operation.owner.session.store;
    let control = BootstrapWorkspace::from_capture_loan(store, loan);
    let temporary = context.temporary_bytes_for_test().unwrap();
    let original = warm_original_file_cache(store);
    assert!(original[0] > 2 * 1024 * 1024, "fixture must actually warm the original source, not infer retention from cache_size");
    let mut policy = SourceCachePolicy::begin(store, &operation.admission._slots, &control).unwrap().unwrap();
    let bounded = warm_original_file_cache(store);
    assert!(bounded[0] < original[0], "actual original-source cache did not shrink: {original:?} -> {bounded:?}");
    // This is an empirical reduction assertion, deliberately not a <=1MiB
    // hard-cache or transient allocator-high-water claim.
    assert_eq!(bounded[1..], original[1..], "schema/statement categories must not be rebranded or deducted");
    policy.finish().unwrap();
    assert_eq!(cache_size(&store.db), -8192);
    let restored = warm_original_file_cache(store);
    assert!(restored[0] > bounded[0], "the same original pager must regrow under its exact restored policy");
    assert_eq!(restored[1..], original[1..]);
    eprintln!("original_source_cache_policy_dbstatus original={original:?} bounded={bounded:?} restored={restored:?}");
    assert_eq!(context.external_retained_bytes().unwrap(), external);
    assert_eq!(context.retained_components_for_test().unwrap(), retained);
    assert_eq!(context.temporary_bytes_for_test().unwrap(), temporary);
    assert_eq!(context.preparation_state_for_test().unwrap(), identity);
    assert_eq!(unsafe { store.db.handle() } as usize, source);
    assert_eq!(snapshot(&store.db).unwrap(), before);
    assert_eq!(std::fs::read(&store.path).unwrap(), file_before);
    assert!(!store.transaction_poisoned.get());
    assert!(transaction_none(&store.db));
    assert!(store.owns_writer_lock());
    drop(policy);
    drop(control);
    drop(result);
    operation.owner.validate_current().unwrap();
}

#[test]
fn source_cache_policy_cancel_after_availability_fence_before_restore_prevents_publication() {
    let (_root, owner, mut backing) = workspace_fixture();
    let mut operation = typed_operation(owner);
    set_cache_size(&operation.owner.session.store.db, -8192);
    let cancel = AtomicBool::new(false);
    let before = snapshot(&operation.owner.session.store.db).unwrap();
    let result = capture_fault(&mut operation, &mut backing, &cancel, 19);
    assert!(result.result().is_err());
    assert!(cancel.load(Ordering::Acquire));
    assert!(matches!(operation.state, CaptureState::Failed));
    assert_eq!(cache_size(&operation.owner.session.store.db), -8192);
    assert!(!operation.owner.session.store.transaction_poisoned.get());
    assert!(operation.owner.session.store.owns_writer_lock());
    assert!(transaction_none(&operation.owner.session.store.db));
    assert_eq!(snapshot(&operation.owner.session.store.db).unwrap(), before);
    assert!(operation.finish().is_err());
}

#[test]
fn source_cache_policy_own_live_pragma_vm_is_counted_before_mutation() {
    let (_root, owner, mut backing) = workspace_fixture();
    let mut operation = typed_operation(owner);
    set_cache_size(&operation.owner.session.store.db, -8192);
    let before = snapshot(&operation.owner.session.store.db).unwrap();
    let log = install_cache_log(&operation.owner.session.store.db);
    let context = operation.owner.session.store.offline_bootstrap.as_ref().unwrap().allocation.clone();
    let CurrentImageCaptureWorkspace { result, loan } = operation.borrow_control_workspace(&mut backing).unwrap();
    let store = &operation.owner.session.store;
    let control = BootstrapWorkspace::from_capture_loan(store, loan);
    let used = context.retained_native_bytes(&store.db).unwrap() + context.external_retained_bytes().unwrap();
    let pressure = context.reserve(&store.db, allocation::LIMIT - used).unwrap();
    // No foreign VM exists: the exact old source fits at the fixed cap. The
    // policy's own newly prepared, still-live PRAGMA VM must cause refusal.
    context.check(Some(&store.db), 0).unwrap();
    assert!(SourceCachePolicy::begin(store, &operation.admission._slots, &control).is_err());
    assert_eq!(*log.lock().unwrap(), vec![None]);
    assert!(writes(&log).is_empty());
    assert!(!store.transaction_poisoned.get());
    assert!(store.owns_writer_lock());
    assert!(transaction_none(&store.db));
    drop(pressure);
    context.check(Some(&store.db), 0).unwrap();
    assert_eq!(cache_size(&store.db), -8192);
    assert_eq!(snapshot(&store.db).unwrap(), before);
    drop(control);
    drop(result);
    operation.owner.validate_current().unwrap();
}
