//! Read-only layout evaluation. This module records no definitions or mappings.
use crate::{
    Error, Result, Store,
    migration::{reference, rows},
    query::{decode, name},
    selection::Evaluation,
    sql::{casefold, scalar},
};
use catabolic_core::{encode, sha256};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

fn fail<T>(message: &str) -> Result<T> {
    Err(Error(message.into()))
}
fn read(store: &Store, key: &str) -> Result<Value> {
    let records = rows(
        &store.db,
        "SELECT value FROM meta WHERE key=?",
        &[key.to_owned().into()],
    )?;
    records
        .first()
        .map(|r| serde_json::from_str(r["value"].as_str().unwrap()).map_err(Into::into))
        .unwrap_or(Ok(Value::Null))
}
fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn allowed(value: &Value, keys: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|v| v.keys().all(|k| keys.contains(&k.as_str())))
}
pub fn relative_path(path: &str) -> Result<()> {
    if path.is_empty() || path.contains(['\0', '\\']) {
        return fail("destination must be a nonempty relative POSIX path");
    }
    if path.split('/').any(|p| matches!(p, "" | "." | "..")) {
        return fail("relative paths cannot contain empty, dot, or parent components");
    }
    if path.split('/').any(|p| p.starts_with(".catabolic")) {
        return fail(".catabolic names are reserved");
    }
    Ok(())
}
#[derive(Debug)]
struct Part {
    literal: String,
    field: Option<String>,
    spec: String,
}
fn parts(template: &str) -> Result<Vec<Part>> {
    if template.is_empty() || template.chars().count() > 4096 {
        return fail("layout path must be a nonempty template of at most 4096 characters");
    }
    let mut chars = template.chars().peekable();
    let mut literal = String::new();
    let mut out = vec![];
    while let Some(c) = chars.next() {
        if matches!(c, '{' | '}') && chars.peek() == Some(&c) {
            literal.push(c);
            chars.next();
            continue;
        }
        if c == '}' {
            return fail("invalid path template: Single '}' encountered in format string");
        }
        if c != '{' {
            literal.push(c);
            continue;
        }
        let mut raw = String::new();
        let mut closed = false;
        for c in chars.by_ref() {
            if c == '}' {
                closed = true;
                break;
            }
            raw.push(c);
        }
        if !closed {
            return fail("invalid path template: expected '}' before end of string");
        }
        let (field, spec) = raw.split_once(':').unwrap_or((&raw, ""));
        let words = field.split('.').collect::<Vec<_>>();
        let valid = words.len() > 1
            && words[0]
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
            && words[0]
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_')
            && words.iter().skip(1).all(|w| {
                !w.is_empty()
                    && w.bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
            });
        let width = spec
            .strip_suffix('d')
            .map(|n| n.strip_prefix('0').unwrap_or(n));
        let format_ok = spec.is_empty()
            || width.is_some_and(|n| {
                n.len() <= 2
                    && !n.starts_with('0')
                    && n.parse::<usize>().is_ok_and(|n| (1..=99).contains(&n))
            });
        if !valid || !format_ok {
            return fail(
                "templates allow dotted field names and small integer formats such as :02d; expressions, indexing, and conversions are not allowed",
            );
        }
        out.push(Part {
            literal: std::mem::take(&mut literal),
            field: Some(field.into()),
            spec: spec.into(),
        });
    }
    out.push(Part {
        literal,
        field: None,
        spec: String::new(),
    });
    if out.iter().any(|p| {
        p.literal
            .chars()
            .any(|c| c == '\\' || c <= '\u{1f}' || c == '\u{7f}')
    }) {
        return fail("template literals cannot contain backslashes or control characters");
    }
    relative_path(
        &out.iter()
            .map(|p| {
                format!(
                    "{}{}",
                    p.literal,
                    if p.field.is_some() { "value" } else { "" }
                )
            })
            .collect::<String>(),
    )?;
    Ok(out)
}
pub fn validate(definition: &Value) -> Result<()> {
    if !allowed(
        definition,
        &["version", "rules", "selection", "profile", "normalization"],
    ) || definition["version"].as_i64() != Some(1)
    {
        return fail("layout requires version: 1 and rules");
    }
    if let Some(profile) = definition.get("profile") {
        let presets: Value = serde_json::from_str(include_str!("../resources/layouts.json"))?;
        let matched = presets
            .as_object()
            .unwrap()
            .values()
            .find(|v| v.get("profile") == Some(profile));
        let Some(matched) = matched else {
            return fail("unsupported layout profile; use layout presets for supported versions");
        };
        if definition["rules"] != matched["rules"]
            || definition.get("normalization") != matched.get("normalization")
        {
            return fail(
                "a versioned profile requires its exact naming rules; remove profile to create a custom layout",
            );
        }
    }
    if !matches!(
        definition["normalization"].as_str().unwrap_or("portable"),
        "portable" | "ascii"
    ) {
        return fail("normalization must be portable or ascii");
    }
    if let Some(selection) = definition.get("selection") {
        crate::selection::validate(selection)?;
    }
    if !definition["rules"].is_array() || !(1..=100).contains(&array(&definition["rules"]).len()) {
        return fail("layout must contain 1–100 rules");
    }
    let mut names = BTreeSet::new();
    for rule in array(&definition["rules"]) {
        if !allowed(rule, &["name", "when", "path", "relations"]) {
            return fail("invalid layout rule fields");
        }
        let id = rule["name"]
            .as_str()
            .ok_or_else(|| Error("each rule needs a name".into()))?;
        name(id)?;
        if !names.insert(id) {
            return fail("rule names must be unique");
        }
        let template = parts(rule["path"].as_str().unwrap_or(""))?;
        let when = rule.get("when").cloned().unwrap_or(json!({}));
        if !allowed(&when, &["kinds", "roles", "has", "metadata", "extensions"]) {
            return fail("when supports kinds, roles, has, metadata and extensions");
        }
        for key in ["kinds", "roles", "has", "extensions"] {
            if when.get(key).is_some()
                && (!when[key].is_array()
                    || array(&when[key]).is_empty()
                    || array(&when[key]).iter().any(|v| !v.is_string()))
            {
                return Err(Error(format!("when.{key} must be a nonempty string array")));
            }
        }
        if array(&when["extensions"]).iter().any(|v| {
            v.as_str().is_none_or(|s| {
                !s.starts_with('.')
                    || s.len() < 2
                    || !s[1..]
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            })
        }) {
            return fail("when.extensions must contain lowercase suffixes such as .epub");
        }
        if when.get("metadata").is_some_and(|v| !v.is_object()) {
            return fail("when.metadata must be a JSON object");
        }
        for (key, vocab, label) in [
            ("kinds", "kinds", "media kind"),
            ("roles", "file_roles", "role"),
        ] {
            for value in array(&when[key]) {
                vocabulary(value, vocab, label)?;
            }
        }
        if rule
            .get("relations")
            .is_some_and(|v| !v.is_array() || array(v).len() > 8)
        {
            return fail("relations must be an array of at most 8 selectors");
        }
        let mut aliases = BTreeSet::from(["item"]);
        for selector in array(&rule["relations"]) {
            if !allowed(
                selector,
                &["alias", "from", "kind", "direction", "target_kind"],
            ) {
                return fail("invalid relation selector");
            }
            let alias = selector["alias"].as_str().unwrap_or("");
            if !alias.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
                || !alias
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
                || aliases.contains(alias)
                || ["file", "association", "probe"].contains(&alias)
            {
                return fail("relation aliases must be unique lowercase names");
            }
            if !aliases.contains(selector["from"].as_str().unwrap_or("item"))
                || !matches!(
                    selector["direction"].as_str().unwrap_or("outgoing"),
                    "outgoing" | "incoming"
                )
            {
                return fail(
                    "relation selector needs an earlier source alias and a valid direction",
                );
            }
            if !selector["kind"].is_string() {
                return fail("relation selector requires kind");
            }
            vocabulary(&selector["kind"], "relationships", "relationship kind")?;
            if selector.get("target_kind").is_some() {
                vocabulary(&selector["target_kind"], "kinds", "media kind")?;
            }
            aliases.insert(alias);
        }
        for part in template {
            if let Some(field) = part.field {
                let alias = field.split('.').next().unwrap();
                if !aliases.contains(alias) && !["file", "association", "probe"].contains(&alias) {
                    return Err(Error(format!("unknown template alias: {field}")));
                }
            }
        }
    }
    Ok(())
}
fn vocabulary(value: &Value, key: &str, label: &str) -> Result<()> {
    let text = value.as_str().unwrap_or("");
    if reference()["media_types"][key].get(text).is_some()
        || array(&reference()["media_types"][key]).contains(value)
        || text.strip_prefix("custom:").is_some_and(|s| {
            !s.is_empty()
                && s.len() <= 63
                && s.as_bytes()[0].is_ascii_lowercase()
                && s.bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_.-".contains(&c))
        })
    {
        return Ok(());
    }
    Err(Error(format!(
        "unknown {label}: {text}; use a built-in name or custom:lowercase_name"
    )))
}
fn component(value: &str) -> Result<String> {
    let value = value
        .nfc()
        .map(|c| {
            if "\\/:*?\"<>|".contains(c) || c <= '\u{1f}' || c == '\u{7f}' {
                '_'
            } else {
                c
            }
        })
        .collect::<String>();
    let value = value.trim().trim_end_matches('.');
    if value.is_empty() || matches!(value, "." | "..") {
        return fail("metadata produced an empty or invalid path component");
    }
    Ok(value.into())
}
pub fn render(template: &str, context: &Value, normalization: &str) -> Result<String> {
    let mut path = String::new();
    for part in parts(template)? {
        path += &part.literal;
        let Some(field) = part.field else {
            continue;
        };
        let mut value = context;
        for key in field.split('.') {
            value = value
                .get(key)
                .filter(|v| !v.is_null())
                .ok_or_else(|| Error(format!("missing template field: {field}")))?;
        }
        if !value.is_string() && !value.is_number() && !value.is_boolean() {
            return Err(Error(format!("template field must be scalar: {field}")));
        }
        let text = if !part.spec.is_empty() {
            let integer = value
                .as_i64()
                .ok_or_else(|| Error(format!("integer required for {field}:{}", part.spec)))?;
            let width = part.spec.trim_end_matches('d').parse::<usize>().unwrap();
            if part.spec.starts_with('0') {
                format!("{integer:0width$}")
            } else {
                format!("{integer:width$}")
            }
        } else {
            match value {
                Value::String(v) => v.clone(),
                Value::Bool(true) => "True".into(),
                Value::Bool(false) => "False".into(),
                _ => encode(value),
            }
        };
        if field == "file.path" {
            relative_path(&text)?;
            path += &text
                .split('/')
                .map(component)
                .collect::<Result<Vec<_>>>()?
                .join("/");
        } else if field == "file.extension" && text.is_empty() {
        } else {
            path += &component(&text)?;
        }
    }
    if normalization == "ascii" {
        path = path
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || "/_.-".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect();
    }
    relative_path(&path)?;
    if path.len() > 4096 || path.split('/').any(|p| p.len() > 255) {
        return fail("generated path exceeds portable filesystem length limits");
    }
    Ok(path)
}
fn item(store: &Store, id: &Value) -> Result<Value> {
    let mut row = rows(
        &store.db,
        "SELECT id,kind,metadata FROM items WHERE id=?",
        &[scalar(id)?],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error("unknown item".into()))?;
    decode(&mut row, &["metadata"])?;
    row["title"] = row["metadata"]["title"].clone();
    row["year"] = row["metadata"]["year"].clone();
    Ok(row)
}
fn suffix(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => &name[i..],
        _ => "",
    }
}
fn matching(rule: &Value, item: &Value, association: &Value) -> bool {
    let when = &rule["when"];
    for (key, value) in [("kinds", &item["kind"]), ("roles", &association["role"])] {
        if when.get(key).is_some() && !array(&when[key]).contains(value) {
            return false;
        }
    }
    if when.get("extensions").is_some()
        && !array(&when["extensions"]).contains(&json!(
            suffix(association["path"].as_str().unwrap()).to_lowercase()
        ))
    {
        return false;
    }
    if array(&when["has"])
        .iter()
        .any(|k| item["metadata"][k.as_str().unwrap_or("")].is_null())
    {
        return false;
    }
    if when["metadata"].as_object().is_some_and(|m| {
        m.iter().any(|(k, v)| {
            item["metadata"]
                .get(k)
                .is_none_or(|actual| encode(actual) != encode(v))
        })
    }) {
        return false;
    }
    true
}
pub fn fact(store: &Store, profile: &str, file: &str, operation: &str) -> Result<Value> {
    let mut records = rows(
        &store.db,
        "SELECT * FROM file_facts WHERE profile=? AND file_id=? AND operation=?",
        &[
            profile.to_owned().into(),
            file.to_owned().into(),
            operation.to_owned().into(),
        ],
    )?;
    let Some(mut row) = records.pop() else {
        return Ok(Value::Null);
    };
    decode(&mut row, &["snapshot", "data"])?;
    let now = crate::components::occurrence(&store.db, profile, file)?;
    row["current"] =
        json!(
            row["status"] == "complete"
                && row["snapshot"].as_object().is_some_and(|snapshot| snapshot
                    .iter()
                    .all(|(k, v)| ["ctime_ns", "source_policy", "volume_uuid"]
                        .contains(&k.as_str())
                        || now[k] == *v))
        );
    Ok(row)
}
fn context(
    store: &Store,
    profile: &str,
    association: &Value,
    rule: &Value,
    mut item: Value,
) -> Result<Value> {
    let mut context = json!({"item":item.take()});
    for selector in array(&rule["relations"]) {
        let origin = context[selector["from"].as_str().unwrap_or("item")]["id"].clone();
        let (source, target) = if selector["direction"] == "incoming" {
            ("target_id", "source_id")
        } else {
            ("source_id", "target_id")
        };
        let mut args = vec![scalar(&origin)?, scalar(&selector["kind"])?];
        let (join, condition) = if selector.get("target_kind").is_some() {
            args.push(scalar(&selector["target_kind"])?);
            (format!("JOIN items i ON i.id=r.{target}"), " AND i.kind=?")
        } else {
            (String::new(), "")
        };
        let row=rows(&store.db,&format!("SELECT count(*) AS matches,min(r.{target}) AS item_id,min(r.position) AS position FROM item_relationships r {join} WHERE r.{source}=? AND r.active=1 AND r.kind=?{condition}"),&args)?.remove(0);
        if row["matches"] != 1 {
            return Err(Error(format!(
                "relation {} must match exactly one item; found {}",
                selector["alias"].as_str().unwrap(),
                row["matches"]
            )));
        }
        let mut target = item_value(store, &row["item_id"])?;
        target["position"] = row["position"].clone();
        context[selector["alias"].as_str().unwrap()] = target;
    }
    let path = association["path"].as_str().unwrap();
    let name = path.rsplit('/').next().unwrap();
    let ext = suffix(path);
    context["file"] = json!({"id":association["file_id"],"name":name,"stem":&name[..name.len()-ext.len()],"extension":ext,"path":path,"location":association["location"]});
    let mut record = association.clone();
    decode(&mut record, &["metadata"])?;
    context["association"] = record;
    if rule["path"].as_str().unwrap().contains("{probe.") {
        let fact = fact(
            store,
            profile,
            association["file_id"].as_str().unwrap(),
            "probe",
        )?;
        context["probe"] = if fact["current"] == true {
            fact["data"].get("summary").cloned().unwrap_or(json!({}))
        } else {
            json!({})
        };
    }
    Ok(context)
}
fn item_value(store: &Store, id: &Value) -> Result<Value> {
    item(store, id)
}
pub fn statistics(
    store: &Store,
    profile: &str,
    associations: &[Value],
    report: &Value,
) -> Result<Value> {
    let files = associations
        .iter()
        .map(|r| r["file_id"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    let mut bytes = 0i128;
    let mut unknown = 0;
    let mut present = 0;
    let mut missing = 0;
    let mut other = 0;
    let mut times = vec![];
    let values = files.iter().cloned().collect::<Vec<_>>();
    for chunk in values.chunks(500) {
        let mut args = vec![profile.to_owned().into()];
        args.extend(chunk.iter().cloned().map(Into::into));
        let marks = vec!["?"; chunk.len()].join(",");
        for row in rows(
            &store.db,
            &format!(
                "SELECT o.size,coalesce(o.status,'unknown') AS status,s.finished_at FROM files f LEFT JOIN observations o ON o.file_id=f.id AND o.profile=? LEFT JOIN scans s ON s.id=o.scan_id WHERE f.id IN ({marks})"
            ),
            &args,
        )? {
            match row["status"].as_str() {
                Some("present") => present += 1,
                Some("missing") => missing += 1,
                _ => other += 1,
            };
            if let Some(size) = row["size"].as_i64() {
                bytes += i128::from(size);
            } else {
                unknown += 1;
            }
            if let Some(time) = row["finished_at"].as_str()
                && !time.is_empty()
            {
                times.push(time.to_owned());
            }
        }
    }
    times.sort();
    Ok(
        json!({"scope":"selected active associations before copy and rendition filtering","query":report,"items":associations.iter().map(|r|r["item_id"].as_str().unwrap()).collect::<BTreeSet<_>>().len(),"associations":associations.len(),"files":files.len(),"known_referenced_bytes":bytes.to_string(),"unknown_size_files":unknown,"availability":{"present":present,"missing":missing,"unknown":other},"oldest_observation_at":times.first(),"newest_observation_at":times.last(),"unobserved_files":files.len()-times.len(),"byte_scope":"sum once per file ID, including recorded missing files; not unique content, physical allocation or symlink storage"}),
    )
}
pub fn plan(
    evaluation: &mut Evaluation,
    id: &str,
    catalog: &str,
    replace: bool,
    allow_empty: bool,
    limit: usize,
) -> Result<Value> {
    if !(1..=1000).contains(&limit) {
        return fail("limit must be between 1 and 1000");
    }
    name(id)?;
    let mut definition = read(evaluation.store(), &format!("layout:{id}"))?;
    if definition.is_null() {
        return Err(Error(format!("unknown layout: {id}")));
    }
    validate(&definition)?;
    let bindings = rows(
        &evaluation.store().db,
        "SELECT * FROM projection_bindings WHERE profile=? AND catalog=?",
        &[evaluation.profile.clone().into(), catalog.to_owned().into()],
    )?;
    if let Some(binding) = bindings.first() {
        if binding["layout"] != id {
            return fail(
                "projection is configured with a different layout; execute that projection explicitly",
            );
        }
        definition["selection"] =
            json!({"query_id":binding["query_id"],"profile":evaluation.profile});
    }
    if rows(
        &evaluation.store().db,
        "SELECT 1 FROM catalogs WHERE id=?",
        &[catalog.to_owned().into()],
    )?
    .is_empty()
    {
        return Err(Error(format!("unknown catalog: {catalog}")));
    }
    if !rows(&evaluation.store().db, "SELECT 1 FROM journal LIMIT 1", &[])?.is_empty() {
        return fail("recover pending operations before changing desired catalog state");
    }
    let owner = read(evaluation.store(), &format!("layout-catalog:{catalog}"))?;
    if !owner.is_null() && owner["layout"] != id && !replace {
        return Err(Error(format!(
            "catalog is managed by layout {}; use --replace-layout explicitly",
            owner["layout"].as_str().unwrap_or("")
        )));
    }
    let owned = array(&owner["mapping_ids"])
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    let current = rows(
        &evaluation.store().db,
        "SELECT * FROM mappings WHERE catalog=?",
        &[catalog.to_owned().into()],
    )?
    .into_iter()
    .map(|r| (r["id"].as_str().unwrap().to_owned(), r))
    .collect::<BTreeMap<_, _>>();
    if owned.iter().any(|id| !current.contains_key(id)) {
        return fail(
            "layout ownership references missing mappings; refusing to discard its history",
        );
    }
    let (admitted, mut publication) =
        crate::publication::candidates(evaluation.store(), &evaluation.profile, catalog)?;
    if admitted.is_some()
        && definition.get("selection").is_some()
        && definition["selection"]["profile"]
            .as_str()
            .unwrap_or("default")
            != evaluation.profile
    {
        return fail("rendition publication selection must use the current profile");
    }
    let (mut associations, selection_report) = if let Some(selection) = definition.get("selection")
    {
        evaluation.associations(selection, admitted.as_deref().unwrap_or(&[]))?
    } else if admitted.is_some() && publication["definition"]["include_originals"] != true {
        (vec![], Value::Null)
    } else {
        (
            rows(
                &evaluation.store().db,
                "SELECT a.*,f.path,f.location FROM item_files a JOIN files f ON f.id=a.file_id WHERE a.active=1 ORDER BY a.id LIMIT 100001",
                &[],
            )?,
            Value::Null,
        )
    };
    if associations.len() > 100000 {
        return fail("layout planning supports at most 100000 active identifications");
    }
    let stats = statistics(
        evaluation.store(),
        &evaluation.profile,
        &associations,
        &selection_report,
    )?;
    if let Some(admitted) = &admitted {
        if definition.get("selection").is_none() {
            for row in admitted {
                if let Some(existing) = associations.iter_mut().find(|a| a["id"] == row["id"]) {
                    *existing = row.clone();
                } else {
                    associations.push(row.clone());
                }
            }
        }
        let allowed = admitted
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_owned())
            .collect::<BTreeSet<_>>();
        if publication["definition"]["include_originals"] != true {
            associations.retain(|r| allowed.contains(r["id"].as_str().unwrap()));
        } else {
            let rendition_files = rows(
                &evaluation.store().db,
                "SELECT DISTINCT file_id FROM media_outputs",
                &[],
            )?
            .into_iter()
            .map(|r| r["file_id"].as_str().unwrap().to_owned())
            .collect::<BTreeSet<_>>();
            associations.retain(|r| {
                !rendition_files.contains(r["file_id"].as_str().unwrap())
                    || allowed.contains(r["id"].as_str().unwrap())
            });
        }
    }
    if associations.len() > 100000 {
        return fail("layout planning supports at most 100000 active identifications");
    }
    let (filtered, copy_report) =
        if admitted.is_none() || publication["definition"]["mode"] == "preferred" {
            copy_filter(
                evaluation.store(),
                &evaluation.profile,
                catalog,
                &associations,
            )?
        } else {
            (associations, json!({"configured":false}))
        };
    associations = filtered;
    if !array(&copy_report["blockers"]).is_empty() {
        return Err(Error(format!(
            "copy selection blocked: {}",
            encode(&copy_report["blockers"])
        )));
    }
    let mut blockers = vec![];
    let mut desired = vec![];
    let mut skipped = 0;
    for association in associations {
        let item = item_value(evaluation.store(), &association["item_id"])?;
        let Some(rule) = array(&definition["rules"])
            .iter()
            .find(|r| matching(r, &item, &association))
        else {
            skipped += 1;
            continue;
        };
        let candidate = (|| -> Result<Value> {
            let context = context(
                evaluation.store(),
                &evaluation.profile,
                &association,
                rule,
                item,
            )?;
            let destination = render(
                rule["path"].as_str().unwrap(),
                &context,
                definition["normalization"].as_str().unwrap_or("portable"),
            )?;
            let mapping = Uuid::new_v5(
                &Uuid::parse_str(&evaluation.store().database_id)
                    .map_err(|e| Error(e.to_string()))?,
                encode(&json!([
                    catalog,
                    association["file_id"],
                    association["item_id"],
                    destination
                ]))
                .as_bytes(),
            )
            .to_string();
            if current.contains_key(&mapping) && !owned.contains(&mapping) {
                return fail(
                    "generated destination belongs to an explicit mapping; refusing to adopt it",
                );
            }
            Ok(
                json!({"id":mapping,"catalog":catalog,"file_id":association["file_id"],"item_id":association["item_id"],"path":destination,"active":1}),
            )
        })();
        match candidate {
            Ok(row) => {
                if let Some(existing) = desired
                    .iter_mut()
                    .find(|r: &&mut Value| r["id"] == row["id"])
                {
                    *existing = row;
                } else {
                    desired.push(row);
                }
            }
            Err(error) => blockers.push(
                json!({"association":association["id"],"rule":rule["name"],"reason":error.0}),
            ),
        }
    }
    let mut paths = BTreeMap::new();
    let mut path_order = vec![];
    for row in desired.iter().chain(
        current
            .iter()
            .filter(|(key, row)| !owned.contains(*key) && row["active"] != 0)
            .map(|(_, row)| row),
    ) {
        let key = casefold(&row["path"].as_str().unwrap().nfc().collect::<String>());
        if let Some(previous) = paths.insert(key.clone(), row.clone()) {
            if previous["id"] != row["id"] {
                blockers.push(json!({"path":row["path"],"reason":"destination collision"}));
            }
        } else {
            path_order.push(key);
        }
    }
    for key in path_order {
        let row = &paths[&key];
        let parts = key.split('/').collect::<Vec<_>>();
        if (1..parts.len()).any(|n| paths.contains_key(&parts[..n].join("/"))) {
            blockers.push(
                json!({"path":row["path"],"reason":"destination has a conflicting file parent"}),
            );
        }
    }
    let ids = desired
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    let mut changes = owned
        .difference(&ids)
        .filter(|id| current[*id]["active"] != 0)
        .map(|id| {
            let mut row = current[id].clone();
            row["action"] = json!("disable");
            row
        })
        .collect::<Vec<_>>();
    let removals = changes.len();
    for row in &desired {
        let id = row["id"].as_str().unwrap();
        if current.get(id).is_none_or(|r| r["active"] == 0) {
            let mut change = row.clone();
            change["action"] = json!(if current.contains_key(id) {
                "enable"
            } else {
                "create"
            });
            changes.push(change);
        }
    }
    if (!selection_report.is_null() || admitted.is_some())
        && desired.is_empty()
        && removals > 0
        && !allow_empty
    {
        blockers.push(json!({"reason":"query layout would clear all generated mappings; inspect the selection and use --allow-empty explicitly"}));
    }
    let excluded = array(&publication["excluded"]).len();
    publication["excluded_count"] = json!(excluded);
    publication["details_truncated"] = json!(excluded > limit);
    publication["excluded"] = json!(
        array(&publication["excluded"])
            .iter()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
    );
    let count = changes.len();
    let blocked = blockers.len();
    let mut report = json!({"layout":id,"catalog":catalog,"definition_sha256":sha256(encode(&definition).as_bytes()),"safe":blocked==0,"blockers":blockers.into_iter().take(limit).collect::<Vec<_>>(),"changes":changes.into_iter().take(limit).collect::<Vec<_>>(),"desired_count":desired.len(),"unchanged_count":desired.len()-(count-removals),"skipped_associations":skipped,"selection_statistics":stats,"publication":publication,"applied":false,"change_count":count,"blocker_count":blocked,"details_truncated":count.max(blocked)>limit});
    if !selection_report.is_null() {
        report["selection"] = selection_report;
    }
    Ok(report)
}

fn score_cmp(a: &[(u8, f64)], b: &[(u8, f64)]) -> std::cmp::Ordering {
    for (a, b) in a.iter().zip(b) {
        let c = a.0.cmp(&b.0).then_with(|| a.1.total_cmp(&b.1));
        if !c.is_eq() {
            return c;
        }
    }
    a.len().cmp(&b.len())
}
fn rank(
    store: &Store,
    profile: &str,
    file: &str,
    definition: &Value,
) -> Result<(Value, Vec<(u8, f64)>)> {
    let observed = crate::components::occurrence(&store.db, profile, file)?;
    let fact = fact(store, profile, file, "probe")?;
    let summary = if fact["current"] == true {
        fact["data"]["summary"].clone()
    } else {
        json!({})
    };
    let mut values = BTreeMap::new();
    for key in ["size", "location", "path"] {
        values.insert(key, observed[key].clone());
    }
    for key in ["height", "width", "hdr", "duration"] {
        values.insert(key, summary[key].clone());
    }
    for (key, source) in [
        ("language", "languages"),
        ("video_codec", "video_codecs"),
        ("audio_codec", "audio_codecs"),
    ] {
        values.insert(key, summary[source].clone());
    }
    let mut failed = vec![];
    if let Some(required) = definition["require"].as_object() {
        for (key, wanted) in required {
            let value = values.get(key.as_str()).unwrap_or(&Value::Null);
            if value.is_null()
                || if value.is_array() {
                    !array(value).contains(wanted)
                } else {
                    value != wanted
                }
            {
                failed.push(key.clone());
            }
        }
    }
    if definition["available_only"] == true && observed["status"] != "present" {
        failed.push("availability".into());
    }
    let mut score = vec![];
    for rule in array(&definition["prefer"]) {
        let value = values
            .get(rule["field"].as_str().unwrap_or(""))
            .unwrap_or(&Value::Null);
        if rule.get("values").is_some() {
            let candidates = if value.is_array() {
                array(value).to_vec()
            } else {
                vec![value.clone()]
            };
            let index = candidates
                .iter()
                .filter_map(|v| array(&rule["values"]).iter().position(|r| r == v))
                .min()
                .unwrap_or(array(&rule["values"]).len());
            score.push((0, index as f64));
        } else if value.is_null() {
            score.push((1, 0.0));
        } else {
            let number = value
                .as_f64()
                .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
                .or_else(|| value.as_bool().map(|b| if b { 1.0 } else { 0.0 }))
                .ok_or_else(|| {
                    Error("numeric ordering needs a numeric field; use values for text".into())
                })?;
            score.push((
                0,
                if rule["order"] == "desc" {
                    -number
                } else {
                    number
                },
            ));
        }
    }
    Ok((
        json!({"file_id":file,"required_fields_failed":failed,"unknown_fields":values.iter().filter(|(_,v)|v.is_null()).map(|(k,_)|*k).collect::<Vec<_>>(),"rank":score,"observed_status":observed["status"]}),
        score,
    ))
}
pub fn copy_filter(
    store: &Store,
    profile: &str,
    catalog: &str,
    associations: &[Value],
) -> Result<(Vec<Value>, Value)> {
    let records = rows(
        &store.db,
        "SELECT definition FROM copy_policies WHERE catalog=?",
        &[catalog.to_owned().into()],
    )?;
    let Some(row) = records.first() else {
        return Ok((associations.to_vec(), json!({"configured":false})));
    };
    let definition: Value = serde_json::from_str(row["definition"].as_str().unwrap())?;
    let mut retained = vec![];
    let mut groups = vec![];
    let mut grouped: BTreeMap<(String, String), Vec<Value>> = BTreeMap::new();
    for row in associations {
        if row["role"] == "primary" {
            let key = (
                row["item_id"].as_str().unwrap().to_owned(),
                encode(&row["part"]),
            );
            if !grouped.contains_key(&key) {
                groups.push(key.clone());
            }
            grouped.entry(key).or_default().push(row.clone());
        } else {
            retained.push(row.clone());
        }
    }
    let mut decisions = vec![];
    let mut blockers = vec![];
    for key in groups {
        let rows = &grouped[&key];
        let mut candidates = vec![];
        let mut eligible = vec![];
        for row in rows {
            let (explanation, score) = rank(
                store,
                profile,
                row["file_id"].as_str().unwrap(),
                &definition,
            )?;
            if array(&explanation["required_fields_failed"]).is_empty() {
                eligible.push((score, row));
            }
            candidates.push(explanation);
        }
        if eligible.is_empty() {
            blockers.push(json!({"item_id":key.0,"part":rows[0]["part"],"reason":"no copy meets required criteria"}));
            continue;
        }
        eligible.sort_by(|a, b| {
            score_cmp(&a.0, &b.0)
                .then_with(|| a.1["file_id"].as_str().cmp(&b.1["file_id"].as_str()))
        });
        let tied = eligible
            .iter()
            .filter(|(score, _)| score_cmp(score, &eligible[0].0).is_eq())
            .map(|(_, row)| row["file_id"].clone())
            .collect::<Vec<_>>();
        if tied.len() > 1 && definition["tie_break"] != "file_id" {
            blockers.push(json!({"item_id":key.0,"part":rows[0]["part"],"reason":"copy preference tie; set an explicit tie_break or refine preferences","files":tied}));
            continue;
        }
        let winner = eligible[0].1;
        retained.push(winner.clone());
        decisions.push(json!({"item_id":key.0,"part":rows[0]["part"],"selected":winner["file_id"],"candidates":candidates}));
    }
    let selected = retained
        .iter()
        .filter(|r| r["role"] == "primary")
        .map(|r| r["file_id"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    let mut attached = vec![];
    for row in retained {
        let metadata: Value = serde_json::from_str(row["metadata"].as_str().unwrap())?;
        let related = metadata.get("primary_file_ids");
        if related.is_some_and(|v| {
            !v.is_null() && (!v.is_array() || array(v).iter().any(|v| !v.is_string()))
        }) {
            return fail("sidecar primary_file_ids must be an array of file IDs");
        }
        if row["role"] == "primary"
            || related.is_none_or(|v| {
                v.is_null()
                    || array(v).is_empty()
                    || array(v)
                        .iter()
                        .any(|id| selected.contains(id.as_str().unwrap()))
            })
        {
            attached.push(row);
        }
    }
    Ok((
        attached,
        json!({"configured":true,"definition":definition,"decisions":decisions,"safe":blockers.is_empty(),"blockers":blockers}),
    ))
}
