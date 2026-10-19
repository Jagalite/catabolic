// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Pure immutable fallback-policy normalization. Defaults and omitted optional
//! fields match the frozen Python Policy model; no selection or I/O occurs here.
use crate::{Error, Result};
use serde_json::{Value, json};
use std::collections::BTreeSet;
const FIELDS: &[&str] = &[
    "size",
    "location",
    "path",
    "height",
    "width",
    "hdr",
    "duration",
    "language",
    "video_codec",
    "audio_codec",
];
fn invalid(message: &str) -> Error {
    Error(format!("invalid fallback policy: {message}"))
}
fn object(value: &Value, allowed: &[&str]) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("expected an object"))?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid("extra fields are not permitted"));
    }
    Ok(())
}
fn merge(defaults: Value, value: &Value) -> Value {
    let mut out = defaults;
    for (key, value) in value.as_object().unwrap() {
        out[key] = value.clone();
    }
    out
}
fn text(value: &Value, min: usize, max: usize) -> Result<()> {
    if !value
        .as_str()
        .is_some_and(|s| (min..=max).contains(&s.chars().count()))
    {
        return Err(invalid("invalid string length or type"));
    }
    Ok(())
}
fn array(value: &Value, min: usize, max: usize) -> Result<&Vec<Value>> {
    value
        .as_array()
        .filter(|a| (min..=max).contains(&a.len()))
        .ok_or_else(|| invalid("invalid list length or type"))
}
fn integer(value: &Value, min: i64, max: i64) -> Result<()> {
    if !value.as_i64().is_some_and(|n| (min..=max).contains(&n)) {
        return Err(invalid("invalid integer budget"));
    }
    Ok(())
}
fn choice(value: &Value, allowed: &[&str]) -> Result<()> {
    if !value.as_str().is_some_and(|s| allowed.contains(&s)) {
        return Err(invalid("invalid literal value"));
    }
    Ok(())
}
fn tiers(value: &Value) -> Result<()> {
    for tier in array(value, 1, 32)? {
        object(tier, &["name", "query_id"])?;
        text(&tier["name"], 1, 128)?;
        text(&tier["query_id"], 1, 256)?;
    }
    Ok(())
}
fn exclude_null(value: &mut Value, keys: &[&str]) {
    let object = value.as_object_mut().unwrap();
    for key in keys {
        if object.get(*key).is_some_and(Value::is_null) {
            object.remove(*key);
        }
    }
}
pub fn definition(input: &Value) -> Result<Value> {
    crate::validate_finite(input)?;
    object(
        input,
        &[
            "version",
            "slot_roles",
            "fallbacks",
            "within_tier",
            "lineage_requirement",
            "failback",
            "on_unavailable",
            "budgets",
            "package",
        ],
    )?;
    let mut out = merge(
        json!({"version":1,"slot_roles":["primary"],"within_tier":{},"lineage_requirement":"current_source_revision","failback":{},"on_unavailable":"retain","budgets":{}}),
        input,
    );
    // Pydantic's frozen Literal[1] accepts True and 1.0 and normalizes both to 1.
    if out["version"] != true && out["version"].as_f64() != Some(1.) {
        return Err(invalid("version must be 1"));
    }
    out["version"] = json!(1);
    for role in array(&out["slot_roles"], 1, 8)? {
        text(role, 0, usize::MAX)?;
    }
    tiers(&out["fallbacks"])?;
    let tier_names = out["fallbacks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    if tier_names.len() != out["fallbacks"].as_array().unwrap().len() {
        return Err(Error("fallback tier names must be unique".into()));
    }
    choice(
        &out["lineage_requirement"],
        &["accepted_source_revision", "current_source_revision"],
    )?;
    choice(&out["on_unavailable"], &["retain"])?;
    object(&out["within_tier"], &["require", "prefer", "tie_break"])?;
    let mut rank = merge(json!({"require":{},"prefer":[]}), &out["within_tier"]);
    let requirements = rank["require"]
        .as_object()
        .ok_or_else(|| invalid("require must be an object"))?;
    if requirements.keys().any(|k| !FIELDS.contains(&k.as_str())) {
        return Err(Error("unsupported required field".into()));
    }
    for rule in array(&rank["prefer"], 0, 20)? {
        object(rule, &["field", "order", "values"])?;
        text(&rule["field"], 0, usize::MAX)?;
        if let Some(v) = rule.get("order").filter(|v| !v.is_null()) {
            choice(v, &["asc", "desc"])?;
        }
        if let Some(v) = rule.get("values").filter(|v| !v.is_null()) {
            array(v, 0, usize::MAX)?;
        }
    }
    for rule in rank["prefer"].as_array_mut().unwrap() {
        exclude_null(rule, &["order", "values"]);
        if !FIELDS.contains(&rule["field"].as_str().unwrap())
            || rule.get("order").is_some() == rule.get("values").is_some()
        {
            return Err(Error(
                "preference requires a supported field and one of order or values".into(),
            ));
        }
    }
    if let Some(tie) = rank.get("tie_break").filter(|v| !v.is_null()) {
        choice(tie, &["file_id"])?;
    }
    exclude_null(&mut rank, &["tie_break"]);
    out["within_tier"] = rank;
    object(
        &out["failback"],
        &[
            "mode",
            "minimum_healthy_seconds",
            "minimum_successful_checks",
        ],
    )?;
    let failback = merge(
        json!({"mode":"stable","minimum_healthy_seconds":300,"minimum_successful_checks":2}),
        &out["failback"],
    );
    choice(&failback["mode"], &["immediate", "stable", "manual"])?;
    integer(&failback["minimum_healthy_seconds"], 0, 86400)?;
    integer(&failback["minimum_successful_checks"], 1, 1000)?;
    out["failback"] = failback;
    object(
        &out["budgets"],
        &[
            "max_entries",
            "max_candidates",
            "max_checks",
            "query_timeout_ms",
            "probe_timeout_ms",
        ],
    )?;
    let budgets = merge(
        json!({"max_entries":1000,"max_candidates":10000,"max_checks":128,"query_timeout_ms":5000,"probe_timeout_ms":1000}),
        &out["budgets"],
    );
    for (key, lo, hi) in [
        ("max_entries", 1, 10000),
        ("max_candidates", 1, 100000),
        ("max_checks", 1, 1000),
        ("query_timeout_ms", 1, 30000),
        ("probe_timeout_ms", 10, 10000),
    ] {
        integer(&budgets[key], lo, hi)?;
    }
    out["budgets"] = budgets;
    if let Some(package) = out.get("package").filter(|v| !v.is_null()).cloned() {
        object(
            &package,
            &[
                "requirements",
                "publication",
                "operation_id",
                "supported_codecs",
            ],
        )?;
        let mut package = merge(json!({"publication":"container"}), &package);
        choice(
            &package["publication"],
            &["container", "sidecars", "selected_only"],
        )?;
        for requirement in array(&package["requirements"], 1, 16)? {
            object(requirement, &["name", "fallbacks", "required"])?;
            text(&requirement["name"], 1, 128)?;
            tiers(&requirement["fallbacks"])?;
            if requirement.get("required").is_some_and(|v| !v.is_boolean()) {
                return Err(invalid("required must be boolean"));
            }
        }
        let mut names = BTreeSet::new();
        for requirement in package["requirements"].as_array_mut().unwrap() {
            if !names.insert(requirement["name"].as_str().unwrap().to_owned())
                || requirement["name"] == "video"
            {
                return Err(Error(
                    "component requirement names must be unique and not video".into(),
                ));
            }
            if requirement.get("required").is_none() {
                requirement["required"] = json!(true);
            }
        }
        if out["slot_roles"] != json!(["primary"]) {
            return Err(Error("package policies use primary slots; components are selected by their requirement queries".into()));
        }
        if let Some(id) = package.get("operation_id").filter(|v| !v.is_null()) {
            text(id, 0, usize::MAX)?;
        }
        if let Some(codecs) = package.get("supported_codecs").filter(|v| !v.is_null()) {
            for codec in array(codecs, 0, 64)? {
                text(codec, 0, usize::MAX)?;
            }
        }
        exclude_null(&mut package, &["operation_id", "supported_codecs"]);
        out["package"] = package;
    } else {
        out.as_object_mut().unwrap().remove("package");
    }
    if crate::encode(&out).len() > 65536 {
        return Err(Error("fallback policy exceeds 64 KiB".into()));
    }
    Ok(out)
}
pub fn query_dependencies(value: &Value) -> BTreeSet<String> {
    let mut dependencies = BTreeSet::new();
    for tier in value["fallbacks"].as_array().into_iter().flatten() {
        if let Some(id) = tier["query_id"].as_str() {
            dependencies.insert(id.to_owned());
        }
    }
    for requirement in value["package"]["requirements"]
        .as_array()
        .into_iter()
        .flatten()
    {
        for tier in requirement["fallbacks"].as_array().into_iter().flatten() {
            if let Some(id) = tier["query_id"].as_str() {
                dependencies.insert(id.to_owned());
            }
        }
    }
    dependencies
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_optional_nulls_and_budgets() {
        let d=definition(&json!({"version":true,"fallbacks":[{"name":"original","query_id":"q"}],"within_tier":{"tie_break":null}})).unwrap();
        assert_eq!(d["version"], 1);
        assert!(d["within_tier"].get("tie_break").is_none());
        assert_eq!(d["budgets"]["max_checks"], 128);
        assert!(
            definition(
                &json!({"fallbacks":[{"name":"a","query_id":"q"}],"budgets":{"max_checks":true}})
            )
            .is_err()
        );
    }
    #[test]
    fn package_dependencies_and_duplicates() {
        let d=definition(&json!({"fallbacks":[{"name":"video","query_id":"v"}],"package":{"requirements":[{"name":"audio","fallbacks":[{"name":"best","query_id":"a"}]}],"operation_id":null}})).unwrap();
        assert_eq!(
            query_dependencies(&d),
            BTreeSet::from(["a".into(), "v".into()])
        );
        assert_eq!(d["package"]["requirements"][0]["required"], true);
        assert!(d["package"].get("operation_id").is_none());
    }
}
