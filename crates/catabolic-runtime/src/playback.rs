// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Supervised native playback encodes, bounded and never published to catalog.
use crate::{
    process::{CommandFailure, CommandOptions, command_output},
    rendering,
};
use catabolic_core::{
    decode_json,
    playback::{self as policy, Event, Evidence, State},
};
use catabolic_store::{
    Error, Result, Store, access::Access, media_mutation::validated_source, playback as sessions,
    playback_cache as cache,
};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{Seek, SeekFrom},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    time::{Duration, Instant},
};

fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .ok_or_else(|| Error(format!("missing playback {key}")))
}
fn decode(v: &Value, key: &str) -> Result<Value> {
    Ok(decode_json(text(v, key)?)?)
}
fn state(job: &Value) -> Result<State> {
    Ok(match text(job, "state")? {
        "queued" => State::Queued,
        "running" => State::Running,
        "ready" => State::Ready,
        "failed" => State::Failed,
        "cancelled" => State::Cancelled,
        "stale" => State::Stale,
        _ => return Err(Error("invalid playback state".into())),
    })
}
fn evidence(store: &Store, job: &Value, tools: &Value, within_budget: bool) -> Result<Evidence> {
    Ok(Evidence {
        authorized_viewers: sessions::interested(store, job)?,
        source_current: validated_source(&decode(job, "snapshot")?, |_| Ok(())).is_ok(),
        tools_current: decode(job, "tools")? == *tools,
        destination_current: cache::destination_root(
            store,
            text(job, "profile")?,
            &decode(job, "destination")?,
        )
        .is_ok(),
        within_budget,
    })
}
pub fn capabilities() -> Result<Value> {
    let mut caps = rendering::capabilities()?;
    let ready = caps["presets"].as_array().is_some_and(|p| {
        p.iter()
            .any(|p| p["name"] == "h264-720p" && p["available"] == true)
    });
    let mut hls = false;
    if ready {
        let output = command_output(
            &[
                text(&caps["tools"]["ffmpeg"], "tool")?,
                "-hide_banner",
                "-muxers",
            ],
            Default::default(),
        )
        .map_err(|e| Error(e.to_string()))?;
        hls = String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.split_whitespace().nth(1) == Some("hls"));
    }
    caps["playback_hls"] = json!(hls);
    Ok(caps)
}
pub fn execute(access: &mut Access, operation: &str, request: &Value) -> Result<Value> {
    let tools = access.detached(|| rendering::tools(false))?;
    execute_with_tools(access, operation, request, &tools)
}
/// The caller may discover tools before opening its authenticated owner.
pub fn execute_with_tools(
    access: &mut Access,
    operation: &str,
    request: &Value,
    tools: &Value,
) -> Result<Value> {
    match operation {
        "admit" => sessions::admit(
            access,
            &request["body"],
            text(request, "idempotency_key")?,
            request
                .get("ttl")
                .map(|v| {
                    v.as_u64()
                        .ok_or_else(|| Error("invalid_playback_request".into()))
                })
                .transpose()?
                .unwrap_or(3600),
            tools,
        ),
        "status" => sessions::status(access, text(request, "session_id")?, tools),
        "cancel" => sessions::cancel(access, text(request, "session_id")?, tools),
        "playlist" => {
            sessions::with_media(access, text(request, "session_id")?, None, tools, |media| {
                match media {
                    sessions::Media::Playlist(bytes) => Ok(
                        json!({"playlist":String::from_utf8(bytes).map_err(|_|Error("invalid_playback_cache".into()))?}),
                    ),
                    _ => unreachable!(),
                }
            })
        }
        "inspect-segment" => {
            if request.get("range").is_some() {
                return Err(Error("range_not_satisfiable".into()));
            }
            sessions::with_media(
                access,
                text(request, "session_id")?,
                Some(text(request, "segment")?),
                tools,
                |media| match media {
                    sessions::Media::Segment(file) => Ok(
                        json!({"size":file.metadata()?.len(),"sha256":rendering::digest(&mut file.try_clone()?)?,"content_type":"video/mp2t"}),
                    ),
                    _ => unreachable!(),
                },
            )
        }
        _ => Err(Error("unknown playback operation".into())),
    }
}
fn hls_command(
    source: &File,
    recipe: &Value,
    tools: &Value,
    before: &Value,
) -> Result<Vec<String>> {
    if let Some(error) = rendering::input_error(recipe, before)? {
        return Err(Error(error));
    }
    if !["h264-720p", "h264-1080p"].contains(&recipe["preset"].as_str().unwrap_or("")) {
        return Err(Error("playback_profile_unsupported".into()));
    }
    let videos = rendering::streams(before, "video");
    let video = recipe["video_stream"].as_u64().unwrap_or(0) as usize;
    let video = videos
        .get(video)
        .ok_or_else(|| Error("input lacks requested video".into()))?;
    let mut args = [
        text(&tools["ffmpeg"], "tool")?,
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-xerror",
        "-protocol_whitelist",
        "file,pipe",
        "-format_whitelist",
        rendering::FORMATS,
        "-max_alloc",
        "33554432",
        "-threads",
        "1",
        "-i",
    ]
    .map(str::to_owned)
    .to_vec();
    args.push(format!("/dev/fd/{}", source.as_raw_fd()));
    args.extend([
        "-map".into(),
        format!("0:{}", video["index"]),
        "-map_metadata".into(),
        "-1".into(),
        "-map_chapters".into(),
        "-1".into(),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        recipe["encoder_speed"].as_str().unwrap_or("medium").into(),
        "-crf".into(),
        recipe["crf"].as_i64().unwrap_or(23).to_string(),
        "-tune".into(),
        "zerolatency".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-vf".into(),
        format!(
            "scale=w=-2:h='min(ih,{})'",
            if recipe["preset"] == "h264-720p" {
                720
            } else {
                1080
            }
        ),
        "-flags".into(),
        "+cgop".into(),
        "-force_key_frames".into(),
        "expr:gte(t,n_forced*2)".into(),
        "-sc_threshold".into(),
        "0".into(),
    ]);
    let audio = rendering::streams(before, "audio");
    let selected = if recipe.get("audio_stream").is_some_and(Value::is_null) {
        None
    } else {
        Some(recipe["audio_stream"].as_u64().unwrap_or(0) as usize)
    };
    if let Some(selected) = selected {
        if !audio.is_empty() {
            let stream = audio
                .get(selected)
                .ok_or_else(|| Error("input lacks requested audio".into()))?;
            args.extend([
                "-map".into(),
                format!("0:{}", stream["index"]),
                "-c:a".into(),
                "aac".into(),
                "-b:a".into(),
                format!("{}k", recipe["audio_bitrate_kbps"].as_i64().unwrap_or(160)),
                "-ac".into(),
                "2".into(),
                "-ar".into(),
                "48000".into(),
            ]);
        } else if recipe.get("audio_stream").is_some() {
            return Err(Error("input lacks requested audio".into()));
        }
    }
    args.extend(
        [
            "-threads",
            "1",
            "-filter_threads",
            "1",
            "-filter_complex_threads",
            "1",
            "-f",
            "hls",
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "event",
            "-hls_flags",
            "temp_file+independent_segments",
            "-hls_segment_filename",
            "segment-%06d.ts",
            "index.m3u8",
        ]
        .map(str::to_owned),
    );
    Ok(args)
}
fn budget(directory: &File, recipe: &Value) -> Result<bool> {
    Ok(cache::available_bytes(directory)?
        >= u128::from(
            recipe["reserve_bytes"]
                .as_u64()
                .ok_or_else(|| Error("invalid playback recipe".into()))?,
        )
        && cache::cache_size(directory)?
            < recipe["max_output_bytes"]
                .as_u64()
                .ok_or_else(|| Error("invalid playback recipe".into()))?)
}
fn fail_command(error: CommandFailure) -> Error {
    if error.message.starts_with("playback_") {
        Error(error.message)
    } else {
        Error(format!("playback_{}", error.state))
    }
}
fn duration(before: &Value) -> Option<f64> {
    before["format"]["duration"]
        .as_f64()
        .or_else(|| {
            before["format"]["duration"]
                .as_str()
                .and_then(|s| s.parse().ok())
        })
        .filter(|n| n.is_finite() && *n > 0.)
}
fn run_job(
    store: &mut Store,
    mut job: Value,
    caps: &Value,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Value> {
    let id = text(&job, "id")?.to_owned();
    let outcome: Result<()> = (|| {
        let recipe = decode(&job, "recipe")?;
        let tools = decode(&job, "tools")?;
        let current_tools = store.detached(|| rendering::tools(false))?;
        let mut initial = evidence(store, &job, &current_tools, true)?;
        initial.tools_current &= caps["playback_hls"] == true;
        let decision = policy::decide(state(&job)?, initial, Event::Start);
        if decision.state != State::Running {
            sessions::apply_decision(store, &job, &decision)?;
            return Ok(());
        }
        let root =
            cache::destination_root(store, text(&job, "profile")?, &decode(&job, "destination")?)?;
        let directory = cache::create_directory(&root, &job)?;
        let stat = directory.metadata()?;
        sessions::start_job(store, &id, stat.dev(), stat.ino())?;
        job["state"] = json!("running");
        job["directory_device"] = json!(stat.dev());
        job["directory_inode"] = json!(stat.ino());
        let path = store.path.clone();
        let database = store.database_id.clone();
        let source_snapshot = decode(&job, "snapshot")?;
        let report = store.detached(|| {
            validated_source(&source_snapshot, |source| {
                let before = rendering::probe(
                    source,
                    &tools,
                    recipe["timeout"].as_u64().unwrap_or(120).min(20),
                    false,
                )?;
                source.seek(SeekFrom::Start(0))?;
                let command = hls_command(source, &recipe, &tools, &before)?;
                let mut last = Instant::now() - Duration::from_secs(1);
                let mut poll = || -> std::result::Result<(), CommandFailure> {
                    let checked: Result<()> = (|| {
                        // Storage is checked every subprocess polling iteration;
                        // authorization/identity polls are bounded to 4 Hz, each
                        // with newly authenticated stored-token snapshots.
                        if !budget(&directory, &recipe)? {
                            return Err(Error("playback_storage_budget".into()));
                        }
                        if last.elapsed() < Duration::from_millis(250) {
                            return Ok(());
                        }
                        last = Instant::now();
                        let current_tools = rendering::tools(false)?;
                        let current = Store::open(&path, true, false)?;
                        if current.database_id != database {
                            return Err(Error("playback_catalog_changed".into()));
                        }
                        let current_job = sessions::job(&current, &id)?;
                        if current_job["state"] != "running" {
                            return Err(Error("playback_claim_lost".into()));
                        }
                        let evidence = evidence(&current, &current_job, &current_tools, true)?;
                        let decision = policy::decide(State::Running, evidence, Event::Poll);
                        if decision.stop {
                            return Err(Error(
                                decision.error.unwrap_or("playback_encode_failed").into(),
                            ));
                        }
                        // The named directory must still identify the descriptor
                        // used as FFmpeg's cwd, not a replacement at that name.
                        cache::job_directory(&current, &current_job)?;
                        let p = cache::playlist(&directory)?;
                        sessions::progress(
                            &current,
                            &current_job,
                            p.segments.len(),
                            p.seconds,
                            std::process::id(),
                        )
                    })();
                    match checked {
                        // A writer collision is transient; we still enforced
                        // cache byte/free-space budgets before returning.
                        Err(e)
                            if e.0.contains("another Catabolic writer")
                                || e.0.contains("database is locked") =>
                        {
                            Ok(())
                        }
                        Err(e) => Err(CommandFailure::new(
                            if e.0 == "playback_cancelled" {
                                "cancelled"
                            } else {
                                "failed"
                            },
                            e.0,
                        )),
                        Ok(()) => Ok(()),
                    }
                };
                command_output(
                    &command,
                    CommandOptions {
                        pass_fds: &[source.as_raw_fd()],
                        cwd_fd: Some(directory.as_raw_fd()),
                        timeout: Duration::from_secs(recipe["timeout"].as_u64().unwrap_or(120)),
                        umask: Some(0o077),
                        on_poll: Some(&mut poll),
                        cancel: Some(cancel),
                        ..Default::default()
                    },
                )
                .map_err(fail_command)?;
                if !budget(&directory, &recipe)? {
                    return Err(Error("playback_storage_budget".into()));
                }
                let p = cache::playlist(&directory)?;
                if p.segments.is_empty()
                    || !p.raw.as_deref().unwrap_or("").contains("#EXT-X-ENDLIST")
                {
                    return Err(Error("incomplete_playback_output".into()));
                }
                for segment in &p.segments {
                    cache::open_segment(&directory, segment, &p)?;
                }
                let expected = duration(&before);
                if recipe["require_duration"] == true && expected.is_none() {
                    return Err(Error("playback_duration_unknown".into()));
                }
                if expected.is_some_and(|v| (p.seconds - v).abs() > 1.0f64.max(v * 0.02)) {
                    return Err(Error("playback_duration_mismatch".into()));
                }
                if recipe["max_size_ratio"].as_f64().is_some_and(|ratio| {
                    ratio > 0.
                        && cache::cache_size(&directory)
                            .map(|s| {
                                s as f64
                                    > source
                                        .metadata()
                                        .map(|s| s.len() as f64 * ratio)
                                        .unwrap_or(-1.)
                            })
                            .unwrap_or(true)
                }) {
                    return Err(Error("playback_size_ratio".into()));
                }
                Ok((p.segments.len(), p.seconds))
            })
        })?;
        let current_tools = store.detached(|| rendering::tools(false))?;
        let current = sessions::job(store, &id)?;
        if current["state"] != "running" {
            return Err(Error("playback_claim_lost".into()));
        }
        cache::job_directory(store, &current)?;
        let decision = policy::decide(
            State::Running,
            evidence(
                store,
                &current,
                &current_tools,
                budget(&directory, &recipe)?,
            )?,
            Event::Complete { valid_output: true },
        );
        sessions::complete_job(store, &current, &decision, report.0, report.1)?;
        Ok(())
    })();
    if let Err(error) = outcome {
        let code = if error.0.starts_with("playback_") || error.0 == "incomplete_playback_output" {
            error.0
        } else {
            "playback_encode_failed".into()
        };
        sessions::fail_job(store, &id, &code)?;
    }
    Ok(json!({"processed":1,"job_id":id,"state":sessions::job(store,&id)?["state"]}))
}
pub(crate) fn recover(store: &Store, profile: &str) -> Result<usize> {
    let jobs = sessions::running_jobs(store, profile)?;
    for job in &jobs {
        let decision = policy::decide(
            State::Running,
            Evidence {
                authorized_viewers: 0,
                source_current: false,
                tools_current: false,
                destination_current: false,
                within_budget: false,
            },
            Event::Recover,
        );
        sessions::apply_decision(store, job, &decision)?;
    }
    Ok(jobs.len())
}
/// One playback step under the caller's API-worker supervisor lock.
pub(crate) fn tick(store: &mut Store, profile: &str, caps: &Value) -> Result<Option<Value>> {
    tick_with_cancel(
        store,
        profile,
        caps,
        &std::sync::atomic::AtomicBool::new(false),
    )
}
pub(crate) fn tick_with_cancel(
    store: &mut Store,
    profile: &str,
    caps: &Value,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Option<Value>> {
    cache::cleanup(store, profile)?;
    if cancel.load(std::sync::atomic::Ordering::Acquire) {
        return Ok(Some(json!({"processed":0})));
    }
    sessions::next_job(store, profile)?
        .map(|job| run_job(store, job, caps, cancel))
        .transpose()
}
/// A single Python-compatible supervisor lock protects recovery, cleanup and
/// work admission while the catalog writer itself detaches around slow tools.
pub fn worker(store: &mut Store, profile: &str, operation: &str, request: &Value) -> Result<Value> {
    if operation == "capabilities" {
        // Capabilities have no need for the caller's database snapshot.
        return if store.db.is_autocommit() {
            store.detached(capabilities)
        } else {
            Err(Error(
                "capabilities require a standalone transaction boundary".into(),
            ))
        };
    }
    if !matches!(operation, "run" | "tick" | "recover" | "cleanup") {
        return Err(Error("unknown playback worker operation".into()));
    }
    if !store.writable || !store.owns_writer_lock() {
        return Err(Error(
            "playback worker requires a writable catalog session".into(),
        ));
    }
    if !store.db.is_autocommit() {
        return Err(Error(
            "playback worker requires a standalone transaction boundary".into(),
        ));
    }
    let mut lockpath = store.path.as_os_str().to_os_string();
    lockpath.push(".api-worker");
    let _supervisor = catabolic_store::writer_lock(std::path::Path::new(&lockpath))?;
    if operation == "cleanup" {
        cache::cleanup(store, profile)?;
        return Ok(json!({"complete":true}));
    }
    let recovered = recover(store, profile)?;
    if operation == "recover" {
        return Ok(json!({"recovered":recovered}));
    }
    let caps = store.detached(capabilities)?;
    let once = operation == "tick" || request["once"] != false;
    let result = (|| {
        loop {
            sessions::worker_heartbeat(store, profile, std::process::id(), &caps)?;
            let result = tick(store, profile, &caps)?.unwrap_or_else(|| json!({"processed":0}));
            if once {
                return Ok(result);
            }
            store.detached(|| {
                std::thread::sleep(Duration::from_millis(250));
                Ok(())
            })?;
        }
    })();
    let removed = sessions::worker_remove(store, profile, std::process::id());
    result.and_then(|v| removed.map(|_| v))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readonly_worker_has_no_filesystem_or_catalog_effects() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.sqlite3");
        catabolic_store::migration::initialize(&path).unwrap();
        let before = std::fs::read(&path).unwrap();
        let mut store = Store::open(&path, false, false).unwrap();
        for operation in ["run", "tick", "recover", "cleanup"] {
            assert!(
                worker(&mut store, "default", operation, &json!({}))
                    .unwrap_err()
                    .0
                    .contains("writable catalog session")
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert!(!root.path().join("catalog.sqlite3.api-worker.lock").exists());
        }
        drop(store);
        let mut writer = Store::open(&path, true, false).unwrap();
        writer.db.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert!(
            worker(&mut writer, "default", "cleanup", &json!({}))
                .unwrap_err()
                .0
                .contains("standalone transaction boundary")
        );
        writer.db.execute_batch("ROLLBACK").unwrap();
        assert!(!root.path().join("catalog.sqlite3.api-worker.lock").exists());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}
