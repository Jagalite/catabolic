// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! R2 admission, handle lifetime and transaction cleanup qualification.
//! Path replacements and incarnation changes below are fault injection into
//! disposable offline fixtures, not an implementation of production restore.

use catabolic_store::{
    Error, OpenErrorCode, Store, definitions,
    embedding::{Session, command},
    fallback_mutation, fallback_projection, migration, native_admission_exclusive,
};
use rusqlite::{
    Connection,
    hooks::{AuthAction, AuthContext, Authorization, TransactionOperation},
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.sqlite3");
    migration::initialize_native(&path).unwrap();
    (root, path)
}

fn sidecar(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(".native-admission.lock");
    value.into()
}

fn replace_with_same_identity(path: &Path) {
    let replacement = path.with_extension("replacement");
    let before = fs::metadata(path).unwrap();
    fs::copy(path, &replacement).unwrap();
    assert_ne!(before.ino(), fs::metadata(&replacement).unwrap().ino());
    fs::rename(replacement, path).unwrap();
}

fn replace_sidecar(path: &Path) {
    let lock = sidecar(path);
    let replacement = path.with_extension("replacement-lock");
    fs::write(&replacement, b"").unwrap();
    fs::rename(replacement, lock).unwrap();
}

/// Exercise the existing identity guard with a controlled SQL fault, while
/// preserving the schema. This is deliberately not a restore entry point.
fn change_incarnation(db: &Connection) -> catabolic_store::Result<()> {
    db.execute_batch(
        "INSERT INTO catalog_mutation_permits(scope_kind,scope_id,incarnation)
         SELECT 'restore',catalog_id,incarnation FROM catalog_state WHERE id=1;
         UPDATE catalog_state SET incarnation=lower(hex(randomblob(16))) WHERE id=1;
         DELETE FROM catalog_mutation_permits WHERE scope_kind='restore';",
    )?;
    Ok(())
}

fn count_profile(store: &Store, name: &str) -> i64 {
    store
        .db
        .query_row("SELECT count(*) FROM profiles WHERE id=?", [name], |row| {
            row.get(0)
        })
        .unwrap()
}

#[test]
fn native_open_is_nonmutating_and_preserves_exact_recovery_gates() {
    let (root, path) = fixture();
    let before = fs::read(&path).unwrap();
    let identity = {
        let store = Store::open(&path, true, false).unwrap();
        assert_eq!(store.schema_version, migration::NATIVE_SCHEMA_VERSION);
        assert!(store.catalog_incarnation.is_some());
        (store.database_id.clone(), store.catalog_incarnation.clone())
    };
    for writable in [false, true] {
        let store = Store::open(&path, writable, false).unwrap();
        assert_eq!(
            (&store.database_id, &store.catalog_incarnation),
            (&identity.0, &identity.1)
        );
        for (flag, expected) in [
            ("foreign_keys", 1),
            ("recursive_triggers", 1),
            ("read_uncommitted", 0),
        ] {
            assert_eq!(
                store
                    .db
                    .pragma_query_value(None, flag, |row| row.get::<_, i64>(0))
                    .unwrap(),
                expected
            );
        }
        drop(store);
        let error = Store::open_typed(&path, writable, true).err().unwrap();
        assert_eq!(error.code, OpenErrorCode::UnsupportedSchema);
        assert!(
            error
                .error
                .0
                .contains("separately qualified recovery admission")
        );
    }
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(
        catabolic_store::RECOVERABLE_SCHEMAS,
        &[
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 24, 25, 26, 28
        ]
    );
    let missing = root.path().join("missing.sqlite3");
    assert!(Store::open(&missing, true, false).is_err());
    assert!(!missing.exists());
}

#[test]
fn native_readers_and_writer_share_lease_and_exclude_lifecycle_owner() {
    let (_root, path) = fixture();
    let first = Store::open(&path, false, false).unwrap();
    let second = Store::open(&path, false, false).unwrap();
    let writer = Store::open(&path, true, false).unwrap();
    assert!(native_admission_exclusive(&path).is_err());
    drop(writer);
    assert!(native_admission_exclusive(&path).is_err());
    drop(first);
    assert!(native_admission_exclusive(&path).is_err());
    drop(second);
    let lifecycle = native_admission_exclusive(&path).unwrap();
    for writable in [false, true] {
        assert_eq!(
            Store::open_typed(&path, writable, false)
                .err()
                .unwrap()
                .code,
            OpenErrorCode::BusyWriter
        );
    }
    drop(lifecycle);
    assert!(Store::open(&path, true, false).is_ok());
}

#[test]
fn native_read_snapshot_is_stable_and_refuses_writes() {
    let (_root, path) = fixture();
    // This qualifies ordinary native snapshots only. Native upgrade admission
    // continues to reject WAL, whose upgrade safety is not inferred here.
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "journal_mode", "WAL")
        .unwrap();
    let reader = Store::open(&path, false, false).unwrap();
    let writer = Store::open(&path, true, false).unwrap();
    assert_eq!(count_profile(&reader, "after-snapshot"), 0);
    writer
        .transaction(|db| {
            db.execute("INSERT INTO profiles VALUES ('after-snapshot')", [])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(count_profile(&reader, "after-snapshot"), 0);
    assert_eq!(count_profile(&writer, "after-snapshot"), 1);
    assert!(
        reader
            .transaction(|_| Ok(()))
            .unwrap_err()
            .0
            .contains("read-only")
    );
    assert!(native_admission_exclusive(&path).is_err());
    drop(reader);
    let fresh = Store::open(&path, false, false).unwrap();
    assert_eq!(count_profile(&fresh, "after-snapshot"), 1);
}

#[test]
fn session_reconnect_refuses_same_id_incarnation_on_replaced_inode() {
    let (_root, path) = fixture();
    let mut session = Session::open(&path, true, false).unwrap();
    let identity = session.store().unwrap().catalog_incarnation.clone();
    let database_id = session.store().unwrap().database_id.clone();
    session.detach().unwrap();
    replace_with_same_identity(&path);
    assert!(session.reconnect().unwrap_err().0.contains("replaced"));
    assert!(session.store().is_err());
    assert!(session.begin().is_err());
    let fresh = Store::open(&path, true, false).unwrap();
    assert_eq!(fresh.database_id, database_id);
    assert_eq!(fresh.catalog_incarnation, identity);
}

#[test]
fn store_detached_refuses_same_id_incarnation_on_replaced_inode() {
    let (_root, path) = fixture();
    let mut store = Store::open(&path, true, false).unwrap();
    let identity = store.catalog_incarnation.clone();
    let outcome = store.detached(|| {
        replace_with_same_identity(&path);
        Ok("external result must not be acknowledged")
    });
    assert!(outcome.unwrap_err().0.contains("replaced"));
    assert!(!store.owns_writer_lock());
    assert!(
        store
            .db
            .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
            .is_err()
    );
    assert!(command(&store, "CREATE TABLE revived(n)", &json!([])).is_err());
    assert!(store.transaction(|_| Ok(())).is_err());
    let fresh = Store::open(&path, true, false).unwrap();
    assert_eq!(fresh.catalog_incarnation, identity);
}

#[test]
fn reconnect_and_detached_refuse_same_inode_incarnation_change() {
    for via_session in [false, true] {
        let (_root, path) = fixture();
        let inode = fs::metadata(&path).unwrap().ino();
        let inject = || -> catabolic_store::Result<()> {
            let db = catabolic_store::connect(&path, true)?;
            db.execute_batch("BEGIN IMMEDIATE")?;
            change_incarnation(&db)?;
            db.execute_batch("COMMIT")?;
            Ok(())
        };
        if via_session {
            let mut session = Session::open(&path, true, false).unwrap();
            session.detach().unwrap();
            inject().unwrap();
            assert!(session.reconnect().unwrap_err().0.contains("replaced"));
            assert!(session.store().is_err());
        } else {
            let mut store = Store::open(&path, true, false).unwrap();
            assert!(store.detached(inject).unwrap_err().0.contains("replaced"));
            assert!(command(&store, "SELECT 1", &json!([])).is_err());
        }
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
        assert!(Store::open(&path, true, false).is_ok());
    }
}

#[test]
fn replaced_admission_sidecar_fences_checked_reads_begin_and_commit() {
    for unlink in [false, true] {
        let (_root, path) = fixture();
        let store = Store::open(&path, true, false).unwrap();
        let outcome = store.transaction(|db| {
            db.execute("INSERT INTO profiles VALUES ('must-rollback')", [])?;
            if unlink {
                fs::remove_file(sidecar(&path))?;
            } else {
                replace_sidecar(&path);
            }
            assert!(command(&store, "SELECT 1", &json!([])).is_err());
            assert!(store.transaction(|_| Ok(())).is_err());
            Ok(())
        });
        assert!(outcome.is_err());
        assert!(
            store.db.is_autocommit(),
            "failed identity commit leaked its transaction"
        );
        assert_eq!(count_profile(&store, "must-rollback"), 0);
        assert!(command(&store, "SELECT 1", &json!([])).is_err());
    }
}

#[test]
fn store_detached_refuses_replaced_admission_sidecar_before_returning_result() {
    let (_root, path) = fixture();
    let mut store = Store::open(&path, true, false).unwrap();
    let outcome = store.detached(|| {
        replace_sidecar(&path);
        Ok("must not escape invalid admission")
    });
    assert!(
        outcome.is_err(),
        "detached acknowledged an invalid lifecycle lease"
    );
    assert!(!store.owns_writer_lock());
    assert!(
        store
            .db
            .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
            .is_err()
    );
    assert!(command(&store, "SELECT 1", &json!([])).is_err());
}

fn evaluation(session: &mut Session) -> Value {
    session
        .query_call(
            "default",
            "evaluation-create",
            &json!({"max_bytes":1048576}),
        )
        .unwrap()
}

fn seed_empty_projection(session: &mut Session) {
    let store = session.store().unwrap();
    store
        .db
        .execute("INSERT INTO catalogs VALUES ('lifetime')", [])
        .unwrap();
    let query = definitions::query_put(
        store,
        "default",
        "lifetime-members",
        &json!({
            "selection":{"language":"sql","query":"SELECT id AS item_id FROM items WHERE 0"}
        }),
    )
    .unwrap();
    let candidates = definitions::query_put(store, "default", "lifetime-candidates", &json!({
        "selection":{"language":"sql","query":"SELECT id AS association_id FROM item_files WHERE 0"}
    })).unwrap();
    let policy = fallback_mutation::put(
        store,
        "default",
        "lifetime-policy",
        &json!({
            "fallbacks":[{"name":"all","query_id":candidates["id"]}],
            "within_tier":{"tie_break":"file_id"},"failback":{"mode":"immediate"}
        }),
    )
    .unwrap();
    definitions::dispatch(store, "default", "layout-put", &json!({
        "name":"lifetime-flat","value":{"version":1,"rules":[{"name":"all","path":"{item.id}/{file.name}"}]}
    })).unwrap();
    session
        .with_evaluation("default", &json!({}), 60000, 10000, |eval| {
            fallback_projection::bind(
                eval,
                "lifetime",
                policy["id"].as_str().unwrap(),
                query["id"].as_str().unwrap(),
                "lifetime-flat",
            )
        })
        .unwrap();
}

fn prepared(session: &mut Session) -> fallback_projection::PreparedProjection {
    session
        .with_evaluation("default", &json!({}), 60000, 10000, |eval| {
            fallback_projection::plan(eval, "lifetime", false, &mut |_, _, _, _, _| {
                Err(Error("empty fixture unexpectedly probed media".into()))
            })
        })
        .unwrap()
}

fn plan_args(handle: &Value) -> Value {
    json!({"plan_owner_matches":true,"plan_token":handle["token"],"plan":handle["plan"]})
}

#[test]
fn reconnect_invalidates_handles_without_reusing_counters_or_release_registry() {
    let (_root, path) = fixture();
    let mut session = Session::open(&path, true, false).unwrap();
    seed_empty_projection(&mut session);
    let old_evaluation = evaluation(&mut session);
    let plan = prepared(&mut session);
    let old_plan = session.fallback_plans().remember("default", plan).unwrap();
    let releases = session.fallback_plans().releases();
    session.detach().unwrap();
    session.reconnect().unwrap();
    assert!(
        session
            .query_call(
                "default",
                "evaluation-check",
                &json!({"session":old_evaluation["token"]})
            )
            .is_err()
    );
    assert!(
        session
            .fallback_plans()
            .checked("default", &plan_args(&old_plan))
            .is_err()
    );
    let new_evaluation = evaluation(&mut session);
    assert!(new_evaluation["token"].as_u64().unwrap() > old_evaluation["token"].as_u64().unwrap());
    let plan = prepared(&mut session);
    assert_eq!(plan.full_plan(), &old_plan["plan"]);
    let mut previous = old_plan["token"].as_u64().unwrap();
    // Exceed the registry's 256-handle bound with the release facade retained
    // before reconnect: rebuilding the registry would leak these new handles.
    for _ in 0..300 {
        let current = session
            .fallback_plans()
            .remember("default", plan.clone())
            .unwrap();
        let token = current["token"].as_u64().unwrap();
        assert!(token > previous);
        previous = token;
        assert!(
            session
                .fallback_plans()
                .checked("default", &plan_args(&current))
                .is_ok()
        );
        releases.forget(token).unwrap();
        assert!(
            session
                .fallback_plans()
                .checked("default", &plan_args(&current))
                .is_err()
        );
        assert!(
            session
                .fallback_plans()
                .checked("default", &plan_args(&old_plan))
                .is_err()
        );
    }
}

#[test]
fn active_and_aborted_lexical_scopes_refuse_detach_without_losing_admission() {
    let (_root, path) = fixture();
    let mut session = Session::open(&path, true, false).unwrap();
    seed_empty_projection(&mut session);
    let plan = prepared(&mut session);
    let plan_handle = session.fallback_plans().remember("default", plan).unwrap();
    let token = evaluation(&mut session);
    let outer = session.begin().unwrap();
    let inner = session.begin().unwrap();
    assert!(
        session
            .detach()
            .unwrap_err()
            .0
            .contains("active transaction")
    );
    assert!(session.store_mut().unwrap().detached(|| Ok(())).is_err());
    assert!(native_admission_exclusive(&path).is_err());
    assert!(
        session
            .check_evaluation("default", &json!({"session":token["token"]}))
            .is_ok()
    );
    assert!(
        session
            .fallback_plans()
            .checked("default", &plan_args(&plan_handle))
            .is_ok()
    );
    session
        .store()
        .unwrap()
        .db
        .execute_batch("ROLLBACK")
        .unwrap();
    assert!(session.detach().is_err());
    assert!(session.begin().is_err());
    assert!(session.check_scope(Some(inner)).is_err());
    session.finish(inner, false).unwrap();
    assert!(session.detach().is_err());
    session.finish(outer, false).unwrap();
    assert!(session.store().unwrap().owns_writer_lock());
    session.detach().unwrap();
    assert!(native_admission_exclusive(&path).is_ok());
    session.reconnect().unwrap();
}

#[test]
fn native_connection_flags_are_fenced_before_checked_reads_and_transactions() {
    let (_root, path) = fixture();
    let store = Store::open(&path, true, false).unwrap();
    for (flag, unsafe_value, expected) in [
        ("foreign_keys", 0, 1),
        ("recursive_triggers", 0, 1),
        ("read_uncommitted", 1, 0),
    ] {
        store.db.pragma_update(None, flag, unsafe_value).unwrap();
        assert!(
            store
                .validate_native_identity()
                .unwrap_err()
                .0
                .contains("unsafe native catalog connection settings")
        );
        assert!(command(&store, "SELECT 1", &json!([])).is_err());
        assert!(store.transaction(|_| Ok(())).is_err());
        store.db.pragma_update(None, flag, expected).unwrap();
        store.validate_native_identity().unwrap();
    }
}

#[test]
fn failed_nested_incarnation_check_rolls_back_only_its_scope() {
    let (_root, path) = fixture();
    let store = Store::open(&path, true, false).unwrap();
    let original = store.catalog_incarnation.clone().unwrap();
    store
        .transaction(|outer| {
            outer.execute("INSERT INTO profiles VALUES ('outer-survives')", [])?;
            let failed = store.transaction(|inner| {
                inner.execute("INSERT INTO profiles VALUES ('inner-rejected')", [])?;
                change_incarnation(inner)?;
                Ok(())
            });
            assert!(failed.unwrap_err().0.contains("incarnation changed"));
            assert!(!outer.is_autocommit());
            assert_eq!(count_profile(&store, "inner-rejected"), 0);
            assert_eq!(
                outer.query_row("SELECT incarnation FROM catalog_state", [], |row| row
                    .get::<_, String>(0))?,
                original
            );
            store.validate_native_identity()?;
            Ok(())
        })
        .unwrap();
    assert!(store.db.is_autocommit());
    assert_eq!(count_profile(&store, "outer-survives"), 1);
}

#[test]
fn native_commit_failure_rolls_back_before_owner_reuse() {
    let (_root, path) = fixture();
    let mut session = Session::open(&path, true, false).unwrap();
    let token = session.begin().unwrap();
    session
        .store()
        .unwrap()
        .db
        .execute_batch(
            "INSERT INTO profiles VALUES ('failed-commit');
         INSERT INTO catalog_mutation_permits(scope_kind,scope_id,incarnation)
         SELECT 'restore',catalog_id,incarnation FROM catalog_state WHERE id=1;",
        )
        .unwrap();
    assert!(session.finish(token, true).is_err());
    assert!(!session.in_transaction().unwrap());
    assert_eq!(count_profile(session.store().unwrap(), "failed-commit"), 0);
    assert!(session.check_scope(Some(token)).is_err());
    let fresh = session.begin().unwrap();
    assert!(fresh > token);
    session.finish(fresh, true).unwrap();
}

#[test]
fn failed_nested_cleanup_poison_cannot_be_revived_by_command_authorizer() {
    let (_root, path) = fixture();
    let store = Store::open(&path, true, false).unwrap();
    let outcome = store.transaction(|outer| {
        outer.execute("INSERT INTO profiles VALUES ('poisoned-outer')", [])?;
        let nested = store.transaction::<()>(|inner| {
            inner.execute("INSERT INTO profiles VALUES ('poisoned-inner')", [])?;
            inner.authorizer(Some(|context: AuthContext<'_>| match context.action {
                AuthAction::Savepoint {
                    operation: TransactionOperation::Rollback,
                    ..
                } => Authorization::Deny,
                _ => Authorization::Allow,
            }))?;
            Err(Error("injected nested failure".into()))
        });
        assert!(nested.unwrap_err().0.contains("native rollback failed"));
        assert!(command(&store, "SELECT 1", &json!([])).is_err());
        assert!(
            store
                .db
                .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                .is_err()
        );
        Ok(())
    });
    assert!(outcome.is_err());
    drop(store);
    let reopened = Store::open(&path, true, false).unwrap();
    assert_eq!(count_profile(&reopened, "poisoned-outer"), 0);
    assert_eq!(count_profile(&reopened, "poisoned-inner"), 0);
}

#[test]
fn legacy_reconnect_retains_its_evaluation_and_plan_handle_behavior() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("legacy.sqlite3");
    migration::initialize(&path).unwrap();
    let mut session = Session::open(&path, true, false).unwrap();
    seed_empty_projection(&mut session);
    let handle = evaluation(&mut session);
    let plan = prepared(&mut session);
    let plan_handle = session.fallback_plans().remember("default", plan).unwrap();
    assert!(session.store().unwrap().catalog_incarnation.is_none());
    session.detach().unwrap();
    session.reconnect().unwrap();
    assert!(
        session
            .check_evaluation("default", &json!({"session":handle["token"]}))
            .is_ok()
    );
    assert!(
        session
            .fallback_plans()
            .checked("default", &plan_args(&plan_handle))
            .is_ok()
    );
    assert_eq!(session.store().unwrap().schema_version, 32);
}

#[test]
fn inherited_native_store_requires_fresh_admission() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "inherited_native_store_helper",
            "--test-threads=1",
        ])
        .env("CATABOLIC_NATIVE_STORE_FORK_HELPER", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("1 passed"), "{stdout}");
}

#[test]
#[ignore = "run through the isolated subprocess wrapper"]
fn inherited_native_store_helper() {
    if std::env::var_os("CATABOLIC_NATIVE_STORE_FORK_HELPER").is_none() {
        return;
    }
    let (_root, path) = fixture();
    let store = Store::open(&path, true, false).unwrap();
    // SAFETY: the isolated helper has no other active SQLite users. The child
    // validates the lease before any SQLite operation. The parent waits until
    // the child has closed its inherited copy before using its own connection.
    let child = unsafe { libc::fork() };
    assert!(child >= 0);
    if child == 0 {
        let rejected = store
            .validate_native_identity()
            .is_err_and(|error| error.0.contains("inherited across fork"));
        let read_rejected = command(&store, "SELECT 1", &json!([])).is_err();
        let begin_rejected = store.transaction(|_| Ok(())).is_err();
        drop(store); // Neither inherited native nor writer lease may be unlocked.
        // SAFETY: terminate only this fork child without running destructors.
        unsafe {
            libc::_exit(if rejected && read_rejected && begin_rejected {
                0
            } else {
                1
            })
        };
    }
    let mut status = 0;
    // SAFETY: child is the process created above and status is writable.
    assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
    assert!(libc::WIFEXITED(status));
    assert_eq!(libc::WEXITSTATUS(status), 0);
    store.validate_native_identity().unwrap();
    assert!(native_admission_exclusive(&path).is_err());
    assert_eq!(
        Store::open_typed(&path, true, false).err().unwrap().code,
        OpenErrorCode::BusyWriter
    );
    drop(store);
    assert!(native_admission_exclusive(&path).is_ok());
}

#[test]
fn replaced_sidecar_in_nested_scope_prevents_outer_commit_too() {
    let (_root, path) = fixture();
    let store = Store::open(&path, true, false).unwrap();
    let failed = store.transaction(|outer| {
        outer.execute("INSERT INTO profiles VALUES ('invalid-outer')", [])?;
        let inner = store.transaction(|inner| {
            inner.execute("INSERT INTO profiles VALUES ('invalid-inner')", [])?;
            replace_sidecar(&path);
            Ok(())
        });
        assert!(inner.is_err());
        assert!(!outer.is_autocommit());
        assert_eq!(count_profile(&store, "invalid-inner"), 0);
        Ok(())
    });
    assert!(failed.is_err());
    assert!(store.db.is_autocommit());
    assert_eq!(count_profile(&store, "invalid-outer"), 0);
}

#[test]
fn implicit_rollback_cancellation_and_panic_keep_tokens_poisoned_until_unwind() {
    for failure in ["implicit-rollback", "cancel", "panic"] {
        let (_root, path) = fixture();
        let mut session = Session::open(&path, true, false).unwrap();
        let outer = session.begin().unwrap();
        session
            .store()
            .unwrap()
            .db
            .execute("INSERT INTO profiles VALUES ('aborted-outer')", [])
            .unwrap();
        let inner = session.begin().unwrap();
        match failure {
            "implicit-rollback" => {
                assert!(
                    session
                        .store()
                        .unwrap()
                        .db
                        .execute("INSERT OR ROLLBACK INTO profiles VALUES ('default')", [])
                        .is_err()
                );
                session.synchronize();
            }
            "cancel" => session.abort_scopes(),
            "panic" => {
                let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _ =
                        session.with_evaluation::<()>("default", &json!({}), 5000, 10000, |_| {
                            panic!("disposable native evaluation panic")
                        });
                }));
                assert!(panic.is_err());
                session.unwind();
            }
            _ => unreachable!(),
        }
        assert!(session.store().is_err());
        assert!(session.check_scope(Some(inner)).is_err());
        assert!(session.begin().is_err());
        assert!(session.detach().is_err());
        assert!(session.finish(inner, true).is_err());
        assert!(session.store().is_err());
        session.finish(outer, false).unwrap();
        assert_eq!(count_profile(session.store().unwrap(), "aborted-outer"), 0);
        let next = session.begin().unwrap();
        assert!(next > inner);
        session.finish(next, true).unwrap();
    }
}

#[test]
fn native_store_panic_rolls_back_only_the_lexically_owned_scope() {
    let (_root, path) = fixture();
    let store = Store::open(&path, true, false).unwrap();
    store
        .transaction(|outer| {
            outer.execute("INSERT INTO profiles VALUES ('panic-outer')", [])?;
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = store.transaction::<()>(|inner| {
                    inner.execute("INSERT INTO profiles VALUES ('panic-inner')", [])?;
                    panic!("disposable native nested panic")
                });
            }));
            assert!(caught.is_err());
            assert!(!outer.is_autocommit());
            assert_eq!(count_profile(&store, "panic-inner"), 0);
            Ok(())
        })
        .unwrap();
    assert_eq!(count_profile(&store, "panic-outer"), 1);
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = store.transaction::<()>(|outer| {
            outer.execute("INSERT INTO profiles VALUES ('panic-top-level')", [])?;
            panic!("disposable native top-level panic")
        });
    }));
    assert!(caught.is_err());
    assert!(store.db.is_autocommit());
    assert_eq!(count_profile(&store, "panic-top-level"), 0);
    store.transaction(|_| Ok(())).unwrap();
}

#[test]
fn connection_flag_change_during_commit_rolls_back_pending_writes() {
    let (_root, path) = fixture();
    let store = Store::open(&path, true, false).unwrap();
    let failed = store.transaction(|db| {
        db.execute("INSERT INTO profiles VALUES ('unsafe-flags')", [])?;
        db.pragma_update(None, "recursive_triggers", false)?;
        Ok(())
    });
    assert!(
        failed
            .unwrap_err()
            .0
            .contains("unsafe native catalog connection settings")
    );
    assert!(store.db.is_autocommit());
    assert_eq!(count_profile(&store, "unsafe-flags"), 0);
    assert!(command(&store, "SELECT 1", &json!([])).is_err());
    store
        .db
        .pragma_update(None, "recursive_triggers", true)
        .unwrap();
    store.transaction(|_| Ok(())).unwrap();
}
