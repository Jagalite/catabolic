//! Independent regression coverage for the embedding transaction ownership seam.
use catabolic_store::{Store, migration};
use std::panic::{AssertUnwindSafe, catch_unwind};

fn fixture() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    migration::initialize(&path).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    (root, store)
}

#[test]
fn unwind_rolls_back_top_level_transaction() {
    let (_root, store) = fixture();
    let caught = catch_unwind(AssertUnwindSafe(|| {
        store.transaction::<()>(|db| {
            db.execute("INSERT INTO profiles VALUES ('review-panic')", [])?;
            panic!("disposable transaction unwind");
        })
    }));
    assert!(caught.is_err());
    assert!(
        store.db.is_autocommit(),
        "panic leaked an active transaction"
    );
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT count(*) FROM profiles WHERE id='review-panic'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    store
        .transaction(|db| {
            db.execute("INSERT INTO profiles VALUES ('review-after')", [])?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn unwind_rolls_back_only_nested_savepoint() {
    let (_root, store) = fixture();
    store
        .transaction(|db| {
            db.execute("INSERT INTO profiles VALUES ('review-outer')", [])?;
            let caught = catch_unwind(AssertUnwindSafe(|| {
                store.transaction::<()>(|inner| {
                    inner.execute("INSERT INTO profiles VALUES ('review-inner')", [])?;
                    panic!("disposable nested unwind");
                })
            }));
            assert!(caught.is_err());
            assert!(
                !db.is_autocommit(),
                "nested unwind rolled back the outer transaction"
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM profiles WHERE id='review-inner'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM profiles WHERE id='review-outer'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            Ok(())
        })
        .unwrap();
    assert!(store.db.is_autocommit());
}

#[test]
fn failed_unwind_rollback_retires_connection_until_drop() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
    let (_root, store) = fixture();
    let path = store.path.clone();
    let caught = catch_unwind(AssertUnwindSafe(|| {
        store.transaction::<()>(|db| {
            db.execute("INSERT INTO profiles VALUES ('review-denied-rollback')", [])?;
            db.authorizer(Some(|context: AuthContext<'_>| match context.action {
                AuthAction::Transaction {
                    operation: TransactionOperation::Rollback,
                } => Authorization::Deny,
                _ => Authorization::Allow,
            }))?;
            panic!("disposable rollback cleanup failure");
        })
    }));
    assert!(caught.is_err());
    assert!(
        store
            .db
            .execute("INSERT INTO profiles VALUES ('review-denied-write')", [])
            .is_err()
    );
    assert!(
        store
            .db
            .query_row("SELECT 1", [], |r| r.get::<_, i64>(0))
            .is_err()
    );
    assert!(store.transaction(|_| Ok(())).is_err());
    drop(store);
    let reopened = Store::open(&path, true, false).unwrap();
    assert_eq!(
        reopened
            .db
            .query_row(
                "SELECT count(*) FROM profiles WHERE id LIKE 'review-denied-%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn failed_shared_detach_cannot_revive_placeholder_through_command() {
    use catabolic_store::embedding::{Session, command};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    migration::initialize(&path).unwrap();
    let mut owner = Session::open(&path, true, false).unwrap();
    let moved = root.path().join("catalog-moved.db");
    let outcome = owner.store_mut().unwrap().detached(|| {
        std::fs::rename(&path, &moved)?;
        Ok(())
    });
    assert!(outcome.is_err());
    let command_attempt = owner.store().and_then(|store| {
        command(
            store,
            "CREATE TABLE review_placeholder(n)",
            &serde_json::json!([]),
        )
    });
    assert!(
        command_attempt.is_err(),
        "command authorizer revived unavailable detached owner"
    );
    owner.close().unwrap();
    let reopened = Store::open(&moved, true, false).unwrap();
    assert_eq!(
        reopened
            .db
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='review_placeholder'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}
