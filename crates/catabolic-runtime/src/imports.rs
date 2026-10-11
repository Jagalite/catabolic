// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Explicit official application imports. No automatic retry follows a launch
//! with an uncertain external outcome, and no source path reaches that tool.
use catabolic_store::{
    Error, Result, Store,
    imports::{self, Prepared},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|path| path.join(name))
        .find(|path| {
            path.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}
fn applied(mut prepared: Prepared, cancel: &AtomicBool, poll: &mut dyn FnMut()) -> Result<Value> {
    let program = which(prepared.executable).ok_or_else(|| {
        Error(format!(
            "install the official {} CLI before using --apply",
            prepared.executable
        ))
    })?;
    let mut environment: BTreeMap<OsString, OsString> = std::env::vars_os()
        .filter(|(k, _)| !k.to_string_lossy().starts_with("IMMICH_"))
        .collect();
    if prepared.executable == "immich" {
        let key = std::env::var_os("IMMICH_API_KEY")
            .filter(|v| !v.is_empty())
            .ok_or_else(|| Error("set IMMICH_API_KEY before applying an Immich import".into()))?;
        environment.insert("IMMICH_API_KEY".into(), key);
        environment.insert(
            "IMMICH_INSTANCE_URL".into(),
            prepared.destination.clone().into(),
        );
    }
    for group in &prepared.groups {
        let files: Vec<_> = group.iter().map(|v| v.file["id"].clone()).collect();
        let mut attempt = json!({"id":uuid::Uuid::new_v4().to_string(),"file_ids":files,"outcome":"failed_before_launch"});
        let mut launched = false;
        let result = (|| -> Result<()> {
            let mut checkpoint = || {
                poll();
                if cancel.load(Ordering::Acquire) {
                    Err(Error(
                        "import interrupted; inspect the destination before retrying".into(),
                    ))
                } else {
                    Ok(())
                }
            };
            checkpoint()?;
            let directory = tempfile::Builder::new()
                .prefix("catabolic-import-")
                .tempdir()?;
            let mut last = PathBuf::new();
            for decision in group {
                last = directory.path().join(format!(
                    "asset{}",
                    imports::suffix(decision.file["path"].as_str().unwrap())
                ));
                imports::stage(decision, &last, &mut checkpoint)?;
            }
            let item = &group[0].item;
            let mut argv: Vec<OsString> = vec![program.as_os_str().to_owned()];
            if prepared.executable == "immich" {
                argv.extend([
                    "upload".into(),
                    "--no-progress".into(),
                    last.into_os_string(),
                ]);
            } else {
                let title = &item["metadata"]["title"];
                let title = if !catabolic_store::exports::truth(title) {
                    &item["id"]
                } else {
                    title
                };
                argv.extend([
                    "add".into(),
                    "--with-library".into(),
                    prepared.destination.clone().into(),
                    "--one-book-per-directory".into(),
                    format!("--title={}", catabolic_store::exports::python_text(title)).into(),
                    format!(
                        "--identifier=catabolic:{}:{}",
                        prepared.database_id,
                        item["id"].as_str().unwrap()
                    )
                    .into(),
                ]);
                let author = &item["metadata"]["author"];
                if catabolic_store::exports::truth(author) {
                    argv.push(
                        format!(
                            "--authors={}",
                            catabolic_store::exports::python_text(author)
                        )
                        .into(),
                    );
                }
                argv.push(directory.path().as_os_str().to_owned());
            }
            let mut started = || {
                launched = true;
                Ok(())
            };
            let mut polling = || {
                poll();
                Ok(())
            };
            crate::process::command_output(
                &argv,
                crate::process::CommandOptions {
                    timeout: Duration::from_secs_f64(prepared.timeout),
                    maximum: 8 * 1024 * 1024,
                    capture: false,
                    env: Some(&environment),
                    on_start: Some(&mut started),
                    on_poll: Some(&mut polling),
                    cancel: Some(cancel),
                    ..Default::default()
                },
            )
            .map_err(|e| Error(e.to_string()))?;
            Ok(())
        })();
        attempt["outcome"] = json!(if result.is_ok() {
            "completed"
        } else if launched {
            "external_outcome_unknown"
        } else {
            "failed_before_launch"
        });
        prepared.report["attempts"]
            .as_array_mut()
            .unwrap()
            .push(attempt.clone());
        if let Err(error) = result {
            let completed = !prepared.report["completed"].as_array().unwrap().is_empty();
            prepared.report["complete"] = json!(false);
            prepared.report["applied"] = if completed {
                json!(true)
            } else if launched {
                Value::Null
            } else {
                json!(false)
            };
            prepared.report["outcome"] = attempt["outcome"].clone();
            prepared.report["safe_to_retry"] = json!(!launched && !completed);
            let interrupted = cancel.load(Ordering::Acquire);
            prepared.report["interrupted"] = json!(interrupted);
            prepared.report["failed_files"] = json!(files);
            prepared.report["error"] = json!(if interrupted {
                "import interrupted; inspect the destination before retrying".to_owned()
            } else {
                error.0
            });
            return Ok(prepared.report);
        }
        prepared.report["completed"]
            .as_array_mut()
            .unwrap()
            .extend(files);
    }
    prepared.report["outcome"] = json!("completed");
    prepared.report["applied"] = json!(true);
    Ok(prepared.report)
}
pub fn execute(store: &mut Store, profile: &str, request: &Value) -> Result<Value> {
    execute_with_poll(store, profile, request, &AtomicBool::new(false), &mut || {})
}
pub fn execute_with_poll(
    store: &mut Store,
    profile: &str,
    request: &Value,
    cancel: &AtomicBool,
    poll: &mut dyn FnMut(),
) -> Result<Value> {
    let prepared = imports::prepare(store, profile, request)?;
    if request["apply"] != true || prepared.groups.is_empty() {
        return Ok(prepared.report);
    }
    store.detached(|| applied(prepared, cancel, poll))
}
