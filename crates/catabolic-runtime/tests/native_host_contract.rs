// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Disposable catalog integration for the privileged native host v1 slice.
use catabolic_runtime::host::{
    CONTRACT_VERSION, Cancellation, ErrorCode, HostSession, Lifecycle, QueryId, ReadBudget, Retry,
};
use catabolic_store::{Store, definitions, migration};
use serde_json::{Value, json};
use std::time::Duration;

struct Fixture {
    root: tempfile::TempDir,
    query: QueryId,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let query = definitions::query_put(&store, "default", "all-items", &json!({"selection":{"language":"sql","query":"SELECT id AS item_id FROM items ORDER BY id"}})).unwrap();
        Self {
            root,
            query: QueryId::new(query["id"].as_str().unwrap()).unwrap(),
        }
    }
    fn path(&self) -> std::path::PathBuf {
        self.root.path().join("catalog.db")
    }
    fn open(&self, writable: bool) -> HostSession {
        HostSession::open(&self.path(), "default", writable, CONTRACT_VERSION).unwrap()
    }
    fn read(&self, host: &mut HostSession) -> Value {
        host.select(
            &self.query,
            ReadBudget {
                timeout: Duration::from_secs(5),
                maximum_ids: 100,
            },
            &Cancellation::default(),
        )
        .unwrap()
    }
}
fn item(id: &str) -> Value {
    json!({"id":id,"kind":"movie","metadata":{"title":id}})
}

#[test]
fn opens_reads_mutates_drains_and_reopens_without_an_adapter() {
    let f = Fixture::new();
    let mut host = f.open(true);
    let identity = host.identity().unwrap();
    assert_eq!(identity["contract_version"], 1);
    assert!(identity.get("path").is_none());
    assert_eq!(f.read(&mut host)["ids"], json!([]));
    let output = host
        .put_items(&[item("b"), item("a")], &Cancellation::default())
        .unwrap();
    assert_eq!(output.len(), 2);
    assert_eq!(f.read(&mut host)["ids"], json!(["a", "b"]));
    host.begin_drain();
    assert_eq!(host.lifecycle(), Lifecycle::Draining);
    assert_eq!(
        host.put_items(&[item("c")], &Cancellation::default())
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        host.select(
            &f.query,
            ReadBudget {
                timeout: Duration::from_secs(1),
                maximum_ids: 100
            },
            &Cancellation::default()
        )
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
    host.close().unwrap();
    host.close().unwrap();
    assert_eq!(host.lifecycle(), Lifecycle::Closed);
    let mut reopened = f.open(false);
    assert_eq!(reopened.identity().unwrap(), identity);
    assert_eq!(f.read(&mut reopened)["ids"], json!(["a", "b"]));
    let denied = reopened
        .put_items(&[item("c")], &Cancellation::default())
        .unwrap_err();
    assert_eq!(denied.code, ErrorCode::UnsupportedCapability);
    assert_eq!(denied.retry, Retry::Reopen);
}

#[test]
fn nested_owner_mutations_roll_back_the_entire_batch() {
    let f = Fixture::new();
    let mut host = f.open(true);
    let failure = host
        .put_items(
            &[
                item("must-rollback"),
                json!({"id":"bad","kind":"not_a_kind","metadata":{}}),
            ],
            &Cancellation::default(),
        )
        .unwrap_err();
    assert_eq!(failure.code, ErrorCode::InvalidInput);
    assert!(!failure.to_string().contains("not_a_kind"));
    assert_eq!(f.read(&mut host)["ids"], json!([]));
    host.put_items(&[item("after-rollback")], &Cancellation::default())
        .unwrap();
    assert_eq!(f.read(&mut host)["ids"], json!(["after-rollback"]));
}

#[test]
fn cancellation_and_invalid_budgets_never_become_successful_empty_results() {
    let f = Fixture::new();
    let mut host = f.open(true);
    let cancelled = Cancellation::default();
    cancelled.cancel();
    assert_eq!(
        host.put_items(&[item("no")], &cancelled).unwrap_err().code,
        ErrorCode::Cancelled
    );
    assert_eq!(
        host.select(
            &f.query,
            ReadBudget {
                timeout: Duration::from_secs(1),
                maximum_ids: 100
            },
            &cancelled
        )
        .unwrap_err()
        .code,
        ErrorCode::Cancelled
    );
    for budget in [
        ReadBudget {
            timeout: Duration::ZERO,
            maximum_ids: 100,
        },
        ReadBudget {
            timeout: Duration::from_secs(61),
            maximum_ids: 100,
        },
        ReadBudget {
            timeout: Duration::from_secs(1),
            maximum_ids: 0,
        },
    ] {
        assert_eq!(
            host.select(&f.query, budget, &Cancellation::default())
                .unwrap_err()
                .code,
            ErrorCode::InvalidInput
        );
    }
    assert_eq!(f.read(&mut host)["ids"], json!([]));
}

#[test]
fn unsupported_contract_and_missing_catalog_do_not_initialize_or_upgrade() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("not-created.db");
    let rejected = HostSession::open(&missing, "default", true, CONTRACT_VERSION + 1)
        .err()
        .unwrap();
    assert_eq!(rejected.code, ErrorCode::UnsupportedCapability);
    assert!(!missing.exists());
    assert!(HostSession::open(&missing, "default", true, CONTRACT_VERSION).is_err());
    assert!(!missing.exists());
    assert!(QueryId::new("").is_err());
    assert!(QueryId::new("\0").is_err());
    let f = Fixture::new();
    assert_eq!(
        HostSession::open(&f.path(), "missing", false, CONTRACT_VERSION)
            .err()
            .unwrap()
            .code,
        ErrorCode::NotFound
    );
}

#[test]
fn explicit_drain_releases_the_existing_writer_for_a_new_host() {
    let f = Fixture::new();
    let mut first = f.open(true);
    assert!(HostSession::open(&f.path(), "default", true, CONTRACT_VERSION).is_err());
    first.close().unwrap();
    let mut second = f.open(true);
    second
        .put_items(&[item("new-owner")], &Cancellation::default())
        .unwrap();
    assert_eq!(f.read(&mut second)["ids"], json!(["new-owner"]));
}

#[test]
fn read_only_session_retains_snapshot_until_reopen() {
    let f = Fixture::new();
    let setup = Store::open(&f.path(), true, false).unwrap();
    setup.db.pragma_update(None, "journal_mode", "WAL").unwrap();
    drop(setup);
    let mut reader = f.open(false);
    assert_eq!(f.read(&mut reader)["ids"], json!([]));
    let mut writer = f.open(true);
    writer
        .put_items(&[item("later")], &Cancellation::default())
        .unwrap();
    writer.close().unwrap();
    assert_eq!(f.read(&mut reader)["ids"], json!([]));
    reader.close().unwrap();
    assert_eq!(f.read(&mut f.open(false))["ids"], json!(["later"]));
}

#[test]
#[ignore = "requires an explicitly built, digest-checked CATABOLIC_HOST_TEST_HELPER"]
fn real_native_helper_scan_cancel_drain_and_reopen() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Permit(Arc<AtomicUsize>);
    impl Drop for Permit {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let helper = std::path::PathBuf::from(
        std::env::var_os("CATABOLIC_HOST_TEST_HELPER").expect("explicit helper required"),
    );
    let digest = catabolic_core::sha256(std::fs::read(&helper).unwrap());
    let f = Fixture::new();
    let source = f.root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    for i in 0..400 {
        std::fs::write(
            source.join(format!("fixture-{i}.txt")),
            "host scan fixture\n",
        )
        .unwrap();
    }
    let mut store = Store::open(&f.path(), true, false).unwrap();
    catabolic_store::observation::execute_detached(
        &mut store,
        "default",
        "bind",
        &json!({"source":"media","root":source}),
    )
    .unwrap();
    drop(store);
    let mut host = f.open(true);
    let released = Arc::new(AtomicUsize::new(0));
    assert_eq!(
        host.start_scan("media", ()).unwrap_err().code,
        ErrorCode::MissingTool
    );
    host.register_helper(&helper, &digest).unwrap();
    let handle = host.start_scan("media", Permit(released.clone())).unwrap();
    assert_eq!(host.active_scan(), Some(handle.clone()));
    assert_eq!(released.load(Ordering::SeqCst), 0);
    assert_eq!(host.close().unwrap_err().code, ErrorCode::Busy);
    // Close rejects admission but preserves cancellation and drain of owned work.
    let mut cancelled = None;
    for _ in 0..50 {
        cancelled = host.cancel_scan(&handle).unwrap();
        if cancelled.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(cancelled.unwrap()["state"], "cancelled");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !host.drain(Duration::from_millis(100)).unwrap() {
        assert!(std::time::Instant::now() < deadline);
    }
    assert_eq!(host.lifecycle(), Lifecycle::Closed);
    assert_eq!(released.load(Ordering::SeqCst), 1);
    let mut reopened = f.open(true);
    reopened.register_helper(&helper, &digest).unwrap();
    let success = reopened
        .start_scan("media", Permit(released.clone()))
        .unwrap();
    let report = loop {
        if let Some(report) = reopened
            .poll_scan(&success, Duration::from_millis(100))
            .unwrap()
        {
            break report;
        }
        assert!(std::time::Instant::now() < deadline);
    };
    assert_eq!(report["complete"], true, "{report}");
    assert_eq!(released.load(Ordering::SeqCst), 2);
    assert!(reopened.drain(Duration::ZERO).unwrap());
    let mut failing = f.open(true);
    failing.register_helper(&helper, &digest).unwrap();
    failing
        .start_scan("missing-source", Permit(released.clone()))
        .unwrap();
    loop {
        match failing.drain(Duration::from_millis(100)) {
            Ok(false) => assert!(std::time::Instant::now() < deadline),
            Err(failure) => {
                assert_eq!(failure.code, ErrorCode::Internal);
                break;
            }
            Ok(true) => panic!("missing source unexpectedly succeeded"),
        }
    }
    assert_eq!(failing.lifecycle(), Lifecycle::Closed);
    assert_eq!(released.load(Ordering::SeqCst), 3);
    assert!(HostSession::open(&f.path(), "default", true, CONTRACT_VERSION).is_ok());
}

#[test]
fn typed_read_errors_distinguish_missing_budget_deadline_and_inflight_cancel() {
    let f = Fixture::new();
    let store = Store::open(&f.path(), true, false).unwrap();
    let slow_sql = "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT CAST(sum(x) AS TEXT) AS item_id FROM n";
    let slow = definitions::query_put(
        &store,
        "default",
        "slow",
        &json!({"timeout_ms":5000,"selection":{"language":"sql","query":slow_sql}}),
    )
    .unwrap();
    let short = definitions::query_put(
        &store,
        "default",
        "short",
        &json!({"timeout_ms":1,"selection":{"language":"sql","query":slow_sql}}),
    )
    .unwrap();
    drop(store);
    let mut host = f.open(true);
    host.put_items(&[item("a"), item("b")], &Cancellation::default())
        .unwrap();
    let budget = ReadBudget {
        timeout: Duration::from_secs(5),
        maximum_ids: 100,
    };
    for _ in 0..8 {
        let failure = host
            .select(
                &QueryId::new(slow["id"].as_str().unwrap()).unwrap(),
                ReadBudget {
                    timeout: Duration::from_millis(1),
                    ..budget
                },
                &Cancellation::default(),
            )
            .unwrap_err();
        assert_eq!(failure.code, ErrorCode::Timeout);
    }

    assert_eq!(
        host.select(
            &QueryId::new("missing").unwrap(),
            budget,
            &Cancellation::default()
        )
        .unwrap_err()
        .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        host.select(
            &f.query,
            ReadBudget {
                maximum_ids: 1,
                ..budget
            },
            &Cancellation::default()
        )
        .unwrap_err()
        .code,
        ErrorCode::BudgetExhausted
    );
    assert_eq!(
        host.select(
            &QueryId::new(short["id"].as_str().unwrap()).unwrap(),
            budget,
            &Cancellation::default()
        )
        .unwrap_err()
        .code,
        ErrorCode::Timeout
    );
    let cancel = Cancellation::default();
    let signal = cancel.clone();
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(25));
        signal.cancel();
    });
    let failure = host
        .select(
            &QueryId::new(slow["id"].as_str().unwrap()).unwrap(),
            budget,
            &cancel,
        )
        .unwrap_err();
    thread.join().unwrap();
    assert_eq!(failure.code, ErrorCode::Cancelled);
    assert_eq!(f.read(&mut host)["ids"], json!(["a", "b"]));
}
