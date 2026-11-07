// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Exact health key lookup and restart qualification; no schema modifications.
use catabolic_store::{
    Store,
    fallback_resolution::{self, Context, Resolver},
    migration,
};
use serde_json::{Value, json};

fn health_result(
    store: &Store,
    profile: &str,
    catalog: &str,
    entry: &str,
    file: &str,
    revision: &str,
) -> catabolic_store::Result<Value> {
    let mut probe = |_: &std::path::Path,
                     _: &str,
                     _: &str,
                     _: &Value,
                     _: u64|
     -> catabolic_store::Result<Value> {
        panic!("health lookup must not perform source IO")
    };
    Resolver::new(Context::Borrowed(store, profile), &mut probe, None).health(
        entry,
        &json!({"file_id":file,"revision":revision}),
        catalog,
    )
}
fn health(
    store: &Store,
    profile: &str,
    catalog: &str,
    entry: &str,
    file: &str,
    revision: &str,
) -> Value {
    health_result(store, profile, catalog, entry, file, revision).unwrap()
}

#[test]
fn exact_candidate_revision_health_survives_reopen_without_cross_identity_credit() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    migration::initialize(&path).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    store
        .transaction(|db| {
            for (profile, catalog, entry, file, count) in [
                ("default", "global", "entry", "a", 2),
                ("default", "global", "entry", "b", 7),
                ("other", "global", "entry", "a", 11),
                ("default", "other", "entry", "a", 13),
                ("default", "global", "other", "a", 17),
            ] {
                db.execute(
                    "INSERT INTO fallback_health VALUES(?,?,?,?, 'rev1',100,?,120)",
                    rusqlite::params![profile, catalog, entry, file, count],
                )?;
            }
            Ok(())
        })
        .unwrap();
    drop(store);
    for _ in 0..2 {
        let store = Store::open(&path, true, false).unwrap();
        let a = health(&store, "default", "global", "entry", "a", "rev1");
        assert_eq!(a["successes"], 3);
        assert_eq!(a["healthy_since"], 100.0);
        let b = health(&store, "default", "global", "entry", "b", "rev1");
        assert_eq!(b["successes"], 8);
        let revised = health(&store, "default", "global", "entry", "a", "rev2");
        assert_eq!(revised["successes"], 1);
        assert_eq!(revised["healthy_since"], revised["last_check"]);
        let absent = health(&store, "default", "global", "missing", "a", "rev1");
        assert_eq!(absent["successes"], 1);
        assert_eq!(store.db.query_row("SELECT successes FROM fallback_health WHERE profile='default' AND catalog='global' AND entry_id='entry' AND file_id='a'",[],|r|r.get::<_,i64>(0)).unwrap(),2);
    }
}
#[test]
fn changed_health_epoch_is_stale_and_missing_history_restarts_at_one() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    migration::initialize(&path).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    store.transaction(|db|{
        db.execute("INSERT INTO fallback_health VALUES('default','global','entry','a','rev1',100,2,120)",[])?;Ok(())
    }).unwrap();
    let epoch = fallback_resolution::epoch(&store).unwrap();
    let captured = health(&store, "default", "global", "entry", "a", "rev1");
    assert_eq!(captured["successes"], 3);
    store
        .transaction(|db| {
            db.execute("DELETE FROM fallback_health", [])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        fallback_resolution::require_epoch(&store, epoch)
            .unwrap_err()
            .0,
        "stale_resolution_plan"
    );
    drop(store);
    let store = Store::open(&path, true, false).unwrap();
    let fresh = health(&store, "default", "global", "entry", "a", "rev1");
    assert_eq!(fresh["successes"], 1);
    assert_eq!(fresh["healthy_since"], fresh["last_check"]);
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM fallback_health", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

fn durable_health_state(store: &Store) -> Value {
    json!({
        "health": migration::rows(&store.db, "SELECT * FROM fallback_health ORDER BY profile,catalog,entry_id,file_id", &[]).unwrap(),
        "epoch": migration::rows(&store.db, "SELECT * FROM fallback_epoch ORDER BY id", &[]).unwrap(),
        "publication": migration::rows(&store.db, "SELECT * FROM publication_generations ORDER BY profile,catalog", &[]).unwrap(),
        "bindings": migration::rows(&store.db, "SELECT * FROM fallback_bindings ORDER BY profile,catalog", &[]).unwrap(),
    })
}

// Valid finite timestamp/nonnegative-count histories retain the prior arithmetic
// and exact identity semantics (including duplicate/backward clocks in core).
// Malformed stored values are an explicit fail-closed compatibility change:
// they now error rather than propagate malformed times, reset noninteger counts,
// or panic/wrap at overflow. They must never be silently repaired or committed.
#[test]
fn malformed_or_overflowing_health_is_not_reset_and_errors_preserve_transactions() {
    use rusqlite::types::Value as SqlValue;
    for (column, value, expected) in [
        (
            "healthy_since",
            SqlValue::Text("invalid".into()),
            "invalid_fallback_health",
        ),
        (
            "last_check",
            SqlValue::Text("invalid".into()),
            "invalid_fallback_health",
        ),
        (
            "healthy_since",
            SqlValue::Real(f64::INFINITY),
            "invalid_fallback_health",
        ),
        (
            "last_check",
            SqlValue::Real(f64::NEG_INFINITY),
            "invalid_fallback_health",
        ),
        (
            "successes",
            SqlValue::Integer(-1),
            "invalid_fallback_health",
        ),
        (
            "successes",
            SqlValue::Text("invalid".into()),
            "invalid_fallback_health",
        ),
        ("successes", SqlValue::Real(1.5), "invalid_fallback_health"),
        (
            "successes",
            SqlValue::Integer(i64::MAX),
            "fallback_health_overflow",
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        store.transaction(|db| {
            db.execute("INSERT INTO fallback_health VALUES('default','global','entry','bad','rev1',100,2,120)", [])?;
            db.execute("INSERT INTO fallback_health VALUES('default','global','entry','good','rev1',100,4,120)", [])?;
            db.execute("INSERT INTO publication_generations(profile,catalog,local_binding,generation,verified) VALUES('default','global','fixture',3,1)", [])?;
            // Column names are a closed fixture list, never external input.
            db.execute(&format!("UPDATE fallback_health SET {column}=? WHERE file_id='bad'"), [value])?;
            Ok(())
        }).unwrap();
        drop(store);
        let store = Store::open(&path, true, false).unwrap();
        let before = durable_health_state(&store);
        let error = health_result(&store, "default", "global", "entry", "bad", "rev1").unwrap_err();
        assert_eq!(error.0, expected, "{column}");
        assert_eq!(
            durable_health_state(&store),
            before,
            "read-only health preview mutated rows"
        );
        // Resolver::health is read-only; this separately proves propagation of
        // its error through the real Store transaction owner rolls back earlier
        // writes and their epoch/generation triggers. It is not a projection-
        // admission crash test: actual plan construction fails before admission.
        let result = store.transaction(|db| {
            db.execute(
                "UPDATE fallback_health SET successes=successes+1 WHERE file_id='good'",
                [],
            )?;
            db.execute(
                "UPDATE publication_generations SET generation=generation+1,verified=0",
                [],
            )?;
            health_result(&store, "default", "global", "entry", "bad", "rev1")
        });
        assert_eq!(result.unwrap_err().0, expected, "{column}");
        assert_eq!(durable_health_state(&store), before);
        drop(store);
        let store = Store::open(&path, true, false).unwrap();
        assert_eq!(
            durable_health_state(&store),
            before,
            "{column} changed durable health after reopen"
        );
        assert_eq!(
            health_result(&store, "default", "global", "entry", "bad", "rev1")
                .unwrap_err()
                .0,
            expected
        );
        assert_eq!(
            health(&store, "default", "global", "entry", "good", "rev1")["successes"],
            5
        );
    }
}
