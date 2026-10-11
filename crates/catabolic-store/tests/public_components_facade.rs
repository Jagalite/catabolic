// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Focused persistent Components / Policies adapter contracts, disposable only.
use catabolic_store::{embedding::Session, fallback_mutation, migration, public_components};
use serde_json::json;

fn owner() -> (tempfile::TempDir, Session) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("db");
    migration::initialize(&path).unwrap();
    let session = Session::open(&path, true, false).unwrap();
    session.store().unwrap().db.execute_batch(
        "INSERT OR IGNORE INTO profiles VALUES ('default');
         INSERT INTO locations VALUES ('media');
         INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','media',1,1,'[]');
         INSERT INTO files(id,location,path) VALUES ('file','media','subtitle.srt');
         INSERT INTO observations(profile,file_id,size,mtime_ns,device,inode,status,scan_id) VALUES ('default','file',1,1,1,1,'present','scan');
         INSERT INTO items(id,kind,metadata) VALUES ('item','movie','{}');
         INSERT INTO item_files(id,file_id,item_id,role,origin) VALUES ('association','file','item','subtitle','explicit');"
    ).unwrap();
    (root, session)
}

#[test]
fn component_revision_uses_existing_identity_and_omits_only_ctime() {
    assert_eq!(
        public_components::revision(&json!({"a":1,"ctime_ns":2})).unwrap(),
        public_components::revision(&json!({"a":1,"ctime_ns":3})).unwrap()
    );
    assert_ne!(
        public_components::revision(&json!({"a":1})).unwrap(),
        public_components::revision(&json!({"a":2})).unwrap()
    );
    assert!(public_components::revision(&json!([])).is_err());
}
#[test]
fn component_adoption_preserves_epoch_and_caller_rollback() {
    let (_root, mut session) = owner();
    let token = session.begin().unwrap();
    let generation: i64 = session
        .store()
        .unwrap()
        .db
        .query_row("SELECT generation FROM fallback_epoch", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        session
            .component_call("default", "backfill", &json!({"token":token}))
            .unwrap(),
        json!(null)
    );
    assert_eq!(
        session
            .store()
            .unwrap()
            .db
            .query_row("SELECT generation FROM fallback_epoch", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        generation
    );
    assert_eq!(
        session
            .store()
            .unwrap()
            .db
            .query_row("SELECT count(*) FROM component_occurrences", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    session.finish(token, false).unwrap();
    assert_eq!(
        session
            .store()
            .unwrap()
            .db
            .query_row("SELECT count(*) FROM component_occurrences", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn component_backfill_checks_expired_and_missing_handle_scope() {
    let (_root, mut session) = owner();
    let token = session.begin().unwrap();
    session.finish(token, true).unwrap();
    assert!(
        session
            .component_call("default", "backfill", &json!({"token":token}))
            .unwrap_err()
            .0
            .contains("no longer active")
    );
    assert!(
        session
            .component_call("default", "backfill", &json!({}))
            .is_err()
    );
    assert!(
        session
            .component_call("default", "backfill", &json!({"token":true}))
            .is_err()
    );
    assert_eq!(
        session
            .store()
            .unwrap()
            .db
            .query_row("SELECT count(*) FROM component_occurrences", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn component_evaluation_shares_budget_and_rejects_foreign_epoch() {
    let (_root, mut session) = owner();
    session
        .component_call("default", "index_file", &json!({"file_id":"file"}))
        .unwrap();
    let id: String = session
        .store()
        .unwrap()
        .db
        .query_row("SELECT id FROM component_occurrences", [], |r| r.get(0))
        .unwrap();
    let token = session
        .query_call("default", "evaluation-create", &json!({"max_bytes":100000}))
        .unwrap()["token"]
        .clone();
    let request =
        json!({"id":id,"session":token,"session_owner_matches":true,"require_current":true});
    assert_eq!(
        session.component_call("default", "get", &request).unwrap()["current"],
        true
    );
    let mut foreign = request.clone();
    foreign["session_owner_matches"] = json!(false);
    assert_eq!(
        session
            .component_call("default", "get", &foreign)
            .unwrap_err()
            .0,
        "evaluation_session_identity_mismatch"
    );
    let tiny = session
        .query_call("default", "evaluation-create", &json!({"max_bytes":1}))
        .unwrap()["token"]
        .clone();
    let mut bounded = request.clone();
    bounded["session"] = tiny;
    assert_eq!(
        session
            .component_call("default", "get", &bounded)
            .unwrap_err()
            .0,
        "evaluation_result_byte_budget"
    );
    session
        .store()
        .unwrap()
        .db
        .execute("UPDATE observations SET size=2", [])
        .unwrap();
    assert_eq!(
        session
            .component_call("default", "get", &request)
            .unwrap_err()
            .0,
        "stale_evaluation_snapshot"
    );
}

#[test]
fn component_live_cancellation_restores_owner_without_charging_budget() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    let (_root, mut session) = owner();
    session
        .component_call("default", "index_file", &json!({"file_id":"file"}))
        .unwrap();
    let id: String = session
        .store()
        .unwrap()
        .db
        .query_row("SELECT id FROM component_occurrences", [], |r| r.get(0))
        .unwrap();
    let token = session
        .query_call("default", "evaluation-create", &json!({"max_bytes":2000}))
        .unwrap()["token"]
        .clone();
    let request = json!({"id":id,"session":token,"session_owner_matches":true});
    let polls = Arc::new(AtomicUsize::new(0));
    let captured = polls.clone();
    let error = catabolic_store::embedding::with_interrupt_check(
        move || captured.fetch_add(1, Ordering::Relaxed) >= 1,
        || session.component_call("default", "get", &request),
    )
    .unwrap_err();
    assert!(matches!(
        error.0.as_str(),
        "evaluation_cancelled" | "evaluation_expansion_cancelled_or_timed_out"
    ));
    assert!(polls.load(Ordering::Relaxed) >= 2);
    assert_eq!(
        session.component_call("default", "get", &request).unwrap()["current"],
        true
    );
    assert_eq!(
        session
            .component_call("default", "get", &request)
            .unwrap_err()
            .0,
        "evaluation_result_byte_budget"
    );
}

#[test]
fn fallback_policy_owner_joins_outer_savepoint() {
    let (_root, mut session) = owner();
    let query = catabolic_store::definitions::dispatch(session.store().unwrap(), "default", "query-put", &json!({"name":"files","value":{"selection":{"language":"sql","query":"SELECT id AS file_id FROM files"}}})).unwrap();
    let outer = session.begin().unwrap();
    let value = json!({"fallbacks":[{"name":"files","query_id":query["id"]}]});
    let policy =
        fallback_mutation::put(session.store().unwrap(), "default", "policy", &value).unwrap();
    assert_eq!(
        policy,
        fallback_mutation::put(session.store().unwrap(), "default", "policy", &value).unwrap()
    );
    assert_eq!(
        fallback_mutation::listing(session.store().unwrap(), "default", 1, "").unwrap()["policies"],
        json!([policy])
    );
    session.finish(outer, false).unwrap();
    assert_eq!(
        fallback_mutation::listing(session.store().unwrap(), "default", 1, "").unwrap()["policies"],
        json!([])
    );
}

#[test]
fn policy_public_listing_preserves_scalar_cursors_and_boolean_limit() {
    let (_root, session) = owner();
    let store = session.store().unwrap();
    for id in ["0-first", "2-next", "a-last"] {
        store
            .db
            .execute(
                "INSERT INTO fallback_policies VALUES (?,'default',?,1,'{}',?)",
                [id, id, id],
            )
            .unwrap();
    }
    let listing = |cursor: serde_json::Value, limit: serde_json::Value| {
        fallback_mutation::execute(
            store,
            "default",
            "list",
            &json!({"after":cursor,"limit":limit}),
        )
        .unwrap()["policies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(listing(json!(null), json!(100)), Vec::<String>::new());
    assert_eq!(listing(json!(true), json!(100)), ["2-next", "a-last"]);
    assert_eq!(listing(json!(1.5), json!(100)), ["2-next", "a-last"]);
    assert_eq!(listing(json!(false), json!(true)), ["0-first"]);
    assert_eq!(
        fallback_mutation::execute(store, "default", "list", &json!({"limit":0,"after":[]}))
            .unwrap_err()
            .0,
        "invalid policy page"
    );
}
