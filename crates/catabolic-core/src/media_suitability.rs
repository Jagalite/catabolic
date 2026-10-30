// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Media suitability decisions from supplied preset and probe evidence.
use crate::{Error, Result};
use serde_json::Value;
fn streams<'a>(data: &'a Value, kind: &str) -> Vec<&'a Value> {
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
fn integer(value: &Value, key: &str, default: i64) -> i64 {
    value[key].as_i64().unwrap_or(default)
}
fn static_frame(v: &Value) -> bool {
    v.is_null() || v == "1" || v == 1
}
pub fn input_error(
    recipe: &Value,
    data: &Value,
    kind: Option<&str>,
    photo: bool,
    video: bool,
) -> Result<Option<String>> {
    let name = recipe["preset"]
        .as_str()
        .ok_or_else(|| Error("missing rendering preset".into()))?;
    let all = data["streams"]
        .as_array()
        .ok_or_else(|| Error("invalid probe streams".into()))?;
    let candidates = if let Some(kind) = kind {
        streams(data, kind)
    } else {
        all.iter().collect()
    };
    let index = integer(
        recipe,
        if kind == Some("video") {
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
    if photo {
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
    if video {
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
