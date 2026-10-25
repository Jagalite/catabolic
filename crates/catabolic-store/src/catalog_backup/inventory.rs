//! Independent logical inventory, not repair or export authority. All schema
//! objects and stored tables are included; indexes are covered by exact schema
//! plus SQLite integrity checking. Only the shipped external-content FTS5
//! virtual table and its four ordinary storage shadows are supported.
use super::nonnegative;
use super::{fail, live};
use crate::Result;
use rusqlite::{Connection, types::ValueRef};
use sha2::{Digest, Sha256};
use std::{sync::atomic::AtomicBool, time::Instant};
const MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ROWS: u64 = 100_000;
const MAX_TABLES: usize = 1_000;
const MAX_CELLS: u64 = 2_000_000;
const MAX_IDENTIFIER: usize = 256;
const MAX_SELECTOR_BYTES: u64 = 1024 * 1024;
const MAX_QUERY_BYTES: usize = 1024 * 1024;
// The export's 32 MiB metadata reserve covers Rust transients independently of
// SQLite DBSTATUS counters: <=8 MiB one schema row/string, <=1 MiB retained
// selectors/names, <=1 MiB each cost fragments/join/query (at most three live),
// <=3.2 MiB row hashes, and bounded 1,000 table/512 column descriptor overhead.
// Per-table digest/name entries add less than 1 MiB under the 1,000-table and
// 256-byte identifier bounds. The simultaneous storage-class map is bounded
// to 1,001 entries and an additional 1 MiB of names/types within the 32 MiB reserve;
// it is consumed before row hashes are allocated. Phases do not retain schema
// SQL strings alongside the final row-hash vectors.
// This conservative reservation is not a process-wide allocator/RSS guarantee.
const FTS: &str = "catalog_search_fts";
const SHADOWS: &[&str] = &[
    "catalog_search_fts_data",
    "catalog_search_fts_idx",
    "catalog_search_fts_docsize",
    "catalog_search_fts_config",
];
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Audit {
    pub(crate) schema: String,
    pub(crate) complete: String,
    pub(crate) ledger: String,
    pub(crate) tables: std::collections::BTreeMap<String, String>,
}
impl Audit {
    pub(crate) fn retained_allocations(&self) -> Result<u64> {
        let mut bytes = (self.schema.capacity() as u64)
            .checked_add(self.complete.capacity() as u64)
            .and_then(|n| n.checked_add(self.ledger.capacity() as u64))
            .ok_or_else(|| fail("current image audit allocation overflow"))?;
        for (key, value) in &self.tables {
            bytes = bytes.checked_add(1024)
                .and_then(|n| n.checked_add(key.capacity() as u64))
                .and_then(|n| n.checked_add(value.capacity() as u64))
                .ok_or_else(|| fail("current image audit allocation overflow"))?;
        }
        Ok(bytes)
    }
}
struct Table {
    name: String,
    selector: String,
    columns: usize,
    rows: u64,
    bytes: u64,
}
fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
fn value(hash: &mut Sha256, v: ValueRef<'_>) {
    let (tag, len) = match v {
        ValueRef::Null => (0u8, 0),
        ValueRef::Integer(_) => (1, 8),
        ValueRef::Real(_) => (2, 8),
        ValueRef::Text(b) => (3, b.len()),
        ValueRef::Blob(b) => (4, b.len()),
    };
    hash.update([tag]);
    hash.update((len as u64).to_be_bytes());
    match v {
        ValueRef::Null => (),
        ValueRef::Integer(n) => hash.update(n.to_be_bytes()),
        ValueRef::Real(n) => hash.update(n.to_bits().to_be_bytes()),
        ValueRef::Text(b) | ValueRef::Blob(b) => hash.update(b),
    }
}
fn add(total: &mut u64, n: u64, max: u64) -> Result<()> {
    *total = total
        .checked_add(n)
        .ok_or_else(|| fail("native inventory accounting overflow"))?;
    if *total > max {
        return Err(fail("native inventory bound exceeded"));
    }
    Ok(())
}
fn supported_fts(sql: &str) -> bool {
    let resource = include_str!("../../resources/migrations/035_native_search.sql");
    let start = resource
        .find("CREATE VIRTUAL TABLE catalog_search_fts ")
        .expect("compiled FTS definition");
    let definition = resource[start..]
        .split(';')
        .next()
        .expect("compiled FTS statement");
    sql.trim() == definition.trim()
}
pub(crate) fn capture(db: &Connection, cancel: &AtomicBool, deadline: Instant) -> Result<Audit> {
    live(cancel, deadline)?;
    let (schema_rows, schema_bytes): (u64, u64) = db.query_row(
        "SELECT count(*),coalesce(sum(length(CAST(type AS BLOB))+length(CAST(name AS BLOB))+length(CAST(tbl_name AS BLOB))+coalesce(length(CAST(sql AS BLOB)),0)+68),0) FROM sqlite_schema", [], |r| Ok((nonnegative(r, 0)?,nonnegative(r, 1)?)))?;
    if schema_rows > 10_000 || schema_bytes > MAX_BYTES {
        return Err(fail("native inventory schema bound exceeded"));
    }
    // Enumerate storage metadata once. Reopening pragma_table_list for every
    // table repeats the complete schema walk on large closed candidate schemas.
    // Precharge names before retaining the map; no storage class is inferred.
    let (storage_count, storage_bytes): (u64, u64) = db.query_row(
        "SELECT count(*),coalesce(sum(length(CAST(name AS BLOB))+length(CAST(type AS BLOB))),0) FROM pragma_table_list WHERE schema='main' AND type!='view'",
        [],
        |r| Ok((nonnegative(r, 0)?, nonnegative(r, 1)?)),
    )?;
    if storage_count > (MAX_TABLES + 1) as u64 || storage_bytes > MAX_SELECTOR_BYTES {
        return Err(fail("native inventory storage metadata bound exceeded"));
    }
    live(cancel, deadline)?;
    let mut storage = std::collections::BTreeMap::new();
    let mut storage_query = db.prepare(
        "SELECT name,type,wr FROM pragma_table_list WHERE schema='main' AND type!='view' LIMIT 1002",
    )?;
    let mut storage_rows = storage_query.query([])?;
    let mut storage_used = 0u64;
    while let Some(row) = storage_rows.next()? {
        live(cancel, deadline)?;
        let name: String = row.get(0)?;
        let kind: String = row.get(1)?;
        let wr: i64 = row.get(2)?;
        if name.len() > MAX_IDENTIFIER || kind.len() > MAX_IDENTIFIER {
            return Err(fail("native inventory storage identifier bound exceeded"));
        }
        add(
            &mut storage_used,
            (name.len() + kind.len()) as u64,
            MAX_SELECTOR_BYTES,
        )?;
        if storage.len() >= MAX_TABLES + 1 || storage.insert(name, (kind, wr)).is_some() {
            return Err(fail(
                "native inventory duplicate or excess storage metadata",
            ));
        }
    }
    drop(storage_rows);
    drop(storage_query);
    if storage.len() as u64 != storage_count || storage_used != storage_bytes {
        return Err(fail("native inventory storage metadata changed"));
    }
    let mut schema_hash = Sha256::new();
    let mut tables = Vec::new();
    let mut total_rows = 0;
    let mut total_bytes = schema_bytes;
    let mut total_cells = 0;
    let mut selector_bytes = 0;
    let mut schema =
        db.prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name")?;
    let mut rows = schema.query([])?;
    while let Some(row) = rows.next()? {
        live(cancel, deadline)?;
        for i in 0..4 {
            value(&mut schema_hash, row.get_ref(i)?);
        }
        if row.get::<_, String>(0)? != "table" {
            continue;
        }
        let name: String = row.get(1)?;
        if name.len() > MAX_IDENTIFIER {
            return Err(fail("native inventory identifier bound exceeded"));
        }
        let sql: Option<String> = row.get(3)?;
        if tables.len() >= MAX_TABLES {
            return Err(fail("native inventory table bound exceeded"));
        }
        let (kind, wr) = storage
            .remove(&name)
            .ok_or_else(|| fail("native inventory table storage metadata missing"))?;
        match kind.as_str() {
            "virtual" if name == FTS && sql.as_deref().is_some_and(supported_fts) && wr == 0 => (),
            "shadow" if SHADOWS.contains(&name.as_str()) => (),
            "table"
                if !name.starts_with("sqlite_")
                    || ["sqlite_sequence", "sqlite_stat1", "sqlite_stat4"]
                        .contains(&name.as_str()) =>
            {
                ()
            }
            _ => return Err(fail("unsupported native inventory table storage form")),
        }
        let (column_count, largest_name): (u64,u64) = db.query_row(
            "SELECT count(*),coalesce(max(length(CAST(name AS BLOB))),0) FROM pragma_table_xinfo(?1)",
            [&name],
            |r| Ok((nonnegative(r, 0)?,nonnegative(r, 1)?)),
        )?;
        if column_count == 0 || column_count > 512 || largest_name > MAX_IDENTIFIER as u64 {
            return Err(fail("native inventory column bound exceeded"));
        }
        let mut stmt = db.prepare("SELECT name,hidden FROM pragma_table_xinfo(?1) ORDER BY cid")?;
        let columns = stmt
            .query_map([&name], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut selected = Vec::new();
        for (column, hidden) in &columns {
            if column.len() > MAX_IDENTIFIER {
                return Err(fail("native inventory column identifier bound exceeded"));
            }
            match *hidden {
                0 => selected.push(quoted(column)),
                // FTS query-control pseudo columns are not stored row data;
                // complete logical fields and every shadow row remain included.
                1 if name == FTS && (column == FTS || column == "rank") => (),
                // No shipped native schema has generated columns. Refuse rather
                // than evaluate unreviewed expressions or omit their values.
                _ => return Err(fail("unsupported native hidden/generated column")),
            }
        }
        if wr == 0 {
            let alias = ["rowid", "_rowid_", "oid"]
                .into_iter()
                .find(|a| !columns.iter().any(|(n, _)| n.eq_ignore_ascii_case(a)))
                .ok_or_else(|| fail("native inventory rowid shadowed"))?;
            selected.insert(0, alias.into());
        } else if wr != 1 {
            return Err(fail("unsupported native row storage"));
        }
        let selector = selected.join(",");
        add(
            &mut selector_bytes,
            (selector.len() + name.len()) as u64,
            MAX_SELECTOR_BYTES,
        )?;
        // Bounded SQL aggregates run BEFORE materializing any table's row values.
        // Storage-class lengths include fixed tags/lengths and numeric bit values.
        let costs = selected.iter().map(|c| format!("9+CASE typeof({c}) WHEN 'null' THEN 0 WHEN 'integer' THEN 8 WHEN 'real' THEN 8 ELSE length(CAST({c} AS BLOB)) END")).collect::<Vec<_>>().join("+");
        if costs.len() > MAX_QUERY_BYTES {
            return Err(fail("native inventory generated SQL bound exceeded"));
        }
        let count: u64 = db.query_row(
            &format!(
                "SELECT count(*) FROM (SELECT 1 FROM {} LIMIT {})",
                quoted(&name),
                MAX_ROWS + 1
            ),
            [],
            |r| nonnegative(r, 0),
        )?;
        add(&mut total_rows, count, MAX_ROWS)?;
        add(
            &mut total_cells,
            count
                .checked_mul(selected.len() as u64)
                .ok_or_else(|| fail("native inventory work overflow"))?,
            MAX_CELLS,
        )?;
        live(cancel, deadline)?;
        let (checked_count, bytes): (u64, u64) = db.query_row(&format!("SELECT count(*),coalesce(sum(row_cost),0) FROM (SELECT ({costs}) AS row_cost FROM {} LIMIT {})", quoted(&name), MAX_ROWS+1), [], |r| Ok((nonnegative(r, 0)?,nonnegative(r, 1)?)))?;
        if checked_count != count {
            return Err(fail("native inventory changed during preflight"));
        }
        add(&mut total_bytes, bytes, MAX_BYTES)?;
        tables.push(Table {
            name,
            selector,
            columns: selected.len(),
            rows: count,
            bytes,
        });
    }
    // SQLite exposes its schema catalog through table_list but not as a row
    // within sqlite_schema. Every other entry must match an inventoried table.
    if storage.remove("sqlite_schema") != Some(("table".into(), 0)) || !storage.is_empty() {
        return Err(fail("native inventory unmatched storage metadata"));
    }
    // Aggregate precharge across ALL tables, not a fresh budget per table.
    let mut count = 0;
    let mut used = schema_bytes;
    for t in &tables {
        add(&mut count, t.rows, MAX_ROWS)?;
        add(&mut used, t.bytes, MAX_BYTES)?;
    }
    let schema = format!("{:x}", schema_hash.finalize());
    let mut complete = Sha256::new();
    complete.update(schema.as_bytes());
    let mut ledger = None;
    let mut table_digests = std::collections::BTreeMap::new();
    for table in tables {
        live(cancel, deadline)?;
        let mut statement = db.prepare(&format!(
            "SELECT {} FROM {} LIMIT {}",
            table.selector,
            quoted(&table.name),
            MAX_ROWS + 1
        ))?;
        let mut rows = statement.query([])?;
        let mut hashes: Vec<[u8; 32]> = Vec::with_capacity(table.rows as usize);
        while let Some(row) = rows.next()? {
            live(cancel, deadline)?;
            if hashes.len() as u64 >= table.rows {
                return Err(fail("native inventory changed after preflight"));
            }
            let mut hash = Sha256::new();
            for i in 0..table.columns {
                value(&mut hash, row.get_ref(i)?);
            }
            hashes.push(hash.finalize().into());
        }
        if hashes.len() as u64 != table.rows {
            return Err(fail("native inventory row count changed"));
        }
        hashes.sort_unstable();
        let mut hash = Sha256::new();
        hash.update((table.name.len() as u64).to_be_bytes());
        hash.update(table.name.as_bytes());
        hash.update(table.rows.to_be_bytes());
        for row in hashes {
            hash.update(row);
        }
        let digest = hash.finalize();
        if table.name == "schema_migrations" {
            ledger = Some(format!("{digest:x}"));
        }
        table_digests.insert(table.name, format!("{digest:x}"));
        complete.update(digest);
    }
    Ok(Audit {
        schema,
        tables: table_digests,
        complete: format!("{:x}", complete.finalize()),
        ledger: ledger.ok_or_else(|| fail("native migration ledger absent from inventory"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn database() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, checksum TEXT); INSERT INTO schema_migrations VALUES(1,'original'); CREATE TABLE receipt(id TEXT PRIMARY KEY,payload); INSERT INTO receipt VALUES('command',X'0102'); CREATE TABLE sample(value); INSERT INTO sample(rowid,value) VALUES(7,1); CREATE TABLE ordered(k TEXT PRIMARY KEY,value) WITHOUT ROWID; INSERT INTO ordered VALUES('a',NULL);").unwrap();
        db
    }
    fn audit(db: &Connection) -> Audit {
        capture(
            db,
            &AtomicBool::new(false),
            Instant::now() + std::time::Duration::from_secs(10),
        )
        .unwrap()
    }
    #[test]
    fn storage_metadata_preserves_rowid_without_rowid_and_view_inventory() {
        let db = database();
        let before = audit(&db);
        db.execute_batch("CREATE VIEW visible_sample AS SELECT value FROM sample")
            .unwrap();
        let with_view = audit(&db);
        assert_ne!(before.schema, with_view.schema);
        assert_eq!(before.tables, with_view.tables);
        assert!(!with_view.tables.contains_key("visible_sample"));
        assert!(with_view.tables.contains_key("sample"));
        assert!(with_view.tables.contains_key("ordered"));
        let mut copy = Connection::open_in_memory().unwrap();
        {
            let backup = rusqlite::backup::Backup::new(&db, &mut copy).unwrap();
            backup
                .run_to_completion(128, std::time::Duration::from_millis(1), None)
                .unwrap();
        }
        assert_eq!(audit(&copy), with_view);
        copy.execute("UPDATE ordered SET k='b'", []).unwrap();
        assert_ne!(audit(&copy).tables["ordered"], with_view.tables["ordered"]);
        assert_eq!(audit(&copy).tables["sample"], with_view.tables["sample"]);
    }
    #[test]
    fn typed_value_storage_class_rowid_ledger_and_receipt_are_not_interchangeable() {
        let db = database();
        let initial = audit(&db);
        db.execute("UPDATE sample SET value='1'", []).unwrap();
        assert_ne!(audit(&db).complete, initial.complete);
        db.execute("UPDATE sample SET value=1", []).unwrap();
        assert_eq!(audit(&db), initial);
        db.execute("UPDATE sample SET rowid=8", []).unwrap();
        assert_ne!(audit(&db).complete, initial.complete);
        db.execute("UPDATE sample SET rowid=7", []).unwrap();
        db.execute("UPDATE receipt SET payload=X'0103'", [])
            .unwrap();
        assert_ne!(audit(&db).complete, initial.complete);
        db.execute("UPDATE receipt SET payload=X'0102'", [])
            .unwrap();
        db.execute("UPDATE schema_migrations SET checksum='different'", [])
            .unwrap();
        assert_ne!(audit(&db).ledger, initial.ledger);
        assert_ne!(audit(&db).complete, initial.complete);
        db.execute("UPDATE schema_migrations SET checksum='original'", [])
            .unwrap();
        db.execute("UPDATE ordered SET value=X''", []).unwrap();
        assert_ne!(audit(&db).complete, initial.complete);
    }
    #[test]
    fn unsupported_virtual_generated_columns_and_precharged_overflow_refuse() {
        let cancel = AtomicBool::new(false);
        let deadline = || Instant::now() + std::time::Duration::from_secs(10);
        let db = database();
        db.execute_batch("CREATE VIRTUAL TABLE unreviewed USING fts5(body)")
            .unwrap();
        assert!(capture(&db, &cancel, deadline()).is_err());
        let db = database();
        db.execute_batch(
            "CREATE TABLE generated(a INTEGER,b INTEGER GENERATED ALWAYS AS(a+1) STORED)",
        )
        .unwrap();
        assert!(capture(&db, &cancel, deadline()).is_err());
        let db = database();
        db.execute_batch(&format!(
            "CREATE TABLE {}(value)",
            quoted(&"n".repeat(MAX_IDENTIFIER + 1))
        ))
        .unwrap();
        assert!(capture(&db, &cancel, deadline()).is_err());
        let db = database();
        db.execute(
            "UPDATE receipt SET payload=zeroblob(?1)",
            [i64::try_from(MAX_BYTES + 1).unwrap()],
        )
        .unwrap();
        assert!(capture(&db, &cancel, deadline()).is_err());
        let db = database();
        assert!(capture(&db, &AtomicBool::new(true), deadline()).is_err());
        assert!(db.is_autocommit());
        assert_eq!(
            db.query_row("SELECT value FROM sample", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[test]
    fn aggregate_row_budget_refuses_before_values_are_materialized() {
        let db = database();
        db.execute_batch("WITH RECURSIVE n(x) AS(VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100001) INSERT INTO receipt SELECT 'bulk-'||x,NULL FROM n").unwrap();
        assert!(
            capture(
                &db,
                &AtomicBool::new(false),
                Instant::now() + std::time::Duration::from_secs(10)
            )
            .is_err()
        );
        assert!(db.is_autocommit());
    }
}
