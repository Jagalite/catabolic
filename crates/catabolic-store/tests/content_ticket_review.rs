// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Actual ticket debit and revocation against a disposable SQLite catalog.
#![cfg(unix)]
use catabolic_core::sha256;
use catabolic_store::{Store, access::Access, content_access, migration, observation};
use rusqlite::params;
use serde_json::json;

fn fixture(allowance: u64) -> (tempfile::TempDir, Access, String, String, String) {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("a.txt"), b"original bytes".repeat(6000)).unwrap();
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
    let ticket = content_access::ticket(&mut access, &file, &revision, 300, allowance).unwrap();
    let id = ticket["ticket_id"].as_str().unwrap().to_owned();
    (root, access, file, revision, id)
}

#[test]
fn ticket_debits_do_not_overspend_and_revocation_fences_next_debit() {
    let (_root, access, _file, _revision, id) = fixture(10);
    let id = id.as_str();
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

#[test]
fn rollback_and_interrupted_delivery_have_distinct_committed_allowance() {
    let (_root, access, _file, _revision, id) = fixture(10);
    let path = access.store().path.clone();
    let rollback: catabolic_store::Result<()> = access.store().transaction(|_| {
        content_access::debit_ticket(&access, &id, 6)?;
        Err(catabolic_store::Error(
            "controlled interruption before commit".into(),
        ))
    });
    assert!(rollback.is_err());
    drop(access);
    let access =
        Access::authenticate(Store::open(&path, true, false).unwrap(), "t.secret").unwrap();
    assert_eq!(remaining(&access, &id), 10);
    content_access::debit_ticket(&access, &id, 6).unwrap();
    // Closing without delivering any body is not authority to refund a committed debit.
    drop(access);
    let access =
        Access::authenticate(Store::open(&path, true, false).unwrap(), "t.secret").unwrap();
    assert_eq!(remaining(&access, &id), 4);
    assert!(content_access::debit_ticket(&access, &id, 6).is_err());
    assert_eq!(remaining(&access, &id), 4);
}

fn remaining(access: &Access, id: &str) -> i64 {
    access
        .store()
        .db
        .query_row(
            "SELECT remaining_bytes FROM api_tickets WHERE id=?",
            [id],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn competing_catalog_owners_commit_one_shared_allowance_debit() {
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};
    let (_root, access, _file, _revision, id) = fixture(10);
    let path = access.store().path.clone();
    drop(access);
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            let id = id.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let deadline = Instant::now() + Duration::from_secs(10);
                let store = loop {
                    match Store::open(&path, true, false) {
                        Ok(store) => break store,
                        Err(error)
                            if Instant::now() < deadline
                                && error.0.contains("another Catabolic writer or upgrade") =>
                        {
                            std::thread::sleep(Duration::from_millis(5))
                        }
                        Err(error) => panic!("catalog admission failed: {error}"),
                    }
                };
                let access = Access::authenticate(store, "t.secret").unwrap();
                content_access::debit_ticket(&access, &id, 6).is_ok()
            })
        })
        .collect();
    let accepted = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .filter(|accepted| *accepted)
        .count();
    assert_eq!(accepted, 1);
    let access =
        Access::authenticate(Store::open(&path, true, false).unwrap(), "t.secret").unwrap();
    assert_eq!(remaining(&access, &id), 4);
}

#[test]
fn transfer_revalidates_every_authority_dimension_before_final_chunk() {
    for mutation in ["ticket", "expiry", "grant", "token", "owner", "revision"] {
        let (_root, access, file, revision, id) = fixture(100_000);
        let path = access.store().path.clone();
        let id_for_debit = id.clone();
        let id_for_delivery = id.clone();
        let result = content_access::with_transfer(access, &file, &revision, Some(&id),
            |access, stat, _| { content_access::debit_ticket(access, &id_for_debit, stat.len())?; Ok(stat.len()) },
            |file, stat, evidence, length| {
                let mut body = evidence.body(file, stat, 0, length)?;
                assert_eq!(body.next_chunk()?.len(), 65536);
                let store = Store::open(&path, true, false)?;
                store.transaction(|db| {
                    match mutation {
                        "ticket" => { db.execute("UPDATE api_tickets SET revoked=1 WHERE id=?", [&id_for_delivery])?; }
                        "expiry" => { db.execute("UPDATE api_tickets SET expires=0 WHERE id=?", [&id_for_delivery])?; }
                        "grant" => { db.execute("UPDATE api_grants SET enabled=0 WHERE id='g'", [])?; }
                        "token" => { db.execute("UPDATE api_tokens SET revoked=1 WHERE id='t'", [])?; }
                        "owner" => { db.execute("INSERT INTO api_principals(id,profile) VALUES('new-owner','default')", [])?;
                            db.execute("UPDATE api_tokens SET principal='new-owner' WHERE id='t'", [])?; }
                        "revision" => { db.execute("UPDATE observations SET size=size+1", [])?; }
                        _ => unreachable!(),
                    }
                    Ok(())
                })?;
                drop(store);
                assert!(body.next_chunk().is_err(), "{mutation} released a final chunk");
                Ok(length)
            }).unwrap();
        let store = Store::open(&path, false, false).unwrap();
        let remaining: i64 = store
            .db
            .query_row(
                "SELECT remaining_bytes FROM api_tickets WHERE id=?",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            remaining,
            100_000 - result as i64,
            "{mutation} refunded a committed transfer"
        );
    }
}
