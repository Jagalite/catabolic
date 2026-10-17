// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Verified native backups and controlled restore with incarnation rotation,
//! on disposable native catalogs.
use catabolic_store::{Store, connect, identity_manifest, migration, native_restore};
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

fn native() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("native.sqlite3");
    migration::initialize_native(&path).unwrap();
    (root, path)
}
fn manifest(path: &Path) -> Value {
    let mut value = identity_manifest::manifest_path(path).unwrap();
    value.as_object_mut().unwrap().remove("database");
    value
}

#[test]
fn native_backup_is_verified_private_and_bound_to_its_catalog() {
    let (root, path) = native();
    let before = fs::read(&path).unwrap();
    let backups = root.path().join("backups");
    fs::create_dir(&backups).unwrap();
    let receipt = native_restore::backup(&path, Some(&backups)).unwrap();
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(receipt["contract"], native_restore::BACKUP_CONTRACT);
    assert_eq!(receipt["schema"], migration::NATIVE_SCHEMA_VERSION);
    let live = manifest(&path);
    assert_eq!(receipt["identity_manifest"], live);
    assert_eq!(receipt["catalog_id"], live["catalog"]["catalog_id"]);
    assert_eq!(receipt["incarnation"], live["catalog"]["incarnation"]);
    let snapshot = PathBuf::from(receipt["backup"].as_str().unwrap());
    assert_eq!(
        receipt["sha256"],
        catabolic_core::sha256(fs::read(&snapshot).unwrap())
    );
    assert_eq!(manifest(&snapshot)["fingerprint"], live["fingerprint"]);
    let directory = snapshot.parent().unwrap();
    assert_eq!(
        fs::metadata(directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let written: Value =
        serde_json::from_slice(&fs::read(directory.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(written, receipt);
    // The default parent is created next to the catalog.
    let default = native_restore::backup(&path, None).unwrap();
    assert!(
        Path::new(default["backup"].as_str().unwrap()).starts_with(
            fs::canonicalize(root.path())
                .unwrap()
                .join("native.sqlite3.backups")
        )
    );
}

#[test]
fn native_backup_refuses_legacy_catalogs_and_missing_parents() {
    let root = tempfile::tempdir().unwrap();
    let legacy = root.path().join("legacy.sqlite3");
    migration::initialize(&legacy).unwrap();
    assert!(
        native_restore::backup(&legacy, None)
            .unwrap_err()
            .0
            .contains("native backup requires a native catalog")
    );
    assert!(!PathBuf::from(format!("{}.backups", legacy.display())).exists());
    let (_root, path) = native();
    assert!(
        native_restore::backup(&path, Some(&root.path().join("missing")))
            .unwrap_err()
            .0
            .contains("must be an existing directory")
    );
    // A live session does not block a consistent backup.
    let session = Store::open(&path, false, false).unwrap();
    native_restore::backup(&path, None).unwrap();
    drop(session);
    migration::validate(&connect(&path, false).unwrap(), true).unwrap();
}
