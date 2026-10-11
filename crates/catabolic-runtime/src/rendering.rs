// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Fixed FFmpeg presets, explicit descriptor effects and independent output checks.
//! Caller owns exclusive temporary output creation and durable publication.
use crate::{
    media,
    process::{CommandFailure, CommandOptions, command_output},
};
use catabolic_core::{encode, sha256};
use catabolic_store::{Error, Result};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::{fd::AsRawFd, unix::fs::FileExt},
    sync::atomic::AtomicBool,
    time::Duration,
};
#[derive(Clone, Copy)]
pub struct Preset {
    pub name: &'static str,
    pub extension: &'static str,
    pub muxer: &'static str,
    pub role: &'static str,
    pub kind: Option<&'static str>,
    pub codec: Option<&'static str>,
}
macro_rules! preset {
    ($n:literal,$e:literal,$m:literal,$r:literal,$k:expr,$c:expr) => {
        Preset {
            name: $n,
            extension: $e,
            muxer: $m,
            role: $r,
            kind: $k,
            codec: $c,
        }
    };
}
pub const PRESETS: &[Preset] = &[
    preset!(
        "thumbnail",
        "png",
        "image2",
        "thumbnail",
        Some("video"),
        Some("png")
    ),
    preset!(
        "image-jpeg",
        "jpg",
        "image2",
        "thumbnail",
        Some("video"),
        Some("mjpeg")
    ),
    preset!(
        "image-webp",
        "webp",
        "image2",
        "thumbnail",
        Some("video"),
        Some("libwebp")
    ),
    preset!(
        "preview",
        "mp4",
        "mp4",
        "extra",
        Some("video"),
        Some("libx264")
    ),
    preset!("remux-mkv", "mkv", "matroska", "primary", None, None),
    preset!("component-mux", "mkv", "matroska", "primary", None, None),
    preset!(
        "audio-flac",
        "flac",
        "flac",
        "custom:audio",
        Some("audio"),
        Some("flac")
    ),
    preset!(
        "audio-aac",
        "m4a",
        "mp4",
        "custom:audio",
        Some("audio"),
        Some("aac")
    ),
    preset!(
        "subtitle-srt",
        "srt",
        "srt",
        "subtitle",
        Some("subtitle"),
        Some("srt")
    ),
    preset!(
        "h264-720p",
        "mp4",
        "mp4",
        "primary",
        Some("video"),
        Some("libx264")
    ),
    preset!(
        "h264-1080p",
        "mp4",
        "mp4",
        "primary",
        Some("video"),
        Some("libx264")
    ),
    preset!(
        "av1-720p",
        "mkv",
        "matroska",
        "primary",
        Some("video"),
        Some("libsvtav1")
    ),
    preset!(
        "audio-opus",
        "opus",
        "opus",
        "custom:audio",
        Some("audio"),
        Some("libopus")
    ),
    preset!(
        "hdr-sdr-1080p",
        "mp4",
        "mp4",
        "primary",
        Some("video"),
        Some("libx264")
    ),
    preset!(
        "waveform",
        "png",
        "image2",
        "custom:waveform",
        Some("audio"),
        Some("png")
    ),
    preset!(
        "audio-normalize",
        "flac",
        "flac",
        "custom:audio",
        Some("audio"),
        Some("flac")
    ),
];
const VIDEO: &[&str] = &[
    "preview",
    "h264-720p",
    "h264-1080p",
    "av1-720p",
    "hdr-sdr-1080p",
];
const AUDIO: &[&str] = &["audio-flac", "audio-aac", "audio-opus", "audio-normalize"];
const PHOTO: &[&str] = &["image-jpeg", "image-webp"];
pub(crate) const FORMATS: &str = "mov,matroska,webm,wav,flac,mp3,ogg,aac,ac3,eac3,aiff,ape,asf,avi,mpeg,mpegts,png_pipe,jpeg_pipe,webp_pipe,gif,bmp_pipe,tiff_pipe,srt,webvtt,ass";
pub fn preset(name: &str) -> Result<Preset> {
    PRESETS
        .iter()
        .find(|p| p.name == name)
        .copied()
        .ok_or_else(|| Error("unknown rendering preset or invalid options".into()))
}
pub fn definition(name: &str, options: &Value) -> Result<Value> {
    preset(name)?;
    let options = options
        .as_object()
        .ok_or_else(|| Error("unknown rendering preset or invalid options".into()))?;
    let mut allowed = BTreeSet::from([
        "timeout",
        "max_output_bytes",
        "reserve_bytes",
        "max_size_ratio",
        "require_duration",
    ]);
    if ["thumbnail", "preview", "waveform"].contains(&name) {
        allowed.insert("start_seconds");
    }
    if ["preview", "waveform"].contains(&name) {
        allowed.insert("duration_seconds");
    }
    if AUDIO.contains(&name) || ["subtitle-srt", "waveform"].contains(&name) {
        allowed.insert("stream");
    }
    if name == "remux-mkv" {
        allowed.insert("stream_indices");
    }
    if name == "thumbnail" || VIDEO.contains(&name) {
        allowed.insert("video_stream");
    }
    if VIDEO.contains(&name) {
        allowed.extend(["audio_stream", "crf", "encoder_speed", "audio_bitrate_kbps"]);
    }
    if ["audio-aac", "audio-opus"].contains(&name) {
        allowed.insert("audio_bitrate_kbps");
    }
    if name == "waveform" {
        allowed.extend(["width", "height"]);
    }
    if name == "audio-normalize" {
        allowed.extend(["integrated_lufs", "true_peak_db", "loudness_range"]);
    }
    if name == "hdr-sdr-1080p" {
        allowed.insert("peak_nits");
    }
    if PHOTO.contains(&name) {
        allowed.extend([
            "width",
            "height",
            "quality",
            "max_input_pixels",
            "max_input_bytes",
        ]);
    }
    if options.keys().any(|k| !allowed.contains(k.as_str())) {
        return Err(Error("unsupported recipe option".into()));
    }
    let mut value = json!({"version":1,"preset":name,"timeout":3600,"max_output_bytes":20u64*1024*1024*1024,"reserve_bytes":64*1024*1024});
    for (key, default) in [
        ("start_seconds", 0),
        ("duration_seconds", 10),
        ("stream", 0),
    ] {
        if allowed.contains(key) {
            value[key] = json!(default);
        }
    }
    if name == "waveform" {
        value["width"] = json!(1280);
        value["height"] = json!(240);
        value["duration_seconds"] = json!(60);
    }
    if name == "audio-normalize" {
        value["integrated_lufs"] = json!(-16);
        value["true_peak_db"] = json!(-2);
        value["loudness_range"] = json!(7);
    }
    if name == "hdr-sdr-1080p" {
        value["peak_nits"] = json!(1000);
    }
    if PHOTO.contains(&name) {
        for (k, v) in [
            ("width", 1280),
            ("height", 1280),
            ("quality", 85),
            ("max_input_pixels", 20_000_000),
            ("max_input_bytes", 64 * 1024 * 1024),
            ("max_output_bytes", 16 * 1024 * 1024),
            ("timeout", 60),
        ] {
            value[k] = json!(v);
        }
    }
    for (key, v) in options {
        value[key] = v.clone();
    }
    if value
        .get("require_duration")
        .is_some_and(|v| !v.is_boolean())
    {
        return Err(Error("require_duration must be boolean".into()));
    }
    if value
        .get("max_size_ratio")
        .is_some_and(|v| !v.as_f64().is_some_and(|n| n > 0. && n <= 100.))
    {
        return Err(Error(
            "max_size_ratio must be a finite positive ratio up to 100".into(),
        ));
    }
    for (key, lo, hi) in [
        ("timeout", 1, 86400),
        ("max_output_bytes", 1024, 1024i64.pow(4)),
        ("reserve_bytes", 0, 1024i64.pow(4)),
        ("start_seconds", 0, 86400),
        ("duration_seconds", 1, 60),
        ("stream", 0, 255),
        ("video_stream", 0, 255),
        ("crf", 0, if name == "av1-720p" { 63 } else { 51 }),
        ("audio_bitrate_kbps", 8, 512),
        ("width", if PHOTO.contains(&name) { 1 } else { 64 }, 4096),
        (
            "height",
            if PHOTO.contains(&name) { 1 } else { 32 },
            if PHOTO.contains(&name) { 4096 } else { 2048 },
        ),
        ("quality", 1, 100),
        ("max_input_pixels", 1, 40_000_000),
        ("max_input_bytes", 1024, 256 * 1024 * 1024),
        ("integrated_lufs", -70, -5),
        ("true_peak_db", -9, 0),
        ("loudness_range", 1, 50),
        ("peak_nits", 100, 10000),
    ] {
        if value
            .get(key)
            .is_some_and(|v| !v.as_i64().is_some_and(|n| (lo..=hi).contains(&n)))
        {
            return Err(Error(format!("invalid recipe {key}")));
        }
    }
    if PHOTO.contains(&name)
        && (value["timeout"].as_i64().unwrap() > 300
            || value["max_output_bytes"].as_i64().unwrap() > 64 * 1024 * 1024)
    {
        return Err(Error(
            "image recipes allow at most 300 seconds and 64 MiB output".into(),
        ));
    }
    if value
        .get("audio_stream")
        .is_some_and(|v| !v.is_null() && !v.as_i64().is_some_and(|n| (0..=255).contains(&n)))
    {
        return Err(Error(
            "audio_stream must be null or an integer between 0 and 255".into(),
        ));
    }
    if value
        .get("encoder_speed")
        .is_some_and(|v| !matches!(v.as_str(), Some("veryfast" | "medium" | "slow")))
    {
        return Err(Error(
            "encoder_speed must be veryfast, medium, or slow".into(),
        ));
    }
    if let Some(indices) = value.get("stream_indices") {
        let valid = indices.as_array().is_some_and(|a| {
            !a.is_empty()
                && a.len() <= 256
                && a.iter().all(|v| v.as_u64().is_some_and(|n| n <= 255))
                && a.iter().map(Value::as_u64).collect::<BTreeSet<_>>().len() == a.len()
        });
        if !valid {
            return Err(Error(
                "stream_indices must be a nonempty list of unique stream indices between 0 and 255"
                    .into(),
            ));
        }
    }
    Ok(value)
}
pub fn tools(required: bool) -> Result<Value> {
    let value =
        json!({"ffmpeg":media::tool_signature("decode"),"ffprobe":media::tool_signature("probe")});
    if required
        && ["ffmpeg", "ffprobe"]
            .iter()
            .any(|k| value[k]["available"] != true)
    {
        return Err(Error(
            "rendering requires optional ffmpeg and ffprobe executables".into(),
        ));
    }
    Ok(value)
}
pub fn capabilities() -> Result<Value> {
    let identity = tools(false)?;
    let available = ["ffmpeg", "ffprobe"]
        .iter()
        .all(|k| identity[k]["available"] == true);
    let mut supported = std::collections::BTreeMap::new();
    if available {
        for category in ["encoders", "muxers", "filters"] {
            let output = command_output(
                &[
                    identity["ffmpeg"]["tool"].as_str().unwrap(),
                    "-hide_banner",
                    &format!("-{category}"),
                ],
                Default::default(),
            )
            .map_err(|e| Error(e.to_string()))?;
            let mut names = BTreeSet::new();
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let parts = line.split_whitespace().collect::<Vec<_>>();
                if parts.len() >= 2
                    && !parts[0].is_empty()
                    && parts[0].len() <= 8
                    && parts[0]
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b == b'.')
                {
                    names.extend(parts[1].split(',').map(str::to_owned));
                }
            }
            supported.insert(category, names);
        }
    }
    let mut presets = vec![];
    for p in PRESETS {
        let mut encoders = BTreeSet::new();
        if let Some(codec) = p.codec {
            encoders.insert(codec);
        }
        let mut filters = BTreeSet::new();
        if p.name == "thumbnail" || PHOTO.contains(&p.name) || VIDEO.contains(&p.name) {
            filters.insert("scale");
        }
        if VIDEO.contains(&p.name) {
            encoders.insert(if p.name == "av1-720p" {
                "libopus"
            } else {
                "aac"
            });
        }
        if p.name == "hdr-sdr-1080p" {
            filters.extend(["zscale", "format", "tonemap", "sidedata"]);
        }
        if p.name == "waveform" {
            filters.extend(["showwavespic", "aformat", "atrim", "asetpts"]);
        }
        if p.name == "audio-normalize" {
            filters.insert("loudnorm");
        }
        let mut missing = vec![];
        for (category, names) in [
            ("muxers", BTreeSet::from([p.muxer])),
            ("encoders", encoders),
            ("filters", filters),
        ] {
            for name in names {
                if !supported.get(category).is_some_and(|s| s.contains(name)) {
                    missing.push(format!("{category}:{name}"));
                }
            }
        }
        presets.push(json!({"name":p.name,"extension":p.extension,"role":p.role,"available":available&&missing.is_empty(),"missing":missing}));
    }
    Ok(json!({"tools":identity,"presets":presets}))
}
pub fn digest(file: &mut File) -> Result<String> {
    digest_with_poll(file, &mut || Ok(()))
}
pub(crate) fn digest_with_poll(
    file: &mut File,
    poll: &mut dyn FnMut() -> Result<()>,
) -> Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let before = file_revision(file)?;
    let mut remaining = file.metadata()?.len();
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    let mut data = [0; 1024 * 1024];
    while remaining > 0 {
        poll()?;
        let wanted = remaining.min(data.len() as u64) as usize;
        let n = file.read(&mut data[..wanted])?;
        if n == 0 {
            return Err(Error("file changed while hashing".into()));
        }
        digest.update(&data[..n]);
        remaining -= n as u64;
    }
    if file_revision(file)? != before {
        return Err(Error("file changed while hashing".into()));
    }
    Ok(format!("{:x}", digest.finalize()))
}
pub fn probe(file: &mut File, identity: &Value, timeout: u64, static_image: bool) -> Result<Value> {
    probe_with_poll(file, identity, timeout, static_image, None, &mut || Ok(()))
}
pub(crate) fn probe_with_poll(
    file: &mut File,
    identity: &Value,
    timeout: u64,
    static_image: bool,
    cancel: Option<&AtomicBool>,
    poll: &mut dyn FnMut() -> Result<()>,
) -> Result<Value> {
    file.seek(SeekFrom::Start(0))?;
    let fd = file.as_raw_fd();
    let mut args = vec![
        identity["ffprobe"]["tool"]
            .as_str()
            .ok_or_else(|| Error("missing ffprobe identity".into()))?
            .to_owned(),
        "-v".into(),
        "error".into(),
        "-protocol_whitelist".into(),
        "file,pipe".into(),
        "-format_whitelist".into(),
        format!("{FORMATS},srt"),
        "-max_alloc".into(),
        "33554432".into(),
        "-probesize".into(),
        "1048576".into(),
        "-analyzeduration".into(),
        "1000000".into(),
    ];
    if static_image {
        args.extend(["-count_packets", "-read_intervals", "%+#2"].map(str::to_owned));
    }
    args.extend(
        [
            "-show_format",
            "-show_streams",
            "-show_chapters",
            "-of",
            "json",
        ]
        .map(str::to_owned),
    );
    args.push(format!("/dev/fd/{fd}"));
    let mut callback = || poll().map_err(|e| CommandFailure::new("failed", e.0));
    let raw = command_output(
        &args,
        CommandOptions {
            pass_fds: &[fd],
            timeout: Duration::from_secs(timeout),
            cancel,
            on_poll: Some(&mut callback),
            ..Default::default()
        },
    )
    .map_err(|e| Error(e.to_string()))?;
    let mut data: Value = catabolic_core::decode_json_bytes(&raw.stdout)?;
    if !data["streams"].as_array().is_some_and(|a| a.len() <= 256) {
        return Err(Error("invalid or excessive output streams".into()));
    }
    // Share bounded metadata validation, removing analysis-only fields which the render contract does not emit.
    data = catabolic_core::media::normalize_probe(data, &[], 1_048_576, 1_000_000, "")
        .map_err(|e| Error(e.0))?;
    data.as_object_mut().unwrap().remove("analysis");
    data.as_object_mut().unwrap().remove("media_health");
    Ok(data)
}
pub(crate) fn streams<'a>(data: &'a Value, kind: &str) -> Vec<&'a Value> {
    data["streams"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|s| {
                    s["codec_type"] == kind
                        && !matches!(s["disposition"]["attached_pic"].as_i64(),Some(n) if n!=0)
                        && s["disposition"]["attached_pic"] != true
                })
                .collect()
        })
        .unwrap_or_default()
}
fn duration(data: &Value) -> f64 {
    data["format"]["duration"]
        .as_f64()
        .or_else(|| {
            data["format"]["duration"]
                .as_str()
                .and_then(|s| s.parse().ok())
        })
        .filter(|n| n.is_finite() && *n > 0.)
        .unwrap_or(0.)
}
fn integer(value: &Value, key: &str, default: i64) -> i64 {
    value[key].as_i64().unwrap_or(default)
}
fn static_frame(v: &Value) -> bool {
    v.is_null() || v == "1" || v == 1
}
pub fn input_error(recipe: &Value, data: &Value) -> Result<Option<String>> {
    let name = recipe["preset"]
        .as_str()
        .ok_or_else(|| Error("missing rendering preset".into()))?;
    let p = preset(name)?;
    let all = data["streams"]
        .as_array()
        .ok_or_else(|| Error("invalid probe streams".into()))?;
    let candidates = if let Some(kind) = p.kind {
        streams(data, kind)
    } else {
        all.iter().collect()
    };
    let index = integer(
        recipe,
        if p.kind == Some("video") {
            "video_stream"
        } else {
            "stream"
        },
        0,
    ) as usize;
    let bad = |s: &str| Ok(Some(s.into()));
    if candidates.len() <= index {
        return bad("input lacks the requested stream");
    }
    if PHOTO.contains(&name) {
        let v = candidates[0];
        if all.len() != 1
            || !matches!(
                data["format"]["format_name"].as_str(),
                Some("png_pipe" | "jpeg_pipe" | "webp_pipe")
            )
            || !matches!(v["codec_name"].as_str(), Some("png" | "mjpeg" | "webp"))
            || !static_frame(&v["nb_frames"])
            || !static_frame(&v["nb_read_packets"])
        {
            return bad("image recipes require a single static PNG, JPEG or WebP image");
        }
        let (w, h) = (
            v["width"].as_i64().unwrap_or(0),
            v["height"].as_i64().unwrap_or(0),
        );
        if w <= 0
            || h <= 0
            || w.checked_mul(h)
                .is_none_or(|n| n > integer(recipe, "max_input_pixels", 0))
        {
            return bad("image dimensions are unknown or exceed the input pixel budget");
        }
        if matches!(
            v["color_transfer"].as_str(),
            Some("smpte2084" | "arib-std-b67")
        ) {
            return bad("image recipes support SDR input only");
        }
    }
    if VIDEO.contains(&name) {
        let v = candidates[index];
        let hdr = matches!(
            v["color_transfer"].as_str(),
            Some("smpte2084" | "arib-std-b67")
        );
        if name == "hdr-sdr-1080p" {
            if !hdr
                || v["color_primaries"] != "bt2020"
                || v["color_space"] != "bt2020nc"
                || !matches!(v["color_range"].as_str(), Some("tv" | "pc"))
            {
                return bad("tone mapping requires tagged BT.2020 PQ/HLG input with a known range");
            }
        } else if hdr {
            return bad("HDR conversion requires a tone-mapping recipe; this preset supports SDR");
        }
        if recipe["audio_stream"]
            .as_u64()
            .is_some_and(|i| i as usize >= streams(data, "audio").len())
        {
            return bad("input lacks the requested audio stream");
        }
    }
    Ok(None)
}
fn add(args: &mut Vec<String>, values: &[&str]) {
    args.extend(values.iter().map(|v| (*v).to_owned()));
}
pub struct Rendered {
    pub validation: Value,
    pub output_revision: Value,
    pub sha256: String,
    pub size: u64,
}
pub fn file_revision(file: &File) -> Result<Value> {
    use std::os::unix::fs::MetadataExt;
    let stat = file.metadata()?;
    Ok(
        json!({"size":stat.size(),"mtime_ns":i128::from(stat.mtime())*1_000_000_000+i128::from(stat.mtime_nsec()),"ctime_ns":i128::from(stat.ctime())*1_000_000_000+i128::from(stat.ctime_nsec()),"device":stat.dev(),"inode":stat.ino()}),
    )
}
/// Transform into an already exclusively created output descriptor. A process
/// exit is followed by independent re-probing, bounds, stream and codec checks.
pub fn render(
    input: &mut File,
    output: &mut File,
    recipe: &Value,
    identity: &Value,
    cancel: &AtomicBool,
    poll: &mut dyn FnMut() -> Result<()>,
) -> Result<Rendered> {
    let name = recipe["preset"]
        .as_str()
        .ok_or_else(|| Error("missing rendering preset".into()))?;
    let spec = preset(name)?;
    if name == "component-mux" {
        return Err(Error(
            "component-mux requires an explicitly selected package".into(),
        ));
    }
    // Validate the full stored recipe again; no arbitrary arguments or filters can enter argv.
    let mut settings = recipe.clone();
    let object = settings
        .as_object_mut()
        .ok_or_else(|| Error("invalid recipe".into()))?;
    for key in ["version", "preset", "output_definition_id"] {
        object.remove(key);
    }
    let normalized = definition(name, &settings)?;
    for (key, value) in normalized.as_object().unwrap() {
        if recipe.get(key) != Some(value) {
            return Err(Error("invalid or incomplete immutable recipe".into()));
        }
    }
    let input_stat = input.metadata()?;
    let output_stat = output.metadata()?;
    use std::os::unix::fs::MetadataExt;
    if !input_stat.is_file()
        || !output_stat.is_file()
        || (input_stat.dev(), input_stat.ino()) == (output_stat.dev(), output_stat.ino())
        || output_stat.len() != 0
    {
        return Err(Error(
            "render requires separate regular input and empty exclusive output".into(),
        ));
    }
    if PHOTO.contains(&name) {
        if input_stat.len() > integer(recipe, "max_input_bytes", 0) as u64 {
            return Err(Error("image input exceeds its byte budget".into()));
        }
        let mut head = [0; 30];
        let n = input.read_at(&mut head, 0)?;
        if n > 20 && &head[..4] == b"RIFF" && &head[8..16] == b"WEBPVP8X" && head[20] & 2 != 0 {
            return Err(Error(
                "animated WebP is not supported by image recipes".into(),
            ));
        }
    }
    let before = probe_with_poll(
        input,
        identity,
        20,
        PHOTO.contains(&name),
        Some(cancel),
        poll,
    )?;
    if let Some(error) = input_error(recipe, &before)? {
        return Err(Error(error));
    }
    if PHOTO.contains(&name)
        && !matches!(&before["streams"][0]["nb_read_packets"],v if v=="1"||v==1)
    {
        return Err(Error(
            "image input lacks evidence of a single static frame".into(),
        ));
    }
    let all = before["streams"].as_array().unwrap();
    let mut selected = if let Some(kind) = spec.kind {
        streams(&before, kind)
    } else {
        all.iter().collect()
    };
    let stream = integer(recipe, "stream", 0) as usize;
    let video = integer(recipe, "video_stream", 0) as usize;
    if selected.is_empty() || stream >= selected.len() {
        return Err(Error("input lacks the requested stream".into()));
    }
    if spec.kind == Some("video") && video >= selected.len() {
        return Err(Error("input lacks the requested video stream".into()));
    }
    if name == "remux-mkv"
        && let Some(indices) = recipe["stream_indices"].as_array()
    {
        selected = indices
            .iter()
            .map(|i| {
                all.iter()
                    .find(|s| s["index"] == *i)
                    .ok_or_else(|| Error("input lacks a requested remux stream index".into()))
            })
            .collect::<Result<Vec<_>>>()?;
    }
    let idx = |s: &Value| -> Result<String> {
        s["index"]
            .as_u64()
            .filter(|i| *i <= i32::MAX as u64)
            .map(|i| format!("0:{i}"))
            .ok_or_else(|| Error("malformed stream index".into()))
    };
    input.seek(SeekFrom::Start(0))?;
    let input_fd = input.as_raw_fd();
    let output_fd = output.as_raw_fd();
    let mut command = vec![
        identity["ffmpeg"]["tool"]
            .as_str()
            .ok_or_else(|| Error("missing ffmpeg identity".into()))?
            .to_owned(),
    ];
    add(
        &mut command,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-protocol_whitelist",
            "file,pipe",
            "-format_whitelist",
            FORMATS,
            "-max_alloc",
            "33554432",
            "-threads",
            "1",
            "-i",
            &format!("/dev/fd/{input_fd}"),
            "-map_metadata",
            "0",
            "-map_chapters",
            "0",
        ],
    );
    if name == "remux-mkv" {
        for s in &selected {
            add(&mut command, &["-map", &idx(s)?]);
        }
        add(&mut command, &["-c", "copy"]);
    } else if PHOTO.contains(&name) {
        add(
            &mut command,
            &[
                "-map",
                &idx(selected[0])?,
                "-frames:v",
                "1",
                "-an",
                "-map_metadata",
                "-1",
                "-map_chapters",
                "-1",
                "-vf",
                &format!(
                    "scale=w='min(iw,{})':h='min(ih,{})':force_original_aspect_ratio=decrease,setsar=1",
                    recipe["width"], recipe["height"]
                ),
                "-c:v",
                spec.codec.unwrap(),
                "-update",
                "1",
            ],
        );
        if name == "image-jpeg" {
            let quality = 2
                + (((100 - integer(recipe, "quality", 85)) as f64 * 29. / 99.).round_ties_even()
                    as i64);
            add(
                &mut command,
                &["-q:v", &quality.to_string(), "-pix_fmt", "yuvj444p"],
            );
        } else {
            add(
                &mut command,
                &[
                    "-quality",
                    &recipe["quality"].to_string(),
                    "-compression_level",
                    "4",
                ],
            );
        }
    } else if name == "thumbnail" {
        add(
            &mut command,
            &[
                "-ss",
                &recipe["start_seconds"].to_string(),
                "-map",
                &idx(streams(&before, "video")[video])?,
                "-frames:v",
                "1",
                "-vf",
                "scale=w='min(640,iw)':h=-2",
                "-c:v",
                "png",
                "-update",
                "1",
            ],
        );
    } else if name == "waveform" {
        add(
            &mut command,
            &[
                "-filter_complex",
                &format!(
                    "[{}]atrim=start={}:duration={},asetpts=PTS-STARTPTS,aformat=sample_rates=48000:channel_layouts=mono,showwavespic=s={}x{}:colors=white[wave]",
                    idx(selected[stream])?,
                    recipe["start_seconds"],
                    recipe["duration_seconds"],
                    recipe["width"],
                    recipe["height"]
                ),
                "-map",
                "[wave]",
                "-frames:v",
                "1",
                "-c:v",
                "png",
                "-update",
                "1",
            ],
        );
    } else if AUDIO.contains(&name) || name == "subtitle-srt" {
        add(
            &mut command,
            &["-map", &idx(selected[stream])?, "-c", spec.codec.unwrap()],
        );
        if name == "audio-aac" && recipe.get("audio_bitrate_kbps").is_some() {
            add(
                &mut command,
                &["-b:a", &format!("{}k", recipe["audio_bitrate_kbps"])],
            );
        }
        if name == "audio-opus" {
            add(
                &mut command,
                &[
                    "-b:a",
                    &format!("{}k", integer(recipe, "audio_bitrate_kbps", 96)),
                    "-ar",
                    "48000",
                    "-ac",
                    "2",
                ],
            );
        }
        if name == "audio-normalize" {
            add(
                &mut command,
                &[
                    "-af",
                    &format!(
                        "loudnorm=I={}:TP={}:LRA={}:linear=false",
                        recipe["integrated_lufs"], recipe["true_peak_db"], recipe["loudness_range"]
                    ),
                    "-ar",
                    "48000",
                    "-sample_fmt",
                    "s16",
                ],
            );
            for key in [
                "REPLAYGAIN_TRACK_GAIN",
                "REPLAYGAIN_TRACK_PEAK",
                "REPLAYGAIN_ALBUM_GAIN",
                "REPLAYGAIN_ALBUM_PEAK",
                "R128_TRACK_GAIN",
                "R128_ALBUM_GAIN",
            ] {
                add(
                    &mut command,
                    &[
                        "-metadata",
                        &format!("{key}="),
                        "-metadata:s:a",
                        &format!("{key}="),
                    ],
                );
            }
        }
    } else {
        let height = if ["h264-1080p", "hdr-sdr-1080p"].contains(&name) {
            1080
        } else {
            720
        };
        let mut filters = format!("scale=w=-2:h='trunc(min({height},ih)/2)*2'");
        if name == "hdr-sdr-1080p" {
            filters = format!(
                "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=hable:desat=2:peak={},zscale=t=bt709:m=bt709:r=tv,format=yuv420p,sidedata=mode=delete,{filters}",
                integer(recipe, "peak_nits", 1000) as f64 / 100.
            );
        }
        let speed = recipe["encoder_speed"].as_str().unwrap_or("medium");
        let speed = if name == "av1-720p" {
            match speed {
                "veryfast" => "12",
                "slow" => "4",
                _ => "8",
            }
        } else {
            speed
        };
        add(
            &mut command,
            &[
                "-map",
                &idx(streams(&before, "video")[video])?,
                "-vf",
                &filters,
                "-c:v",
                spec.codec.unwrap(),
                "-preset",
                speed,
                "-crf",
                &integer(recipe, "crf", if name == "av1-720p" { 32 } else { 23 }).to_string(),
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                if name == "av1-720p" { "libopus" } else { "aac" },
                "-b:a",
                &format!(
                    "{}k",
                    integer(
                        recipe,
                        "audio_bitrate_kbps",
                        if name == "av1-720p" { 96 } else { 160 }
                    )
                ),
            ],
        );
        if name == "av1-720p" {
            add(
                &mut command,
                &["-svtav1-params", "lp=1", "-ar", "48000", "-ac", "2"],
            );
        }
        if name == "hdr-sdr-1080p" {
            add(
                &mut command,
                &[
                    "-color_primaries",
                    "bt709",
                    "-color_trc",
                    "bt709",
                    "-colorspace",
                    "bt709",
                    "-color_range",
                    "tv",
                ],
            );
        }
        let audio = streams(&before, "audio");
        if recipe.get("audio_stream").is_none_or(|v| !v.is_null()) && !audio.is_empty() {
            add(
                &mut command,
                &[
                    "-map",
                    &idx(audio[integer(recipe, "audio_stream", 0) as usize])?,
                ],
            );
        }
        if name == "preview" {
            add(
                &mut command,
                &[
                    "-ss",
                    &recipe["start_seconds"].to_string(),
                    "-t",
                    &recipe["duration_seconds"].to_string(),
                ],
            );
        }
    }
    add(
        &mut command,
        &[
            "-threads",
            "1",
            "-filter_threads",
            "1",
            "-filter_complex_threads",
            "1",
            "-fs",
            &recipe["max_output_bytes"].to_string(),
            "-f",
            spec.muxer,
            &format!("/dev/fd/{output_fd}"),
        ],
    );
    let mut callback = || poll().map_err(|e| CommandFailure::new("failed", e.0));
    command_output(
        &command,
        CommandOptions {
            pass_fds: &[input_fd, output_fd],
            timeout: Duration::from_secs(integer(recipe, "timeout", 3600) as u64),
            cancel: Some(cancel),
            on_poll: Some(&mut callback),
            ..Default::default()
        },
    )
    .map_err(|e| Error(e.to_string()))?;
    poll()?;
    output.sync_all()?;
    let output_revision = file_revision(output)?;
    let after = probe_with_poll(output, identity, 20, false, Some(cancel), poll)?;
    let size = output.metadata()?.len();
    let budget = integer(recipe, "max_output_bytes", 0) as u64;
    if size == 0 || size >= budget {
        return Err(Error(
            "empty output or output reached its byte budget".into(),
        ));
    }
    if recipe["max_size_ratio"]
        .as_f64()
        .is_some_and(|ratio| size as f64 > input_stat.len() as f64 * ratio)
    {
        return Err(Error(
            "output exceeds the recipe's maximum input/output size ratio".into(),
        ));
    }
    let expected = if name == "waveform" {
        Some("video")
    } else {
        spec.kind
    };
    if expected.is_some_and(|k| streams(&after, k).is_empty()) {
        return Err(Error("output is missing its expected stream".into()));
    }
    let output_streams = after["streams"].as_array().unwrap();
    if name == "remux-mkv" {
        let signature = |s: &Value| (s["codec_type"].clone(), s["codec_name"].clone());
        if selected.iter().map(|s| signature(s)).collect::<Vec<_>>()
            != output_streams.iter().map(signature).collect::<Vec<_>>()
        {
            return Err(Error(
                "remux did not preserve the selected streams and codecs".into(),
            ));
        }
    }
    if VIDEO.contains(&name) {
        if streams(&after, "video")[0]["codec_name"]
            != if name == "av1-720p" { "av1" } else { "h264" }
        {
            return Err(Error("output codec does not match the recipe".into()));
        }
        let expected_audio = recipe.get("audio_stream").is_none_or(|v| !v.is_null())
            && !streams(&before, "audio").is_empty();
        let audio = streams(&after, "audio");
        if audio.len() != usize::from(expected_audio)
            || audio
                .iter()
                .any(|s| s["codec_name"] != if name == "av1-720p" { "opus" } else { "aac" })
        {
            return Err(Error("output audio does not match the recipe".into()));
        }
    }
    if name == "hdr-sdr-1080p" {
        let v = streams(&after, "video")[0];
        if ["color_transfer", "color_primaries", "color_space"]
            .iter()
            .any(|k| v[k] != "bt709")
            || v["color_range"] != "tv"
        {
            return Err(Error(
                "output lacks the required BT.709 SDR color evidence".into(),
            ));
        }
    }
    if AUDIO.contains(&name) || name == "subtitle-srt" {
        let wanted = match name {
            "audio-aac" => "aac",
            "audio-opus" => "opus",
            "subtitle-srt" => "subrip",
            _ => "flac",
        };
        if output_streams.len() != 1 || output_streams[0]["codec_name"] != wanted {
            return Err(Error(
                "extracted stream codec does not match the recipe".into(),
            ));
        }
    }
    if ["thumbnail", "waveform"].contains(&name)
        && streams(&after, "video")[0]["codec_name"] != "png"
    {
        return Err(Error("output is not a PNG image".into()));
    }
    if PHOTO.contains(&name) {
        let image = streams(&after, "video")[0];
        let (w, h) = (integer(image, "width", 0), integer(image, "height", 0));
        if output_streams.len() != 1
            || image["codec_name"]
                != if name == "image-jpeg" {
                    "mjpeg"
                } else {
                    "webp"
                }
            || w <= 0
            || h <= 0
            || w > integer(recipe, "width", 0)
            || h > integer(recipe, "height", 0)
            || i128::from(w) * i128::from(h)
                > i128::from(integer(selected[0], "width", 0))
                    * i128::from(integer(selected[0], "height", 0))
        {
            return Err(Error(
                "image output codec or dimensions do not match the recipe".into(),
            ));
        }
    }
    if name == "waveform"
        && ["width", "height"]
            .iter()
            .any(|k| streams(&after, "video")[0][k] != recipe[k])
    {
        return Err(Error("waveform dimensions do not match the recipe".into()));
    }
    let mut wanted = duration(&before);
    if recipe["require_duration"] == true && (wanted == 0. || duration(&after) == 0.) {
        return Err(Error("required duration evidence is unknown".into()));
    }
    if name == "preview" {
        wanted = (integer(recipe, "duration_seconds", 10) as f64)
            .min((wanted - integer(recipe, "start_seconds", 0) as f64).max(0.));
    }
    if ![
        "thumbnail",
        "waveform",
        "image-jpeg",
        "image-webp",
        "subtitle-srt",
    ]
    .contains(&name)
        && wanted > 0.
        && (duration(&after) - wanted).abs() > 1f64.max(wanted * 0.02)
    {
        return Err(Error(
            "output duration does not match the requested content".into(),
        ));
    }
    let mut validation = json!({"version":1,"input":before,"output":after,"coverage":"bounded stream probe and operation-specific checks; not full decode","recipe_digest":sha256(encode(recipe))});
    if name == "waveform" {
        validation["waveform_window"] = json!({"start_seconds":recipe["start_seconds"],"duration_seconds":recipe["duration_seconds"]});
    }
    if name == "audio-normalize" {
        validation["normalization"] = json!({"mode":"single-pass dynamic","integrated_lufs":recipe["integrated_lufs"],"true_peak_db":recipe["true_peak_db"],"loudness_range":recipe["loudness_range"],"coverage":"configured targets; achieved loudness is not independently measured"});
    }
    if name == "hdr-sdr-1080p" {
        validation["tone_mapping"] = json!({"algorithm":"hable","peak_nits":recipe["peak_nits"],"coverage":"explicit peak assumption and tagged input; not display certification"});
    }
    let sha256 = digest_with_poll(output, poll)?;
    if file_revision(output)? != output_revision {
        return Err(Error("output changed during validation and hashing".into()));
    }
    Ok(Rendered {
        validation,
        output_revision,
        sha256,
        size,
    })
}
