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
/// differences here; restore admission is a separate decision.
pub fn diff(before: &Value, after: &Value) -> Value {
    let mut differences = Vec::new();
    let null = Value::Null;
    for manifest in [before, after] {
        if manifest["contract"] != CONTRACT {
            difference(
                &mut differences,
                "contract",
                "contract",
                &json!(CONTRACT),
                &manifest["contract"],
            );
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
