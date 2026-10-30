// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Actual ticket debit and revocation against a disposable SQLite catalog.
#![cfg(unix)]
use catabolic_core::sha256;
use catabolic_store::{Store, access::Access, content_access, migration, observation};
use rusqlite::params;
use serde_json::json;

#[test]
fn ticket_debits_do_not_overspend_and_revocation_fences_next_debit() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("a.txt"), b"original bytes").unwrap();
    let db = root.path().join("catalog.sqlite3");
    migration::initialize(&db).unwrap();
    let store = Store::open(&db, true, false).unwrap();
    observation::execute(
        &store,
        "default",
        "bind",
        &json!({"source":"media","root":source}),
    )
    .unwrap();
    observation::execute(&store, "default", "scan", &json!({"source":"media"})).unwrap();
    store.transaction(|db|{
        db.execute_batch("INSERT INTO api_principals(id,profile) VALUES ('p','default'); INSERT INTO api_grants(id,principal,definition) VALUES ('g','p','{\"actions\":[\"*\"],\"operator\":true}'); INSERT INTO api_sources VALUES ('default','media');")?;
        db.execute("INSERT INTO api_tokens(id,principal,digest,grants,expires) VALUES ('t','p',?,'[\"g\"]',?)",params![sha256("t.secret"),catabolic_store::notifications::now()+1000.0])?;
        Ok(())
    }).unwrap();
    let file: String = store
        .db
        .query_row("SELECT id FROM files WHERE path='a.txt'", [], |r| r.get(0))
        .unwrap();
    let revision = content_access::revision_of(&store, "default", &file).unwrap();
    let mut access = Access::authenticate(store, "t.secret").unwrap();
    let ticket = content_access::ticket(&mut access, &file, &revision, 300, 10).unwrap();
    let id = ticket["ticket_id"].as_str().unwrap();
    content_access::debit_ticket(&access, id, 6).unwrap();
    assert!(content_access::debit_ticket(&access, id, 5).is_err());
    let remaining: i64 = access
        .store()
        .db
        .query_row(
            "SELECT remaining_bytes FROM api_tickets WHERE id=?",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 4);
    content_access::revoke_ticket(&access, id).unwrap();
    assert!(content_access::debit_ticket(&access, id, 1).is_err());
    let remaining: i64 = access
        .store()
        .db
        .query_row(
            "SELECT remaining_bytes FROM api_tickets WHERE id=?",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 4);
}
