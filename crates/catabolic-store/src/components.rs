use crate::{Error, Result, migration::rows};
use catabolic_core::{encode, sha256};
use rusqlite::{Connection, functions::FunctionFlags, params, types::Value as SqlValue};
use serde_json::{Value, json};
use uuid::Uuid;

fn language(value: &Value) -> Value {
    let Some(value) = value.as_str() else {
        return Value::Null;
    };
    if value.trim().is_empty() || ["und", "unknown"].contains(&value.to_lowercase().as_str()) {
        return Value::Null;
    }
    let lower = value.to_lowercase();
    json!(match lower.as_str() {
        "eng" => "en",
        "fra" | "fre" => "fr",
        "deu" | "ger" => "de",
        "spa" => "es",
        "jpn" => "ja",
        "zho" | "chi" => "zh",
        "ita" => "it",
        "por" => "pt",
        "rus" => "ru",
        "kor" => "ko",
        "hin" => "hi",
        "ara" => "ar",
        _ => &lower,
    })
}
fn parse(raw: Option<String>, fallback: &str) -> rusqlite::Result<Value> {
    serde_json::from_str(raw.as_deref().filter(|s| !s.is_empty()).unwrap_or(fallback))
        .map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))
}
fn normalize(
    stream: Value,
    metadata: Value,
    assertions: Value,
    format: Option<String>,
    inherited: Value,
) -> Result<String> {
    let mut observed = serde_json::Map::new();
    observed.insert("language".into(), language(&stream["tags"]["language"]));
    observed.insert("title".into(), stream["tags"]["title"].clone());
    let raw_subtitle = format
        .as_deref()
        .is_some_and(|f| ["srt", "webvtt", "ass", "ssa", "microdvd", "subviewer"].contains(&f));
    for (key, flag) in [
        ("default", "default"),
        ("forced", "forced"),
        ("commentary", "comment"),
        ("hearing_impaired", "hearing_impaired"),
        ("visual_impaired", "visual_impaired"),
    ] {
        let value = stream["disposition"][flag].as_i64();
        observed.insert(
            key.into(),
            if !raw_subtitle && value.is_some_and(|n| n == 0 || n == 1) {
                json!(value == Some(1))
            } else {
                Value::Null
            },
        );
    }
    let mut asserted = serde_json::Map::new();
    if let Some(o) = inherited.as_object() {
        for (k, v) in o {
            if observed.contains_key(k) && !v.is_null() {
                asserted.insert(k.clone(), v.clone());
            }
        }
    }
    if let Some(o) = metadata.as_object() {
        for (k, v) in o {
            if observed.contains_key(k) {
                asserted.insert(k.clone(), v.clone());
            }
        }
    }
    if let Some(v) = metadata.get("sdh") {
        asserted.insert("hearing_impaired".into(), v.clone());
    }
    if let Some(v) = asserted.get_mut("language") {
        *v = language(v);
    }
    let mut conflicts = std::collections::BTreeSet::new();
    let mut compatibility = vec![];
    let mut dependencies = vec![];
    let mut identities = std::collections::BTreeSet::new();
    let mut complete = vec![];
    for assertion in assertions
        .as_array()
        .ok_or_else(|| Error("component assertions must be an array".into()))?
    {
        identities.insert(encode(&assertion["component_id"]));
        let payload = &assertion["payload"];
        if let Some(v) = payload.get("compatibility").filter(|v| !v.is_null()) {
            compatibility.push(v.clone());
        }
        if let Some(v) = payload["dependencies"].as_array() {
            dependencies.extend(v.iter().cloned());
        }
        if let Some(v) = payload
            .get("dependencies_complete")
            .filter(|v| !v.is_null())
        {
            complete.push(v.clone());
        }
        if let Some(attributes) = payload["attributes"].as_object() {
            for (key, value) in attributes {
                let value = if key == "language" {
                    language(value)
                } else {
                    value.clone()
                };
                if asserted.get(key).is_some_and(|v| *v != value) {
                    conflicts.insert(key.clone());
                }
                asserted.insert(key.clone(), value);
            }
        }
    }
    if identities.len() > 1 {
        conflicts.insert("component_identity".into());
    }
    if compatibility
        .iter()
        .map(encode)
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        > 1
    {
        conflicts.insert("compatibility".into());
    }
    let mut effective = serde_json::Map::new();
    for (key, value) in &observed {
        let claim = asserted.get(key).cloned().unwrap_or(Value::Null);
        if !value.is_null() && !claim.is_null() && *value != claim {
            conflicts.insert(key.clone());
        }
        effective.insert(
            key.clone(),
            if conflicts.contains(key) {
                Value::Null
            } else if !value.is_null() {
                value.clone()
            } else {
                claim
            },
        );
    }
    let mut unique_deps = vec![];
    for dep in dependencies {
        if !unique_deps.iter().any(|v| encode(v) == encode(&dep)) {
            unique_deps.push(dep);
        }
    }
    let compatibility = if !conflicts.contains("compatibility") {
        compatibility.first().cloned().unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    let dependencies_complete = if complete
        .iter()
        .map(encode)
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        == 1
    {
        complete[0].clone()
    } else {
        Value::Null
    };
    Ok(encode(
        &json!({"observed":observed,"asserted":asserted,"effective":effective,"compatibility":compatibility,"dependencies":unique_deps,"dependencies_complete":dependencies_complete,"conflicts":conflicts}),
    ))
}
pub fn install(db: &Connection) -> Result<()> {
    db.create_scalar_function(
        "component_normalize",
        5,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            normalize(
                parse(ctx.get(0)?, "{}")?,
                parse(ctx.get(1)?, "{}")?,
                parse(ctx.get(2)?, "[]")?,
                ctx.get(3)?,
                parse(ctx.get(4)?, "{}")?,
            )
            .map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))
        },
    )?;
    Ok(())
}
fn revision(value: &Value) -> String {
    let mut value = value.clone();
    if let Some(o) = value.as_object_mut() {
        o.remove("ctime_ns");
    }
    sha256(encode(&value))
}
pub(crate) fn occurrence(db: &Connection, profile: &str, file: &str) -> Result<Value> {
    let args = [
        SqlValue::Text(profile.into()),
        SqlValue::Text(profile.into()),
        SqlValue::Text(file.into()),
    ];
    let mut snapshot = rows(db,"SELECT f.id,f.location,f.path,o.size,o.mtime_ns,o.device,o.inode,o.status,b.root,b.device AS root_device,b.inode AS root_inode FROM files f LEFT JOIN observations o ON o.file_id=f.id AND o.profile=? LEFT JOIN bindings b ON b.profile=? AND b.kind='source' AND b.owner=f.location WHERE f.id=?",&args)?.into_iter().next().ok_or_else(|| Error(format!("unknown file: {file}")))?;
    let policies = rows(
        db,
        "SELECT * FROM source_identity_policies WHERE profile=? AND location=?",
        &[
            SqlValue::Text(profile.into()),
            SqlValue::Text(snapshot["location"].as_str().unwrap().into()),
        ],
    )?;
    let row = policies.first();
    let preset = row
        .and_then(|r| r["identity_policy"].as_str())
        .unwrap_or("strict");
    let setting = if preset == "path" { "skip" } else { "check" };
    let mut settings = json!({"uuid":setting,"device":setting,"inode":setting});
    if let Some(raw) = row
        .and_then(|r| r["settings"].as_str())
        .filter(|s| !s.is_empty())
        && let Some(o) = serde_json::from_str::<Value>(raw)?.as_object()
    {
        for (k, v) in o {
            settings[k] = v.clone();
        }
    }
    snapshot["source_policy"] = json!({"preset":preset,"settings":settings,"revision":row.map(|r| &r["revision"]).cloned().unwrap_or(json!(0))});
    let volumes = rows(
        db,
        "SELECT volume_uuid FROM binding_volumes WHERE profile=? AND kind='source' AND owner=?",
        &[
            SqlValue::Text(profile.into()),
            SqlValue::Text(snapshot["location"].as_str().unwrap().into()),
        ],
    )?;
    snapshot["volume_uuid"] = volumes
        .first()
        .map(|r| r["volume_uuid"].clone())
        .unwrap_or(Value::Null);
    Ok(snapshot)
}
#[allow(clippy::too_many_arguments)]
fn insert(
    db: &Connection,
    namespace: &Uuid,
    profile: &str,
    association: &Value,
    item: &str,
    snapshot: &Value,
    kind: &str,
    locator: Value,
    job: Option<&str>,
    key: Option<usize>,
    stream: Option<&Value>,
) -> Result<()> {
    let rev = revision(snapshot);
    let id = Uuid::new_v5(
        namespace,
        encode(&json!([
            "component_occurrence",
            profile,
            association["id"],
            rev,
            locator,
            association["metadata"],
            association["part"],
            stream
        ]))
        .as_bytes(),
    )
    .to_string();
    let logical = Uuid::new_v5(
        namespace,
        encode(&json!([
            "component",
            profile,
            association["file_id"],
            rev,
            locator
        ]))
        .as_bytes(),
    )
    .to_string();
    db.execute(
        "INSERT OR IGNORE INTO media_components VALUES (?,?)",
        params![logical, kind],
    )?;
    db.execute("INSERT INTO component_occurrences(id,component_id,profile,file_id,association_id,item_id,revision,snapshot,association_evidence,item_evidence,storage,locator,probe_job_id,stream_key,probe_path) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO NOTHING", params![id,logical,profile,association["file_id"].as_str(),association["id"].as_str(),association["item_id"].as_str(),rev,encode(snapshot),encode(association),item,if association["role"]=="primary" {"embedded"} else {"external"},encode(&locator),job,key.map(|k| k as i64),""])?;
    Ok(())
}
pub fn backfill(db: &Connection) -> Result<()> {
    let identity = rows(db, "SELECT value FROM meta WHERE key='database_id'", &[])?;
    let Some(id) = identity.first() else {
        return Ok(());
    };
    let namespace = Uuid::parse_str(
        id["value"]
            .as_str()
            .ok_or_else(|| Error("invalid database identity".into()))?,
    )
    .map_err(|e| Error(e.to_string()))?;
    let generation: i64 = db.query_row(
        "SELECT generation FROM fallback_epoch WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    for row in rows(
        db,
        "SELECT DISTINCT o.profile,o.file_id FROM observations o JOIN item_files a ON a.file_id=o.file_id",
        &[],
    )? {
        let profile = row["profile"].as_str().unwrap();
        let file = row["file_id"].as_str().unwrap();
        let associations = rows(
            db,
            "SELECT * FROM item_files WHERE file_id=? AND (active=1 OR EXISTS (SELECT 1 FROM media_outputs mo WHERE mo.file_id=item_files.file_id AND mo.item_id=item_files.item_id)) ORDER BY id",
            &[SqlValue::Text(file.into())],
        )?;
        if associations.is_empty() {
            continue;
        }
        let now = occurrence(db, profile, file)?;
        let args = [SqlValue::Text(profile.into()), SqlValue::Text(file.into())];
        let mut jobs = rows(
            db,
            "SELECT * FROM processing_jobs WHERE profile=? AND file_id=? AND operation='probe' AND state='complete' ORDER BY created_at,id",
            &args,
        )?;
        jobs.extend(rows(db,"SELECT j.*,f.snapshot AS evidence_snapshot,f.data AS evidence_data FROM file_facts f JOIN processing_jobs j ON j.id=f.job_id WHERE f.profile=? AND f.file_id=? AND f.operation='probe' AND f.status='complete' AND j.state='complete'",&args)?);
        for association in associations {
            let item: String = db.query_row(
                "SELECT metadata FROM items WHERE id=?",
                [association["item_id"].as_str().unwrap()],
                |r| r.get(0),
            )?;
            let metadata: Value = serde_json::from_str(association["metadata"].as_str().unwrap())?;
            let role = association["role"].as_str().unwrap();
            let role_kind = match role {
                "subtitle" => Some("subtitle"),
                "custom:audio" => Some("audio"),
                "custom:video" => Some("video"),
                _ => None,
            };
            let mut indexed = false;
            for job in &jobs {
                let data: Value = serde_json::from_str(
                    job.get("evidence_data")
                        .unwrap_or(&job["result"])
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or("{}"),
                )?;
                let snapshot: Value = serde_json::from_str(
                    job.get("evidence_snapshot")
                        .unwrap_or(&job["snapshot"])
                        .as_str()
                        .unwrap(),
                )?;
                if let Some(streams) = data["streams"].as_array() {
                    for (key, stream) in streams.iter().enumerate() {
                        let Some(kind) = stream["codec_type"]
                            .as_str()
                            .filter(|k| ["video", "audio", "subtitle", "attachment"].contains(k))
                        else {
                            continue;
                        };
                        if !stream["index"].is_i64() {
                            continue;
                        }
                        if role != "primary"
                            && (role_kind != Some(kind)
                                || metadata
                                    .get("stream_index")
                                    .is_some_and(|v| !v.is_null() && *v != stream["index"]))
                        {
                            continue;
                        }
                        insert(
                            db,
                            &namespace,
                            profile,
                            &association,
                            &item,
                            &snapshot,
                            kind,
                            json!({"convention":"ffprobe_absolute_stream_index","index":stream["index"]}),
                            job["id"].as_str(),
                            Some(key),
                            Some(stream),
                        )?;
                        indexed |= revision(&snapshot) == revision(&now);
                    }
                }
            }
            if !indexed
                && now["status"] == "present"
                && let Some(kind) = role_kind
            {
                insert(
                    db,
                    &namespace,
                    profile,
                    &association,
                    &item,
                    &now,
                    kind,
                    json!({"convention":"whole_file"}),
                    None,
                    None,
                    None,
                )?;
            }
        }
    }
    db.execute(
        "UPDATE fallback_epoch SET generation=? WHERE id=1",
        [generation],
    )?;
    Ok(())
}
