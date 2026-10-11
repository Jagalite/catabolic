// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Durable opt-in catalog-refresh configuration, generation fences and retries.
use crate::{Error, Result, Store, migration::rows};
use serde_json::{Value, json};

/// Called in the producer's existing transaction. Queue generations and watcher
/// event suppression therefore commit or roll back with that producer.
pub fn enqueue(
    db: &rusqlite::Connection,
    profile: &str,
    observation_job: Option<&str>,
) -> Result<()> {
    crate::observation::dirty_watchers(db, profile, observation_job)?;
    db.execute("INSERT INTO catalog_refresh_queue(profile,catalog) SELECT profile,catalog FROM catalog_refresh_settings WHERE profile=? AND enabled=1 AND NOT EXISTS (SELECT 1 FROM watcher_owners w WHERE w.profile=catalog_refresh_settings.profile AND w.catalog=catalog_refresh_settings.catalog) ON CONFLICT(profile,catalog) DO UPDATE SET generation=generation+1,next_attempt=0,error=NULL,updated_at=CURRENT_TIMESTAMP", [profile])?;
    Ok(())
}

/// Python evaluates enabled only after these native configuration preconditions.
pub fn validate_configuration_preconditions<'a>(
    store: &Store,
    request: &'a Value,
) -> Result<(&'a str, i64)> {
    crate::mutation::require_recovered(&store.db)?;
    configuration_parameters(store, request)
}

fn configuration_parameters<'a>(store: &Store, request: &'a Value) -> Result<(&'a str, i64)> {
    let removals = request
        .get("max_removals")
        .map(|v| {
            v.as_i64()
                .filter(|v| *v >= 0)
                .ok_or_else(|| Error("max removals must be a nonnegative integer".into()))
        })
        .transpose()?
        .unwrap_or(0);
    let catalog = request["catalog"]
        .as_str()
        .ok_or_else(|| Error("unknown catalog".into()))?;
    if !store
        .db
        .prepare("SELECT 1 FROM catalogs WHERE id=?")?
        .exists([catalog])?
    {
        return Err(Error("unknown catalog".into()));
    }
    Ok((catalog, removals))
}

pub fn validate_configuration<'a>(
    store: &Store,
    profile: &str,
    request: &'a Value,
) -> Result<(&'a str, bool, i64)> {
    crate::mutation::require_recovered(&store.db)?;
    // Retain the strict companion's boolean/type validation order.
    request["catalog"]
        .as_str()
        .ok_or_else(|| Error("unknown catalog".into()))?;
    let enabled = request["enabled"]
        .as_bool()
        .ok_or_else(|| Error("enabled must be boolean".into()))?;
    let (catalog, removals) = configuration_parameters(store, request)?;
    if enabled {
        let fallback = crate::fallback_projection::binding(store, profile, catalog)?.is_some();
        let has_owner = if fallback {
            true
        } else {
            let owner = rows(
                &store.db,
                "SELECT value FROM meta WHERE key=?",
                &[format!("layout-catalog:{catalog}").into()],
            )?;
            owner
                .first()
                .map(|row| -> Result<Value> {
                    let raw = row["value"].as_str().ok_or_else(|| {
                        Error("catalog refresh owner metadata must be text".into())
                    })?;
                    Ok(catabolic_core::decode_json(raw)?)
                })
                .transpose()?
                .as_ref()
                .is_some_and(|value| match value {
                    Value::Null => false,
                    Value::Bool(value) => *value,
                    Value::Number(value) => value.as_f64().is_none_or(|value| value != 0.0),
                    Value::String(value) => !value.is_empty(),
                    Value::Array(value) => !value.is_empty(),
                    Value::Object(value) => !value.is_empty(),
                })
        };
        if !has_owner {
            return Err(Error(
                "apply a saved layout to the catalog before enabling automatic refresh".into(),
            ));
        }
    }
    Ok((catalog, enabled, removals))
}

pub fn configure(store: &Store, profile: &str, request: &Value) -> Result<Value> {
    let (_, enabled, _) = validate_configuration(store, profile, request)?;
    configure_integer(store, profile, request, i64::from(enabled))
}

/// Public Python's SQLite parameter is int(enabled), while its control flow
/// uses truthiness. Keep that compatibility separate from the strict JSON API.
pub fn configure_integer(
    store: &Store,
    profile: &str,
    request: &Value,
    stored_enabled: i64,
) -> Result<Value> {
    let (catalog, enabled, removals) = validate_configuration(store, profile, request)?;
    store.transaction(|db| {
                db.execute("INSERT INTO catalog_refresh_settings VALUES(?,?,?,?) ON CONFLICT(profile,catalog) DO UPDATE SET enabled=excluded.enabled,max_removals=excluded.max_removals",(profile,catalog,stored_enabled,removals))?;
                if enabled {
                    db.execute("INSERT INTO catalog_refresh_queue(profile,catalog) VALUES(?,?) ON CONFLICT(profile,catalog) DO UPDATE SET generation=generation+1,next_attempt=0,error=NULL",(profile,catalog))?;
                } else {db.execute("DELETE FROM catalog_refresh_queue WHERE profile=? AND catalog=?",(profile,catalog))?;}
                Ok(())
            })?;
    Ok(json!({"catalog":catalog,"enabled":enabled,"max_removals":removals}))
}
pub fn pending(store: &Store, profile: &str, limit: usize) -> Result<Value> {
    let pending = rows(
        &store.db,
        "SELECT q.* FROM catalog_refresh_queue q JOIN catalog_refresh_settings s USING(profile,catalog) WHERE q.profile=? AND s.enabled=1 ORDER BY q.next_attempt,q.catalog LIMIT ?",
        &[profile.to_string().into(), ((limit + 1) as i64).into()],
    )?;
    Ok(
        json!({"pending":pending.iter().take(limit).collect::<Vec<_>>(),"truncated":pending.len()>limit}),
    )
}
pub fn selected(
    store: &Store,
    profile: &str,
    catalog: Option<&str>,
    limit: usize,
    force: bool,
) -> Result<Vec<Value>> {
    rows(
        &store.db,
        "SELECT q.*,s.max_removals FROM catalog_refresh_queue q JOIN catalog_refresh_settings s USING(profile,catalog) WHERE q.profile=? AND s.enabled=1 AND (? IS NULL OR q.catalog=?) AND (? OR q.next_attempt<=unixepoch('now')) ORDER BY q.next_attempt,q.updated_at,q.catalog LIMIT ?",
        &[
            profile.to_string().into(),
            catalog.map(str::to_string).into(),
            catalog.map(str::to_string).into(),
            i64::from(force).into(),
            (limit as i64).into(),
        ],
    )
}
pub fn pending_count(store: &Store, profile: &str, catalog: Option<&str>) -> Result<i64> {
    Ok(store.db.query_row("SELECT count(*) FROM catalog_refresh_queue q JOIN catalog_refresh_settings s USING(profile,catalog) WHERE q.profile=? AND s.enabled=1 AND (? IS NULL OR q.catalog=?)",(profile,catalog,catalog),|r|r.get(0))?)
}
pub fn acknowledge(store: &Store, profile: &str, row: &Value) -> Result<bool> {
    store.transaction(|db| {
        Ok(db.execute(
            "DELETE FROM catalog_refresh_queue WHERE profile=? AND catalog=? AND generation=?",
            (profile, row["catalog"].as_str(), row["generation"].as_i64()),
        )? > 0)
    })
}
pub fn retry(store: &Store, profile: &str, row: &Value, error: &str) -> Result<bool> {
    let backoff = (5i64 << row["attempts"].as_i64().unwrap_or(0).clamp(0, 10)).min(3600);
    store.transaction(|db| Ok(db.execute("UPDATE catalog_refresh_queue SET attempts=attempts+1,error=?,next_attempt=unixepoch('now')+? WHERE profile=? AND catalog=? AND generation=?",(error,backoff,profile,row["catalog"].as_str(),row["generation"].as_i64()))? > 0))
}

pub struct Target {
    pub watcher_owned: bool,
    pub pending_journal: bool,
    pub fallback: Option<Value>,
    pub layout: Option<String>,
}
pub fn target(store: &Store, profile: &str, catalog: &str) -> Result<Target> {
    let watcher_owned = store
        .db
        .prepare("SELECT 1 FROM watcher_owners WHERE profile=? AND catalog=?")?
        .exists((profile, catalog))?;
    // Watchers own publication for their catalogs. Do not inspect stale or
    // malformed layout metadata for a row the refresh worker must skip.
    if watcher_owned {
        return Ok(Target {
            watcher_owned: true,
            pending_journal: false,
            fallback: None,
            layout: None,
        });
    }
    let pending_journal = store
        .db
        .prepare("SELECT 1 FROM journal WHERE profile=? AND catalog=? LIMIT 1")?
        .exists((profile, catalog))?;
    let fallback = crate::fallback_projection::binding(store, profile, catalog)?;
    let owner = rows(
        &store.db,
        "SELECT value FROM meta WHERE key=?",
        &[format!("layout-catalog:{catalog}").into()],
    )?;
    let owner = owner
        .first()
        .map(|row| catabolic_core::decode_json(row["value"].as_str().unwrap()))
        .transpose()?;
    let layout = owner.and_then(|value| value["layout"].as_str().map(str::to_string));
    Ok(Target {
        watcher_owned,
        pending_journal,
        fallback,
        layout,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejected_owner_metadata_does_not_change_configuration_or_queue() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("catalog.db");
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        for owner in ["{}", "null", "false", "[]", "\"\"", "0", "0.0", "{"] {
            store
                .transaction(|db| {
                    db.execute(
                        "INSERT OR REPLACE INTO meta(key,value) VALUES('layout-catalog:global',?)",
                        [owner],
                    )?;
                    Ok(())
                })
                .unwrap();
            let error = configure(
                &store,
                "default",
                &json!({"catalog":"global","enabled":true}),
            )
            .unwrap_err();
            assert_eq!(
                error.0,
                if owner == "{" {
                    "invalid JSON"
                } else {
                    "apply a saved layout to the catalog before enabling automatic refresh"
                }
            );
            assert_eq!(
                store
                    .db
                    .query_row("SELECT count(*) FROM catalog_refresh_settings", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap(),
                0
            );
            assert_eq!(pending_count(&store, "default", None).unwrap(), 0);
            assert_eq!(
                store
                    .db
                    .query_row("SELECT count(*) FROM catalog_refresh_queue", [], |row| row
                        .get::<_, i64>(
                        0
                    ))
                    .unwrap(),
                0
            );
        }
    }

    #[test]
    fn rejected_nontext_owner_metadata_returns_error_without_mutation() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("catalog.db");
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        store
            .transaction(|db| {
                db.execute(
                    "INSERT INTO meta(key,value) VALUES('layout-catalog:global',X'7b')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            configure(
                &store,
                "default",
                &json!({"catalog":"global","enabled":true})
            )
            .unwrap_err()
            .0,
            "catalog refresh owner metadata must be text"
        );
        for table in ["catalog_refresh_settings", "catalog_refresh_queue"] {
            assert_eq!(
                store
                    .db
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }

    #[test]
    fn late_acknowledgment_and_failure_do_not_clear_newer_generation() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("catalog.db");
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        store
            .transaction(|db| {
                db.execute(
                    "INSERT INTO catalog_refresh_settings VALUES('default','global',1,0)",
                    [],
                )?;
                db.execute(
                    "INSERT INTO catalog_refresh_queue(profile,catalog) VALUES('default','global')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let original = selected(&store, "default", None, 100, false)
            .unwrap()
            .remove(0);
        store.transaction(|db| {db.execute("UPDATE catalog_refresh_queue SET generation=generation+1,next_attempt=0,error=NULL",[])?;Ok(())}).unwrap();
        assert!(!acknowledge(&store, "default", &original).unwrap());
        assert!(!retry(&store, "default", &original, "late failure").unwrap());
        let newer = selected(&store, "default", None, 100, false)
            .unwrap()
            .remove(0);
        assert_eq!(newer["generation"], 2);
        assert_eq!(newer["attempts"], 0);
        assert!(newer["error"].is_null());
        assert!(retry(&store, "default", &newer, "offline").unwrap());
        assert!(
            selected(&store, "default", None, 100, false)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            selected(&store, "default", None, 100, true).unwrap().len(),
            1
        );
        assert!(acknowledge(&store, "default", &newer).unwrap());
        assert_eq!(pending_count(&store, "default", None).unwrap(), 0);
    }
    #[test]
    fn shared_enqueue_rolls_back_and_coalesces_on_the_producer_connection() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("catalog.db");
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        store
            .transaction(|db| {
                db.execute(
                    "INSERT INTO catalog_refresh_settings VALUES('default','global',1,0)",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let failed = store.transaction(|db| {
            enqueue(db, "default", None)?;
            enqueue(db, "default", None)?;
            assert_eq!(
                pending(&store, "default", 10)?["pending"][0]["generation"],
                2
            );
            Err::<(), _>(Error("producer rolled back".into()))
        });
        assert!(failed.is_err());
        assert_eq!(pending_count(&store, "default", None).unwrap(), 0);
        store
            .transaction(|db| {
                enqueue(db, "default", None)?;
                enqueue(db, "default", None)?;
                enqueue(db, "other", None)?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            pending(&store, "default", 10).unwrap()["pending"][0]["generation"],
            2
        );
        assert_eq!(pending_count(&store, "other", None).unwrap(), 0);
    }

    #[test]
    fn public_integer_coercion_does_not_relax_companion_boolean_or_schema_constraints() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("catalog.db");
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        assert_eq!(
            configure(&store, "default", &json!({"catalog":"global","enabled":2}))
                .unwrap_err()
                .0,
            "enabled must be boolean"
        );
        // False truthiness avoids any layout dependency; stored values still
        // pass through the original schema constraint in the shared owner.
        assert!(
            configure_integer(
                &store,
                "default",
                &json!({"catalog":"global","enabled":false}),
                2
            )
            .unwrap_err()
            .0
            .contains("CHECK constraint failed")
        );
        assert_eq!(
            store
                .db
                .query_row("SELECT count(*) FROM catalog_refresh_settings", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        assert_eq!(
            configure_integer(
                &store,
                "default",
                &json!({"catalog":"global","enabled":false}),
                0
            )
            .unwrap()["enabled"],
            false
        );
    }
}
