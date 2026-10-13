// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Frozen CLI GraphQL admission and typed variable-JSON diagnostics.
use super::{
    dispatch::{read_text, text},
    failure::Result,
};
use catabolic_store::Error;
use serde_json::{Value, json};
use std::path::Path;

pub fn execute(a: &Value) -> Result<Value> {
    let choices = usize::from(!a["document"].is_null())
        + usize::from(!a["file"].is_null())
        + usize::from(a["schema"] == true);
    if choices != 1 {
        return Err(Error("choose exactly one query document, --file, or --schema".into()).into());
    }
    if a["schema"] == true {
        if !a["variables"].is_null() || !a["operation_name"].is_null() {
            return Err(Error(
                "--schema cannot be combined with query variables or operation name".into(),
            )
            .into());
        }
        return Ok(catabolic_store::graphql::description());
    }
    if text(a, "db").is_empty() {
        return Err(Error("select --db or set CATABOLIC_DB".into()).into());
    }
    let document = if !a["file"].is_null() {
        // Match the frozen CLI admission boundary before parsing variables
        // or invoking the shared GraphQL document owner.
        read_text(text(a, "file"), 131072)?
    } else {
        text(a, "document").into()
    };
    let timeout = a["timeout_ms"]
        .as_u64()
        .filter(|value| (1..=60000).contains(value))
        .ok_or_else(|| Error("timeout-ms must be between 1 and 60000".into()))?;
    if document.len() > 131072 {
        return Err(Error("query document must be at most 131072 bytes".into()).into());
    }
    if !a["variables"].is_null() {
        let raw = text(a, "variables");
        if raw.len() > 131072 {
            return Err(Error("variables exceed input budget".into()).into());
        }
        super::json_diagnostic::validate(raw)?;
    }
    let variables = if a["variables"].is_null()
        || catabolic_core::decode_json(text(a, "variables"))
            .map_err(Error::from)?
            .is_null()
    {
        // Frozen execute_graphql treats omitted and JSON-null variables
        // alike. Other transports retain their own validated adapters.
        json!({}).into()
    } else {
        catabolic_store::graphql::InputVariables::from_json(text(a, "variables"))?
    };
    catabolic_store::graphql::execute(
        Path::new(text(a, "db")),
        &document,
        text(a, "profile"),
        variables,
        a["operation_name"].as_str(),
        timeout,
    )
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure::Failure;

    #[test]
    fn supervisor_preserves_json_provenance_and_ordinary_failures() {
        let args = json!({"command":"graphql", "document":"{profile}",
            "variables":"{", "db":"unused-catalog", "timeout_ms":5000});
        let error = crate::execute(&args).unwrap_err();
        assert!(matches!(error, Failure::Json(_)));
        let message = error.to_string();
        let mut context = args.clone();
        context["machine"] = json!(true);
        let output = crate::presentation::legacy_error(&context, error);
        let report: Value = serde_json::from_str(&output.stdout).unwrap();
        assert_eq!(report["data"]["error"]["type"], "JSONDecodeError");
        // Identical native error text never forges syntax-error provenance.
        let output = crate::presentation::legacy_error(&context, Error(message).into());
        let report: Value = serde_json::from_str(&output.stdout).unwrap();
        assert_eq!(report["data"]["error"]["type"], "CatabolicError");
        let output = crate::presentation::legacy_error(
            &context,
            Error("native legacy CLI interrupted".into()).into(),
        );
        let report: Value = serde_json::from_str(&output.stdout).unwrap();
        assert_eq!(output.code, 130);
        assert_eq!(report["data"]["error"]["type"], "KeyboardInterrupt");
        context["db"] = json!("");
        assert!(matches!(
            crate::execute(&context).unwrap_err(),
            Failure::Detailed(_)
        ));
    }

    #[test]
    fn admission_precedes_variable_syntax() {
        let base = json!({"document":"{profile}", "variables":"{",
            "db":"unused-catalog", "timeout_ms":5000});
        for (key, value, expected) in [
            (
                "timeout_ms",
                json!(0),
                "timeout-ms must be between 1 and 60000",
            ),
            (
                "document",
                json!("x".repeat(131073)),
                "query document must be at most 131072 bytes",
            ),
            (
                "variables",
                json!(" ".repeat(131072) + "{"),
                "variables exceed input budget",
            ),
        ] {
            let mut args = base.clone();
            args[key] = value;
            let error = execute(&args).unwrap_err();
            assert!(matches!(error, Failure::Detailed(_)));
            assert_eq!(error.to_string(), expected);
        }
    }
}
