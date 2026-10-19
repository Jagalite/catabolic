// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! First-use query setup preserves existing native owner state without warming it.
use catabolic_store::{Store, connect, embedding, migration, sql};
use rusqlite::limits::Limit;
use serde_json::json;

fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("query-setup.db");
    migration::initialize(&path).unwrap();
    (root, path)
}

fn unchanged(
    owner: &embedding::Session,
    temp_store: i32,
    query_only: i32,
    transaction: bool,
    changes: u64,
) {
    let db = &owner.store().unwrap().db;
    assert_eq!(owner.in_transaction().unwrap(), transaction);
    assert_eq!(db.total_changes(), changes);
    for (name, expected) in [
        ("temp_store", temp_store),
        ("query_only", query_only),
        ("busy_timeout", 1777),
    ] {
        assert_eq!(
            db.pragma_query_value(None, name, |r| r.get::<_, i32>(0))
                .unwrap(),
            expected,
            "{name}"
        );
    }
    assert_eq!(db.limit(Limit::SQLITE_LIMIT_COLUMN).unwrap(), 192);
    assert_eq!(
        db.query_row("SELECT value FROM owner_marker_view", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        73
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM temp.sqlite_schema WHERE name IN ('owner_marker','owner_marker_view','owner_marker_guard')",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap(),
        3
    );
}

fn success(owner: &mut embedding::Session, language: &str) {
    if language == "graphql" {
        assert_eq!(
            owner
                .query_call("default", language, &json!({"document":"{profile}"}))
                .unwrap(),
            json!({"data":{"profile":"default"},"extensions":{"interfaceVersion":1,"profile":"default","records":0}})
        );
    } else {
        assert_eq!(
            owner
                .query_call(
                    "default",
                    language,
                    &json!({"sql":"SELECT value FROM owner_marker_view ORDER BY value"}),
                )
                .unwrap()["rows"],
            json!([[73]])
        );
    }
}

#[test]
fn first_queries_preserve_caller_temp_state_storage_and_transaction() {
    let (_root, path) = fixture();
    for temp_store in [0, 1, 2] {
        for query_only in [0, 1] {
            for transaction in [false, true] {
                for first in ["graphql", "sql"] {
                    let mut owner = embedding::Session::open(&path, true, false).unwrap();
                    let db = &owner.store().unwrap().db;
                    db.pragma_update(None, "temp_store", temp_store).unwrap();
                    db.pragma_update(None, "busy_timeout", 1777).unwrap();
                    db.set_limit(Limit::SQLITE_LIMIT_COLUMN, 192).unwrap();
                    if transaction {
                        db.execute_batch("BEGIN").unwrap();
                    }
                    db.execute_batch(
                        "CREATE TEMP TABLE owner_marker(value); INSERT INTO owner_marker VALUES(73);
                         CREATE TEMP VIEW owner_marker_view AS SELECT value FROM owner_marker;
                         CREATE TEMP TRIGGER owner_marker_guard BEFORE INSERT ON owner_marker
                         WHEN NEW.value<0 BEGIN SELECT RAISE(ABORT,'owner marker guard'); END;",
                    )
                    .unwrap();
                    db.pragma_update(None, "query_only", query_only).unwrap();
                    let changes = db.total_changes();
                    // Exact diagnostic from installed qualification, before setup has run.
                    assert_eq!(
                        owner
                            .query_call("default", "graphql", &json!({"document":"{items()}"}))
                            .unwrap(),
                        json!({"data":null,"errors":[{"message":"Syntax Error: Expected Name, found ')'.","locations":[{"line":1,"column":8}]}]})
                    );
                    unchanged(&owner, temp_store, query_only, transaction, changes);
                    success(&mut owner, first);
                    unchanged(&owner, temp_store, query_only, transaction, changes);
                    // Both engines use the same owner after first initialization.
                    success(&mut owner, "graphql");
                    success(&mut owner, "sql");
                    let rejected = owner.query_call(
                        "default",
                        "sql",
                        &json!({"sql":"INSERT INTO owner_marker VALUES(99)"}),
                    );
                    assert_eq!(
                        rejected.unwrap_err().0,
                        "read-only SQL rejected an operation or function: owner_marker"
                    );
                    unchanged(&owner, temp_store, query_only, transaction, changes);
                    let bounded = owner
                        .query_call(
                            "default",
                            "sql",
                            &json!({"sql":"WITH n(x) AS (VALUES(1),(2)) SELECT x FROM n ORDER BY x","max_rows":1}),
                        )
                        .unwrap();
                    assert_eq!(bounded["rows"], json!([[1]]));
                    assert_eq!(bounded["truncation_reason"], "max_rows");
                    assert_eq!(bounded["complete"], false);
                    success(&mut owner, "graphql");
                    unchanged(&owner, temp_store, query_only, transaction, changes);
                    let db = &owner.store().unwrap().db;
                    db.pragma_update(None, "query_only", false).unwrap();
                    assert_eq!(
                        db.execute("INSERT INTO owner_marker VALUES(-1)", [])
                            .unwrap_err()
                            .to_string(),
                        "owner marker guard"
                    );
                    db.execute("INSERT INTO owner_marker VALUES(79)", [])
                        .unwrap();
                    if transaction {
                        db.execute_batch("ROLLBACK").unwrap();
                    }
                }
            }
        }
    }
}

#[test]
fn rolled_back_first_setup_is_recreated_on_the_same_owner() {
    let (_root, path) = fixture();
    for temp_store in [0, 1, 2] {
        let mut owner = embedding::Session::open(&path, true, false).unwrap();
        owner
            .store()
            .unwrap()
            .db
            .pragma_update(None, "temp_store", temp_store)
            .unwrap();
        owner.store().unwrap().db.execute_batch(
            "BEGIN; CREATE TEMP TABLE rollback_marker(value); INSERT INTO rollback_marker VALUES(73)",
        ).unwrap();
        success(&mut owner, "graphql");
        owner.store().unwrap().db.execute_batch("ROLLBACK").unwrap();
        assert!(!owner.in_transaction().unwrap());
        assert_eq!(
            owner
                .store()
                .unwrap()
                .db
                .query_row("SELECT count(*) FROM temp.sqlite_schema", [], |r| r
                    .get::<_, i64>(0),)
                .unwrap(),
            0
        );
        assert_eq!(
            owner
                .query_call(
                    "default",
                    "sql",
                    &json!({"sql":"SELECT count(*) FROM catalog_items"})
                )
                .unwrap()["rows"],
            json!([[0]])
        );
        success(&mut owner, "graphql");
        assert_eq!(
            owner
                .store()
                .unwrap()
                .db
                .pragma_query_value(None, "temp_store", |r| r.get::<_, i32>(0))
                .unwrap(),
            temp_store
        );
        assert!(!owner.in_transaction().unwrap());
    }
}

#[test]
fn new_connections_keep_memory_default_and_readonly_queries_keep_their_snapshot() {
    let (_root, path) = fixture();
    let native_path = path.with_file_name("native-query-setup.db");
    migration::initialize_native(&native_path).unwrap();
    for path in [&path, &native_path] {
        for writable in [false, true] {
            let db = connect(path, writable).unwrap();
            assert_eq!(
                db.pragma_query_value(None, "temp_store", |r| r.get::<_, i32>(0))
                    .unwrap(),
                2
            );
            // Native Store admission closes and reopens its initial connection.
            let store = Store::open(path, writable, false).unwrap();
            assert_eq!(
                store
                    .db
                    .pragma_query_value(None, "temp_store", |r| r.get::<_, i32>(0))
                    .unwrap(),
                2
            );
        }
        for chosen in [0, 1] {
            let mut owner = embedding::Session::open(path, true, false).unwrap();
            owner
                .store()
                .unwrap()
                .db
                .pragma_update(None, "temp_store", chosen)
                .unwrap();
            owner.detach().unwrap();
            owner.reconnect().unwrap();
            assert_eq!(
                owner
                    .store()
                    .unwrap()
                    .db
                    .pragma_query_value(None, "temp_store", |r| r.get::<_, i32>(0))
                    .unwrap(),
                2
            );
            success(&mut owner, "graphql");
        }
    }
    let mut owner = embedding::Session::open(&path, false, false).unwrap();
    assert!(owner.in_transaction().unwrap());
    owner
        .store()
        .unwrap()
        .db
        .execute_batch(
            "CREATE TEMP TABLE owner_marker(value); INSERT INTO owner_marker VALUES(73);
         CREATE TEMP VIEW owner_marker_view AS SELECT value FROM owner_marker",
        )
        .unwrap();
    let changes = owner.store().unwrap().db.total_changes();
    success(&mut owner, "graphql");
    success(&mut owner, "sql");
    assert!(owner.in_transaction().unwrap());
    assert_eq!(owner.store().unwrap().db.total_changes(), changes);
    assert!(
        owner
            .store()
            .unwrap()
            .db
            .execute("INSERT INTO profiles VALUES('readonly-no')", [])
            .is_err()
    );
    assert_eq!(
        sql::execute(
            &path,
            Some("SELECT count(*) FROM catalog_items"),
            &sql::Options::default()
        )
        .unwrap()["rows"],
        json!([[0]])
    );
    assert_eq!(
        sql::execute(
            &path,
            Some("DELETE FROM profiles"),
            &sql::Options::default()
        )
        .unwrap_err()
        .0,
        "read-only SQL rejected an operation or function: profiles"
    );
}

#[test]
fn direct_setup_preserves_storage_of_caller_supplied_connections() {
    for temp_store in [0, 1, 2] {
        for transaction in [false, true] {
            let db = rusqlite::Connection::open_in_memory().unwrap();
            db.pragma_update(None, "temp_store", temp_store).unwrap();
            if transaction {
                db.execute_batch("BEGIN").unwrap();
            }
            db.execute_batch(
                "CREATE TEMP TABLE direct_marker(value); INSERT INTO direct_marker VALUES(73)",
            )
            .unwrap();
            let changes = db.total_changes();
            sql::setup(&db).unwrap();
            assert_eq!(db.is_autocommit(), !transaction);
            assert_eq!(db.total_changes(), changes);
            assert_eq!(
                db.pragma_query_value(None, "temp_store", |r| r.get::<_, i32>(0))
                    .unwrap(),
                temp_store
            );
            assert_eq!(
                db.query_row("SELECT value FROM direct_marker", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                73
            );
            assert!(
                db.pragma_query_value(None, "query_only", |r| r.get::<_, bool>(0))
                    .unwrap()
            );
        }
    }
}
