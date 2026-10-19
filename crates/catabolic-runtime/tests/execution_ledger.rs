// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Independent event ledger against real schema-32 mutation functions. The ledger
//! never reads reducer state/effects to decide expected behavior. Exploration is
//! bounded to depth five over eight actions, with SQLite rollback between edges.
use catabolic_runtime::media;
use catabolic_store::{Store, media_mutation as storage, migration};
use serde_json::json;
use std::{fs, os::unix::fs::MetadataExt};

const ACTIONS: [&str; 8] = [
    "start",
    "cancel",
    "retry",
    "tick",
    "finish",
    "old_finish",
    "renew",
    "release",
];
struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    id: String,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.db");
        let root = dir.path().join("input");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("sample"), b"bytes").unwrap();
        migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let r = root.metadata().unwrap();
        let s = root.join("sample").metadata().unwrap();
        store.transaction(|db| {db.execute("INSERT INTO locations VALUES ('input')",[])?;
            db.execute("INSERT INTO bindings VALUES ('default','source','input',?,?,?)",rusqlite::params![root.to_str(),r.dev() as i64,r.ino() as i64])?;
            db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','input',1,1,'[]')",[])?;
            db.execute("INSERT INTO files VALUES ('file','input','sample')",[])?;
            db.execute("INSERT INTO observations VALUES ('default','file',?,?,?,?, 'present','scan')",rusqlite::params![s.len() as i64,s.mtime()*1_000_000_000+s.mtime_nsec(),s.dev() as i64,s.ino() as i64])?;Ok(())}).unwrap();
        let result = media::enqueue(
            &store,
            "default",
            "sniff",
            &["file".into()],
            &json!({}),
            true,
            None,
        )
        .unwrap();
        let id = result["queued"][0].as_str().unwrap().into();
        Self {
            _dir: dir,
            store,
            id,
        }
    }
}
#[derive(Clone)]
struct Ledger {
    phase: &'static str,
    now: f64,
    generation: i64,
    attempts: i64,
    until: f64,
    successes: i64,
    current: Option<storage::Claim>,
    first: Option<storage::Claim>,
}
impl Default for Ledger {
    fn default() -> Self {
        Self {
            phase: "queued",
            now: 1000.0,
            generation: 0,
            attempts: 0,
            until: 0.0,
            successes: 0,
            current: None,
            first: None,
        }
    }
}
impl Ledger {
    fn live(&self) -> bool {
        self.current.is_some() && self.until > self.now
    }
    fn finishable(&self, old: bool) -> bool {
        self.phase == "running" && self.live() && (!old || self.generation == 1)
    }
    fn apply(&mut self, f: &Fixture, action: &str, fault: bool) -> Result<(), String> {
        let result = json!({"state":"complete","data":{},"error":null,"retryable":false});
        match action {
            "start" => {
                let expected = matches!(self.phase, "queued" | "running") && !self.live();
                let observed = storage::start(&f.store, "default", &f.id, self.now);
                if observed.is_ok() != expected {
                    return Err(format!("start disposition differs: expected {expected}"));
                }
                if let Ok(c) = observed {
                    self.phase = "running";
                    self.generation += 1;
                    self.attempts += 1;
                    self.until = self.now + 60.0;
                    if c.generation != self.generation
                        || c.attempt != self.attempts
                        || c.lease_until != self.until
                    {
                        return Err("acquisition counters differ".into());
                    }
                    if self.first.is_none() {
                        self.first = Some(c.clone());
                    }
                    self.current = Some(c);
                }
            }
            "cancel" => {
                let expected = matches!(self.phase, "queued" | "running");
                let observed = storage::cancel(&f.store, "default", &f.id).map_err(|e| e.0)?;
                if observed["cancelled"] != expected {
                    return Err("cancel disposition differs".into());
                }
                if expected {
                    self.phase = "cancelled";
                }
            }
            "retry" => {
                let expected = matches!(self.phase, "failed" | "timeout" | "cancelled");
                let observed = storage::retry(&f.store, "default", &f.id);
                if observed.is_ok() != expected {
                    return Err("retry disposition differs".into());
                }
                if expected {
                    self.phase = "queued";
                }
            }
            "tick" => self.now += 60.0,
            "finish" | "old_finish" => {
                let old = action == "old_finish";
                let claim = if old { &self.first } else { &self.current };
                if let Some(claim) = claim {
                    let expected = self.finishable(old);
                    let observed =
                        storage::complete(&f.store, "default", claim, &result, self.now, 30, None);
                    // Negative control deliberately bypasses the rejected fence in
                    // the test adapter, persisting the forbidden publication.
                    if fault && !expected {
                        f.store
                            .db
                            .execute(
                                "UPDATE processing_jobs SET state='complete' WHERE id=?",
                                [&f.id],
                            )
                            .unwrap();
                    }
                    if observed.is_ok() != expected {
                        return Err("completion disposition differs".into());
                    }
                    if expected {
                        self.phase = "complete";
                        self.until = 0.0;
                        self.successes += 1;
                    }
                }
            }
            "renew" => {
                if let Some(c) = &self.current {
                    let expected = self.finishable(false);
                    let observed = storage::heartbeat(&f.store, c, self.now);
                    if observed.is_ok() != expected {
                        return Err("renewal disposition differs".into());
                    }
                    if expected {
                        self.until = self.now + 60.0;
                    }
                }
            }
            "release" => {
                if let Some(c) = &self.current {
                    storage::release(&f.store, c).map_err(|e| e.0)?;
                    self.until = 0.0;
                }
            }
            _ => return Err("unknown ledger action".into()),
        }
        let (phase, attempts): (String, i64) = f
            .store
            .db
            .query_row(
                "SELECT state,attempts FROM processing_jobs WHERE id=?",
                [&f.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        if phase != self.phase || attempts != self.attempts {
            return Err(format!(
                "durable job differs: {phase}/{attempts}, expected {}/{}",
                self.phase, self.attempts
            ));
        }
        let (facts,attempts):(i64,i64)=f.store.db.query_row("SELECT (SELECT count(*) FROM file_facts),(SELECT count(*) FROM processing_attempts)",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        if facts != self.successes || attempts != self.successes {
            return Err("missing or duplicate durable publication".into());
        }
        if self.current.is_some() {
            let (generation, attempt, until): (i64, i64, f64) = f
                .store
                .db
                .query_row(
                    "SELECT generation,attempt,lease_until FROM execution_claims WHERE job_id=?",
                    [&f.id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            if (generation, attempt, until) != (self.generation, self.attempts, self.until) {
                return Err("durable claim differs".into());
            }
        }
        Ok(())
    }
}
fn replay(f: &Fixture, actions: &[String], fault: bool) -> Result<(), String> {
    f.store.db.execute_batch("SAVEPOINT replay").unwrap();
    let mut ledger = Ledger::default();
    let result = actions.iter().try_for_each(|a| ledger.apply(f, a, fault));
    f.store
        .db
        .execute_batch("ROLLBACK TO replay; RELEASE replay")
        .unwrap();
    result
}
#[test]
fn bounded_durable_ledger_explores_production_mutations() {
    let f = Fixture::new();
    let mut edges = 0u64;
    fn visit(
        f: &Fixture,
        ledger: &Ledger,
        path: &mut Vec<String>,
        remaining: usize,
        edges: &mut u64,
    ) {
        if remaining == 0 {
            return;
        }
        for action in ACTIONS {
            f.store.db.execute_batch("SAVEPOINT explore").unwrap();
            let mut next = ledger.clone();
            path.push(action.into());
            *edges += 1;
            if let Err(e) = next.apply(f, action, false) {
                panic!("{}: {e}", serde_json::to_string(path).unwrap());
            }
            visit(f, &next, path, remaining - 1, edges);
            path.pop();
            f.store
                .db
                .execute_batch("ROLLBACK TO explore; RELEASE explore")
                .unwrap();
        }
    }
    visit(&f, &Ledger::default(), &mut vec![], 5, &mut edges);
    assert_eq!(edges, 37448); // 8 + 8² + ... + 8⁵, no budget-stop passed as exhaustive.
}
#[test]
fn stale_publication_fault_is_shrunk_and_replayed_in_fresh_process() {
    if std::env::var_os("CATABOLIC_EXECUTION_LEDGER_REPLAY").is_some() {
        return;
    }
    let f = Fixture::new();
    let mut trace = vec!["tick", "start", "renew", "cancel", "tick", "finish"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(replay(&f, &trace, true).is_err());
    let mut at = 0;
    while at < trace.len() {
        let mut candidate = trace.clone();
        candidate.remove(at);
        if replay(&f, &candidate, true).is_err() {
            trace = candidate;
            at = 0;
        } else {
            at += 1;
        }
    }
    assert_eq!(trace.len(), 3);
    assert!(replay(&f, &trace, false).is_ok());
    let trace_path = f._dir.path().join("replay.json");
    fs::write(
        &trace_path,
        serde_json::to_vec(&json!({"version":1,"actions":trace,"fault":"accept_stale_claim"}))
            .unwrap(),
    )
    .unwrap();
    // Pin the executable inode even if a parallel cargo invocation relinks it.
    let executable = f._dir.path().join("ledger-test");
    #[cfg(target_os = "linux")]
    let original = std::path::PathBuf::from("/proc/self/exe");
    #[cfg(not(target_os = "linux"))]
    let original = std::env::current_exe().unwrap();
    fs::copy(original, &executable).unwrap();
    let output = std::process::Command::new(executable)
        .args(["--exact", "fresh_process_durable_replay", "--nocapture"])
        .env("CATABOLIC_EXECUTION_LEDGER_REPLAY", &trace_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("detected durable stale publication"));
}
#[test]
fn fresh_process_durable_replay() {
    let Some(path) = std::env::var_os("CATABOLIC_EXECUTION_LEDGER_REPLAY") else {
        return;
    };
    let value = catabolic_core::decode_json_bytes(&fs::read(path).unwrap()).unwrap();
    assert_eq!(value["version"], 1);
    assert_eq!(value["fault"], "accept_stale_claim");
    let trace = value["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(trace.len() <= 8);
    let f = Fixture::new();
    assert!(replay(&f, &trace, false).is_ok());
    assert!(replay(&f, &trace, true).is_err());
    println!("detected durable stale publication");
}

#[test]
fn all_durable_identity_dimensions_fence_real_completion() {
    let f = Fixture::new();
    let c = storage::start(&f.store, "default", &f.id, 1000.0).unwrap();
    let result = json!({"state":"partial","data":{},"error":null,"retryable":false});
    for update in [
        "generation=generation+1",
        "attempt=attempt+1",
        "token=token||'x'",
        "snapshot=snapshot||' '",
        "options=options||' '",
        "lease_until=1000",
        "worker_pid=0",
    ] {
        f.store.db.execute_batch("SAVEPOINT corrupt").unwrap();
        f.store
            .db
            .execute(
                &format!("UPDATE execution_claims SET {update} WHERE job_id=?"),
                [&f.id],
            )
            .unwrap();
        assert!(
            storage::complete(&f.store, "default", &c, &result, 1000.0, 30, None).is_err(),
            "{update}"
        );
        assert_eq!(
            f.store
                .db
                .query_row("SELECT count(*) FROM processing_attempts", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        f.store
            .db
            .execute_batch("ROLLBACK TO corrupt; RELEASE corrupt")
            .unwrap();
    }
    for update in [
        "snapshot=snapshot||' '",
        "options=options||' '",
        "attempts=attempts+1",
        "state='cancelled'",
    ] {
        f.store.db.execute_batch("SAVEPOINT corrupt").unwrap();
        f.store
            .db
            .execute(
                &format!("UPDATE processing_jobs SET {update} WHERE id=?"),
                [&f.id],
            )
            .unwrap();
        assert!(
            storage::complete(&f.store, "default", &c, &result, 1000.0, 30, None).is_err(),
            "{update}"
        );
        assert_eq!(
            f.store
                .db
                .query_row("SELECT count(*) FROM processing_attempts", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        f.store
            .db
            .execute_batch("ROLLBACK TO corrupt; RELEASE corrupt")
            .unwrap();
    }
    assert_eq!(
        storage::complete(&f.store, "default", &c, &result, 1000.0, 30, None).unwrap(),
        "partial"
    );
}
