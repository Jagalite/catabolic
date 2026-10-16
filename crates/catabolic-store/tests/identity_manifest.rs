// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Identity manifests and diffs compare separately captured catalog files.
use catabolic_store::{connect, identity_manifest, migration};
use migration::{NATIVE_SCHEMA_VERSION, NativeUpgradeQuiescence};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn populated() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.sqlite3");
    migration::initialize(&path).unwrap();
    connect(&path, true)
        .unwrap()
        .execute_batch(
            "INSERT INTO locations VALUES('source');
             INSERT INTO files VALUES('file-a','source','a.mkv'),('file-b','source','b.mkv');
             INSERT INTO items VALUES('item-a','movie','{\"title\":\"A\"}'),('item-b','movie','{\"title\":\"B\"}');
             INSERT INTO identities VALUES('tmdb','1','item-a'),('imdb','tt2','item-b');
             INSERT INTO api_events(profile,resource_type,resource_id,state) VALUES('default','job','one','complete'),('default','job','two','complete');",
        )
        .unwrap();
    (root, path)
}
fn copy(root: &Path, path: &Path, name: &str) -> PathBuf {
    let target = root.join(name);
    fs::write(&target, fs::read(path).unwrap()).unwrap();
    target
}
fn kinds(before: &Value, after: &Value) -> Vec<(String, String)> {
    let result = identity_manifest::diff(before, after);
    assert_eq!(
        result["preserved"],
        result["differences"].as_array().unwrap().is_empty()
    );
    result["differences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["kind"].as_str().unwrap().to_owned(),
                d["subject"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}
fn pair(kind: &str, subject: &str) -> (String, String) {
    (kind.into(), subject.into())
}

#[test]
fn manifest_records_identities_without_modifying_the_catalog() {
    let (_root, path) = populated();
    let bytes = fs::read(&path).unwrap();
    let manifest = identity_manifest::manifest_path(&path).unwrap();
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(manifest["contract"], identity_manifest::CONTRACT);
    assert_eq!(manifest["schema"], 32);
    assert_eq!(manifest["catalog"], Value::Null);
    assert_eq!(manifest["history"].as_array().unwrap().len(), 32);
    assert_eq!(manifest["database_id"], manifest["raw_database_id"]);
    for (table, key, rows) in [
        ("items", json!(["id"]), 2),
        ("files", json!(["id"]), 2),
        ("item_files", json!(["id"]), 0),
        ("identities", json!(["namespace", "value"]), 2),
        ("meta", json!(["key"]), 1),
    ] {
        assert_eq!(manifest["tables"][table]["columns"], key, "{table}");
        assert_eq!(manifest["tables"][table]["rows"], rows, "{table}");
    }
    assert!(manifest["tables"].get("schema_migrations").is_none());
    assert_eq!(manifest["sequences"]["api_events"], 2);
    assert_eq!(
        manifest["legacy_identity_evidence"]["identities"]["rows"],
        2
    );
    assert_eq!(
        manifest["legacy_identity_evidence"]["native_baseline"],
        Value::Null
    );
    let mut unsigned = manifest.clone();
    let object = unsigned.as_object_mut().unwrap();
    object.remove("database");
    let fingerprint = object.remove("fingerprint").unwrap();
    assert_eq!(
        fingerprint,
        catabolic_core::sha256(catabolic_core::encode(&unsigned))
    );
    assert_eq!(
        identity_manifest::diff(&manifest, &manifest)["preserved"],
        true
    );
}

#[test]
fn native_upgrade_preserves_every_recorded_identity() {
    let (root, path) = populated();
    let before = identity_manifest::manifest_path(&path).unwrap();
    let report = migration::upgrade_native(
        &path,
        &migration::plan_native(&path).unwrap(),
        NativeUpgradeQuiescence {
            admission_stopped: true,
            readers_and_workers_stopped: true,
            exclusive_operational_ownership: true,
        },
        Some(&root.path().join("backups")),
        &|_, _, _| Ok(()),
    )
    .unwrap();
    let after = identity_manifest::manifest_path(&path).unwrap();
    assert_eq!(after["schema"], NATIVE_SCHEMA_VERSION);
    assert_eq!(after["catalog"]["catalog_id"], before["database_id"]);
    assert_eq!(
        after["legacy_identity_evidence"]["native_baseline"],
        before["legacy_identity_evidence"]["identities"]
    );
    assert_eq!(kinds(&before, &after), vec![]);
    identity_manifest::require_preserved(&before, &after).unwrap();
    // The verified backup is the pre-upgrade identity set exactly.
    let backup =
        identity_manifest::manifest_path(Path::new(report["backup"].as_str().unwrap())).unwrap();
    assert_eq!(backup["fingerprint"], before["fingerprint"]);
    // A downgrade (for example, a restored backup) is reported, not hidden.
    let reversed = kinds(&after, &backup);
    assert!(reversed.contains(&pair("schema_downgraded", "schema")));
    assert!(reversed.contains(&pair("history", "schema_migrations")));
    assert!(reversed.contains(&pair("catalog_id", "catalog_id")));
    assert!(reversed.contains(&pair(
        "legacy_identity_evidence",
        "catalog_legacy_identity_evidence"
    )));
}

#[test]
fn changed_identities_are_reported_individually() {
    let (root, path) = populated();
    let before = identity_manifest::manifest_path(&path).unwrap();
    for (name, sql, expected) in [
        (
            "item.sqlite3",
            "INSERT INTO items VALUES('item-c','movie','{}')",
            vec![pair("table_keys", "items")],
        ),
        (
            "file.sqlite3",
            "DELETE FROM files WHERE id='file-b'",
            vec![pair("table_keys", "files")],
        ),
        (
            "sequence.sqlite3",
            "UPDATE sqlite_sequence SET seq=seq+1 WHERE name='api_events'",
            vec![pair("sequence", "api_events")],
        ),
        (
            "history.sqlite3",
            "UPDATE schema_migrations SET applied_at='changed' WHERE version=4",
            vec![pair("history", "schema_migrations")],
        ),
        (
            "evidence.sqlite3",
            "UPDATE identities SET item_id='item-b' WHERE namespace='tmdb'",
            vec![pair("legacy_identity_evidence", "identities")],
        ),
        (
            "database.sqlite3",
            "UPDATE meta SET value='2ff334d9-68fc-44b9-aa39-e9635593ccaa' WHERE key='database_id'",
            vec![
                pair("database_id", "database_id"),
                pair("raw_database_id", "raw_database_id"),
            ],
        ),
    ] {
        let changed = copy(root.path(), &path, name);
        connect(&changed, true).unwrap().execute_batch(sql).unwrap();
        let after = identity_manifest::manifest_path(&changed).unwrap();
        // Key digests deliberately ignore non-key values; the identity evidence
        // section and the migration owner's whole-row snapshot cover those.
        let mut actual = kinds(&before, &after);
        actual.sort();
        let mut expected = expected;
        expected.sort();
        assert_eq!(actual, expected, "{name}");
        assert!(
            identity_manifest::require_preserved(&before, &after)
                .unwrap_err()
                .0
                .contains("identity preservation failed"),
            "{name}"
        );
    }
}

#[test]
fn native_catalog_identity_and_contract_mismatches_are_reported() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("native.sqlite3");
    migration::initialize_native(&path).unwrap();
    let manifest = identity_manifest::manifest_path(&path).unwrap();
    assert_eq!(manifest["catalog"]["catalog_id"], manifest["database_id"]);
    assert_eq!(
        manifest["catalog"]["incarnation"].as_str().unwrap().len(),
        32
    );
    let mut rotated = manifest.clone();
    rotated["catalog"]["incarnation"] = json!("0".repeat(32));
    rotated["catalog"]["generation"] = json!(99);
    assert_eq!(kinds(&manifest, &sign(rotated)), vec![]);
    let mut changed = manifest.clone();
    changed["catalog"]["catalog_id"] = json!("2ff334d9-68fc-44b9-aa39-e9635593ccaa");
    assert_eq!(
        kinds(&manifest, &sign(changed)),
        vec![pair("catalog_id", "catalog_id")]
    );
    // A schema-34+ manifest without its native baseline is incomplete evidence.
    let mut dropped = manifest.clone();
    dropped["legacy_identity_evidence"]["native_baseline"] = Value::Null;
    assert_eq!(
        kinds(&manifest, &sign(dropped)),
        vec![pair("invalid_manifest", "after")]
    );
    let mut foreign = manifest.clone();
    foreign["contract"] = json!("other");
    assert_eq!(
        kinds(&manifest, &sign(foreign)),
        vec![pair("invalid_manifest", "after")]
    );
}

/// Recompute a fingerprint so a test isolates one deliberate change.
fn sign(mut manifest: Value) -> Value {
    let object = manifest.as_object_mut().unwrap();
    object.remove("database");
    object.remove("fingerprint");
    let fingerprint = catabolic_core::sha256(catabolic_core::encode(&manifest));
    manifest["fingerprint"] = json!(fingerprint);
    manifest
}

#[test]
fn incomplete_or_unsigned_manifests_are_never_compared() {
    let (root, path) = populated();
    let legacy = identity_manifest::manifest_path(&path).unwrap();
    identity_manifest::validate(&legacy).unwrap();
    // A file round trip keeps the fingerprint valid.
    let file = root.path().join("manifest.json");
    fs::write(&file, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();
    let reread = catabolic_core::decode_json_bytes(&fs::read(&file).unwrap()).unwrap();
    identity_manifest::validate(&reread).unwrap();
    assert_eq!(kinds(&legacy, &reread), vec![]);

    // The reviewer's case: contract and schemas only.
    let bare_before = json!({"contract":identity_manifest::CONTRACT,"schema":32});
    let bare_after = json!({"contract":identity_manifest::CONTRACT,"schema":NATIVE_SCHEMA_VERSION});
    assert_eq!(
        kinds(&bare_before, &bare_after),
        vec![
            pair("invalid_manifest", "before"),
            pair("invalid_manifest", "after")
        ]
    );
    assert!(
        identity_manifest::require_preserved(&bare_before, &bare_after)
            .unwrap_err()
            .0
            .contains("invalid_manifest")
    );

    let mut cases: Vec<(&str, Value)> = Vec::new();
    for field in [
        "history",
        "tables",
        "sequences",
        "database_id",
        "raw_database_id",
        "legacy_identity_evidence",
        "catalog",
        "fingerprint",
    ] {
        let mut missing = legacy.clone();
        missing.as_object_mut().unwrap().remove(field);
        cases.push((field, sign_unless(field, missing)));
    }
    let mut empty_history = legacy.clone();
    empty_history["history"] = json!([]);
    cases.push(("empty history", sign(empty_history)));
    let mut null_id = legacy.clone();
    null_id["database_id"] = Value::Null;
    cases.push(("null database_id", sign(null_id)));
    let mut no_items = legacy.clone();
    no_items["tables"].as_object_mut().unwrap().remove("items");
    cases.push(("missing items digest", sign(no_items)));
    let mut bad_digest = legacy.clone();
    bad_digest["tables"]["files"]["sha256"] = json!("short");
    cases.push(("malformed digest", sign(bad_digest)));
    let mut bad_sequence = legacy.clone();
    bad_sequence["sequences"]["api_events"] = json!("2");
    cases.push(("string sequence", sign(bad_sequence)));
    let mut legacy_catalog = legacy.clone();
    legacy_catalog["catalog"] = json!({"catalog_id":"x","incarnation":"y","generation":0});
    cases.push(("legacy with catalog", sign(legacy_catalog)));
    let mut extra = legacy.clone();
    extra["unexpected"] = json!(true);
    cases.push(("unknown field", sign(extra)));
    // Changing evidence without re-signing breaks the fingerprint.
    let mut unsigned = legacy.clone();
    unsigned["tables"]["items"]["rows"] = json!(3);
    cases.push(("unsigned change", unsigned));
    for (label, manifest) in cases {
        assert!(identity_manifest::validate(&manifest).is_err(), "{label}");
        assert_eq!(
            kinds(&legacy, &manifest),
            vec![pair("invalid_manifest", "after")],
            "{label}"
        );
        assert_eq!(
            kinds(&manifest, &legacy),
            vec![pair("invalid_manifest", "before")],
            "{label}"
        );
    }
}

fn sign_unless(field: &str, manifest: Value) -> Value {
    if field == "fingerprint" {
        manifest
    } else {
        sign(manifest)
    }
}

/// Omissions survive a recomputed fingerprint, so the table and key inventory
/// and sequence completeness are checked against the declared schema itself.
#[test]
fn resigned_omissions_from_the_inventory_are_rejected() {
    let (root, path) = populated();
    connect(&path, true)
        .unwrap()
        .execute_batch("INSERT INTO item_files(id,file_id,item_id,role,origin) VALUES('assoc-a','file-a','item-a','primary','explicit')")
        .unwrap();
    let before = identity_manifest::manifest_path(&path).unwrap();
    identity_manifest::validate(&before).unwrap();
    assert_eq!(before["tables"]["item_files"]["rows"], 1);

    // The reviewer's case: drop item_files from "before", re-sign, and compare
    // with a genuine "after" whose item_files rows were deleted.
    let mut omitted = before.clone();
    omitted["tables"]
        .as_object_mut()
        .unwrap()
        .remove("item_files");
    let omitted = sign(omitted);
    let deleted = copy(root.path(), &path, "deleted.sqlite3");
    connect(&deleted, true)
        .unwrap()
        .execute_batch("DELETE FROM item_files")
        .unwrap();
    let after = identity_manifest::manifest_path(&deleted).unwrap();
    assert_eq!(
        kinds(&omitted, &after),
        vec![pair("invalid_manifest", "before")]
    );
    assert_eq!(
        kinds(&before, &after),
        vec![pair("table_keys", "item_files")]
    );

    let mut cases: Vec<(&str, Value)> = Vec::new();
    let mut ghost = before.clone();
    ghost["tables"]["ghost"] = before["tables"]["items"].clone();
    cases.push(("unexpected table", ghost));
    let mut rekeyed = before.clone();
    rekeyed["tables"]["items"]["columns"] = json!(["rowid"]);
    cases.push(("wrong key columns", rekeyed));
    let mut no_sequence = before.clone();
    no_sequence["sequences"]
        .as_object_mut()
        .unwrap()
        .remove("api_events");
    cases.push(("sequence omitted for a table with rows", no_sequence));
    let mut foreign_sequence = before.clone();
    foreign_sequence["sequences"]["items"] = json!(1);
    cases.push((
        "sequence for a table without AUTOINCREMENT",
        foreign_sequence,
    ));
    let mut unknown_schema = before.clone();
    unknown_schema["schema"] = json!(NATIVE_SCHEMA_VERSION + 1);
    cases.push(("unsupported schema", unknown_schema));
    for (label, manifest) in cases {
        let manifest = sign(manifest);
        assert!(identity_manifest::validate(&manifest).is_err(), "{label}");
        assert_eq!(
            kinds(&manifest, &before),
            vec![pair("invalid_manifest", "before")],
            "{label}"
        );
    }

    // The native inventory, including derived search tables, is enforced too.
    let native = root.path().join("native.sqlite3");
    migration::initialize_native(&native).unwrap();
    let manifest = identity_manifest::manifest_path(&native).unwrap();
    identity_manifest::validate(&manifest).unwrap();
    for table in [
        "item_files",
        "catalog_state",
        "catalog_legacy_identity_evidence",
    ] {
        let mut omitted = manifest.clone();
        assert!(
            omitted["tables"]
                .as_object_mut()
                .unwrap()
                .remove(table)
                .is_some(),
            "{table}"
        );
        assert!(
            identity_manifest::validate(&sign(omitted)).is_err(),
            "{table}"
        );
    }
}
