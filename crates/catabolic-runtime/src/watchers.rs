// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded native watcher workers and detached processing reactions.
use catabolic_store::{Error, Result, Store, watcher};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

fn count(request: &Value, key: &str, default: u64, maximum: u64) -> Result<usize> {
    let value = request
        .get(key)
        .map(|v| {
            v.as_u64()
                .ok_or_else(|| Error(format!("invalid watcher {key}")))
        })
        .transpose()?
        .unwrap_or(default);
    if value == 0 || value > maximum {
        return Err(Error(format!("invalid watcher {key}")));
    }
    Ok(value as usize)
}

pub fn worker(path: &Path, profile: &str, name: &str) -> Result<Value> {
    worker_with_options(path, profile, name, &json!({}))
}
pub fn worker_with_options(
    path: &Path,
    profile: &str,
    name: &str,
    options: &Value,
) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match Store::open(path, true, false) {
            Ok(mut store) => {
                watcher::execute(&store, profile, "recover", &json!({}))?;
                let mut request = json!({"name":name,"trigger":"scheduled"});
                if cfg!(debug_assertions) && options["disposable_fixture"] == true {
                    request["disposable_fixture"] = json!(true);
                }
                return execute(&mut store, profile, "run", &request);
            }
            Err(error)
                if error.0.contains("another Catabolic writer") && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(error) => return Err(error),
        }
    }
}
pub fn execute(store: &mut Store, profile: &str, action: &str, request: &Value) -> Result<Value> {
    match action {
        "run" => {
            let name = request["name"]
                .as_str()
                .ok_or_else(|| Error("unknown watcher".into()))?;
            let definition = watcher::definition(store, profile, name)?;
            if definition["reaction"] == "projection"
                && !(cfg!(debug_assertions) && request["disposable_fixture"] == true)
            {
                return Err(Error("native fallback publication is restricted to disposable debug fixtures pending filesystem qualification".into()));
            }
            let now = request
                .get("now")
                .map(|v| {
                    v.as_f64()
                        .filter(|v| v.is_finite())
                        .ok_or_else(|| Error("invalid watcher clock".into()))
                })
                .transpose()?
                .unwrap_or_else(crate::media::now);
            watcher::run(
                store,
                profile,
                name,
                request["trigger"].as_str().unwrap_or("manual"),
                now,
                &mut |store, request| {
                    let mut request = request.clone();
                    request["isolate"] = json!(true);
                    crate::observation::execute(store, profile, "scan", &request)
                },
                &mut |store, context| complete(store, profile, context, request),
            )
        }
        "tick" => tick(store, profile, request),
        "startup" => {
            let sources = watcher::startup_sources(store, profile)?;
            let mut result = vec![];
            for source in sources {
                let mut value = catabolic_store::observation::execute(
                    store,
                    profile,
                    "dirty",
                    &json!({"source":source}),
                )?;
                value["reason"] = json!("startup_reconciliation");
                result.push(value);
            }
            Ok(json!(result))
        }
        _ => watcher::execute(store, profile, action, request),
    }
}

fn complete(
    store: &mut Store,
    profile: &str,
    context: watcher::Completion,
    request: &Value,
) -> Result<Value> {
    let reference = &context.definition["plan"];
    if reference["kind"] == "projection" {
        return crate::fallback::with_projection_plan(
            store,
            profile,
            reference["catalog"].as_str().unwrap(),
            false,
            |store, prepared, guard| {
                let description = catabolic_store::plans::describe(store, profile, reference)?;
                let value = prepared.present();
                let complete = value["desired"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|d| d["state"] != "blocked");
                let evaluated = json!({"kind":"projection","complete":complete,"value":value,"description":description});
                let mut prepared = Some(prepared);
                watcher::complete_evaluation(
                    store,
                    profile,
                    &context,
                    &evaluated,
                    &mut |store, definition, _, _| {
                        let options = json!({"expected_plan":evaluated["value"]["plan_id"],"max_changes":definition["max_changes"],"max_removals":definition["max_removals"],"disposable_fixture":request["disposable_fixture"]});
                        crate::fallback::apply_prepared(
                            store,
                            profile,
                            prepared
                                .take()
                                .ok_or_else(|| Error("projection already consumed".into()))?,
                            &options,
                            guard,
                        )
                    },
                )
            },
        );
    }
    if reference["kind"] == "fallback" {
        let path = store.path.clone();
        let database = store.database_id.clone();
        return store.detached(|| {
            let writer = Store::open(&path, true, false)?;
            if writer.database_id != database {
                return Err(Error("catalog replaced during watcher evaluation".into()));
            }
            let _guard = catabolic_store::filesystem_mutation::publication_lock(&writer)?;
            let mut evaluation = catabolic_store::selection::Evaluation::new(
                writer, profile, 60_000, 100_000, false,
            )?;
            let previous = context.row["baseline"]
                .as_str()
                .map(catabolic_core::decode_json)
                .transpose()?;
            let evaluated = catabolic_store::plans::evaluate_fallback(
                &mut evaluation,
                reference,
                previous.as_ref(),
                &format!("watcher:{}", context.name),
                &mut crate::fallback::probe,
            )?;
            watcher::complete_evaluation(
                evaluation.store_mut(),
                profile,
                &context,
                &evaluated,
                &mut |store, definition, evaluation, run| {
                    processing(store, profile, definition, evaluation, run)
                },
            )
        });
    }
    let evaluated = watcher::evaluate(store, profile, reference)?;
    watcher::complete_evaluation(
        store,
        profile,
        &context,
        &evaluated,
        &mut |store, definition, evaluation, run| {
            processing(store, profile, definition, evaluation, run)
        },
    )
}

fn processing(
    store: &mut Store,
    profile: &str,
    definition: &Value,
    evaluation: &Value,
    run: &Value,
) -> Result<Value> {
    let operation_id = definition["operation_id"]
        .as_str()
        .ok_or_else(|| Error("processing requires an operation_id".into()))?;
    let operation = catabolic_store::media_mutation::operation_get(store, operation_id)?;
    let inputs = watcher::processing_inputs(
        store,
        profile,
        evaluation,
        definition["max_jobs"].as_u64().unwrap() as usize,
    )?;
    let lineage = if evaluation["kind"] == "fallback" {
        let policy = catabolic_store::fallback_mutation::get(
            store,
            profile,
            definition["plan"]["policy_id"].as_str().unwrap(),
        )?;
        policy["definition"]["lineage_requirement"]
            .as_str()
            .map(str::to_owned)
    } else {
        None
    };
    let kind = operation["operation_kind"].as_str().unwrap_or("");
    let preset = operation["preset"].as_str().unwrap_or("");
    let prepared = match kind {
        "render" => store.detached(crate::rendering::capabilities)?,
        "analysis" => store.detached(|| {
            crate::media::operation_config(preset, &operation["definition"]["options"])
        })?,
        _ => return Err(Error("watcher operation kind is not supported".into())),
    };
    watcher::require_run(
        store,
        profile,
        run["name"].as_str().unwrap(),
        run["run_id"].as_str().unwrap(),
        &run["definition_id"],
    )?;
    // Revalidate the entire input set before the first admission after waiting.
    for input in &inputs {
        if catabolic_store::content_access::revision_of(
            store,
            profile,
            input["file_id"].as_str().unwrap(),
        )? != input["revision"]
        {
            return Err(Error("stale_watcher_processing_input".into()));
        }
    }
    let mut jobs = vec![];
    for input in inputs {
        watcher::require_run(
            store,
            profile,
            run["name"].as_str().unwrap(),
            run["run_id"].as_str().unwrap(),
            &run["definition_id"],
        )?;
        let file = input["file_id"].as_str().unwrap();
        if catabolic_store::content_access::revision_of(store, profile, file)? != input["revision"]
        {
            return Err(Error("stale_watcher_processing_input".into()));
        }
        let admission = if kind == "render" {
            let capability = prepared["presets"]
                .as_array()
                .and_then(|rows| rows.iter().find(|row| row["name"] == preset))
                .ok_or_else(|| Error("unknown render preset".into()))?;
            if capability["available"] != true {
                return Err(Error("watcher render capability unavailable".into()));
            }
            if let Some(lineage) = &lineage {
                catabolic_store::artifact_mutation::enqueue_lineage(
                    store,
                    profile,
                    file,
                    operation_id,
                    definition["destination"].as_str().ok_or_else(|| {
                        Error("render reaction requires an approved destination".into())
                    })?,
                    input["item_id"].as_str().unwrap(),
                    &prepared["tools"],
                    lineage,
                )?
            } else {
                catabolic_store::artifact_mutation::enqueue(
                    store,
                    profile,
                    file,
                    operation_id,
                    definition["destination"].as_str().ok_or_else(|| {
                        Error("render reaction requires an approved destination".into())
                    })?,
                    input["item_id"].as_str().unwrap(),
                    &prepared["tools"],
                )?
            }
        } else {
            let mut configuration = prepared.clone();
            configuration["operation_id"] = json!(operation_id);
            let job = catabolic_store::media_mutation::enqueue(
                store,
                profile,
                file,
                preset,
                &configuration,
                false,
                Some(operation_id),
            )?;
            json!({"queued":if job["cached"]==true{json!([])}else{json!([job["id"]])},"cached":if job["cached"]==true{json!([job["id"]])}else{json!([])},"complete":true,"errors":[],"next_after":null})
        };
        jobs.push(json!({"input":input,"admission":admission}));
    }
    Ok(json!({"jobs":jobs,"complete":true}))
}

struct Worker {
    name: String,
    child: Child,
    output: File,
    started: Instant,
}
impl Drop for Worker {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
fn launch(
    binary: &Path,
    path: &Path,
    profile: &str,
    name: &str,
    options: &Value,
) -> Result<Worker> {
    let output = tempfile::tempfile()?;
    let child = Command::new(binary)
        .args(["--db"])
        .arg(path)
        .args([
            "--profile",
            profile,
            "watcher-worker",
            name,
            "--options",
            &catabolic_core::encode(options),
        ])
        .stdout(Stdio::from(output.try_clone()?))
        .stderr(Stdio::from(output.try_clone()?))
        .spawn()?;
    Ok(Worker {
        name: name.to_string(),
        child,
        output,
        started: Instant::now(),
    })
}
fn tick(store: &mut Store, profile: &str, request: &Value) -> Result<Value> {
    if request["native"] == true {
        return Err(Error(
            "native filesystem event monitoring is not yet qualified".into(),
        ));
    }
    let limit = count(request, "limit", 100, 1000)?;
    let workers = count(request, "workers", 4, 4)?;
    let timeout = count(request, "timeout_seconds", 120, 120)?;
    // The supervisor launches this native executable, never an interpreted host.
    let binary = crate::helper::executable()?;

    watcher::execute(store, profile, "recover", &json!({}))?;
    let now = crate::media::now();
    let names = watcher::supervisor_candidates(store, profile, now, limit)?;
    let options = if cfg!(debug_assertions) && request["disposable_fixture"] == true {
        json!({"disposable_fixture":true})
    } else {
        json!({})
    };
    let path = store.path.clone();
    let results = store.detached(|| {
        let mut pending = names.into_iter();
        let mut active: Vec<Worker> = vec![];
        let mut results = vec![];
        loop {
            while active.len()<workers {
                let Some(name)=pending.next() else {break};
                match launch(&binary,&path,profile,&name,&options) {
                    Ok(worker)=>active.push(worker),
                    Err(error)=>results.push(json!({"watcher":name,"complete":false,"error":error.0})),
                }
            }
            if active.is_empty() {break;}
            let mut index=0;
            while index<active.len() {
                let worker=&mut active[index];
                if worker.started.elapsed()>Duration::from_secs(timeout as u64) || worker.output.metadata()?.len()>8*1024*1024 {
                    worker.child.kill()?;
                    worker.child.wait()?;
                }
                if let Some(status)=worker.child.try_wait()? {
                    let mut worker=active.remove(index);
                    worker.output.seek(SeekFrom::Start(0))?;
                    let mut data=String::new();
                    (&mut worker.output).take(8*1024*1024).read_to_string(&mut data)?;
                    let result = if status.success() {catabolic_core::decode_json(&data).unwrap_or_else(|_|json!({"watcher":worker.name,"complete":false,"error":"invalid watcher worker response"}))}
                        else {json!({"watcher":worker.name,"complete":false,"error":data.chars().rev().take(2000).collect::<String>().chars().rev().collect::<String>()})};
                    results.push(result);
                    // Confirmed exit precedes any recovery; never reap a live
                    // replacement owner simply because a timeout elapsed.
                    if let Ok(recovery)=Store::open(&path,true,false) {watcher::execute(&recovery,profile,"recover",&json!({}))?;}
                } else {index+=1;}
            }
            if !active.is_empty() {std::thread::sleep(Duration::from_millis(50));}
        }
        Ok(results)
    })?;
    watcher::execute(store, profile, "recover", &json!({}))?;
    let pending = watcher::pending_count(store, profile, crate::media::now())?;
    Ok(
        json!({"complete":pending==0&&results.iter().all(|r|r["complete"]==true),"runs":results,"pending":pending}),
    )
}
