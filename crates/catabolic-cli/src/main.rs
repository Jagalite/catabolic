use catabolic_store::{Error, Result, migration};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn argument(args: &mut Vec<String>, name: &str) -> Result<Option<String>> {
    if let Some(i) = args.iter().position(|v| v == name) {
        if i + 1 >= args.len() {
            return Err(Error(format!("{name} requires a value")));
        }
        args.remove(i);
        return Ok(Some(args.remove(i)));
    }
    Ok(None)
}
fn flag(args: &mut Vec<String>, name: &str) -> bool {
    if let Some(i) = args.iter().position(|v| v == name) {
        args.remove(i);
        true
    } else {
        false
    }
}
fn run(mut args: Vec<String>) -> Result<Value> {
    let path =
        PathBuf::from(argument(&mut args, "--db")?.unwrap_or_else(|| "catabolic.sqlite3".into()));
    let _json = flag(&mut args, "--json");
    let full = flag(&mut args, "--full");
    let dry_run = flag(&mut args, "--dry-run");
    let backup = argument(&mut args, "--backup-dir")?.map(PathBuf::from);
    let failpoint = argument(&mut args, "--migration-failpoint")?;
    let options = argument(&mut args, "--options")?.unwrap_or_else(|| "{}".into());
    let variables = argument(&mut args, "--variables")?.unwrap_or_else(|| "{}".into());
    let operation = argument(&mut args, "--operation-name")?;
    let profile = argument(&mut args, "--profile")?.unwrap_or_else(|| "default".into());
    let params = argument(&mut args, "--params")?;
    let describe = flag(&mut args, "--schema");
    let stable = flag(&mut args, "--stable");
    let http = flag(&mut args, "--http");
    let max_rows = argument(&mut args, "--max-rows")?
        .map(|s| {
            s.parse::<usize>()
                .map_err(|_| Error("max-rows must be between 1 and 10000".into()))
        })
        .transpose()?
        .unwrap_or(1000);
    let timeout_ms = argument(&mut args, "--timeout-ms")?
        .map(|s| {
            s.parse::<u64>()
                .map_err(|_| Error("timeout-ms must be between 1 and 60000".into()))
        })
        .transpose()?
        .unwrap_or(5000);
    if http
        && args.first().is_some_and(|command| {
            [
                "operation",
                "tag",
                "definitions",
                "curation",
                "programs",
                "plans",
                "manifest-build",
                "manifest-write",
                "notify",
                "observe",
                "outputs",
                "media-enqueue",
                "media-run",
                "media-cancel",
                "media-retry",
                "operation-put",
            ]
            .contains(&command.as_str())
        })
    {
        return Err(Error(
            "native mutation HTTP evaluation requires authorization context".into(),
        ));
    }
    match args.as_slice() {
        [command] if command == "--version" || command == "version" => Ok(
            json!({"version":env!("CARGO_PKG_VERSION"),"schema":migration::SCHEMA_VERSION,"reference":migration::reference()["reference_commit"]}),
        ),
        [command] if command == "init" => migration::initialize(&path),
        [command, sub] if command == "db" && sub == "inspect" => migration::inspect(&path, full),
        [command, sub] if command == "db" && sub == "upgrade" => {
            migration::upgrade(&path, dry_run, backup.as_deref(), &|stage, _, version| {
                if failpoint.as_deref() == Some(&format!("{stage}:{version}")) {
                    return Err(Error(format!(
                        "injected migration failure: {stage}:{version}"
                    )));
                }
                Ok(())
            })
        }
        [command, sub] if command == "db" && sub == "snapshot" => {
            let path = catabolic_store::database_path(Path::new(&path))?;
            let db = catabolic_store::connect(&path, false)?;
            db.execute_batch("BEGIN")?;
            migration::snapshot(&db)
        }
        [command] if command == "manifest-build" => {
            let store = catabolic_store::Store::open(&path, false, false)?;
            let request = catabolic_core::decode_json(&options)?;
            let mut evaluation =
                catabolic_store::selection::Evaluation::new(store, &profile, 60000, 100000, http)?;
            catabolic_store::manifest::build(
                &mut evaluation,
                request["catalog"].as_str().unwrap_or("global"),
                request.get("extra"),
                request.get("selection"),
            )
        }
        [command] if command == "manifest-write" => catabolic_runtime::api::execute(
            &path,
            &profile,
            "manifest",
            "write",
            &catabolic_core::decode_json(&options)?,
        ),
        [command, surface, operation] if command == "operation" => catabolic_runtime::api::execute(
            &path,
            &profile,
            surface,
            operation,
            &catabolic_core::decode_json(&options)?,
        ),
        [command, action] if command == "observe" || command == "outputs" => {
            catabolic_runtime::api::execute(
                &path,
                &profile,
                command,
                action,
                &catabolic_core::decode_json(&options)?,
            )
        }
        [command, op, ids @ ..] if command == "media-enqueue" => {
            let store = catabolic_store::Store::open(&path, true, false)?;
            let request: Value = catabolic_core::decode_json(&options)?;
            catabolic_runtime::media::enqueue(&store, &profile, op, ids, &request, false, None)
        }
        [command, ids @ ..] if command == "media-run" => {
            let mut store = catabolic_store::Store::open(&path, true, false)?;
            catabolic_runtime::media::run(
                &mut store,
                &profile,
                ids,
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
        }
        [command, id] if command == "media-get" => {
            let store = catabolic_store::Store::open(&path, false, false)?;
            catabolic_store::media_mutation::get_job(&store, &profile, id)
        }
        [command, id] if command == "media-cancel" || command == "media-retry" => {
            let store = catabolic_store::Store::open(&path, true, false)?;
            if command == "media-cancel" {
                catabolic_store::media_mutation::cancel(&store, &profile, id)
            } else {
                catabolic_store::media_mutation::retry(&store, &profile, id)
            }
        }
        [command, name, definition] if command == "operation-put" => {
            let store = catabolic_store::Store::open(&path, true, false)?;
            catabolic_runtime::media::put_operation(
                &store,
                name,
                &catabolic_core::decode_json(definition)?,
            )
        }
        [command, action] if command == "notify" => catabolic_runtime::api::execute(
            &path,
            &profile,
            "notify",
            action,
            &catabolic_core::decode_json(&options)?,
        ),
        [command, action, request] if command == "interchange" => {
            catabolic_store::interchange::dispatch(action, &catabolic_core::decode_json(request)?)
        }
        [command, action, request]
            if command == "definitions"
                || command == "curation"
                || command == "programs"
                || command == "plans" =>
        {
            catabolic_runtime::api::execute(
                &path,
                &profile,
                command,
                action,
                &catabolic_core::decode_json(request)?,
            )
        }
        [command, action] if command == "tag" => {
            let store = catabolic_store::Store::open(&path, true, false)?;
            catabolic_store::tagging::dispatch(
                &store,
                &profile,
                action,
                &catabolic_core::decode_json(&options)?,
            )
        }
        [command, id, catalog] if command == "layout-plan" => {
            let store = catabolic_store::Store::open(&path, false, false)?;
            let request: Value = catabolic_core::decode_json(&options)?;
            let mut evaluation =
                catabolic_store::selection::Evaluation::new(store, &profile, 60000, 100000, http)?;
            catabolic_store::layout::plan(
                &mut evaluation,
                id,
                catalog,
                request["replace_layout"] == true,
                request["allow_empty"] == true,
                request["limit"].as_u64().unwrap_or(100) as usize,
            )
        }
        [command, definition] if command == "select" => {
            let selection: Value = catabolic_core::decode_json(definition)?;
            let store = catabolic_store::Store::open(&path, false, false)?;
            let mut evaluation = catabolic_store::selection::Evaluation::new(
                store,
                &profile,
                selection["timeout_ms"].as_u64().unwrap_or(5000),
                selection["max_ids"].as_u64().unwrap_or(10000) as usize,
                http,
            )?;
            let result = evaluation.select(&selection)?;
            Ok(json!({"entity":result.entity,"ids":result.ids,"report":result.report}))
        }
        [command, id] if command == "saved-query" => {
            let store = catabolic_store::Store::open(&path, false, false)?;
            let mut evaluation =
                catabolic_store::selection::Evaluation::new(store, &profile, 60000, 100000, http)?;
            evaluation.run(id, max_rows)
        }
        [command, value] if command == "encode" => {
            Ok(json!({"encoded":catabolic_core::encode(&catabolic_core::decode_json(value)?)}))
        }
        [command, _entity] if command == "catalog" && http => Err(Error(
            "catalog HTTP evaluation requires authorization context".into(),
        )),
        [command, entity] if command == "catalog" => {
            let store = catabolic_store::Store::open(&path, false, false)?;
            catabolic_store::query::CatalogQuery::new(&store, &profile)?
                .execute(entity, &catabolic_core::decode_json(&options)?)
        }
        [command, _document] if command == "graphql" && http => Err(Error(
            "GraphQL HTTP evaluation requires authorization context".into(),
        )),
        [command, document] if command == "graphql" => catabolic_store::graphql::execute(
            &path,
            document,
            &profile,
            catabolic_core::decode_json(&variables)?,
            operation.as_deref(),
            timeout_ms,
        ),
        [command] if command == "graphql" && describe => {
            Ok(catabolic_store::graphql::description())
        }
        [command, rest @ ..] if command == "query" && rest.len() <= 1 => {
            catabolic_store::sql::execute(
                &path,
                rest.first().map(String::as_str),
                &catabolic_store::sql::Options {
                    profile: &profile,
                    params: params.as_deref(),
                    describe,
                    max_rows,
                    timeout_ms,
                    stable,
                    http,
                },
            )
        }
        _ => Err(Error(
            "supported commands: init, db inspect, db upgrade, db snapshot, encode, --version"
                .into(),
        )),
    }
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args == ["--stdio"] {
        use std::io::{BufRead, Write};
        for line in std::io::stdin().lock().lines() {
            use catabolic_core::lifecycle::{Effect, Input, State, reduce};
            let (request, _) = reduce(&State::default(), &Input::Attach(0));
            let result = line
                .map_err(Error::from)
                .and_then(|line| serde_json::from_str::<Vec<String>>(&line).map_err(Error::from))
                .and_then(run);
            let (_, effects) = reduce(&request, &Input::Complete(request.generation));
            if !effects.contains(&Effect::Deliver {
                caller: 0,
                generation: request.generation,
            }) {
                break;
            }
            let reply = match result {
                Ok(value) => json!({"status":0,"result":value}),
                Err(error) => json!({"status":2,"error":format!("catabolic: {error}")}),
            };
            println!("{reply}");
            if std::io::stdout().flush().is_err() {
                break;
            }
        }
        return;
    }
    match run(args) {
        Ok(value) => println!("{}", serde_json::to_string(&value).expect("result JSON")),
        Err(error) => {
            eprintln!("catabolic: {error}");
            std::process::exit(2);
        }
    }
}
