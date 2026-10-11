// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Native source analysis. Tool waits own no database connection or writer lock;
//! source descriptors and durable execution claims fence every accepted result.
use crate::process::{CommandOptions, command_output};
use catabolic_core::media::{HEADER_COVERAGE, failure_health, normalize_probe};
use catabolic_store::{Error, Result, Store, media_mutation as storage};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::File,
    io::Read,
    os::{
        fd::AsRawFd,
        unix::fs::{FileExt, MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
fn executable(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|p| p.join(name))
        .find(|p| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .and_then(|p| p.canonicalize().ok())
}
pub fn tool_signature(operation: &str) -> Value {
    let (name, adapter) = match operation {
        "probe" => ("ffprobe", "probe-v3"),
        "decode" => ("ffmpeg", "decode-v2"),
        _ => return json!({"adapter":format!("{operation}-v1")}),
    };
    let Some(path) = executable(name) else {
        return json!({"adapter":adapter,"tool":name,"available":false});
    };
    let Ok(metadata) = path.metadata() else {
        return json!({"adapter":adapter,"tool":name,"available":false});
    };
    let version = command_output(
        &[path.as_os_str(), "-version".as_ref()],
        CommandOptions {
            timeout: Duration::from_secs(5),
            maximum: 65536,
            ..Default::default()
        },
    )
    .ok()
    .map(|o| {
        String::from_utf8_lossy(&o.into_bytes(true))
            .lines()
            .next()
            .unwrap_or("version unavailable")
            .to_owned()
    })
    .unwrap_or_else(|| "version unavailable".into());
    json!({"adapter":adapter,"tool":path,"available":true,"version":version,"size":metadata.size(),"mtime_ns":i128::from(metadata.mtime())*1_000_000_000+i128::from(metadata.mtime_nsec())})
}
pub fn operation_config(operation: &str, options: &Value) -> Result<Value> {
    if operation == "text" {
        return crate::document::operation_config(options).map_err(|e| Error(e.to_string()));
    }
    if !["sniff", "hash", "verify", "probe", "decode"].contains(&operation) {
        return Err(Error("unsupported processing operation".into()));
    }
    let values = options
        .as_object()
        .filter(|_| catabolic_core::encode(options).len() <= 256 * 1024)
        .ok_or_else(|| Error("payload must be a JSON object of at most 256 KiB".into()))?;
    if values.keys().any(|k| {
        !["timeout", "max_bytes", "analysis_bytes", "analysis_us"].contains(&k.as_str())
            && !(operation == "probe" && k == "required_streams")
    }) {
        return Err(Error("unknown processing option".into()));
    }
    let mut config = json!({"timeout":if ["hash","verify","decode"].contains(&operation) {120} else {20},"max_bytes":2*1024*1024,"analysis_bytes":1024*1024,"analysis_us":1_000_000});
    for (k, v) in values {
        config[k] = v.clone();
    }
    for (key, max) in [
        ("timeout", 86400),
        ("max_bytes", 8 * 1024 * 1024),
        ("analysis_bytes", 64 * 1024 * 1024),
        ("analysis_us", 60_000_000),
    ] {
        if !config[key].as_u64().is_some_and(|n| (1..=max).contains(&n)) {
            return Err(Error(format!("invalid {key} budget")));
        }
    }
    if operation == "probe" {
        let empty = vec![];
        let streams = config
            .get("required_streams")
            .map(|v| {
                v.as_array().ok_or_else(|| {
                    Error("required_streams must be a list of audio/video stream types".into())
                })
            })
            .transpose()?
            .unwrap_or(&empty);
        if streams.len() > 2
            || streams
                .iter()
                .any(|s| !matches!(s.as_str(), Some("audio" | "video")))
        {
            return Err(Error(
                "required_streams must be a list of audio/video stream types".into(),
            ));
        }
        config["required_streams"] = json!(
            streams
                .iter()
                .filter_map(Value::as_str)
                .collect::<BTreeSet<_>>()
        );
    }
    config["extractor"] = tool_signature(operation);
    Ok(config)
}
pub fn enqueue(
    store: &Store,
    profile: &str,
    operation: &str,
    file_ids: &[String],
    options: &Value,
    refresh: bool,
    recipe: Option<&str>,
) -> Result<Value> {
    if file_ids.len() > 1000 {
        return Err(Error("file_ids must contain at most 1000 IDs".into()));
    }
    let config = operation_config(operation, options)?;
    enqueue_config(
        store, profile, operation, file_ids, options, config, refresh, recipe, None,
    )
}

/// Admission with an identity already resolved outside the writer lock.
#[allow(clippy::too_many_arguments)]
pub fn enqueue_config(
    store: &Store,
    profile: &str,
    operation: &str,
    file_ids: &[String],
    options: &Value,
    mut config: Value,
    refresh: bool,
    recipe: Option<&str>,
    cancel: Option<&AtomicBool>,
) -> Result<Value> {
    if file_ids.len() > 1000 {
        return Err(Error("file_ids must contain at most 1000 IDs".into()));
    }
    if let Some(id) = recipe {
        let recipe = storage::operation_get(store, id)?;
        if recipe["operation_kind"] != "analysis"
            || recipe["preset"] != operation
            || recipe["definition"]["options"] != *options
        {
            return Err(Error(
                "processing operation does not match its immutable definition".into(),
            ));
        }
        config["operation_id"] = json!(id);
    }
    let mut queued = vec![];
    let mut cached = vec![];
    let mut errors = vec![];
    for file in file_ids.iter().collect::<BTreeSet<_>>() {
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(Error("processing cancelled".into()));
        }
        storage::snapshot(store, profile, file)?;
        match storage::enqueue(store, profile, file, operation, &config, refresh, recipe) {
            Ok(v) => {
                if v["cached"] == true {
                    cached.push(v["id"].clone())
                } else {
                    queued.push(v["id"].clone())
                }
            }
            Err(e) => errors.push(json!({"file_id":file,"error":e.0})),
        }
    }
    Ok(
        json!({"queued":queued,"cached":cached,"complete":errors.is_empty(),"errors":errors,"next_after":null}),
    )
}
fn failure(operation: &str, state: &str, message: impl Into<String>, diagnostic: &str) -> Value {
    json!({"state":state,"data":if ["probe","decode"].contains(&operation) {json!({"media_health":failure_health(operation,state,diagnostic)})} else {json!({})},"error":message.into(),"retryable":state=="timeout"})
}
fn extension(path: &str) -> String {
    Path::new(path)
        .extension()
        .map(|p| format!(".{}", p.to_string_lossy().to_lowercase()))
        .unwrap_or_default()
}
fn inspect_header(file: &File, snapshot: &Value) -> Result<Value> {
    let size = snapshot["size"]
        .as_u64()
        .ok_or_else(|| Error("invalid source size".into()))?;
    let path = snapshot["path"].as_str().unwrap_or("");
    let lower = path.to_lowercase();
    let reason = if size == 0 {
        Some("empty")
    } else if [
        ".!qb",
        ".aria2",
        ".crdownload",
        ".download",
        ".incomplete",
        ".part",
        ".partial",
        ".tmp",
    ]
    .iter()
    .any(|s| lower.ends_with(s))
    {
        Some("incomplete_download")
    } else {
        None
    };
    if let Some(reason) = reason {
        return Ok(json!({"status":"invalid","reason":reason,"bytes_read":0}));
    }
    if ![
        ".mkv", ".mka", ".webm", ".avi", ".wav", ".wave", ".flac", ".ogg", ".oga", ".ogv", ".opus",
    ]
    .contains(&extension(path).as_str())
    {
        return Ok(json!({"status":"unknown","reason":"unsupported_header","bytes_read":0}));
    }
    let mut data = [0; 64];
    let read = file.read_at(&mut data, 0)?;
    if read != size.min(64) as usize {
        return Err(Error("source changed while checking media header".into()));
    }
    let invalid = read > 0 && data[..read].iter().all(|b| *b == 0);
    Ok(
        json!({"status":if invalid {"invalid"} else {"not_detected"},"reason":if invalid {json!("zero_filled_header")} else {Value::Null},"bytes_read":read}),
    )
}
const FORMATS: &str = "mov,matroska,webm,wav,flac,mp3,ogg,aac,ac3,eac3,aiff,ape,asf,avi,mpeg,mpegts,png_pipe,jpeg_pipe,webp_pipe,gif,bmp_pipe,tiff_pipe";
pub fn process(job: &Value, cancel: &AtomicBool) -> Value {
    let operation = job["operation"].as_str().unwrap_or("");
    let options = &job["options"];
    let snapshot = &job["snapshot"];
    let started = Instant::now();
    let work = storage::validated_source(snapshot, |file| {
        if ["probe", "decode"].contains(&operation) {
            let header = inspect_header(file, snapshot)?;
            if header["status"] == "invalid" {
                return Ok(
                    json!({"state":"failed","data":{"media_health":{"status":"invalid","reason":header["reason"],"warnings":[],"coverage":HEADER_COVERAGE}},"error":header["reason"]}),
                );
            }
        }
        let data = match operation {
            "hash" | "verify" => {
                let mut digest = Sha256::new();
                let mut buffer = vec![0; 1024 * 1024];
                let mut size = 0u64;
                loop {
                    if cancel.load(Ordering::Relaxed) {
                        return Ok(failure(operation, "cancelled", "processing cancelled", ""));
                    }
                    if started.elapsed().as_secs_f64()
                        > options["timeout"].as_f64().unwrap_or(120.0)
                    {
                        return Ok(failure(
                            operation,
                            "timeout",
                            "hash exceeded elapsed-time limit",
                            "",
                        ));
                    }
                    let n = file.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    digest.update(&buffer[..n]);
                    size += n as u64;
                }
                json!({"algorithm":"sha256","digest":format!("{:x}",digest.finalize()),"size":size,"bytes_read":size})
            }
            "sniff" => {
                let mut bytes =
                    vec![0; options["max_bytes"].as_u64().unwrap_or(4096).min(4096) as usize];
                let n = file.read(&mut bytes)?;
                bytes.truncate(n);
                let mut mime = None;
                for (signature, kind) in [
                    (b"\x89PNG\r\n\x1a\n".as_slice(), "image/png"),
                    (b"\xff\xd8\xff", "image/jpeg"),
                    (b"GIF8", "image/gif"),
                    (b"fLaC", "audio/flac"),
                    (b"OggS", "application/ogg"),
                    (b"ID3", "audio/mpeg"),
                    (b"\x1a\x45\xdf\xa3", "video/x-matroska"),
                    (b"%PDF-", "application/pdf"),
                    (b"PK\x03\x04", "application/zip"),
                ] {
                    if bytes.starts_with(signature) {
                        mime = Some(kind);
                        break;
                    }
                }
                if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE") {
                    mime = Some("audio/wav");
                }
                if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
                    mime = Some("image/webp");
                }
                if bytes.get(4..8) == Some(b"ftyp") {
                    mime = Some("video/mp4");
                }
                json!({"mime":mime,"extension":extension(snapshot["path"].as_str().unwrap_or("")),"bytes_read":n,"coverage":"signature only"})
            }
            "text" => match crate::document::extract(
                file,
                snapshot["path"].as_str().unwrap_or(""),
                options,
                cancel,
            ) {
                Ok(data) => data,
                Err(error) => {
                    return Ok(failure(
                        operation,
                        &error.state,
                        error.to_string(),
                        &String::from_utf8_lossy(&error.stderr),
                    ));
                }
            },
            "probe" | "decode" => {
                let tool = &options["extractor"];
                if *tool != tool_signature(operation) {
                    return Ok(failure(
                        operation,
                        "changed",
                        "extractor changed since enqueue; enqueue a new job",
                        "",
                    ));
                }
                if tool["available"] != true {
                    return Ok(failure(
                        operation,
                        "unsupported",
                        format!(
                            "optional tool {} is unavailable",
                            tool["tool"].as_str().unwrap_or("unknown")
                        ),
                        "",
                    ));
                }
                let fd = file.as_raw_fd();
                let source = format!("/dev/fd/{fd}");
                let args = if operation == "probe" {
                    vec![
                        tool["tool"].as_str().unwrap().to_owned(),
                        "-v".into(),
                        "warning".into(),
                        "-protocol_whitelist".into(),
                        "file,pipe".into(),
                        "-format_whitelist".into(),
                        format!("{FORMATS},srt,webvtt,ass"),
                        "-max_alloc".into(),
                        "33554432".into(),
                        "-threads".into(),
                        "1".into(),
                        "-probesize".into(),
                        options["analysis_bytes"].to_string(),
                        "-analyzeduration".into(),
                        options["analysis_us"].to_string(),
                        "-show_format".into(),
                        "-show_streams".into(),
                        "-show_chapters".into(),
                        "-show_error".into(),
                        "-of".into(),
                        "json".into(),
                        source,
                    ]
                } else {
                    vec![
                        tool["tool"].as_str().unwrap().to_owned(),
                        "-v".into(),
                        "error".into(),
                        "-xerror".into(),
                        "-nostdin".into(),
                        "-protocol_whitelist".into(),
                        "file,pipe".into(),
                        "-format_whitelist".into(),
                        FORMATS.into(),
                        "-max_alloc".into(),
                        "33554432".into(),
                        "-i".into(),
                        source,
                        "-map".into(),
                        "0:v?".into(),
                        "-map".into(),
                        "0:a?".into(),
                        "-threads".into(),
                        "1".into(),
                        "-f".into(),
                        "null".into(),
                        "-".into(),
                    ]
                };
                let output = match command_output(
                    &args,
                    CommandOptions {
                        pass_fds: &[fd],
                        timeout: Duration::from_secs(options["timeout"].as_u64().unwrap_or(20)),
                        maximum: options["max_bytes"].as_u64().unwrap_or(2 * 1024 * 1024) as usize,
                        cancel: Some(cancel),
                        ..Default::default()
                    },
                ) {
                    Ok(output) => output,
                    Err(e) => {
                        return Ok(failure(
                            operation,
                            if e.state == "unavailable" {
                                "failed"
                            } else {
                                &e.state
                            },
                            e.to_string(),
                            &String::from_utf8_lossy(&e.stderr),
                        ));
                    }
                };
                if operation == "probe" {
                    let required = options["required_streams"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let data: Value = match catabolic_core::decode_json_bytes(&output.stdout) {
                        Ok(v) => v,
                        Err(e) => return Ok(failure(operation, "failed", e.to_string(), "")),
                    };
                    match normalize_probe(
                        data,
                        &required,
                        options["analysis_bytes"].as_u64().unwrap_or(0),
                        options["analysis_us"].as_u64().unwrap_or(0),
                        &String::from_utf8_lossy(&output.stderr),
                    ) {
                        Ok(v) => v,
                        Err(e) => {
                            return Ok(failure(
                                operation,
                                if e.0 == "probe exceeds 256 streams" {
                                    "partial"
                                } else {
                                    "failed"
                                },
                                e.0,
                                "",
                            ));
                        }
                    }
                } else {
                    json!({"decoded":true,"coverage":"complete audio/video decode","media_health":{"status":"not_detected","reason":null,"warnings":[],"coverage":"complete audio/video decode"}})
                }
            }
            _ => {
                return Ok(failure(
                    operation,
                    "unsupported",
                    "native analysis operation is not implemented",
                    "",
                ));
            }
        };
        let invalid = data["media_health"]["status"] == "invalid";
        Ok(
            json!({"state":if invalid {"failed"} else {"complete"},"error":if invalid {data["media_health"]["reason"].clone()} else {Value::Null},"data":data,"elapsed_seconds":started.elapsed().as_secs_f64()}),
        )
    });
    match work {
        Ok(value) => value,
        Err(error) => {
            let changed = [
                "source is not recorded",
                "source changed",
                "source revision changed",
                "source root changed",
                "root identity changed",
                "bound volume identity",
                "invalid source path",
                "root must be absolute",
            ]
            .iter()
            .any(|prefix| error.0.starts_with(prefix));
            if changed {
                failure(operation, "changed", error.0, "")
            } else {
                let diagnostic = error.0.clone();
                let mut result = failure(operation, "failed", error.0, &diagnostic);
                if ["probe", "decode"].contains(&operation) {
                    result["data"]["media_health"] =
                        failure_health(operation, "io_error", &diagnostic);
                }
                result
            }
        }
    }
}

/// Run exactly the requested queued/abandoned jobs. Each operation is detached
/// with a bounded heartbeat, then its current fence is rechecked before commit.
struct HeartbeatGuard {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for HeartbeatGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn run(
    store: &mut Store,
    profile: &str,
    job_ids: &[String],
    cancel: Arc<AtomicBool>,
) -> Result<Value> {
    if job_ids.len() > 1000 {
        return Err(Error("job_ids must contain at most 1000 IDs".into()));
    }
    let mut counts = serde_json::Map::new();
    for id in job_ids.iter().collect::<BTreeSet<_>>() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let job = storage::get_job(store, profile, id)?;
        if !matches!(job["state"].as_str(), Some("queued" | "running"))
            || (job["state"] == "running" && storage::active(store, id, now())?)
        {
            continue;
        }
        let started_at = catabolic_store::mutation::timestamp()?;
        let claim = storage::start(store, profile, id, now())?;
        let path = store.path.clone();
        let db_id = store.database_id.clone();
        let worker_claim = claim.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let heartbeat_stop = Arc::clone(&stop);
        let task_cancel = Arc::clone(&cancel);
        let outcome = store.detached(|| {
            let heartbeat = std::thread::spawn(move || {
                let mut last = Instant::now();
                while !heartbeat_stop.load(Ordering::Relaxed) {
                    for _ in 0..50 {
                        if heartbeat_stop.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    if let Ok(session) = Store::open(&path, true, false) {
                        if session.database_id != db_id
                            || storage::heartbeat(&session, &worker_claim, now()).is_err()
                        {
                            task_cancel.store(true, Ordering::Relaxed);
                            return;
                        }
                        last = Instant::now();
                    }
                    if last.elapsed() > Duration::from_secs(45) {
                        task_cancel.store(true, Ordering::Relaxed);
                        return;
                    }
                }
            });
            let _heartbeat = HeartbeatGuard {
                stop: Arc::clone(&stop),
                thread: Some(heartbeat),
            };
            Ok(process(&job, &cancel))
        });
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(e) => {
                let _ = storage::release(store, &claim);
                return Err(e);
            }
        };
        let state = match storage::complete(
            store,
            profile,
            &claim,
            &outcome,
            now(),
            30,
            Some(&started_at),
        ) {
            Ok(state) => state,
            Err(error) => {
                let _ = storage::release(store, &claim);
                return Err(error);
            }
        };
        let count = counts.get(&state).and_then(Value::as_u64).unwrap_or(0);
        counts.insert(state, json!(count + 1));
    }
    Ok(
        json!({"processed":counts.values().filter_map(Value::as_u64).sum::<u64>(),"complete":counts.keys().all(|k|k=="complete"),"counts":counts,"retried":0,"reused_physical_results":0}),
    )
}

pub fn put_operation(store: &Store, name: &str, definition: &Value) -> Result<Value> {
    let object = definition
        .as_object()
        .ok_or_else(|| Error("invalid version 1 operation definition".into()))?;
    if object
        .keys()
        .any(|k| !["version", "kind", "operation", "options"].contains(&k.as_str()))
        || definition
            .get("version")
            .is_some_and(|v| v.as_u64() != Some(1))
    {
        return Err(Error("invalid version 1 operation definition".into()));
    }
    if definition["kind"] != "analysis" {
        return Err(Error(
            "native immutable operations currently support analysis only".into(),
        ));
    }
    let operation = definition["operation"]
        .as_str()
        .ok_or_else(|| Error("operation must be a name and options must be an object".into()))?;
    let options = definition.get("options").cloned().unwrap_or(json!({}));
    operation_config(operation, &options)?;
    storage::operation_put(
        store,
        name,
        &json!({"version":1,"kind":"analysis","operation":operation,"options":options}),
    )
}
