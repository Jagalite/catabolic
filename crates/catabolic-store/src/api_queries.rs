// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Approved HTTP queries retain the request-owned snapshot and disclosure mode.
use crate::{Error, Result, access::Access, api_catalog, definitions, migration::rows, sql};
use catabolic_core::encode;
use serde_json::{Value, json};
fn rows_query(access: &Access, value: &Value, limit: usize) -> Result<Value> {
    let parameters = encode(value.get("params").unwrap_or(&json!({})));
    sql::execute_store(
        access.store(),
        value["query"].as_str(),
        &sql::Options {
            profile: access.profile(),
            params: Some(&parameters),
            max_rows: limit,
            http: true,
            ..Default::default()
        },
    )
}
pub fn sql(access: Access, body: &Value) -> Result<Value> {
    access.require_operator("sql:read")?;
    rows_query(
        &access,
        body,
        body["limit"].as_u64().unwrap_or(100) as usize,
    )
}
pub fn saved(mut access: Access, id: &str, limit: usize, cursor: Option<&str>) -> Result<Value> {
    access.require("metadata:read")?;
    if !(1..=1000).contains(&limit) {
        return Err(Error("invalid_page".into()));
    }
    let approved = access.operator("sql:read")
        || access.matching("metadata:read").iter().any(|g| {
            g["report_ids"]
                .as_array()
                .is_some_and(|v| v.iter().any(|v| v == id))
        });
    let allowed = approved
        || access.operator("metadata:read")
        || access
            .matching("metadata:read")
            .iter()
            .any(|g| g["query_id"] == id);
    if !allowed {
        return Err(Error("not_found".into()));
    }
    let profile = access.profile().to_owned();
    let query = definitions::query_get(access.store(), &profile, id)?;
    let definition = &query["definition"];
    if definition["mode"] == "rows" {
        if !approved {
            return Err(Error("forbidden".into()));
        }
        return rows_query(&access, &definition["selection"], limit);
    }
    if definition["mode"] == "document" {
        let authority = api_catalog::install(&mut access)?;
        let value = &definition["selection"];
        return crate::graphql::execute_authorized_store(
            access.into_store(),
            authority,
            value["query"].as_str().unwrap_or(""),
            value.get("variables").cloned().unwrap_or_else(|| json!({})),
            None,
            5000,
        )
        .map(|(_, v)| v);
    }
    // Only this Access-created immutable selection proof permits global ID
    // evaluation. No global fields, rows or counts are returned: install the
    // current metadata authority and intersect IDs before pagination below.
    let selected = access.approved_selection(id)?;
    if selected.report["complete"] != true {
        return Err(Error("selection_incomplete".into()));
    }
    let authority = api_catalog::install(&mut access)?;
    let store = access.into_store();
    let table = match selected.entity.as_str() {
        "item_id" => "items",
        "file_id" => "files",
        "association_id" => "item_files",
        _ => return Err(Error("selection_incomplete".into())),
    };
    let scope = json!(["saved", id]);
    let after = authority.after(&scope, cursor)?;
    let ids = encode(&json!(selected.ids));
    let mut allowed = rows(
        &store.db,
        &format!(
            "SELECT id FROM {table} WHERE id IN (SELECT value FROM json_each(?)) AND id>? ORDER BY id LIMIT ?"
        ),
        &[ids.into(), after.into(), ((limit + 1) as i64).into()],
    )?;
    let more = allowed.len() > limit;
    allowed.truncate(limit);
    let next = if more {
        json!(authority.cursor(&scope, allowed.last().unwrap()["id"].as_str().unwrap())?)
    } else {
        Value::Null
    };
    Ok(
        json!({"mode":"selection","entity":selected.entity,"ids":allowed.into_iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),"page_complete":true,"has_more":more,"selection_complete":!more,"next_cursor":next}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Store,
        graphql::tests::{fixture, limited},
    };
    use rusqlite::params;
    fn reopened(root: &tempfile::TempDir) -> Access {
        Access::authenticate(
            Store::open(&root.path().join("catalog.sqlite3"), false, false).unwrap(),
            "token.secret",
        )
        .unwrap()
    }
    fn grant() -> Value {
        let mut grant = limited();
        grant["report_ids"] = json!([
            "document",
            "selection",
            "sql",
            "mixed",
            "intersection",
            "difference",
            "cycle",
            "wrong",
            "bad"
        ]);
        grant
    }
    fn add_queries(access: &Access) {
        access.store().transaction(|db| {
            for (id, definition) in [
                ("sql",json!({"mode":"selection","selection":{"language":"sql","query":"SELECT id AS item_id FROM items WHERE (id='a' AND metadata LIKE '%DO_NOT_DISCLOSE%') OR id='hidden'"}})),
                ("mixed",json!({"mode":"selection","combine":"union","queries":["sql","selection"]})),
                ("intersection",json!({"mode":"selection","combine":"intersection","queries":["sql","selection"]})),
                ("difference",json!({"mode":"selection","combine":"difference","queries":["selection","sql"]})),
                ("cycle",json!({"mode":"selection","combine":"union","queries":["sql","cycle"]})),
                ("bad",json!({"mode":"selection","combine":"union","queries":["sql","document"]})),
                ("file-selection",json!({"mode":"selection","selection":{"language":"graphql","query":"query($after:String){files(first:1,after:$after){nodes{id} pageInfo{hasNextPage endCursor}}}"}})),
                ("wrong",json!({"mode":"selection","combine":"union","queries":["sql","file-selection"]})),
            ] {
                db.execute("INSERT INTO saved_queries(id,profile,name,revision,definition,digest) VALUES (?,'default',?,1,?,?)",params![id,id,encode(&definition),id])?;
            }
            Ok(())
        }).unwrap();
    }
    #[test]
    fn http_saved_document_and_graphql_selection_use_admitted_snapshot() {
        let (root, access) = fixture(grant());
        let value = saved(access, "document", 100, None).unwrap();
        assert!(value.get("errors").is_none(), "{value}");
        assert_eq!(value["data"]["items"]["nodes"].as_array().unwrap().len(), 2);
        assert!(!encode(&value).contains("DO_NOT_DISCLOSE"));
        let first = saved(reopened(&root), "selection", 1, None).unwrap();
        assert_eq!(first["ids"], json!(["a"]));
        assert_eq!(first["selection_complete"], false);
        let cursor = first["next_cursor"].as_str().unwrap();
        let second = saved(reopened(&root), "selection", 1, Some(cursor)).unwrap();
        assert_eq!(second["ids"], json!(["b"]));
        assert_eq!(second["selection_complete"], true);
        assert_eq!(
            saved(reopened(&root), "unapproved", 100, None)
                .unwrap_err()
                .0,
            "not_found"
        );
    }
    #[test]
    fn http_saved_mixed_compositions_preserve_global_sql_then_intersect() {
        let (root, access) = fixture(grant());
        add_queries(&access);
        drop(access);
        for (id, ids) in [
            ("sql", json!(["a"])),
            ("mixed", json!(["a", "b"])),
            ("intersection", json!(["a"])),
            ("difference", json!(["b"])),
        ] {
            let value = saved(reopened(&root), id, 100, None).unwrap();
            assert_eq!(value["ids"], ids, "{id}: {value}");
            assert_eq!(value["selection_complete"], true);
        }
        let first = saved(reopened(&root), "mixed", 1, None).unwrap();
        assert_eq!(
            saved(
                reopened(&root),
                "selection",
                1,
                first["next_cursor"].as_str()
            )
            .unwrap_err()
            .0,
            "invalid_cursor"
        );
    }
    #[test]
    fn http_saved_invalid_branches_fail_closed_and_graphql_grant_bootstrap_is_scoped() {
        let (root, access) = fixture(grant());
        add_queries(&access);
        drop(access);
        for (id, error) in [
            ("cycle", "query composition cycle or depth limit"),
            (
                "bad",
                "rule/projection requires a complete ID selection, not rows/document",
            ),
            ("wrong", "composed queries must select the same entity"),
        ] {
            assert_eq!(saved(reopened(&root), id, 100, None).unwrap_err().0, error);
        }
        let mut membership = grant();
        membership["query_id"] = json!("selection");
        let (_root, access) = fixture(membership);
        let result = saved(access, "selection", 100, None).unwrap();
        assert_eq!(result["ids"], json!(["a", "b", "hidden"]));
    }
    #[test]
    fn http_authority_cannot_bind_after_snapshot_change() {
        let (_root, mut access) = fixture(grant());
        let authority = api_catalog::install(&mut access).unwrap();
        access
            .store()
            .db
            .pragma_update(None, "query_only", false)
            .unwrap();
        access
            .store()
            .db
            .execute("UPDATE main.items SET metadata='{}' WHERE id='a'", [])
            .unwrap();
        let result = access.into_evaluation().bind_authority(authority);
        assert!(matches!(result, Err(ref error) if error.0 == "authorization_snapshot_mismatch"));
    }
}
