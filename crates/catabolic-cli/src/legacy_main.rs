// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Explicit opt-in legacy CLI adapter. Python remains the installed default.
#[path = "legacy/dispatch.rs"]
mod dispatch;
#[path = "legacy/parser.rs"]
mod parser;
#[path = "legacy/presentation.rs"]
mod presentation;
fn main() {
    let args = match std::env::args_os()
        .skip(1)
        .map(|value| value.into_string())
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(args) => args,
        Err(_) => {
            eprintln!("error: native legacy CLI requires UTF-8 command-line arguments");
            std::process::exit(2);
        }
    };
    let output = match parser::parse(&args) {
        Ok(args) => match execute(&args) {
            Ok(value) => presentation::success(&args, value),
            Err(error) => presentation::error(&args, error),
        },
        Err(output) => output,
    };
    print!("{}", output.stdout);
    eprint!("{}", output.stderr);
    std::process::exit(output.code);
}

fn execute(args: &serde_json::Value) -> catabolic_store::Result<serde_json::Value> {
    if matches!(
        args["command"].as_str(),
        Some("query" | "graphql" | "inbox")
    ) {
        let args = args.clone();
        catabolic_http::interruptible(move |cancel| {
            let check = cancel.clone();
            let result = catabolic_store::embedding::with_interrupt_check(
                move || check.load(std::sync::atomic::Ordering::Acquire),
                || dispatch::dispatch(&args),
            );
            if cancel.load(std::sync::atomic::Ordering::Acquire) {
                Err(catabolic_store::Error(
                    "native legacy CLI interrupted".into(),
                ))
            } else {
                result
            }
        })
    } else {
        dispatch::dispatch(args)
    }
}
