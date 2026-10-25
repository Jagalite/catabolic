use super::*;

fn cold_fts_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("planned-reads.db");
    let db = Connection::open(&path).unwrap();
    // The shipped search index is external-content FTS5. Populate a real index
    // and reopen it so integrity cannot rely on warm connection-local state.
    db.execute_batch(
        "CREATE TABLE documents(rowid INTEGER PRIMARY KEY, body TEXT NOT NULL);
         CREATE VIRTUAL TABLE search USING fts5(
             body, content='documents', content_rowid='rowid'
         );
         INSERT INTO documents VALUES(1, 'alpha beta');
         INSERT INTO search(rowid, body) VALUES(1, 'alpha beta');",
    )
    .unwrap();
    drop(db);
    (root, path)
}

fn integrity(db: &Connection) -> Vec<String> {
    let mut statement = db.prepare("PRAGMA integrity_check").unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

#[test]
fn planned_read_observer_pinned_fts_integrity_emits_main_no_value_data_version() {
    let (_root, path) = cold_fts_fixture();
    let db = Connection::open(path).unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let events = seen.clone();
    // Isolated measurement of the pinned engine's nested callback arguments.
    // The next test independently exercises the production observer itself.
    db.authorizer(Some(move |context: AuthContext<'_>| {
        if let AuthAction::Pragma {
            pragma_name: "data_version",
            pragma_value,
        } = context.action
        {
            events.lock().unwrap().push((
                pragma_value.map(str::to_owned),
                context.database_name.map(str::to_owned),
            ));
        }
        Authorization::Allow
    }))
    .unwrap();
    assert_eq!(integrity(&db), ["ok"]);
    let seen = seen.lock().unwrap();
    assert!(!seen.is_empty(), "real FTS5 integrity did not reach data_version");
    assert!(seen.iter().all(|event| event == &(None, Some("main".into()))));
}

#[test]
fn planned_read_observer_runs_real_fts_integrity_without_database_mutation() {
    for original in [true, false] {
        let (_root, path) = cold_fts_fixture();
        let before = std::fs::read(&path).unwrap();
        let db = Connection::open(&path).unwrap();
        let mut observer = PlannedReadObserver::prepared(original);
        observer.install(&db).unwrap();
        observer.boundary(&db, EndpointBoundary::Begin).unwrap();
        assert_eq!(integrity(&db), ["ok"]);
        let hits: i64 = db
            .query_row("SELECT count(*) FROM search WHERE search MATCH 'alpha'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(hits, 1);
        observer.check(&db).unwrap();
        assert!(!db.is_autocommit());
        observer.boundary(&db, EndpointBoundary::Rollback).unwrap();
        observer.check(&db).unwrap();
        observer.restore(&db, original).unwrap();
        assert!(db.is_autocommit());
        drop(db);
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
}

#[test]
fn planned_read_observer_metadata_exception_keeps_other_attempts_sticky() {
    for sql in [
        "PRAGMA main.data_version=1",
        "PRAGMA main.data_version(1)",
        "PRAGMA data_version",
        "PRAGMA temp.data_version",
        "PRAGMA other.data_version",
        "PRAGMA main.cache_size",
        "PRAGMA main.database_list",
        "PRAGMA main.unrecognized_planning_pragma",
        "PRAGMA main.foreign_keys=OFF",
        "PRAGMA main.schema_version=7",
        "PRAGMA main.writable_schema=ON",
        "COMMIT",
        "SAVEPOINT hostile",
        "CREATE TABLE hostile(x)",
        "UPDATE retained SET value=2",
    ] {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE retained(value INTEGER); INSERT INTO retained VALUES(1);
             CREATE TEMP TABLE scratch(x); ATTACH ':memory:' AS other;",
        )
        .unwrap();
        let mut observer = PlannedReadObserver::prepared(false);
        observer.install(&db).unwrap();
        observer.boundary(&db, EndpointBoundary::Begin).unwrap();
        observer.check(&db).unwrap();
        let error = db.execute_batch(sql).expect_err(sql);
        assert!(
            matches!(error, rusqlite::Error::SqliteFailure(code, _) if code.extended_code & 0xff == rusqlite::ffi::SQLITE_AUTH),
            "attempt did not reach authorizer denial: {sql}"
        );
        assert!(observer.check(&db).is_err(), "denial was not sticky: {sql}");
        let _: i64 = db
            .query_row("PRAGMA 'main'.data_version", [], |r| r.get(0))
            .unwrap();
        assert!(observer.check(&db).is_err(), "allowed read cleared denial: {sql}");
        assert!(!db.is_autocommit(), "denied SQL ended the transaction: {sql}");
        let value: i64 = db.query_row("SELECT value FROM retained", [], |r| r.get(0)).unwrap();
        assert_eq!(value, 1);
        observer.boundary(&db, EndpointBoundary::Rollback).unwrap();
        assert!(observer.check(&db).is_err(), "rollback cleared denial: {sql}");
        observer.restore(&db, false).unwrap();
        assert!(db.is_autocommit());
    }
}
