// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Native generated-media orchestration. External transforms run detached from
//! SQLite; only fenced validated bytes acquire a durable publication intent.
use crate::rendering;
use catabolic_store::{
    Error, Result, Store, artifact_mutation as storage, media_mutation as claims,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::File,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub fn recipe(
    store: &Store,
    name: &str,
    preset: &str,
    options: &Value,
    output: Option<&str>,
) -> Result<Value> {
    let definition = rendering::definition(preset, options)?;
    storage::recipe(
        store,
        name,
        &definition,
        rendering::preset(preset)?.role,
        output,
    )
}
pub fn enqueue(
    store: &mut Store,
    profile: &str,
    file: &str,
    recipe: &str,
    location: &str,
    item: &str,
) -> Result<Value> {
    enqueue_package(store, profile, file, recipe, location, item, None)
}
pub fn enqueue_package(
    store: &mut Store,
    profile: &str,
    file: &str,
    recipe: &str,
    location: &str,
    item: &str,
    package: Option<&Value>,
) -> Result<Value> {
    let available = capabilities_for(store, recipe)?;
    storage::enqueue_package(
        store,
        profile,
        file,
        recipe,
        location,
        item,
        &available["tools"],
        package,
    )
}
fn capabilities_for(store: &mut Store, recipe: &str) -> Result<Value> {
    let saved = storage::get_recipe(store, recipe)?;
    let available = store.detached(rendering::capabilities)?;
    let capability = available["presets"]
        .as_array()
        .and_then(|rows| rows.iter().find(|p| p["name"] == saved["preset"]))
        .ok_or_else(|| Error("unknown render preset".into()))?;
    if capability["available"] != true {
        return Err(Error(format!(
            "the installed FFmpeg build lacks required encoders, muxers or filters: {}",
            capability["missing"]
        )));
    }
    Ok(available)
}
/// Explicit local historical derivative admission; ordinary originals retain
/// their exact cache identity and the concrete chosen input remains pinned.
pub fn enqueue_lineage(
    store: &mut Store,
    profile: &str,
    file: &str,
    recipe: &str,
    location: &str,
    item: &str,
    lineage: &str,
) -> Result<Value> {
    let available = capabilities_for(store, recipe)?;
    storage::enqueue_lineage(
        store,
        profile,
        file,
        recipe,
        location,
        item,
        &available["tools"],
        lineage,
    )
}
fn budget(root: &File, output: &File, recipe: &Value) -> Result<()> {
    // SAFETY: descriptor is live and fstatvfs initializes this output on success.
    let mut space: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: descriptor and mutable buffer are valid for this call.
    if unsafe { libc::fstatvfs(root.as_raw_fd(), &mut space) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let available = u128::from(space.f_bavail) * u128::from(space.f_frsize);
    if available
        < u128::from(
            recipe["reserve_bytes"]
                .as_u64()
                .ok_or_else(|| Error("invalid render reserve".into()))?,
        )
    {
        return Err(Error(
            "generated filesystem is below the free-space reserve".into(),
        ));
    }
    if output.metadata()?.size()
        >= recipe["max_output_bytes"]
            .as_u64()
            .ok_or_else(|| Error("invalid render byte budget".into()))?
    {
        return Err(Error("generated output exceeds its byte budget".into()));
    }
    Ok(())
}
pub fn run_one(store: &mut Store, profile: &str, id: &str, cancel: &AtomicBool) -> Result<Value> {
    let job = claims::get_job(store, profile, id)?;
    let options = &job["options"];
    let recipe = &options["recipe"];
    let preset = rendering::preset(recipe["preset"].as_str().unwrap_or(""))?;
    let (claim, artifact) = storage::start(store, profile, id, preset.extension)?;
    let artifact_id = artifact["id"].as_str().unwrap();
    let result = (|| {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error("artifact execution cancelled".into()));
        }
        let (root, mut output) = storage::create_temporary(store, profile, &claim, &artifact)?;
        let observer = output.try_clone()?;
        let rendered = storage::detached(store, &claim, |lost| {
            claims::validated_source(&job["snapshot"], |input| {
                if rendering::tools(true)? != options["tools"] {
                    return Err(Error(
                        "FFmpeg or ffprobe changed since enqueue; submit a new job".into(),
                    ));
                }
                let mut poll = || {
                    if lost.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
                        return Err(Error("artifact execution cancelled or claim lost".into()));
                    }
                    budget(&root, &observer, recipe)
                };
                poll()?;
                if let Some(package) = options.get("component_package") {
                    crate::component_packaging::render_selected(
                        input,
                        &mut output,
                        recipe,
                        &options["tools"],
                        cancel,
                        &mut poll,
                        package,
                    )
                } else {
                    rendering::render(
                        input,
                        &mut output,
                        recipe,
                        &options["tools"],
                        cancel,
                        &mut poll,
                    )
                }
            })
        })?;
        output.sync_all()?;
        root.sync_all()?;
        let revision = rendering::file_revision(&output)?;
        if revision != rendered.output_revision {
            return Err(Error("artifact changed after validation".into()));
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(Error("artifact execution cancelled".into()));
        }
        storage::intent(
            store,
            profile,
            &claim,
            artifact_id,
            rendered.size,
            &rendered.sha256,
            &rendered.validation,
            &rendered.output_revision,
        )?;
        storage::publish(store, profile, &claim, artifact_id)
    })();
    if let Err(error) = &result {
        let _ = storage::fail(store, profile, &claim, artifact_id, &error.0);
    }
    result
}
/// None selects the oldest queued render jobs; Some([]) deliberately runs none.
pub fn run(
    store: &mut Store,
    profile: &str,
    limit: usize,
    job_ids: Option<&[String]>,
    cancel: Arc<AtomicBool>,
) -> Result<Value> {
    if !(1..=1000).contains(&limit) || job_ids.is_some_and(|ids| ids.len() > 1000) {
        return Err(Error("limit and job_ids must be at most 1000".into()));
    }
    catabolic_store::mutation::require_recovered(&store.db)?;
    {
        let mut statement=store.db.prepare("SELECT DISTINCT job_id FROM processing_artifacts WHERE profile=? AND state IN ('planned','writing','validating','publishing')")?;
        let mut pending = statement.query([profile])?;
        while let Some(row) = pending.next()? {
            let id: String = row.get(0)?;
            if !storage::active(store, &id)? {
                return Err(Error(
                    "run artifact recover before starting more output jobs".into(),
                ));
            }
        }
    }
    let mut sql =
        "SELECT id FROM processing_jobs WHERE profile=? AND operation='render' AND state='queued'"
            .to_owned();
    let mut args = vec![profile.to_owned().into()];
    if let Some(ids) = job_ids {
        let ids = ids.iter().collect::<BTreeSet<_>>();
        sql.push_str(&format!(" AND id IN ({})", vec!["?"; ids.len()].join(",")));
        args.extend(ids.into_iter().map(|id| id.clone().into()));
    }
    sql.push_str(" ORDER BY created_at,id LIMIT ?");
    args.push((limit as i64).into());
    let queued = catabolic_store::migration::rows(&store.db, &sql, &args)?;
    let ids = queued
        .iter()
        .filter_map(|v| v["id"].as_str())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut completed = vec![];
    let mut errors = vec![];
    for id in ids {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        match run_one(store, profile, &id, &cancel) {
            Ok(v) => completed.push(v),
            Err(e) => {
                errors.push(json!({"job_id":id,"error":e.0.chars().take(4000).collect::<String>()}))
            }
        }
    }
    Ok(
        json!({"completed":completed,"complete":errors.is_empty()&&!cancel.load(Ordering::Relaxed),"errors":errors}),
    )
}
pub fn recover(store: &mut Store, profile: &str, limit: usize) -> Result<Value> {
    storage::recover(store, profile, limit)
}
