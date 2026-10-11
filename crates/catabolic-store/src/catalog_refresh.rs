// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Durable opt-in catalog-refresh configuration, generation fences and retries.
use crate::{Error, Result, Store, migration::rows};
use serde_json::{Value, json};

pub fn configure(store: &Store, profile: &str, request: &Value) -> Result<Value> {
    crate::mutation::require_recovered(&store.db)?;
    let catalog = request["catalog"]
        .as_str()
        .ok_or_else(|| Error("unknown catalog".into()))?;
    let enabled = request["enabled"]
        .as_bool()
        .ok_or_else(|| Error("enabled must be boolean".into()))?;
    let removals = request
        .get("max_removals")
        .map(|v| {
            v.as_i64()
                .filter(|v| *v >= 0)
                .ok_or_else(|| Error("max removals must be a nonnegative integer".into()))
        })
        .transpose()?
        .unwrap_or(0);
    if !store
        .db
        .prepare("SELECT 1 FROM catalogs WHERE id=?")?
        .exists([catalog])?
    {
        return Err(Error("unknown catalog".into()));
    }
    if enabled {
        let fallback = crate::fallback_projection::binding(store, profile, catalog)?.is_some();
        if !fallback
            && !store
                .db
                .prepare("SELECT 1 FROM meta WHERE key=?")?
                .exists([format!("layout-catalog:{catalog}")])?
        {
            return Err(Error(
                "apply a saved layout to the catalog before enabling automatic refresh".into(),
            ));
        }
    }
    store.transaction(|db| {
                db.execute("INSERT INTO catalog_refresh_settings VALUES(?,?,?,?) ON CONFLICT(profile,catalog) DO UPDATE SET enabled=excluded.enabled,max_removals=excluded.max_removals",(profile,catalog,enabled,removals))?;
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
}
