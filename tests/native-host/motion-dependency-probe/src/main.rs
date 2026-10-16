// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
use catabolic_runtime::host::{
    CONTRACT_VERSION, Cancellation, ErrorCode, HostSession, Lifecycle, QueryId, ReadBudget,
};
use catabolic_store::{definitions, embedding::Session, migration};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn budget() -> ReadBudget {
    ReadBudget {
        timeout: Duration::from_secs(5),
        maximum_ids: 100,
    }
}

struct Permit(Arc<AtomicUsize>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn real_helper_lifecycle(catalog: &Path, helper: &Path, digest: &str) -> Result<Value> {
    let released = Arc::new(AtomicUsize::new(0));
    let mut host = HostSession::open(catalog, "default", true, CONTRACT_VERSION)?;
    assert_eq!(
        host.start_scan("r0-media", ()).unwrap_err().code,
        ErrorCode::MissingTool
    );
    host.register_helper(helper, digest)?;
    let handle = host.start_scan("r0-media", Permit(released.clone()))?;
    assert_eq!(host.active_scan(), Some(handle.clone()));
    assert_eq!(released.load(Ordering::SeqCst), 0);
    assert_eq!(host.close().unwrap_err().code, ErrorCode::Busy);
    let deadline = Instant::now() + Duration::from_secs(20);
    let cancelled = loop {
        if let Some(report) = host.cancel_scan(&handle)? {
            break report;
        }
        assert!(Instant::now() < deadline, "cancellation deadline exceeded");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(cancelled["state"], "cancelled");
    while !host.drain(Duration::from_millis(100))? {
        assert!(Instant::now() < deadline, "drain deadline exceeded");
    }
    assert_eq!(host.lifecycle(), Lifecycle::Closed);
    assert_eq!(released.load(Ordering::SeqCst), 1);

    let mut reopened = HostSession::open(catalog, "default", true, CONTRACT_VERSION)?;
    reopened.register_helper(helper, digest)?;
    let handle = reopened.start_scan("r0-media", Permit(released.clone()))?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let success = loop {
        if let Some(report) = reopened.poll_scan(&handle, Duration::from_millis(100))? {
            break report;
        }
        assert!(Instant::now() < deadline, "scan deadline exceeded");
    };
    assert_eq!(success["complete"], true, "{success}");
    assert_eq!(released.load(Ordering::SeqCst), 2);
    assert!(reopened.drain(Duration::ZERO)?);
    assert_eq!(reopened.lifecycle(), Lifecycle::Closed);
    let mut final_owner = HostSession::open(catalog, "default", true, CONTRACT_VERSION)?;
    final_owner.close()?;
    Ok(json!({
        "status":"passed",
        "registered_path":helper,
        "registered_sha256":digest,
        "cancellation_state":cancelled["state"],
        "successful_scan_complete":success["complete"],
        "permits_released":released.load(Ordering::SeqCst),
        "reopened_after_drain":true
    }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let helper = std::env::var_os("CATABOLIC_HOST_TEST_HELPER").map(PathBuf::from);
    let helper_digest = std::env::var("CATABOLIC_HOST_TEST_HELPER_SHA256").ok();
    if helper.is_some() != helper_digest.is_some() {
        return Err(
            "set both CATABOLIC_HOST_TEST_HELPER and CATABOLIC_HOST_TEST_HELPER_SHA256".into(),
        );
    }
    let temp = tempfile::tempdir()?;
    let motion_db = temp.path().join("motion.sqlite3");
    let catalog_db = temp.path().join("catabolic.sqlite3");
    let pool = playscale::db::connect(&motion_db).await?;
    let motion_sqlite: String = sqlx::query_scalar("SELECT sqlite_version()")
        .fetch_one(&pool)
        .await?;
    let motion_source: String = sqlx::query_scalar("SELECT sqlite_source_id()")
        .fetch_one(&pool)
        .await?;
    let motion_migrations: i64 =
        sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success = 1")
            .fetch_one(&pool)
            .await?;
    // All Catabolic work, including helper waits, stays off async executor threads.
    // The Motion SQLx pool remains live throughout the native host lifecycle.
    let catabolic = tokio::task::spawn_blocking(move || -> Result<Value> {
        let initialized = migration::initialize(&catalog_db)?;
        let mut setup = Session::open(&catalog_db, true, false)?;
        let sqlite: String = setup
            .store()?
            .db
            .query_row("SELECT sqlite_version()", [], |row| row.get(0))?;
        let source_id: String = setup
            .store()?
            .db
            .query_row("SELECT sqlite_source_id()", [], |row| row.get(0))?;
        let math: f64 = setup
            .store()?
            .db
            .query_row("SELECT sqrt(9)", [], |row| row.get(0))?;
        assert_eq!(math, 3.0);
        let soundex: i64 = setup.store()?.db.query_row(
            "SELECT count(*) FROM pragma_function_list WHERE name = 'soundex'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(soundex, 0);
        // Trusted fixture setup provisions a saved query. Public host reads use
        // only the resulting QueryId, not raw SQL or caller-authored definitions.
        let query = definitions::query_put(
            setup.store()?,
            "default",
            "r0-all-items",
            &json!({"selection":{"language":"sql","query":"SELECT id AS item_id FROM items ORDER BY id"}}),
        )?;
        let query = QueryId::new(query["id"].as_str().ok_or("query identity missing")?)?;
        let empty_scan = catabolic_runtime::public_scan::execute(
            setup.store_mut()?,
            "default",
            &json!({}),
        )?;
        assert_eq!(empty_scan["complete"], true);
        if helper.is_some() {
            let source = catalog_db.parent().ok_or("missing catalog parent")?.join("source");
            std::fs::create_dir(&source)?;
            for i in 0..400 {
                std::fs::write(source.join(format!("fixture-{i}.txt")), "host scan fixture\n")?;
            }
            catabolic_store::observation::execute_detached(
                setup.store_mut()?,
                "default",
                "bind",
                &json!({"source":"r0-media","root":source}),
            )?;
        }
        setup.close()?;

        let mut host = HostSession::open(&catalog_db, "default", true, CONTRACT_VERSION)?;
        let identity = host.identity()?;
        assert_eq!(identity["contract_version"], CONTRACT_VERSION);
        assert!(identity.get("path").is_none());
        assert_eq!(host.select(&query, budget(), &Cancellation::default())?["ids"], json!([]));
        let inserted = host.put_items(
            &[
                json!({"id":"r0-b","kind":"movie","metadata":{"title":"R0 B"}}),
                json!({"id":"r0-a","kind":"movie","metadata":{"title":"R0 A"}}),
            ],
            &Cancellation::default(),
        )?;
        assert_eq!(inserted.len(), 2);
        assert_eq!(host.select(&query, budget(), &Cancellation::default())?["ids"], json!(["r0-a","r0-b"]));
        let cancelled = Cancellation::default();
        cancelled.cancel();
        assert_eq!(host.select(&query, budget(), &cancelled).unwrap_err().code, ErrorCode::Cancelled);
        host.begin_drain();
        assert_eq!(host.lifecycle(), Lifecycle::Draining);
        assert_eq!(host.select(&query, budget(), &Cancellation::default()).unwrap_err().code, ErrorCode::Conflict);
        assert!(host.drain(Duration::ZERO)?);
        assert_eq!(host.lifecycle(), Lifecycle::Closed);
        let mut reopened = HostSession::open(&catalog_db, "default", false, CONTRACT_VERSION)?;
        assert_eq!(reopened.identity()?, identity);
        assert_eq!(reopened.select(&query, budget(), &Cancellation::default())?["ids"], json!(["r0-a","r0-b"]));
        reopened.close()?;
        let helper_receipt = match (helper, helper_digest) {
            (Some(path), Some(digest)) => real_helper_lifecycle(&catalog_db, &path, &digest)?,
            _ => json!({"status":"not_requested"}),
        };
        Ok(json!({
            "sqlite_version":sqlite,
            "sqlite_source_id":source_id,
            "schema":initialized["schema"],
            "empty_scan":empty_scan,
            "host_contract_version":CONTRACT_VERSION,
            "host_bounded_read_mutation_cancel_drain_reopen":"passed",
            "host_reopened_catalog_identity_matches":true,
            "helper":helper_receipt
        }))
    }).await??;
    assert_eq!(json!(motion_sqlite), catabolic["sqlite_version"]);
    assert_eq!(json!(motion_source), catabolic["sqlite_source_id"]);
    pool.close().await;
    let reopened = playscale::db::connect(&motion_db).await?;
    let after_reopen: i64 =
        sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success = 1")
            .fetch_one(&reopened)
            .await?;
    assert_eq!(motion_migrations, after_reopen);
    reopened.close().await;
    println!(
        "{}",
        json!({
            "result":"passed",
            "common_sqlite_version":motion_sqlite,
            "common_sqlite_source_id":motion_source,
            "motion_applied_migrations":motion_migrations,
            "catabolic":catabolic,
            "math_functions":true,
            "soundex":false,
            "both_databases_reopened":true
        })
    );
    Ok(())
}
