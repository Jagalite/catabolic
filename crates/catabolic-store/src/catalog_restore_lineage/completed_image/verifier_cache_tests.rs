//! Real owned-deserialize pager retirement, not an accounting discount or a
//! claim that an ordinary :memory: database has purgeable page storage.
use super::*;
use rusqlite::hooks::{AuthContext, Authorization};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

fn categories(db: &Connection) -> [u64; 3] {
    [rusqlite::ffi::SQLITE_DBSTATUS_CACHE_USED,
        rusqlite::ffi::SQLITE_DBSTATUS_SCHEMA_USED,
        rusqlite::ffi::SQLITE_DBSTATUS_STMT_USED].map(|operation| {
        let mut current = 0;
        let mut highwater = 0;
        // SAFETY: this exact live connection remains owned throughout the
        // read-only DBSTATUS call. Both output integers are valid, reset=false.
        let code = unsafe { rusqlite::ffi::sqlite3_db_status(db.handle(), operation,
            &mut current, &mut highwater, 0) };
        assert_eq!(code, rusqlite::ffi::SQLITE_OK);
        u64::try_from(current).unwrap()
    })
}

fn verifier() -> (Connection, u64, String) {
    let producer = Connection::open_in_memory().unwrap();
    producer.execute_batch("CREATE TABLE sample(id INTEGER PRIMARY KEY,value BLOB NOT NULL); WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<64) INSERT INTO sample SELECT x,zeroblob(32768) FROM n;").unwrap();
    let (data, bytes, digest) = {
        let serialized = producer.serialize(rusqlite::MAIN_DB).unwrap();
        let bytes = serialized.len() as u64;
        let digest = catabolic_core::sha256(&*serialized);
        let (data, actual) = take_owned_current_serialization(serialized, bytes).unwrap();
        assert_eq!(actual, bytes);
        (data, bytes, digest)
    };
    producer.close().unwrap();
    let mut verified = Connection::open_in_memory().unwrap();
    // Exactly the production flag choice: FREEONCLOSE | RESIZEABLE, not READONLY.
    verified.deserialize(rusqlite::MAIN_DB, data, false).unwrap();
    settings(&verified).unwrap();
    assert_eq!(warm(&verified), 64 * 32768 * 2);
    (verified, bytes, digest)
}

fn warm(db: &Connection) -> i64 {
    // hex forces every blob page to be read; length(blob) alone would not.
    db.query_row("SELECT sum(length(hex(value))) FROM sample", [], |row| row.get(0)).unwrap()
}

#[test]
fn owned_verifier_cache_retirement_preserves_backing_schema_statements_and_hooks() {
    let (verified, bytes, digest) = verifier();
    let handle = unsafe { verified.handle() };
    let statement = verified.prepare("SELECT id FROM sample WHERE id=?1").unwrap();
    let shared = verified.serialize(rusqlite::MAIN_DB).unwrap();
    assert!(matches!(&shared, rusqlite::serialize::Data::Shared(_)));
    assert_eq!(shared.len() as u64, bytes);
    assert_eq!(catabolic_core::sha256(&*shared), digest);
    let pointer = shared.as_ptr();
    let authorization = Arc::new(AtomicUsize::new(0));
    let observed = authorization.clone();
    verified.authorizer(Some(move |_: AuthContext<'_>| {
        observed.fetch_add(1, Ordering::SeqCst);
        Authorization::Deny
    })).unwrap();
    let progress = Arc::new(AtomicUsize::new(0));
    let observed = progress.clone();
    verified.progress_handler(1, Some(move || {
        observed.fetch_add(1, Ordering::SeqCst);
        true
    })).unwrap();
    let before = categories(&verified);
    assert!(before[2] > 0, "a real prepared VM must stay alive through retirement");
    release_verified_cache(&verified).unwrap();
    let after = categories(&verified);
    assert!(after[0] < before[0], "actual clean verifier pages must retire: {before:?} -> {after:?}");
    assert_eq!(after[1..], before[1..], "schema and live statements remain additive");
    assert_eq!(authorization.load(Ordering::SeqCst), 0, "retirement must run no SQL or replace the authorizer");
    assert_eq!(progress.load(Ordering::SeqCst), 0, "retirement must not invoke or suppress the existing handler");
    assert_eq!(unsafe { verified.handle() }, handle);
    assert!(verified.is_autocommit());
    assert_eq!(verified.transaction_state(Some("main")).unwrap(), rusqlite::TransactionState::None);
    assert_eq!(shared.as_ptr(), pointer);
    assert_eq!(shared.len() as u64, bytes);
    assert_eq!(catabolic_core::sha256(&*shared), digest);
    release_verified_cache(&verified).unwrap();
    assert_eq!(categories(&verified), after, "repeating an actual release creates no fictitious credit");
    assert!(verified.prepare("SELECT 1").is_err(), "the original deny hook remains installed");
    assert!(authorization.load(Ordering::SeqCst) > 0);
    drop(shared);
    drop(statement);
    verified.authorizer(None::<fn(AuthContext<'_>) -> Authorization>).unwrap();
    assert!(verified.query_row("SELECT 1", [], |_| Ok(())).is_err(), "the real interrupting progress hook remains installed");
    assert!(progress.load(Ordering::SeqCst) > 0);
    verified.progress_handler(0, None::<fn() -> bool>).unwrap();
    assert_eq!(warm(&verified), 64 * 32768 * 2);
    assert_eq!(catabolic_core::sha256(&*verified.serialize(rusqlite::MAIN_DB).unwrap()), digest);
}

#[test]
fn owned_verifier_cache_retirement_allows_real_regrowth_and_resizeable_mutation() {
    let (verified, bytes, digest) = verifier();
    release_verified_cache(&verified).unwrap();
    let cold = categories(&verified);
    let cold_total = bytes + crate::catalog_backup::allocations(&verified).unwrap();
    assert_eq!(warm(&verified), 64 * 32768 * 2);
    let hot = categories(&verified);
    assert!(hot[0] > cold[0], "later reads must really repopulate this same verifier");
    assert_eq!(hot[1..], cold[1..]);
    assert!(bytes + crate::catalog_backup::allocations(&verified).unwrap() > cold_total,
        "new real cache stays additive to the unchanged backing");
    assert_eq!(catabolic_core::sha256(&*verified.serialize(rusqlite::MAIN_DB).unwrap()), digest);
    // Later rotation relies on a writable/resizable private copy. Force growth
    // beyond the original owned buffer rather than merely changing a scalar.
    verified.execute("INSERT INTO sample VALUES(65,zeroblob(4194304))", []).unwrap();
    assert_eq!(warm(&verified), (64 * 32768 + 4194304) * 2);
    let changed = {
        let data = verified.serialize(rusqlite::MAIN_DB).unwrap();
        assert!(matches!(&data, rusqlite::serialize::Data::Shared(_)));
        assert!(data.len() as u64 > bytes);
        let changed = catabolic_core::sha256(&*data);
        assert_ne!(changed, digest);
        changed
    };
    release_verified_cache(&verified).unwrap();
    assert_eq!(verified.query_row("SELECT count(*) FROM sample", [], |r| r.get::<_, i64>(0)).unwrap(), 65);
    assert_eq!(verified.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0)).unwrap(), "ok");
    assert_eq!(catabolic_core::sha256(&*verified.serialize(rusqlite::MAIN_DB).unwrap()), changed);
}

#[test]
fn owned_verifier_cache_retirement_refuses_live_read_or_write_transactions() {
    let (verified, _, _) = verifier();
    verified.execute_batch("BEGIN").unwrap();
    assert_eq!(warm(&verified), 64 * 32768 * 2);
    let before = categories(&verified);
    assert!(release_verified_cache(&verified).is_err());
    assert_eq!(categories(&verified), before);
    assert_eq!(verified.transaction_state(Some("main")).unwrap(), rusqlite::TransactionState::Read);
    verified.execute_batch("ROLLBACK; BEGIN IMMEDIATE; UPDATE sample SET value=x'01' WHERE id=1").unwrap();
    let before = categories(&verified);
    assert!(release_verified_cache(&verified).is_err());
    assert_eq!(categories(&verified), before);
    assert_eq!(verified.transaction_state(Some("main")).unwrap(), rusqlite::TransactionState::Write);
    assert_eq!(verified.query_row("SELECT hex(value) FROM sample WHERE id=1", [], |r| r.get::<_, String>(0)).unwrap(), "01");
    verified.execute_batch("ROLLBACK").unwrap();
    release_verified_cache(&verified).unwrap();
    assert_eq!(warm(&verified), 64 * 32768 * 2);
}

#[test]
fn owned_verifier_cache_retirement_never_assumes_ordinary_memory_pages_are_freed() {
    let memory = Connection::open_in_memory().unwrap();
    memory.execute_batch("CREATE TABLE sample(value BLOB); INSERT INTO sample VALUES(zeroblob(262144))").unwrap();
    assert_eq!(warm(&memory), 262144 * 2);
    let before = categories(&memory);
    release_verified_cache(&memory).unwrap();
    assert_eq!(categories(&memory), before, "ordinary :memory: storage has no independent backing to discard");
    assert_eq!(warm(&memory), 262144 * 2);
}
