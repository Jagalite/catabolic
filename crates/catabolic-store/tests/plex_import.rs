// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Deterministic per-file decision boundaries, using disposable source/catalogs.
use catabolic_store::{Error, Result, Store, consumers, migration, plex_import};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::MetadataExt, path::PathBuf};

struct Fixture {
    _root: tempfile::TempDir,
    source: PathBuf,
    store: Store,
    connection: Value,
    library: Value,
    request: Value,
    mappings: Vec<Value>,
    data: Value,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        let database = root.path().join("catalog.db");
        migration::initialize(&database).unwrap();
        let store = Store::open(&database, true, false).unwrap();
        let metadata = source.metadata().unwrap();
        store.transaction(|db| {
            db.execute("INSERT INTO locations VALUES ('seed')",[])?;
            db.execute("INSERT INTO bindings VALUES ('default','source','seed',?,?,?)",rusqlite::params![source.to_str(),metadata.dev() as i64,metadata.ino() as i64])?;
            db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','seed',1,3,'[]')",[])?;
            for i in 1..=3 {
                let name = format!("{i}.mkv");
                fs::write(source.join(&name),format!("original {i}"))?;
                let f = source.join(&name).metadata()?;
                db.execute("INSERT INTO files VALUES (?,'seed',?)",rusqlite::params![format!("file-{i}"),name])?;
                db.execute("INSERT INTO observations VALUES ('default',?,?,?,?,?,'present','scan')",rusqlite::params![format!("file-{i}"),f.len() as i64,f.mtime()*1_000_000_000+f.mtime_nsec(),f.dev() as i64,f.ino() as i64])?;
            }
            db.execute("INSERT INTO consumer_connections(profile,id,application,endpoint,credential_env,server_id,evidence) VALUES ('default','home','plex','http://127.0.0.1:9','UNUSED','server','{}')",[])?;
            Ok(())
        }).unwrap();
        let connection = consumers::connection(&store, "default", "home").unwrap();
        let library =
            json!({"id":"7","uuid":"library","type":"movie","roots":["/media"],"created_at":"1"});
        let request = json!({"connection":"home","library_id":"7","mappings":[{"remote_root":"/media","location":"seed"}]});
        let mappings = plex_import::mappings(&request["mappings"]).unwrap();
        let records: Vec<_> = (1..=3).map(|i|json!({"rating_key":i.to_string(),"kind":"movie","metadata":{"title":format!("Movie {i}")},"parts":[{"path":format!("/media/{i}.mkv"),"part":null}],"guids":[],"guid":format!("plex://movie/{i}"),"reported_type":"movie"})).collect();
        let data = json!({"records":records,"total":3,"next_offset":null});
        Self {
            _root: root,
            source,
            store,
            connection,
            library,
            request,
            mappings,
            data,
        }
    }
    fn run(&self, request: &Value, checkpoint: &mut dyn FnMut() -> Result<()>) -> Result<Value> {
        plex_import::run(
            &self.store,
            "default",
            request,
            plex_import::RemotePage {
                connection: &self.connection,
                library: &self.library,
                mappings: &self.mappings,
                data: &self.data,
            },
            checkpoint,
        )
    }
    fn preview(&self) -> Value {
        self.run(&self.request, &mut || Ok(())).unwrap()
    }
    fn apply_request(&self) -> Value {
        let mut request = self.request.clone();
        request["apply"] = json!(true);
        request["expected_plan"] = self.preview()["plan_id"].clone();
        request
    }
    fn count(&self, table: &str) -> i64 {
        self.store
            .db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
}

#[test]
fn interrupted_second_decision_preserves_first_and_reuses_pending_proposal() {
    let f = Fixture::new();
    let request = f.apply_request();
    let result = f.run(&request, &mut || {
        if f.count("items") == 1 && f.count("proposals") == 2 {
            return Err(Error("interrupted fixture".into()));
        }
        Ok(())
    });
    assert_eq!(result.unwrap_err().0, "interrupted fixture");
    assert_eq!(f.count("items"), 1);
    assert_eq!(f.count("proposals"), 2);
    let done = f.run(&f.apply_request(), &mut || Ok(())).unwrap();
    assert_eq!(done["imported"].as_array().unwrap().len(), 2);
    assert_eq!(done["complete"], true);
    assert_eq!(f.count("items"), 3);
    assert_eq!(f.count("proposals"), 3);
    assert_eq!(f.count("decision_events"), 3);
    assert_eq!(
        f.run(&f.apply_request(), &mut || Ok(())).unwrap()["imported"],
        json!([])
    );
}

#[test]
fn decision_error_stops_remaining_files_without_rolling_back_prior_decision() {
    let f = Fixture::new();
    let request = f.apply_request();
    let mut changed = false;
    let result = f
        .run(&request, &mut || {
            if !changed && f.count("items") == 1 && f.count("proposals") == 2 {
                fs::write(f.source.join("2.mkv"), b"changed after second put")?;
                changed = true;
            }
            Ok(())
        })
        .unwrap();
    assert!(changed);
    assert_eq!(result["imported"].as_array().unwrap().len(), 1);
    assert_eq!(result["errors"].as_array().unwrap().len(), 1);
    assert_eq!(result["errors"][0]["file_id"], "file-2");
    assert!(
        result["errors"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("source changed since scan")
    );
    assert_eq!(result["complete"], false);
    assert_eq!(f.count("items"), 1);
    assert_eq!(f.count("proposals"), 2);
    assert_eq!(f.count("decision_events"), 1);
}

#[test]
fn complete_review_fences_connection_and_any_local_or_deferred_change() {
    let f = Fixture::new();
    let request = f.apply_request();
    f.store
        .transaction(|db| {
            db.execute(
                "UPDATE observations SET status='missing' WHERE file_id='file-3'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(
        f.run(&request, &mut || Ok(()))
            .unwrap_err()
            .0
            .contains("import_plan_changed_preview_again")
    );
    assert_eq!(f.count("proposals"), 0);
    f.store
        .transaction(|db| {
            db.execute("UPDATE consumer_connections SET revision=revision+1", [])?;
            Ok(())
        })
        .unwrap();
    assert!(
        f.run(&request, &mut || Ok(()))
            .unwrap_err()
            .0
            .contains("import_connection_changed")
    );
}

#[test]
fn mapping_and_page_request_boundaries_are_exact() {
    for request in [
        json!({"offset":true}),
        json!({"offset":-1}),
        json!({"offset":2147483648_u64}),
        json!({"limit":0}),
        json!({"limit":101}),
        json!({"limit":1.0}),
        json!({"limit":null}),
    ] {
        assert!(
            plex_import::page_bounds(&request)
                .unwrap_err()
                .0
                .contains("invalid_import_page")
        );
    }
    assert_eq!(plex_import::page_bounds(&json!({})).unwrap(), (0, 100));
    for mappings in [
        json!([]),
        json!({}),
        json!([{"remote_root":"/m/../x","location":"seed"}]),
        json!([{"remote_root":"/m","location":"seed"},{"remote_root":"/m/n","location":"seed"}]),
        json!([{"remote_root":"/m","location":"seed","extra":true}]),
    ] {
        assert!(plex_import::mappings(&mappings).is_err());
    }
    assert_eq!(plex_import::mappings(&json!([{"remote_root":"/z/","location":"z"},{"remote_root":"/a","location":"a","subtree":"x/"}])).unwrap(),vec![json!({"remote_root":"/a","location":"a","subtree":"x"}),json!({"remote_root":"/z","location":"z","subtree":""})]);
}
