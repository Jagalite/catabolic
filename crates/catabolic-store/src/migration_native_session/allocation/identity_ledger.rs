//! Fixed identity-ledger capture, not a general SQL measurement cache.
use super::*;

#[derive(Clone, Copy)]
pub(in crate::migration::native_session) enum IdentityLedger {
    Current,
    Original,
}
impl IdentityLedger {
    fn sql(self) -> &'static str {
        match self {
            Self::Current => {
                "SELECT version,filename,checksum FROM main.schema_migrations ORDER BY version"
            }
            Self::Original => {
                "SELECT version,filename,checksum,origin,applied_at FROM main.schema_migrations WHERE version<=40 ORDER BY version"
            }
        }
    }
    fn fallback_sql(self) -> &'static str {
        match self {
            Self::Current => {
                "SELECT version,filename,checksum FROM schema_migrations ORDER BY version"
            }
            Self::Original => {
                "SELECT version,filename,checksum,origin,applied_at FROM schema_migrations WHERE version<=40 ORDER BY version"
            }
        }
    }
}
fn eligible(context: &AllocationContext, db: &Connection) -> Result<bool> {
    // SAFETY: read only the identity of this live, exclusively borrowed
    // connection. No FFI call or native-pointer dereference is performed.
    let handle = unsafe { db.handle() } as usize;
    Ok(
        db.transaction_state(Some("main"))? == rusqlite::TransactionState::Write
            && context.0.lock().map_err(|_| refused())?.sqlite_handle == Some(handle),
    )
}

pub(in crate::migration::native_session) fn identity_ledger_rows(
    db: &Connection,
    projection: IdentityLedger,
) -> Result<Vec<Value>> {
    if let Some(context) = current() {
        if eligible(&context, db)? {
            return capture(&context, db, projection);
        }
    }
    // Autocommit, READ, foreign references and callers without the closed
    // retained allocation owner keep the complete existing measurement path.
    crate::migration::rows(db, projection.fallback_sql(), &[])
}

fn capture(
    context: &AllocationContext,
    db: &Connection,
    projection: IdentityLedger,
) -> Result<Vec<Value>> {
    if !eligible(context, db)? {
        return Err(Error(
            "identity ledger requires exact retained WRITE connection".into(),
        ));
    }
    // Both callers first validate the closed main schema. This is a fixed
    // ordinary base-table SELECT: no UDF, virtual table, trigger or supplied SQL.
    // The private completed owner never gives this Connection to a callback.
    // WRITE prevents other connections changing main's schema; this lexical
    // cursor executes no schema/namespace operations on this connection.
    // This proof DOES NOT extend to generic migration::rows or caller-held Store.
    let mut statement = db.prepare(projection.sql())?;
    if !statement.readonly() {
        return Err(refused());
    }
    let count = statement.column_count();
    let mut name_bytes = 0_u64;
    for i in 0..count {
        name_bytes = name_bytes
            .checked_add(statement.column_name(i)?.len() as u64)
            .and_then(|n| n.checked_add(1024))
            .ok_or_else(refused)?;
    }
    let mut owned = context.reserve(db, name_bytes)?;
    let names = (0..count)
        .map(|i| statement.column_name(i).map(str::to_owned))
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut cursor = statement.query([])?;
    let mut result = Vec::new();
    let mut admitted = name_bytes;
    let mut schema = None;
    while let Some(row) = cursor.next()? {
        // First sqlite3_step settles prepare/reprepare and schema loading.
        // The measured component exists only for this fixed cursor's lifetime.
        let schema = match schema {
            Some(bytes) => bytes,
            None => {
                let bytes = crate::catalog_backup::schema_allocations(db)?;
                schema = Some(bytes);
                bytes
            }
        };
        let bytes = row_bytes(row, count)?
            .checked_add(name_bytes)
            .ok_or_else(refused)?;
        admitted = admitted
            .checked_add(bytes)
            .filter(|n| *n <= MIGRATION_SCRATCH)
            .ok_or_else(|| {
                Error("offline retained metadata rows exceed bounded migration scratch".into())
            })?;
        grow(&mut owned, db, bytes, schema)?;
        let mut object = serde_json::Map::new();
        for (i, name) in names.iter().enumerate() {
            object.insert(name.clone(), crate::migration::value(row.get_ref(i)?));
        }
        result.push(Value::Object(object));
    }
    // Full terminal measurement while output, names and VM are still retained.
    // Error/unwind drops the ordinary scratch guard without touching any work
    // budget, deadline, transaction, or outer operation attachment.
    context.check(Some(db), 0)?;
    Ok(result)
}

// Private to this fixed loop. Never exported as a reusable measurement token.
fn grow(owned: &mut AllocationGuard, db: &Connection, bytes: u64, schema: u64) -> Result<()> {
    if !eligible(&owned.context, db)? {
        return Err(refused());
    }
    let measured = add(schema, crate::catalog_backup::dynamic_allocations(db)?)?;
    let GuardKind::Temporary(ref mut retained) = owned.kind else {
        return Err(refused());
    };
    let mut state = owned.context.0.lock().map_err(|_| refused())?;
    state.sqlite = measured;
    AllocationContext::total(&state, bytes)?;
    state.temporary = add(state.temporary, bytes)?;
    *retained = add(*retained, bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Connection, AllocationContext) {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,filename TEXT,checksum TEXT,origin TEXT,applied_at TEXT); INSERT INTO schema_migrations VALUES(40,'old','a','migrated','then'),(41,'new','b','migrated','now');").unwrap();
        let context = AllocationContext::default();
        context.bind_source(&db).unwrap();
        (db, context)
    }
    #[test]
    fn fixed_ledger_both_projections_match_and_ignore_temp_shadow() {
        let (db, context) = fixture();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        for projection in [IdentityLedger::Current, IdentityLedger::Original] {
            let expected = crate::migration::rows(&db, projection.sql(), &[]).unwrap();
            assert_eq!(capture(&context, &db, projection).unwrap(), expected);
        }
        db.execute_batch("CREATE TEMP TABLE schema_migrations(version,filename,checksum,origin,applied_at); INSERT INTO temp.schema_migrations VALUES(99,'shadow','x','x','x');").unwrap();
        assert_eq!(
            capture(&context, &db, IdentityLedger::Current).unwrap()[0]["version"],
            40
        );
        assert_eq!(context.0.lock().unwrap().temporary, 0);
        db.execute_batch("ROLLBACK").unwrap();
    }
    #[test]
    fn fixed_ledger_refuses_foreign_nonwrite_and_wrapper_preserves_fallback() {
        let (db, context) = fixture();
        let (foreign, _) = fixture();
        foreign.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert!(capture(&context, &foreign, IdentityLedger::Current).is_err());
        for begin in ["", "BEGIN", "BEGIN; SELECT * FROM schema_migrations"] {
            if !begin.is_empty() {
                db.execute_batch(begin).unwrap();
            }
            assert!(capture(&context, &db, IdentityLedger::Current).is_err());
            let _scope = context.enter().unwrap();
            assert_eq!(
                identity_ledger_rows(&db, IdentityLedger::Current)
                    .unwrap()
                    .len(),
                2
            );
            if !db.is_autocommit() {
                db.execute_batch("ROLLBACK").unwrap();
            }
        }
    }
    #[test]
    fn fixed_ledger_resamples_new_schema_and_live_vm_allocations() {
        let (db, context) = fixture();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        capture(&context, &db, IdentityLedger::Current).unwrap();
        let first = context.0.lock().unwrap().sqlite;
        db.execute_batch("CREATE TABLE additional(a,b,c,d,e,f,g,h,i,j); CREATE INDEX more ON additional(a,b,c,d,e,f,g,h,i,j);").unwrap();
        capture(&context, &db, IdentityLedger::Current).unwrap();
        assert!(context.0.lock().unwrap().sqlite > first);
        let schema = crate::catalog_backup::schema_allocations(&db).unwrap();
        let mut owned = context.reserve(&db, 0).unwrap();
        grow(&mut owned, &db, 0, schema).unwrap();
        {
            let mut state = context.0.lock().unwrap();
            state.external = LIMIT - state.sqlite - 512;
        }
        let sql = format!("SELECT '{}'", "x".repeat(16384));
        let statement = db.prepare(&sql).unwrap();
        assert!(grow(&mut owned, &db, 0, schema).is_err());
        drop(statement);
        context.0.lock().unwrap().external = 0;
        drop(owned);
        assert_eq!(context.0.lock().unwrap().temporary, 0);
        db.execute_batch("ROLLBACK").unwrap();
    }
    #[test]
    fn fixed_ledger_row_refusal_releases_scratch_and_keeps_writer() {
        let (db, context) = fixture();
        db.execute_batch("BEGIN IMMEDIATE; UPDATE schema_migrations SET filename=CAST(zeroblob(2000000) AS TEXT) WHERE version=40").unwrap();
        assert!(capture(&context, &db, IdentityLedger::Current).is_err());
        assert_eq!(context.0.lock().unwrap().temporary, 0);
        assert_eq!(
            db.transaction_state(Some("main")).unwrap(),
            rusqlite::TransactionState::Write
        );
        db.execute_batch("ROLLBACK; BEGIN IMMEDIATE").unwrap();
        assert_eq!(
            capture(&context, &db, IdentityLedger::Current)
                .unwrap()
                .len(),
            2
        );
        db.execute_batch("ROLLBACK").unwrap();
    }
    #[test]
    fn fixed_ledger_live_file_cache_growth_still_refuses() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ledger.sqlite");
        {
            let db = Connection::open(&path).unwrap();
            db.execute_batch("CREATE TABLE cache_payload(payload BLOB); WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<64) INSERT INTO cache_payload SELECT zeroblob(8192) FROM n;").unwrap();
        }
        // Reopen to start with a genuinely cold SQLite page cache, not an OS
        // cache assumption. No schema changes occur between these measurements.
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA cache_size=512; BEGIN IMMEDIATE")
            .unwrap();
        // Resolve the real table before the schema baseline, without fetching
        // its data pages; this test isolates changing CACHE_USED, not loading.
        drop(
            db.prepare("SELECT payload FROM main.cache_payload LIMIT 0")
                .unwrap(),
        );
        let context = AllocationContext::default();
        context.bind_source(&db).unwrap();
        let schema = crate::catalog_backup::schema_allocations(&db).unwrap();
        let mut owned = context.reserve(&db, 0).unwrap();
        grow(&mut owned, &db, 0, schema).unwrap();
        let before = crate::catalog_backup::dynamic_allocations(&db).unwrap();
        {
            let mut state = context.0.lock().unwrap();
            state.external = LIMIT - state.sqlite - 16 * 1024;
        }
        // hex forces actual overflow-page content reads; length(blob) alone
        // could be answered from record headers without loading those pages.
        let bytes: i64 = db
            .query_row(
                "SELECT sum(length(hex(payload))) FROM cache_payload",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(bytes, 64 * 8192 * 2);
        assert_eq!(
            crate::catalog_backup::schema_allocations(&db).unwrap(),
            schema
        );
        assert!(crate::catalog_backup::dynamic_allocations(&db).unwrap() > before + 16 * 1024);
        assert!(grow(&mut owned, &db, 0, schema).is_err());
        context.0.lock().unwrap().external = 0;
        drop(owned);
        assert_eq!(context.0.lock().unwrap().temporary, 0);
        db.execute_batch("ROLLBACK").unwrap();
    }
    #[test]
    fn fixed_ledger_unwind_releases_actual_row_reservation() {
        let (db, context) = fixture();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        let entered = std::cell::Cell::new(false);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut owned = context.reserve(&db, 4096).unwrap();
            let schema = crate::catalog_backup::schema_allocations(&db).unwrap();
            grow(&mut owned, &db, 8192, schema).unwrap();
            entered.set(true);
            panic!("intentional after admitted row reservation");
        }));
        assert!(entered.get());
        assert!(outcome.is_err());
        assert_eq!(context.0.lock().unwrap().temporary, 0);
        assert_eq!(
            db.transaction_state(Some("main")).unwrap(),
            rusqlite::TransactionState::Write
        );
        assert_eq!(
            capture(&context, &db, IdentityLedger::Current)
                .unwrap()
                .len(),
            2
        );
        db.execute_batch("ROLLBACK").unwrap();
    }
}


/// The returned digest keeps its own output coverage until the caller finishes
/// comparing it. No row or encoded array escapes the active metadata scratch.
pub(in crate::migration::native_session) struct OriginalIdentityDigest {
    digest: String,
    _retention: AllocationGuard,
}
impl OriginalIdentityDigest {
    pub(in crate::migration::native_session) fn as_str(&self) -> &str { &self.digest }
}

/// Fixed, fresh original-ledger hash. This is not generic SQL, an authority
/// cache, or a discount on ordinary rows. Every row keeps the exact existing
/// row_bytes convention (including its encoder transients) until hashing ends.
pub(in crate::migration::native_session) fn original_identity_digest(
    db: &Connection, metadata: &MetadataReservation<'_>,
) -> Result<OriginalIdentityDigest> {
    #[cfg(test)]
    { original_identity_digest_inner(db, metadata, false) }
    #[cfg(not(test))]
    { original_identity_digest_inner(db, metadata) }
}
fn original_identity_digest_inner(
    db: &Connection, metadata: &MetadataReservation<'_>, #[cfg(test)] panic_after_row: bool,
) -> Result<OriginalIdentityDigest> {
    use sha2::{Digest, Sha256};
    if !matches!(metadata, MetadataReservation::Control(_)) { return Err(refused()); }
    let context = metadata.context();
    let handle = unsafe { db.handle() } as usize;
    {
        let state = context.0.lock().map_err(|_| refused())?;
        if state.sqlite_handle != Some(handle) || !state.control_loan || !state.control_scratch {
            return Err(refused());
        }
    }
    // Preserve the whole-input preflight, including rows beyond version40.
    // It bounds schema/ledger/identity inputs under this same live scratch.
    catalog_preflight(db)?;
    let retention = context.reserve(db, 64 + std::mem::size_of::<String>() as u64)?;
    const NAMES: [&str; 5] = ["version", "filename", "checksum", "origin", "applied_at"];
    let mut statement = db.prepare(IdentityLedger::Original.sql())?;
    if !statement.readonly() || statement.column_count() != NAMES.len() { return Err(refused()); }
    for (index, expected) in NAMES.iter().enumerate() {
        if statement.column_name(index)? != *expected { return Err(refused()); }
    }
    let name_bytes = NAMES.iter().try_fold(0u64, |n, name| add(n, name.len() as u64 + 1024))?;
    let _names = context.reserve(db, name_bytes)?;
    let mut cursor = statement.query([])?;
    let mut hash = Sha256::new();
    hash.update(b"[");
    let mut count = 0u64;
    let mut input_bytes = 0u64;
    let mut admitted = name_bytes;
    let mut schema = None;
    let mut schema_checked = false;
    while let Some(row) = cursor.next()? {
        // Reuse only the existing fixed WRITE-cursor proof. The first step has
        // settled schema loading/reprepare; this base-table cursor executes no
        // other SQL, UDF or namespace operation. No owner connection escapes to
        // a callback. Main's WRITE transaction excludes competing schema edits.
        // READ/autocommit retain full measurement, not an inferred cache grant.
        if !schema_checked {
            if eligible(context, db)? {
                schema = Some(crate::catalog_backup::schema_allocations(db)?);
            }
            schema_checked = true;
        }
        count = count.checked_add(1).filter(|n| *n <= 64).ok_or_else(refused)?;
        for index in 1..NAMES.len() {
            let length = match row.get_ref(index)? {
                rusqlite::types::ValueRef::Text(v) | rusqlite::types::ValueRef::Blob(v) => v.len() as u64,
                rusqlite::types::ValueRef::Null => 0,
                _ => 32, // conservative bound for SQLite numeric text
            };
            input_bytes = add(input_bytes, length)?;
            if input_bytes > 1024 * 1024 { return Err(refused()); }
        }
        let bytes = add(row_bytes(row, NAMES.len())?, name_bytes)?;
        admitted = add(admitted, bytes)?;
        if admitted > MIGRATION_SCRATCH { return Err(refused()); }
        // Preadmit the same full per-row price before constructing either the
        // Value or its canonical encoding. They die before this guard drops.
        let _row = match schema {
            Some(schema) => {
                // Zero initial coverage is not a credit: grow performs the
                // same full byte admission with fresh CACHE_USED/STMT_USED.
                let mut row = AllocationGuard {
                    context: context.clone(),
                    kind: GuardKind::Temporary(0),
                    attachment: None,
                };
                grow(&mut row, db, bytes, schema)?;
                row
            }
            None => context.reserve(db, bytes)?,
        };
        let mut object = serde_json::Map::new();
        for (index, name) in NAMES.iter().enumerate() {
            object.insert((*name).to_owned(), crate::migration::value(row.get_ref(index)?));
        }
        let value = Value::Object(object);
        let encoded = catabolic_core::encode(&value);
        #[cfg(test)]
        if panic_after_row { panic!("controlled original-ledger row unwind"); }
        if count != 1 { hash.update(b", "); }
        hash.update(encoded.as_bytes());
        // Sample actual dynamic source growth while VM, row and encoding
        // remain live. Recheck the exact source/WRITE gate on every reuse.
        match schema {
            Some(schema) => {
                let mut check = AllocationGuard {
                    context: context.clone(),
                    kind: GuardKind::Temporary(0),
                    attachment: None,
                };
                grow(&mut check, db, 0, schema)?;
            }
            None => context.check(Some(db), 0)?,
        }
    }
    hash.update(b"]");
    let digest = format!("{:x}", hash.finalize());
    context.check(Some(db), 0)?;
    Ok(OriginalIdentityDigest { digest, _retention: retention })
}

#[cfg(test)]
mod streamed_tests {
    use super::*;
    fn fixture() -> (Connection, OfflineControlWorkspaceBacking, AllocationContext) {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE schema_migrations(version,filename,checksum,origin,applied_at); CREATE TABLE catalog_state(catalog_id,incarnation); CREATE TABLE meta(key,value);").unwrap();
        let mut backing = OfflineControlWorkspaceBacking::new();
        let context = backing.admit(OfflineAllocationReservation::default()).unwrap();
        context.bind_source(&db).unwrap();
        (db, backing, context)
    }
    fn parity(db: &Connection, backing: &mut OfflineControlWorkspaceBacking, context: &AllocationContext) {
        let rows = crate::migration::rows(db, IdentityLedger::Original.sql(), &[]).unwrap();
        let wire = catabolic_core::encode(&serde_json::json!(&rows));
        let framed = format!("[{}]", rows.iter().map(catabolic_core::encode).collect::<Vec<_>>().join(", "));
        assert_eq!(wire.as_bytes(), framed.as_bytes());
        let expected = catabolic_core::sha256(wire);
        let before = context.temporary_bytes_for_test().unwrap();
        let mut loan = backing.loan(context, db).unwrap();
        let metadata = MetadataReservation::new(context, db, Some(&mut loan)).unwrap();
        let digest = original_identity_digest(db, &metadata).unwrap();
        assert_eq!(digest.as_str(), expected);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), before + 64 + std::mem::size_of::<String>() as u64);
        drop(metadata); drop(loan);
        assert_eq!(digest.as_str(), expected);
        drop(digest);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), before);
    }
    #[test]
    fn streamed_original_ledger_matches_empty_maximum_order_and_scalar_encoding() {
        let (db, mut backing, context) = fixture();
        parity(&db, &mut backing, &context);
        // Reversed insertion, mixed original/new rows, escaping, Unicode,
        // NULL/integer/real/blob/nonfinite and lossy UTF8 use migration::value.
        db.execute_batch("INSERT INTO schema_migrations VALUES(41,'excluded','x','new','now'); INSERT INTO schema_migrations VALUES(2,CAST(x'ff' AS TEXT),x'00ff',NULL,2.5); INSERT INTO schema_migrations VALUES(1,'quote\" slash\\ line'||char(10)||'雪',12,'migrated','now');").unwrap();
        db.execute("INSERT INTO schema_migrations VALUES(3,?1,?2,?3,?4)", rusqlite::params![f64::INFINITY, f64::NEG_INFINITY, -17i64, "\t\r\u{0001}"]).unwrap();
        parity(&db, &mut backing, &context);
        db.execute_batch("DELETE FROM schema_migrations").unwrap();
        for version in (-23..=40).rev() {
            db.execute("INSERT INTO schema_migrations VALUES(?1,'file','checksum','migrated','date')", [version]).unwrap();
        }
        parity(&db, &mut backing, &context);
        db.execute("INSERT INTO schema_migrations VALUES(41,'extra','x','new','date')", []).unwrap();
        let mut loan = backing.loan(&context, &db).unwrap();
        let metadata = MetadataReservation::new(&context, &db, Some(&mut loan)).unwrap();
        assert!(original_identity_digest(&db, &metadata).is_err());
    }
    #[test]
    fn streamed_original_ledger_write_cursor_parity_resamples_each_new_schema() {
        let (db, mut backing, context) = fixture();
        db.execute_batch("INSERT INTO schema_migrations VALUES(2,'b','two','migrated','then'),(1,'a','one','migrated','now'); BEGIN IMMEDIATE").unwrap();
        assert!(eligible(&context, &db).unwrap());
        parity(&db, &mut backing, &context);
        let before = crate::catalog_backup::schema_allocations(&db).unwrap();
        db.execute_batch("CREATE TABLE added_between_cursors(a,b,c,d,e,f,g,h); CREATE INDEX added_index ON added_between_cursors(a,b,c,d,e,f,g,h)").unwrap();
        let after = crate::catalog_backup::schema_allocations(&db).unwrap();
        assert!(after > before);
        parity(&db, &mut backing, &context);
        // No schema measurement survives the invocation. Actual source usage
        // includes the new schema, with final full DBSTATUS still performed.
        assert!(context.0.lock().unwrap().sqlite >= after);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
        db.execute_batch("ROLLBACK; BEGIN; SELECT * FROM schema_migrations").unwrap();
        assert!(!eligible(&context, &db).unwrap());
        parity(&db, &mut backing, &context);
        db.execute_batch("ROLLBACK").unwrap();
        assert!(!eligible(&context, &db).unwrap());
        parity(&db, &mut backing, &context);
    }
    #[test]
    fn streamed_original_ledger_write_unwind_keeps_exact_loan_and_releases_rows() {
        let (db, mut backing, context) = fixture();
        db.execute_batch("INSERT INTO schema_migrations VALUES(1,'a','b','migrated','now'); BEGIN IMMEDIATE").unwrap();
        let mut loan = backing.loan(&context, &db).unwrap();
        let metadata = MetadataReservation::new(&context, &db, Some(&mut loan)).unwrap();
        let before = context.temporary_bytes_for_test().unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = original_identity_digest_inner(&db, &metadata, true);
        }));
        assert!(outcome.is_err());
        assert_eq!(context.temporary_bytes_for_test().unwrap(), before);
        assert!(context.0.lock().unwrap().control_loan);
        assert!(context.0.lock().unwrap().control_scratch);
        assert!(eligible(&context, &db).unwrap());
        db.progress_handler(1, Some(|| true)).unwrap();
        assert!(original_identity_digest(&db, &metadata).is_err());
        db.progress_handler(0, None::<fn() -> bool>).unwrap();
        assert_eq!(context.temporary_bytes_for_test().unwrap(), before);
        assert!(context.0.lock().unwrap().control_loan);
        assert!(context.0.lock().unwrap().control_scratch);
        drop(metadata);
        drop(loan);
        db.execute_batch("ROLLBACK").unwrap();
    }
    #[test]
    fn streamed_original_ledger_refuses_foreign_owned_scratch_and_oversized_input() {
        let (db, mut backing, context) = fixture();
        let (foreign, _other_backing, _) = fixture();
        let owned = MetadataReservation::new(&context, &db, None).unwrap();
        assert!(original_identity_digest(&db, &owned).is_err());
        drop(owned);
        let mut loan = backing.loan(&context, &db).unwrap();
        let metadata = MetadataReservation::new(&context, &db, Some(&mut loan)).unwrap();
        assert!(original_identity_digest(&foreign, &metadata).is_err());
        db.execute("INSERT INTO schema_migrations VALUES(1,?1,'x','migrated','now')", ["x".repeat(1024*1024+1)]).unwrap();
        let before = context.temporary_bytes_for_test().unwrap();
        assert!(original_identity_digest(&db, &metadata).is_err());
        assert_eq!(context.temporary_bytes_for_test().unwrap(), before);
    }
    #[test]
    fn streamed_original_ledger_interrupt_and_budget_refusal_keep_original_loan() {
        let (db, mut backing, context) = fixture();
        let mut loan = backing.loan(&context, &db).unwrap();
        let metadata = MetadataReservation::new(&context, &db, Some(&mut loan)).unwrap();
        db.execute_batch("INSERT INTO schema_migrations VALUES(1,'file','checksum','migrated','now')").unwrap();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            original_identity_digest_inner(&db, &metadata, true)
        }));
        assert!(panic.is_err());
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
        { let state = context.0.lock().unwrap(); assert!(state.control_loan && state.control_scratch); }
        db.progress_handler(1, Some(|| true)).unwrap();
        assert!(original_identity_digest(&db, &metadata).is_err());
        db.progress_handler(0, None::<fn() -> bool>).unwrap();
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
        // Respect the public constructor's32MiB floor, then retain an actual
        // same-context demand guard rather than forge a host reservation.
        let external = LIMIT - MIGRATION_SCRATCH;
        context.refresh(OfflineAllocationReservation::new(external).unwrap(), Some(&db)).unwrap();
        let native = context.retained_native_bytes(&db).unwrap();
        let remaining = LIMIT.checked_sub(external + native).unwrap();
        let pressure = context.reserve(&db, remaining).unwrap();
        assert_eq!(context.temporary_bytes_for_test().unwrap(), remaining);
        match original_identity_digest(&db, &metadata) {
            Err(error) => assert_eq!(error.to_string(), "offline aggregate allocation budget exhausted"),
            Ok(_) => panic!("unadmitted digest escaped the real held demand"),
        }
        assert_eq!(context.temporary_bytes_for_test().unwrap(), remaining);
        drop(pressure);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
        { let state = context.0.lock().unwrap(); assert!(state.control_loan && state.control_scratch); }
        drop(metadata); drop(loan);
        assert!(!context.0.lock().unwrap().control_loan);
    }
}
