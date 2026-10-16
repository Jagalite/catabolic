// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Explicit opt-in legacy CLI adapter. Python remains the installed default.
#[path = "legacy/dispatch.rs"]
mod dispatch;
#[path = "legacy/failure.rs"]
mod failure;
#[path = "legacy/graphql_cli.rs"]
mod graphql_cli;
#[path = "legacy/json_diagnostic.rs"]
mod json_diagnostic;
#[path = "legacy/mapping_events.rs"]
mod mapping_events;
#[path = "legacy/parser.rs"]
mod parser;
#[path = "legacy/presentation.rs"]
mod presentation;
#[path = "legacy/signals.rs"]
mod signals;
#[path = "legacy/spec_roundtrip.rs"]
mod spec_roundtrip;
#[path = "legacy/transport.rs"]
mod transport;
#[path = "legacy/variable_admission.rs"]
mod variable_admission;
fn main() {
    if let Err(error) = std::env::current_exe()
        .map_err(catabolic_store::Error::from)
        .and_then(|path| catabolic_runtime::notifications::register_provider_helper(&path))
    {
        eprintln!("error: {error}");
        std::process::exit(2);
    }
    let mut args = match std::env::args_os()
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
    if args.len() == 1 && args[0] == catabolic_runtime::notifications::WORKER_ARGUMENT {
        catabolic_runtime::notifications::worker_main();
        return;
    }
    if args.len() == 1 && args[0] == catabolic_runtime::legacy_refresh::WORKER_ARGUMENT {
        catabolic_runtime::legacy_refresh::worker_main();
        return;
    }
    if args.len() == 1 && args[0] == catabolic_runtime::imports::IDENTITY_WORKER {
        catabolic_runtime::imports::identity_worker_main();
        return;
    }
    if args.len() == 1 && args[0] == catabolic_runtime::identify::WORKER_ARGUMENT {
        catabolic_runtime::identify::worker_main();
        return;
    }
    if args.len() == 1 && args[0] == catabolic_runtime::processors::WORKER_ARGUMENT {
        catabolic_runtime::processors::worker_main();
        return;
    }
    if args.len() == 1 && args[0] == dispatch::consumer_watch::WORKER_ARGUMENT {
        dispatch::consumer_watch::worker_main();
        return;
    }
    let callable = args.len() == 1 && args[0] == transport::ARGUMENT;
    if callable {
        let request = transport::start().unwrap_or_else(|error| {
            eprintln!("native Python CLI transport failed: {error}");
            std::process::exit(1);
        });
        if request["kind"] == "render-error" {
            let context = &request["context"];
            let error = &request["error"];
            if !context.is_object() || !error["type"].is_string() || !error["message"].is_string() {
                std::process::exit(1);
            }
            if transport::dispatching(context).is_err() {
                std::process::exit(1);
            }
            if transport::reporting_error().is_err() {
                std::process::exit(1);
            }
            deliver(
                presentation::python_error(context, error.clone()),
                true,
                false,
            );
        }
        args = serde_json::from_value(request["argv"].clone())
            .unwrap_or_else(|_| std::process::exit(1));
    }
    let mut parse_exit = false;
    let output = match parser::parse(&args) {
        Ok(args) => {
            if callable && transport::dispatching(&args).is_err() {
                std::process::exit(1);
            }
            let result = if args["command"] == "spec" && args["operation"] == "roundtrip" {
                spec_roundtrip::execute(&args).map_err(Into::into)
            } else if args["command"] == "projection"
                && args["operation"] == "mapping-events"
                // Preserve shared dispatch's omitted-database error/provenance.
                && !dispatch::text(&args, "db").is_empty()
            {
                mapping_events::execute(&args)
            } else {
                let result = execute(&args);
                // Command owners, helper supervisors and runtimes have returned.
                // Do not render an interrupt/error envelope for standalone TERM.
                signals::finish();
                result.map(|value| presentation::success(&args, value))
            };
            match result {
                Ok(output) => output,
                Err(error) => {
                    if transport::reporting_error().is_err() {
                        std::process::exit(1);
                    }
                    presentation::legacy_error(&args, error)
                }
            }
        }
        Err(output) => {
            parse_exit = true;
            output
        }
    };
    deliver(output, callable, parse_exit);
}

fn deliver(output: parser::Output, callable: bool, parse_exit: bool) -> ! {
    if transport::write("stdout", &output.stdout, false).is_err()
        || transport::write("stderr", &output.stderr, false).is_err()
        || (callable && transport::finished(output.code, parse_exit).is_err())
    {
        std::process::exit(1);
    }
    std::process::exit(if callable { 0 } else { output.code });
}

fn execute(args: &serde_json::Value) -> failure::Result<serde_json::Value> {
    if args["command"] == "graphql" {
        let args = args.clone();
        return catabolic_http::interruptible_detailed(move |cancel| {
            let check = cancel.clone();
            let result = catabolic_store::embedding::with_interrupt_check(
                move || check.load(std::sync::atomic::Ordering::Acquire),
                || Ok(graphql_cli::execute(&args)),
            )?;
            if cancel.load(std::sync::atomic::Ordering::Acquire) {
                Err(catabolic_store::Error("native legacy CLI interrupted".into()).into())
            } else {
                result
            }
        })?;
    }
    if matches!(args["command"].as_str(), Some("query" | "inbox"))
        || (args["command"] == "rule"
            && args["operation"] == "run"
            && args["worker"].as_str().is_some_and(|v| !v.is_empty()))
        || (args["command"] == "projection"
            && matches!(
                args["operation"].as_str(),
                Some(
                    "mapping-preview"
                        | "mapping-apply"
                        | "mapping-events"
                        | "mapping-recover"
                        | "preview"
                        | "execute"
                )
            ))
    {
        let args = args.clone();
        let external_rule = args["command"] == "rule";
        let operation = move |cancel: std::sync::Arc<std::sync::atomic::AtomicBool>| {
            let check = cancel.clone();
            let result = catabolic_store::embedding::with_interrupt_check(
                move || check.load(std::sync::atomic::Ordering::Acquire),
                || dispatch::dispatch_with_cancel(&args, &cancel),
            );
            if cancel.load(std::sync::atomic::Ordering::Acquire) {
                Err(catabolic_store::Error(
                    "native legacy CLI interrupted".into(),
                ))
            } else {
                result
            }
        };
        if external_rule {
            signals::interruptible(operation)
        } else {
            catabolic_http::interruptible(operation)
        }
        .map_err(Into::into)
    } else {
        dispatch::dispatch_detailed(args)
    }
}
