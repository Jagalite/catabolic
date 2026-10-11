// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Catalog-wide tag vocabulary and independently attributed tag assertions.
use crate::{Error, Result, Store, mutation};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use unicode_normalization::UnicodeNormalization;

pub(crate) fn unicode_other(character: char) -> bool {
    static RANGES: std::sync::OnceLock<Vec<[u32; 2]>> = std::sync::OnceLock::new();
    let ranges = RANGES.get_or_init(|| {
        let value: Value = serde_json::from_str(include_str!("../resources/unicode-other.json"))
            .expect("frozen Unicode categories");
        serde_json::from_value(value["ranges"].clone()).expect("ordered Unicode ranges")
    });
    let code = u32::from(character);
    let index = ranges.partition_point(|range| range[1] < code);
    ranges.get(index).is_some_and(|range| range[0] <= code)
}

pub fn text_value<'a>(
    value: &'a Value,
    label: &str,
    maximum: usize,
    empty: bool,
) -> Result<&'a str> {
    value
        .as_str()
        .filter(|value| {
            (empty || !value.trim().is_empty())
                && value.chars().count() <= maximum
                && !value.chars().any(unicode_other)
        })
        .ok_or_else(|| {
            Error(format!(
                "{label} must be text without control characters, at most {maximum} characters"
            ))
        })
}

pub fn tag_name(value: &Value) -> Result<String> {
    let name = text_value(value, "tag name", 255, false)?;
    let mut normalized = crate::sql::casefold(name.trim()).nfc().collect::<String>();
    if let Some((namespace, label)) = normalized.split_once(':') {
        let namespace = namespace.trim();
        let label = label.trim();
        if label.is_empty()
            || namespace.is_empty()
            || !namespace.chars().enumerate().all(|(i, c)| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || i > 0 && "_.-".contains(c)
            })
        {
            return Err(Error("namespaced tags require namespace:value; namespace uses letters, digits, dots, underscores or hyphens".into()));
        }
        normalized = format!("{namespace}:{label}");
    }
    text_value(&json!(normalized), "tag name", 255, false)?;
    Ok(normalized)
}

pub fn resolve_tag(db: &Connection, value: &Value) -> Result<String> {
    let name = tag_name(value)?;
    db.query_row("SELECT tag_id FROM tag_names WHERE name=?", [&name], |r| {
        r.get(0)
    })
    .optional()?
    .ok_or_else(|| {
        Error(format!(
            "unknown tag: {}; create it with tag put",
            value.as_str().unwrap_or_default()
        ))
    })
}

fn reserve_name(db: &Connection, name: &str, identifier: &str) -> Result<()> {
    if db.query_row(
        "SELECT EXISTS(SELECT 1 FROM tag_names WHERE name=?)",
        [name],
        |r| r.get::<_, bool>(0),
    )? {
        return Ok(());
    }
    if db.query_row(
        "SELECT count(*) FROM tag_names WHERE tag_id=?",
        [identifier],
        |r| r.get::<_, i64>(0),
    )? >= 101
    {
        return Err(Error("a tag may have at most 100 aliases".into()));
    }
    db.execute(
        "INSERT INTO tag_names VALUES (?,?)",
        params![name, identifier],
    )?;
    Ok(())
}

fn collision(db: &Connection, name: &str, identifier: &str) -> Result<()> {
    let existing: Option<String> = db
        .query_row("SELECT tag_id FROM tag_names WHERE name=?", [name], |r| {
            r.get(0)
        })
        .optional()?;
    if existing.is_some_and(|existing| existing != identifier) {
        return Err(Error("that name belongs to another tag".into()));
    }
    Ok(())
}

pub fn put(store: &Store, name: &Value, description: Option<&Value>) -> Result<Value> {
    let name = tag_name(name)?;
    let description = description
        .filter(|v| !v.is_null())
        .map(|v| text_value(v, "description", 4000, true))
        .transpose()?;
    store.transaction(|db| {
        mutation::require_recovered(db)?;
        let existing: Option<String> = db.query_row("SELECT tag_id FROM tag_names WHERE name=?", [&name], |r|r.get(0)).optional()?;
        let identifier = existing.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if existing.is_none() {
            db.execute("INSERT INTO tags VALUES (?,?,?)", params![identifier,name,description.unwrap_or("")])?;
            db.execute("INSERT INTO tag_names VALUES (?,?)", params![name,identifier])?;
        } else if let Some(description) = description {
            db.execute("UPDATE tags SET description=? WHERE id=?", params![description,identifier])?;
        }
        Ok(db.query_row("SELECT id,name,description FROM tags WHERE id=?", [&identifier], |r| Ok(json!({"id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"description":r.get::<_,String>(2)?})))?)
    })
}

pub fn rename(store: &Store, name: &Value, new_name: &Value) -> Result<Value> {
    let new_name = tag_name(new_name)?;
    store.transaction(|db| {
        mutation::require_recovered(db)?;
        let identifier = resolve_tag(db, name)?;
        collision(db, &new_name, &identifier)?;
        reserve_name(db, &new_name, &identifier)?;
        db.execute(
            "UPDATE tags SET name=? WHERE id=?",
            params![new_name, identifier],
        )?;
        Ok(json!({"id":identifier,"name":new_name}))
    })
}

pub fn alias(store: &Store, name: &Value, alias: &Value, remove: bool) -> Result<Value> {
    let alias = tag_name(alias)?;
    store.transaction(|db| {
        mutation::require_recovered(db)?;
        let identifier = resolve_tag(db, name)?;
        collision(db, &alias, &identifier)?;
        if remove {
            if db.query_row(
                "SELECT EXISTS(SELECT 1 FROM tags WHERE name=?)",
                [&alias],
                |r| r.get::<_, bool>(0),
            )? {
                return Err(Error(
                    "cannot remove a canonical name; use tag rename".into(),
                ));
            }
            db.execute(
                "DELETE FROM tag_names WHERE name=? AND tag_id=?",
                params![alias, identifier],
            )?;
        } else {
            reserve_name(db, &alias, &identifier)?;
        }
        Ok(json!({"tag_id":identifier,"alias":alias,"removed":remove}))
    })
}

pub fn parent(store: &Store, child: &Value, parent: &Value, remove: bool) -> Result<Value> {
    store.transaction(|db| {
        mutation::require_recovered(db)?;
        let child_id = resolve_tag(db,child)?;
        let parent_id = resolve_tag(db,parent)?;
        if !remove && db.query_row("WITH RECURSIVE ancestors(id) AS (VALUES (?) UNION SELECT p.parent_id FROM tag_parents p JOIN ancestors a ON a.id=p.child_id) SELECT EXISTS(SELECT 1 FROM ancestors WHERE id=?)",params![parent_id,child_id],|r|r.get::<_,bool>(0))? { return Err(Error("tag parent would create a cycle".into())); }
        if remove {
            db.execute("DELETE FROM tag_parents WHERE child_id=? AND parent_id=?",params![child_id,parent_id])?;
        } else {
            let exists = db.query_row("SELECT EXISTS(SELECT 1 FROM tag_parents WHERE child_id=? AND parent_id=?)",params![child_id,parent_id],|r|r.get::<_,bool>(0))?;
            if !exists && db.query_row("SELECT count(*) FROM tag_parents WHERE child_id=?",[&child_id],|r|r.get::<_,i64>(0))? >= 100 { return Err(Error("a tag may have at most 100 direct parents".into())); }
            db.execute("INSERT OR IGNORE INTO tag_parents VALUES (?,?)",params![child_id,parent_id])?;
        }
        Ok(json!({"child_id":child_id,"parent_id":parent_id,"removed":remove}))
    })
}

pub struct Assignment<'a> {
    pub names: &'a [Value],
    pub items: &'a [String],
    pub files: &'a [String],
    pub source: &'a Value,
    pub confidence: &'a Value,
    pub note: &'a Value,
    pub remove: bool,
}

pub fn assign(store: &Store, request: Assignment<'_>) -> Result<Value> {
    let Assignment {
        names,
        items,
        files,
        source,
        confidence,
        note,
        remove,
    } = request;
    let items: BTreeSet<_> = items.iter().collect();
    let files: BTreeSet<_> = files.iter().collect();
    if names.is_empty()
        || items.is_empty() && files.is_empty()
        || names.len().saturating_mul(items.len() + files.len()) > 1000
    {
        return Err(Error(
            "provide tags and item/file IDs; at most 1000 assignments per atomic batch".into(),
        ));
    }
    let source = text_value(source, "source", 200, false)?;
    let note = text_value(note, "note", 4000, true)?;
    let confidence = if confidence.is_null() {
        None
    } else {
        Some(
            confidence
                .as_f64()
                .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
                .ok_or_else(|| {
                    Error("confidence must be a finite number between 0 and 1".into())
                })?,
        )
    };
    store.transaction(|db| {
        mutation::require_recovered(db)?;
        let ids: BTreeSet<_> = names.iter().map(|name| resolve_tag(db,name)).collect::<Result<_>>()?;
        let now = mutation::timestamp()?;
        let mut result = Vec::new();
        for (subject, targets) in [("item",items),("file",files)] {
            for target in targets {
                if !db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {subject}s WHERE id=?)"),[target],|r|r.get::<_,bool>(0))? { return Err(Error(format!("unknown {subject}: {target}"))); }
                for tag_id in &ids {
                    let row: Option<(String,bool,Option<f64>,String)> = db.query_row(&format!("SELECT id,active,confidence,note FROM {subject}_tags WHERE {subject}_id=? AND tag_id=? AND source=?"),params![target,tag_id,source],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
                    let identifier = row.as_ref().map(|r|r.0.clone()).unwrap_or_else(||uuid::Uuid::new_v4().to_string());
                    if remove {
                        if row.as_ref().is_some_and(|r|r.1) { db.execute(&format!("UPDATE {subject}_tags SET active=0,updated_at=? WHERE id=?"),params![now,identifier])?; }
                    } else if row.is_none() {
                        db.execute(&format!("INSERT INTO {subject}_tags VALUES (?,?,?,?,?,?,1,?,?)"),params![identifier,target,tag_id,source,confidence,note,now,now])?;
                    } else if row.as_ref().is_some_and(|r| !r.1 || r.2 != confidence || r.3 != note) {
                        db.execute(&format!("UPDATE {subject}_tags SET active=1,confidence=?,note=?,updated_at=? WHERE id=?"),params![confidence,note,now,identifier])?;
                    }
                    result.push(json!({"id":if row.is_some() || !remove {Some(identifier)} else {None},"subject_type":subject,"subject_id":target,"tag_id":tag_id,"source":source,"active":!remove}));
                }
            }
        }
        Ok(json!({"count":result.len(),"assignments":result}))
    })
}

fn ids(request: &Value, key: &str) -> Result<Vec<String>> {
    request
        .get(key)
        .map(|v| {
            v.as_array()
                .ok_or_else(|| Error(format!("{key} must be an array of IDs")))
                .and_then(|values| {
                    values
                        .iter()
                        .map(|v| {
                            v.as_str()
                                .map(str::to_owned)
                                .ok_or_else(|| Error(format!("{key} must be an array of IDs")))
                        })
                        .collect()
                })
        })
        .transpose()
        .map(|v| v.unwrap_or_default())
}

/// Companion JSON boundary. Business operations are shared by the CLI and wheel.
pub fn dispatch(store: &Store, profile: &str, operation: &str, request: &Value) -> Result<Value> {
    mutation::profile(store, profile)?;
    let remove = request["remove"] == true;
    match operation {
        "put" => put(store, &request["name"], request.get("description")),
        "rename" => rename(store, &request["name"], &request["new_name"]),
        "alias" => alias(store, &request["name"], &request["alias"], remove),
        "parent" => parent(store, &request["child"], &request["parent"], remove),
        "assign" => assign(
            store,
            Assignment {
                names: request["names"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
                items: &ids(request, "items")?,
                files: &ids(request, "files")?,
                source: request.get("source").unwrap_or(&json!("manual")),
                confidence: &request["confidence"],
                note: request.get("note").unwrap_or(&json!("")),
                remove,
            },
        ),
        _ => Err(Error("unknown tag operation".into())),
    }
}
