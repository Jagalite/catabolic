// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Independent review regressions for logical source-tier and failback decisions.
//! All decisions and SQLite transitions use production owners; local files are
//! disposable fixtures and the probe callback invokes the production checker.
use catabolic_runtime::fallback;
use catabolic_store::{
    Store, definitions, fallback_mutation, fallback_resolution, migration, selection::Evaluation,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, os::unix::fs::MetadataExt};

struct Fixture {
    _root: tempfile::TempDir,
    source: std::path::PathBuf,
    evaluation: Evaluation,
    policy: String,
    previous: BTreeMap<String, Value>,
}
impl Fixture {
    fn new(mode: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let r = source.metadata().unwrap();
        store.transaction(|db| {
            db.execute("INSERT INTO locations VALUES ('source')", [])?;
            db.execute("INSERT INTO bindings VALUES ('default','source','source',?,?,?)", rusqlite::params![source.to_str(), r.dev() as i64, r.ino() as i64])?;
            db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','source',1,2,'[]')", [])?;
            db.execute("INSERT INTO items VALUES ('item','movie','{}')", [])?;
            for name in ["preferred", "fallback"] {
                let filename = format!("{name}.bin");
                fs::write(source.join(&filename), format!("disposable {name} content"))?;
                let f = source.join(&filename).metadata()?;
                db.execute("INSERT INTO files VALUES (?,'source',?)", rusqlite::params![name, filename])?;
                db.execute("INSERT INTO observations VALUES ('default',?,?,?,?,?,'present','scan')", rusqlite::params![name, f.len() as i64, f.mtime()*1_000_000_000+f.mtime_nsec(), f.dev() as i64, f.ino() as i64])?;
                db.execute("INSERT INTO item_files(id,file_id,item_id,role,metadata,active,origin) VALUES (?,?,'item','primary','{}',1,'explicit')", rusqlite::params![format!("association-{name}"),name])?;
            }
            Ok(())
        }).unwrap();
        let tiers = ["preferred", "fallback"].map(|name| {
            let query = definitions::query_put(
                &store,
                "default",
                name,
                &json!({"selection":{"language":"sql","query":format!("SELECT file_id FROM item_files WHERE active=1 AND file_id='{name}'")}}),
            ).unwrap();
            json!({"name":name,"query_id":query["id"]})
        });
        let policy = fallback_mutation::put(
            &store,
            "default",
            "review-failback",
            &json!({"fallbacks":tiers,"failback":{"mode":mode,"minimum_healthy_seconds":0,"minimum_successful_checks":2}}),
        ).unwrap()["id"].as_str().unwrap().to_owned();
        let entry = fallback_resolution::logical_entry(&json!({"item_id":"item"})).unwrap();
        let key = fallback_resolution::entry_id(&store, "default", "", &entry).unwrap();
        let revision =
            catabolic_store::content_access::revision_of(&store, "default", "fallback").unwrap();
        let previous = BTreeMap::from([(
            key,
            json!({"file_id":"fallback","revision":revision,"tier":1,"evidence":"{}"}),
        )]);
        let evaluation = Evaluation::new(store, "default", 5000, 10000, false).unwrap();
        Self {
            _root: root,
            source,
            evaluation,
            policy,
            previous,
        }
    }
    fn resolve(&mut self, manual: bool) -> Value {
        let mut probe = |path: &std::path::Path,
                         profile: &str,
                         database: &str,
                         snapshots: &Value,
                         _: u64| {
            fallback::probe_check(
                &json!({"database":path,"database_id":database,"profile":profile,"snapshots":snapshots}),
            )
        };
        let mut resolver = fallback_resolution::Resolver::new(
            fallback_resolution::Context::Local(&mut self.evaluation),
            &mut probe,
            None,
        );
        resolver
            .resolve(
                &[json!({"item_id":"item"})],
                &self.policy,
                "",
                &self.previous,
                manual,
            )
            .unwrap()
    }
}

#[test]
fn manual_failback_holds_only_an_eligible_live_previous_candidate() {
    let mut f = Fixture::new("manual");
    let result = f.resolve(false);
    assert_eq!(result["decisions"][0]["file_id"], "fallback");
    assert_eq!(result["decisions"][0]["selected_tier"], 1);
    assert_eq!(result["checks"], 2);
    assert_eq!(result["health"][0]["file_id"], "preferred");
    assert_eq!(f.resolve(true)["decisions"][0]["file_id"], "preferred");

    let mut f = Fixture::new("manual");
    fs::remove_file(f.source.join("fallback.bin")).unwrap();
    assert_eq!(f.resolve(false)["decisions"][0]["file_id"], "preferred");

    let mut f = Fixture::new("manual");
    f.evaluation
        .store()
        .transaction(|db| {
            db.execute(
                "UPDATE item_files SET active=0 WHERE file_id='fallback'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    // Capture a fresh evaluation after the intentional fixture change.
    let store = f.evaluation.into_store();
    f.evaluation = Evaluation::new(store, "default", 5000, 10000, false).unwrap();
    assert_eq!(f.resolve(false)["decisions"][0]["file_id"], "preferred");
}

#[test]
fn stable_failback_counts_successes_for_the_preferred_revision() {
    let mut f = Fixture::new("stable");
    let result = f.resolve(false);
    assert_eq!(result["decisions"][0]["file_id"], "fallback");
    let health = &result["health"][0];
    f.evaluation.store().transaction(|db| {
        db.execute("INSERT INTO fallback_health(profile,catalog,entry_id,file_id,revision,healthy_since,successes,last_check) VALUES ('default','',?,?,?,?,?,?)", rusqlite::params![health["entry_id"].as_str(),health["file_id"].as_str(),health["revision"].as_str(),health["healthy_since"].as_f64(),health["successes"].as_i64(),health["last_check"].as_f64()])?;
        Ok(())
    }).unwrap();
    let store = f.evaluation.into_store();
    f.evaluation = Evaluation::new(store, "default", 5000, 10000, false).unwrap();
    let result = f.resolve(false);
    assert_eq!(result["decisions"][0]["file_id"], "preferred");
    assert_eq!(result["health"][0]["successes"], 2);
}

#[test]
fn stale_attempt_receipts_cannot_change_shared_demand_or_its_reservation() {
    use catabolic_store::{access::Access, artifact_mutation, media_mutation, rendition_mutation};
    let f = Fixture::new("immediate");
    let output = f._root.path().join("generated");
    fs::create_dir(&output).unwrap();
    let mut store = f.evaluation.into_store();
    artifact_mutation::bind(&store, "default", "generated", output.to_str().unwrap()).unwrap();
    let recipe = catabolic_runtime::artifacts::recipe(
        &store,
        "review-thumbnail",
        "thumbnail",
        &json!({"max_output_bytes":1048576,"reserve_bytes":0}),
        None,
    )
    .unwrap();
    store
        .transaction(|db| {
            db.execute(
                "INSERT INTO api_operations VALUES ('operation','default',?,'generated',1)",
                [recipe["id"].as_str()],
            )?;
            for caller in 0..2 {
                let principal = format!("caller{caller}");
                let token = format!("token{caller}");
                let grant = format!("grant{caller}");
                db.execute(
                    "INSERT INTO api_principals VALUES (?,'default',1)",
                    [&principal],
                )?;
                db.execute(
                    "INSERT INTO api_grants VALUES (?,?,?,1,1)",
                    rusqlite::params![
                        grant,
                        principal,
                        catabolic_core::encode(&json!({"actions":["*"],"operator":true}))
                    ],
                )?;
                db.execute(
                    "INSERT INTO api_tokens VALUES (?,?,?, ?,4102444800,0)",
                    rusqlite::params![
                        token,
                        principal,
                        "digest",
                        catabolic_core::encode(&json!([grant]))
                    ],
                )?;
            }
            Ok(())
        })
        .unwrap();
    let revision =
        catabolic_store::content_access::revision_of(&store, "default", "preferred").unwrap();
    let body = json!({"item_id":"item","source_file_id":"preferred","source_revision":revision,"operation_id":"operation"});
    let capabilities = json!({"presets":[{"name":"thumbnail","available":true}],"tools":{"fixture":"no media execution"}});
    let mut demands = Vec::new();
    for caller in 0..2 {
        let mut access = Access::from_stored_token(store, &format!("token{caller}")).unwrap();
        let result = rendition_mutation::admit(
            &mut access,
            &body,
            "shared",
            &capabilities,
            rendition_mutation::RequestLimits::default(),
            None,
        )
        .unwrap();
        demands.push(result["request_id"].as_str().unwrap().to_owned());
        store = access.into_store();
    }
    let job: String = store
        .db
        .query_row("SELECT job_id FROM api_requests LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let (old, artifact) = artifact_mutation::start(&store, "default", &job, "jpg").unwrap();
    let mut access = Access::from_stored_token(store, "token0").unwrap();
    rendition_mutation::cancel(&mut access, &demands[0]).unwrap();
    store = access.into_store();
    artifact_mutation::fail(
        &store,
        "default",
        &old,
        artifact["id"].as_str().unwrap(),
        "first attempt failed",
    )
    .unwrap();
    let mut access = Access::from_stored_token(store, "token1").unwrap();
    rendition_mutation::retry(&mut access, &demands[1]).unwrap();
    store = access.into_store();
    let (current, artifact2) = artifact_mutation::start(&store, "default", &job, "jpg").unwrap();
    assert!(current.attempt > old.attempt);
    let snapshot = |store: &Store| {
        [
            "processing_jobs",
            "processing_attempts",
            "processing_artifacts",
            "execution_claims",
            "api_requests",
            "api_reservations",
            "api_events",
        ]
        .map(|table| {
            migration::rows(
                &store.db,
                &format!("SELECT * FROM {table} ORDER BY rowid"),
                &[],
            )
            .unwrap()
        })
    };
    let before = snapshot(&store);
    assert!(
        artifact_mutation::fail(
            &store,
            "default",
            &old,
            artifact["id"].as_str().unwrap(),
            "late failure receipt"
        )
        .is_err()
    );
    assert!(
        media_mutation::complete(
            &store,
            "default",
            &old,
            &json!({"state":"complete","data":{},"error":null}),
            catabolic_store::notifications::now(),
            1,
            None
        )
        .is_err()
    );
    assert_eq!(snapshot(&store), before);
    let mut access = Access::from_stored_token(store, "token1").unwrap();
    assert_eq!(
        rendition_mutation::status(&mut access, &demands[1]).unwrap()["state"],
        "running"
    );
    rendition_mutation::cancel(&mut access, &demands[1]).unwrap();
    store = access.into_store();
    artifact_mutation::fail(
        &store,
        "default",
        &current,
        artifact2["id"].as_str().unwrap(),
        "second attempt failed",
    )
    .unwrap();
    assert_eq!(
        migration::rows(
            &store.db,
            "SELECT state,reserved_bytes FROM api_requests",
            &[]
        )
        .unwrap(),
        vec![json!({"state":"cancelled","reserved_bytes":0}); 2]
    );
    assert!(
        migration::rows(&store.db, "SELECT * FROM api_reservations", &[])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn failed_malformed_and_oversized_helper_receipts_release_only_reaped_slots() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new("immediate");
    let store = f.evaluation.into_store();
    let path = store.path.clone();
    let database = store.database_id.clone();
    drop(store);
    let mode = f._root.path().join("mode");
    let pidfile = f._root.path().join("pid");
    let helper = f._root.path().join("receipt-fault-helper");
    fs::write(&helper, format!(
        "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\n/bin/cat >/dev/null\ncase \"$(/bin/cat '{}')\" in\nexit) exit 7;;\nmalformed) printf '[]';;\noversized) /usr/bin/head -c 70000 /dev/zero;;\nvalid) printf '%s' '{{\"usable\":true,\"reason\":null}}';;\nesac\n",
        pidfile.display(), mode.display()
    )).unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    catabolic_runtime::helper::register(&helper, None).unwrap();
    for kind in ["exit", "malformed", "oversized", "valid"] {
        fs::write(&mode, kind).unwrap();
        let result = fallback::probe(&path, "default", &database, &json!([{}]), 1000).unwrap();
        assert_eq!(
            result,
            if kind == "valid" {
                json!({"usable":true,"reason":null})
            } else {
                json!({"usable":false,"reason":"probe_failed"})
            },
            "{kind}"
        );
        let pid: i64 = fs::read_to_string(&pidfile).unwrap().parse().unwrap();
        assert!(
            !fallback_resolution::process_alive(pid),
            "{kind}: helper still running"
        );
        let store = Store::open(&path, false, false).unwrap();
        let count: i64 = store.db.query_row("SELECT count(*) FROM fallback_probe_slots WHERE owner IS NOT NULL OR pid IS NOT NULL", [], |r|r.get(0)).unwrap();
        assert_eq!(count, 0, "{kind}: stale durable slot");
    }
}
