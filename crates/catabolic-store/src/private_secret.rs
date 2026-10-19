// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Exclusive local API credential output on the caller's existing Store.
//!
//! This is deliberately not the Plex atomic-replacement protocol. A failed
//! serialization/registration leaves the newly created file, as in the frozen
//! API. No cleanup, replacement, permission broadening, or catalog reopening.
use crate::{Error, Store};
use rusqlite::params;
use std::{
    fs::{File, OpenOptions},
    io::{self, BufWriter, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

#[derive(Debug)]
pub enum Failure<E> {
    Io(io::Error),
    Native(Error),
    Serialization(E),
}

/// `path` is the boundary's expanded absolute pathname. The serializer owns
/// representation only; all file effects and registration belong here. The
/// caller must read summary fields only after this operation succeeds.
pub fn save<E>(
    store: &Store,
    path: &Path,
    serialize: impl FnOnce(&mut BufWriter<File>) -> std::result::Result<(), E>,
) -> std::result::Result<(), Failure<E>> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(Failure::Io)?;
    {
        let mut stream = BufWriter::new(file);
        let encoded = serialize(&mut stream);
        // Python's context manager flushes partial JSON when serialization
        // raises. Preserve those bytes, but never register or fsync that case.
        if encoded.is_err() {
            stream.flush().map_err(Failure::Io)?;
        }
        encoded.map_err(Failure::Serialization)?;
        stream.write_all(b"\n").map_err(Failure::Io)?;
        stream.flush().map_err(Failure::Io)?;
        stream.get_ref().sync_all().map_err(Failure::Io)?;
    }
    let identity = path.metadata().map_err(Failure::Io)?;
    store
        .transaction(|db| {
            let path = path
                .to_str()
                .ok_or_else(|| Error("credential pathname is not valid UTF-8".into()))?;
            let device = i64::try_from(identity.dev())
                .map_err(|_| Error("Python int too large to convert to SQLite INTEGER".into()))?;
            let inode = i64::try_from(identity.ino())
                .map_err(|_| Error("Python int too large to convert to SQLite INTEGER".into()))?;
            db.execute(
                "INSERT OR REPLACE INTO api_private_files VALUES (?,?,?)",
                params![path, device, inode],
            )?;
            Ok(())
        })
        .map_err(Failure::Native)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fixture() -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("fixture.db");
        crate::migration::initialize(&db).unwrap();
        let store = Store::open(&db, true, false).unwrap();
        (root, store)
    }

    #[test]
    fn exclusive_file_bytes_mode_and_registered_identity() {
        let (root, store) = fixture();
        let path = root.path().join("fake-credential.json");
        let payload = b"{\"token_id\": \"FAKE-ID\", \"token\": \"FAKE-ONLY\", \"expires\": 0}";
        save(&store, &path, |stream| stream.write_all(payload)).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            [payload.as_slice(), b"\n"].concat()
        );
        let stat = path.metadata().unwrap();
        assert_eq!(stat.permissions().mode() & 0o777, 0o600);
        let identity: (i64, i64) = store
            .db
            .query_row(
                "SELECT device,inode FROM api_private_files WHERE path=?",
                [path.to_str().unwrap()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(identity, (stat.dev() as i64, stat.ino() as i64));
        let error = save(&store, &path, |_| -> io::Result<()> {
            panic!("must not serialize existing file")
        })
        .unwrap_err();
        assert!(
            matches!(error, Failure::Io(error) if error.kind() == io::ErrorKind::AlreadyExists)
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            [payload.as_slice(), b"\n"].concat()
        );
    }

    #[test]
    fn serialization_failure_keeps_partial_file_without_registration() {
        let (root, store) = fixture();
        let path = root.path().join("fake-partial.json");
        let result = save(&store, &path, |stream| {
            stream.write_all(b"{\"fake\": ").unwrap();
            Err("fixture serialization failure")
        });
        assert!(matches!(
            result,
            Err(Failure::Serialization("fixture serialization failure"))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"fake\": ");
        assert_eq!(
            store
                .db
                .query_row("SELECT count(*) FROM api_private_files", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn same_store_outer_rollback_preserves_file_but_not_registration() {
        let (root, store) = fixture();
        let path = root.path().join("fake-rollback.json");
        let result: crate::Result<()> = store.transaction(|db| {
            db.execute(
                "INSERT INTO api_principals VALUES ('fake-uncommitted','default',1)",
                [],
            )?;
            save(&store, &path, |stream| stream.write_all(b"{}")).unwrap();
            assert_eq!(
                db.query_row("SELECT count(*) FROM api_private_files", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            Err(Error("fixture rollback".into()))
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{}\n");
        for table in ["api_private_files", "api_principals"] {
            assert_eq!(
                store
                    .db
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }

    #[test]
    fn registration_error_preserves_file_and_native_provenance() {
        let (root, store) = fixture();
        let path = root.path().join("fake-registration.json");
        store.db.execute_batch("CREATE TEMP TRIGGER fail_private BEFORE INSERT ON api_private_files BEGIN SELECT RAISE(ABORT,'invalid_grant'); END;").unwrap();
        let error = save(&store, &path, |stream| stream.write_all(b"{}")).unwrap_err();
        assert!(matches!(error, Failure::Native(error) if error.0 == "invalid_grant"));
        assert_eq!(std::fs::read(&path).unwrap(), b"{}\n");
        store
            .db
            .execute_batch("DROP TRIGGER fail_private;")
            .unwrap();
        save(&store, &root.path().join("fake-reuse.json"), |stream| {
            stream.write_all(b"{}")
        })
        .unwrap();
    }
}
