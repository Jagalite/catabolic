// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Command-local presentation for completed, newly requested public scans.
//! The owner Value, persisted guarantees and native numeric contracts are never
//! changed. Resume, continuation and incomplete observations keep their existing
//! rendering until independently qualified.
use serde_json::Value;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) enum Context {
    #[default]
    Other,
    Scan,
    Maintenance,
    Stages,
    Watch,
    Cycle,
    Observations,
    Observation,
    Guarantees,
    MaxAge,
    Execution,
    Budgets,
    Reports,
    Report,
    Validation,
    Policy,
}

impl Context {
    pub(super) fn command(args: &Value) -> Self {
        // A configured maximum has no default-type provenance. Public CLI
        // scans do not expose this option; embedders must retain their type.
        if args.get("max_age").is_some() {
            return Self::Other;
        }
        match args["command"].as_str() {
            Some("scan") if args["request_id"].is_null() && args["continue_request"].is_null() => {
                Self::Scan
            }
            Some("maintenance") => Self::Maintenance,
            Some("watch") => Self::Watch,
            _ => Self::Other,
        }
    }

    pub(super) fn field(self, parent: &Value, key: &str) -> Self {
        match (self, key) {
            (Self::Maintenance, "stages") => Self::Stages,
            (Self::Watch, "last") => Self::Cycle,
            (Self::Cycle, "scan") => Self::Scan,
            (Self::Scan, "observations") if parent["complete"] == true => Self::Observations,
            (Self::Scan, "scans") if parent["complete"] == true => Self::Reports,
            (Self::Observation, "guarantees") => Self::Guarantees,
            (Self::Observation, "report") => Self::Report,
            (Self::Guarantees, "max_age") => Self::MaxAge,
            (Self::Guarantees, "execution") => Self::Execution,
            (Self::Execution, "budgets") => Self::Budgets,
            (Self::Report, "validation") => Self::Validation,
            (Self::Validation, "policy") => Self::Policy,
            _ => Self::Other,
        }
    }

    pub(super) fn element(self, value: &Value) -> Self {
        match self {
            Self::Stages if value["stage"] == "scan" => Self::Scan,
            Self::Observations
                if value["state"] == "complete"
                    && value["reuse"] == "created"
                    && value["guarantees"]["reuse_completed"] == false
                    && value["guarantees"]["method"] == "guarded-inventory-v2" =>
            {
                Self::Observation
            }
            Self::Reports if value["complete"] == true => Self::Report,
            _ => Self::Other,
        }
    }

    pub(super) fn keys(self, value: &Value) -> Option<Vec<&str>> {
        let preferred: &[&str] = match self {
            Self::Observation => &[
                "id",
                "profile",
                "source",
                "compatibility",
                "exclusions",
                "dirty_generation",
                "generation",
                "state",
                "requested_at",
                "started_at",
                "completed_at",
                "lease_until",
                "worker_pid",
                "scan_id",
                "report",
                "guarded",
                "execution",
                "parent_job",
                "claim_token",
                "reuse",
                "request_id",
                "guarantees",
                "coverage",
                "job_state",
                "retry_state",
                "request_state",
            ],
            Self::Guarantees => &[
                "minimum_start",
                "after",
                "max_age",
                "exclusions",
                "reuse_completed",
                "require_complete",
                "coverage",
                "method",
                "execution",
                "parent_job",
            ],
            Self::Execution => &[
                "budgets",
                "scopes",
                "policy_revision",
                "extensions",
                "include_regex",
                "exclude_regex",
            ],
            Self::Budgets => &[
                "directory_entries",
                "depth",
                "directory_seconds",
                "metadata_seconds",
                "errors",
                "total_seconds",
                "total_entries",
                "pending_directories",
                "batch_records",
                "batch_bytes",
                "batch_seconds",
                "temporary_bytes",
                "minimum_free_bytes",
            ],
            Self::Policy => &["preset", "revision", "settings"],
            _ => return None,
        };
        let object = value.as_object()?;
        Some(
            preferred
                .iter()
                .copied()
                .filter(|key| object.contains_key(*key))
                .chain(
                    object
                        .keys()
                        .map(String::as_str)
                        .filter(|key| !preferred.contains(key)),
                )
                .collect(),
        )
    }

    pub(super) fn number(self, value: &Value) -> Option<&'static str> {
        // Only a newly requested public scan's default, positive zero has an
        // integer spelling in the legacy wire contract. Never coerce arbitrary
        // floats, negative zero, counters, timestamps or stored resume requests.
        (self == Self::MaxAge
            && value
                .as_f64()
                .is_some_and(|n| n == 0.0 && !n.is_sign_negative()))
        .then_some("0")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation;
    use serde_json::json;

    fn fixtures() -> Value {
        serde_json::from_str(include_str!(
            "../../../../tests/parity/scan-report-presentation-v1.json"
        ))
        .unwrap()
    }

    fn scan_mut<'a>(command: &str, value: &'a mut Value) -> &'a mut Value {
        match command {
            "maintenance" => &mut value["stages"][1],
            "watch" => &mut value["last"]["scan"],
            _ => value,
        }
    }

    #[test]
    fn complete_frozen_envelopes_and_watch_progress_are_exact() {
        let fixture = fixtures();
        assert_eq!(fixture["schema"], 1);
        assert_eq!(fixture["cases"].as_array().unwrap().len(), 9);
        for case in fixture["cases"].as_array().unwrap() {
            let command = case["args"]["command"].as_str().unwrap();
            let mut value = case["value"].clone();
            let scan = scan_mut(command, &mut value);
            scan["observations"][0]["guarantees"]["max_age"] = json!(0.0);
            let before = value.clone();
            let output = presentation::success(&case["args"], value.clone());
            assert_eq!(
                output.code,
                case["expected"]["code"].as_i64().unwrap() as i32,
                "{}",
                case["name"]
            );
            assert_eq!(
                output.stdout,
                case["expected"]["stdout"].as_str().unwrap(),
                "{}",
                case["name"]
            );
            assert!(output.stderr.is_empty());
            // Progress is emitted by the transport, not by success().
            if command == "watch" {
                let body = presentation::scan_watch_cycle_text(&value["last"]);
                let progress = format!("{{\"cycle\": 1, {}\n", body.strip_prefix('{').unwrap());
                assert_eq!(
                    progress,
                    case["expected"]["stderr"].as_str().unwrap(),
                    "{}",
                    case["name"]
                );
            }
            assert_eq!(value, before);
            assert!(
                scan_mut(command, &mut value)["observations"][0]["guarantees"]["max_age"].is_f64()
            );
        }
    }

    #[test]
    fn numeric_provenance_is_local_and_never_changes_the_owner_value() {
        let fixture = fixtures();
        let case = &fixture["cases"][2];
        for number in [json!(0), json!(0.0), json!(-0.0), json!(4), json!(4.25)] {
            for mode in [
                "default",
                "configured",
                "resume",
                "continuation",
                "unrelated",
            ] {
                let mut args = case["args"].clone();
                match mode {
                    "configured" => args["max_age"] = number.clone(),
                    "resume" => args["request_id"] = json!("stored-request"),
                    "continuation" => args["continue_request"] = json!("stored-request"),
                    "unrelated" => args["command"] = json!("files"),
                    _ => (),
                }
                let mut value = case["value"].clone();
                value["observations"][0]["guarantees"]["max_age"] = number.clone();
                // Floats and integer counters elsewhere never acquire max_age's default spelling.
                value["counter_control"] = json!({"float":0.0,"negative_zero":-0.0,"int":0});
                let before = value.to_string();
                let output = presentation::success(&args, value.clone());
                let rendered: Value = serde_json::from_str(&output.stdout).unwrap();
                let actual = &rendered["data"]["observations"][0]["guarantees"]["max_age"];
                let expected = if mode == "default"
                    && number.as_f64().unwrap() == 0.0
                    && !number.as_f64().unwrap().is_sign_negative()
                {
                    json!(0)
                } else {
                    number.clone()
                };
                assert_eq!(actual.to_string(), expected.to_string(), "{mode} {number}");
                assert_eq!(
                    rendered["data"]["counter_control"].to_string(),
                    value["counter_control"].to_string()
                );
                assert_eq!(value.to_string(), before);
            }
        }
    }

    #[test]
    fn incomplete_resumed_and_unrelated_nested_shapes_keep_existing_rendering() {
        let fixture = fixtures();
        let case = &fixture["cases"][2];
        for state in ["pending", "unavailable", "error", "complete"] {
            let mut value = case["value"].clone();
            value["observations"][0]["state"] = json!(state);
            value["observations"][0]["reuse"] = json!("resumed");
            value["observations"][0]["guarantees"]["max_age"] = json!(0.0);
            let output = presentation::success(&case["args"], value.clone());
            let body: Value = serde_json::from_str(&output.stdout).unwrap();
            assert!(body["data"]["observations"][0]["guarantees"]["max_age"].is_f64());
        }
        let mut value = case["value"].clone();
        let provider = json!({"z":0.0,"a":-0.0,"observations":[{"state":"complete","reuse":"created","guarantees":{"max_age":0.0,"reuse_completed":false,"method":"guarded-inventory-v2"}}],"custom":{"maximum":4.25,"id":0}});
        value["observations"][0]["guarantees"]["provider"] = provider.clone();
        value["observations"][0]["unknown"] = provider.clone();
        value["observations"][0]["report"]["validation"]["policy"]["provider"] = provider.clone();
        let before = value.to_string();
        let output = presentation::success(&case["args"], value.clone());
        let body: Value = serde_json::from_str(&output.stdout).unwrap();
        for actual in [
            &body["data"]["observations"][0]["guarantees"]["provider"],
            &body["data"]["observations"][0]["unknown"],
            &body["data"]["observations"][0]["report"]["validation"]["policy"]["provider"],
        ] {
            assert_eq!(actual.to_string(), provider.to_string());
        }
        assert_eq!(value.to_string(), before);
        let provider_text = presentation::json_text(&provider, None, false, 0);
        assert!(
            output
                .stdout
                .contains(&format!("\"unknown\": {provider_text}"))
        );
        assert_eq!(
            output
                .stdout
                .matches(&format!("\"provider\": {provider_text}"))
                .count(),
            2
        );
        assert_eq!(Context::Other.keys(&provider), None);
        assert_eq!(Context::Other.number(&json!(0.0)), None);
        assert!(presentation::json_text(&provider, None, false, 0).contains("\"max_age\": 0.0"));
    }
}
