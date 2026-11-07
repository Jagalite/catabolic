// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Semantic projection of evaluated plans, independent of catalog I/O.
use serde_json::{Value, json};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidKind,
    Incomplete,
}
pub fn semantic(result: &Value) -> Result<Value, Error> {
    let kind = result["kind"].as_str().ok_or(Error::InvalidKind)?;
    let original = &result["value"];
    let mut value = original.clone();
    if ["fallback", "projection"].contains(&kind) {
        let key = if kind == "fallback" {
            "decisions"
        } else {
            "desired"
        };
        let mut selected = vec![];
        for decision in original[key].as_array().map(Vec::as_slice).unwrap_or(&[]) {
            let mut normalized = json!({});
            for field in [
                "entry_id",
                "file_id",
                "revision",
                "state",
                "policy_id",
                "selected_tier",
                "rendition_id",
            ] {
                normalized[field] = decision[field].clone();
            }
            if kind == "projection" {
                normalized["path"] = decision["path"].clone();
            }
            let mut evidence = json!({});
            if let Some(object) = decision["evidence"].as_object() {
                for (k, v) in object {
                    if [
                        "semantic_relationship",
                        "byte_identity",
                        "source_currentness",
                        "lineage_requirement",
                        "accepted_source_revision",
                        "output_digest",
                        "layout_digest",
                        "component_package",
                        "component_package_signature",
                        "occurrence_id",
                    ]
                    .contains(&k.as_str())
                    {
                        evidence[k] = v.clone();
                    }
                }
            }
            normalized["evidence"] = evidence;
            selected.push(normalized);
        }
        value = if kind == "projection" {
            json!({"desired":selected,"removed":original["removed"],"safe":original["safe"]})
        } else {
            json!(selected)
        };
    } else if kind == "rows" {
        value = json!({"columns":original["columns"],"rows":original["rows"]});
    } else if kind == "document" {
        value = original["data"].clone();
    }
    Ok(json!({"kind":kind,"value":value}))
}
pub fn compare(previous: &Value, current: &Value) -> Result<Value, Error> {
    if current["complete"] != true {
        return Err(Error::Incomplete);
    }
    let baseline = semantic(current)?;
    Ok(json!({"changed":previous!=&baseline,"baseline":baseline}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timing_and_diagnostics_do_not_change_semantic_plan() {
        let a = json!({"kind":"fallback","complete":true,"value":{"decisions":[{"entry_id":"e","file_id":"f","revision":"r","evidence":{"byte_identity":"same","elapsed":1}}]}});
        let mut b = a.clone();
        b["value"]["decisions"][0]["evidence"]["elapsed"] = json!(99);
        let baseline = semantic(&a).unwrap();
        assert_eq!(compare(&baseline, &b).unwrap()["changed"], false);
        b["value"]["decisions"][0]["revision"] = json!("new");
        assert_eq!(compare(&baseline, &b).unwrap()["changed"], true);
        b["complete"] = json!(false);
        assert_eq!(compare(&baseline, &b), Err(Error::Incomplete));
    }
}
