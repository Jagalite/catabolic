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
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

fn applied(mut prepared: Prepared, cancel: &AtomicBool, poll: &mut dyn FnMut()) -> Result<Value> {
    let program = imports::executable(prepared.executable).ok_or_else(|| {
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

/// The mapping preview needs the actual Immich credential owner. It never uses
/// a URL-only substitute, discovers a CLI by launching it, or submits media.
pub const IDENTITY_WORKER: &str = "__publication-immich-identity-worker";
const IDENTITY_LIMIT: usize = 1024 * 1024;
const IDENTITY_INPUT: usize = 256 * 1024;
const IDENTITY_ERROR: &str = "HTTP worker failed; check endpoint and network";
fn identity_token() -> Result<String> {
    let token = std::env::var("IMMICH_API_KEY")
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error("set credential environment variable IMMICH_API_KEY".into()))?;
    if token.contains(['\r', '\n']) {
        return Err(Error("invalid credential value".into()));
    }
    Ok(token)
}
fn identity_url(destination: &str) -> Result<String> {
    let base = catabolic_store::exports::http_base(Some(destination))?;
    let base = base.trim_end_matches('/');
    Ok(format!(
        "{base}{}/users/me",
        if base.ends_with("/api") { "" } else { "/api" }
    ))
}
fn identity_request(destination: &str) -> Result<(String, String)> {
    let url = identity_url(destination)?;
    let token = identity_token()?;
    let budget = catabolic_core::encode(
        &json!({"url":url,"headers":{"x-api-key":token,"Accept":"application/json"},"method":"GET","maximum":IDENTITY_LIMIT,"timeout":15,"body":null,"structured_errors":true}),
    );
    if budget.len() > IDENTITY_INPUT {
        return Err(Error("HTTP request exceeds input limit".into()));
    }
    Ok((url, token))
}
fn fetch_identity(destination: &str) -> Result<Value> {
    use std::io::Read;
    let (url, token) = identity_request(destination)?;
    let network = || catabolic_store::consumers::Failure::new("temporarily_failed").error();
    let bytes: Vec<u8> = token
        .chars()
        .map(|c| u8::try_from(u32::from(c)))
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| network())?;
    let mut token = ureq::http::HeaderValue::from_bytes(&bytes).map_err(|_| network())?;
    token.set_sensitive(true);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .max_idle_connections(0)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(15)))
        .timeout_connect(Some(Duration::from_secs(10)))
        .build()
        .into();
    let mut response = agent
        .get(&url)
        .header("x-api-key", token)
        .header("Accept", "application/json")
        .header("Connection", "close")
        .call()
        .map_err(|_| network())?;
    let status = response.status().as_u16();
    if [301, 302, 303, 307, 308].contains(&status) && response.headers().contains_key("location") {
        return Err(network());
    }
    if !(200..300).contains(&status) {
        return Err(catabolic_store::consumers::Failure::new(match status {
            401 | 403 => "unauthorized",
            429 => "rate_limited",
            404 => "unavailable",
            405 | 501 => "unsupported",
            400 | 412 | 422 => "invalid_configuration",
            _ => "temporarily_failed",
        })
        .error());
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take((IDENTITY_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| network())?;
    if bytes.len() > IDENTITY_LIMIT {
        return Err(network());
    }
    let user = crate::consumers::json_wire::decode(&bytes)
        .map_err(|_| Error("mapping_invalid_import_account_identity".into()))?;
    let id = user["id"]
        .as_str()
        .filter(|id| !id.is_empty() && id.chars().count() <= 128)
        .ok_or_else(|| Error("mapping_invalid_import_account_identity".into()))?;
    Ok(json!({"user_id":id}))
}
/// Private bounded helper receives only the endpoint. Credentials stay in its
/// environment and the only successful output is the frozen account identifier.
pub fn identity_worker_main() {
    use std::io::{Read, Write};
    let result = (|| -> Result<Value> {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take((IDENTITY_INPUT + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > IDENTITY_INPUT {
            return Err(Error("HTTP request exceeds input limit".into()));
        }
        let value =
            catabolic_core::decode_json_bytes(&bytes).map_err(|_| Error(IDENTITY_ERROR.into()))?;
        catabolic_store::filesystem_mutation::admit_failpoints(&value)?;
        fetch_identity(
            value["destination"]
                .as_str()
                .ok_or_else(|| Error(IDENTITY_ERROR.into()))?,
        )
    })();
    let value = match result {
        Ok(identity) => json!({"identity":identity}),
        Err(error) => json!({"error":error.0}),
    };
    let _ = writeln!(std::io::stdout(), "{}", catabolic_core::encode(&value));
}
pub fn mapping_identity(
    destination: &str,
    executable: &std::path::Path,
    cancel: &AtomicBool,
) -> Result<Value> {
    let checkpoint = || {
        if cancel.load(Ordering::Acquire) {
            Err(Error("native legacy CLI interrupted".into()))
        } else {
            Ok(())
        }
    };
    checkpoint()?;
    // Preserve request-size admission before a helper is spawned, including
    // escaped credential bytes, without transmitting the token in the protocol.
    identity_request(destination)?;
    let input = catabolic_core::encode(&json!({"destination":destination}));
    let output = crate::process::command_output(
        &[
            executable.as_os_str(),
            std::ffi::OsStr::new(IDENTITY_WORKER),
        ],
        crate::process::CommandOptions {
            input_bytes: Some(input.as_bytes()),
            cancel: Some(cancel),
            timeout: Duration::from_secs(15),
            maximum: 4096,
            ..Default::default()
        },
    );
    checkpoint()?;
    let output = output.map_err(|error| {
        Error(
            match error.state.as_str() {
                "timeout" => "HTTP request exceeded the elapsed-time limit",
                _ => IDENTITY_ERROR,
            }
            .into(),
        )
    })?;
    if output.returncode != 0 || !output.stderr.is_empty() {
        return Err(Error(IDENTITY_ERROR.into()));
    }
    let value = catabolic_core::decode_json_bytes(&output.stdout)
        .map_err(|_| Error(IDENTITY_ERROR.into()))?;
    if let Some(error) = value["error"].as_str() {
        return Err(Error(error.into()));
    }
    let id = value["identity"]["user_id"]
        .as_str()
        .filter(|id| !id.is_empty() && id.chars().count() <= 128)
        .ok_or_else(|| Error(IDENTITY_ERROR.into()))?;
    Ok(json!({"user_id":id}))
}
