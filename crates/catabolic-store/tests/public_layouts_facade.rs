// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Narrow ordinary facade checks, not publication or definition safety review.
use catabolic_store::{
    embedding::{Session, with_interrupt_check},
    public_layouts::call,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn fixture() -> (tempfile::TempDir, Session) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("db");
    catabolic_store::migration::initialize(&path).unwrap();
    let mut owner = Session::open(&path, true, false).unwrap();
    owner.store().unwrap().transaction(|db| {
        db.execute_batch("INSERT INTO locations VALUES ('media'); INSERT INTO files VALUES ('file','media','book.txt'); INSERT INTO items(id,kind,metadata) VALUES ('book','book','{}'); INSERT INTO item_files(id,file_id,item_id,role,origin) VALUES ('association','file','book','primary','explicit');")?;
        Ok(())
    }).unwrap();
    call(&mut owner, "default", "put", &json!({"name":"flat","value":{"version":1,"rules":[{"name":"all","path":"{item.kind}/{item.id}/{file.name}"}]}})).unwrap();
    (root, owner)
}
fn run(owner: &mut Session, apply: bool) -> catabolic_store::Result<Value> {
    call(
        owner,
        "default",
        "run",
        &json!({"name":"flat","catalog":"global","apply":apply,"limit":100}),
    )
}
fn mappings(owner: &Session) -> i64 {
    owner
        .store()
        .unwrap()
        .db
        .query_row("SELECT count(*) FROM mappings", [], |r| r.get(0))
        .unwrap()
}
#[test]
fn apply_lends_uncommitted_owner_and_outer_rollback_restores_all_state() {
    let (_root, mut owner) = fixture();
    let scope = owner.begin().unwrap();
    owner
        .store()
        .unwrap()
        .db
        .execute("UPDATE files SET path='uncommitted.txt'", [])
        .unwrap();
    assert_eq!(
        run(&mut owner, false).unwrap()["changes"][0]["path"],
        "book/book/uncommitted.txt"
    );
    assert_eq!(run(&mut owner, true).unwrap()["applied"], true);
    assert_eq!(mappings(&owner), 1);
    owner.finish(scope, false).unwrap();
    assert_eq!(mappings(&owner), 0);
    assert_eq!(
        run(&mut owner, true).unwrap()["changes"][0]["path"],
        "book/book/book.txt"
    );
}
#[test]
fn late_cancellation_rolls_back_mapping_owner_statistics_and_reuses_session() {
    let (_root, mut owner) = fixture();
    let scope = owner.begin().unwrap();
    let checks = Arc::new(AtomicUsize::new(0));
    let observed = checks.clone();
    let result = with_interrupt_check(
        move || observed.fetch_add(1, Ordering::SeqCst) == 2,
        || run(&mut owner, true),
    );
    assert!(result.unwrap_err().0.contains("cancelled"));
    assert_eq!(checks.load(Ordering::SeqCst), 3);
    assert_eq!(mappings(&owner), 0);
    assert_eq!(owner.store().unwrap().db.query_row("SELECT count(*) FROM meta WHERE key LIKE 'layout-catalog:%' OR key LIKE 'statistics:%'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    owner.finish(scope, true).unwrap();
    assert_eq!(run(&mut owner, true).unwrap()["applied"], true);
}
#[test]
fn late_panic_restores_lexical_scope_and_owner() {
    let (_root, mut owner) = fixture();
    let scope = owner.begin().unwrap();
    let checks = Arc::new(AtomicUsize::new(0));
    let observed = checks.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        with_interrupt_check(
            move || {
                if observed.fetch_add(1, Ordering::SeqCst) == 2 {
                    panic!("callback panic")
                }
                false
            },
            || run(&mut owner, true),
        )
    }));
    assert!(result.is_err());
    assert_eq!(mappings(&owner), 0);
    owner.finish(scope, false).unwrap();
    assert_eq!(run(&mut owner, true).unwrap()["applied"], true);
}
#[test]
fn invalid_limit_precedes_lookup_and_readonly_write_admission() {
    let (root, mut owner) = fixture();
    owner.close().unwrap();
    let mut readonly = Session::open(&root.path().join("db"), false, false).unwrap();
    for limit in [json!(false), json!(0), json!(1001), json!(1.0), json!("5")] {
        assert_eq!(
            call(
                &mut readonly,
                "default",
                "run",
                &json!({"name":"missing","catalog":"missing","limit":limit,"apply":true})
            )
            .unwrap_err()
            .0,
            "limit must be between 1 and 1000"
        );
    }
    assert!(
        run(&mut readonly, true)
            .unwrap_err()
            .0
            .contains("read-only")
    );
    assert_eq!(run(&mut readonly, false).unwrap()["desired_count"], 1);
}
#[test]
fn unchanged_path_preparation_still_refuses_caller_transaction() {
    let (_root, mut owner) = fixture();
    let scope = owner.begin().unwrap();
    assert!(
        catabolic_store::layout_mutation::prepare(
            owner.store().unwrap(),
            "default",
            "flat",
            "global",
            false,
            false
        )
        .unwrap_err_for_test()
        .contains("fresh snapshot")
    );
    owner.finish(scope, false).unwrap();
}
trait ErrorText<T> {
    fn unwrap_err_for_test(self) -> String;
}
impl<T> ErrorText<T> for catabolic_store::Result<T> {
    fn unwrap_err_for_test(self) -> String {
        match self {
            Err(error) => error.0,
            Ok(_) => panic!("expected refusal"),
        }
    }
}

#[test]
fn sqlite_cancellation_after_evaluation_restores_bridge_and_poisoned_scope() {
    use std::sync::atomic::AtomicBool;
    let (_root, mut owner) = fixture();
    let writing = Arc::new(AtomicBool::new(false));
    let observed = writing.clone();
    owner
        .store()
        .unwrap()
        .db
        .create_scalar_function(
            "layout_write_started",
            0,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            move |_| {
                observed.store(true, Ordering::SeqCst);
                Ok(0)
            },
        )
        .unwrap();
    owner.store().unwrap().db.execute_batch(
        "CREATE TEMP TRIGGER layout_cancel_write AFTER INSERT ON mappings BEGIN
        SELECT layout_write_started();
        SELECT sum(n) FROM (WITH RECURSIVE x(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM x WHERE n<10000) SELECT n FROM x);
        END;"
    ).unwrap();
    let outer = owner.begin().unwrap();
    let observed = writing.clone();
    let result = with_interrupt_check(
        move || observed.load(Ordering::SeqCst),
        || run(&mut owner, true),
    );
    assert!(writing.load(Ordering::SeqCst));
    assert!(result.unwrap_err().0.contains("interrupt"));
    assert!(
        owner.store().is_err(),
        "aborted lexical scope must remain poisoned"
    );
    owner.finish(outer, false).unwrap();
    assert_eq!(mappings(&owner), 0);
    owner
        .store()
        .unwrap()
        .db
        .execute_batch("DROP TRIGGER layout_cancel_write")
        .unwrap();
    let callbacks = Arc::new(AtomicUsize::new(0));
    let copied = callbacks.clone();
    with_interrupt_check(move || { copied.fetch_add(1, Ordering::SeqCst); false }, || {
        let sum: i64 = owner.store()?.db.query_row(
            "WITH RECURSIVE x(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM x WHERE n<10000) SELECT sum(n) FROM x",
            [], |r| r.get(0),
        )?;
        assert_eq!(sum, 50005000);
        Ok(())
    }).unwrap();
    assert_eq!(
        callbacks.load(Ordering::SeqCst),
        0,
        "poison cleanup must not leak a SQLite handler"
    );

    assert_eq!(run(&mut owner, true).unwrap()["applied"], true);
}
