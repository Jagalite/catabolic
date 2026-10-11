// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Preserved Jellyfin refresh configuration and durable delivery records.
//! Output publication remains owned by filesystem_mutation; this module never
//! creates dirty records, changes links, or treats request acceptance as indexing.
use crate::{Error, Result, Store, mutation, notifications, public_reads::bounded_rows};
use rusqlite::params;
use serde_json::{Value, json};

pub const DELIVERY_ERROR: &str =
    "legacy refresh delivery failed; inspect endpoint and credential reference";

pub fn limit(request: &Value) -> Result<usize> {
    request
        .get("limit")
        .map_or(Some(100), Value::as_u64)
        .filter(|n| (1..=1000).contains(n))
        .map(|n| n as usize)
        .ok_or_else(|| Error("limit must be between 1 and 1000".into()))
}
fn integer(request: &Value, key: &str, default: i64) -> Result<i64> {
    request.get(key).map_or(Ok(default), |value| {
        value
            .as_i64()
            .ok_or_else(|| Error("Python int too large to convert to SQLite INTEGER".into()))
    })
}
pub fn configure(store: &Store, profile: &str, request: &Value) -> Result<Value> {
    mutation::profile(store, profile)?;
    let application = request["application"].as_str().unwrap_or("jellyfin");
    if application != "jellyfin" {
        return Err(Error("the first refresh adapter supports jellyfin".into()));
    }
    let endpoint = request["endpoint"].as_str().unwrap_or("");
    if endpoint.len() > 4096 {
        return Err(Error(
            "endpoint must be nonempty text of at most 4096 bytes".into(),
        ));
    }
    // Deliberate, documented stricter URL admission uses the established native
    // credential destination validator, rather than creating a second parser.
    let endpoint =
        crate::consumers::endpoint(endpoint).map_err(|_| {
            Error(
        "endpoint must be an HTTP(S) server base URL without credentials, query or fragment".into())
        })?;
    let credential = request["credential_env"]
        .as_str()
        .unwrap_or("JELLYFIN_TOKEN");
    notifications::credential_reference(credential)
        .map_err(|_| Error("invalid credential environment variable name".into()))?;
    let catalog = request["catalog"].as_str().unwrap_or("global");
    if !store
        .db
        .prepare("SELECT id FROM catalogs WHERE id=?")?
        .exists([catalog])?
    {
        return Err(Error("unknown catalog".into()));
    }
    store.transaction(|db| {
        db.execute("INSERT INTO refresh_targets VALUES (?,?,?,?,?) ON CONFLICT(profile,catalog) DO UPDATE SET application=excluded.application,endpoint=excluded.endpoint,credential_env=excluded.credential_env",
            params![profile,catalog,application,endpoint,credential])?;
        Ok(())
    })?;
    Ok(
        json!({"catalog":catalog,"application":application,"endpoint":endpoint,"credential_env":credential}),
    )
}
pub fn list(store: &Store, profile: &str, request: &Value) -> Result<Value> {
    mutation::profile(store, profile)?;
    let n = limit(request)?;
    let after = integer(request, "after", 0)?;
    let mut events = bounded_rows(
        store,
        "SELECT * FROM refresh_events WHERE profile=? AND id>? ORDER BY id LIMIT ?",
        &[
            profile.to_owned().into(),
            after.into(),
            ((n + 1) as i64).into(),
        ],
    )?;
    let next = if events.len() > n {
        events[n - 1]["id"].clone()
    } else {
        Value::Null
    };
    events.truncate(n);
    for event in &mut events {
        crate::query::decode(event, &["target"])?;
    }
    Ok(json!({"events":events,"next_after":next}))
}
pub fn retry(store: &Store, profile: &str, request: &Value) -> Result<Value> {
    mutation::profile(store, profile)?;
    let id = integer(request, "id", 0)?;
    // Frozen retry intentionally clears a live lease, resets attempts, and
    // preserves finished_at. A displaced acknowledgment is fenced by its token.
    let count = store.transaction(|db| Ok(db.execute(
        "UPDATE refresh_events SET attempts=0,state='queued',error=NULL,lease_token=NULL,lease_until=NULL WHERE id=? AND profile=? AND state!='complete'",
        params![id,profile])?))?;
    if count == 0 {
        return Err(Error("unknown or completed refresh event".into()));
    }
    Ok(json!({"queued":id}))
}
pub fn next(store: &Store, profile: &str, now: f64) -> Result<Option<Value>> {
    let mut rows = bounded_rows(
        store,
        "SELECT * FROM refresh_events WHERE profile=? AND state!='complete' AND attempts<3 AND (lease_until IS NULL OR lease_until<=?) ORDER BY id LIMIT 1",
        &[profile.to_owned().into(), now.into()],
    )?;
    let Some(mut row) = rows.pop() else {
        return Ok(None);
    };
    crate::query::decode(&mut row, &["target"])?;
    Ok(Some(row))
}
pub fn lease(store: &Store, profile: &str, id: i64, token: &str, now: f64) -> Result<bool> {
    store.transaction(|db| Ok(db.execute(
        "UPDATE refresh_events SET attempts=attempts+1,lease_token=?,lease_until=? WHERE id=? AND profile=? AND state!='complete' AND attempts<3 AND (lease_until IS NULL OR lease_until<=?)",
        params![token,now+60.0,id,profile,now])? == 1))
}
pub fn acknowledge(
    store: &Store,
    profile: &str,
    id: i64,
    token: &str,
    delivered: bool,
) -> Result<()> {
    store.transaction(|db| {
        db.execute("UPDATE refresh_events SET state=?,error=?,finished_at=CURRENT_TIMESTAMP,lease_token=NULL,lease_until=NULL WHERE id=? AND profile=? AND lease_token=?",
            params![if delivered {"complete"} else {"failed"}, if delivered {None} else {Some(DELIVERY_ERROR)}, id,profile,token])?;
        Ok(())
    })
}
pub fn unresolved(store: &Store, profile: &str) -> Result<i64> {
    Ok(store.db.query_row(
        "SELECT count(*) FROM refresh_events WHERE profile=? AND state!='complete'",
        [profile],
        |row| row.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("refresh.sqlite3");
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        (root, store)
    }
    fn seed(store: &Store) {
        store.transaction(|db| {
            db.execute("INSERT INTO refresh_events(id,profile,catalog,target) VALUES(1,'default','global','{}')", [])?;
            Ok(())
        }).unwrap();
    }
    #[test]
    fn leased_attempts_and_displaced_acknowledgment_are_durable() {
        let (_root, store) = fixture();
        seed(&store);
        assert!(lease(&store, "default", 1, "first", 10.0).unwrap());
        assert!(next(&store, "default", 69.0).unwrap().is_none());
        assert!(next(&store, "default", 70.0).unwrap().is_some());
        assert!(!lease(&store, "default", 1, "second", 69.0).unwrap());
        assert!(lease(&store, "default", 1, "second", 70.0).unwrap());
        acknowledge(&store, "default", 1, "first", true).unwrap();
        let row = &list(&store, "default", &json!({})).unwrap()["events"][0];
        assert_eq!(row["state"], "queued");
        assert_eq!(row["attempts"], 2);
        assert_eq!(row["lease_token"], "second");
        acknowledge(&store, "default", 1, "second", false).unwrap();
        assert!(lease(&store, "default", 1, "third", 80.0).unwrap());
        acknowledge(&store, "default", 1, "third", false).unwrap();
        assert!(next(&store, "default", 1000.0).unwrap().is_none());
        assert_eq!(unresolved(&store, "default").unwrap(), 1);
        retry(&store, "default", &json!({"id":1})).unwrap();
        let row = &list(&store, "default", &json!({})).unwrap()["events"][0];
        assert_eq!(row["attempts"], 0);
        assert!(!row["finished_at"].is_null());
        assert!(row["error"].is_null());
        assert!(lease(&store, "default", 1, "last", 1000.0).unwrap());
        acknowledge(&store, "default", 1, "last", true).unwrap();
        assert_eq!(unresolved(&store, "default").unwrap(), 0);
        assert!(retry(&store, "default", &json!({"id":1})).is_err());
    }
    #[test]
    fn profile_fence_and_readonly_refusal_do_not_mutate() {
        let (_root, store) = fixture();
        seed(&store);
        store
            .transaction(|db| {
                db.execute("INSERT INTO profiles(id) VALUES('other')", [])?;
                Ok(())
            })
            .unwrap();
        assert!(!lease(&store, "other", 1, "wrong", 0.0).unwrap());
        assert!(retry(&store, "other", &json!({"id":1})).is_err());
        assert!(list(&store, "missing", &json!({})).is_err());
        let readonly = Store::open(&store.path, false, false).unwrap();
        assert!(retry(&readonly, "default", &json!({"id":1})).is_err());
        assert_eq!(
            list(&store, "default", &json!({})).unwrap()["events"][0]["attempts"],
            0
        );
    }
    #[test]
    fn unbounded_rows_refuse_without_claiming_completion() {
        let (_root, store) = fixture();
        seed(&store);
        store
            .transaction(|db| {
                db.execute(
                    "UPDATE refresh_events SET target=?",
                    ["x".repeat(4 * 1024 * 1024 + 1)],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            list(&store, "default", &json!({})).unwrap_err().0,
            "result exceeds the result budget"
        );
        assert_eq!(
            next(&store, "default", 0.0).unwrap_err().0,
            "result exceeds the result budget"
        );
    }
}
