// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Narrow ordinary fallback adapter/lending contracts, on disposable catalogs.
use catabolic_runtime::public_fallback as runtime;
use catabolic_store::{
    definitions, embedding::Session, fallback_mutation, migration, public_fallback,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct Fixture {
    _root: tempfile::TempDir,
    session: Session,
    policy: Value,
    live: Value,
    members: Value,
}
fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("db");
    migration::initialize(&path).unwrap();
    let session = Session::open(&path, true, false).unwrap();
    let store = session.store().unwrap();
    store.db.execute_batch("INSERT OR IGNORE INTO profiles VALUES ('default'); INSERT INTO profiles VALUES ('alternate'); INSERT INTO locations VALUES ('media'); INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','media',1,1,'[]'); INSERT INTO files(id,location,path) VALUES ('file','media','video.mkv'); INSERT INTO observations(profile,file_id,size,mtime_ns,device,inode,status,scan_id) VALUES ('default','file',10,20,30,40,'present','scan'); INSERT INTO items(id,kind,metadata) VALUES ('item','movie','{}'); INSERT INTO item_files(id,file_id,item_id,role,origin,metadata) VALUES ('association','file','item','primary','explicit','{}'); INSERT INTO meta VALUES ('layout:flat','{\"version\":1,\"rules\":[{\"name\":\"all\",\"path\":\"{item.kind}/{item.id}/{file.name}\"}]}');").unwrap();
    let query = |name, sql| {
        definitions::query_put(
            store,
            "default",
            name,
            &json!({"selection":{"language":"sql","query":sql}}),
        )
        .unwrap()["id"]
            .clone()
    };
    let empty = query("empty", "SELECT id AS file_id FROM files WHERE 0");
    let all = query("all", "SELECT id AS file_id FROM files");
    let members = query("members", "SELECT id AS item_id FROM items");
    let policy = fallback_mutation::put(
        store,
        "default",
        "empty",
        &json!({"fallbacks":[{"name":"empty","query_id":empty}]}),
    )
    .unwrap();
    let live = fallback_mutation::put(
        store,
        "default",
        "live",
        &json!({"fallbacks":[{"name":"all","query_id":all}],"budgets":{"max_checks":1}}),
    )
    .unwrap();
    Fixture {
        _root: root,
        session,
        policy,
        live,
        members,
    }
}
fn bind(f: &mut Fixture) {
    runtime::call(&mut f.session,"default","projection-bind",&json!({"catalog":"global","policy_id":f.policy["id"],"query_id":f.members,"layout":"flat"})).unwrap();
}
fn evaluation(f: &mut Fixture, bytes: usize) -> Value {
    f.session
        .query_call(
            "default",
            "evaluation-create",
            &json!({"timeout_ms":60000,"max_ids":10000,"max_bytes":bytes}),
        )
        .unwrap()["token"]
        .clone()
}
fn request(f: &Fixture) -> Value {
    json!({"entries":[{"item_id":"item"}],"policy_id":f.policy["id"],"primary_choices":{},"total_checks":0,"catalog":"global"})
}
#[test]
fn resolver_lends_original_writer_and_retains_cumulative_checks() {
    let mut f = fixture();
    let token = evaluation(&mut f, 1000000);
    let mut args = request(&f);
    args["policy_id"] = f.live["id"].clone();
    args["session"] = token;
    let checks = Arc::new(AtomicUsize::new(0));
    let counted = checks.clone();
    let mut probe = move |_: &std::path::Path, _: &str, _: &str, _: &Value, _: u64| {
        counted.fetch_add(1, Ordering::Relaxed);
        Ok(json!({"usable":true}))
    };
    let first = f
        .session
        .with_evaluation("default", &args, 60000, 10000, |e| {
            public_fallback::resolve(e, &args, &mut probe)
        })
        .unwrap();
    assert_eq!(
        first["result"]["decisions"][0]["state"],
        "resolved_preferred"
    );
    assert_eq!(first["total_checks"], 1);
    args["total_checks"] = first["total_checks"].clone();
    let second = f
        .session
        .with_evaluation("default", &args, 60000, 10000, |e| {
            public_fallback::resolve(e, &args, &mut probe)
        })
        .unwrap();
    assert_eq!(checks.load(Ordering::Relaxed), 1);
    assert_eq!(second["result"]["decisions"][0]["state"], "blocked");
    assert!(f.session.store().unwrap().owns_writer_lock());
}
#[test]
fn resolver_refuses_active_transaction_and_returns_owner() {
    let mut f = fixture();
    let args = request(&f);
    let scope = f.session.begin().unwrap();
    assert!(
        runtime::call(&mut f.session, "default", "resolver-resolve", &args)
            .unwrap_err()
            .0
            .contains("outside a transaction")
    );
    assert!(!f.session.store().unwrap().db.is_autocommit());
    f.session.finish(scope, false).unwrap();
    assert_eq!(
        runtime::call(&mut f.session, "default", "resolver-resolve", &args).unwrap()["result"]["decisions"]
            [0]["state"],
        "unresolved"
    );
}
#[test]
fn evaluation_rejects_foreign_profile_epoch_drop_and_cancellation() {
    let mut f = fixture();
    let token = evaluation(&mut f, 100000);
    let mut args = request(&f);
    args["session"] = token.clone();
    args["session_owner_matches"] = json!(false);
    assert_eq!(
        runtime::call(&mut f.session, "default", "resolver-resolve", &args)
            .unwrap_err()
            .0,
        "evaluation_session_identity_mismatch"
    );
    args["session_owner_matches"] = json!(true);
    args["cancelled"] = json!(true);
    assert_eq!(
        runtime::call(&mut f.session, "default", "resolver-resolve", &args)
            .unwrap_err()
            .0,
        "evaluation_cancelled"
    );
    args["cancelled"] = json!(false);
    f.session
        .store()
        .unwrap()
        .db
        .execute("UPDATE observations SET size=11", [])
        .unwrap();
    assert_eq!(
        runtime::call(&mut f.session, "default", "resolver-resolve", &args)
            .unwrap_err()
            .0,
        "stale_evaluation_snapshot"
    );
    args["cancelled"] = json!(true);
    assert_eq!(
        runtime::call(&mut f.session, "default", "resolver-resolve", &args)
            .unwrap_err()
            .0,
        "stale_evaluation_snapshot"
    );
    f.session
        .query_call("default", "evaluation-drop", &json!({"session":token}))
        .unwrap();
    assert_eq!(
        runtime::call(&mut f.session, "default", "resolver-resolve", &args)
            .unwrap_err()
            .0,
        "evaluation_session_identity_mismatch"
    );
}
#[test]
fn cumulative_evaluation_bytes_and_panic_restore_owner() {
    let mut f = fixture();
    let token = evaluation(&mut f, 12);
    let args = json!({"session":token});
    f.session
        .with_evaluation("default", &args, 60000, 10000, |e| {
            e.account_value(&json!("1234"))
        })
        .unwrap();
    f.session
        .with_evaluation("default", &args, 60000, 10000, |e| {
            e.account_value(&json!("1234"))
        })
        .unwrap();
    assert_eq!(
        f.session
            .with_evaluation("default", &args, 60000, 10000, |e| e
                .account_value(&json!("5")))
            .unwrap_err()
            .0,
        "evaluation_result_byte_budget"
    );
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        f.session
            .with_evaluation::<()>("default", &json!({}), 60000, 10000, |_| {
                panic!("test unwind")
            })
    }));
    assert!(result.is_err());
    assert!(f.session.store().unwrap().db.is_autocommit());
}
#[test]
fn live_interrupt_rejects_before_lending_and_does_not_poison_scope() {
    let mut f = fixture();
    let token = evaluation(&mut f, 100000);
    let args = json!({"session":token});
    let error = catabolic_store::embedding::with_interrupt_check(
        || true,
        || {
            f.session
                .with_evaluation("default", &args, 60000, 10000, |_| Ok(()))
        },
    )
    .unwrap_err();
    assert_eq!(error.0, "evaluation_cancelled");
    f.session
        .with_evaluation("default", &args, 60000, 10000, |_| Ok(()))
        .unwrap();
}
#[test]
fn bind_and_worker_configuration_roll_back_with_caller() {
    let mut f = fixture();
    let scope = f.session.begin().unwrap();
    bind(&mut f);
    let args =
        json!({"catalog":"global","enabled":1,"interval":42,"max_changes":5,"max_removals":2});
    assert_eq!(
        runtime::call(&mut f.session, "default", "worker-configure", &args).unwrap()["enabled"],
        1
    );
    f.session.finish(scope, false).unwrap();
    assert!(
        runtime::call(
            &mut f.session,
            "default",
            "binding",
            &json!({"catalog":"global"})
        )
        .unwrap()
        .is_null()
    );
    assert_eq!(
        f.session
            .store()
            .unwrap()
            .db
            .query_row("SELECT count(*) FROM catalog_refresh_settings", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        0
    );
}
#[test]
fn plans_retain_full_value_but_reject_spoofed_or_changed_tokens() {
    let mut f = fixture();
    bind(&mut f);
    let args = json!({"catalog":"global"});
    let plan = runtime::call(&mut f.session, "default", "projection-plan", &args).unwrap();
    assert!(plan["plan"].get("captures").is_some());
    let mut apply = json!({"plan_token":plan["token"],"plan":plan["plan"],"plan_owner_matches":true,"max_removals":0,"max_changes":100});
    let error = runtime::call(&mut f.session, "default", "projection-apply", &apply).unwrap_err();
    assert!(error.0.contains("disposable"));
    apply["plan"]["safe"] = json!(true);
    assert!(
        runtime::call(&mut f.session, "default", "projection-apply", &apply)
            .unwrap_err()
            .0
            .contains("changed")
    );
    apply["plan_owner_matches"] = json!(false);
    assert!(
        runtime::call(&mut f.session, "default", "projection-apply", &apply)
            .unwrap_err()
            .0
            .contains("identity mismatch")
    );
    assert_eq!(
        f.session
            .store()
            .unwrap()
            .db
            .query_row(
                "SELECT generation FROM fallback_bindings WHERE catalog='global'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}
#[test]
fn capture_preserves_primary_choice_filter_and_health_is_read_only() {
    let mut f = fixture();
    let row = catabolic_store::migration::rows(
        &f.session.store().unwrap().db,
        "SELECT a.*,f.path,f.location FROM item_files a JOIN files f ON f.id=a.file_id",
        &[],
    )
    .unwrap()
    .remove(0);
    let mut args =
        json!({"row":row,"policy":f.policy["definition"],"catalog":"global","primary_choices":{}});
    args["row"]["metadata"] = json!("{\"for_file_id\":\"file\"}");
    assert!(
        runtime::call(&mut f.session, "default", "resolver-capture", &args)
            .unwrap()
            .is_null()
    );
    args["primary_choices"] = json!({"item":"file"});
    let captured = runtime::call(&mut f.session, "default", "resolver-capture", &args).unwrap();
    assert_eq!(captured["file_id"], "file");
    let health = runtime::call(
        &mut f.session,
        "default",
        "resolver-health",
        &json!({"entry":"entry","candidate":captured,"catalog":"global"}),
    )
    .unwrap();
    assert_eq!(health["successes"], 1);
    assert_eq!(
        f.session
            .store()
            .unwrap()
            .db
            .query_row("SELECT count(*) FROM fallback_health", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn worker_tick_is_bounded_and_retains_publication_gate_and_backoff() {
    let mut f = fixture();
    bind(&mut f);
    let path = f.session.store().unwrap().path.clone();
    runtime::call(
        &mut f.session,
        "default",
        "worker-configure",
        &json!({"catalog":"global","enabled":1,"interval":60,"max_changes":10,"max_removals":0}),
    )
    .unwrap();
    f.session.close().unwrap();
    let cancel = AtomicBool::new(false);
    let result = runtime::tick(&path, "default", &json!({}), &cancel).unwrap();
    assert_eq!(result["processed"], 1);
    assert_eq!(result["complete"], false);
    assert!(
        result["projections"][0]["error"]
            .as_str()
            .unwrap()
            .contains("disposable")
    );
    assert_eq!(
        runtime::tick(&path, "default", &json!({}), &cancel).unwrap()["processed"],
        0
    );
    assert!(runtime::tick(&path, "default", &json!({"limit":-1}), &cancel).is_err());
}

#[test]
fn readonly_health_keeps_snapshot_and_closed_detached_calls_refuse() {
    let mut f = fixture();
    let path = f.session.store().unwrap().path.clone();
    f.session
        .store()
        .unwrap()
        .db
        .execute_batch("PRAGMA journal_mode=WAL")
        .unwrap();
    f.session.close().unwrap();
    let mut reader = Session::open(&path, false, false).unwrap();
    let args = json!({"entry":"entry","candidate":{"file_id":"file","revision":"revision"},"catalog":"global"});
    assert_eq!(
        runtime::call(&mut reader, "default", "resolver-health", &args).unwrap()["successes"],
        1
    );
    let writer = Session::open(&path, true, false).unwrap();
    writer.store().unwrap().db.execute("INSERT INTO fallback_health VALUES ('default','global','entry','file','revision',1,8,2)",[]).unwrap();
    assert_eq!(
        runtime::call(&mut reader, "default", "resolver-health", &args).unwrap()["successes"],
        1
    );
    reader.close().unwrap();
    drop(writer);
    f.session = Session::open(&path, true, false).unwrap();
    f.session.detach().unwrap();
    assert!(
        runtime::call(
            &mut f.session,
            "default",
            "binding",
            &json!({"catalog":"global"})
        )
        .unwrap_err()
        .0
        .contains("detached")
    );
    f.session.reconnect().unwrap();
    assert_eq!(
        runtime::call(&mut f.session, "default", "resolver-health", &args).unwrap()["successes"],
        9
    );
    f.session.close().unwrap();
    assert!(
        runtime::call(&mut f.session, "default", "resolver-health", &args)
            .unwrap_err()
            .0
            .contains("closed")
    );
}
#[test]
fn worker_skips_watcher_owned_rows_without_scheduling_them() {
    let mut f = fixture();
    bind(&mut f);
    let path = f.session.store().unwrap().path.clone();
    runtime::call(
        &mut f.session,
        "default",
        "worker-configure",
        &json!({"catalog":"global","enabled":1,"interval":60,"max_changes":10,"max_removals":0}),
    )
    .unwrap();
    f.session.store().unwrap().db.execute_batch("INSERT INTO watcher_definitions VALUES ('watcher-def','default','watcher',1,'{}','digest',0); INSERT INTO watchers(profile,name,definition_id) VALUES ('default','watcher','watcher-def');").unwrap();
    f.session.store().unwrap().db.execute("INSERT INTO watcher_owners(profile,catalog,watcher) VALUES ('default','global','watcher')",[]).unwrap();
    f.session.close().unwrap();
    let result = runtime::tick(&path, "default", &json!({}), &AtomicBool::new(false)).unwrap();
    assert_eq!(result["processed"], 0);
    f.session = Session::open(&path, true, false).unwrap();
    assert_eq!(
        runtime::call(
            &mut f.session,
            "default",
            "binding",
            &json!({"catalog":"global"})
        )
        .unwrap()["next_attempt"],
        0.0
    );
    f.session.close().unwrap();
    assert!(
        runtime::tick(&path, "default", &json!({}), &AtomicBool::new(true))
            .unwrap_err()
            .0
            .contains("cancelled")
    );
}
#[test]
fn public_requests_cannot_supply_fixture_admission_and_plan_uses_session() {
    let mut f = fixture();
    bind(&mut f);
    let token = evaluation(&mut f, 1);
    let args = json!({"catalog":"global","session":token});
    assert_eq!(
        runtime::call(&mut f.session, "default", "projection-plan", &args)
            .unwrap_err()
            .0,
        "evaluation_result_byte_budget"
    );
    let error = runtime::call(
        &mut f.session,
        "default",
        "projection-run",
        &json!({"catalog":"global","apply":true,"disposable_fixture":true}),
    )
    .unwrap_err();
    assert!(error.0.contains("unqualified"));
    assert_eq!(
        f.session
            .store()
            .unwrap()
            .db
            .query_row("SELECT count(*) FROM journal", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn release_handle_is_store_local_and_works_during_evaluation_lending() {
    let mut f = fixture();
    let mut other = fixture();
    bind(&mut f);
    bind(&mut other);
    let args = json!({"catalog":"global"});
    let reply = runtime::call(&mut f.session, "default", "projection-plan", &args).unwrap();
    let foreign = runtime::call(&mut other.session, "default", "projection-plan", &args).unwrap();
    assert_eq!(reply["token"], foreign["token"]);
    let released = f.session.fallback_plans().releases();
    let token = reply["token"].as_u64().unwrap();
    f.session
        .with_evaluation("default", &json!({}), 60000, 10000, |_| {
            released.forget(token)?;
            released.forget(token)
        })
        .unwrap();
    let check = |reply: &Value| {
        json!({
            "plan_token":reply["token"],"plan":reply["plan"],"plan_owner_matches":true
        })
    };
    assert_eq!(
        f.session
            .fallback_plans()
            .checked("default", &check(&reply))
            .err()
            .unwrap()
            .0,
        "native prepared projection identity mismatch"
    );
    assert!(
        other
            .session
            .fallback_plans()
            .checked("default", &check(&foreign))
            .is_ok()
    );
    assert!(f.session.store().unwrap().owns_writer_lock());
}

#[test]
fn release_handle_frees_capacity_while_session_is_detached_or_closed() {
    let mut f = fixture();
    bind(&mut f);
    let reply = runtime::call(
        &mut f.session,
        "default",
        "projection-plan",
        &json!({"catalog":"global"}),
    )
    .unwrap();
    let check = json!({"plan_token":reply["token"],"plan":reply["plan"],"plan_owner_matches":true});
    let plan = f
        .session
        .fallback_plans()
        .checked("default", &check)
        .unwrap();
    for _ in 1..256 {
        f.session
            .fallback_plans()
            .remember("default", plan.clone())
            .unwrap();
    }
    assert!(
        f.session
            .fallback_plans()
            .remember("default", plan.clone())
            .is_err()
    );
    let released = f.session.fallback_plans().releases();
    f.session.detach().unwrap();
    std::thread::spawn(move || released.forget(1))
        .join()
        .unwrap()
        .unwrap();
    f.session.reconnect().unwrap();
    let last = f
        .session
        .fallback_plans()
        .remember("default", plan.clone())
        .unwrap();
    assert!(
        f.session
            .fallback_plans()
            .remember("default", plan.clone())
            .is_err()
    );
    let released = f.session.fallback_plans().releases();
    f.session.close().unwrap();
    released.forget(last["token"].as_u64().unwrap()).unwrap();
    f.session
        .fallback_plans()
        .remember("default", plan)
        .unwrap();
}
