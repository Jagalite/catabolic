// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Disposable logical publication and consumer fences. Each callback runs the
//! real checker while the production owner is detached; fixture writers make
//! deterministic progress at exactly that boundary.
use catabolic_store::{
    Result, Store, consumers, definitions, fallback_mutation, fallback_projection as projection,
    filesystem_mutation, migration, selection::Evaluation,
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::MetadataExt, path::Path};

fn check(path: &Path, profile: &str, database: &str, snapshots: &Value, _: u64) -> Result<Value> {
    catabolic_runtime::fallback::probe_check(
        &json!({"database":path,"database_id":database,"profile":profile,"snapshots":snapshots}),
    )
}
struct Fixture {
    _root: tempfile::TempDir,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let output = root.path().join("output");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&output).unwrap();
        let path = root.path().join("catalog.db");
        migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let r = source.metadata().unwrap();
        let o = output.metadata().unwrap();
        store.transaction(|db| {
            db.execute("INSERT INTO locations VALUES ('source')", [])?;
            db.execute("INSERT INTO bindings VALUES ('default','source','source',?,?,?)", rusqlite::params![source.to_str(), r.dev() as i64, r.ino() as i64])?;
            db.execute("INSERT INTO bindings VALUES ('default','output','global',?,?,?)", rusqlite::params![output.to_str(), o.dev() as i64, o.ino() as i64])?;
            db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','source',1,1,'[]')", [])?;
            db.execute("INSERT INTO items VALUES ('item','movie','{}')", [])?;
            fs::write(source.join("main.bin"), b"disposable consumer media")?;
            let f = source.join("main.bin").metadata()?;
            db.execute("INSERT INTO files VALUES ('main','source','main.bin')", [])?;
            db.execute("INSERT INTO observations VALUES ('default','main',?,?,?,?,'present','scan')", rusqlite::params![f.len() as i64,f.mtime()*1_000_000_000+f.mtime_nsec(),f.dev() as i64,f.ino() as i64])?;
            db.execute("INSERT INTO item_files(id,file_id,item_id,role,metadata,active,origin) VALUES ('association','main','item','primary','{}',1,'explicit')", [])?;
            Ok(())
        }).unwrap();
        let query = definitions::query_put(
            &store,
            "default",
            "main",
            &json!({"selection":{"language":"sql","query":"SELECT id AS file_id FROM files"}}),
        )
        .unwrap();
        let policy = fallback_mutation::put(&store,"default","policy",&json!({"fallbacks":[{"name":"main","query_id":query["id"]}],"within_tier":{"tie_break":"file_id"},"failback":{"mode":"immediate"}})).unwrap();
        let members = definitions::query_put(
            &store,
            "default",
            "members",
            &json!({"selection":{"language":"sql","query":"SELECT id AS item_id FROM items"}}),
        )
        .unwrap();
        definitions::dispatch(&store,"default","layout-put",&json!({"name":"flat","value":{"version":1,"rules":[{"name":"all","path":"{item.id}/{file.name}"}]}})).unwrap();
        let mut evaluation = Evaluation::new(store, "default", 60000, 100000, false).unwrap();
        projection::bind(
            &mut evaluation,
            "global",
            policy["id"].as_str().unwrap(),
            members["id"].as_str().unwrap(),
            "flat",
        )
        .unwrap();
        let prepared = projection::plan(&mut evaluation, "global", false, &mut check).unwrap();
        let mut store = evaluation.into_store();
        let admitted = projection::admit(&store, "default", prepared, 0, 100).unwrap();
        // The disposable fixture gate remains explicit; this tests the same
        // journal owner used by the runtime, without a test-only publisher.
        let published = filesystem_mutation::execute_mut(
            &mut store,
            "default",
            "apply",
            &json!({"catalog":"global","disposable_fixture":true}),
            &mut check,
        )
        .unwrap();
        assert_eq!(published["healthy"], true, "{published}");
        projection::finish(&store, "default", &admitted, &published).unwrap();
        let spec = consumers::connection_spec("default", &json!({"id":"plex","application":"plex","endpoint":"http://127.0.0.1:9","credential_env":"UNUSED_DISPOSABLE_CONSUMER"})).unwrap();
        consumers::put_connection(
            &store,
            "default",
            &json!({"apply":true}),
            &spec,
            &json!({"server_id":"fixture"}),
        )
        .unwrap();
        let connection = consumers::connection(&store, "default", "plex").unwrap();
        let library = json!({"id":"1","uuid":"fixture-library","type":"movie","roots":["/remote"],"created_at":"1"});
        consumers::bind(&mut store,"default",&json!({"id":"movies","connection":"plex","catalog":"global","subtree":"","remote_root":"/remote","library_id":"1","type":"movie","apply":true,"initial_scan":true}),&connection,&[library],&mut check).unwrap();
        Self { _root: root, store }
    }
    fn claim(&mut self) -> consumers::Claim {
        consumers::claim(&mut self.store, "default", false, &mut check)
            .unwrap()
            .expect("healthy logical publication is claimable")
    }
    fn binding(&self) -> Value {
        consumers::binding(&self.store, "default", "movies").unwrap()
    }
    fn delivery(&self) -> Value {
        migration::rows(&self.store.db, "SELECT * FROM consumer_deliveries", &[])
            .unwrap()
            .remove(0)
    }
}

#[test]
fn claim_probe_cannot_steal_or_rewrite_a_competing_lease() {
    // Cover both initial validation and the publication-verification update.
    for race_probe in [1, 2] {
        let mut fixture = Fixture::new();
        let mut calls = 0;
        let mut probe = |path: &Path, profile: &str, database: &str, snapshots: &Value, timeout| {
            calls += 1;
            if calls == race_probe {
                let writer = Store::open(path, true, false)?;
                writer.transaction(|db| {
                db.execute("UPDATE consumer_deliveries SET state='leased',lease_token='competitor',lease_until=?,attempts=3,error='preserve'", [catabolic_store::notifications::now()+180.0])?;
                Ok(())
            })?;
            }
            check(path, profile, database, snapshots, timeout)
        };
        assert!(
            consumers::claim(&mut fixture.store, "default", false, &mut probe)
                .unwrap()
                .is_none()
        );
        assert!(
            calls >= race_probe,
            "the independent writer must run during an actual probe"
        );
        let delivery = fixture.delivery();
        assert_eq!(delivery["lease_token"], "competitor");
        assert_eq!(delivery["state"], "leased");
        assert_eq!(delivery["attempts"], 3);
        assert_eq!(delivery["error"], "preserve");
        assert_eq!(fixture.binding()["acknowledged"], 0);
        assert_eq!(
            fixture
                .store
                .db
                .query_row("SELECT count(*) FROM consumer_attempts", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn pre_send_probe_refences_generation_connection_and_lease() {
    for (sql, expected) in [
        (
            "UPDATE consumer_bindings SET generation=generation+1",
            "publication_changed",
        ),
        (
            "UPDATE consumer_connections SET revision=revision+1",
            "binding_changed",
        ),
        (
            "UPDATE consumer_deliveries SET lease_token='competitor'",
            "stale_claim",
        ),
        (
            "UPDATE consumer_deliveries SET lease_until=0",
            "stale_claim",
        ),
        (
            "UPDATE consumer_bindings SET enabled=0,revision=revision+1",
            "binding_changed",
        ),
    ] {
        let mut fixture = Fixture::new();
        let claimed = fixture.claim();
        let mut calls = 0;
        let mut probe = |path: &Path, profile: &str, database: &str, snapshots: &Value, timeout| {
            calls += 1;
            {
                let writer = Store::open(path, true, false)?;
                writer.transaction(|db| {
                    db.execute(sql, [])?;
                    Ok(())
                })?;
            }
            check(path, profile, database, snapshots, timeout)
        };
        let failure = consumers::before_send(&mut fixture.store, "default", &claimed, &mut probe)
            .unwrap_err();
        assert_eq!(failure.0, expected, "{sql}");
        assert!(calls > 0);
        consumers::acknowledge(
            &fixture.store,
            "default",
            &claimed,
            Some(&consumers::Failure::new(expected)),
        )
        .unwrap();
        assert_eq!(fixture.binding()["acknowledged"], 0, "{sql}");
        if sql.contains("competitor") {
            assert_eq!(fixture.delivery()["lease_token"], "competitor");
        }
    }
}

#[test]
fn pre_send_requires_new_live_evidence_and_failed_probe_never_acknowledges() {
    let mut fixture = Fixture::new();
    let claimed = fixture.claim();
    let mut calls = 0;
    consumers::before_send(
        &mut fixture.store,
        "default",
        &claimed,
        &mut |path, profile, database, snapshots, timeout| {
            calls += 1;
            check(path, profile, database, snapshots, timeout)
        },
    )
    .unwrap();
    assert!(
        calls > 0,
        "claim-time evidence cannot replace pre-send verification"
    );
    let failure = consumers::before_send(
        &mut fixture.store,
        "default",
        &claimed,
        &mut |_, _, _, _, _| Ok(json!({"usable":false,"reason":"fixture_unavailable"})),
    )
    .unwrap_err();
    assert_eq!(failure.0, "publication_unhealthy");
    consumers::acknowledge(
        &fixture.store,
        "default",
        &claimed,
        Some(&consumers::Failure::new(&failure.0)),
    )
    .unwrap();
    assert_eq!(fixture.binding()["acknowledged"], 0);
    assert_eq!(fixture.delivery()["state"], "retry");
}

#[test]
fn indexing_uses_effective_paths_and_rejects_a_changed_generation() {
    let mut fixture = Fixture::new();
    let claim =
        consumers::indexing_claim(&mut fixture.store, "default", "movies", 1, &mut check).unwrap();
    assert_eq!(claim.expected, vec!["/remote/item/main.bin"]);
    fixture
        .store
        .transaction(|db| {
            db.execute("UPDATE consumer_bindings SET generation=generation+1", [])?;
            Ok(())
        })
        .unwrap();
    assert!(
        consumers::indexing_observed(&fixture.store, "default", &claim, &json!({"complete":true}))
            .is_err()
    );
    assert_eq!(fixture.binding()["indexing"], Value::Null);
    let mut probe = |path: &Path, profile: &str, database: &str, snapshots: &Value, timeout| {
        {
            let writer = Store::open(path, true, false)?;
            writer.transaction(|db| {
                db.execute("UPDATE consumer_bindings SET generation=generation+1", [])?;
                Ok(())
            })?;
        }
        check(path, profile, database, snapshots, timeout)
    };
    assert!(
        consumers::indexing_claim(&mut fixture.store, "default", "movies", 1, &mut probe).is_err()
    );
}

#[test]
fn claim_rechecks_configuration_and_schedule_at_both_probe_boundaries() {
    for race_probe in [1, 2] {
        for sql in [
            "UPDATE consumer_bindings SET generation=generation+1",
            "UPDATE consumer_bindings SET revision=revision+1",
            "UPDATE consumer_connections SET revision=revision+1",
            "UPDATE consumer_bindings SET enabled=0,revision=revision+1",
            "UPDATE consumer_bindings SET due_at=99999999999",
            "UPDATE consumer_deliveries SET due_at=99999999999,error='preserve'",
            "UPDATE consumer_deliveries SET state='repair',error='preserve'",
        ] {
            let mut fixture = Fixture::new();
            let mut calls = 0;
            let mut probe =
                |path: &Path, profile: &str, database: &str, snapshots: &Value, timeout| {
                    calls += 1;
                    if calls == race_probe {
                        let writer = Store::open(path, true, false)?;
                        writer.transaction(|db| {
                            db.execute(sql, [])?;
                            Ok(())
                        })?;
                    }
                    check(path, profile, database, snapshots, timeout)
                };
            assert!(
                consumers::claim(&mut fixture.store, "default", false, &mut probe)
                    .unwrap()
                    .is_none(),
                "probe {race_probe}: {sql}"
            );
            assert!(calls >= race_probe, "probe {race_probe}: {sql}");
            assert_eq!(fixture.delivery()["attempts"], 0, "{sql}");
            assert_eq!(fixture.binding()["acknowledged"], 0, "{sql}");
            assert_eq!(
                fixture
                    .store
                    .db
                    .query_row("SELECT count(*) FROM consumer_attempts", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0,
                "{sql}"
            );
            if sql.contains("preserve") {
                assert_eq!(fixture.delivery()["error"], "preserve", "{sql}");
            }
        }
    }
}

#[test]
fn failed_claim_probe_preserves_competing_delivery_state() {
    let mut fixture = Fixture::new();
    let mut calls = 0;
    let mut probe = |path: &Path, _: &str, _: &str, _: &Value, _: u64| {
        calls += 1;
        let writer = Store::open(path, true, false)?;
        writer.transaction(|db| {
            db.execute("UPDATE consumer_deliveries SET state='leased',lease_token='competitor',lease_until=?,attempts=3,error='preserve'", [catabolic_store::notifications::now()+180.0])?;
            Ok(())
        })?;
        Err(catabolic_store::Error("fixture probe failure".into()))
    };
    assert!(
        consumers::claim(&mut fixture.store, "default", false, &mut probe)
            .unwrap()
            .is_none()
    );
    assert!(calls > 0);
    assert_eq!(fixture.delivery()["lease_token"], "competitor");
    assert_eq!(fixture.delivery()["error"], "preserve");
    assert_eq!(fixture.delivery()["attempts"], 3);
    assert_eq!(fixture.binding()["acknowledged"], 0);
}

#[test]
fn failed_pre_send_probe_still_refences_lease_and_connection() {
    for (sql, expected) in [
        (
            "UPDATE consumer_deliveries SET lease_token='competitor'",
            "stale_claim",
        ),
        (
            "UPDATE consumer_deliveries SET lease_until=0",
            "stale_claim",
        ),
        (
            "UPDATE consumer_connections SET revision=revision+1",
            "binding_changed",
        ),
    ] {
        let mut fixture = Fixture::new();
        let claimed = fixture.claim();
        let mut calls = 0;
        let mut probe = |path: &Path, _: &str, _: &str, _: &Value, _: u64| {
            calls += 1;
            let writer = Store::open(path, true, false)?;
            writer.transaction(|db| {
                db.execute(sql, [])?;
                Ok(())
            })?;
            Err(catabolic_store::Error("fixture probe failure".into()))
        };
        let failure = consumers::before_send(&mut fixture.store, "default", &claimed, &mut probe)
            .unwrap_err();
        assert!(calls > 0);
        assert_eq!(failure.0, expected, "{sql}");
        let outcome = consumers::acknowledge(
            &fixture.store,
            "default",
            &claimed,
            Some(&consumers::Failure::new(&failure.0)),
        )
        .unwrap();
        assert_eq!(outcome["state"], "stale_acknowledgement", "{sql}");
        assert_eq!(fixture.binding()["acknowledged"], 0, "{sql}");
    }
}

#[test]
fn initial_scan_bind_cannot_report_success_after_connection_changes() {
    let mut fixture = Fixture::new();
    let connection = consumers::connection(&fixture.store, "default", "plex").unwrap();
    let library = json!({"id":"1","uuid":"fixture-library","type":"movie","roots":["/remote"],"created_at":"1"});
    let mut calls = 0;
    let mut probe = |path: &Path, profile: &str, database: &str, snapshots: &Value, timeout| {
        calls += 1;
        {
            let writer = Store::open(path, true, false)?;
            writer.transaction(|db| {
                db.execute("UPDATE consumer_connections SET revision=revision+1", [])?;
                Ok(())
            })?;
        }
        check(path, profile, database, snapshots, timeout)
    };
    let error = consumers::bind(&mut fixture.store, "default", &json!({"id":"movies","connection":"plex","catalog":"global","subtree":"","remote_root":"/remote","library_id":"1","type":"movie","apply":true,"initial_scan":true}), &connection, &[library], &mut probe).unwrap_err();
    assert!(calls > 0);
    assert_eq!(error.0, "binding_changed");
    assert_eq!(fixture.binding()["acknowledged"], 0);
    assert_eq!(fixture.delivery()["attempts"], 0);
}

#[test]
fn effective_indexing_subtree_and_translated_path_bounds_are_exact() {
    let mut fixture = Fixture::new();
    for (subtree, expected) in [
        ("item", vec!["/remote/main.bin"]),
        ("item/main.bin", vec!["/remote"]),
        ("ite", vec![]),
        ("itemx", vec![]),
    ] {
        fixture
            .store
            .transaction(|db| {
                db.execute("UPDATE consumer_bindings SET subtree=?", [subtree])?;
                Ok(())
            })
            .unwrap();
        let claim =
            consumers::indexing_claim(&mut fixture.store, "default", "movies", 1, &mut check)
                .unwrap();
        assert_eq!(claim.expected, expected, "{subtree}");
    }
    fixture
        .store
        .transaction(|db| {
            db.execute(
                "UPDATE consumer_bindings SET subtree='item/main.bin',remote_root='/'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let claim =
        consumers::indexing_claim(&mut fixture.store, "default", "movies", 1, &mut check).unwrap();
    assert_eq!(claim.expected, ["/"]);
    fixture
        .store
        .transaction(|db| {
            db.execute(
                "UPDATE consumer_bindings SET subtree='',remote_root=?",
                [format!("/{}", "x".repeat(4095))],
            )?;
            Ok(())
        })
        .unwrap();
    let claim = consumers::indexing_claim(&mut fixture.store, "default", "movies", 1, &mut check)
        .expect("valid independently bounded inputs may compose beyond 4096 characters");
    assert_eq!(
        claim.expected,
        [format!("/{}/item/main.bin", "x".repeat(4095))]
    );
    assert!(claim.expected[0].len() > 4096);
    fixture
        .store
        .transaction(|db| {
            db.execute(
                "UPDATE consumer_bindings SET remote_root=?",
                [format!("/{}", "x".repeat(4096))],
            )?;
            Ok(())
        })
        .unwrap();
    let error = consumers::indexing_claim(&mut fixture.store, "default", "movies", 1, &mut check)
        .err()
        .expect("the remote-root input retains its frozen 4096-character bound");
    assert_eq!(error.0, "invalid_configuration");
}

#[test]
fn new_group_member_during_probe_prevents_claim_and_send_admission() {
    for (phase, race_probe) in [("claim", 1), ("claim", 2), ("send", 1)] {
        let mut fixture = Fixture::new();
        fixture
            .store
            .transaction(|db| {
                db.execute(
                    "UPDATE consumer_bindings SET remote_root='/remote/first'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let claimed = (phase == "send").then(|| fixture.claim());
        let mut calls = 0;
        let mut probe = |path: &Path, profile: &str, database: &str, snapshots: &Value, timeout| {
            calls += 1;
            if calls == race_probe {
                let mut writer = Store::open(path, true, false)?;
                let connection = consumers::connection(&writer, "default", "plex")?;
                let library = json!({"id":"1","uuid":"fixture-library","type":"movie","roots":["/remote"],"created_at":"1"});
                consumers::bind(
                    &mut writer,
                    "default",
                    &json!({"id":"extra","connection":"plex","catalog":"global","subtree":"","remote_root":"/remote/second","library_id":"1","type":"movie","apply":true}),
                    &connection,
                    &[library],
                    &mut check,
                )?;
            }
            check(path, profile, database, snapshots, timeout)
        };
        if let Some(claimed) = &claimed {
            let error = consumers::before_send(&mut fixture.store, "default", claimed, &mut probe)
                .unwrap_err();
            assert_eq!(error.0, "binding_changed");
            let outcome = consumers::acknowledge(
                &fixture.store,
                "default",
                claimed,
                Some(&consumers::Failure::new(&error.0)),
            )
            .unwrap();
            assert_eq!(outcome["state"], "stale_acknowledgement");
        } else {
            assert!(
                consumers::claim(&mut fixture.store, "default", false, &mut probe)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(fixture.delivery()["attempts"], 0);
        }
        assert!(calls >= race_probe, "{phase} probe {race_probe}");
        assert_eq!(fixture.binding()["acknowledged"], 0);
    }
}

#[test]
fn indexing_completion_rechecks_every_mutable_consumer_fence() {
    for sql in [
        "UPDATE consumer_bindings SET generation=generation+1",
        "UPDATE consumer_bindings SET revision=revision+1",
        "UPDATE consumer_bindings SET enabled=0,revision=revision+1",
        "UPDATE consumer_connections SET revision=revision+1",
    ] {
        let mut fixture = Fixture::new();
        let claimed =
            consumers::indexing_claim(&mut fixture.store, "default", "movies", 1, &mut check)
                .unwrap();
        fixture
            .store
            .transaction(|db| {
                db.execute(sql, [])?;
                Ok(())
            })
            .unwrap();
        let error = consumers::indexing_observed(
            &fixture.store,
            "default",
            &claimed,
            &json!({"complete":true}),
        )
        .unwrap_err();
        assert_eq!(error.0, "binding_changed", "{sql}");
        assert_eq!(fixture.binding()["indexing"], Value::Null, "{sql}");
        assert_eq!(fixture.binding()["acknowledged"], 0, "{sql}");
    }
}
