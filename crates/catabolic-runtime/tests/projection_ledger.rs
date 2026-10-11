// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Independent logical publication ledger over actual plans, SQL admission and
//! completion. External health is an explicit input; reducer output is never
//! consulted by the ledger. Source/caller state is not fabricated from effects.
use catabolic_store::{
    Store, definitions, fallback_mutation, fallback_projection as projection, migration,
    selection::Evaluation,
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::MetadataExt};
const ACTIONS: [&str; 6] = [
    "preferred",
    "backup",
    "preview",
    "healthy",
    "failed",
    "stale",
];
struct Fixture {
    _root: tempfile::TempDir,
    evaluation: Evaluation,
}
impl Fixture {
    fn new() -> Self {
        struct Seed {
            _root: tempfile::TempDir,
            bytes: Vec<u8>,
        }
        // TLS drops the disposable genesis directory when each test exits.
        thread_local! { static SEED:Seed = {
            let Fixture { _root, evaluation } = Fixture::initial();
            let path=evaluation.store().path.clone();drop(evaluation);
            Seed{_root,bytes:fs::read(path).unwrap()}
        }; }
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        SEED.with(|seed| fs::write(&path, &seed.bytes).unwrap());
        let evaluation = Evaluation::new(
            Store::open(&path, true, false).unwrap(),
            "default",
            60000,
            100000,
            false,
        )
        .unwrap();
        Self {
            _root: root,
            evaluation,
        }
    }
    fn initial() -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        let path = root.path().join("catalog.db");
        migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let r = source.metadata().unwrap();
        store.transaction(|db|{
 db.execute("INSERT INTO locations VALUES ('source')",[])?;
 db.execute("INSERT INTO bindings VALUES ('default','source','source',?,?,?)",rusqlite::params![source.to_str(),r.dev()as i64,r.ino()as i64])?;
 db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','source',1,2,'[]')",[])?;
 db.execute("INSERT INTO items VALUES ('item','movie','{}')",[])?;
 for id in ["a","b"]{fs::write(source.join(format!("{id}.bin")),b"immutable source")?;let f=source.join(format!("{id}.bin")).metadata()?;
 db.execute("INSERT INTO files VALUES (?,'source',?)",rusqlite::params![id,format!("{id}.bin")])?;
 db.execute("INSERT INTO observations VALUES ('default',?,?,?,?,?,'present','scan')",rusqlite::params![id,f.len()as i64,f.mtime()*1_000_000_000+f.mtime_nsec(),f.dev()as i64,f.ino()as i64])?;
 db.execute("INSERT INTO item_files(id,file_id,item_id,role,metadata,active,origin) VALUES (?,?,'item','primary','{}',1,'explicit')",rusqlite::params![format!("association-{id}"),id])?;
 }Ok(())}).unwrap();
        let mut tiers = vec![];
        for id in ["a", "b"] {
            let query=definitions::query_put(&store,"default",id,&json!({"selection":{"language":"sql","query":"SELECT id AS file_id FROM files WHERE id=:id","params":{"id":id}}})).unwrap();
            tiers.push(json!({"name":id,"query_id":query["id"]}));
        }
        let policy=fallback_mutation::put(&store,"default","policy",&json!({"fallbacks":tiers,"within_tier":{"tie_break":"file_id"},"failback":{"mode":"immediate"}})).unwrap();
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
        Self {
            _root: root,
            evaluation,
        }
    }
}
#[derive(Clone)]
struct Ledger {
    live: [bool; 2],
    generation: i64,
    published: i64,
    file: Option<&'static str>,
    state: Option<&'static str>,
}
impl Default for Ledger {
    fn default() -> Self {
        Self {
            live: [true, true],
            generation: 0,
            published: 0,
            file: None,
            state: None,
        }
    }
}
impl Ledger {
    fn check(&self, f: &Fixture) -> std::result::Result<(), String> {
        let binding = projection::binding(f.evaluation.store(), "default", "global")
            .unwrap()
            .unwrap();
        if binding["generation"] != self.generation {
            return Err("admitted generation differs".into());
        }
        let rows = migration::rows(
            &f.evaluation.store().db,
            "SELECT * FROM fallback_entries",
            &[],
        )
        .unwrap();
        if self.state.is_none() {
            if !rows.is_empty() {
                return Err("preview changed membership".into());
            }
            return Ok(());
        }
        if rows.len() != 1 {
            return Err("logical membership cardinality differs".into());
        }
        let row = &rows[0];
        if row["file_id"] != json!(self.file)
            || row["state"] != json!(self.state)
            || row["generation"] != self.generation
            || row["published_generation"] != self.published
            || row["active"] != 1
        {
            return Err(format!(
                "independent publication ledger differs: {}",
                catabolic_core::encode(row)
            ));
        }
        Ok(())
    }
    fn event(
        &mut self,
        f: &mut Fixture,
        event: &str,
        fault: bool,
    ) -> std::result::Result<(), String> {
        if event == "preferred" {
            self.live[0] = !self.live[0];
            return self.check(f);
        }
        if event == "backup" {
            self.live[1] = !self.live[1];
            return self.check(f);
        }
        let profile = f.evaluation.profile.clone();
        let live = self.live;
        let mut probe = move |_path: &std::path::Path,
                              _profile: &str,
                              _database: &str,
                              snapshots: &Value,
                              _timeout: u64| {
            let id = snapshots[0]["id"].as_str().unwrap();
            Ok(json!({"usable":if id=="a"{live[0]}else{live[1]},"reason":null}))
        };
        let prepared =
            projection::plan(&mut f.evaluation, "global", false, &mut probe).map_err(|e| e.0)?;
        if event == "preview" {
            return self.check(f);
        }
        if event == "stale" {
            f.evaluation.store().transaction(|db|{db.execute("UPDATE items SET metadata=json_set(metadata,'$.counter',coalesce(json_extract(metadata,'$.counter'),0)+1)",[])?;Ok(())}).unwrap();
            let result = projection::admit(f.evaluation.store(), &profile, prepared, 100, 100);
            if !matches!(result,Err(ref e) if e.0=="stale_resolution_plan") {
                return Err("stale plan admitted".into());
            }
            return self.check(f);
        }
        let winner = if self.live[0] {
            Some("a")
        } else if self.live[1] {
            Some("b")
        } else {
            None
        };
        let admitted = projection::admit(f.evaluation.store(), &profile, prepared, 100, 100)
            .map_err(|e| e.0)?;
        self.generation += 1;
        if let Some(winner) = winner {
            self.file = Some(winner);
            self.state = Some(if winner == "a" {
                "resolved_preferred"
            } else {
                "resolved_fallback"
            });
            projection::finish(f.evaluation.store(),&profile,&admitted,&json!({"healthy":event=="healthy","applied":["injected external publication completion"]})).map_err(|e|e.0)?;
            if event == "healthy" {
                self.published = self.generation;
            }
        } else {
            self.state = Some("unresolved");
            if fault {
                f.evaluation
                    .store()
                    .db
                    .execute(
                        "UPDATE fallback_entries SET published_generation=generation",
                        [],
                    )
                    .unwrap();
            }
        }
        self.check(f)
    }
}
fn replay(trace: &[String], fault: bool) -> std::result::Result<(), String> {
    let mut fixture = Fixture::new();
    let mut ledger = Ledger::default();
    for event in trace {
        ledger.event(&mut fixture, event, fault)?;
    }
    Ok(())
}
#[test]
fn bounded_production_projection_sql_ledger() {
    let mut traces = vec![vec![]];
    let mut count = 0;
    for _depth in 0..4 {
        let mut next = vec![];
        for trace in traces {
            for event in ACTIONS {
                let mut trace = trace.clone();
                trace.push(event.to_owned());
                replay(&trace, false).unwrap_or_else(|error| panic!("{trace:?}: {error}"));
                count += 1;
                next.push(trace);
            }
        }
        traces = next;
    }
    assert_eq!(count, 1554);
}
#[test]
fn premature_publication_generation_fault_shrinks_and_replays() {
    let mut trace = ["preview", "preferred", "backup", "healthy", "preview"]
        .map(str::to_owned)
        .to_vec();
    assert!(replay(&trace, true).is_err());
    let mut changed = true;
    while changed {
        changed = false;
        for index in 0..trace.len() {
            let mut smaller = trace.clone();
            smaller.remove(index);
            if replay(&smaller, true).is_err() {
                trace = smaller;
                changed = true;
                break;
            }
        }
    }
    assert_eq!(trace, ["preferred", "backup", "healthy"]);
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "fresh_projection_trace", "--nocapture"])
        .env(
            "CATABOLIC_PROJECTION_TRACE",
            catabolic_core::encode(&json!(trace)),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
#[test]
fn fresh_projection_trace() {
    if let Ok(raw) = std::env::var("CATABOLIC_PROJECTION_TRACE") {
        let trace: Vec<String> = serde_json::from_str(&raw).unwrap();
        assert!(replay(&trace, true).is_err());
        assert!(replay(&trace, false).is_ok());
    }
}
