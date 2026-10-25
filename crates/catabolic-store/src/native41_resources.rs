//! Closed, unregistered integration resource description. This module grants no
//! Store admission, migration, bootstrap completion or writer capability.
use crate::{Result, migration};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::OnceLock};

const STATIC_PARTS: &[(&str, &str)] = &[
    ("nfo", include_str!("catalog_nfo_attribution/schema41.sql")),
    (
        "source_succession",
        include_str!("source_succession_schema.sql"),
    ),
    (
        "restore_lineage",
        include_str!("catalog_restore_lineage/schema41.sql"),
    ),
];

pub(crate) fn parts() -> &'static [(&'static str, &'static str)] {
    static MEMBERSHIP: OnceLock<String> = OnceLock::new();
    static SCOPE: OnceLock<String> = OnceLock::new();
    static PARTS: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();
    PARTS.get_or_init(|| {
        let mut parts = STATIC_PARTS.to_vec();
        parts.push((
            "source_observation_provenance",
            crate::catalog_source_succession::current::observation_schema(),
        ));
        parts.push((
            "source_succession_membership",
            MEMBERSHIP
                .get_or_init(crate::catalog_source_succession::current::membership_schema)
                .as_str(),
        ));
        parts.push((
            "source_scope_history",
            SCOPE
                .get_or_init(crate::catalog_source_succession::scope_history::schema)
                .as_str(),
        ));
        parts.push((
            "native_transition_40_41",
            include_str!("migration_native_session/schema41.sql"),
        ));
        parts
    })
}

/// Exact single migration text for the eventual registered resource. Component
/// boundaries are fixed and named. Production registration remains separate.
pub(crate) fn sql() -> &'static str {
    static SQL: OnceLock<String> = OnceLock::new();
    SQL.get_or_init(|| {
        let mut text = String::new();
        for (name, body) in parts() {
            text.push_str("-- native41 component: ");
            text.push_str(name);
            text.push('\n');
            text.push_str(body);
            text.push('\n');
        }
        text
    })
}
pub(crate) fn sql_sha256() -> String {
    catabolic_core::sha256(sql())
}

pub(crate) fn schema(db: &Connection) -> Result<Value> {
    let mut statement = db.prepare("SELECT type,name,sql FROM sqlite_schema WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY type,name")?;
    let rows = statement
        .query_map([], |row| {
            let sql: String = row.get(2)?;
            Ok(json!([
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                sql.split_whitespace().collect::<Vec<_>>().join(" ")
            ]))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(json!(rows))
}

/// Fixed reviewed relation inventory, not a prefix-based query permission.
pub(crate) const SOURCE_VIEWS: &[&str] = &[
    "catalog_source_current_context_membership",
    "catalog_source_required_scopes",
    "catalog_source_current_scope_effects",
    "catalog_source_current_head_effects",
    "catalog_source_bounded_evidence",
    "catalog_source_live_transfers",
    "catalog_source_transferred_facts",
    "catalog_source_effective_file_facts",
    "catalog_source_transferred_headers",
    "catalog_source_effective_headers",
    "catalog_source_effective_assertions",
    "catalog_source_recorded_occurrence_transfers",
    "catalog_source_current_occurrence_transfers",
];

/// Extract only the exact framing emitted by our fixed resource definitions.
/// Preserve body case, whitespace and string literals byte-for-byte.
fn source_view_body<'a>(name: &str, sql: &'a str) -> Result<&'a str> {
    if !SOURCE_VIEWS.contains(&name) {
        return Err(crate::Error("unreviewed source relation".into()));
    }
    sql.strip_prefix(&format!("CREATE VIEW {name} AS\n"))
        .ok_or_else(|| crate::Error("unexpected closed source view framing".into()))
}

fn source_views(db: &Connection) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    let mut remaining = 1024 * 1024i64;
    for name in SOURCE_VIEWS {
        let length: i64 = db.query_row(
            "SELECT length(CAST(sql AS BLOB)) FROM sqlite_schema WHERE type='view' AND name=?1",
            [name],
            |row| row.get(0),
        )?;
        if length < 0 || length > remaining {
            return Err(crate::Error(
                "closed source relation resource exceeds bound".into(),
            ));
        }
        remaining -= length;
        let sql: String = db.query_row(
            "SELECT sql FROM sqlite_schema WHERE type='view' AND name=?1",
            [name],
            |row| row.get(0),
        )?;
        source_view_body(name, &sql)?;
        result.insert((*name).to_owned(), sql);
    }
    Ok(result)
}

/// Raw immutable SQL from the independently constructed shipped reference.
/// Never read target database DDL, and never normalize literals or rebuild the
/// reference per query. The cache is the same exact composite resource cache.
pub(crate) fn source_view_sql(name: &str) -> Result<&'static str> {
    reference()?
        .source_views
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| crate::Error("unreviewed source relation".into()))
}
pub(crate) fn source_view_select(name: &str) -> Result<&'static str> {
    source_view_body(name, source_view_sql(name)?)
}

pub(crate) struct ClosedReference {
    pub(crate) schema: Value,
    pub(crate) predecessor_ledger: Value,
    source_views: BTreeMap<String, String>,
}
/// Reconstructed only from shipped40 resources, never from a target database.
/// The in-memory predecessor is independently validated against closed40 before
/// exact41 SQL is applied. Its generated identity/data are not proof inputs.
pub(crate) fn reference() -> Result<&'static ClosedReference> {
    static REFERENCE: OnceLock<ClosedReference> = OnceLock::new();
    if let Some(value) = REFERENCE.get() {
        return Ok(value);
    }
    let db = migration::closed_native40_reference_connection()?;
    let predecessor_ledger = {
        let mut statement =
            db.prepare("SELECT version,filename,checksum FROM schema_migrations ORDER BY version")?;
        let rows = statement
            .query_map([], |row| {
                Ok(json!([
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?
                ]))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        json!(rows)
    };
    db.execute_batch("BEGIN IMMEDIATE")?;
    migration::execute(
        &db,
        &migration::Migration {
            version: 41,
            filename: "0041_integration_unregistered.sql",
            sql: sql(),
        },
    )?;
    let value = ClosedReference {
        schema: schema(&db)?,
        predecessor_ledger,
        source_views: source_views(&db)?,
    };
    db.execute_batch("ROLLBACK")?;
    let _ = REFERENCE.set(value);
    Ok(REFERENCE.get().expect("closed reference initialized"))
}
pub(crate) fn reference_sha256() -> Result<String> {
    Ok(catabolic_core::sha256(catabolic_core::encode(
        &reference()?.schema,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_seven_components_share_one_exact_resource_and_reference() {
        assert_eq!(parts().len(), 7);
        assert_eq!(parts()[6].0, "native_transition_40_41");
        let db = migration::closed_native40_reference_connection().unwrap();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        for (_, body) in parts() {
            migration::execute(
                &db,
                &migration::Migration {
                    version: 41,
                    filename: "component_test.sql",
                    sql: body,
                },
            )
            .unwrap();
        }
        assert_eq!(schema(&db).unwrap(), reference().unwrap().schema);
        assert_eq!(sql_sha256(), catabolic_core::sha256(sql()));
        assert_eq!(
            reference()
                .unwrap()
                .predecessor_ledger
                .as_array()
                .unwrap()
                .len(),
            40
        );
        db.execute_batch("CREATE TABLE unreviewed_extra(value TEXT)")
            .unwrap();
        assert_ne!(schema(&db).unwrap(), reference().unwrap().schema);
    }
    #[test]
    fn source_relation_cache_preserves_exact_resource_sql() {
        let db = migration::closed_native40_reference_connection().unwrap();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        migration::execute(
            &db,
            &migration::Migration {
                version: 41,
                filename: "source_reference_test.sql",
                sql: sql(),
            },
        )
        .unwrap();
        assert_eq!(SOURCE_VIEWS.len(), 13);
        for name in SOURCE_VIEWS {
            let actual: String = db
                .query_row(
                    "SELECT sql FROM sqlite_schema WHERE type='view' AND name=?1",
                    [name],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(source_view_sql(name).unwrap(), actual);
            assert_eq!(
                source_view_select(name).unwrap(),
                source_view_body(name, &actual).unwrap()
            );
        }
        assert!(source_view_sql("catalog_source_successions").is_err());
        let name = "catalog_source_effective_headers";
        let body = "SELECT 'MiXeD  literal\nValue' AS CaseSensitive";
        let raw = format!("CREATE VIEW {name} AS\n{body}");
        assert_eq!(source_view_body(name, &raw).unwrap(), body);
        assert!(source_view_body(name, &raw.to_lowercase()).is_err());
        assert!(source_view_body(name, "CREATE VIEW other AS\nSELECT 1").is_err());
    }

    #[test]
    fn predecessor_remains_closed40_and_registration_is_unchanged() {
        let db = migration::closed_native40_reference_connection().unwrap();
        assert_eq!(
            schema(&db).unwrap(),
            *migration::native_reference_at(40).unwrap()
        );
        assert!(!migration::is_supported_native_schema(41));
        assert!(migration::native_reference_at(41).is_none());
        assert_eq!(reference_sha256().unwrap().len(), 64);
    }
}
