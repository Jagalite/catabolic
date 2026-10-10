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
        [command, value] if command == "encode" => {
            Ok(json!({"encoded":catabolic_core::encode(&serde_json::from_str::<Value>(value)?)}))
        }
        [command, entity] if command == "catalog" => {
            let store = catabolic_store::Store::open(&path, false, false)?;
            catabolic_store::query::CatalogQuery::new(&store, &profile)?
                .execute(entity, &serde_json::from_str(&options)?)
        }
        [command, document] if command == "graphql" => catabolic_store::graphql::execute(
            &path,
            document,
            &profile,
            serde_json::from_str(&variables)?,
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
            let result = line
                .map_err(Error::from)
                .and_then(|line| serde_json::from_str::<Vec<String>>(&line).map_err(Error::from))
                .and_then(run);
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
