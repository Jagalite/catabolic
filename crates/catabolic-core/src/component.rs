// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Pure component compatibility and packaging decisions. Inputs are the frozen
//! normalized component/policy contract; no store, filesystem, or clock reads.
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Exact coordinate evidence for the native binding contract. It is additive:
/// legacy component claims and `compatible` retain their original semantics.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingCoordinateMapping {
    pub contract_version: u32,
    pub source: crate::catalog_identity::IntervalUs,
    pub timeline: crate::catalog_identity::IntervalUs,
}

/// Existing effect owners cannot execute the versioned integer-range contract.
pub fn requires_native_binding_effect(relation: &Value) -> bool {
    relation["requires_interval_enforcement"] == true
        || [
            "binding_id",
            "source_interval",
            "timeline_interval",
            "coordinate_offset_us",
        ]
        .iter()
        .any(|field| relation.get(*field).is_some())
}

/// Validate one component against a complete, revision-pinned native version.
/// This is a metadata compatibility decision, never permission to publish an
/// unsliced container. Effect owners must enforce the returned exact intervals.
pub fn compatible_binding(
    version: &crate::catalog_identity::VersionContract,
    binding: &crate::catalog_identity::VersionBinding,
    component: &Value,
    video: &Value,
    primary: &Value,
) -> Option<Value> {
    use crate::catalog_identity::{VersionState, validate_version};
    validate_version(version).ok()?;
    if version.state != VersionState::Ready
        || !version.bindings.contains(binding)
        || primary["id"] != binding.association_id
        || primary["item_id"] != version.association_item_id
        || primary["file_id"] != binding.file.file_id
        || primary["part"].as_u64() != Some(u64::from(binding.part))
        || video["file_id"] != binding.file.file_id
        || video["revision"] != binding.file.revision
        || video["item_id"] != version.association_item_id
        || video["part"].as_u64() != Some(u64::from(binding.part))
        || (component["file_id"] == video["file_id"] && component["revision"] != video["revision"])
        || component["revision"].as_str().is_none_or(str::is_empty)
    {
        return None;
    }
    let source = binding.source.interval().ok()??;
    let timeline = binding.timeline_interval?;
    let mut native_primary = primary.clone();
    // Typed ownership overrides arbitrary legacy association metadata only on
    // this explicitly versioned path. The legacy function itself is unchanged.
    if !native_primary["metadata"].is_object() {
        native_primary["metadata"] = json!({});
    }
    native_primary["metadata"]["edition_id"] = json!(version.edition_item_id);
    native_primary["metadata"]["timeline_id"] = json!(version.timeline_id);
    let mut relation = compatible(component, video, &native_primary)?;
    let internal =
        component["file_id"] == video["file_id"] && component["revision"] == video["revision"];
    let component_source = if internal {
        source
    } else {
        let mapping: BindingCoordinateMapping =
            serde_json::from_value(component["compatibility"]["coordinate_mapping"].clone())
                .ok()?;
        if mapping.contract_version != 1
            || mapping.timeline != timeline
            || mapping.source.duration_us() != timeline.duration_us()
        {
            return None;
        }
        mapping.source
    };
    relation["basis"] = json!(if internal {
        "native_internal_binding_v1"
    } else {
        "native_external_binding_v1"
    });
    relation["contract_version"] = json!(1);
    relation["binding_id"] = json!(binding.binding_id);
    relation["version_id"] = json!(version.version_id);
    relation["edition_id"] = json!(version.edition_item_id);
    relation["timeline_id"] = json!(version.timeline_id);
    relation["source_interval"] = json!(component_source);
    relation["timeline_interval"] = json!(timeline);
    // The native interval mapping is authoritative. Do not retain a separate
    // floating-point legacy offset that could contradict it downstream.
    relation.as_object_mut()?.remove("offset_seconds");
    relation["coordinate_offset_us"] = json!(timeline.start_us() - component_source.start_us());
    relation["coverage"] = json!("selected_binding");
    relation["requires_interval_enforcement"] = json!(true);
    Some(relation)
}

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
    if selected
        .iter()
        .any(|value| requires_native_binding_effect(&value["compatibility"]))
    {
        // Legacy mux/publication owners do not consume integer binding ranges.
        // A metadata-compatible slice must never become a whole-file effect.
        return json!({"action":"block","ready":false,"reason":"native_binding_effect_unsupported","exposes_unselected_components":false});
    }
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

#[cfg(test)]
mod native_binding_tests {
    use super::*;
    use crate::catalog_identity::{
        ExactFileRevision, IntervalUs, SourceExtent, VersionBinding, VersionContract, VersionState,
    };

    fn fixture() -> (VersionContract, Value, Value) {
        let binding = VersionBinding {
            binding_id: "binding".into(),
            association_id: "association".into(),
            file: ExactFileRevision {
                file_id: "file".into(),
                revision: "revision".into(),
            },
            part: 1,
            source: SourceExtent::Slice {
                interval: IntervalUs::new(10, 20).unwrap(),
            },
            timeline_interval: Some(IntervalUs::new(0, 10).unwrap()),
        };
        let version = VersionContract {
            contract_version: 1,
            version_id: "version".into(),
            association_item_id: "version-item".into(),
            edition_item_id: "edition".into(),
            timeline_id: "timeline".into(),
            state: VersionState::Ready,
            declared_coverage: Some(IntervalUs::new(0, 10).unwrap()),
            bindings: vec![binding],
        };
        let video =
            json!({"item_id":"version-item","part":1,"file_id":"file","revision":"revision"});
        let primary = json!({"id":"association","item_id":"version-item","file_id":"file","part":1,"metadata":{"edition_id":"untrusted","timeline_id":"untrusted"}});
        (version, video, primary)
    }

    #[test]
    fn native_internal_component_inherits_checked_slice_and_stable_timeline() {
        let (version, video, primary) = fixture();
        let result =
            compatible_binding(&version, &version.bindings[0], &video, &video, &primary).unwrap();
        assert_eq!(result["timeline_id"], "timeline");
        assert_eq!(result["edition_id"], "edition");
        assert_eq!(
            result["source_interval"],
            json!({"start_us":10,"end_us":20})
        );
        assert_eq!(
            result["timeline_interval"],
            json!({"start_us":0,"end_us":10})
        );
        assert_eq!(result["requires_interval_enforcement"], true);
        let effect = decision(
            &json!({"publication":"selected_only"}),
            &[json!({"occurrence":video,"compatibility":result})],
            &[],
        );
        assert_eq!(effect["action"], "block");
        assert_eq!(effect["reason"], "native_binding_effect_unsupported");
        // The original adapter keeps its recorded container-derived behavior.
        assert_eq!(
            compatible(&video, &video, &primary).unwrap()["timeline_id"],
            "container:revision"
        );
    }

    #[test]
    fn native_component_refuses_stale_revision_wrong_owner_and_unlisted_binding() {
        let (version, video, primary) = fixture();
        for (key, value) in [
            ("revision", json!("replacement")),
            ("item_id", json!("work")),
            ("part", json!(2)),
        ] {
            let mut changed = video.clone();
            changed[key] = value;
            assert!(
                compatible_binding(&version, &version.bindings[0], &changed, &changed, &primary)
                    .is_none()
            );
        }
        let mut binding = version.bindings[0].clone();
        binding.binding_id = "not-owned".into();
        assert!(compatible_binding(&version, &binding, &video, &video, &primary).is_none());
        let mut stale_track = video.clone();
        stale_track["revision"] = json!("old-container");
        assert!(
            compatible_binding(
                &version,
                &version.bindings[0],
                &stale_track,
                &video,
                &primary
            )
            .is_none()
        );
    }

    #[test]
    fn native_external_component_requires_complete_exact_coordinate_mapping() {
        let (version, video, primary) = fixture();
        let mut external = json!({"item_id":"version-item","part":1,"file_id":"subtitle","revision":"subtitle-revision","compatibility":{
            "edition_id":"edition","video_file_id":"file","video_revision":"revision","part":1,
            "timeline_id":"timeline","coverage":"full","offset_seconds":0.0,"synchronization":"declared"
        }});
        assert!(
            compatible_binding(&version, &version.bindings[0], &external, &video, &primary)
                .is_none()
        );
        external["compatibility"]["coordinate_mapping"] = json!({"contract_version":1,"source":{"start_us":20,"end_us":30},"timeline":{"start_us":0,"end_us":10}});
        let good = external.clone();
        assert!(
            compatible_binding(&version, &version.bindings[0], &external, &video, &primary)
                .is_some()
        );
        for (key, value) in [
            ("coverage", json!("partial")),
            ("edition_id", json!("other-cut")),
            ("video_revision", json!("replacement")),
            ("timeline_id", json!("other-timeline")),
        ] {
            let mut bad = good.clone();
            bad["compatibility"][key] = value;
            assert!(
                compatible_binding(&version, &version.bindings[0], &bad, &video, &primary)
                    .is_none()
            );
        }
        for mapping in [
            json!({"contract_version":1,"source":{"start_us":20,"end_us":25},"timeline":{"start_us":0,"end_us":10}}),
            json!({"contract_version":1,"source":{"start_us":20,"end_us":30},"timeline":{"start_us":1,"end_us":11}}),
            json!({"contract_version":2,"source":{"start_us":20,"end_us":30},"timeline":{"start_us":0,"end_us":10}}),
        ] {
            let mut bad = good.clone();
            bad["compatibility"]["coordinate_mapping"] = mapping;
            assert!(
                compatible_binding(&version, &version.bindings[0], &bad, &video, &primary)
                    .is_none()
            );
        }
    }
}
