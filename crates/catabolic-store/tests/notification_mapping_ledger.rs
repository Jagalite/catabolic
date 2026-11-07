// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Committed notification/mapped replacement boundaries. Plans here are already
//! prepared inputs: OpenAPI/query approval and live HTTP effects are qualified
//! separately. Every tested owner commits, drops and reopens the real catalog.
use catabolic_store::{Store, http_mappings, migration, notifications};
use notifications::Outcome;
use serde_json::{Value, json};

struct Fixture {
    _root: tempfile::TempDir,
    store: Option<Store>,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        // A local-only immutable operation fixture, never dispatched. The
        // production enqueue/claim/acknowledge owners below are not mocked.
        store.transaction(|db| {
            db.execute("INSERT INTO http_operations(id,profile,name,revision,definition,digest) VALUES ('operation','default','fixture',1,'{}','fixture')", [])?;
            Ok(())
        }).unwrap();
        notifications::configure(
            &store,
            "default",
            &json!({
                "id":"operation:operation", "credential_env":"TEST_URL",
                "subscriptions":["projection_updated"], "apply":true,
            }),
        )
        .unwrap();
        Self {
            _root: root,
            store: Some(store),
        }
    }
    fn store(&self) -> &Store {
        self.store.as_ref().unwrap()
    }
    fn reopen(&mut self) {
        let path = self.store().path.clone();
        drop(self.store.take());
        self.store = Some(Store::open(&path, true, false).unwrap());
    }
    fn snapshot(&self) -> Vec<Vec<Value>> {
        [
            "consumer_events",
            "notification_deliveries",
            "http_mapping_definitions",
            "http_mapping_deliveries",
            "http_mapping_ledger",
        ]
        .iter()
        .map(|table| {
            migration::rows(
                &self.store().db,
                &format!("SELECT * FROM {table} ORDER BY rowid"),
                &[],
            )
            .unwrap()
        })
        .collect()
    }
    fn row(&self, key: &str, revision: u8) -> Value {
        let previous = migration::rows(
            &self.store().db,
            "SELECT key,request_digest,event_id FROM http_mapping_ledger WHERE key=?",
            &[key.to_owned().into()],
        )
        .unwrap()
        .into_iter()
        .next()
        .unwrap_or(Value::Null);
        json!({"key":key, "request":{"revision":revision}, "request_digest":format!("{key}-{revision}"),
            "previous":previous, "changed":true})
    }
    fn enqueue(&mut self, rows: Vec<Value>, id: &str) -> catabolic_store::Result<Value> {
        let plan = json!({"plan_id":id, "definition":{"id":"mapping","operation":"operation"},"rows":rows});
        let result = http_mappings::enqueue(self.store(), "default", &plan, id, 2);
        self.reopen();
        result
    }
}

#[test]
fn live_lease_refuses_entire_replacement_batch_then_expiry_fences_old_ack() {
    let mut f = Fixture::new();
    f.enqueue(vec![f.row("a", 0), f.row("b", 0)], "initial")
        .unwrap();
    let now = notifications::now();
    let lease = notifications::claim(f.store(), "default", None, now)
        .unwrap()
        .unwrap();
    f.reopen();
    let key: String = f
        .store()
        .db
        .query_row(
            "SELECT key FROM http_mapping_ledger WHERE event_id=?",
            [&lease.event_id],
            |r| r.get(0),
        )
        .unwrap();
    let other = if key == "a" { "b" } else { "a" };
    let before = f.snapshot();
    // The unleased row mutates first; refusal of the second must roll it back.
    let error = f
        .enqueue(vec![f.row(other, 1), f.row(&key, 1)], "blocked")
        .unwrap_err();
    assert!(error.0.contains("delivery_in_progress"));
    assert_eq!(f.snapshot(), before);

    // Expiry is supplied as durable clock evidence, not a sleep-sensitive race.
    f.store()
        .transaction(|db| {
            db.execute(
                "UPDATE notification_deliveries SET lease_until=0 WHERE event_id=?",
                [&lease.event_id],
            )?;
            Ok(())
        })
        .unwrap();
    f.reopen();
    f.enqueue(vec![f.row(other, 1), f.row(&key, 1)], "replacement")
        .unwrap();
    let committed = f.snapshot();
    assert_eq!(
        notifications::acknowledge(f.store(), &lease, Outcome::Complete, now + 1.0).unwrap()["state"],
        "stale_acknowledgement"
    );
    f.reopen();
    assert_eq!(f.snapshot(), committed);
    let states = migration::rows(
        &f.store().db,
        "SELECT state,count(*) AS n FROM notification_deliveries GROUP BY state ORDER BY state",
        &[],
    )
    .unwrap();
    assert_eq!(
        states,
        vec![
            json!({"state":"cancelled","n":2}),
            json!({"state":"pending","n":2})
        ]
    );
    assert_eq!(
        f.store()
            .db
            .query_row("SELECT count(*) FROM http_mapping_ledger", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn committed_retry_chain_exhausts_at_five_and_rejects_each_previous_token() {
    let mut f = Fixture::new();
    f.enqueue(vec![f.row("a", 0)], "initial").unwrap();
    let mut now = 100.0;
    let mut old = None;
    for attempt in 1..=5 {
        let lease = notifications::claim(f.store(), "default", None, now)
            .unwrap()
            .unwrap();
        f.reopen();
        assert_eq!(lease.previous_attempts, attempt - 1);
        if let Some(previous) = &old {
            let before = f.snapshot();
            assert_eq!(
                notifications::acknowledge(f.store(), previous, Outcome::Complete, now).unwrap()["state"],
                "stale_acknowledgement"
            );
            f.reopen();
            assert_eq!(f.snapshot(), before);
        }
        let result =
            notifications::acknowledge(f.store(), &lease, Outcome::TemporarilyFailed, now + 1.0)
                .unwrap();
        f.reopen();
        let expected = if attempt == 5 { "exhausted" } else { "retry" };
        assert_eq!(result["state"], expected);
        let row = migration::rows(
            &f.store().db,
            "SELECT state,attempts,due_at,lease_token,lease_until FROM notification_deliveries",
            &[],
        )
        .unwrap()
        .remove(0);
        assert_eq!(row["state"], expected);
        assert_eq!(row["attempts"], attempt);
        assert!(row["lease_token"].is_null() && row["lease_until"].is_null());
        let delay = [5.0, 10.0, 20.0, 40.0, 80.0][(attempt - 1) as usize];
        now += 1.0 + delay;
        assert_eq!(row["due_at"], now);
        assert!(
            notifications::claim(f.store(), "default", None, now - 0.5)
                .unwrap()
                .is_none()
        );
        f.reopen();
        old = Some(lease);
    }
    assert!(
        notifications::claim(f.store(), "default", None, now + 1000.0)
            .unwrap()
            .is_none()
    );
    f.reopen();
    assert_eq!(
        f.store()
            .db
            .query_row("SELECT attempts FROM notification_deliveries", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        5
    );
}
