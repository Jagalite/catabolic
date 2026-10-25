// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Request-owned authorized views, principal-bound cursors and REST presentation.
use crate::{Error, Result, Store, access::Access, content_access, migration::rows};
use base64::{Engine, engine::general_purpose::URL_SAFE};
use catabolic_core::{decode_json, encode};
use hmac::{Hmac, Mac};
use rusqlite::{functions::FunctionFlags, params};
use serde_json::{Value, json};
use sha2::Sha256;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone)]
pub struct Authority {
    profile: String,
    database_id: String,
    marker: String,
    fingerprint: String,
    key: Vec<u8>,
    grants: Vec<Value>,
    epoch: i64,
}
impl Authority {
    pub fn profile(&self) -> &str {
        &self.profile
    }
    fn matching<'a>(&'a self, action: &'a str) -> impl Iterator<Item = &'a Value> {
        self.grants.iter().filter(move |g| {
            g["actions"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v == action || v == "*"))
        })
    }
    pub fn operator(&self, action: &str) -> bool {
        self.matching(action).any(|g| g["operator"] == true)
    }
    pub fn require(&self, action: &str) -> Result<()> {
        if self.matching(action).next().is_none() {
            Err(Error("forbidden".into()))
        } else {
            Ok(())
        }
    }
    fn permits(&self, key: &str, id: &str) -> bool {
        self.matching("metadata:read").any(|g| {
            g["operator"] == true || g[key].as_array().is_some_and(|a| a.iter().any(|v| v == id))
        })
    }
    pub fn permits_fallback_policy(&self, id: &str) -> bool {
        self.permits("fallback_policy_ids", id)
    }
    pub fn permits_catalog(&self, id: &str) -> bool {
        self.permits("projection_ids", id)
    }
    pub fn permits_report(&self, id: &str) -> bool {
        self.matching("metadata:read").any(|g| {
            g["report_ids"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v == id))
        })
    }
    pub fn check_store(&self, store: &Store) -> Result<()> {
        if store.database_id != self.database_id
            || store.db.query_row(
                "SELECT generation FROM fallback_epoch WHERE id=1",
                [],
                |r| r.get::<_, i64>(0),
            )? != self.epoch
        {
            return Err(Error("authorization_snapshot_mismatch".into()));
        }
        let valid=store.db.prepare("SELECT 1 FROM temp.api_authorized_snapshot WHERE marker=? AND profile=? AND fingerprint=?").and_then(|mut s|s.exists(params![self.marker,self.profile,self.fingerprint])).unwrap_or(false);
        if valid {
            Ok(())
        } else {
            Err(Error("authorization_snapshot_mismatch".into()))
        }
    }
    pub fn cursor(&self, scope: &Value, after: &str) -> Result<String> {
        authenticated_cursor(&self.key, &self.fingerprint, scope, after)
    }
    pub fn after(&self, scope: &Value, cursor: Option<&str>) -> Result<String> {
        authenticated_cursor_after(&self.key, &self.fingerprint, scope, cursor)
    }
}
/// Shared authenticated cursor codec. Keep this exact wire representation for
/// existing API/GraphQL clients; callers supply independently trusted binding
/// and secret material. A cursor is a continuation token, never an authority.
pub(crate) fn authenticated_cursor(
    key: &[u8],
    fingerprint: &str,
    scope: &Value,
    after: &str,
) -> Result<String> {
    let body = encode(&json!([fingerprint, scope, after]));
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key).map_err(|_| Error("invalid_cursor".into()))?;
    mac.update(body.as_bytes());
    let mut raw = mac.finalize().into_bytes().to_vec();
    raw.extend(body.as_bytes());
    Ok(URL_SAFE.encode(raw))
}
pub(crate) fn authenticated_cursor_after(
    key: &[u8],
    fingerprint: &str,
    scope: &Value,
    cursor: Option<&str>,
) -> Result<String> {
    let Some(cursor) = cursor.filter(|s| !s.is_empty()) else {
        return Ok(String::new());
    };
    let invalid = || Error("invalid_cursor".into());
    if cursor.len() > 4096 {
        return Err(invalid());
    }
    let raw = URL_SAFE.decode(cursor).map_err(|_| invalid())?;
    if raw.len() < 32 {
        return Err(invalid());
    }
    let (sig, body) = raw.split_at(32);
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| invalid())?;
    mac.update(body);
    mac.verify_slice(sig).map_err(|_| invalid())?;
    let value = catabolic_core::decode_json_bytes(body).map_err(|_| invalid())?;
    if value.as_array().is_none_or(|a| a.len() != 3)
        || value[0] != fingerprint
        || value[1] != *scope
    {
        return Err(invalid());
    }
    value[2].as_str().map(str::to_owned).ok_or_else(invalid)
}

fn strings(v: Option<&Value>) -> Result<Vec<String>> {
    match v {
        None => Ok(vec![]),
        Some(v) => v
            .as_array()
            .ok_or_else(|| Error("unauthorized".into()))?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| Error("unauthorized".into()))
            })
            .collect(),
    }
}
/// Authority may only be consumed with this same owned SQLite snapshot.
pub fn install(access: &mut Access) -> Result<Authority> {
    let mut items = BTreeSet::new();
    let mut files = BTreeSet::new();
    let mut fields: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let operator = access.operator("metadata:read");
    for grant in access.matching("metadata:read") {
        if grant["operator"] == true {
            for r in rows(
                &access.store().db,
                "SELECT id FROM main.items LIMIT 100001",
                &[],
            )? {
                items.insert(r["id"].as_str().unwrap().to_owned());
            }
            for r in rows(
                &access.store().db,
                "SELECT id FROM main.files LIMIT 100001",
                &[],
            )? {
                files.insert(r["id"].as_str().unwrap().to_owned());
            }
        } else {
            let members = access.members(&grant)?.clone();
            items.extend(members.iter().cloned());
            files.extend(strings(grant.get("file_ids"))?);
            let wanted = strings(Some(
                grant
                    .get("metadata_fields")
                    .unwrap_or(&json!(["title", "year"])),
            ))?;
            for item in members {
                fields
                    .entry(item.clone())
                    .or_default()
                    .extend(wanted.iter().cloned());
                if grant["derivatives"] == true {
                    for r in rows(
                        &access.store().db,
                        "SELECT file_id FROM main.media_outputs WHERE source_item_id=? AND profile=?",
                        &[item.into(), access.profile().to_owned().into()],
                    )? {
                        files.insert(r["file_id"].as_str().unwrap().to_owned());
                    }
                }
            }
        }
    }
    if items.len() > 100000 || files.len() > 100000 {
        return Err(Error("authorization_scope_too_large".into()));
    }
    let mut admitted = Vec::new();
    for id in files {
        if access.file(&id, "metadata:read", None)? {
            admitted.push(id);
        }
    }
    let mut projections = BTreeSet::new();
    for g in access.matching("metadata:read") {
        projections.extend(strings(g.get("projection_ids"))?)
    }
    if operator {
        for r in rows(
            &access.store().db,
            "SELECT id FROM main.catalogs LIMIT 100001",
            &[],
        )? {
            projections.insert(r["id"].as_str().unwrap().to_owned());
        }
    }
    let mut components = BTreeSet::new();
    for g in access.matching("component:read") {
        components.extend(strings(g.get("occurrence_ids"))?)
    }
    if components.len() > 100000 {
        return Err(Error("authorization_scope_too_large".into()));
    }
    let authority = Authority {
        profile: access.profile().into(),
        database_id: access.store().database_id.clone(),
        marker: uuid::Uuid::new_v4().to_string(),
        fingerprint: access.fingerprint(),
        key: access.cursor_key().to_vec(),
        grants: access.grants().to_vec(),
        epoch: access.store().db.query_row(
            "SELECT generation FROM fallback_epoch WHERE id=1",
            [],
            |r| r.get(0),
        )?,
    };
    let db = &access.store().db;
    crate::sql::setup(db)?;
    db.pragma_update(None, "query_only", false)?;
    db.execute_batch("CREATE TEMP TABLE api_visible_items(id TEXT PRIMARY KEY);CREATE TEMP TABLE api_visible_files(id TEXT PRIMARY KEY);CREATE TEMP TABLE api_visible_catalogs(id TEXT PRIMARY KEY);CREATE TEMP TABLE api_visible_components(id TEXT PRIMARY KEY);CREATE TEMP TABLE api_profile(id TEXT PRIMARY KEY);CREATE TEMP TABLE api_authorized_snapshot(marker TEXT,profile TEXT,fingerprint TEXT);")?;
    for (table, ids) in [
        ("api_visible_items", items.into_iter().collect::<Vec<_>>()),
        ("api_visible_files", admitted),
        ("api_visible_catalogs", projections.into_iter().collect()),
        ("api_visible_components", components.into_iter().collect()),
    ] {
        let mut s = db.prepare(&format!("INSERT INTO {table} VALUES (?)"))?;
        for id in ids {
            s.execute([id])?;
        }
    }
    db.execute("INSERT INTO api_profile VALUES (?)", [access.profile()])?;
    db.execute(
        "INSERT INTO api_authorized_snapshot VALUES (?,?,?)",
        params![authority.marker, authority.profile, authority.fingerprint],
    )?;
    db.create_scalar_function("api_metadata", 2, FunctionFlags::SQLITE_UTF8, move |ctx| {
        let id: String = ctx.get(0)?;
        let raw: String = ctx.get(1)?;
        let v = decode_json(&raw).map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))?;
        if operator {
            return Ok(encode(&v));
        }
        let mut out = serde_json::Map::new();
        if let Some(keys) = fields.get(&id) {
            for k in keys {
                if let Some(v) = v.get(k) {
                    out.insert(k.clone(), v.clone());
                }
            }
        }
        Ok(encode(&Value::Object(out)))
    })?;
    db.execute_batch(&format!("CREATE TEMP VIEW items AS SELECT id,kind,api_metadata(id,metadata) AS metadata FROM main.items WHERE id IN (SELECT id FROM api_visible_items);
CREATE TEMP VIEW files AS SELECT id,{} FROM main.files WHERE id IN (SELECT id FROM api_visible_files);
CREATE TEMP VIEW item_files AS SELECT * FROM main.item_files WHERE file_id IN (SELECT id FROM api_visible_files) AND item_id IN (SELECT id FROM api_visible_items);
CREATE TEMP VIEW item_relationships AS SELECT * FROM main.item_relationships WHERE source_id IN (SELECT id FROM api_visible_items) AND target_id IN (SELECT id FROM api_visible_items);
CREATE TEMP VIEW media_outputs AS SELECT * FROM main.media_outputs WHERE profile IN (SELECT id FROM api_profile) AND file_id IN (SELECT id FROM api_visible_files);
CREATE TEMP VIEW identities AS SELECT * FROM main.identities WHERE item_id IN (SELECT id FROM api_visible_items);
CREATE TEMP VIEW catalogs AS SELECT * FROM main.catalogs WHERE id IN (SELECT id FROM api_visible_catalogs);
CREATE TEMP VIEW mappings AS SELECT id,catalog,file_id,item_id,path,active FROM (SELECT m.* FROM main.mappings m WHERE NOT EXISTS (SELECT 1 FROM main.fallback_bindings b WHERE b.catalog=m.catalog AND b.profile IN (SELECT id FROM api_profile)) UNION ALL SELECT id,catalog,file_id,item_id,path,active FROM main.fallback_entries WHERE profile IN (SELECT id FROM api_profile) AND path IS NOT NULL) WHERE catalog IN (SELECT id FROM api_visible_catalogs) AND item_id IN (SELECT id FROM api_visible_items) AND file_id IN (SELECT id FROM api_visible_files);
DROP VIEW catalog_component_occurrences;CREATE TEMP VIEW catalog_component_occurrences AS SELECT c.* FROM ({}) c WHERE c.profile IN (SELECT id FROM api_profile) AND {};
DROP VIEW catalog_components;CREATE TEMP VIEW catalog_components AS SELECT component_id,kind,profile,json_group_array(DISTINCT item_id) AS item_ids FROM catalog_component_occurrences GROUP BY component_id,kind,profile;",if operator{"location,path"}else{"'' AS location,'' AS path"},crate::query::view_sql_for(access.store(), "catalog_component_occurrences"),if operator{"1"}else{"c.occurrence_id IN (SELECT id FROM api_visible_components)"}))?;
    Ok(authority)
}
fn page_limit(n: usize) -> Result<()> {
    if (1..=1000).contains(&n) {
        Ok(())
    } else {
        Err(Error("invalid_page".into()))
    }
}
pub fn present(a: &Access, resource: &str, row: &Value) -> Result<Value> {
    if resource == "items" {
        let mut r = row.clone();
        r["metadata"] = decode_json(
            r["metadata"]
                .as_str()
                .ok_or_else(|| Error("catalog_error".into()))?,
        )?;
        return Ok(r);
    }
    let id = row[if resource == "files" { "id" } else { "file_id" }]
        .as_str()
        .ok_or_else(|| Error("catalog_error".into()))?;
    let rev = content_access::revision_of(a.store(), a.profile(), id)?;
    if resource == "renditions" {
        return Ok(
            json!({"id":row["id"],"file_id":id,"definition_id":row["definition_id"],"origin":row["origin"],"revision":rev}),
        );
    }
    let mut result = json!({"id":id,"revision":rev});
    if let Some(r) = rows(
        &a.store().db,
        "SELECT size,mtime_ns,status FROM observations WHERE profile=? AND file_id=?",
        &[a.profile().to_owned().into(), id.to_owned().into()],
    )?
    .first()
    {
        for k in ["size", "mtime_ns", "status"] {
            result[k] = if k == "status" {
                r[k].clone()
            } else {
                json!(if r[k].is_null() {
                    "None".into()
                } else {
                    r[k].to_string()
                })
            };
        }
    }
    if a.operator("metadata:read") {
        result["location"] = row["location"].clone();
        result["path"] = row["path"].clone()
    }
    Ok(result)
}
fn table(resource: &str) -> Result<&'static str> {
    match resource {
        "items" => Ok("items"),
        "files" => Ok("files"),
        "renditions" => Ok("media_outputs"),
        _ => Err(Error("not_found".into())),
    }
}
fn finish(
    a: &Access,
    auth: &Authority,
    resource: &str,
    scope: &Value,
    mut values: Vec<Value>,
    limit: usize,
    selection: bool,
) -> Result<Value> {
    let more = values.len() > limit;
    values.truncate(limit);
    let next = if more {
        json!(auth.cursor(scope, values.last().unwrap()["id"].as_str().unwrap())?)
    } else {
        Value::Null
    };
    let data = if resource.is_empty() {
        values
    } else {
        values
            .iter()
            .map(|v| present(a, resource, v))
            .collect::<Result<Vec<_>>>()?
    };
    let mut out = json!({"data":data,"page_complete":true,"has_more":more,"next_cursor":next});
    if selection {
        out["selection_complete"] = json!(!more)
    }
    Ok(out)
}
pub fn listing(
    a: &Access,
    auth: &Authority,
    resource: &str,
    limit: usize,
    cursor: Option<&str>,
) -> Result<Value> {
    auth.check_store(a.store())?;
    a.require("metadata:read")?;
    page_limit(limit)?;
    let scope = json!([resource, null]);
    let after = auth.after(&scope, cursor)?;
    let values = rows(
        &a.store().db,
        &format!(
            "SELECT * FROM {} WHERE id>? ORDER BY id LIMIT ?",
            table(resource)?
        ),
        &[after.into(), ((limit + 1) as i64).into()],
    )?;
    finish(a, auth, resource, &scope, values, limit, true)
}
pub fn get(a: &Access, auth: &Authority, resource: &str, id: &str) -> Result<Value> {
    auth.check_store(a.store())?;
    let r = rows(
        &a.store().db,
        &format!("SELECT * FROM {} WHERE id=?", table(resource)?),
        &[id.to_owned().into()],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error("not_found".into()))?;
    present(a, resource, &r)
}
pub fn snapshot(a: &Access, auth: &Authority) -> Result<Value> {
    auth.check_store(a.store())?;
    a.require("metadata:read")?;
    let ids = rows(
        &a.store().db,
        "SELECT id FROM items ORDER BY id LIMIT 10001",
        &[],
    )?
    .into_iter()
    .map(|r| r["id"].clone())
    .collect::<Vec<_>>();
    if ids.len() > 10000 {
        return Err(Error("snapshot_limit".into()));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let expires = crate::notifications::now() + 900.;
    a.store().transaction(|db| {
        db.execute(
            "INSERT INTO api_snapshots VALUES (?,?,?,?,?)",
            params![id, a.principal(), "items", encode(&json!(ids)), expires],
        )?;
        Ok(())
    })?;
    Ok(json!({"snapshot_id":id,"expires":expires,"selection_complete":true}))
}
pub fn snapshot_page(
    a: &Access,
    auth: &Authority,
    id: &str,
    limit: usize,
    cursor: Option<&str>,
) -> Result<Value> {
    auth.check_store(a.store())?;
    page_limit(limit)?;
    let saved = rows(
        &a.store().db,
        "SELECT ids FROM api_snapshots WHERE id=? AND principal=? AND scope='items' AND expires>?",
        &[
            id.to_owned().into(),
            a.principal().to_owned().into(),
            crate::notifications::now().into(),
        ],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error("not_found".into()))?;
    let scope = json!(["snapshot", id]);
    let after = auth.after(&scope, cursor)?;
    let values = rows(
        &a.store().db,
        "SELECT * FROM items WHERE id>? AND id IN (SELECT value FROM json_each(?)) ORDER BY id LIMIT ?",
        &[
            after.into(),
            saved["ids"].as_str().unwrap().to_owned().into(),
            ((limit + 1) as i64).into(),
        ],
    )?;
    finish(a, auth, "items", &scope, values, limit, true)
}
pub fn projection(
    a: &Access,
    auth: &Authority,
    id: &str,
    limit: usize,
    cursor: Option<&str>,
    resolutions: bool,
) -> Result<Value> {
    auth.check_store(a.store())?;
    page_limit(limit)?;
    if !auth.permits_catalog(id)
        || (!resolutions
            && !a
                .store()
                .db
                .prepare("SELECT 1 FROM catalogs WHERE id=?")?
                .exists([id])?)
    {
        return Err(Error("not_found".into()));
    }
    let scope = json!([
        if resolutions {
            "fallback"
        } else {
            "projection"
        },
        id
    ]);
    let after = auth.after(&scope, cursor)?;
    let values = if resolutions {
        rows(
            &a.store().db,
            "SELECT e.id,e.item_id,e.file_id,e.revision,e.policy_id,e.state,e.tier,e.generation,e.published_generation FROM fallback_entries e WHERE e.profile=? AND e.catalog=? AND e.active=1 AND e.id>? AND e.item_id IN (SELECT id FROM items) AND (e.file_id IS NULL OR e.file_id IN (SELECT id FROM files)) ORDER BY e.id LIMIT ?",
            &[
                a.profile().to_owned().into(),
                id.to_owned().into(),
                after.into(),
                ((limit + 1) as i64).into(),
            ],
        )?
    } else {
        rows(
            &a.store().db,
            "SELECT id,item_id,file_id,path FROM mappings WHERE catalog=? AND active=1 AND id>? ORDER BY id LIMIT ?",
            &[
                id.to_owned().into(),
                after.into(),
                ((limit + 1) as i64).into(),
            ],
        )?
    };
    finish(a, auth, "", &scope, values, limit, false)
}

pub fn identity(access: &Access) -> Result<Value> {
    let actions = access
        .grants()
        .iter()
        .flat_map(|g| g["actions"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    let expires: f64 = access.store().db.query_row(
        "SELECT expires FROM api_tokens WHERE id=?",
        [access.token_id()],
        |r| r.get(0),
    )?;
    Ok(
        json!({"principal":access.principal(),"profile":access.profile(),"actions":actions,"expires":expires}),
    )
}
pub fn capabilities(
    access: &Access,
    read_only: bool,
    streams: usize,
    per_principal: usize,
    rate: usize,
) -> Result<Value> {
    let worker = rows(
        &access.store().db,
        "SELECT worker_pid,updated_at,capabilities FROM api_worker_status WHERE profile=?",
        &[access.profile().to_owned().into()],
    )?
    .into_iter()
    .next();
    let ready = worker.as_ref().is_some_and(|r| {
        r["worker_pid"]
            .as_i64()
            .and_then(|p| i32::try_from(p).ok())
            .is_some_and(|p| {
                // SAFETY: signal zero checks process existence without sending a signal.
                unsafe { libc::kill(p, 0) == 0 }
            })
            && r["updated_at"]
                .as_f64()
                .is_some_and(|t| crate::notifications::now() - t <= 30.)
    });
    let mut operations = vec![];
    for r in rows(
        &access.store().db,
        "SELECT id FROM api_operations WHERE profile=? AND enabled=1",
        &[access.profile().to_owned().into()],
    )? {
        if access.matching("processing:request").iter().any(|g| {
            g["operator"] == true
                || g["operation_ids"]
                    .as_array()
                    .is_some_and(|a| a.contains(&r["id"]))
        }) {
            operations.push(r["id"].clone());
        }
    }
    let mut profiles = vec![];
    for r in rows(
        &access.store().db,
        "SELECT o.id FROM api_operations o JOIN processing_recipes r ON r.id=o.recipe_id WHERE o.profile=? AND o.enabled=1 AND json_extract(r.definition,'$.preset') IN ('h264-720p','h264-1080p')",
        &[access.profile().to_owned().into()],
    )? {
        if operations.contains(&r["id"]) {
            profiles.push(r["id"].clone())
        }
    }
    let permission = !access.matching("processing:request").is_empty();
    let hls = worker
        .as_ref()
        .and_then(|r| r["capabilities"].as_str())
        .and_then(|s| decode_json(s).ok())
        .is_some_and(|v| v["playback_hls"] == true);
    Ok(
        json!({"version":1,"schema":access.store().schema_version,"read_only":read_only,"processing_admission":!read_only&&ready&&permission,"worker_ready":ready,"operations":operations,"playback":{"admission":!read_only&&ready&&permission&&hls,"profiles":profiles,"segment_seconds":2,"seeking":"produced_segments"},"limits":{"page_size":1000,"streams":streams,"streams_per_principal":per_principal,"stream_buffer_bytes":65536,"requests_per_minute":rate},"content":{"ranges":"single","etag":"weak_revision","in_place_mutation":"abort_remaining_transfer"}}),
    )
}

#[cfg(test)]
mod cursor_codec_tests {
    use super::*;
    #[test]
    fn extracted_codec_preserves_legacy_wire_bytes_and_empty_start() {
        let scope = json!({"resource":"items","limit":1});
        let token = authenticated_cursor(b"key", "fp", &scope, "a").unwrap();
        assert_eq!(
            token,
            "cdCTWivDsZ5NkcUoPZnYH0ZMrOs4V_86zv5W7syvr9JbImZwIiwgeyJsaW1pdCI6IDEsICJyZXNvdXJjZSI6ICJpdGVtcyJ9LCAiYSJd"
        );
        assert_eq!(
            authenticated_cursor_after(b"key", "fp", &scope, Some(&token)).unwrap(),
            "a"
        );
        assert_eq!(
            authenticated_cursor_after(b"key", "fp", &scope, None).unwrap(),
            ""
        );
        assert_eq!(
            authenticated_cursor_after(b"key", "fp", &scope, Some("")).unwrap(),
            ""
        );
        assert!(authenticated_cursor_after(b"other", "fp", &scope, Some(&token)).is_err());
        assert!(authenticated_cursor_after(b"key", "other", &scope, Some(&token)).is_err());
        assert!(
            authenticated_cursor_after(
                b"key",
                "fp",
                &json!({"resource":"items","limit":2}),
                Some(&token)
            )
            .is_err()
        );
    }
}
