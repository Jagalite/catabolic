// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Identity-preservation evidence for explicit upgrades, rehearsals and restores.
//!
//! A manifest records the identities an upgrade must keep: the database and
//! native catalog IDs, every existing table's primary-key set, migration
//! history, AUTOINCREMENT sequences and legacy identity evidence. It does not
//! replace the migration owner's whole-row preservation check; it is the
//! operator-visible comparison between two separately captured files.
use crate::{Error, Result, connect, database_path, migration};
use catabolic_core::{encode, sha256};
use rusqlite::Connection;
use serde_json::{Map, Value, json};
use std::path::Path;

pub const CONTRACT: &str = "catabolic-identity-manifest-v1";
const LEGACY_EVIDENCE: [&str; 3] = ["namespace", "value", "item_id"];

fn table_exists(db: &Connection, name: &str) -> Result<bool> {
    Ok(db
        .prepare("SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?")?
        .exists([name])?)
}
/// Declared primary-key columns in key order. Rowid tables without a declared
/// key are identified by rowid, which SQLite's backup and migration paths keep.
fn key_columns(db: &Connection, table: &str) -> Result<Vec<String>> {
    let mut statement = db.prepare(&format!(
        "SELECT name FROM pragma_table_info({}) WHERE pk>0 ORDER BY pk",
        migration::identifier(table)
    ))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(if columns.is_empty() {
        vec!["rowid".into()]
    } else {
        columns
    })
}

/// Capture the identity manifest from one connection. Callers that need a
/// consistent view must hold a read transaction; `manifest_path` does so.
pub fn manifest(db: &Connection) -> Result<Value> {
    let info = migration::validate(db, false)?;
    let raw_database_id: String = db.query_row(
        "SELECT value FROM meta WHERE key='database_id'",
        [],
        |row| row.get(0),
    )?;
    let catalog = if table_exists(db, "catalog_state")? {
        db.query_row(
            "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
            [],
            |row| {
                Ok(json!({
                    "catalog_id":row.get::<_, String>(0)?,
                    "incarnation":row.get::<_, String>(1)?,
                    "generation":row.get::<_, i64>(2)?
                }))
            },
        )?
    } else {
        Value::Null
    };
    let mut tables = Map::new();
    for row in migration::rows(
        db,
        "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name<>'schema_migrations' ORDER BY name",
        &[],
    )? {
        let name = row["name"].as_str().expect("table name");
        let columns = key_columns(db, name)?;
        tables.insert(name.into(), migration::digest_table(db, name, &columns)?);
    }
    let mut sequences = Map::new();
    if table_exists(db, "sqlite_sequence")? {
        for row in migration::rows(
            db,
            "SELECT name,seq FROM sqlite_sequence ORDER BY name",
            &[],
        )? {
            sequences.insert(
                row["name"].as_str().expect("sequence name").into(),
                row["seq"].clone(),
            );
        }
    }
    let evidence_columns = LEGACY_EVIDENCE.map(String::from);
    let legacy = json!({
        "identities": migration::digest_table(db, "identities", &evidence_columns)?,
        "native_baseline": if table_exists(db, "catalog_legacy_identity_evidence")? {
            migration::digest_table(db, "catalog_legacy_identity_evidence", &evidence_columns)?
        } else {
            Value::Null
        }
    });
    let mut manifest = json!({
        "contract":CONTRACT,
        "schema":info["schema"],
        "database_id":info["database_id"],
        "raw_database_id":raw_database_id,
        "catalog":catalog,
        "history":info["history"],
        "tables":tables,
        "sequences":sequences,
        "legacy_identity_evidence":legacy,
    });
    manifest["fingerprint"] = json!(sha256(encode(&manifest)));
    Ok(manifest)
}

/// Read-only manifest of a database path inside one consistent read transaction.
/// A hot rollback journal left by an interrupted writer must be recovered by a
/// writable open first; this function never performs that recovery itself.
pub fn manifest_path(path: &Path) -> Result<Value> {
    let path = database_path(path)?;
    let db = connect(&path, false)?;
    db.execute_batch("BEGIN")?;
    let mut value = manifest(&db)?;
    value["database"] = json!(path);
    Ok(value)
}

/// Expected tables of one schema version: key columns as `manifest` derives
/// them, and whether the table uses AUTOINCREMENT. Built once per version by
/// creating the frozen schema signature's tables in a private in-memory
/// database, so a manifest cannot omit or rename a table or its key.
type Inventory = std::collections::BTreeMap<String, (Vec<String>, bool)>;

fn expected_inventory(schema: u32) -> std::result::Result<Inventory, String> {
    use std::{collections::BTreeMap, sync::Mutex};
    static CACHE: Mutex<BTreeMap<u32, Inventory>> = Mutex::new(BTreeMap::new());
    if let Some(found) = CACHE.lock().expect("inventory cache").get(&schema) {
        return Ok(found.clone());
    }
    let signature = if (1..=migration::SCHEMA_VERSION).contains(&schema) {
        &migration::reference()["schemas"][schema.to_string()]
    } else {
        migration::native_reference_at(schema)
            .ok_or_else(|| format!("schema {schema} is not a supported schema"))?
    };
    let build = || -> Result<Inventory> {
        let db = Connection::open_in_memory()?;
        crate::components::install(&db)?;
        let mut inventory = Inventory::new();
        for entry in signature.as_array().into_iter().flatten() {
            let (Some("table"), Some(name), Some(sql)) =
                (entry[0].as_str(), entry[1].as_str(), entry[2].as_str())
            else {
                continue;
            };
            // Virtual tables create their shadow tables themselves.
            if !table_exists(&db, name)? {
                db.execute_batch(sql)?;
            }
            if name != "schema_migrations" {
                let autoincrement = sql.to_ascii_uppercase().contains("AUTOINCREMENT");
                inventory.insert(name.to_owned(), (key_columns(&db, name)?, autoincrement));
            }
        }
        Ok(inventory)
    };
    let inventory = build().map_err(|error| format!("schema {schema} inventory: {error}"))?;
    if inventory.is_empty() {
        return Err(format!("schema {schema} has no table signature"));
    }
    CACHE
        .lock()
        .expect("inventory cache")
        .insert(schema, inventory.clone());
    Ok(inventory)
}

fn is_digest(value: &Value) -> bool {
    value["columns"]
        .as_array()
        .is_some_and(|columns| !columns.is_empty() && columns.iter().all(Value::is_string))
        && value["rows"].is_u64()
        && value["sha256"].as_str().is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        })
        && value.as_object().is_some_and(|object| object.len() == 3)
}

/// Reject a manifest that is incomplete, mistyped or does not match its own
/// fingerprint. Comparing partial evidence could otherwise report missing
/// identities as preserved.
pub fn validate(manifest: &Value) -> std::result::Result<(), String> {
    let object = manifest
        .as_object()
        .ok_or_else(|| "manifest is not an object".to_owned())?;
    if manifest["contract"] != CONTRACT {
        return Err(format!("contract is not {CONTRACT}"));
    }
    let expected: &[&str] = &[
        "catalog",
        "contract",
        "database_id",
        "fingerprint",
        "history",
        "legacy_identity_evidence",
        "raw_database_id",
        "schema",
        "sequences",
        "tables",
    ];
    let mut keys = object
        .keys()
        .map(String::as_str)
        .filter(|key| *key != "database")
        .collect::<Vec<_>>();
    keys.sort_unstable();
    if keys != expected {
        return Err(format!("fields are {keys:?}, expected {expected:?}"));
    }
    if object.get("database").is_some_and(|path| !path.is_string()) {
        return Err("database is not a path string".into());
    }
    let schema = manifest["schema"]
        .as_u64()
        .filter(|schema| *schema >= 1)
        .ok_or_else(|| "schema is not a positive integer".to_owned())?;
    let database_id = manifest["database_id"]
        .as_str()
        .ok_or_else(|| "database_id is not a string".to_owned())?;
    if uuid::Uuid::parse_str(database_id)
        .ok()
        .map(|id| id.to_string())
        .as_deref()
        != Some(database_id)
    {
        return Err("database_id is not a canonical UUID".into());
    }
    let raw = manifest["raw_database_id"]
        .as_str()
        .ok_or_else(|| "raw_database_id is not a string".to_owned())?;
    if uuid::Uuid::parse_str(raw)
        .ok()
        .map(|id| id.to_string())
        .as_deref()
        != Some(database_id)
    {
        return Err("raw_database_id does not identify database_id".into());
    }
    let native = schema >= u64::from(migration::FIRST_NATIVE_SCHEMA_VERSION);
    let catalog = &manifest["catalog"];
    if native {
        let complete = catalog.as_object().is_some_and(|object| object.len() == 3)
            && catalog["catalog_id"].is_string()
            && catalog["incarnation"].is_string()
            && catalog["generation"].is_i64();
        if !complete {
            return Err("native catalog identity is missing or incomplete".into());
        }
    } else if !catalog.is_null() {
        return Err("a legacy schema has no native catalog identity".into());
    }
    let history = manifest["history"]
        .as_array()
        .ok_or_else(|| "history is not an array".to_owned())?;
    let expected_history = if schema >= 2 { schema as usize } else { 0 };
    if history.len() != expected_history {
        return Err(format!(
            "history has {} entries for schema {schema}",
            history.len()
        ));
    }
    for (index, entry) in history.iter().enumerate() {
        let complete = entry.as_object().is_some_and(|object| object.len() == 5)
            && entry["version"].as_u64() == Some(index as u64 + 1)
            && ["filename", "checksum", "origin", "applied_at"]
                .iter()
                .all(|field| entry[*field].is_string());
        if !complete {
            return Err(format!("history entry {} is incomplete", index + 1));
        }
    }
    let tables = manifest["tables"]
        .as_object()
        .ok_or_else(|| "tables is not an object".to_owned())?;
    let inventory =
        expected_inventory(u32::try_from(schema).map_err(|_| "schema is out of range")?)?;
    if !tables
        .keys()
        .collect::<std::collections::BTreeSet<_>>()
        .eq(&inventory.keys().collect())
    {
        let missing = inventory
            .keys()
            .filter(|name| !tables.contains_key(*name))
            .collect::<Vec<_>>();
        let unexpected = tables
            .keys()
            .filter(|name| !inventory.contains_key(*name))
            .collect::<Vec<_>>();
        return Err(format!(
            "table inventory does not match schema {schema}: missing {missing:?}, unexpected {unexpected:?}"
        ));
    }
    for (name, digest) in tables {
        if !is_digest(digest) || digest["columns"] != json!(inventory[name].0) {
            return Err(format!(
                "table key digest for {name} is incomplete or has the wrong key"
            ));
        }
    }
    let sequences = manifest["sequences"]
        .as_object()
        .ok_or_else(|| "sequences is not an object".to_owned())?;
    for (name, seq) in sequences {
        if !seq.is_i64() || !inventory.get(name).is_some_and(|(_, auto)| *auto) {
            return Err(format!(
                "sequence {name} is not an AUTOINCREMENT table's integer"
            ));
        }
    }
    // A row in an AUTOINCREMENT table proves sqlite_sequence recorded it.
    for (name, (_, autoincrement)) in &inventory {
        if *autoincrement && tables[name]["rows"] != 0 && !sequences.contains_key(name) {
            return Err(format!(
                "sequence for {name} is missing although the table has rows"
            ));
        }
    }
    let legacy = &manifest["legacy_identity_evidence"];
    if legacy.as_object().is_none_or(|object| object.len() != 2)
        || !is_digest(&legacy["identities"])
    {
        return Err("legacy identity evidence is incomplete".into());
    }
    let baseline = &legacy["native_baseline"];
    let has_baseline = schema >= 34;
    if has_baseline && !is_digest(baseline) || !has_baseline && !baseline.is_null() {
        return Err("native legacy identity baseline does not match the schema".into());
    }
    let mut unsigned = manifest.clone();
    let unsigned_object = unsigned.as_object_mut().expect("object checked above");
    unsigned_object.remove("database");
    let fingerprint = unsigned_object.remove("fingerprint");
    if fingerprint.as_ref().and_then(Value::as_str) != Some(sha256(encode(&unsigned)).as_str()) {
        return Err("fingerprint does not match the manifest contents".into());
    }
    Ok(())
}

fn difference(
    differences: &mut Vec<Value>,
    kind: &str,
    subject: &str,
    before: &Value,
    after: &Value,
) {
    differences.push(json!({"kind":kind,"subject":subject,"before":before,"after":after}));
}

/// Compare a manifest captured before an upgrade, rehearsal or restore with one
/// captured afterwards. New tables, sequences and history entries are allowed;
/// any added, removed or changed key in an existing table, or any changed
/// existing sequence or history row, is reported. Schema downgrades are only
/// differences here; restore admission is a separate decision. Both manifests
/// must first pass `validate`; incomplete evidence is never compared.
pub fn diff(before: &Value, after: &Value) -> Value {
    let mut differences = Vec::new();
    let null = Value::Null;
    for (subject, manifest) in [("before", before), ("after", after)] {
        if let Err(reason) = validate(manifest) {
            differences.push(json!({"kind":"invalid_manifest","subject":subject,"reason":reason}));
        }
    }
    if !differences.is_empty() {
        return json!({"preserved":false,"differences":differences});
    }
    for field in ["database_id", "raw_database_id"] {
        if before[field] != after[field] {
            difference(
                &mut differences,
                field,
                field,
                &before[field],
                &after[field],
            );
        }
    }
    match (before["schema"].as_u64(), after["schema"].as_u64()) {
        (Some(old), Some(new)) if new >= old => {}
        _ => difference(
            &mut differences,
            "schema_downgraded",
            "schema",
            &before["schema"],
            &after["schema"],
        ),
    }
    // Native catalog identity is derived once from the canonical database ID
    // and must never change afterwards. Incarnation and generation may change.
    let catalog_id = &after["catalog"]["catalog_id"];
    if !before["catalog"].is_null() {
        if before["catalog"]["catalog_id"] != *catalog_id {
            difference(
                &mut differences,
                "catalog_id",
                "catalog_id",
                &before["catalog"]["catalog_id"],
                catalog_id,
            );
        }
    } else if !after["catalog"].is_null() && *catalog_id != before["database_id"] {
        difference(
            &mut differences,
            "catalog_id",
            "catalog_id",
            &before["database_id"],
            catalog_id,
        );
    }
    let old_history = before["history"].as_array().map_or(&[][..], Vec::as_slice);
    let new_history = after["history"].as_array().map_or(&[][..], Vec::as_slice);
    if new_history.len() < old_history.len() || new_history[..old_history.len()] != *old_history {
        difference(
            &mut differences,
            "history",
            "schema_migrations",
            &before["history"],
            &after["history"],
        );
    }
    // Schema34 appends exactly its legacy-identity baseline to this index. The
    // migration owner verifies that exception row by row; key digests cannot.
    let schema34_index = before["schema"].as_u64().is_some_and(|v| v < 34)
        && after["schema"].as_u64().is_some_and(|v| v >= 34);
    for (section, kind) in [("tables", "table_keys"), ("sequences", "sequence")] {
        if let Some(entries) = before[section].as_object() {
            for (name, expected) in entries {
                let actual = after[section].get(name).unwrap_or(&null);
                if schema34_index && section == "tables" && name == "catalog_entity_revisions" {
                    continue;
                }
                if actual != expected {
                    difference(&mut differences, kind, name, expected, actual);
                }
            }
        }
    }
    let old_legacy = &before["legacy_identity_evidence"];
    let new_legacy = &after["legacy_identity_evidence"];
    if old_legacy["identities"] != new_legacy["identities"] {
        difference(
            &mut differences,
            "legacy_identity_evidence",
            "identities",
            &old_legacy["identities"],
            &new_legacy["identities"],
        );
    }
    // The schema34 baseline is the complete legacy identity set at upgrade time.
    let expected_baseline = if old_legacy["native_baseline"].is_null() {
        &old_legacy["identities"]
    } else {
        &old_legacy["native_baseline"]
    };
    if !new_legacy["native_baseline"].is_null()
        && new_legacy["native_baseline"] != *expected_baseline
    {
        difference(
            &mut differences,
            "legacy_identity_evidence",
            "catalog_legacy_identity_evidence",
            expected_baseline,
            &new_legacy["native_baseline"],
        );
    }
    if !old_legacy["native_baseline"].is_null() && new_legacy["native_baseline"].is_null() {
        difference(
            &mut differences,
            "legacy_identity_evidence",
            "catalog_legacy_identity_evidence",
            &old_legacy["native_baseline"],
            &null,
        );
    }
    json!({"preserved":differences.is_empty(),"differences":differences})
}

/// Refuse with every reported difference when identities were not preserved.
pub fn require_preserved(before: &Value, after: &Value) -> Result<()> {
    let result = diff(before, after);
    if result["preserved"] != true {
        return Err(Error(format!(
            "identity preservation failed: {}",
            encode(&result["differences"])
        )));
    }
    Ok(())
}
