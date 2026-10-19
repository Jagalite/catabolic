// Independent matching boundary tests; uses only a disposable production fixture.
// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
// Matching admission uses the production snapshot and curation owners.
use catabolic_store::{Error, Result, Store, consumers, migration, plex_matching};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{MetadataExt, symlink},
    path::PathBuf,
};
#[allow(dead_code)]
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
fn cancellation_during_final_publication_check_prevents_acceptance() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let f = Fixture::new();
    let request = f.apply();
    let after_put = Arc::new(AtomicBool::new(false));
    let cancelled = Arc::new(AtomicBool::new(false));
    let hook_after_put = after_put.clone();
    let hook_cancelled = cancelled.clone();
    // Model a CLI signal arriving after the post-put checkpoint but during
    // the final local publication revalidation. Do not interrupt SQLite itself.
    f.store
        .db
        .progress_handler(
            1,
            Some(move || {
                if hook_after_put.load(Ordering::SeqCst) {
                    hook_cancelled.store(true, Ordering::SeqCst);
                }
                false
            }),
        )
        .unwrap();
    let mut checkpoints = 0;
    let result = f.run(&request, &mut || {
        checkpoints += 1;
        if cancelled.load(Ordering::SeqCst) {
            return Err(Error("cancel fixture".into()));
        }
        if checkpoints == 3 {
            after_put.store(true, Ordering::SeqCst);
        }
        Ok(())
    });
    f.store
        .db
        .progress_handler(0, None::<fn() -> bool>)
        .unwrap();
    assert!(cancelled.load(Ordering::SeqCst));
    assert_eq!(f.count("proposals"), 1);
    assert_eq!(
        f.count("decision_events"),
        0,
        "accepted after cancellation during publication revalidation: {result:?}"
    );
    assert_eq!(result.unwrap_err().0, "cancel fixture");
    let state: String = f
        .store
        .db
        .query_row("SELECT state FROM proposals", [], |r| r.get(0))
        .unwrap();
    assert_eq!(state, "pending");
    let accepted = f.run(&request, &mut || Ok(())).unwrap();
    assert_eq!(accepted["applied"], true);
    assert_eq!(accepted, f.run(&request, &mut || Ok(())).unwrap());
    assert_eq!(f.count("proposals"), 1);
    assert_eq!(f.count("decision_events"), 1);
}
