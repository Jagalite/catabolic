// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Multi-descriptor muxing with exact packet, timing and attachment validation.
use crate::{
    packet_verification,
    process::{CommandFailure, CommandOptions, command_output},
    rendering,
};
use catabolic_store::{Error, Result, media_mutation::validated_source};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{Seek, SeekFrom},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
fn tool(identity: &Value, name: &str) -> Result<String> {
    identity[name]["tool"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Error(format!("missing {name} identity")))
}
fn push(args: &mut Vec<String>, parts: &[&str]) {
    args.extend(parts.iter().map(|s| s.to_string()));
}
fn with_sources<T>(
    snapshots: &[Value],
    files: &mut Vec<File>,
    operation: &mut dyn FnMut(&mut [File]) -> Result<T>,
) -> Result<T> {
    if let Some((first, rest)) = snapshots.split_first() {
        validated_source(first, |file| {
            files.push(file.try_clone()?);
            let result = with_sources(rest, files, operation);
            files.pop();
            result
        })
    } else {
        operation(files)
    }
}
fn execute(
    args: &[String],
    fds: &[i32],
    maximum: usize,
    timeout: u64,
    cancel: &AtomicBool,
    poll: &mut dyn FnMut() -> Result<()>,
) -> Result<Vec<u8>> {
    let mut callback = || poll().map_err(|e| CommandFailure::new("failed", e.0));
    Ok(command_output(
        args,
        CommandOptions {
            pass_fds: fds,
            maximum,
            timeout: Duration::from_secs(timeout),
            cancel: Some(cancel),
            on_poll: Some(&mut callback),
            ..Default::default()
        },
    )
    .map_err(|e| Error(e.to_string()))?
    .stdout)
}
fn packets(
    file: &mut File,
    index: &Value,
    identity: &Value,
    timeout: u64,
    cancel: &AtomicBool,
    poll: &mut dyn FnMut() -> Result<()>,
) -> Result<Value> {
    file.seek(SeekFrom::Start(0))?;
    let fd = file.as_raw_fd();
    let mut args = vec![tool(identity, "ffprobe")?];
    push(
        &mut args,
        &[
            "-v",
            "error",
            "-protocol_whitelist",
            "file,pipe",
            "-format_whitelist",
            rendering::FORMATS,
            "-max_alloc",
            "33554432",
            "-select_streams",
            &index.to_string(),
            "-show_packets",
            "-show_entries",
            "packet=pts_time,dts_time,duration_time,data_hash",
            "-show_data_hash",
            "sha256",
            "-of",
            "json",
            &format!("/dev/fd/{fd}"),
        ],
    );
    let mut value = catabolic_core::decode_json_bytes(&execute(
        &args,
        &[fd],
        64 * 1024 * 1024,
        timeout,
        cancel,
        poll,
    )?)?;
    let packets = value
        .get_mut("packets")
        .map(Value::take)
        .filter(|v| {
            v.as_array().is_some_and(|a| {
                !a.is_empty()
                    && a.iter()
                        .all(|p| p["data_hash"].as_str().is_some_and(|s| !s.is_empty()))
            })
        })
        .ok_or_else(|| Error("component packet evidence is incomplete".into()))?;
    Ok(packets)
}
pub fn render(
    output: &mut File,
    recipe: &Value,
    identity: &Value,
    cancel: &AtomicBool,
    outer_poll: &mut dyn FnMut() -> Result<()>,
    package: &Value,
) -> Result<rendering::Rendered> {
    let timeout = recipe["timeout"]
        .as_u64()
        .filter(|n| (1..=86400).contains(n))
        .ok_or_else(|| Error("invalid component mux timeout".into()))?;
    let started = Instant::now();
    let mut poll = || {
        outer_poll()?;
        if cancel.load(Ordering::Relaxed) {
            return Err(Error("component mux cancelled".into()));
        }
        if started.elapsed() > Duration::from_secs(timeout) {
            return Err(Error("component mux exceeded operation time budget".into()));
        }
        Ok(())
    };
    let mut settings = recipe.clone();
    for key in ["version", "preset", "output_definition_id"] {
        settings
            .as_object_mut()
            .ok_or_else(|| Error("invalid recipe".into()))?
            .remove(key);
    }
    if recipe["preset"] != "component-mux" {
        return Err(Error(
            "multi-input packaging requires a component-mux operation".into(),
        ));
    }
    for (key, value) in rendering::definition("component-mux", &settings)?
        .as_object()
        .unwrap()
    {
        if recipe.get(key) != Some(value) {
            return Err(Error("invalid or incomplete immutable recipe".into()));
        }
    }
    let entries = package["components"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 17)
        .ok_or_else(|| Error("invalid component package".into()))?;
    let dependencies = package["dependencies"]
        .as_array()
        .filter(|a| a.len() <= 544)
        .ok_or_else(|| Error("invalid component dependencies".into()))?;
    let snapshots = entries
        .iter()
        .map(|e| e["occurrence"]["snapshot"].clone())
        .chain(dependencies.iter().map(|d| d["snapshot"].clone()))
        .collect::<Vec<_>>();
    with_sources(&snapshots, &mut vec![], &mut |files| {
        poll()?;
        let output_stat = output.metadata()?;
        if !output_stat.is_file() || output_stat.len() != 0 {
            return Err(Error(
                "mux requires an empty exclusive regular output".into(),
            ));
        }
        let out_fd = output.as_raw_fd();
        let mut args = vec![tool(identity, "ffmpeg")?];
        push(
            &mut args,
            &[
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-y",
                "-copyts",
            ],
        );
        let mut selected = vec![];
        for (number, entry) in entries.iter().enumerate() {
            let row = &entry["occurrence"];
            let file = &mut files[number];
            let st = file.metadata()?;
            if (st.dev(), st.ino()) == (output_stat.dev(), output_stat.ino()) {
                return Err(Error("mux input aliases output".into()));
            }
            if row["locator"]["convention"] != "ffprobe_absolute_stream_index" {
                return Err(Error(
                    "component mux requires an explicitly probed stream locator".into(),
                ));
            }
            let before =
                rendering::probe_with_poll(file, identity, 20, false, Some(cancel), &mut poll)?;
            let matches = before["streams"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|s| s["index"] == row["locator"]["index"])
                .collect::<Vec<_>>();
            if matches.len() != 1
                || matches[0]["codec_type"] != row["kind"]
                || matches[0]["codec_name"] != row["codec"]
            {
                return Err(Error(
                    "pinned component stream does not match its probe evidence".into(),
                ));
            }
            let mut stream = matches[0].clone();
            for key in ["language", "title"] {
                if !row[key].is_null() && stream["tags"][key].as_str().is_none_or(str::is_empty) {
                    if !stream["tags"].is_object() {
                        stream["tags"] = json!({});
                    }
                    stream["tags"][key] = row[key].clone();
                }
            }
            for (key, flag) in [
                ("default_flag", "default"),
                ("forced", "forced"),
                ("commentary", "comment"),
                ("hearing_impaired", "hearing_impaired"),
                ("visual_impaired", "visual_impaired"),
            ] {
                if !row[key].is_null() {
                    if !stream["disposition"].is_object() {
                        stream["disposition"] = json!({});
                    }
                    stream["disposition"][flag] = json!(if row[key] == true || row[key] == 1 {
                        1
                    } else {
                        0
                    });
                }
            }
            let offset = &entry["compatibility"]["offset_seconds"];
            if offset.as_f64().is_none_or(|n| !n.is_finite()) {
                return Err(Error("invalid accepted component offset".into()));
            }
            file.seek(SeekFrom::Start(0))?;
            push(
                &mut args,
                &[
                    "-protocol_whitelist",
                    "file,pipe",
                    "-format_whitelist",
                    rendering::FORMATS,
                    "-threads",
                    "1",
                    "-itsoffset",
                    &catabolic_core::encode(offset),
                    "-i",
                    &format!("/dev/fd/{}", file.as_raw_fd()),
                ],
            );
            selected.push(stream);
        }
        let supported = [
            "default",
            "forced",
            "dub",
            "original",
            "comment",
            "lyrics",
            "karaoke",
            "hearing_impaired",
            "visual_impaired",
            "clean_effects",
            "attached_pic",
        ];
        for (i, stream) in selected.iter().enumerate() {
            let flags = supported
                .iter()
                .filter(|key| stream["disposition"][**key] == 1)
                .copied()
                .collect::<Vec<_>>()
                .join("+");
            push(
                &mut args,
                &[
                    "-map",
                    &format!("{i}:{}", stream["index"]),
                    &format!("-disposition:{i}"),
                    if flags.is_empty() { "0" } else { &flags },
                ],
            );
        }
        push(
            &mut args,
            &["-map_metadata", "-1", "-map_chapters", "-1", "-c", "copy"],
        );
        for (i, stream) in selected.iter().enumerate() {
            for key in ["language", "title"] {
                if let Some(value) = stream["tags"][key].as_str() {
                    push(
                        &mut args,
                        &[&format!("-metadata:s:{i}"), &format!("{key}={value}")],
                    );
                }
            }
        }
        let mut attachments = vec![];
        for (i, dep) in dependencies.iter().enumerate() {
            if dep["purpose"] != "font" {
                return Err(Error(
                    "component mux supports font dependencies only".into(),
                ));
            }
            let filename = dep["snapshot"]["path"]
                .as_str()
                .and_then(|s| s.rsplit('/').next())
                .ok_or_else(|| Error("invalid font path".into()))?;
            let extension = filename
                .rsplit_once('.')
                .map(|(_, s)| s.to_ascii_lowercase())
                .unwrap_or_default();
            let mime = match extension.as_str() {
                "ttf" => "application/x-truetype-font",
                "otf" => "application/vnd.ms-opentype",
                _ => return Err(Error("unsupported font dependency format".into())),
            };
            let file = &mut files[entries.len() + i];
            push(
                &mut args,
                &[
                    "-attach",
                    &format!("/dev/fd/{}", file.as_raw_fd()),
                    &format!("-metadata:s:t:{i}"),
                    &format!("mimetype={mime}"),
                    &format!("-metadata:s:t:{i}"),
                    &format!("filename={filename}"),
                ],
            );
            attachments.push(json!({"file_id":dep["file_id"],"revision":dep["revision"],"sha256":rendering::digest_with_poll(file,&mut poll)?}));
            file.seek(SeekFrom::Start(0))?;
        }
        push(
            &mut args,
            &[
                "-max_alloc",
                "33554432",
                "-threads",
                "1",
                "-fs",
                &recipe["max_output_bytes"].to_string(),
                "-f",
                "matroska",
                &format!("/dev/fd/{out_fd}"),
            ],
        );
        let mut fds = files.iter().map(AsRawFd::as_raw_fd).collect::<Vec<_>>();
        fds.push(out_fd);
        execute(&args, &fds, 2 * 1024 * 1024, timeout, cancel, &mut poll)?;
        poll()?;
        output.sync_all()?;
        let output_revision = rendering::file_revision(output)?;
        let after =
            rendering::probe_with_poll(output, identity, 20, false, Some(cancel), &mut poll)?;
        let streams = after["streams"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["codec_type"] != "attachment")
            .collect::<Vec<_>>();
        if streams.len() != selected.len()
            || streams.iter().zip(&selected).any(|(out, src)| {
                out["codec_type"] != src["codec_type"] || out["codec_name"] != src["codec_name"]
            })
        {
            return Err(Error(
                "mux output does not contain exactly the selected component representations".into(),
            ));
        }
        if after["streams"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["codec_type"] == "attachment")
            .count()
            != attachments.len()
        {
            return Err(Error("mux output lost a required font attachment".into()));
        }
        if !attachments.is_empty() {
            output.seek(SeekFrom::Start(0))?;
            let mut args = vec![tool(identity, "ffprobe")?];
            push(
                &mut args,
                &[
                    "-v",
                    "error",
                    "-protocol_whitelist",
                    "file,pipe",
                    "-format_whitelist",
                    "matroska",
                    "-show_streams",
                    "-show_data_hash",
                    "sha256",
                    "-of",
                    "json",
                    &format!("/dev/fd/{out_fd}"),
                ],
            );
            let raw = catabolic_core::decode_json_bytes(&execute(
                &args,
                &[out_fd],
                2 * 1024 * 1024,
                timeout,
                cancel,
                &mut poll,
            )?)?;
            let hashes = raw["streams"]
                .as_array()
                .ok_or_else(|| Error("invalid attachment evidence".into()))?
                .iter()
                .filter(|s| s["codec_type"] == "attachment")
                .map(|s| {
                    s["extradata_hash"]
                        .as_str()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .trim_start_matches("sha256:")
                        .to_owned()
                })
                .collect::<Vec<_>>();
            if hashes
                != attachments
                    .iter()
                    .map(|a| a["sha256"].as_str().unwrap().to_owned())
                    .collect::<Vec<_>>()
            {
                return Err(Error("mux changed required font attachment bytes".into()));
            }
        }
        for (source, target) in selected.iter().zip(&streams) {
            for name in ["language", "title"] {
                if !source["tags"][name].is_null() && source["tags"][name] != target["tags"][name] {
                    return Err(Error("mux lost selected stream metadata".into()));
                }
            }
            for flag in [
                "default",
                "forced",
                "comment",
                "hearing_impaired",
                "visual_impaired",
            ] {
                if !source["disposition"][flag].is_null()
                    && source["disposition"][flag] != target["disposition"][flag]
                {
                    return Err(Error("mux changed selected stream flags".into()));
                }
            }
        }
        let mut verified = vec![];
        for (i, (entry, target)) in entries.iter().zip(&streams).enumerate() {
            let before = packets(
                &mut files[i],
                &entry["occurrence"]["locator"]["index"],
                identity,
                timeout,
                cancel,
                &mut poll,
            )?;
            let after = packets(
                output,
                &target["index"],
                identity,
                timeout,
                cancel,
                &mut poll,
            )?;
            verified.push(packet_verification::verify_packets(
                &before,
                &after,
                &entry["compatibility"]["offset_seconds"],
            )?);
        }
        let size = output.metadata()?.len();
        if size == 0
            || recipe["max_output_bytes"]
                .as_u64()
                .is_none_or(|limit| size >= limit)
        {
            return Err(Error(
                "mux output reached its byte budget or is empty".into(),
            ));
        }
        let sha256 = rendering::digest_with_poll(output, &mut poll)?;
        poll()?;
        if rendering::file_revision(output)? != output_revision {
            return Err(Error("mux output changed after validation".into()));
        }
        let validation = json!({"operation":"component-mux","output":after,"component_signature":package["signature"],"components":entries.iter().zip(&streams).map(|(entry,out)|json!({"occurrence_id":entry["occurrence"]["occurrence_id"],"output_stream_index":out["index"],"relationship":"mux_copy"})).collect::<Vec<_>>(),"dependencies":attachments,"packet_verification":verified,"size":size});
        Ok(rendering::Rendered {
            validation,
            output_revision,
            sha256,
            size,
        })
    })
}
