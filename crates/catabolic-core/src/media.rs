// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Pure, bounded interpretation of media evidence. Process execution and catalog
//! writes remain in the runtime and store respectively.
use crate::{Error, Result};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const PROBE_COVERAGE: &str = "bounded stream analysis; not a full decode";
pub const HEADER_COVERAGE: &str = "zero-filled container header screen; not a probe or decode";

pub fn failure_health(operation: &str, state: &str, diagnostic: &str) -> Value {
    let message = diagnostic.to_lowercase();
    let mut status = if state == "unsupported" {
        "unsupported"
    } else {
        "unknown"
    };
    let mut reason = state;
    let unsupported = [
        "not on whitelist",
        "not on the whitelist",
        "protocol not found",
        "decoder not found",
        "unknown decoder",
        "unsupported codec",
        "decoding requested, but no decoder found",
        "not implemented",
    ];
    let decoder_not_found = message.lines().any(|line| {
        line.find("decoder")
            .is_some_and(|i| line[i..].contains(" not found"))
    });
    if unsupported.iter().any(|s| message.contains(s)) || decoder_not_found {
        status = "unsupported";
        reason = "unsupported_format_or_codec";
    } else if [
        "permission denied",
        "input/output error",
        "cannot allocate memory",
        "no such file",
        "resource temporarily unavailable",
    ]
    .iter()
    .any(|s| message.contains(s))
    {
        reason = "io_error";
    } else if state == "failed" {
        for (token, flag) in [
            ("ebml header parsing failed", "container_parse_error"),
            ("moov atom not found", "missing_container_index"),
            ("invalid nal unit size", "corrupt_packet"),
            ("packet corrupt", "corrupt_packet"),
            ("corrupt input packet", "corrupt_packet"),
            ("invalid pcm packet", "corrupt_packet"),
            ("error while decoding stream", "decode_corruption"),
            ("corrupt decoded frame", "decode_corruption"),
        ] {
            if message.contains(token) {
                status = "invalid";
                reason = flag;
                break;
            }
        }
    }
    json!({"status": status, "reason": reason, "warnings": [], "coverage": if operation == "probe" {"bounded stream analysis"} else {"incomplete decode"}})
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64() != Some(0.0),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}

pub fn probe_health(data: &Value, required: &[String]) -> Value {
    let empty = vec![];
    let streams = data["streams"].as_array().unwrap_or(&empty);
    let present = streams
        .iter()
        .filter(|s| !truthy(&s["disposition"]["attached_pic"]))
        .filter_map(|s| s["codec_type"].as_str())
        .collect::<BTreeSet<_>>();
    let missing = required
        .iter()
        .filter(|s| !present.contains(s.as_str()))
        .cloned()
        .collect::<BTreeSet<_>>();
    let duration = data["format"]["duration"].as_f64().or_else(|| {
        data["format"]["duration"]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
    });
    let mut warnings = vec![];
    if !duration.is_some_and(|n| n.is_finite() && n > 0.0) {
        warnings.push("duration_missing_or_nonpositive");
    }
    if streams.iter().any(|s| {
        matches!(s["codec_type"].as_str(), Some("video" | "audio")) && !truthy(&s["codec_name"])
    }) {
        warnings.push("codec_unidentified");
    }
    json!({"status": if missing.is_empty() {"not_detected"} else {"content_mismatch"}, "reason": if missing.is_empty() {Value::Null} else {json!("missing_required_streams")}, "missing_streams": missing, "warnings": warnings, "coverage": PROBE_COVERAGE})
}

/// Sanitize a successful ffprobe result without promoting its bounded evidence
/// to a full-decode certification. Filename is removed before persistence.
pub fn normalize_probe(
    mut data: Value,
    required: &[String],
    bytes: u64,
    microseconds: u64,
    diagnostic: &str,
) -> Result<Value> {
    if !data.is_object() || data.get("format").is_some_and(|v| !v.is_object()) {
        return Err(Error("malformed probe result".into()));
    }
    let empty = vec![];
    let streams = match data.get("streams") {
        None => &empty,
        Some(v) => v
            .as_array()
            .ok_or_else(|| Error("malformed probe streams".into()))?,
    };
    if streams.iter().any(|s| {
        !s.is_object()
            || ["disposition", "tags"]
                .iter()
                .any(|k| s.get(k).is_some_and(|v| !v.is_object()))
    }) {
        return Err(Error("malformed probe streams".into()));
    }
    if streams.len() > 256 {
        return Err(Error("probe exceeds 256 streams".into()));
    }
    let videos = streams
        .iter()
        .filter(|s| s["codec_type"] == "video" && !truthy(&s["disposition"]["attached_pic"]))
        .collect::<Vec<_>>();
    let dimensions = |key: &str| -> Result<Value> {
        if videos.is_empty() {
            return Ok(Value::Null);
        }
        let mut largest = 0i64;
        for s in &videos {
            let v = s.get(key).unwrap_or(&Value::Null);
            if v.is_null() && s.get(key).is_some() {
                return Err(Error("malformed probe dimensions".into()));
            }
            let n = if s.get(key).is_none() {
                0
            } else {
                v.as_i64()
                    .ok_or_else(|| Error("malformed probe dimensions".into()))?
            };
            largest = largest.max(n);
        }
        Ok(json!(largest))
    };
    let strings = |key: &str, kind: Option<&str>| -> Result<Value> {
        let mut values = BTreeSet::new();
        for stream in streams {
            if kind.is_some_and(|k| {
                stream["codec_type"] != k
                    || (k == "video" && truthy(&stream["disposition"]["attached_pic"]))
            }) {
                continue;
            }
            let value = if key == "language" {
                &stream["tags"][key]
            } else {
                &stream[key]
            };
            if truthy(value) {
                values.insert(
                    value
                        .as_str()
                        .ok_or_else(|| Error("malformed probe metadata".into()))?
                        .to_owned(),
                );
            }
        }
        Ok(json!(values))
    };
    let summary = json!({"width": dimensions("width")?, "height": dimensions("height")?, "video_codecs": strings("codec_name", Some("video"))?, "audio_codecs": strings("codec_name", Some("audio"))?, "languages": strings("language", None)?, "duration": data["format"]["duration"], "format": data["format"]["format_name"], "hdr": if !videos.is_empty() && videos.iter().all(|s| truthy(&s["color_transfer"])) {json!(videos.iter().any(|s| matches!(s["color_transfer"].as_str(), Some("smpte2084" | "arib-std-b67"))))} else {Value::Null}});
    if let Some(format) = data.get_mut("format").and_then(Value::as_object_mut) {
        format.remove("filename");
    }
    data["summary"] = summary;
    data["analysis"] =
        json!({"bytes": bytes, "microseconds": microseconds, "coverage": PROBE_COVERAGE});
    data["media_health"] = probe_health(&data, required);
    if !diagnostic.is_empty() {
        let health = failure_health("probe", "failed", diagnostic);
        if matches!(health["status"].as_str(), Some("invalid" | "unsupported"))
            || health["reason"] == "io_error"
        {
            data["media_health"] = health;
        } else {
            data["media_health"]["warnings"]
                .as_array_mut()
                .unwrap()
                .push(json!("probe_diagnostics"));
        }
        data["diagnostic"] = json!(diagnostic.chars().take(1000).collect::<String>());
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn health_preserves_environmental_precedence() {
        assert_eq!(
            failure_health("probe", "failed", "Permission denied; moov atom not found")["reason"],
            "io_error"
        );
        assert_eq!(
            failure_health("decode", "timeout", "packet corrupt")["status"],
            "unknown"
        );
        assert_eq!(
            failure_health("probe", "failed", "decoder 'foo' not found; packet corrupt")["status"],
            "unsupported"
        );
    }
    #[test]
    fn cover_art_does_not_satisfy_required_video() {
        let data = normalize_probe(json!({"streams":[{"index":0,"codec_type":"video","codec_name":"mjpeg","disposition":{"attached_pic":1}}],"format":{"filename":"private/path","duration":"NaN"}}), &["video".into()], 1, 2, "").unwrap();
        assert_eq!(data["media_health"]["status"], "content_mismatch");
        assert!(data["format"].get("filename").is_none());
        assert!(data["summary"]["width"].is_null());
    }
    #[test]
    fn stdout_success_does_not_override_corruption_evidence() {
        let data = normalize_probe(
            json!({"streams":[],"format":{"duration":"1"}}),
            &[],
            1,
            2,
            "moov atom not found",
        )
        .unwrap();
        assert_eq!(data["media_health"]["status"], "invalid");
        assert!(normalize_probe(json!({"streams":[{"tags":null}]}), &[], 1, 2, "").is_err());
        assert!(normalize_probe(json!({"streams":vec![json!({});257]}), &[], 1, 2, "").is_err());
    }
}
