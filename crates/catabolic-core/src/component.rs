// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Pure component compatibility and packaging decisions. Inputs are the frozen
//! normalized component/policy contract; no store, filesystem, or clock reads.
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// A cross-file component requires an explicit, matching full-timeline claim.
pub fn compatible(component: &Value, video: &Value, primary: &Value) -> Option<Value> {
    if component["item_id"] != primary["item_id"] || component["part"] != primary["part"] {
        return None;
    }
    let edition = primary["metadata"]
        .get("edition_id")
        .unwrap_or(&primary["item_id"]);
    if component["file_id"] == video["file_id"] && component["revision"] == video["revision"] {
        return Some(json!({"basis":"container_timestamps","edition_id":edition,
            "video_file_id":video["file_id"],"video_revision":video["revision"],"part":primary["part"],
            "timeline_id":format!("container:{}",video["revision"].as_str().unwrap_or("")),
            "coverage":"container","offset_seconds":0.0,"synchronization":"container_timestamps"}));
    }
    let claim = &component["compatibility"];
    let fallback = json!(format!(
        "container:{}",
        video["revision"].as_str().unwrap_or("")
    ));
    let timeline = primary["metadata"].get("timeline_id").unwrap_or(&fallback);
    if !claim.is_object()
        || &claim["edition_id"] != edition
        || claim["video_file_id"] != video["file_id"]
        || claim["video_revision"] != video["revision"]
        || claim["part"] != primary["part"]
        || &claim["timeline_id"] != timeline
        || claim["coverage"] != "full"
        || !matches!(
            claim["synchronization"].as_str(),
            Some("declared" | "verified")
        )
        || !claim["offset_seconds"].is_number()
    {
        return None;
    }
    let mut relation = claim.clone();
    relation["basis"] = json!("accepted_compatibility");
    Some(relation)
}

/// Decide whether selected tracks can be exposed as-is, need an approved
/// transform, or must be refused for incomplete dependency evidence.
pub fn decision(config: &Value, selected: &[Value], dependencies: &[Value]) -> Value {
    let occurrences = selected
        .iter()
        .map(|v| &v["occurrence"])
        .collect::<Vec<_>>();
    let Some(video) = occurrences.first() else {
        return json!({"action":"block","ready":false,"reason":"missing_video_component","exposes_unselected_components":false});
    };
    let external = occurrences
        .iter()
        .copied()
        .filter(|v| v["file_id"] != video["file_id"])
        .collect::<Vec<_>>();
    let offsets = selected
        .iter()
        .any(|v| v["compatibility"]["offset_seconds"].as_f64() != Some(0.0));
    if let Some(codecs) = config["supported_codecs"].as_array()
        && occurrences.iter().any(|v| !codecs.contains(&v["codec"]))
    {
        return json!({"action":"convert","ready":false,"operation_id":config["operation_id"],"reason":"approved_conversion_required","exposes_unselected_components":false});
    }
    let indexes = occurrences
        .iter()
        .map(|v| crate::encode(&v["locator"]["index"]))
        .collect::<BTreeSet<_>>();
    let exact = external.is_empty()
        && dependencies.is_empty()
        && !offsets
        && video["stream_count"].as_u64() == Some(indexes.len() as u64);
    if config["publication"] == "selected_only" && exact {
        return json!({"action":"use_container","ready":true,"exposes_unselected_components":false});
    }
    let embedded_dependencies = dependencies.iter().any(|d| {
        occurrences.iter().any(|v| {
            v["file_id"] == video["file_id"] && d["for_occurrence_id"] == v["occurrence_id"]
        })
    });
    let action = if config["publication"] == "selected_only" || offsets || embedded_dependencies {
        "mux"
    } else if !external.is_empty() {
        if config["publication"] == "container" {
            "mux"
        } else {
            "publish_sidecars"
        }
    } else {
        "use_container"
    };
    if action == "publish_sidecars" {
        if external.iter().any(|v| {
            v["locator"]["convention"] != "whole_file"
                && !(v["stream_count"] == 1
                    && matches!(
                        v["format_name"].as_str(),
                        Some("srt" | "webvtt" | "ass" | "flac" | "aac" | "mp3" | "wav" | "ogg")
                    ))
        }) {
            return json!({"action":"extract","ready":false,"operation_id":config["operation_id"],"reason":"external_component_needs_extracted_artifact","exposes_unselected_components":true});
        }
        if dependencies.iter().any(|d| d["purpose"] == "font") {
            return json!({"action":"block","ready":false,"reason":"font_sidecar_layout_unsupported","exposes_unselected_components":true});
        }
    }
    if action == "mux"
        && dependencies.iter().any(|d| {
            matches!(
                d["purpose"].as_str(),
                Some("subtitle_index" | "subtitle_data")
            )
        })
    {
        return json!({"action":"block","ready":false,"reason":"paired_subtitle_mux_unsupported","exposes_unselected_components":false});
    }
    for occurrence in occurrences {
        let path = occurrence["snapshot"]["path"].as_str().unwrap_or("");
        let filename = path.rsplit('/').next().unwrap_or("");
        let extension = filename
            .rsplit_once('.')
            .filter(|(base, tail)| !base.is_empty() && !tail.is_empty())
            .map(|(_, tail)| tail.to_lowercase());
        if (matches!(
            occurrence["codec"].as_str(),
            Some("ass" | "ssa" | "dvd_subtitle")
        ) || matches!(extension.as_deref(), Some("ass" | "ssa" | "idx" | "sub")))
            && action != "use_container"
            && !truthy(&occurrence["dependencies_complete"])
        {
            return json!({"action":"block","ready":false,"reason":"subtitle_dependencies_unverified","exposes_unselected_components":false});
        }
    }
    json!({"action":action,"ready":matches!(action,"use_container"|"publish_sidecars"),"operation_id":config["operation_id"],"exposes_unselected_components":matches!(action,"use_container"|"publish_sidecars")})
}
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_timeline_claim_and_exact_container() {
        let video = json!({"item_id":"item","part":null,"file_id":"video","revision":"rev","locator":{"index":0},"stream_count":1,"codec":"h264","snapshot":{"path":"video.mkv"}});
        let primary = json!({"item_id":"item","part":null,"metadata":{}});
        let relation = compatible(&video, &video, &primary).unwrap();
        assert_eq!(relation["basis"], "container_timestamps");
        let selected = vec![json!({"occurrence":video,"compatibility":relation})];
        assert_eq!(
            decision(&json!({"publication":"selected_only"}), &selected, &[]),
            json!({"action":"use_container","ready":true,"exposes_unselected_components":false})
        );
        let mut external = video.clone();
        external["file_id"] = json!("external");
        assert!(compatible(&external, &video, &primary).is_none());
        external["compatibility"] = json!({"edition_id":"item","video_file_id":"video","video_revision":"rev","part":null,"timeline_id":"container:rev","coverage":"full","offset_seconds":0.0,"synchronization":"declared"});
        assert!(compatible(&external, &video, &primary).is_some());
        external["compatibility"]["offset_seconds"] = json!(true);
        assert!(compatible(&external, &video, &primary).is_none());
    }
}
