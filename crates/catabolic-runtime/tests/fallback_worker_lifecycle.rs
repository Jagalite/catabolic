// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Ordinary fallback worker ownership only; publication remains gated.
use catabolic_runtime::public_fallback;
use catabolic_store::{Store, migration, writer_lock};
use serde_json::json;
use std::{sync::atomic::AtomicBool, time::Duration};

#[test]
fn once_releases_owners_and_catalog_contention_is_only_a_report() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog");
    migration::initialize(&path).unwrap();
    let cancel = AtomicBool::new(false);
    let run = |profile| public_fallback::run(&path, profile, true, Duration::from_secs(1), &cancel);
    assert_eq!(
        run("default").unwrap(),
        json!({"processed":0,"complete":true,"projections":[]})
    );
    let store = Store::open(&path, true, false).unwrap();
    assert_eq!(
        run("missing").unwrap(),
        json!({"processed":0,"complete":false,"blockers":["catalog_busy"]})
    );
    drop(store);
    assert_eq!(run("missing").unwrap_err().0, "unknown profile: missing");
    assert!(run("default").is_ok());
    assert!(writer_lock(&root.path().join("catalog.fallback-worker")).is_ok());
}

#[test]
fn singleton_contention_precedes_profile_and_is_not_retry_report() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog");
    migration::initialize(&path).unwrap();
    let singleton = writer_lock(&root.path().join("catalog.fallback-worker")).unwrap();
    let result = public_fallback::run(
        &path,
        "missing",
        true,
        Duration::from_secs(1),
        &AtomicBool::new(false),
    );
    assert_eq!(
        result.unwrap_err().0,
        "another Catabolic writer or upgrade is using this database"
    );
    drop(singleton);
    assert!(
        public_fallback::run(
            &path,
            "default",
            true,
            Duration::from_secs(1),
            &AtomicBool::new(false)
        )
        .is_ok()
    );
}

#[test]
fn cancellation_before_tick_releases_singleton_without_catalog_mutation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog");
    migration::initialize(&path).unwrap();
    let result = public_fallback::run(
        &path,
        "default",
        false,
        Duration::from_secs(1),
        &AtomicBool::new(true),
    );
    assert_eq!(result.unwrap_err().0, "fallback worker cancelled");
    assert!(writer_lock(&root.path().join("catalog.fallback-worker")).is_ok());
    assert!(Store::open(&path, true, false).is_ok());
}
