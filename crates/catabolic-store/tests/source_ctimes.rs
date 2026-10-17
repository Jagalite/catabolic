// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Schema-39 source ctimes (work plan A5, filesystem review FS3).
//!
//! A native scan records each present source's ctime, bound to the exact
//! observation it saw. The ctime joins the content revision, so an edit that
//! restores mtime is refused before a rescan and rotates the revision after
//! one. Schema 32 keeps the frozen revision contract.
use catabolic_core::sha256;
use catabolic_store::{
    Store, access::Access, content_access, migration, migration::NativeUpgradeQuiescence,
    observation,
};
use rusqlite::params;
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt, path::Path, path::PathBuf};

struct Fixture {
    root: tempfile::TempDir,
    path: PathBuf,
}
impl Fixture {
    /// A schema-32 catalog with one exposed, scanned source file and an
    /// operator API token.
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let media = root.path().join("media");
        fs::create_dir(&media).unwrap();
        fs::write(media.join("file.bin"), b"original bytes").unwrap();
        let path = root.path().join("catalog.sqlite3");
        migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        observation::execute(
            &store,
            "default",
            "bind",
            &json!({"source":"media","root":media.to_str().unwrap()}),
        )
        .unwrap();
        store.transaction(|db|{db.execute_batch("INSERT INTO api_principals(id,profile) VALUES ('test','default'); INSERT INTO api_grants(id,principal,definition) VALUES ('grant','test','{\"actions\":[\"*\"],\"operator\":true}'); INSERT INTO api_sources VALUES ('default','media');")?;db.execute("INSERT INTO api_tokens(id,principal,digest,grants,expires) VALUES ('token','test',?,'[\"grant\"]',?)",params![sha256("token.secret"),4_000_000_000.0])?;Ok(())}).unwrap();
        drop(store);
        let fixture = Self { root, path };
        fixture.scan();
        fixture
    }
    fn media(&self) -> PathBuf {
        self.root.path().join("media/file.bin")
    }
    fn scan(&self) {
        let store = Store::open(&self.path, true, false).unwrap();
        observation::execute(&store, "default", "scan", &json!({"source":"media"})).unwrap();
    }
    fn upgrade(&self) {
        let plan = migration::plan_native(&self.path).unwrap();
        assert_eq!(plan["to_schema"], migration::NATIVE_SCHEMA_VERSION);
        let quiescence = NativeUpgradeQuiescence {
            admission_stopped: true,
            readers_and_workers_stopped: true,
            exclusive_operational_ownership: true,
        };
        migration::upgrade_native(&self.path, &plan, quiescence, None, &|_, _, _| Ok(())).unwrap();
        let store = Store::open(&self.path, false, false).unwrap();
        assert_eq!(store.schema_version, 39);
    }
    fn file_id(&self) -> String {
        let store = Store::open(&self.path, false, false).unwrap();
        store
            .db
            .query_row("SELECT id FROM files WHERE path='file.bin'", [], |r| {
                r.get(0)
            })
            .unwrap()
    }
    fn revision(&self) -> String {
        let store = Store::open(&self.path, false, false).unwrap();
        content_access::revision_of(&store, "default", &self.file_id()).unwrap()
    }
    fn recorded_ctime(&self) -> Option<i64> {
        let store = Store::open(&self.path, false, false).unwrap();
        store
            .db
            .query_row(
                "SELECT ctime_ns FROM native_source_ctimes WHERE profile='default' AND file_id=?",
                [self.file_id()],
                |r| r.get(0),
            )
            .ok()
    }
    /// Read the file through the content owner with `revision`.
    fn read(&self, revision: &str) -> catabolic_store::Result<Vec<u8>> {
        let store = Store::open(&self.path, false, false).unwrap();
        let mut access = Access::authenticate(store, "token.secret").unwrap();
        content_access::with_opened(&mut access, &self.file_id(), revision, |file, stat| {
            content_access::read_chunk(file, stat, 0, 1024)
        })
    }
}

fn ctime_ns(path: &Path) -> i64 {
    use std::os::unix::fs::MetadataExt;
    let stat = fs::metadata(path).unwrap();
    stat.ctime() * 1_000_000_000 + stat.ctime_nsec()
}

/// Change only the inode change time: same size, mtime, device and inode.
fn touch_ctime(path: &Path) {
    let before = ctime_ns(path);
    for attempt in 0..200 {
        let mode = if attempt % 2 == 0 { 0o600 } else { 0o644 };
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        if ctime_ns(path) != before {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("ctime did not advance");
}

/// Rewrite the bytes in place with the same size and restore the old mtime.
fn rewrite_restoring_mtime(path: &Path, bytes: &[u8]) {
    let mtime = fs::metadata(path).unwrap().modified().unwrap();
    let before = ctime_ns(path);
    fs::write(path, bytes).unwrap();
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), mtime);
    if ctime_ns(path) == before {
        touch_ctime(path);
    }
}

#[test]
fn schema32_keeps_the_frozen_revision_contract() {
    let f = Fixture::new();
    let revision = f.revision();
    let snapshot = {
        let store = Store::open(&f.path, false, false).unwrap();
        content_access::snapshot(&store, "default", &f.file_id()).unwrap()
    };
    assert!(snapshot.get("ctime_ns").is_none());
    // Inherited limit at schema 32: an edit that restores mtime is not seen.
    rewrite_restoring_mtime(&f.media(), b"modified bytes");
    assert_eq!(f.revision(), revision);
    assert_eq!(f.read(&revision).unwrap(), b"modified bytes");
}

#[test]
fn upgrade_keeps_revisions_until_a_native_scan_records_ctime() {
    let f = Fixture::new();
    let before = f.revision();
    f.upgrade();
    // No historical ctime is known: nothing is backfilled.
    assert_eq!(f.recorded_ctime(), None);
    assert_eq!(f.revision(), before);
    f.scan();
    assert_eq!(f.recorded_ctime(), Some(ctime_ns(&f.media())));
    let after = f.revision();
    assert_ne!(
        after, before,
        "the first native scan rotates the revision once"
    );
    assert_eq!(f.read(&after).unwrap(), b"original bytes");
    // The pre-scan revision no longer authorizes content.
    assert_eq!(f.read(&before).unwrap_err().0, "revision_mismatch");
    // A rescan of an unchanged file keeps the revision.
    f.scan();
    assert_eq!(f.revision(), after);
}

#[test]
fn an_edit_that_restores_mtime_is_refused_then_rotates_the_revision() {
    let f = Fixture::new();
    f.upgrade();
    f.scan();
    let revision = f.revision();
    rewrite_restoring_mtime(&f.media(), b"modified bytes");
    // Same size, mtime, device and inode: only ctime shows the edit.
    let error = f.read(&revision).unwrap_err();
    assert!(error.0.contains("rescan"), "{error:?}");
    f.scan();
    let rotated = f.revision();
    assert_ne!(rotated, revision);
    assert_eq!(f.read(&rotated).unwrap(), b"modified bytes");
    assert_eq!(f.read(&revision).unwrap_err().0, "revision_mismatch");
}

#[test]
fn a_metadata_only_change_also_requires_a_rescan() {
    // ctime cannot tell a content edit from chmod, chown, xattrs or a new hard
    // link, so every ctime change is treated as a possible content change.
    let f = Fixture::new();
    f.upgrade();
    f.scan();
    let revision = f.revision();
    touch_ctime(&f.media());
    assert!(f.read(&revision).unwrap_err().0.contains("rescan"));
    f.scan();
    let rotated = f.revision();
    assert_ne!(rotated, revision);
    assert_eq!(f.read(&rotated).unwrap(), b"original bytes");
}

fn count(store: &Store) -> i64 {
    store
        .db
        .query_row("SELECT count(*) FROM native_source_ctimes", [], |r| {
            r.get(0)
        })
        .unwrap()
}

fn refused(store: &Store, sql: &str) -> String {
    store
        .transaction(|db| {
            db.execute_batch(sql)?;
            Ok(())
        })
        .unwrap_err()
        .0
}

#[test]
fn companions_are_bound_to_their_exact_present_observation() {
    let f = Fixture::new();
    f.upgrade();
    f.scan();
    let store = Store::open(&f.path, true, false).unwrap();
    assert_eq!(count(&store), 1);
    // A ctime for another tuple, an unknown key or a rekey is refused.
    for sql in [
        "UPDATE native_source_ctimes SET size=size+1",
        "UPDATE native_source_ctimes SET file_id='other'",
        "INSERT INTO native_source_ctimes VALUES('default','other',1,1,1,1,1)",
    ] {
        let error = refused(&store, sql);
        assert!(
            error.contains("native_source_ctime_unbound"),
            "{sql}: {error}"
        );
    }
    let error = refused(
        &store,
        "UPDATE native_source_ctimes SET ctime_ns='not a number'",
    );
    assert!(error.contains("CHECK"), "{error}");
    // The bound row itself may record a new ctime.
    store
        .transaction(|db| {
            db.execute("UPDATE native_source_ctimes SET ctime_ns=ctime_ns+1", [])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(count(&store), 1);
}

#[test]
fn any_change_to_the_observation_discards_its_ctime() {
    for change in [
        "UPDATE observations SET size=size+1",
        "UPDATE observations SET mtime_ns=mtime_ns+1",
        "UPDATE observations SET inode=inode+1",
        "UPDATE observations SET status='missing'",
        "DELETE FROM observations",
    ] {
        let f = Fixture::new();
        f.upgrade();
        f.scan();
        let id = f.file_id();
        let store = Store::open(&f.path, true, false).unwrap();
        assert_eq!(count(&store), 1);
        let generation = |store: &Store| -> i64 {
            store
                .db
                .query_row("SELECT generation FROM catalog_state", [], |r| r.get(0))
                .unwrap()
        };
        let before = generation(&store);
        store
            .transaction(|db| {
                db.execute_batch(change)?;
                Ok(())
            })
            .unwrap();
        assert_eq!(count(&store), 0, "{change}");
        assert!(generation(&store) > before, "{change}");
        let deleted: i64 = store
            .db
            .query_row(
                "SELECT deleted FROM catalog_entity_revisions WHERE entity_kind='native_source_ctimes' AND entity_key=json_array('default',?)",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(deleted, 1, "{change}");
    }
}
