// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Matching admission uses the production snapshot and curation owners.
use catabolic_store::{Error, Result, Store, consumers, migration, plex_matching};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{MetadataExt, symlink},
    path::PathBuf,
};
struct Fixture {
    _root: tempfile::TempDir,
    store: Store,
    source: PathBuf,
    output: PathBuf,
    connection: Value,
    local: Value,
    evidence: Value,
    request: Value,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let output = root.path().join("output");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&output).unwrap();
        fs::write(source.join("movie.mkv"), b"original bytes").unwrap();
        let database = root.path().join("catalog.db");
        migration::initialize(&database).unwrap();
        let store = Store::open(&database, true, false).unwrap();
        let library =
            json!({"id":"7","uuid":"library","type":"movie","roots":["/media"],"created_at":"1"});
        store.transaction(|db|{
            db.execute("INSERT INTO locations VALUES ('seed')",[])?;
            db.execute("INSERT INTO catalogs VALUES ('plex')",[])?;
            for (kind,owner,path) in [("source","seed",&source),("output","plex",&output)] {
                let m=path.metadata()?;
                db.execute("INSERT INTO bindings VALUES ('default',?,?,?,?,?)",rusqlite::params![kind,owner,path.to_str(),m.dev() as i64,m.ino() as i64])?;
            }
            db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','seed',1,1,'[]')",[])?;
            db.execute("INSERT INTO files VALUES ('file','seed','movie.mkv')",[])?;
            let m=source.join("movie.mkv").metadata()?;
            db.execute("INSERT INTO observations VALUES ('default','file',?,?,?,?,'present','scan')",rusqlite::params![m.len() as i64,m.mtime()*1_000_000_000+m.mtime_nsec(),m.dev() as i64,m.ino() as i64])?;
            db.execute("INSERT INTO items VALUES ('item','series','{\"title\":\"Local title\"}')",[])?;
            db.execute("INSERT INTO item_files(id,file_id,item_id,role,part,metadata,origin) VALUES ('association','file','item','primary',NULL,'{\"title\":\"Episode title\"}','explicit')",[])?;
            db.execute("INSERT INTO mappings VALUES ('mapping','plex','file','item','movie.mkv',1)",[])?;
            db.execute("INSERT INTO owned_links VALUES ('default','plex','movie.mkv','../source/movie.mkv')",[])?;
            db.execute("INSERT INTO consumer_connections(profile,id,application,endpoint,credential_env,server_id,evidence) VALUES ('default','home','plex','http://127.0.0.1:9','UNUSED','server','{}')",[])?;
            db.execute("INSERT INTO consumer_bindings(profile,id,connection_id,catalog,subtree,remote_root,local_binding,library,group_id,origin) VALUES ('default','fixture','home','plex','','/media','{}',?,'group','explicit')",[catabolic_core::encode(&library)])?;
            Ok(())
        }).unwrap();
        fs::write(output.join(".catabolic-owner.json"),catabolic_core::encode(&json!({"format":1,"database_id":store.database_id,"profile":"default","catalog":"plex"}))).unwrap();
        symlink("../source/movie.mkv", output.join("movie.mkv")).unwrap();
        let local =
            plex_matching::published_snapshot(&store, "default", "fixture", "mapping").unwrap();
        let connection = consumers::connection(&store, "default", "home").unwrap();
        let evidence=plex_matching::evidence(&connection,&library,&local,&json!({"kind":"movie","reported_type":"movie","rating_key":"1","guid":"plex://movie/example","parts":[{"path":"/media/movie.mkv"}]}),"association").unwrap();
        let request = json!({"binding_id":"fixture","mapping_id":"mapping"});
        Self {
            _root: root,
            store,
            source,
            output,
            connection,
            local,
            evidence,
            request,
        }
    }
    fn run(&self, request: &Value, checkpoint: &mut dyn FnMut() -> Result<()>) -> Result<Value> {
        plex_matching::finish(
            &self.store,
            "default",
            request,
            &self.connection,
            &self.local,
            &self.evidence,
            checkpoint,
        )
    }
    fn apply(&self) -> Value {
        let mut request = self.request.clone();
        request["apply"] = json!(true);
        request["expected_plan"] =
            self.run(&self.request, &mut || Ok(())).unwrap()["plan_id"].clone();
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
fn cancel_after_put_resumes_without_duplicate_decision_or_catalog_changes() {
    let f = Fixture::new();
    let request = f.apply();
    let error = f
        .run(&request, &mut || {
            if f.count("proposals") == 1 {
                Err(Error("cancel fixture".into()))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
    assert_eq!(error.0, "cancel fixture");
    assert_eq!(f.count("proposals"), 1);
    assert_eq!(f.count("decision_events"), 0);
    let accepted = f.run(&request, &mut || Ok(())).unwrap();
    assert_eq!(accepted["applied"], true);
    assert_eq!(accepted, f.run(&request, &mut || Ok(())).unwrap());
    assert_eq!(f.count("proposals"), 1);
    assert_eq!(f.count("decision_events"), 1);
    assert_eq!(f.count("items"), 1);
    assert_eq!(f.count("item_files"), 1);
    assert_eq!(f.count("identities"), 0);
    assert_eq!(
        plex_matching::published_snapshot(&f.store, "default", "fixture", "mapping").unwrap(),
        f.local
    );
}
#[test]
fn decision_failure_rolls_back_acceptance_but_retains_pending_proposal() {
    let f = Fixture::new();
    let request = f.apply();
    f.store.db.execute_batch("CREATE TEMP TRIGGER reject_decision BEFORE INSERT ON decision_events BEGIN SELECT RAISE(ABORT,'fixture decision rejection'); END;").unwrap();
    assert!(
        f.run(&request, &mut || Ok(()))
            .unwrap_err()
            .0
            .contains("fixture decision rejection")
    );
    assert_eq!(f.count("decision_events"), 0);
    let state: String = f
        .store
        .db
        .query_row("SELECT state FROM proposals", [], |r| r.get(0))
        .unwrap();
    assert_eq!(state, "pending");
    f.store
        .db
        .execute_batch("DROP TRIGGER reject_decision")
        .unwrap();
    f.run(&request, &mut || Ok(())).unwrap();
    assert_eq!(f.count("decision_events"), 1);
}
#[test]
fn enclosing_transaction_rollback_undoes_both_owning_steps() {
    let f = Fixture::new();
    let request = f.apply();
    let result: Result<()> = f.store.transaction(|_| {
        f.run(&request, &mut || Ok(()))?;
        assert_eq!(f.count("decision_events"), 1);
        Err(Error("rollback fixture".into()))
    });
    assert_eq!(result.unwrap_err().0, "rollback fixture");
    assert_eq!(f.count("proposals"), 0);
    assert_eq!(f.count("decision_events"), 0);
}
#[test]
fn publication_change_between_proposal_and_decision_is_rejected() {
    let f = Fixture::new();
    let request = f.apply();
    let mut changed = false;
    let result = f.run(&request, &mut || {
        if f.count("proposals") == 1 && !changed {
            changed = true;
            fs::remove_file(f.output.join("movie.mkv"))?;
            symlink("elsewhere", f.output.join("movie.mkv"))?;
        }
        Ok(())
    });
    assert!(result.unwrap_err().0.contains("published_link_changed"));
    assert_eq!(f.count("decision_events"), 0);
}
#[test]
fn saved_publication_allows_metadata_but_rejects_structure_and_source_changes() {
    let f = Fixture::new();
    let done = f.run(&f.apply(), &mut || Ok(())).unwrap();
    let mid = done["match_id"].as_str().unwrap();
    let library = plex_matching::library(&f.local).unwrap();
    f.store
        .db
        .execute(
            "UPDATE item_files SET metadata='{\"title\":\"Edited title\"}'",
            [],
        )
        .unwrap();
    assert!(
        plex_matching::matched_publication(&f.store, "default", &f.connection, &library, mid)
            .is_ok()
    );
    f.store
        .db
        .execute("UPDATE item_files SET part=2", [])
        .unwrap();
    assert!(
        plex_matching::matched_publication(&f.store, "default", &f.connection, &library, mid)
            .unwrap_err()
            .0
            .contains("publication_changed")
    );
    f.store
        .db
        .execute("UPDATE item_files SET part=NULL", [])
        .unwrap();
    fs::write(f.source.join("movie.mkv"), b"replacement different bytes").unwrap();
    assert!(
        plex_matching::matched_publication(&f.store, "default", &f.connection, &library, mid)
            .is_err()
    );
}
#[test]
fn stale_local_connection_plan_and_evidence_budget_refuse_without_admission() {
    let mut f = Fixture::new();
    let mut request = f.apply();
    request["expected_plan"] = json!("stale");
    assert!(
        f.run(&request, &mut || Ok(()))
            .unwrap_err()
            .0
            .contains("plan_changed")
    );
    let request = f.apply();
    f.store
        .db
        .execute("UPDATE consumer_connections SET revision=2", [])
        .unwrap();
    assert!(
        f.run(&request, &mut || Ok(()))
            .unwrap_err()
            .0
            .contains("local_changed")
    );
    f.store
        .db
        .execute("UPDATE consumer_connections SET revision=1", [])
        .unwrap();
    f.evidence["fixture_padding"] = json!("x".repeat(4 * 1024 * 1024));
    assert!(
        f.run(&f.request, &mut || Ok(()))
            .unwrap_err()
            .0
            .contains("plan_too_large")
    );
    assert_eq!(f.count("proposals"), 0);
}
