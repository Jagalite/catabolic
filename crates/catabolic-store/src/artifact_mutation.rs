// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Durable generated-output publication. Validated bytes, an immutable recipe,
//! an exclusive output name and a live execution claim meet at one commit.
use crate::{
    Error, Result, Store, components, filesystem_mutation as fs, media_mutation as claims,
    migration::rows, mutation, observation, output_mutation as outputs, source,
};
use catabolic_core::{encode, sha256};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::{File, Metadata},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{FileExt, MetadataExt},
    },
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// Crash-injection hooks are compiled only into debug/test builds. They never
/// alter publication semantics in release binaries.
fn fault(stage: &str) -> Result<()> {
    #[cfg(debug_assertions)]
    {
        if std::env::var("CATABOLIC_TEST_ARTIFACT_CRASH_AT").as_deref() == Ok(stage) {
            std::process::exit(86);
        }
        if std::env::var("CATABOLIC_TEST_ARTIFACT_FAILPOINT").as_deref() == Ok(stage) {
            return Err(Error(format!("injected artifact failure at {stage}")));
        }
    }
    let _ = stage;
    Ok(())
}

fn name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
    {
        return Err(Error("invalid name".into()));
    }
    Ok(())
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .ok_or_else(|| Error(format!("missing artifact {key}")))
}
fn owner(store: &Store, profile: &str, location: &str) -> Value {
    json!({"database_id":store.database_id,"profile":profile,"location":location,"kind":"generated","version":1})
}
fn strict_root(binding: &Value) -> Result<File> {
    let root = fs::open_root(text(binding, "root")?)?;
    let st = root.metadata()?;
    if binding["device"].as_u64() != Some(st.dev()) || binding["inode"].as_u64() != Some(st.ino()) {
        return Err(Error("generated root identity changed".into()));
    }
    Ok(root)
}
pub(crate) fn owned_root(store: &Store, profile: &str, binding: &Value) -> Result<File> {
    let location = text(binding, "owner")?;
    if observation::binding(store, profile, "source", location)? != *binding {
        return Err(Error("generated binding changed since enqueue".into()));
    }
    let root = strict_root(binding)?;
    let policy = observation::trust_policy(store, profile, location)?;
    if ["uuid", "device", "inode"]
        .iter()
        .any(|key| policy["settings"][key] == "skip")
    {
        return Err(Error(
            "generated artifact locations require strict identity".into(),
        ));
    }
    // Include enrolled volume UUID checks, not just the serialized binding's
    // device/inode. The newly opened descriptor must still name the same root.
    let (checked, _) = observation::checked_root(store, profile, binding)?;
    if fs::identity(&root.metadata()?) != fs::identity(&checked.metadata()?) {
        return Err(Error(
            "generated root changed during identity verification".into(),
        ));
    }
    if fs::owner_state(&root, &owner(store, profile, location))? != "owned" {
        return Err(Error("generated location ownership is missing".into()));
    }
    Ok(root)
}
pub fn bind(store: &Store, profile: &str, location: &str, path: &str) -> Result<Value> {
    mutation::require_recovered(&store.db)?;
    mutation::profile(store, profile)?;
    name(location)?;
    let canonical = Path::new(path).canonicalize()?;
    let path = canonical
        .to_str()
        .ok_or_else(|| Error("paths must be valid UTF-8".into()))?;
    if store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM generated_locations WHERE profile=? AND location=?)",
        params![profile, location],
        |r| r.get::<_, bool>(0),
    )? {
        let binding = observation::binding(store, profile, "source", location)?;
        if binding["root"] != path {
            return Err(Error(
                "generated locations cannot be relocated in this version".into(),
            ));
        }
        owned_root(store, profile, &binding)?;
        return Ok(binding);
    }
    if store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM locations WHERE id=?)",
        [location],
        |r| r.get::<_, bool>(0),
    )? {
        return Err(Error(
            "use a new location; existing media locations cannot be adopted for writing".into(),
        ));
    }
    // Reuse all source/output overlap and database containment admission rules.
    store.transaction(|db| {
        let binding = observation::execute(
            store,
            profile,
            "bind",
            &json!({"kind":"source","source":location,"root":path}),
        )?;
        let root = strict_root(&binding)?;
        fs::claim_output(
            &root,
            &owner(store, profile, location),
            &uuid::Uuid::new_v4().to_string(),
        )?;
        let current = strict_root(&binding)?;
        if fs::identity(&current.metadata()?) != fs::identity(&root.metadata()?) {
            return Err(Error("generated root changed during binding".into()));
        }
        db.execute(
            "INSERT INTO generated_locations VALUES (?,?)",
            params![profile, location],
        )?;
        Ok(binding)
    })
}
pub fn recipe(
    store: &Store,
    recipe_name: &str,
    definition: &Value,
    default_role: &str,
    output_id: Option<&str>,
) -> Result<Value> {
    name(recipe_name)?;
    let preset = text(definition, "preset")?;
    let output = match output_id {
        Some(id) => outputs::get_definition(store, id)?,
        None => outputs::define(
            store,
            &format!("builtin-{preset}"),
            &json!({"role":default_role}),
        )?,
    };
    let mut definition = definition.clone();
    definition["output_definition_id"] = output["id"].clone();
    let serialized = encode(&definition);
    let digest = sha256(&serialized);
    let id=store.transaction(|db| {
        if let Some(id)=db.query_row("SELECT id FROM processing_recipes WHERE name=? AND digest=?",params![recipe_name,digest],|r|r.get::<_,String>(0)).optional()? {return Ok(id);}
        let id=uuid::Uuid::new_v4().to_string(); let revision:i64=db.query_row("SELECT coalesce(max(revision),0)+1 FROM processing_recipes WHERE name=?",[recipe_name],|r|r.get(0))?;
        db.execute("INSERT INTO processing_recipes(id,name,revision,preset,definition,digest,output_definition_id,operation_kind) VALUES (?,?,?,?,?,?,?,'render')",params![id,recipe_name,revision,preset,serialized,digest,output["id"].as_str()])?; Ok(id)
    })?;
    get_recipe(store, &id)
}
pub fn get_recipe(store: &Store, id: &str) -> Result<Value> {
    let value = claims::operation_get(store, id)?;
    if value["operation_kind"] != "render" {
        return Err(Error(
            "artifact execution requires a render operation".into(),
        ));
    }
    Ok(value)
}
pub fn get(store: &Store, profile: &str, id: &str) -> Result<Value> {
    let mut v = rows(
        &store.db,
        "SELECT * FROM processing_artifacts WHERE id=? AND profile=?",
        &[id.to_owned().into(), profile.to_owned().into()],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error("unknown artifact".into()))?;
    for k in ["binding", "validation", "publication_snapshot"] {
        if let Some(raw) = v[k].as_str() {
            v[k] = catabolic_core::decode_json(raw)?;
        }
    }
    Ok(v)
}
fn supported(options: &Value) -> Result<()> {
    if options["recipe"]["preset"] == "component-mux" && options.get("component_package").is_none()
    {
        return Err(Error(
            "component-mux requires an explicitly selected package".into(),
        ));
    }
    if options
        .get("source_lineage_mode")
        .is_some_and(|v| v != "accepted_source_revision")
    {
        return Err(Error("invalid source lineage mode".into()));
    }
    Ok(())
}
pub fn enqueue(
    store: &mut Store,
    profile: &str,
    file: &str,
    recipe: &str,
    location: &str,
    item: &str,
    tools: &Value,
) -> Result<Value> {
    enqueue_package(store, profile, file, recipe, location, item, tools, None)
}
struct PreparedRender {
    profile: String,
    file: String,
    recipe_id: String,
    snapshot: Value,
    options: Value,
    key: String,
}
#[allow(clippy::too_many_arguments)]
fn prepare_enqueue(
    store: &Store,
    profile: &str,
    file: &str,
    recipe_id: &str,
    location: &str,
    item: &str,
    tools: &Value,
    package: Option<&Value>,
    lineage: Option<&str>,
) -> Result<PreparedRender> {
    mutation::require_recovered(&store.db)?;
    mutation::profile(store, profile)?;
    let recipe = get_recipe(store, recipe_id)?;
    let output = outputs::get_definition(store, text(&recipe, "output_definition_id")?)?;
    if lineage
        .is_some_and(|v| !["current_source_revision", "accepted_source_revision"].contains(&v))
    {
        return Err(Error("invalid source lineage mode".into()));
    }
    let lineage = if lineage == Some("accepted_source_revision")
        && store.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM media_outputs WHERE profile=? AND file_id=?)",
            params![profile, file],
            |r| r.get::<_, bool>(0),
        )? {
        lineage
    } else {
        None
    };
    outputs::validate_source_item_mode(store, profile, file, item, &output["definition"], lineage)?;
    if !store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM generated_locations WHERE profile=? AND location=?)",
        params![profile, location],
        |r| r.get::<_, bool>(0),
    )? {
        return Err(Error(
            "destination must be an explicitly bound generated location".into(),
        ));
    }
    let binding = observation::binding(store, profile, "source", location)?;
    owned_root(store, profile, &binding)?;
    let mut snapshot = components::occurrence(&store.db, profile, file)?;
    snapshot["ctime_ns"] = source::validate(&snapshot)?["ctime_ns"].clone();
    let mut options = json!({"recipe":recipe["definition"],"tools":tools,"destination":binding,"item_id":item,"output_definition":output});
    if let Some(package) = package {
        crate::component_mutation::validate_package(store, profile, package, Some(&recipe), false)?;
        options["component_package"] = package.clone();
    }
    if let Some(mode) = lineage {
        options["source_lineage_mode"] = json!(mode);
    }
    supported(&options)?;
    let key = sha256(encode(&json!([snapshot, recipe["digest"], options])));
    Ok(PreparedRender {
        profile: profile.into(),
        file: file.into(),
        recipe_id: recipe_id.into(),
        snapshot,
        options,
        key,
    })
}
fn reusable_job(
    store: &Store,
    prepared: &PreparedRender,
) -> Result<Option<(String, Option<Value>)>> {
    let old=store.db.query_row("SELECT id,state FROM processing_jobs WHERE profile=? AND operation='render' AND cache_key=? ORDER BY created_at DESC,id DESC LIMIT 1",params![prepared.profile,prepared.key],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()?;
    if let Some((id, state)) = old {
        if matches!(state.as_str(), "queued" | "running") {
            return Ok(Some((id, None)));
        }
        if state == "complete" {
            let artifact = store
                .db
                .query_row(
                    "SELECT id FROM processing_artifacts WHERE job_id=? AND state='ready'",
                    [&id],
                    |r| r.get::<_, String>(0),
                )
                .optional()?;
            if let Some(artifact) = artifact {
                return Ok(Some((id, Some(get(store, &prepared.profile, &artifact)?))));
            }
        }
    }
    Ok(None)
}
fn create_job(store: &Store, prepared: PreparedRender) -> Result<Value> {
    let PreparedRender {
        profile,
        file,
        recipe_id,
        snapshot,
        options,
        key,
    } = prepared;
    let id = uuid::Uuid::new_v4().to_string();
    store.transaction(|db| {
        db.execute("INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,recipe_id) VALUES (?,?,?,'render',?,?,?,?,?)",params![id,profile,file,snapshot["location"].as_str(),encode(&snapshot),encode(&options),key,recipe_id])?;
        if let Some(package)=options.get("component_package") { for (ordinal,entry) in package["components"].as_array().unwrap().iter().enumerate() {
            db.execute("INSERT INTO component_job_inputs VALUES (?,?,?,?)",params![id,ordinal as i64,entry["occurrence"]["occurrence_id"].as_str(),encode(entry)])?;
        }}
        Ok(())
    })?;
    Ok(json!({"job_id":id,"reused":false}))
}
#[allow(clippy::too_many_arguments)]
pub fn enqueue_package(
    store: &mut Store,
    profile: &str,
    file: &str,
    recipe_id: &str,
    location: &str,
    item: &str,
    tools: &Value,
    package: Option<&Value>,
) -> Result<Value> {
    let prepared = prepare_enqueue(
        store, profile, file, recipe_id, location, item, tools, package, None,
    )?;
    enqueue_prepared(store, profile, prepared)
}
#[allow(clippy::too_many_arguments)]
pub fn enqueue_lineage(
    store: &mut Store,
    profile: &str,
    file: &str,
    recipe_id: &str,
    location: &str,
    item: &str,
    tools: &Value,
    lineage: &str,
) -> Result<Value> {
    let prepared = prepare_enqueue(
        store,
        profile,
        file,
        recipe_id,
        location,
        item,
        tools,
        None,
        Some(lineage),
    )?;
    enqueue_prepared(store, profile, prepared)
}
/// Persistent public facade admission. Reuses the exact durable preparation,
/// immutable package validation, and cache publication checks used by workers.
#[allow(clippy::too_many_arguments)]
pub fn enqueue_facade(
    store: &mut Store,
    profile: &str,
    file: &str,
    recipe_id: &str,
    location: &str,
    item: &str,
    capabilities: &Value,
    full_cache_check: bool,
    lineage: Option<&str>,
    package: Option<&Value>,
) -> Result<Value> {
    // Frozen facade admits historical derivatives only for this explicit mode;
    // all other values keep ordinary source admission and cache identity.
    let lineage = lineage.filter(|mode| *mode == "accepted_source_revision");
    let prepared = prepare_enqueue(
        store,
        profile,
        file,
        recipe_id,
        location,
        item,
        &capabilities["tools"],
        package,
        lineage,
    )?;
    let capability = capabilities["presets"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["name"] == prepared.options["recipe"]["preset"])
        })
        .ok_or_else(|| Error("unknown render preset".into()))?;
    if capability["available"] != true {
        let missing = capability["missing"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        return Err(Error(format!(
            "the installed FFmpeg build lacks required encoders, muxers or filters{}",
            if missing.is_empty() {
                String::new()
            } else {
                format!(": {missing}")
            }
        )));
    }
    if let Some((id, artifact)) = reusable_job(store, &prepared)? {
        if let Some(artifact) = artifact {
            let usable = if full_cache_check {
                usable(store, profile, &artifact)?
            } else {
                usable_metadata(store, profile, &artifact)?
            };
            if crate::embedding::poll_interrupt() {
                return Err(Error("artifact admission cancelled".into()));
            }
            if usable {
                return Ok(json!({"job_id":id,"artifact_id":artifact["id"],"reused":true}));
            }
        } else {
            return Ok(json!({"job_id":id,"reused":true}));
        }
    }
    if crate::embedding::poll_interrupt() {
        return Err(Error("artifact admission cancelled".into()));
    }
    create_job(store, prepared)
}
fn enqueue_prepared(store: &mut Store, profile: &str, prepared: PreparedRender) -> Result<Value> {
    if let Some((id, artifact)) = reusable_job(store, &prepared)? {
        if let Some(artifact) = artifact {
            if usable(store, profile, &artifact)? {
                return Ok(json!({"job_id":id,"artifact_id":artifact["id"],"reused":true}));
            }
        } else {
            return Ok(json!({"job_id":id,"reused":true}));
        }
    }
    create_job(store, prepared)
}
/// Request admission runs in the caller's existing transaction. Capabilities
/// are supplied by the supervised worker; cache verification is metadata-only,
/// matching the frozen demand contract and never detaching an active writer.
#[allow(clippy::too_many_arguments)]
pub fn enqueue_request(
    store: &Store,
    profile: &str,
    file: &str,
    recipe_id: &str,
    location: &str,
    item: &str,
    capabilities: &Value,
    lineage: Option<&str>,
) -> Result<Value> {
    let recipe = get_recipe(store, recipe_id)?;
    let capability = capabilities["presets"]
        .as_array()
        .and_then(|v| v.iter().find(|v| v["name"] == recipe["preset"]))
        .ok_or_else(|| Error("unknown render preset".into()))?;
    if capability["available"] != true {
        return Err(Error(
            "the installed FFmpeg build lacks required encoders, muxers or filters".into(),
        ));
    }
    let prepared = prepare_enqueue(
        store,
        profile,
        file,
        recipe_id,
        location,
        item,
        &capabilities["tools"],
        None,
        lineage,
    )?;
    if let Some((id, artifact)) = reusable_job(store, &prepared)? {
        if let Some(artifact) = artifact {
            if usable_metadata(store, profile, &artifact)? {
                return Ok(json!({"job_id":id,"artifact_id":artifact["id"],"reused":true}));
            }
        } else {
            return Ok(json!({"job_id":id,"reused":true}));
        }
    }
    create_job(store, prepared)
}
fn claim_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<claims::Claim> {
    Ok(claims::Claim {
        job_id: row.get(0)?,
        generation: row.get(1)?,
        token: row.get(2)?,
        worker_pid: row.get(3)?,
        attempt: row.get(4)?,
        lease_until: row.get(5)?,
        snapshot: row.get(6)?,
        options: row.get(7)?,
    })
}
pub fn active(store: &Store, id: &str) -> Result<bool> {
    Ok(store.db.query_row("SELECT job_id,generation,token,worker_pid,attempt,lease_until,snapshot,options FROM execution_claims WHERE job_id=?",[id],claim_row).optional()?.is_some_and(|c|claims::claim_live(&c,fs::now())))
}
fn acquire(db: &Connection, job: &Value, attempt: i64) -> Result<claims::Claim> {
    let id = text(job, "id")?;
    let previous = job["attempts"]
        .as_i64()
        .ok_or_else(|| Error("invalid processing attempts".into()))?;
    let recover = if attempt == previous {
        Some(attempt)
    } else if previous.checked_add(1) == Some(attempt) {
        None
    } else {
        return Err(Error("invalid artifact attempt transition".into()));
    };
    let claim = claims::acquire(db, id, fs::now(), recover)?;
    if claim.attempt != attempt {
        return Err(Error(
            "artifact attempt changed during claim acquisition".into(),
        ));
    }
    Ok(claim)
}

pub fn start(
    store: &Store,
    profile: &str,
    id: &str,
    extension: &str,
) -> Result<(claims::Claim, Value)> {
    mutation::require_recovered(&store.db)?;
    name(extension)?;
    let job = claims::get_job(store, profile, id)?;
    supported(&job["options"])?;
    if job["options"].get("source_lineage_mode").is_some()
        && !store.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM media_outputs WHERE profile=? AND file_id=?)",
            params![profile, text(&job, "file_id")?],
            |r| r.get::<_, bool>(0),
        )?
    {
        return Err(Error(
            "historical lineage requires a recorded derivative".into(),
        ));
    }
    if let Some(package) = job["options"].get("component_package") {
        let recipe = get_recipe(store, text(&job, "recipe_id")?)?;
        crate::component_mutation::validate_package(store, profile, package, Some(&recipe), false)?;
    }
    if job["operation"] != "render" || job["state"] != "queued" {
        return Err(Error("only queued render jobs can be started".into()));
    }
    let options = &job["options"];
    outputs::validate_source_item_mode(
        store,
        profile,
        text(&job, "file_id")?,
        text(options, "item_id")?,
        &options["output_definition"]["definition"],
        options["source_lineage_mode"].as_str(),
    )?;
    owned_root(store, profile, &options["destination"])?;
    let id_artifact = uuid::Uuid::new_v4().to_string();
    let attempt = job["attempts"]
        .as_i64()
        .and_then(|v| v.checked_add(1))
        .ok_or_else(|| Error("processing attempt overflow".into()))?;
    let claim=store.transaction(|db| {
        if db.query_row("SELECT EXISTS(SELECT 1 FROM processing_artifacts WHERE job_id=? AND state IN ('planned','writing','validating','publishing'))",[id],|r|r.get::<_,bool>(0))? {return Err(Error("recover pending artifacts before running this job".into()));}
        let claim=acquire(db,&job,attempt)?;
        db.execute("INSERT INTO processing_attempts(job_id,attempt,state,retryable,retry_after,started_at) VALUES (?,?,'running',0,0,CURRENT_TIMESTAMP)",params![id,attempt])?;
        db.execute("UPDATE processing_jobs SET state='running',attempts=?,error=NULL,finished_at=NULL WHERE id=?",params![attempt,id])?;
        db.execute("INSERT INTO processing_artifacts(id,job_id,attempt,profile,location,path,temporary_path,binding,state,item_id,role) VALUES (?,?,?,?,?,?,?,?,'planned',?,?)",params![id_artifact,id,attempt,profile,options["destination"]["owner"].as_str(),format!("{id_artifact}.{extension}"),format!(".{id_artifact}.partial"),encode(&options["destination"]),options["item_id"].as_str(),options["output_definition"]["definition"]["role"].as_str()])?;
        Ok(claim)
    })?;
    fault("after_planned")?;
    Ok((claim, get(store, profile, &id_artifact)?))
}
pub fn create_temporary(
    store: &Store,
    profile: &str,
    claim: &claims::Claim,
    artifact: &Value,
) -> Result<(File, File)> {
    claims::fence(store, claim, fs::now())?;
    if artifact["job_id"] != claim.job_id
        || artifact["attempt"] != claim.attempt
        || artifact["state"] != "planned"
    {
        return Err(Error(
            "artifact does not belong to current execution claim".into(),
        ));
    }
    let root = owned_root(store, profile, &artifact["binding"])?;
    let leaf = text(artifact, "temporary_path")?;
    crate::layout::relative_path(leaf)?;
    if leaf.contains('/') {
        return Err(Error(
            "temporary artifact must be a root-relative leaf".into(),
        ));
    }
    let name = CString::new(leaf).map_err(|_| Error("invalid temporary path".into()))?;
    // SAFETY: descriptor/name are live; exclusive no-follow creation cannot adopt another file.
    let fd = unsafe {
        libc::openat(
            root.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: openat returned a fresh owned descriptor.
    let file = unsafe { File::from_raw_fd(fd) };
    let st = file.metadata()?;
    store.transaction(|db| {claims::fence(store,claim,fs::now())?;db.execute("UPDATE processing_artifacts SET state='writing',device=?,inode=? WHERE id=? AND state='planned'",params![st.dev() as i64,st.ino() as i64,artifact["id"].as_str()])?;Ok(())})?;
    fault("after_temporary")?;
    Ok((root, file))
}
/// The worker releases all catalog locks while waiting on tools and checksums.
/// Heartbeats use short owned sessions; any lost claim cancels the external work.
pub fn detached<T>(
    store: &mut Store,
    claim: &claims::Claim,
    operation: impl FnOnce(&AtomicBool) -> Result<T>,
) -> Result<T> {
    claims::fence(store, claim, fs::now())?;
    let path = store.path.clone();
    let identity = store.database_id.clone();
    let claim_copy = claim.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let lost = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let thread_lost = lost.clone();
    struct Guard {
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }
    let result = store.detached(|| {
        let thread = std::thread::spawn(move || {
            let mut last = Instant::now();
            while !thread_stop.load(Ordering::Relaxed) {
                for _ in 0..50 {
                    if thread_stop.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                if let Ok(s) = Store::open(&path, true, false) {
                    if s.database_id != identity
                        || claims::heartbeat(&s, &claim_copy, fs::now()).is_err()
                    {
                        thread_lost.store(true, Ordering::Relaxed);
                        return;
                    }
                    last = Instant::now();
                }
                if last.elapsed() > Duration::from_secs(45) {
                    thread_lost.store(true, Ordering::Relaxed);
                    return;
                }
            }
        });
        let _guard = Guard {
            stop: stop.clone(),
            thread: Some(thread),
        };
        operation(&lost)
    });
    claims::fence(store, claim, fs::now())?;
    if lost.load(Ordering::Relaxed) {
        return Err(Error("artifact execution claim was lost".into()));
    }
    result
}
#[allow(clippy::too_many_arguments)]
pub fn intent(
    store: &Store,
    profile: &str,
    claim: &claims::Claim,
    id: &str,
    size: u64,
    digest: &str,
    validation: &Value,
    output_revision: &Value,
) -> Result<()> {
    if size == 0
        || size > i64::MAX as u64
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || !validation["output"].is_object()
    {
        return Err(Error("invalid validated artifact evidence".into()));
    }
    let job = claims::get_job(store, profile, &claim.job_id)?;
    if let Some(package) = job["options"].get("component_package") {
        crate::component_mutation::validate_package(store, profile, package, None, true)?;
    }
    let current = components::occurrence(&store.db, profile, text(&job, "file_id")?)?;
    if job["snapshot"]
        .as_object()
        .ok_or_else(|| Error("invalid source snapshot".into()))?
        .iter()
        .any(|(k, v)| k != "ctime_ns" && current.get(k) != Some(v))
    {
        return Err(Error(
            "source or policy changed before publication; enqueue a new job".into(),
        ));
    }
    source::validate(&job["snapshot"])?;
    let artifact = get(store, profile, id)?;
    let root = owned_root(store, profile, &artifact["binding"])?;
    let (_, revision) = fs::stat_at(&root, text(&artifact, "temporary_path")?)?;
    if revision != *output_revision
        || revision["size"] != size
        || revision["device"] != artifact["device"]
        || revision["inode"] != artifact["inode"]
    {
        return Err(Error("artifact changed after output validation".into()));
    }
    store.transaction(|db| {claims::fence(store,claim,fs::now())?;
        if db.execute("UPDATE processing_artifacts SET state='publishing',size=?,sha256=?,validation=? WHERE id=? AND job_id=? AND attempt=? AND state IN ('writing','validating')",params![size as i64,digest,encode(validation),id,claim.job_id,claim.attempt])?!=1 {return Err(Error("artifact does not belong to the current execution claim".into()));}Ok(())})?;
    fault("after_intent")
}
fn check(artifact: &Value, root: &File, leaf: &str, cancel: &AtomicBool) -> Result<Metadata> {
    crate::layout::relative_path(leaf)?;
    if leaf.contains('/') {
        return Err(Error("artifact path must be a root-relative leaf".into()));
    }
    let file = fs::open_at(root, leaf, false)?;
    let before = file.metadata()?;
    if !before.is_file()
        || before.dev() != artifact["device"].as_u64().unwrap_or(u64::MAX)
        || before.ino() != artifact["inode"].as_u64().unwrap_or(u64::MAX)
        || before.len() != artifact["size"].as_u64().unwrap_or(0)
    {
        return Err(Error(
            "artifact identity or size changed; refusing publication".into(),
        ));
    }
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut offset = 0;
    while offset < before.len() {
        if cancel.load(Ordering::Relaxed) || crate::embedding::poll_interrupt() {
            return Err(Error("artifact verification cancelled".into()));
        }
        let remaining = (before.len() - offset).min(buffer.len() as u64) as usize;
        let n = file.read_at(&mut buffer[..remaining], offset)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
        offset += n as u64;
    }
    if format!("{:x}", digest.finalize()) != text(artifact, "sha256")? {
        return Err(Error(
            "artifact checksum changed; refusing publication".into(),
        ));
    }
    let (_, named) = fs::stat_at(root, leaf)?;
    if fs::version(&before) != fs::version(&file.metadata()?) || fs::version(&before) != named {
        return Err(Error(
            "artifact changed while verifying its checksum".into(),
        ));
    }
    Ok(before)
}
/// Read-only revision/ownership check for access snapshots. It does not hash
/// bytes, acquire a writer session, or detach the caller's SQLite snapshot.
pub fn usable_metadata(store: &Store, profile: &str, artifact: &Value) -> Result<bool> {
    let result: Result<()> = (|| {
        if !artifact["publication_snapshot"].is_object() {
            return Err(Error("artifact has no publication snapshot".into()));
        }
        let root = owned_root(store, profile, &artifact["binding"])?;
        let revision = source::validate(&artifact["publication_snapshot"])?;
        let (st, named) = fs::stat_at(&root, text(artifact, "path")?)?;
        if u64::from(st) & u64::from(libc::S_IFMT) != u64::from(libc::S_IFREG)
            || named != revision
            || named["size"] != artifact["size"]
            || named["device"] != artifact["device"]
            || named["inode"] != artifact["inode"]
        {
            return Err(Error("artifact publication revision changed".into()));
        }
        Ok(())
    })();
    Ok(result.is_ok())
}
pub fn usable(store: &mut Store, profile: &str, artifact: &Value) -> Result<bool> {
    let result: Result<()> = (|| {
        let root = owned_root(store, profile, &artifact["binding"])?;
        let st = store.detached(|| {
            check(
                artifact,
                &root,
                text(artifact, "path")?,
                &AtomicBool::new(false),
            )
        })?;
        let current = owned_root(store, profile, &artifact["binding"])?;
        if fs::stat_at(&current, text(artifact, "path")?)?.1 != fs::version(&st) {
            return Err(Error("artifact changed after verification".into()));
        }
        Ok(())
    })();
    Ok(result.is_ok())
}
pub(crate) fn rename_noreplace(root: &File, from: &str, to: &str) -> Result<()> {
    let from = CString::new(from).map_err(|_| Error("invalid artifact path".into()))?;
    let to = CString::new(to).map_err(|_| Error("invalid artifact path".into()))?;
    #[cfg(target_os = "linux")]
    // SAFETY: valid live descriptors and names; kernel RENAME_NOREPLACE forbids replacement.
    let rc = unsafe {
        libc::renameat2(
            root.as_raw_fd(),
            from.as_ptr(),
            root.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    // SAFETY: valid live descriptors and names; RENAME_EXCL forbids replacement.
    let rc = unsafe {
        libc::renameatx_np(
            root.as_raw_fd(),
            from.as_ptr(),
            root.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return Err(Error(
        "exclusive rename is unavailable; publication refused".into(),
    ));
    if rc != 0 {
        return Err(Error(format!(
            "exclusive artifact publication refused: {}; destination was not overwritten",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}
fn finish(
    db: &Connection,
    artifact: &Value,
    state: &str,
    result: Option<&Value>,
    error: Option<&str>,
) -> Result<()> {
    let data = result.map(encode);
    db.execute("UPDATE processing_attempts SET state=?,result=?,error=?,finished_at=CURRENT_TIMESTAMP WHERE job_id=? AND attempt=?",params![state,data,error,artifact["job_id"].as_str(),artifact["attempt"].as_i64()])?;
    db.execute("UPDATE processing_jobs SET state=?,result=?,error=?,finished_at=CURRENT_TIMESTAMP WHERE id=? AND attempts=? AND state!='cancelled'",params![state,data,error,artifact["job_id"].as_str(),artifact["attempt"].as_i64()])?;
    Ok(())
}
pub fn fail(
    store: &Store,
    profile: &str,
    claim: &claims::Claim,
    id: &str,
    message: &str,
) -> Result<()> {
    let artifact = get(store, profile, id)?;
    let message = message.chars().take(4000).collect::<String>();
    store.transaction(|db| {
        claims::fence(store, claim, fs::now())?;
        if !["publishing", "ready"].contains(&artifact["state"].as_str().unwrap_or("")) {
            db.execute(
                "UPDATE processing_artifacts SET state='failed',error=? WHERE id=?",
                params![message, id],
            )?;
            finish(db, &artifact, "failed", None, Some(&message))?;
        }
        db.execute(
            "UPDATE execution_claims SET lease_until=0 WHERE job_id=? AND token=?",
            params![claim.job_id, claim.token],
        )?;
        Ok(())
    })
}
pub fn publish(store: &mut Store, profile: &str, claim: &claims::Claim, id: &str) -> Result<Value> {
    publish_inner(store, profile, claim, id, None)
}
fn publish_inner(
    store: &mut Store,
    profile: &str,
    claim: &claims::Claim,
    id: &str,
    failpoint: Option<&str>,
) -> Result<Value> {
    claims::fence(store, claim, fs::now())?;
    let artifact = get(store, profile, id)?;
    if artifact["state"] != "publishing"
        || artifact["job_id"] != claim.job_id
        || artifact["attempt"] != claim.attempt
    {
        return Err(Error(
            "artifact publication requires its current execution claim".into(),
        ));
    }
    let root = owned_root(store, profile, &artifact["binding"])?;
    let leaf = text(&artifact, "path")?;
    if fs::state(&root, leaf)?.0 == "absent" {
        let verified = detached(store, claim, |cancel| {
            check(&artifact, &root, text(&artifact, "temporary_path")?, cancel)
        })?;
        owned_root(store, profile, &artifact["binding"])?;
        if fs::stat_at(&root, text(&artifact, "temporary_path")?)?.1 != fs::version(&verified) {
            return Err(Error(
                "artifact changed before exclusive publication".into(),
            ));
        }
        claims::fence(store, claim, fs::now())?;
        fault("before_rename")?;
        if failpoint == Some("before_rename") {
            return Err(Error("injected failure before rename".into()));
        }
        rename_noreplace(&root, text(&artifact, "temporary_path")?, leaf)?;
        fault("after_rename_before_sync")?;
        if failpoint == Some("after_rename_before_sync") {
            return Err(Error(
                "injected failure after rename before directory sync".into(),
            ));
        }
        root.sync_all()?;
        fault("after_rename")?;
        if failpoint == Some("after_rename") {
            return Err(Error("injected failure after rename".into()));
        }
    }
    let st = detached(store, claim, |cancel| check(&artifact, &root, leaf, cancel))?;
    // Recovery may encounter a rename whose directory sync was interrupted.
    root.sync_all()?;
    owned_root(store, profile, &artifact["binding"])?;
    let job = claims::get_job(store, profile, &claim.job_id)?;
    let options = &job["options"];
    supported(options)?;
    if let Some(package) = options.get("component_package") {
        crate::component_mutation::validate_package(store, profile, package, None, true)?;
    }
    outputs::validate_source_item_mode(
        store,
        profile,
        text(&job, "file_id")?,
        text(options, "item_id")?,
        &options["output_definition"]["definition"],
        options["source_lineage_mode"].as_str(),
    )?;
    let database = uuid::Uuid::parse_str(&store.database_id).map_err(|e| Error(e.to_string()))?;
    let file = uuid::Uuid::new_v5(
        &database,
        encode(&json!([artifact["location"], artifact["path"]])).as_bytes(),
    )
    .to_string();
    let scan = uuid::Uuid::new_v4().to_string();
    let mut snapshot = fs::version(&st);
    for (key, v) in [
        ("id", json!(file)),
        ("location", artifact["location"].clone()),
        ("path", artifact["path"].clone()),
        ("root", artifact["binding"]["root"].clone()),
        ("root_device", artifact["binding"]["device"].clone()),
        ("root_inode", artifact["binding"]["inode"].clone()),
        ("status", json!("present")),
    ] {
        snapshot[key] = v;
    }
    store.transaction(|db| {claims::fence(store,claim,fs::now())?;
        if fs::stat_at(&root,leaf)?.1!=fs::version(&st) {return Err(Error("artifact changed before registration".into()));}
        db.execute("INSERT INTO files(id,location,path) VALUES (?,?,?)",params![file,artifact["location"].as_str(),leaf])?;
        db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES (?,?,?,1,1,'[]')",params![scan,profile,artifact["location"].as_str()])?;
        db.execute("INSERT INTO observations(profile,file_id,size,mtime_ns,device,inode,status,scan_id) VALUES (?,?,?,?,?,?,'present',?)",params![profile,file,st.len() as i64,snapshot["mtime_ns"].as_i64(),st.dev() as i64,st.ino() as i64,scan])?;
        let output=outputs::record(db,profile,&file,text(&job,"file_id")?,text(options,"item_id")?,text(&options["output_definition"],"id")?,Some(id),&json!({"artifact_id":id,"generated":true}))?;
        db.execute("UPDATE processing_artifacts SET state='ready',file_id=?,item_id=?,publication_snapshot=?,error=NULL WHERE id=?",params![file,output["item_id"].as_str(),encode(&snapshot),id])?;
        db.execute("INSERT INTO content_baselines VALUES (?,?,?,?,?,?)",params![profile,file,"sha256",artifact["sha256"].as_str(),st.len() as i64,claim.job_id])?;
        let mut fact_snapshot=components::occurrence(db,profile,&file)?;fact_snapshot["ctime_ns"]=snapshot["ctime_ns"].clone();
        db.execute("INSERT INTO file_facts VALUES (?,?,?,?,?,?,?)",params![profile,file,"probe",claim.job_id,encode(&fact_snapshot),encode(&artifact["validation"]["output"]),"complete"])?;
        let result=json!({"artifact_id":id,"file_id":file,"output_id":output["output_id"],"item_id":output["item_id"],"validation":artifact["validation"]});
        finish(db,&artifact,"complete",Some(&result),None)?;components::index_file(db,profile,&file,false)?;
        if let Some(package)=options.get("component_package"){crate::component_mutation::record_lineage(db,&artifact,&artifact["validation"],package)?;}
        db.execute("UPDATE execution_claims SET lease_until=0 WHERE job_id=? AND token=?",params![claim.job_id,claim.token])?;
        db.execute("INSERT INTO catalog_refresh_queue(profile,catalog) SELECT profile,catalog FROM catalog_refresh_settings WHERE profile=? AND enabled=1 AND NOT EXISTS (SELECT 1 FROM watcher_owners w WHERE w.profile=catalog_refresh_settings.profile AND w.catalog=catalog_refresh_settings.catalog) ON CONFLICT(profile,catalog) DO UPDATE SET generation=generation+1,next_attempt=0,error=NULL,updated_at=CURRENT_TIMESTAMP",[profile])?;
        db.execute("UPDATE watchers SET pending_generation=pending_generation+1,event_first=coalesce(event_first,unixepoch('now')),event_last=unixepoch('now') WHERE profile=? AND enabled=1 AND definition_id IN (SELECT id FROM watcher_definitions WHERE json_extract(definition,'$.events')=1)",[profile])?;
        fault("before_register_commit")?;
        if failpoint==Some("before_register_commit"){return Err(Error("injected registration failure".into()));}
        Ok(json!({"job_id":claim.job_id,"artifact_id":id,"file_id":file,"output_id":output["output_id"],"item_id":output["item_id"]}))
    }).and_then(|result| {fault("after_register_commit")?;Ok(result)})
}
pub fn recover(store: &mut Store, profile: &str, limit: usize) -> Result<Value> {
    if !(1..=1000).contains(&limit) {
        return Err(Error("limit must be 1..1000".into()));
    }
    let pending = rows(
        &store.db,
        "SELECT id,job_id FROM processing_artifacts WHERE profile=? AND state IN ('planned','writing','validating','publishing') ORDER BY created_at,id LIMIT ?",
        &[profile.to_owned().into(), (limit as i64).into()],
    )?;
    let mut recovered = vec![];
    let mut errors = vec![];
    for row in pending {
        if crate::embedding::poll_interrupt() {
            return Err(Error("artifact recovery cancelled".into()));
        }
        let id = text(&row, "id")?;
        let job_id = text(&row, "job_id")?;
        if active(store, job_id)? {
            continue;
        }
        let artifact = get(store, profile, id)?;
        if artifact["state"] == "publishing" {
            let result = (|| {
                let job = claims::get_job(store, profile, job_id)?;
                if job["state"] != "running" || job["attempts"] != artifact["attempt"] {
                    return Err(Error(
                        "publication intent no longer matches its job attempt".into(),
                    ));
                }
                let claim = store
                    .transaction(|db| acquire(db, &job, artifact["attempt"].as_i64().unwrap()))?;
                let result = publish(store, profile, &claim, id);
                let released = claims::release(store, &claim);
                result.and_then(|v| released.map(|_| v))
            })();
            match result {
                Ok(_) => recovered.push(json!(id)),
                Err(e) => {
                    errors.push(json!({"id":id,"error":e.0}));
                    store.transaction(|db| {
                        db.execute(
                            "UPDATE processing_artifacts SET error=? WHERE id=?",
                            params![e.0.chars().take(4000).collect::<String>(), id],
                        )?;
                        Ok(())
                    })?;
                }
            }
        } else {
            store.transaction(|db| {if active(store,job_id)? {return Ok(());}let message="interrupted before validated publication; retained partial file for inspection";db.execute("UPDATE processing_artifacts SET state='interrupted',error=? WHERE id=?",params![message,id])?;finish(db,&artifact,"failed",None,Some(message))})?;
            recovered.push(json!(id));
        }
    }
    Ok(json!({"recovered":recovered,"complete":errors.is_empty(),"errors":errors}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    struct Fixture {
        dir: tempfile::TempDir,
        store: Store,
        recipe: String,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let db = dir.path().join("catalog.db");
            crate::migration::initialize(&db).unwrap();
            let store = Store::open(&db, true, false).unwrap();
            let input = dir.path().join("input");
            std::fs::create_dir(&input).unwrap();
            std::fs::write(input.join("input.wav"), b"source evidence").unwrap();
            let output = dir.path().join("output");
            std::fs::create_dir(&output).unwrap();
            let r = input.metadata().unwrap();
            let st = input.join("input.wav").metadata().unwrap();
            store.transaction(|db| {
                db.execute("INSERT INTO locations VALUES ('input')",[])?;
                db.execute("INSERT INTO bindings VALUES ('default','source','input',?,?,?)",params![input.to_str(),r.dev() as i64,r.ino() as i64])?;
                db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','input',1,1,'[]')",[])?;
                db.execute("INSERT INTO files VALUES ('file','input','input.wav')",[])?;
                db.execute("INSERT INTO observations VALUES ('default','file',?,?,?,?, 'present','scan')",params![st.len() as i64,st.mtime()*1_000_000_000+st.mtime_nsec(),st.dev() as i64,st.ino() as i64])?;
                db.execute("INSERT INTO items VALUES ('item','recording','{}')",[])?;
                db.execute("INSERT INTO item_files(id,file_id,item_id,role,origin) VALUES ('association','file','item','primary','explicit')",[])?;Ok(())
            }).unwrap();
            bind(&store, "default", "generated", output.to_str().unwrap()).unwrap();
            let recipe = recipe(
                &store,
                "audio",
                &json!({"version":1,"preset":"audio-flac","timeout":3600,"reserve_bytes":0,"max_output_bytes":1048576}),
                "primary",
                None,
            )
            .unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned();
            Self { dir, store, recipe }
        }
        fn queued(&mut self) -> String {
            enqueue(
                &mut self.store,
                "default",
                "file",
                &self.recipe,
                "generated",
                "item",
                &json!({}),
            )
            .unwrap()["job_id"]
                .as_str()
                .unwrap()
                .into()
        }
        fn prepared(&mut self) -> (claims::Claim, Value) {
            let job = self.queued();
            let (claim, a) = start(&self.store, "default", &job, "flac").unwrap();
            let (root, mut output) = create_temporary(&self.store, "default", &claim, &a).unwrap();
            output.write_all(b"validated generated bytes").unwrap();
            output.sync_all().unwrap();
            root.sync_all().unwrap();
            let revision = fs::version(&output.metadata().unwrap());
            let validation = json!({"output":{"streams":[{"index":0,"codec_type":"audio","codec_name":"flac"}]},"input":{}});
            intent(
                &self.store,
                "default",
                &claim,
                text(&a, "id").unwrap(),
                output.metadata().unwrap().len(),
                &sha256(b"validated generated bytes"),
                &validation,
                &revision,
            )
            .unwrap();
            (
                claim,
                get(&self.store, "default", text(&a, "id").unwrap()).unwrap(),
            )
        }
    }
    #[test]
    fn generated_binding_is_explicit_owned_and_cannot_adopt_or_overlap() {
        let f = Fixture::new();
        let root = f.dir.path().join("output");
        let marker: Value =
            catabolic_core::decode_json_bytes(&std::fs::read(root.join(fs::MARKER)).unwrap())
                .unwrap();
        assert_eq!(marker, owner(&f.store, "default", "generated"));
        assert!(bind(&f.store, "default", "generated", root.to_str().unwrap()).is_ok());
        assert!(bind(&f.store, "default", "input", root.to_str().unwrap()).is_err());
        assert!(bind(&f.store, "default", "overlap", root.to_str().unwrap()).is_err());
        std::fs::write(root.join(fs::MARKER), b"{}").unwrap();
        assert!(bind(&f.store, "default", "generated", root.to_str().unwrap()).is_err());
    }
    #[test]
    fn publication_is_atomic_idempotently_recoverable_before_and_after_rename() {
        for stage in ["before_rename", "after_rename_before_sync", "after_rename"] {
            let mut f = Fixture::new();
            let (claim, a) = f.prepared();
            let id = text(&a, "id").unwrap();
            assert!(publish_inner(&mut f.store, "default", &claim, id, Some(stage)).is_err());
            assert_eq!(get(&f.store, "default", id).unwrap()["state"], "publishing");
            assert!(
                recover(&mut f.store, "default", 100).unwrap()["recovered"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
                "live worker must not be recovered"
            );
            claims::release(&f.store, &claim).unwrap();
            let result = recover(&mut f.store, "default", 100).unwrap();
            assert_eq!(result["complete"], true, "{result}");
            assert_eq!(result["recovered"], json!([id]));
            let artifact = get(&f.store, "default", id).unwrap();
            assert_eq!(artifact["state"], "ready");
            assert!(
                recover(&mut f.store, "default", 100).unwrap()["recovered"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            let counts:(i64,i64,i64)=f.store.db.query_row("SELECT (SELECT count(*) FROM media_outputs),(SELECT count(*) FROM content_baselines),(SELECT count(*) FROM file_facts)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
            assert_eq!(counts, (1, 1, 1));
            assert_eq!(
                claims::get_job(&f.store, "default", &claim.job_id).unwrap()["state"],
                "complete"
            );
            assert_eq!(
                enqueue(
                    &mut f.store,
                    "default",
                    "file",
                    &f.recipe,
                    "generated",
                    "item",
                    &json!({})
                )
                .unwrap()["artifact_id"],
                id
            );
        }
    }
    #[test]
    fn exclusive_publication_never_overwrites_and_changed_bytes_are_not_adopted() {
        for replace in [false, true] {
            let mut f = Fixture::new();
            let (claim, a) = f.prepared();
            let id = text(&a, "id").unwrap();
            let root = f.dir.path().join("output");
            let path = root.join(if replace {
                text(&a, "path").unwrap()
            } else {
                text(&a, "temporary_path").unwrap()
            });
            std::fs::write(&path, b"untrusted conflicting file").unwrap();
            assert!(publish(&mut f.store, "default", &claim, id).is_err());
            assert_eq!(std::fs::read(path).unwrap(), b"untrusted conflicting file");
            assert_eq!(get(&f.store, "default", id).unwrap()["state"], "publishing");
            assert_eq!(
                f.store
                    .db
                    .query_row("SELECT count(*) FROM media_outputs", [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }
    #[test]
    fn stale_claims_cannot_commit_intent_publish_or_fail_successor() {
        let mut f = Fixture::new();
        let (claim, a) = f.prepared();
        let id = text(&a, "id").unwrap();
        claims::release(&f.store, &claim).unwrap();
        let job = claims::get_job(&f.store, "default", &claim.job_id).unwrap();
        let successor = f
            .store
            .transaction(|db| acquire(db, &job, claim.attempt))
            .unwrap();
        assert!(publish(&mut f.store, "default", &claim, id).is_err());
        assert!(fail(&f.store, "default", &claim, id, "stale").is_err());
        assert_eq!(get(&f.store, "default", id).unwrap()["state"], "publishing");
        assert!(publish(&mut f.store, "default", &successor, id).is_ok());
    }
    #[test]
    fn failed_and_cancelled_workers_keep_partial_files_without_publication() {
        let mut f = Fixture::new();
        let job = f.queued();
        let (claim, a) = start(&f.store, "default", &job, "flac").unwrap();
        let (_root, mut out) = create_temporary(&f.store, "default", &claim, &a).unwrap();
        out.write_all(b"partial").unwrap();
        claims::release(&f.store, &claim).unwrap();
        let result = recover(&mut f.store, "default", 100).unwrap();
        assert_eq!(result["complete"], true);
        assert_eq!(
            get(&f.store, "default", text(&a, "id").unwrap()).unwrap()["state"],
            "interrupted"
        );
        assert!(
            f.dir
                .path()
                .join("output")
                .join(text(&a, "temporary_path").unwrap())
                .exists()
        );
        assert_eq!(
            claims::get_job(&f.store, "default", &job).unwrap()["state"],
            "failed"
        );
    }
    #[test]
    fn source_policy_and_validated_revision_changes_fence_intent() {
        let mut f = Fixture::new();
        let job = f.queued();
        let (claim, a) = start(&f.store, "default", &job, "flac").unwrap();
        let (_root, mut out) = create_temporary(&f.store, "default", &claim, &a).unwrap();
        out.write_all(b"valid bytes").unwrap();
        let revision = fs::version(&out.metadata().unwrap());
        out.write_all(b"mutated").unwrap();
        assert!(
            intent(
                &f.store,
                "default",
                &claim,
                text(&a, "id").unwrap(),
                11,
                &sha256(b"valid bytes"),
                &json!({"output":{}}),
                &revision
            )
            .is_err()
        );
        assert_eq!(
            get(&f.store, "default", text(&a, "id").unwrap()).unwrap()["state"],
            "writing"
        );
    }
    #[test]
    fn registration_rollback_retains_recoverable_intent_without_partial_database_facts() {
        let mut f = Fixture::new();
        let (claim, a) = f.prepared();
        let id = text(&a, "id").unwrap();
        assert!(
            publish_inner(
                &mut f.store,
                "default",
                &claim,
                id,
                Some("before_register_commit")
            )
            .unwrap_err()
            .0
            .contains("injected")
        );
        let counts: (i64, i64) = f
            .store
            .db
            .query_row(
                "SELECT (SELECT count(*) FROM files),(SELECT count(*) FROM media_outputs)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(counts, (1, 0));
        assert_eq!(get(&f.store, "default", id).unwrap()["state"], "publishing");
        assert!(
            f.dir
                .path()
                .join("output")
                .join(text(&a, "path").unwrap())
                .exists()
        );
        claims::release(&f.store, &claim).unwrap();
        assert_eq!(
            recover(&mut f.store, "default", 100).unwrap()["complete"],
            true
        );
    }
    #[test]
    fn noncanonical_lineage_and_malformed_package_jobs_are_refused_before_creating_artifacts() {
        for field in ["source_lineage_mode", "component_package"] {
            let mut f = Fixture::new();
            let id = f.queued();
            let mut job = claims::get_job(&f.store, "default", &id).unwrap();
            job["options"][field] = json!("accepted_source_revision");
            f.store
                .db
                .execute(
                    "UPDATE processing_jobs SET options=? WHERE id=?",
                    params![encode(&job["options"]), id],
                )
                .unwrap();
            assert!(start(&f.store, "default", &id, "flac").is_err());
            assert_eq!(
                f.store
                    .db
                    .query_row("SELECT count(*) FROM processing_artifacts", [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }
    #[test]
    fn current_source_and_ownership_are_required_at_publication_boundary() {
        let mut f = Fixture::new();
        let id = f.queued();
        let (claim, a) = start(&f.store, "default", &id, "flac").unwrap();
        let (_root, mut out) = create_temporary(&f.store, "default", &claim, &a).unwrap();
        out.write_all(b"valid bytes").unwrap();
        let revision = fs::version(&out.metadata().unwrap());
        std::fs::write(f.dir.path().join("input/input.wav"), b"different source").unwrap();
        assert!(
            intent(
                &f.store,
                "default",
                &claim,
                text(&a, "id").unwrap(),
                11,
                &sha256(b"valid bytes"),
                &json!({"output":{}}),
                &revision
            )
            .is_err()
        );
        let mut f = Fixture::new();
        let (claim, a) = f.prepared();
        std::fs::write(f.dir.path().join("output").join(fs::MARKER), b"{}").unwrap();
        assert!(publish(&mut f.store, "default", &claim, text(&a, "id").unwrap()).is_err());
        assert_eq!(
            f.store
                .db
                .query_row("SELECT count(*) FROM media_outputs", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    /// Python is an explicitly selected compatibility oracle, never a runtime fallback.
    #[test]
    #[ignore = "requires CATABOLIC_PYTHON_ORACLE and CATABOLIC_PYTHON_REFERENCE"]
    fn python_native_publication_intents_recover_in_both_directions() {
        let python = std::env::var("CATABOLIC_PYTHON_ORACLE").unwrap();
        let reference = std::env::var("CATABOLIC_PYTHON_REFERENCE").unwrap();
        let oracle = |path: &Path, script: &str| -> Result<Value> {
            let output = std::process::Command::new(&python)
                .args(["-c", script, path.to_str().unwrap()])
                .env("PYTHONPATH", &reference)
                .output()?;
            if !output.status.success() {
                return Err(Error(String::from_utf8_lossy(&output.stderr).into_owned()));
            }
            Ok(catabolic_core::decode_json_bytes(&output.stdout)?)
        };
        let mut f = Fixture::new();
        let (claim, a) = f.prepared();
        let id = text(&a, "id").unwrap();
        claims::release(&f.store, &claim).unwrap();
        let path = f.store.path.clone();
        let result = f
            .store
            .detached(|| {
                oracle(
                    &path,
                    r#"
import json,sys
from catabolic.store import Store
from catabolic.app import Application
from catabolic.artifacts import Artifacts
store=Store(sys.argv[1],writable=True)
try: print(json.dumps(Artifacts(Application(store)).recover()))
finally: store.close()
"#,
                )
            })
            .unwrap();
        assert_eq!(result["complete"], true, "{result}");
        assert_eq!(get(&f.store, "default", id).unwrap()["state"], "ready");
        let mut f = Fixture::new();
        let path = f.store.path.clone();
        let result=f.store.detached(||oracle(&path,r#"
import json,sys,os
from unittest.mock import patch
from catabolic.store import Store
from catabolic.app import Application
from catabolic.artifacts import Artifacts
from catabolic.domain import CatabolicError
store=Store(sys.argv[1],writable=True)
try:
    artifacts=Artifacts(Application(store))
    recipe=store.rows("SELECT id FROM processing_recipes")[0]["id"]
    artifacts.enqueue("file",recipe,"generated","item",_capabilities={"presets":[{"name":"audio-flac","available":True}],"tools":{}})
    def render(input_fd,output_fd,*args):
        os.write(output_fd,b"validated generated bytes")
        os.fsync(output_fd)
        return {"output":{"streams":[{"index":0,"codec_type":"audio","codec_name":"flac"}]},"input":{}}
    with patch("catabolic.artifacts.rendering.tools",return_value={}),patch("catabolic.artifacts.rendering.render",side_effect=render),patch.object(artifacts,"_publish",side_effect=CatabolicError("injected publication interruption")):
        result=artifacts.run()
    print(json.dumps({"result":result,"artifacts":store.rows("SELECT id,state FROM processing_artifacts")}))
finally: store.close()
"#)).unwrap();
        assert_eq!(result["artifacts"][0]["state"], "publishing", "{result}");
        let id = result["artifacts"][0]["id"].as_str().unwrap();
        let recovered = recover(&mut f.store, "default", 100).unwrap();
        assert_eq!(recovered["complete"], true, "{recovered}");
        assert_eq!(get(&f.store, "default", id).unwrap()["state"], "ready");
    }
    #[test]
    fn generated_roots_require_enrolled_volume_identity() {
        let mut f = Fixture::new();
        let (claim, a) = f.prepared();
        f.store.transaction(|db|{db.execute("INSERT INTO binding_volumes(profile,kind,owner,volume_uuid) VALUES ('default','source','generated','deliberately-wrong-volume') ON CONFLICT(profile,kind,owner) DO UPDATE SET volume_uuid=excluded.volume_uuid",[])?;Ok(())}).unwrap();
        assert!(
            publish(&mut f.store, "default", &claim, text(&a, "id").unwrap())
                .unwrap_err()
                .0
                .contains("volume identity")
        );
        assert_eq!(
            get(&f.store, "default", text(&a, "id").unwrap()).unwrap()["state"],
            "publishing"
        );
    }
}
