// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Read-only, versioned desired-catalog manifest snapshots.
//! Filesystem publication is a separate guarded operation, never implicit here.
use crate::{
    Error, Result, Store, definitions, interchange, layout,
    migration::rows,
    query::{CatalogQuery, decode},
    selection::Evaluation,
};
use catabolic_core::{encode, sha256};
use rusqlite::types::Value as SqlValue;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::{
    ffi::CString,
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::{Component, Path},
};
use unicode_normalization::UnicodeNormalization;

const MAX_RECORDS: usize = 100000;
const NAMESPACE: &str = "catabolic:statistics";
const FILENAME: &str = ".catabolic-manifest.json";
const MAX_BYTES: usize = 128 * 1024 * 1024;

fn path_key(path: impl AsRef<Path>) -> String {
    crate::sql::casefold(&path.as_ref().to_string_lossy().nfc().collect::<String>())
}
fn database_name(candidate: &str, database: &str) -> bool {
    candidate == database
        || candidate.starts_with(&(database.to_owned() + "."))
        || ["-wal", "-shm", "-journal"]
            .iter()
            .any(|suffix| candidate == database.to_owned() + suffix)
}
fn check_owner(store: &Store, profile: &str, root: &File, catalog: &str) -> Result<()> {
    use crate::filesystem_mutation::{owner, owner_state};
    if owner_state(root, &owner(store, profile, catalog))? != "owned" {
        return fail("sync this catalog before writing its manifest");
    }
    Ok(())
}
fn bound_root(store: &Store, binding: &Value) -> Result<File> {
    let root = crate::filesystem_mutation::open_root(string(&binding["root"]))?;
    let status = root.metadata()?;
    if binding["device"].as_u64() != Some(status.dev())
        || binding["inode"].as_u64() != Some(status.ino())
    {
        return Err(Error(format!(
            "root identity changed: {}; inspect remount repair",
            string(&binding["root"])
        )));
    }
    let volume = rows(
        &store.db,
        "SELECT volume_uuid FROM binding_volumes WHERE profile=? AND kind=? AND owner=?",
        &[
            string(&binding["profile"]).to_owned().into(),
            string(&binding["kind"]).to_owned().into(),
            string(&binding["owner"]).to_owned().into(),
        ],
    )?;
    if let Some(volume) = volume.first()
        && crate::source::volume_uuid(&root).as_deref() != volume["volume_uuid"].as_str()
    {
        return fail("bound volume identity changed or unavailable");
    }
    Ok(root)
}

/// Recognize renamed source and output roots by inode as well as their stored
/// spelling. Every ancestor is examined through a held, no-follow descriptor.
fn guard_ancestors(
    store: &Store,
    profile: &str,
    root: &File,
    leaf: &str,
    catalog: &str,
) -> Result<()> {
    use crate::filesystem_mutation::{identity, open_at};
    let bindings = rows(&store.db, "SELECT * FROM bindings", &[])?;
    let database_parent = identity(&store.path.parent().unwrap().metadata()?);
    let database_leaf = path_key(store.path.file_name().unwrap());
    let leaf_key = path_key(leaf);
    let mut current = root.try_clone()?;
    let mut depth = 0;
    loop {
        let current_identity = identity(&current.metadata()?);
        if depth == 0
            && current_identity == database_parent
            && database_name(&leaf_key, &database_leaf)
        {
            return fail("manifest output cannot replace database state");
        }
        for binding in &bindings {
            if (binding["device"].as_u64(), binding["inode"].as_u64())
                != (Some(current_identity.0), Some(current_identity.1))
            {
                continue;
            }
            if binding["kind"] == "source" {
                return fail("manifest output cannot be inside a source tree");
            }
            if depth != 0
                || leaf != FILENAME
                || binding["profile"] != profile
                || binding["owner"] != catalog
            {
                return fail("inside an output catalog, use its reserved --in-catalog manifest");
            }
            check_owner(store, profile, &current, catalog)?;
        }
        let parent = open_at(&current, "..", true)?;
        if identity(&parent.metadata()?) == current_identity {
            return Ok(());
        }
        current = parent;
        depth += 1;
    }
}

fn existing(root: &File, leaf: &str, document: &Value) -> Result<(Option<Value>, bool)> {
    let mut file = match crate::filesystem_mutation::open_at(root, leaf, false) {
        Ok(file) => file,
        Err(error) if error.0.contains("No such file or directory") => return Ok((None, false)),
        Err(error) => return Err(error),
    };
    let status = file.metadata()?;
    if !status.is_file() || status.nlink() != 1 || status.len() > MAX_BYTES as u64 {
        return fail(
            "existing manifest must be a single-link regular file within the output limit",
        );
    }
    let mut raw = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut raw)?;
    let old = catabolic_core::decode_json_bytes(&raw)
        .map_err(|_| Error("refusing to replace an unrelated or edited metadata file".into()))?;
    let content = old.get("content");
    // Missing keys and wrong container types have the same refusal as malformed
    // JSON, before the distinct foreign-owner/unsupported-version refusal.
    for value in [
        content.and_then(|v| v.get("database_id")),
        content.and_then(|v| v.get("profile")),
        content
            .and_then(|v| v.get("catalog"))
            .and_then(|v| v.get("id")),
        old.get("format"),
        old.get("format_version"),
        old.get("content_sha256"),
    ] {
        if value.is_none() {
            return fail("refusing to replace an unrelated or edited metadata file");
        }
    }
    let same_owner = ["database_id", "profile"]
        .iter()
        .all(|key| old["content"][key] == document["content"][key])
        && old["content"]["catalog"]["id"] == document["content"]["catalog"]["id"];
    let valid = old["format"] == "catabolic.catalog-manifest"
        && (matches!(old["format_version"].as_u64(), Some(1..=3)) || old["format_version"] == true)
        && old["content_sha256"] == sha256(encode(&old["content"]));
    if valid {
        interchange::validate(&old)?;
    }
    if !same_owner || !valid {
        return fail("refusing to replace a foreign, edited, or unsupported manifest");
    }
    Ok((
        Some(crate::filesystem_mutation::version(&status)),
        old["content_sha256"] == document["content_sha256"],
    ))
}

fn publish(
    store: &Store,
    profile: &str,
    root: &File,
    path: &Path,
    document: &Value,
    options: &Value,
) -> Result<Value> {
    let leaf = path
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or_else(|| Error("invalid manifest output path".into()))?;
    guard_ancestors(
        store,
        profile,
        root,
        leaf,
        string(&document["content"]["catalog"]["id"]),
    )?;
    let payload = encode(document) + "\n";
    if payload.len() > MAX_BYTES {
        return fail("manifest exceeds the output limit");
    }
    let (identity, unchanged) = existing(root, leaf, document)?;
    let mut result = json!({"output":path.to_string_lossy(),"content_sha256":document["content_sha256"],"counts":document["content"]["counts"],"written":false,"unchanged":unchanged});
    if unchanged {
        return Ok(result);
    }
    if identity.is_some() && options["replace"] != true {
        return fail("manifest changed; use --replace to refresh this owned export");
    }
    let temporary = CString::new(format!(
        ".catabolic-manifest-{}",
        uuid::Uuid::new_v4().simple()
    ))
    .unwrap();
    let name = CString::new(leaf).map_err(|_| Error("invalid manifest output path".into()))?;
    // SAFETY: a live directory descriptor and NUL-terminated relative name are
    // supplied; exclusive creation grants ownership of the returned descriptor.
    let fd = unsafe {
        libc::openat(
            root.as_raw_fd(),
            temporary.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let operation = (|| -> Result<()> {
        // SAFETY: openat returned a fresh descriptor owned by this operation.
        let mut file = unsafe { File::from_raw_fd(fd) };
        file.write_all(payload.as_bytes())?;
        file.flush()?;
        crate::filesystem_mutation::failpoint(options, "manifest-before-file-sync")?;
        file.sync_all()?;
        drop(file);
        crate::filesystem_mutation::failpoint(options, "manifest-before-publish")?;
        if identity.is_none() {
            // SAFETY: descriptors and both relative names are live. linkat
            // atomically refuses an occupied leaf and never follows a symlink.
            if unsafe {
                libc::linkat(
                    root.as_raw_fd(),
                    temporary.as_ptr(),
                    root.as_raw_fd(),
                    name.as_ptr(),
                    0,
                )
            } < 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        } else {
            let (current, _) = existing(root, leaf, document)?;
            if current != identity {
                return fail("manifest changed during export; refusing replacement");
            }
            // SAFETY: descriptor-relative rename publishes only the checked
            // owned manifest, retaining the parent descriptor throughout.
            if unsafe {
                libc::renameat(
                    root.as_raw_fd(),
                    temporary.as_ptr(),
                    root.as_raw_fd(),
                    name.as_ptr(),
                )
            } < 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        root.sync_all()?;
        result["written"] = json!(true);
        Ok(())
    })();
    // SAFETY: temporary is our unpredictable, exclusively created relative
    // name. A successful rename already removed it; that ENOENT is harmless.
    let cleanup = unsafe { libc::unlinkat(root.as_raw_fd(), temporary.as_ptr(), 0) };
    if cleanup == 0 {
        root.sync_all()?;
    } else if std::io::Error::last_os_error().kind() != std::io::ErrorKind::NotFound {
        return Err(std::io::Error::last_os_error().into());
    }
    operation?;
    Ok(result)
}

/// Publish a validated manifest under the existing writer lock and transaction.
/// The descriptor-relative path guards are independent of the document builder.
pub fn write(store: &Store, profile: &str, document: &Value, options: &Value) -> Result<Value> {
    crate::filesystem_mutation::admit_failpoints(options)?;
    if store.lock.is_none() || store.db.is_autocommit() {
        return fail("manifest publication requires the catalog writer lock and snapshot");
    }
    interchange::validate(document)?;
    let catalog = string(&document["content"]["catalog"]["id"]);
    if options["in_catalog"] == true {
        let binding = crate::observation::binding(store, profile, "output", catalog)?;
        let path = Path::new(string(&binding["root"])).join(FILENAME);
        let root = bound_root(store, &binding)?;
        check_owner(store, profile, &root, catalog)?;
        return publish(store, profile, &root, &path, document, options);
    }
    let output = options["output"]
        .as_str()
        .ok_or_else(|| Error("select an output path or --in-catalog".into()))?;
    let path = if Path::new(output).is_absolute() {
        Path::new(output).to_path_buf()
    } else {
        std::env::current_dir()?.join(output)
    };
    if path.components().any(|part| part == Component::ParentDir) {
        return fail("manifest output cannot contain parent traversal");
    }
    let path = path.components().collect::<std::path::PathBuf>();
    let key = path_key(&path);
    for binding in rows(&store.db, "SELECT * FROM bindings", &[])? {
        let binding_path = Path::new(string(&binding["root"]));
        if key.starts_with(&(path_key(binding_path).trim_end_matches('/').to_owned() + "/")) {
            if binding["kind"] == "source" {
                return fail("manifest output cannot be inside a source tree");
            }
            if binding["profile"] != profile
                || binding["owner"] != catalog
                || path != binding_path.join(FILENAME)
            {
                return fail("inside an output catalog, use its reserved --in-catalog manifest");
            }
            let root = bound_root(store, &binding)?;
            check_owner(store, profile, &root, catalog)?;
            return publish(store, profile, &root, &path, document, options);
        }
    }
    if path
        .file_name()
        .is_some_and(|leaf| path_key(leaf).starts_with(".catabolic"))
    {
        return fail("reserved manifest names belong inside their output catalog");
    }
    if database_name(&key, &path_key(&store.path)) {
        return fail("manifest output cannot replace database state");
    }
    let parent = path
        .parent()
        .and_then(|v| v.to_str())
        .ok_or_else(|| Error("invalid manifest output path".into()))?;
    let root = crate::filesystem_mutation::open_root(parent)?;
    publish(store, profile, &root, &path, document, options)
}
fn fail<T>(message: &str) -> Result<T> {
    Err(Error(message.into()))
}
fn string(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}
fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn bounded(n: usize, label: &str) -> Result<()> {
    if n > MAX_RECORDS {
        return Err(Error(format!(
            "manifest exceeds {MAX_RECORDS} {label}; export a smaller catalog"
        )));
    }
    Ok(())
}
fn read(store: &Store, key: &str) -> Result<Value> {
    rows(
        &store.db,
        "SELECT value FROM meta WHERE key=?",
        &[key.to_owned().into()],
    )?
    .first()
    .map(|r| catabolic_core::decode_json(string(&r["value"])).map_err(Into::into))
    .unwrap_or(Ok(Value::Null))
}
fn stat_read(store: &Store, profile: &str, kind: &str, id: &str) -> Result<Value> {
    read(
        store,
        &("statistics:".to_owned() + &encode(&json!([profile, kind, id]))),
    )
}
fn count(store: &Store, profile: &str, catalog: &str, table: &str) -> Result<Value> {
    Ok(rows(
        &store.db,
        &format!("SELECT count(*) AS n FROM {table} WHERE profile=? AND catalog=?"),
        &[profile.to_owned().into(), catalog.to_owned().into()],
    )?[0]["n"]
        .clone())
}
fn file_totals(store: &Store, profile: &str, records: &[Value]) -> Result<Value> {
    let files = records
        .iter()
        .filter(|r| !r["file_id"].is_null())
        .map(|r| string(&r["file_id"]))
        .collect::<BTreeSet<_>>();
    if files.len() > MAX_RECORDS {
        return fail("statistics exceed 100000 files");
    }
    // Reuse the already qualified recorded-file summation, with its decoration
    // removed to expose projection_stats.file_totals exactly.
    let entries = files
        .iter()
        .map(|id| json!({"file_id":id,"item_id":""}))
        .collect::<Vec<_>>();
    let mut result = layout::statistics(store, profile, &entries, &Value::Null)?;
    for key in ["scope", "query", "items", "associations"] {
        result.as_object_mut().unwrap().remove(key);
    }
    Ok(result)
}
pub fn statistics(store: &Store, profile: &str, catalog: &str) -> Result<Value> {
    let fallback = store
        .db
        .prepare("SELECT 1 FROM fallback_bindings WHERE profile=? AND catalog=?")?
        .exists([profile, catalog])?;
    let records = if fallback {
        rows(
            &store.db,
            "SELECT file_id,item_id FROM fallback_entries WHERE profile=? AND catalog=? AND active=1 LIMIT ?",
            &[
                profile.to_owned().into(),
                catalog.to_owned().into(),
                (MAX_RECORDS as i64 + 1).into(),
            ],
        )?
    } else {
        rows(
            &store.db,
            "SELECT file_id,item_id FROM mappings WHERE catalog=? AND active=1 LIMIT ?",
            &[catalog.to_owned().into(), (MAX_RECORDS as i64 + 1).into()],
        )?
    };
    if records.len() > MAX_RECORDS {
        return fail("statistics exceed 100000 mappings");
    }
    let owner = read(store, &format!("layout-catalog:{catalog}"))?;
    let mut result = json!({"as_of":"enclosing execution started_at or manifest generated_at","scope":{"profile":profile,"catalog":catalog},"desired_entries":records.len(),"mapped_items":records.iter().map(|r|string(&r["item_id"])).collect::<BTreeSet<_>>().len(),"selection":file_totals(store,profile,&records)?,"last_query_evaluation":stat_read(store,profile,"selection",catalog)?,"layout_definition_sha256":owner["definition_sha256"],"recorded_links":count(store,profile,catalog,"owned_links")?,"recorded_hardlinks":count(store,profile,catalog,"owned_hardlinks")?,"rules":[]});
    let associated = rows(
        &store.db,
        "SELECT DISTINCT r.id,r.name,r.revision,r.query_id FROM processing_rules r WHERE r.profile=? AND (r.query_id IN (SELECT query_id FROM projection_bindings WHERE profile=? AND catalog=?) OR r.id IN (SELECT json_extract(definition,'$.rule_id') FROM rendition_policies WHERE profile=? AND catalog=?) OR EXISTS (SELECT 1 FROM rule_jobs j JOIN mappings m ON m.file_id=j.file_id OR m.item_id=j.item_id WHERE j.rule_id=r.id AND m.catalog=? AND m.active=1) OR EXISTS (SELECT 1 FROM rule_processor_jobs j JOIN mappings m ON m.file_id=j.file_id OR m.item_id=j.item_id WHERE j.rule_id=r.id AND m.catalog=? AND m.active=1)) ORDER BY r.id LIMIT 1001",
        &[
            profile.to_owned().into(),
            profile.to_owned().into(),
            catalog.to_owned().into(),
            profile.to_owned().into(),
            catalog.to_owned().into(),
            catalog.to_owned().into(),
            catalog.to_owned().into(),
        ],
    )?;
    if associated.len() > 1000 {
        return fail("statistics exceed 1000 associated rules");
    }
    for mut rule in associated {
        let id = string(&rule["id"]).to_owned();
        let mut jobs = json!({});
        let mut activity = json!({});
        for (label, link, table, field) in [
            ("local", "rule_jobs", "processing_jobs", "finished_at"),
            (
                "external",
                "rule_processor_jobs",
                "processor_jobs",
                "updated_at",
            ),
        ] {
            let mut states = json!({});
            for row in rows(
                &store.db,
                &format!(
                    "SELECT j.state,count(*) AS n FROM {link} r JOIN {table} j ON j.id=r.job_id WHERE r.rule_id=? GROUP BY j.state"
                ),
                &[id.clone().into()],
            )? {
                states[string(&row["state"])] = row["n"].clone();
            }
            jobs[label] = states;
            activity[label]=rows(&store.db,&format!("SELECT max(j.{field}) AS at FROM {link} r JOIN {table} j ON j.id=r.job_id WHERE r.rule_id=?"),&[id.clone().into()])?[0]["at"].clone();
        }
        let object = rule.as_object_mut().unwrap();
        object.insert("scope".into(),json!("whole rule revision associated by projection query, rendition policy, or jobs for mapped files/items"));
        object.insert(
            "last_evaluation".into(),
            stat_read(store, profile, "rule", &id)?,
        );
        object.insert("recorded_job_states".into(), jobs);
        object.insert(
            "latest_local_job_finished_at".into(),
            activity["local"].clone(),
        );
        object.insert(
            "latest_external_job_updated_at".into(),
            activity["external"].clone(),
        );
        object.insert(
            "job_scope".into(),
            json!(
                "all attached historical jobs; completion does not imply current rule satisfaction"
            ),
        );
        result["rules"].as_array_mut().unwrap().push(rule);
    }
    Ok(
        json!({"version":1,"current_recorded":result,"last_execution":stat_read(store,profile,"execution",catalog)?,"filesystem_verified_at_export":false}),
    )
}
fn selected_rows(
    store: &Store,
    sql: &str,
    ids: &BTreeSet<String>,
    prefix: &[SqlValue],
) -> Result<Vec<Value>> {
    let ids = ids.iter().collect::<Vec<_>>();
    let mut output = vec![];
    for chunk in ids.chunks(500) {
        let mut values = prefix.to_vec();
        values.extend(chunk.iter().map(|s| (*s).clone().into()));
        let statement = sql.replace("{marks}", &vec!["?"; chunk.len()].join(","));
        output.extend(rows(&store.db, &statement, &values)?);
    }
    Ok(output)
}
fn join(root: &str, relative: &str) -> String {
    format!("{}/{}", root.trim_end_matches('/'), relative)
}
fn relative(source: &str, output: &str) -> String {
    let source = source
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect::<Vec<_>>();
    let output = output
        .rsplit_once('/')
        .map(|(p, _)| p)
        .unwrap_or("")
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect::<Vec<_>>();
    let common = source
        .iter()
        .zip(&output)
        .take_while(|(a, b)| a == b)
        .count();
    let mut pieces = vec![".."; output.len() - common];
    pieces.extend(&source[common..]);
    if pieces.is_empty() {
        ".".into()
    } else {
        pieces.join("/")
    }
}
pub fn build(
    evaluation: &mut Evaluation,
    catalog: &str,
    extra: Option<&Value>,
    selection: Option<&Value>,
) -> Result<Value> {
    let extra = extra.filter(|value| !value.is_null());
    let selection = selection.filter(|value| !value.is_null());
    if evaluation.store().db.is_autocommit() {
        return fail("manifest export requires a database snapshot");
    }
    definitions::require_recovered(evaluation.store())?;
    if extra.is_some_and(|v| !v.is_object()) {
        return fail("extra metadata must be a JSON object");
    }
    if extra.is_some_and(|v| v.get(NAMESPACE).is_some()) {
        return fail("catabolic:statistics is reserved for generated statistics");
    }
    if !evaluation
        .store()
        .db
        .prepare("SELECT id FROM catalogs WHERE id=?")?
        .exists([catalog])?
    {
        return Err(Error(format!("unknown catalog: {catalog}")));
    }
    let mut mappings = rows(
        &evaluation.store().db,
        "SELECT * FROM mappings WHERE catalog=? AND active=1 ORDER BY path,id LIMIT ?",
        &[catalog.to_owned().into(), (MAX_RECORDS as i64 + 1).into()],
    )?;
    bounded(mappings.len(), "entries")?;
    let selected = if let Some(selection) = selection {
        let (selected_rows, _) = evaluation.associations(selection, &[])?;
        let pairs = selected_rows
            .iter()
            .map(|r| {
                (
                    string(&r["item_id"]).to_owned(),
                    string(&r["file_id"]).to_owned(),
                )
            })
            .collect::<BTreeSet<_>>();
        let available = mappings
            .iter()
            .map(|r| {
                (
                    string(&r["item_id"]).to_owned(),
                    string(&r["file_id"]).to_owned(),
                )
            })
            .collect::<BTreeSet<_>>();
        if !pairs.is_subset(&available) {
            return fail("mapping selection includes associations outside the source catalog");
        }
        mappings.retain(|r| {
            pairs.contains(&(
                string(&r["item_id"]).to_owned(),
                string(&r["file_id"]).to_owned(),
            ))
        });
        Some(
            selected_rows
                .iter()
                .map(|r| string(&r["id"]).to_owned())
                .collect::<BTreeSet<_>>(),
        )
    } else {
        None
    };
    let store = evaluation.store();
    let profile = &evaluation.profile;
    let query = CatalogQuery::new(store, profile)?;
    let mode = rows(
        &store.db,
        "SELECT mode FROM catalog_link_modes WHERE catalog=?",
        &[catalog.to_owned().into()],
    )?
    .first()
    .map(|r| r["mode"].clone())
    .unwrap_or(json!("symlink"));
    let bindings = rows(
        &store.db,
        "SELECT kind,owner,root FROM bindings WHERE profile=?",
        &[profile.clone().into()],
    )?;
    let roots = bindings
        .iter()
        .map(|r| {
            (
                (string(&r["kind"]), string(&r["owner"])),
                string(&r["root"]),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let output_root = roots.get(&("output", catalog)).copied();
    let included_paths = mappings
        .iter()
        .map(|r| string(&r["path"]))
        .collect::<BTreeSet<_>>();
    let mut records = rows(
        &store.db,
        "SELECT path,target FROM owned_links WHERE profile=? AND catalog=? ORDER BY path LIMIT ?",
        &[
            profile.clone().into(),
            catalog.to_owned().into(),
            (MAX_RECORDS as i64 + 1).into(),
        ],
    )?;
    bounded(records.len(), "recorded links")?;
    if selected.is_some() {
        records.retain(|r| included_paths.contains(string(&r["path"])));
    }
    let recorded = records
        .iter()
        .map(|r| (string(&r["path"]), &r["target"]))
        .collect::<BTreeMap<_, _>>();
    let file_ids = mappings
        .iter()
        .map(|r| string(&r["file_id"]).to_owned())
        .collect::<BTreeSet<_>>();
    let mut files = BTreeMap::new();
    for mut row in selected_rows(
        store,
        "SELECT f.*,o.size,o.mtime_ns,coalesce(o.status,'unknown') AS status,o.scan_id,s.finished_at AS observed_at FROM files f LEFT JOIN observations o ON o.file_id=f.id AND o.profile=? LEFT JOIN scans s ON s.id=o.scan_id WHERE f.id IN ({marks})",
        &file_ids,
        &[profile.clone().into()],
    )? {
        let root = roots.get(&("source", string(&row["location"]))).copied();
        row["source_root"] = json!(root);
        row["source_path"] = root
            .map(|root| json!(join(root, string(&row["path"]))))
            .unwrap_or(Value::Null);
        for field in ["size", "mtime_ns"] {
            if !row[field].is_null() {
                row[field] = json!(row[field].to_string());
            }
        }
        files.insert(string(&row["id"]).to_owned(), row);
    }
    let mut associations = rows(
        &store.db,
        "SELECT a.* FROM (SELECT DISTINCT item_id,file_id FROM mappings WHERE active=1 AND catalog=?) m JOIN item_files a ON a.item_id=m.item_id AND a.file_id=m.file_id ORDER BY a.id LIMIT ?",
        &[catalog.to_owned().into(), (MAX_RECORDS as i64 + 1).into()],
    )?;
    bounded(associations.len(), "associations")?;
    let owner = read(store, &format!("layout-catalog:{catalog}"))?;
    let admitted = array(&owner["rendition_associations"])
        .iter()
        .map(string)
        .collect::<BTreeSet<_>>();
    associations.retain(|r| {
        (r["active"].as_i64() == Some(1) || admitted.contains(string(&r["id"])))
            && selected
                .as_ref()
                .is_none_or(|s| s.contains(string(&r["id"])))
    });
    let mut by_pair: BTreeMap<(String, String), Vec<Value>> = BTreeMap::new();
    for row in &mut associations {
        decode(row, &["metadata"])?;
        row["active"] = json!(true);
        by_pair
            .entry((
                string(&row["item_id"]).to_owned(),
                string(&row["file_id"]).to_owned(),
            ))
            .or_default()
            .push(row["id"].clone());
    }
    let (mut items, mut relationships) = (BTreeMap::new(), BTreeMap::new());
    let mut frontier = mappings
        .iter()
        .map(|r| string(&r["item_id"]).to_owned())
        .collect::<BTreeSet<_>>();
    while !frontier.is_empty() {
        if items.len() + frontier.len() > MAX_RECORDS {
            return fail("manifest related-item closure exceeds the record limit");
        }
        let mut found = selected_rows(
            store,
            "SELECT * FROM items WHERE id IN ({marks})",
            &frontier,
            &[],
        )?;
        if found
            .iter()
            .map(|r| string(&r["id"]).to_owned())
            .collect::<BTreeSet<_>>()
            != frontier
        {
            return fail("manifest references missing related items");
        }
        query.decorate_items(&mut found)?;
        for item in found {
            items.insert(string(&item["id"]).to_owned(), item);
        }
        let mut next = BTreeSet::new();
        for mut edge in selected_rows(
            store,
            "SELECT * FROM item_relationships WHERE active=1 AND source_id IN ({marks}) ORDER BY id LIMIT 100001",
            &frontier,
            &[],
        )? {
            decode(&mut edge, &["metadata"])?;
            edge["active"] = json!(edge["active"].as_i64() != Some(0));
            next.insert(string(&edge["target_id"]).to_owned());
            relationships.insert(string(&edge["id"]).to_owned(), edge);
            bounded(relationships.len(), "relationships")?;
        }
        frontier = next
            .into_iter()
            .filter(|id| !items.contains_key(id))
            .collect();
    }
    let mut layout = Value::Null;
    let owned = array(&owner["mapping_ids"])
        .iter()
        .map(string)
        .collect::<BTreeSet<_>>();
    if !owner.is_null() {
        let id = string(&owner["layout"]);
        let mut definition = definitions::layout_get(store, id)?["definition"].clone();
        if let Some(binding) = rows(
            &store.db,
            "SELECT * FROM projection_bindings WHERE profile=? AND catalog=?",
            &[profile.clone().into(), catalog.to_owned().into()],
        )?
        .first()
        {
            if binding["layout"] != id {
                return fail(
                    "projection is configured with a different layout; execute that projection explicitly",
                );
            }
            definition["selection"] = json!({"query_id":binding["query_id"],"profile":profile});
        }
        let hash = sha256(encode(&definition));
        layout = json!({"name":id,"current_definition":definition,"current_definition_sha256":hash,"last_applied_definition_sha256":owner["definition_sha256"],"definition_matches_last_apply":hash==owner["definition_sha256"]});
    }
    let mut entries = vec![];
    for row in &mappings {
        let source = &files[string(&row["file_id"])]["source_path"];
        let output = output_root.map(|root| join(root, string(&row["path"])));
        let expected = if mode == "symlink" {
            source
                .as_str()
                .zip(output.as_deref())
                .map(|(s, o)| relative(s, o))
        } else {
            None
        };
        let recorded_target = recorded
            .get(string(&row["path"]))
            .copied()
            .cloned()
            .unwrap_or(Value::Null);
        entries.push(json!({"mapping_id":row["id"],"path":row["path"],"output_path":output,"item_id":row["item_id"],"file_id":row["file_id"],"association_ids":by_pair.get(&(string(&row["item_id"]).to_owned(),string(&row["file_id"]).to_owned())).cloned().unwrap_or_default(),"managed_by_layout":if owned.contains(string(&row["id"])){owner["layout"].clone()}else{Value::Null},"expected_link_target":expected,"recorded_link_target":recorded_target,"recorded_link_matches_desired":expected.as_ref().map(|e|recorded_target==*e)}));
    }
    let mut extra = extra.cloned().unwrap_or(json!({}));
    extra[NAMESPACE] = statistics(store, profile, catalog)?;
    let mut content = json!({"database_id":store.database_id,"database_schema":crate::migration::SCHEMA_VERSION,"profile":profile,"catalog":{"id":catalog,"output_root":output_root,"link_mode":mode},"state":"desired_catalog","filesystem_verified":false,"layout":layout,"entries":entries,"items":items.values().collect::<Vec<_>>(),"files":files.values().collect::<Vec<_>>(),"associations":associations,"relationships":relationships.values().collect::<Vec<_>>(),"recorded_links":records,"extra":extra});
    let mut taggings = vec![];
    for (subject, included) in [
        ("item", items.keys().cloned().collect::<BTreeSet<_>>()),
        ("file", files.keys().cloned().collect::<BTreeSet<_>>()),
    ] {
        for mut row in selected_rows(
            store,
            &format!(
                "SELECT * FROM {subject}_tags WHERE {subject}_id IN ({{marks}}) ORDER BY id LIMIT 100001"
            ),
            &included,
            &[],
        )? {
            row["subject_type"] = json!(subject);
            row["subject_id"] = row
                .as_object_mut()
                .unwrap()
                .remove(&format!("{subject}_id"))
                .unwrap();
            row["active"] = json!(row["active"].as_i64() != Some(0));
            taggings.push(row);
            bounded(taggings.len(), "taggings")?;
        }
    }
    let mut tags = BTreeMap::new();
    let mut names = vec![];
    let mut parents = vec![];
    let mut frontier = taggings
        .iter()
        .map(|r| string(&r["tag_id"]).to_owned())
        .collect::<BTreeSet<_>>();
    while !frontier.is_empty() {
        bounded(tags.len() + frontier.len(), "tag closure")?;
        for row in selected_rows(
            store,
            "SELECT * FROM tags WHERE id IN ({marks})",
            &frontier,
            &[],
        )? {
            tags.insert(string(&row["id"]).to_owned(), row);
        }
        names.extend(selected_rows(
            store,
            "SELECT * FROM tag_names WHERE tag_id IN ({marks}) LIMIT 100001",
            &frontier,
            &[],
        )?);
        let edges = selected_rows(
            store,
            "SELECT * FROM tag_parents WHERE child_id IN ({marks}) LIMIT 100001",
            &frontier,
            &[],
        )?;
        let next = edges
            .iter()
            .map(|r| string(&r["parent_id"]).to_owned())
            .collect::<BTreeSet<_>>();
        parents.extend(edges);
        bounded(names.len(), "tag names")?;
        bounded(parents.len(), "tag parents")?;
        frontier = next
            .into_iter()
            .filter(|id| !tags.contains_key(id))
            .collect();
    }
    names.sort_by(|a, b| string(&a["name"]).cmp(string(&b["name"])));
    parents.sort_by(|a, b| {
        (string(&a["child_id"]), string(&a["parent_id"]))
            .cmp(&(string(&b["child_id"]), string(&b["parent_id"])))
    });
    taggings.sort_by(|a, b| string(&a["id"]).cmp(string(&b["id"])));
    content["tags"] = json!(tags.values().collect::<Vec<_>>());
    content["tag_names"] = json!(names);
    content["tag_parents"] = json!(parents);
    content["taggings"] = json!(taggings);
    for (table, key) in [
        ("owned_hardlinks", "hardlinks"),
        ("retained_hardlinks", "retained_hardlinks"),
    ] {
        let records = rows(
            &store.db,
            &format!("SELECT * FROM {table} WHERE profile=? AND catalog=? ORDER BY path LIMIT ?"),
            &[
                profile.clone().into(),
                catalog.to_owned().into(),
                (MAX_RECORDS as i64 + 1).into(),
            ],
        )?;
        bounded(records.len(), key)?;
        let mut result = vec![];
        for record in records {
            if selected.is_some() && !included_paths.contains(string(&record["path"])) {
                continue;
            }
            let target: Value = catabolic_core::decode_json(string(&record["target"]))?;
            let mut entry = json!({"path":record["path"]});
            for k in ["file_id", "location", "source_path"] {
                entry[k] = target[k].clone();
            }
            for k in ["device", "inode"] {
                entry[k] = json!(target[k].to_string());
            }
            if table == "retained_hardlinks" {
                for k in ["id", "original_path", "retained_at"] {
                    entry[k] = record[k].clone();
                }
            }
            result.push(entry);
        }
        content[key] = json!(result);
    }
    let mut counts = json!({});
    for key in [
        "entries",
        "items",
        "files",
        "associations",
        "relationships",
        "recorded_links",
        "tags",
        "tag_names",
        "tag_parents",
        "taggings",
        "hardlinks",
        "retained_hardlinks",
    ] {
        counts[key] = json!(array(&content[key]).len());
    }
    content["counts"] = counts;
    let document = json!({"format":"catabolic.catalog-manifest","format_version":3,"generator":{"name":"catabolic","version":env!("CARGO_PKG_VERSION")},"generated_at":crate::mutation::timestamp()?,"content_sha256":sha256(encode(&content)),"content":content});
    if encode(&document).len() > 128 * 1024 * 1024 {
        return fail("manifest exceeds the 128 MiB output limit; nothing was exported");
    }
    interchange::validate(&document)
}
