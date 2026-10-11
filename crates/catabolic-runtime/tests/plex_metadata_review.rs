// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Independent metadata owner cleanup review, using a disposable empty catalog.
use catabolic_runtime::{api, plex_metadata};
use catabolic_store::Store;
use serde_json::json;
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::atomic::AtomicBool,
};

fn reusable(path: &Path) {
    let store = Store::open(path, true, false).unwrap();
    let events: i64 = store
        .db
        .query_row("SELECT count(*) FROM consumer_events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(events, 0);
    let mut publication = path.as_os_str().to_os_string();
    publication.push(".plex-metadata");
    let _lock = catabolic_store::writer_lock(Path::new(&publication)).unwrap();
}

#[test]
fn panic_in_owner_poll_releases_store_and_publication_locks() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("catalog.db");
    catabolic_store::migration::initialize(&path).unwrap();
    let request =
        json!({"item_ids":["fixture"],"fields":["title"],"apply":true,"expected_plan":"fixture"});
    let cancelled = AtomicBool::new(false);
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        api::execute_plex_metadata_with_poll(&path, "default", &request, &cancelled, &mut || {
            panic!("independent owner callback panic")
        })
    }));
    assert!(outcome.is_err());
    reusable(&path);
}

#[test]
fn pre_cancelled_owner_has_no_intent_and_releases_both_locks() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("catalog.db");
    catabolic_store::migration::initialize(&path).unwrap();
    let request =
        json!({"item_ids":["fixture"],"fields":["title"],"apply":true,"expected_plan":"fixture"});
    let cancelled = AtomicBool::new(true);
    let error =
        api::execute_plex_metadata_with_poll(&path, "default", &request, &cancelled, &mut || {})
            .unwrap_err();
    assert_eq!(error.0, plex_metadata::INTERRUPTED);
    reusable(&path);
}
