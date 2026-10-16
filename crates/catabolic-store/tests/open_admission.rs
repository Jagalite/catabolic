// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Typed opening reuses the existing owner and preserves legacy diagnostics.
use catabolic_store::{OpenError, OpenErrorCode, Store, embedding::Session, migration};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use std::{fs, path::Path};

fn store_error(path: &Path, writable: bool, recovery: bool) -> OpenError {
    Store::open_typed(path, writable, recovery).err().unwrap()
}

#[test]
fn missing_catalog_is_typed_without_creating_it() {
    let root = tempfile::tempdir().unwrap();
    for path in [
        root.path().join("missing.db"),
        root.path().join("missing-parent/catalog.db"),
    ] {
        let typed = store_error(&path, true, false);
        assert_eq!(typed.code, OpenErrorCode::InvalidCatalog);
        assert_eq!(
            typed.error.0,
            Store::open(&path, true, false).err().unwrap().0
        );
        let session = Session::open_typed(&path, true, false).err().unwrap();
        assert_eq!(session.code, typed.code);
        assert_eq!(session.error.0, typed.error.0);
        assert!(!path.exists());
    }
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn unsupported_schema_is_explicit_and_never_upgraded() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    migration::initialize(&path).unwrap();
    for version in [0, 1, migration::NATIVE_SCHEMA_VERSION + 1] {
        {
            let db = rusqlite::Connection::open(&path).unwrap();
            db.pragma_update(None, "user_version", version).unwrap();
        }
        let before = fs::read(&path).unwrap();
        let typed = store_error(&path, false, false);
        assert_eq!(typed.code, OpenErrorCode::UnsupportedSchema);
        assert_eq!(
            typed.error.0,
            Store::open(&path, false, false).err().unwrap().0
        );
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}

#[test]
fn invalid_catalog_bytes_structure_identity_and_history_have_typed_causes() {
    let root = tempfile::tempdir().unwrap();
    let corrupt = root.path().join("corrupt.db");
    fs::write(&corrupt, b"this is not a SQLite catalog").unwrap();
    let typed = store_error(&corrupt, false, false);
    assert_eq!(typed.code, OpenErrorCode::InvalidCatalog);
    assert_eq!(
        typed.error.0,
        Store::open(&corrupt, false, false).err().unwrap().0
    );

    for (name, alteration, diagnostic) in [
        (
            "structure",
            "CREATE TABLE unexpected (id TEXT)",
            format!(
                "database structure does not match known schema {}; refusing to modify it",
                migration::SCHEMA_VERSION
            ),
        ),
        (
            "identity",
            "DELETE FROM meta WHERE key='database_id'",
            "database identity is missing or invalid".into(),
        ),
        (
            "history",
            "DELETE FROM schema_migrations WHERE version=1",
            "migration history is incomplete or inconsistent".into(),
        ),
    ] {
        let path = root.path().join(format!("{name}.db"));
        migration::initialize(&path).unwrap();
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(alteration)
            .unwrap();
        let typed = store_error(&path, true, false);
        assert_eq!(typed.code, OpenErrorCode::InvalidCatalog);
        assert_eq!(typed.error.0, diagnostic);
        // A failed typed open must release its writer lock before another open.
        assert_eq!(Store::open(&path, true, false).err().unwrap().0, diagnostic);
    }
}

#[test]
fn writer_contention_retains_one_owner_and_legacy_diagnostic() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    migration::initialize(&path).unwrap();
    let owner = Store::open_typed(&path, true, false).unwrap();
    let typed = store_error(&path, true, false);
    assert_eq!(typed.code, OpenErrorCode::BusyWriter);
    assert_eq!(
        typed.error.0,
        "another Catabolic writer or upgrade is using this database"
    );
    assert_eq!(
        Store::open(&path, true, false).err().unwrap().0,
        typed.error.0
    );
    let session = Session::open_typed(&path, true, false).err().unwrap();
    assert_eq!(session.code, typed.code);
    assert_eq!(session.error.0, typed.error.0);
    assert!(Store::open_typed(&path, false, false).is_ok());
    drop(owner);
    assert!(Session::open_typed(&path, true, false).is_ok());
}

#[test]
fn typed_session_retains_legacy_metadata_and_read_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    migration::initialize(&path).unwrap();
    let typed = Session::open_typed(&path, false, false).unwrap();
    let legacy = Session::open(&path, false, false).unwrap();
    assert_eq!(typed.info(), legacy.info());
    assert!(!typed.store().unwrap().db.is_autocommit());
    assert!(!legacy.store().unwrap().db.is_autocommit());
}

#[test]
fn typed_recovery_uses_the_existing_reviewed_schema_admission() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("schema-one.db");
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        migration::execute(&db, &migration::MIGRATIONS[0]).unwrap();
        db.execute(
            "INSERT INTO meta VALUES ('database_id',?)",
            ["975cc6b5-7f32-457a-8e13-7a117a8ac92e"],
        )
        .unwrap();
        db.pragma_update(None, "user_version", 1).unwrap();
    }
    let before = fs::read(&path).unwrap();
    assert_eq!(
        store_error(&path, false, false).code,
        OpenErrorCode::UnsupportedSchema
    );
    let typed = Session::open_typed(&path, false, true).unwrap();
    let legacy = Session::open(&path, false, true).unwrap();
    assert_eq!(typed.info(), legacy.info());
    assert_eq!(typed.store().unwrap().schema_version, 1);
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn validation_keeps_internal_sqlite_cause_even_when_legacy_text_says_invalid() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    migration::initialize(&path).unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.authorizer(Some(|context: AuthContext<'_>| {
        if matches!(
            context.action,
            AuthAction::Read {
                table_name: "meta",
                ..
            }
        ) {
            Authorization::Deny
        } else {
            Authorization::Allow
        }
    }))
    .unwrap();
    let typed = migration::validate_typed(&db, false).unwrap_err();
    assert_eq!(typed.code, OpenErrorCode::Internal);
    assert_eq!(typed.error.0, "database identity is missing or invalid");
    assert_eq!(
        migration::validate(&db, false).unwrap_err().0,
        typed.error.0
    );
}

#[test]
fn sqlite_result_codes_are_independent_of_diagnostic_words() {
    for (sqlite_code, expected) in [
        (rusqlite::ffi::SQLITE_BUSY, OpenErrorCode::BusyWriter),
        (rusqlite::ffi::SQLITE_LOCKED, OpenErrorCode::BusyWriter),
        (rusqlite::ffi::SQLITE_CORRUPT, OpenErrorCode::InvalidCatalog),
        (rusqlite::ffi::SQLITE_NOTADB, OpenErrorCode::InvalidCatalog),
        (rusqlite::ffi::SQLITE_IOERR, OpenErrorCode::Internal),
        (rusqlite::ffi::SQLITE_NOMEM, OpenErrorCode::Internal),
        (rusqlite::ffi::SQLITE_CANTOPEN, OpenErrorCode::Internal),
    ] {
        let source = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(sqlite_code),
            Some("busy unsupported schema invalid catalog internal".into()),
        );
        let diagnostic = source.to_string();
        let typed = OpenError::from(source);
        assert_eq!(typed.code, expected);
        assert_eq!(typed.to_string(), diagnostic);
        let legacy: catabolic_store::Error = typed.into();
        assert_eq!(legacy.0, diagnostic);
    }
}
