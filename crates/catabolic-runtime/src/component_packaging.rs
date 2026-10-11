// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Reviewed single-component extraction through the artifact journal owner.
use crate::{artifacts, rendering};
use catabolic_store::{Error, Result, Store, component_mutation as components};
use serde_json::{Value, json};
use std::{fs::File, sync::atomic::AtomicBool};

pub fn extraction(
    store: &mut Store,
    profile: &str,
    identifier: &str,
    operation: &str,
    location: &str,
    apply: bool,
    expected_plan: Option<&str>,
) -> Result<Value> {
    let plan = components::extraction_plan(store, profile, identifier, operation)?;
    if !apply {
        return Ok(plan);
    }
    if plan["plan_id"].as_str() != expected_plan {
        return Err(Error("stale_or_missing_component_extraction_plan".into()));
    }
    let package = &plan["package"];
    let row = &package["components"][0]["occurrence"];
    artifacts::enqueue_package(
        store,
        profile,
        row["file_id"].as_str().unwrap(),
        operation,
        location,
        row["item_id"].as_str().unwrap(),
        Some(package),
    )
}
pub fn render_selected(
    input: &mut File,
    output: &mut File,
    recipe: &Value,
    identity: &Value,
    cancel: &AtomicBool,
    poll: &mut dyn FnMut() -> Result<()>,
    package: &Value,
) -> Result<rendering::Rendered> {
    if package["packaging"]["action"] == "mux" {
        return crate::component_mux::render(output, recipe, identity, cancel, poll, package);
    }
    let row = &package["components"][0]["occurrence"];
    if row["locator"]["convention"] != "ffprobe_absolute_stream_index" {
        return Err(Error("extraction needs an exact stream locator".into()));
    }
    poll()?;
    let before = rendering::probe(
        input,
        identity,
        recipe["timeout"].as_u64().unwrap_or(20).min(60),
        false,
    )?;
    let index = &row["locator"]["index"];
    if recipe["preset"] == "remux-mkv" {
        if recipe["stream_indices"] != json!([index]) {
            return Err(Error(
                "approved remux operation must select exactly this component".into(),
            ));
        }
    } else {
        let streams = rendering::streams(&before, row["kind"].as_str().unwrap_or(""));
        let selected = recipe["stream"].as_u64().unwrap_or(0) as usize;
        if streams.get(selected).map(|v| &v["index"]) != Some(index) {
            return Err(Error(
                "approved extraction operation selects a different stream".into(),
            ));
        }
    }
    let mut result = rendering::render(input, output, recipe, identity, cancel, poll)?;
    let streams = result.validation["output"]["streams"]
        .as_array()
        .filter(|a| a.len() == 1)
        .ok_or_else(|| Error("component extraction exposed additional streams".into()))?;
    if streams[0]["codec_type"] != row["kind"] {
        return Err(Error(
            "component extraction produced the wrong stream kind".into(),
        ));
    }
    result.validation["components"] = json!([{"occurrence_id":row["occurrence_id"],"output_stream_index":streams[0]["index"],"relationship":if recipe["preset"]=="remux-mkv"{"extracted_copy"}else{"extracted_conversion"}}]);
    result.validation["component_signature"] = package["signature"].clone();
    Ok(result)
}
