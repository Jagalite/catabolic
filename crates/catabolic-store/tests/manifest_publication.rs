//! Manifest publication builds and writes inside one writer transaction.
//! Disposable native catalog and output only; no live catalog or media.
use catabolic_store::{Store, manifest, migration};
use serde_json::{Value, json};
use std::{cell::RefCell, time::Duration};

#[test]
fn no_writer_can_interleave_between_the_build_and_the_write() {
    let root = tempfile::tempdir().unwrap();
    // Publication refuses symlinked path components, and /var is one on macOS.
    let base = root.path().canonicalize().unwrap();
    let path = base.join("catalog.db");
    migration::initialize_native_at(&path, 34).unwrap();
    let output = base.join("manifest.json");
    let options = json!({"output": output.to_str().unwrap()});
    let observed = RefCell::new(Vec::new());
    let store = Store::open(&path, true, false).unwrap();
    let result = manifest::publish_between(store, "default", "global", None, &options, |held| {
        // The hook runs where the old code had no lock at all.
        assert!(held.owns_writer_lock());
        assert!(
            !held.db.is_autocommit(),
            "build and write share one transaction"
        );
        let second = Store::open(&path, true, false);
        observed.borrow_mut().push(format!(
            "store:{}",
            second.err().map(|e| e.0).unwrap_or_default()
        ));
        let raw = rusqlite::Connection::open(&path).unwrap();
        raw.busy_timeout(Duration::ZERO).unwrap();
        let begun = raw.execute_batch("BEGIN IMMEDIATE");
        observed.borrow_mut().push(format!(
            "raw:{}",
            begun.err().map(|e| e.to_string()).unwrap_or_default()
        ));
    })
    .unwrap();
    let observed = observed.into_inner();
    assert!(
        observed[0].contains("another Catabolic writer"),
        "a second Catabolic writer must be refused: {observed:?}"
    );
    assert!(
        observed[1].contains("locked"),
        "a raw writer must be refused: {observed:?}"
    );
    assert_eq!(result["written"], true);
    let published: Value = serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
    assert_eq!(published["format"], "catabolic.catalog-manifest");
    // Everything is released afterwards.
    let again = Store::open(&path, true, false).unwrap();
    assert!(again.db.is_autocommit());
}

#[test]
fn a_failed_build_releases_the_lock_and_publishes_nothing() {
    let root = tempfile::tempdir().unwrap();
    // Publication refuses symlinked path components, and /var is one on macOS.
    let base = root.path().canonicalize().unwrap();
    let path = base.join("catalog.db");
    migration::initialize_native_at(&path, 34).unwrap();
    let output = base.join("manifest.json");
    let options = json!({"output": output.to_str().unwrap()});
    let store = Store::open(&path, true, false).unwrap();
    let error = manifest::build_and_publish(store, "default", "no-such-catalog", None, &options)
        .unwrap_err();
    assert!(error.0.contains("unknown catalog"), "{}", error.0);
    assert!(!output.exists());
    assert!(Store::open(&path, true, false).is_ok());
}
