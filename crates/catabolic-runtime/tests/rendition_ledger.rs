// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Independent event ledger consumes inputs and observed operation disposition,
//! never production reducer effects. Every edge invokes actual SQLite owners.
use catabolic_runtime::artifacts;
use catabolic_store::{
    Result, Store,
    access::Access,
    artifact_mutation, media_mutation, migration,
    rendition_mutation::{self, RequestLimits},
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::MetadataExt};
const ACTIONS: [&str; 8] = [
    "attach0",
    "attach1",
    "cancel0",
    "cancel1",
    "retry0",
    "retry1",
    "cancel_job",
    "fail_job",
];
const BUDGET: i64 = 1_048_576;
struct Fixture {
    _root: tempfile::TempDir,
    store: Option<Store>,
    body: Value,
    capabilities: Value,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        let input = root.path().join("source");
        let output = root.path().join("output");
        fs::create_dir(&input).unwrap();
        fs::create_dir(&output).unwrap();
        fs::write(input.join("sample.bin"), b"unchanged source").unwrap();
        migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let r = input.metadata().unwrap();
        let f = input.join("sample.bin").metadata().unwrap();
        store.transaction(|db|{
            db.execute("INSERT INTO locations VALUES ('source')",[])?;
            db.execute("INSERT INTO bindings VALUES ('default','source','source',?,?,?)",rusqlite::params![input.to_str(),r.dev() as i64,r.ino() as i64])?;
            db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','source',1,1,'[]')",[])?;
            db.execute("INSERT INTO files VALUES ('file','source','sample.bin')",[])?;
            db.execute("INSERT INTO observations VALUES ('default','file',?,?,?,?, 'present','scan')",rusqlite::params![f.len() as i64,f.mtime()*1_000_000_000+f.mtime_nsec(),f.dev() as i64,f.ino() as i64])?;
            db.execute("INSERT INTO items VALUES ('item','movie','{}')",[])?;
            db.execute("INSERT INTO item_files(id,file_id,item_id,role,metadata,active,origin) VALUES ('association','file','item','primary','{}',1,'explicit')",[])?;
            for caller in 0..2 {let principal=format!("caller{caller}");let token=format!("token{caller}");let grant=format!("grant{caller}");
                db.execute("INSERT INTO api_principals VALUES (?,'default',1)",[&principal])?;
                db.execute("INSERT INTO api_grants VALUES (?,?,?,1,1)",rusqlite::params![grant,principal,catabolic_core::encode(&json!({"actions":["*"],"operator":true}))])?;
                db.execute("INSERT INTO api_tokens VALUES (?,?,? ,?,4102444800,0)",rusqlite::params![token,principal,"digest",catabolic_core::encode(&json!([grant]))])?;
            } Ok(())
        }).unwrap();
        artifact_mutation::bind(&store, "default", "generated", output.to_str().unwrap()).unwrap();
        let recipe = artifacts::recipe(
            &store,
            "thumbnail",
            "thumbnail",
            &json!({"max_output_bytes":BUDGET,"reserve_bytes":0}),
            None,
        )
        .unwrap();
        store
            .transaction(|db| {
                db.execute(
                    "INSERT INTO api_operations VALUES ('operation','default',?,'generated',1)",
                    [recipe["id"].as_str()],
                )?;
                Ok(())
            })
            .unwrap();
        let revision =
            catabolic_store::content_access::revision_of(&store, "default", "file").unwrap();
        Self {
            _root: root,
            store: Some(store),
            body: json!({"item_id":"item","source_file_id":"file","source_revision":revision,"operation_id":"operation"}),
            capabilities: json!({"presets":[{"name":"thumbnail","available":true}],"tools":{"fixture":"no tool execution during admission"}}),
        }
    }
    fn store(&self) -> &Store {
        self.store.as_ref().unwrap()
    }
    fn access<T>(
        &mut self,
        caller: usize,
        call: impl FnOnce(&mut Access) -> Result<T>,
    ) -> Result<T> {
        let mut access =
            Access::from_stored_token(self.store.take().unwrap(), &format!("token{caller}"))?;
        let result = call(&mut access);
        self.store = Some(access.into_store());
        result
    }
}
#[derive(Clone)]
struct Demand {
    id: String,
    job: usize,
    phase: &'static str,
    reserved: i64,
}
#[derive(Clone)]
struct Job {
    id: String,
    phase: &'static str,
}
#[derive(Clone, Default)]
struct Ledger {
    demands: [Option<Demand>; 2],
    jobs: Vec<Job>,
}
impl Ledger {
    fn physical(&mut self, index: usize, phase: &'static str) {
        self.jobs[index].phase = phase;
        for demand in self.demands.iter_mut().flatten().filter(|d| d.job == index) {
            if demand.phase != "cancelled" {
                demand.phase = phase;
            }
            if !matches!(phase, "queued" | "running") {
                demand.reserved = 0;
            }
        }
    }
    fn apply(
        &mut self,
        f: &mut Fixture,
        action: &str,
        fault: bool,
    ) -> std::result::Result<(), String> {
        if let Some(caller) = action
            .chars()
            .last()
            .and_then(|c| c.to_digit(10))
            .map(|v| v as usize)
        {
            let old = self.demands[caller].clone();
            if action.starts_with("attach") {
                let body = f.body.clone();
                let capabilities = f.capabilities.clone();
                let result = f
                    .access(caller, |a| {
                        rendition_mutation::admit(
                            a,
                            &body,
                            "same-key",
                            &capabilities,
                            RequestLimits::default(),
                            None,
                        )
                    })
                    .map_err(|e| e.0)?;
                if old.is_none() {
                    let id = result["request_id"].as_str().unwrap().to_owned();
                    let job_id = f
                        .store()
                        .db
                        .query_row("SELECT job_id FROM api_requests WHERE id=?", [&id], |r| {
                            r.get::<_, String>(0)
                        })
                        .unwrap();
                    let index = if self
                        .jobs
                        .last()
                        .is_some_and(|j| matches!(j.phase, "queued" | "running"))
                    {
                        let index = self.jobs.len() - 1;
                        if self.jobs[index].id != job_id {
                            return Err("shared physical identity was not reused".into());
                        }
                        index
                    } else {
                        self.jobs.push(Job {
                            id: job_id,
                            phase: "queued",
                        });
                        self.jobs.len() - 1
                    };
                    self.demands[caller] = Some(Demand {
                        id,
                        job: index,
                        phase: self.jobs[index].phase,
                        reserved: BUDGET,
                    });
                } else if old
                    .as_ref()
                    .is_some_and(|old| result["request_id"] != old.id)
                {
                    return Err("idempotent demand changed identity".into());
                }
            } else if let Some(old) = old {
                if action.starts_with("cancel") {
                    f.access(caller, |a| rendition_mutation::cancel(a, &old.id))
                        .map_err(|e| e.0)?;
                    self.demands[caller].as_mut().unwrap().phase = "cancelled";
                    if fault {
                        f.store()
                            .db
                            .execute(
                                "UPDATE processing_jobs SET state='cancelled' WHERE id=?",
                                [&self.jobs[old.job].id],
                            )
                            .unwrap();
                    }
                } else {
                    let expected = matches!(old.phase, "failed" | "cancelled" | "blocked");
                    let result = f.access(caller, |a| rendition_mutation::retry(a, &old.id));
                    if result.is_ok() != expected {
                        return Err("retry disposition differs".into());
                    }
                    if expected {
                        if matches!(self.jobs[old.job].phase, "failed" | "timeout" | "cancelled") {
                            self.physical(old.job, "queued");
                        }
                        let demand = self.demands[caller].as_mut().unwrap();
                        demand.phase = "queued";
                        demand.reserved = BUDGET;
                    }
                }
            }
        } else if let Some(index) = self.jobs.len().checked_sub(1) {
            let job = self.jobs[index].clone();
            if action == "cancel_job" {
                media_mutation::cancel(f.store(), "default", &job.id).map_err(|e| e.0)?;
                if matches!(job.phase, "queued" | "running") {
                    self.physical(index, "cancelled");
                }
            } else if action == "fail_job" && matches!(job.phase, "queued" | "running") {
                let (claim, artifact) =
                    artifact_mutation::start(f.store(), "default", &job.id, "jpg")
                        .map_err(|e| e.0)?;
                artifact_mutation::fail(
                    f.store(),
                    "default",
                    &claim,
                    artifact["id"].as_str().unwrap(),
                    "fixture failure",
                )
                .map_err(|e| e.0)?;
                self.physical(index, "failed");
            }
        }
        self.check(f)
    }
    fn check(&self, f: &Fixture) -> std::result::Result<(), String> {
        let actual: i64 = f
            .store()
            .db
            .query_row("SELECT count(*) FROM processing_jobs", [], |r| r.get(0))
            .unwrap();
        if actual != self.jobs.len() as i64 {
            return Err("physical job cardinality differs".into());
        }
        for job in &self.jobs {
            let phase: String = f
                .store()
                .db
                .query_row(
                    "SELECT state FROM processing_jobs WHERE id=?",
                    [&job.id],
                    |r| r.get(0),
                )
                .unwrap();
            if phase != job.phase {
                return Err(format!("physical phase differs: {phase} != {}", job.phase));
            }
            let reserved: i64 = f
                .store()
                .db
                .query_row(
                    "SELECT coalesce(sum(bytes),0) FROM api_reservations WHERE job_id=?",
                    [&job.id],
                    |r| r.get(0),
                )
                .unwrap();
            if reserved
                != if matches!(job.phase, "queued" | "running") {
                    BUDGET
                } else {
                    0
                }
            {
                return Err("shared reservation differs".into());
            }
        }
        for demand in self.demands.iter().flatten() {
            let (phase, reserved, job): (String, i64, String) = f
                .store()
                .db
                .query_row(
                    "SELECT state,reserved_bytes,job_id FROM api_requests WHERE id=?",
                    [&demand.id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            if phase != demand.phase
                || reserved != demand.reserved
                || job != self.jobs[demand.job].id
            {
                return Err("caller publication/state or pinned job differs".into());
            }
        }
        Ok(())
    }
}
fn replay(f: &mut Fixture, events: &[String], fault: bool) -> std::result::Result<(), String> {
    f.store().db.execute_batch("SAVEPOINT replay").unwrap();
    let mut ledger = Ledger::default();
    let result = events.iter().try_for_each(|e| ledger.apply(f, e, fault));
    f.store()
        .db
        .execute_batch("ROLLBACK TO replay; RELEASE replay")
        .unwrap();
    result
}
fn explore(f: &mut Fixture, ledger: &Ledger, depth: usize, count: &mut usize) {
    if depth == 0 {
        return;
    }
    for action in ACTIONS {
        f.store().db.execute_batch("SAVEPOINT edge").unwrap();
        let mut next = ledger.clone();
        next.apply(f, action, false)
            .unwrap_or_else(|e| panic!("{action}: {e}"));
        *count += 1;
        explore(f, &next, depth - 1, count);
        f.store()
            .db
            .execute_batch("ROLLBACK TO edge; RELEASE edge")
            .unwrap();
    }
}
#[test]
fn real_sql_shared_demand_ledger() {
    let mut f = Fixture::new();
    let mut count = 0;
    explore(&mut f, &Ledger::default(), 5, &mut count);
    assert_eq!(count, 37448);
}
#[test]
fn injected_shared_cancel_is_shrunk_and_replayed_in_fresh_process() {
    let mut f = Fixture::new();
    let mut events = vec!["attach0", "attach1", "cancel0", "cancel1"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(replay(&mut f, &events, true).is_err());
    let mut index = 0;
    while index < events.len() {
        let mut candidate = events.clone();
        candidate.remove(index);
        if replay(&mut f, &candidate, true).is_err() {
            events = candidate;
            index = 0;
        } else {
            index += 1;
        }
    }
    assert_eq!(events.len(), 2);
    assert!(replay(&mut f, &events, false).is_ok());
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "fresh_process_event_trace", "--nocapture"])
        .env(
            "CATABOLIC_RENDITION_TRACE",
            serde_json::to_string(&events).unwrap(),
        )
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8_lossy(&result.stdout).contains("forbidden shared cancellation detected")
    );
}
#[test]
fn fresh_process_event_trace() {
    if let Ok(raw) = std::env::var("CATABOLIC_RENDITION_TRACE") {
        assert!(raw.len() < 4096);
        let events: Vec<String> = serde_json::from_str(&raw).unwrap();
        assert!(events.len() <= 32 && events.iter().all(|v| ACTIONS.contains(&v.as_str())));
        let mut f = Fixture::new();
        assert!(replay(&mut f, &events, true).is_err());
        println!("forbidden shared cancellation detected");
    }
}
