// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Ordinary adapter contracts on disposable metadata-only catalogs.
use catabolic_runtime::public_watchers as public;
use catabolic_store::{definitions, embedding::Session, migration, watcher};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct Fixture {
    _root: tempfile::TempDir,
    session: Session,
}
fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("db");
    migration::initialize(&path).unwrap();
    let session = Session::open(&path, true, false).unwrap();
    session.store().unwrap().db.execute_batch("INSERT OR IGNORE INTO profiles VALUES('default'); INSERT INTO profiles VALUES('other');").unwrap();
    Fixture {
        _root: root,
        session,
    }
}
fn define(f: &mut Fixture, name: &str, reaction: &str) -> Value {
    let query = definitions::query_put(
        f.session.store().unwrap(),
        "default",
        name,
        &json!({"selection":{"language":"sql","query":"SELECT id AS item_id FROM items"}}),
    )
    .unwrap();
    public::call(&mut f.session, "default", "put", &json!({"name":name,"definition":{"plan":{"kind":"query","query_id":query["id"]},"reaction":reaction}})).unwrap()
}
#[test]
fn borrowed_run_sees_uncommitted_values_and_rolls_back_with_owner() {
    let mut f = fixture();
    let outer = f.session.begin().unwrap();
    define(&mut f, "one", "event");
    f.session
        .store()
        .unwrap()
        .db
        .execute(
            "INSERT INTO items(id,kind,metadata) VALUES('new','movie','{}')",
            [],
        )
        .unwrap();
    let first = public::call(&mut f.session, "default", "run", &json!({"name":"one"})).unwrap();
    assert_eq!(first["complete"], true, "{first}");
    assert_eq!(
        first["evaluation"]["baseline"]["value"]["ids"],
        json!(["new"])
    );
    let second = public::call(&mut f.session, "default", "run", &json!({"name":"one"})).unwrap();
    assert_eq!(second["complete"], true, "{second}");
    assert_eq!(second["evaluation"]["changed"], false);
    assert!(!f.session.store().unwrap().db.is_autocommit());
    f.session.finish(outer, false).unwrap();
    assert_eq!(
        public::call(&mut f.session, "default", "list", &json!({})).unwrap(),
        json!([])
    );
    assert_eq!(
        f.session
            .store()
            .unwrap()
            .db
            .query_row("SELECT count(*) FROM watcher_runs", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn original_runtime_wrapper_and_borrowed_owner_share_query_lifecycle() {
    let mut f = fixture();
    define(&mut f, "normal", "report");
    define(&mut f, "borrowed", "report");
    let normal = catabolic_runtime::watchers::execute(
        f.session.store_mut().unwrap(),
        "default",
        "run",
        &json!({"name":"normal"}),
    )
    .unwrap();
    let borrowed = public::call(
        &mut f.session,
        "default",
        "run",
        &json!({"name":"borrowed"}),
    )
    .unwrap();
    for field in ["complete", "evaluation", "observations", "reaction"] {
        assert_eq!(
            normal[field], borrowed[field],
            "{field}: {normal} / {borrowed}"
        );
    }
    assert_eq!(normal["complete"], true);
    assert_eq!(
        watcher::execute(
            f.session.store().unwrap(),
            "default",
            "history",
            &json!({"name":"normal"})
        )
        .unwrap()[0]["state"],
        "complete"
    );
}
#[test]
fn invalidation_preserves_nested_transaction_and_profile() {
    let mut f = fixture();
    let outer = f.session.begin().unwrap();
    assert_eq!(
        public::call(
            &mut f.session,
            "default",
            "invalidate",
            &json!({"source":"unbound"})
        )
        .unwrap(),
        json!({"source":"unbound","reason":"event","coverage":"full_source"})
    );
    let inner = f.session.begin().unwrap();
    public::call(
        &mut f.session,
        "other",
        "invalidate",
        &json!({"source":"unbound"}),
    )
    .unwrap();
    f.session.finish(inner, false).unwrap();
    assert_eq!(
        f.session
            .store()
            .unwrap()
            .db
            .query_row("SELECT count(*) FROM observation_sources", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    f.session.finish(outer, false).unwrap();
    assert_eq!(
        f.session
            .store()
            .unwrap()
            .db
            .query_row("SELECT count(*) FROM observation_sources", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn unsupported_run_refuses_before_admission_and_ignores_fixture_options() {
    let mut f = fixture();
    define(&mut f, "one", "report");
    // Metadata fixture changes only the reaction to reach the refusal boundary;
    // no output, policy, source binding or processing service is configured.
    f.session.store().unwrap().db.execute("UPDATE watcher_definitions SET definition=json_set(definition,'$.reaction','projection')", []).unwrap();
    let error = public::call(
        &mut f.session,
        "default",
        "run",
        &json!({"name":"one","disposable_fixture":true}),
    )
    .unwrap_err();
    assert!(
        error.0.contains("only disposable debug fixtures"),
        "{error}"
    );
    assert_eq!(
        f.session
            .store()
            .unwrap()
            .db
            .query_row("SELECT count(*) FROM watcher_runs", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    f.session
        .store()
        .unwrap()
        .db
        .execute(
            "UPDATE watcher_definitions SET definition=json_set(definition,'$.reaction','http')",
            [],
        )
        .unwrap();
    assert!(
        public::call(&mut f.session, "default", "run", &json!({"name":"one"}))
            .unwrap_err()
            .0
            .contains("borrowed runtime owner")
    );
    assert_eq!(
        f.session
            .store()
            .unwrap()
            .db
            .query_row("SELECT count(*) FROM watcher_runs", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

fn cancellation_at_completion(borrowed: bool, inline: bool) {
    let mut f = fixture();
    let before = define(&mut f, "one", "report");
    let cancelled = Arc::new(AtomicBool::new(false));
    let requested = cancelled.clone();
    f.session
        .store()
        .unwrap()
        .db
        .create_scalar_function(
            "cancel_at_completion",
            0,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            move |_| {
                requested.store(true, Ordering::Release);
                Ok(1)
            },
        )
        .unwrap();
    // The original runtime opens a fresh reader, whose schema check must not
    // see fixture-only objects. The borrowed query rebuilds temporary objects.
    let temporary = if borrowed { "" } else { "TEMP " };
    f.session.store().unwrap().db.execute_batch(&format!(
        "CREATE {temporary}TRIGGER cancel_completion BEFORE UPDATE OF baseline ON watchers BEGIN
         SELECT cancel_at_completion();
         SELECT (WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<1000000) SELECT sum(x) FROM n);
         END"
    )).unwrap();
    let outer = inline.then(|| f.session.begin().unwrap());
    let checked = cancelled.clone();
    let result = catabolic_store::embedding::with_interrupt_check(
        move || checked.load(Ordering::Acquire),
        || {
            if borrowed {
                public::call(&mut f.session, "default", "run", &json!({"name":"one"}))
            } else {
                // The original Store-facing writer never lends its connection
                // to query evaluation. Its existing handler stays installed.
                catabolic_store::public_watchers::resume_interrupts(f.session.store().unwrap())?;
                catabolic_runtime::watchers::execute(
                    f.session.store_mut().unwrap(),
                    "default",
                    "run",
                    &json!({"name":"one"}),
                )
            }
        },
    );
    assert!(
        cancelled.load(Ordering::Acquire),
        "borrowed={borrowed} inline={inline}: {result:?}"
    );
    assert_eq!(result.as_ref().unwrap()["complete"], false, "{result:?}");
    if let Some(token) = outer {
        // SQLite interrupted a write, so the caller's transaction was aborted;
        // the Session preserves its lexical token for ordinary cleanup.
        assert!(f.session.store().is_err());
        f.session.finish(token, false).unwrap();
    }
    let row = public::call(&mut f.session, "default", "get", &json!({"name":"one"})).unwrap();
    assert_eq!(row["baseline"], Value::Null);
    assert_eq!(row["active_run"], Value::Null);
    assert_eq!(row["completed_generation"], before["completed_generation"]);
    let history =
        public::call(&mut f.session, "default", "history", &json!({"name":"one"})).unwrap();
    if inline {
        assert_eq!(history, json!([]));
        assert_eq!(row, before);
    } else {
        assert_eq!(history.as_array().unwrap().len(), 1);
        assert_eq!(history[0]["state"], "failed");
    }
    f.session
        .store()
        .unwrap()
        .db
        .execute_batch("DROP TRIGGER cancel_completion")
        .unwrap();
    assert_eq!(
        public::call(&mut f.session, "default", "run", &json!({"name":"one"})).unwrap()["complete"],
        true
    );
}

#[test]
fn borrowed_completion_restores_cancellation_on_idle_and_transaction_owners() {
    for inline in [false, true] {
        cancellation_at_completion(true, inline);
    }
}

#[test]
fn original_store_completion_keeps_its_cancellation_behavior() {
    for inline in [false, true] {
        cancellation_at_completion(false, inline);
    }
}
