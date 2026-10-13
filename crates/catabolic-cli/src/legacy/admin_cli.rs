// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Legacy envelopes and bounded input only; all mutations belong to Store.
use super::{read_text, text};
use catabolic_store::{Result, embedding::Session};
use serde_json::Value;
use std::path::Path;
pub fn execute(args: &Value) -> Result<Value> {
    execute_detailed(args).map_err(Into::into)
}
pub fn execute_detailed(args: &Value) -> catabolic_store::access_admin::DetailedResult<Value> {
    let operation = text(args, "operation");
    let mut session = Session::open(Path::new(text(args, "db")), operation != "tokens", false)?;
    let mut request = args.clone();
    if operation == "grant-put" {
        request["definition"] =
            catabolic_core::decode_json(&read_text(text(args, "file"), 262144)?)
                .map_err(catabolic_store::Error::from)?;
    }
    catabolic_store::admin_cli::execute_detailed(
        &mut session,
        text(args, "profile"),
        operation,
        &request,
    )
}
