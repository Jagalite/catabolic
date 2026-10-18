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
/// Write an operator artifact once. An existing pathname, including a symlink, is
/// refused rather than replaced so an approved plan cannot be silently overwritten.
fn write_new_file(path: &Path, value: &Value) -> Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| Error(format!("cannot create {}: {error}", path.display())))?;
    file.write_all(serde_json::to_string_pretty(value)?.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    // The new directory entry is durable only once its directory is synced.
    let absolute = std::path::absolute(path)?;
    let parent = absolute
        .parent()
        .ok_or_else(|| Error(format!("{} has no parent directory", path.display())))?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}
/// Read a bounded JSON operator artifact without following a final symlink.
fn read_json_file(path: &Path) -> Result<Value> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    const LIMIT: u64 = 256 * 1024 * 1024;
    let mut raw = Vec::new();
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| Error(format!("cannot read {}: {error}", path.display())))?
        .take(LIMIT + 1)
        .read_to_end(&mut raw)?;
    if raw.len() as u64 > LIMIT {
        return Err(Error(format!("{} exceeds 256 MiB", path.display())));
    }
    Ok(catabolic_core::decode_json_bytes(&raw)?)
}
/// Each operator assertion is named explicitly; unknown names are refused and
/// missing names are left for the store's own quiescence refusal.
fn quiescence(assertions: &[String]) -> Result<migration::NativeUpgradeQuiescence> {
    let mut quiescence = migration::NativeUpgradeQuiescence::default();
    for assertion in assertions {
        match assertion.as_str() {
            "admission-stopped" => quiescence.admission_stopped = true,
            "readers-and-workers-stopped" => quiescence.readers_and_workers_stopped = true,
            "exclusive-operational-ownership" => {
                quiescence.exclusive_operational_ownership = true;
            }
            other => {
                return Err(Error(format!(
                    "unknown --assert-quiescent value {other:?}; expected admission-stopped, readers-and-workers-stopped or exclusive-operational-ownership"
                )));
            }
        }
    }
    Ok(quiescence)
}
fn run(mut args: Vec<String>) -> Result<Value> {
    let path =
        PathBuf::from(argument(&mut args, "--db")?.unwrap_or_else(|| "catabolic.sqlite3".into()));
    let _json = flag(&mut args, "--json");
    let full = flag(&mut args, "--full");
    let dry_run = flag(&mut args, "--dry-run");
    let backup = argument(&mut args, "--backup-dir")?.map(PathBuf::from);
    let failpoint = argument(&mut args, "--migration-failpoint")?;
    let failure = |stage: &str, version: u32| {
        if failpoint.as_deref() == Some(&format!("{stage}:{version}")) {
            return Err(Error(format!(
                "injected migration failure: {stage}:{version}"
            )));
        }
        Ok(())
    };
    // Lifecycle options are only consumed for db commands, so other command
    // families keep their existing argument errors.
    let lifecycle = args.first().is_some_and(|command| command == "db");
    let mut plan_out = None;
    let mut plan_in = None;
    let mut assertions = Vec::new();
    let mut compared = (None, None);
    let mut journal_target = None;
    let mut restore_source = None;
    if lifecycle {
        plan_out = argument(&mut args, "--out")?.map(PathBuf::from);
        plan_in = argument(&mut args, "--plan")?.map(PathBuf::from);
        while let Some(assertion) = argument(&mut args, "--assert-quiescent")? {
            assertions.push(assertion);
        }
        compared = (
            argument(&mut args, "--before")?.map(PathBuf::from),
            argument(&mut args, "--after")?.map(PathBuf::from),
        );
        journal_target = argument(&mut args, "--to")?;
        restore_source = argument(&mut args, "--backup")?.map(PathBuf::from);
        let sub = args.get(1).map(String::as_str);
        if journal_target.is_some() && sub != Some("convert-journal") {
            return Err(Error("--to is only accepted by db convert-journal".into()));
        }
        if plan_out.is_some() && !matches!(sub, Some("native-plan" | "identity-manifest")) {
            return Err(Error(
                "--out is only accepted by db native-plan and db identity-manifest".into(),
            ));
        }
        if (compared.0.is_some() || compared.1.is_some()) && sub != Some("identity-diff") {
            return Err(Error(
                "--before and --after are only accepted by db identity-diff".into(),
            ));
        }
        if plan_in.is_some() && sub != Some("native-upgrade") {
            return Err(Error("--plan is only accepted by db native-upgrade".into()));
        }
        if restore_source.is_some() && sub != Some("native-restore") {
            return Err(Error(
                "--backup is only accepted by db native-restore".into(),
            ));
        }
        if !assertions.is_empty()
            && !matches!(
                sub,
                Some("native-upgrade" | "convert-journal" | "native-restore")
            )
        {
            return Err(Error(
                "--assert-quiescent is only accepted by db native-upgrade, db convert-journal and db native-restore"
                    .into(),
            ));
        }
    }
    let options = argument(&mut args, "--options")?.unwrap_or_else(|| "{}".into());
    let variables = argument(&mut args, "--variables")?.unwrap_or_else(|| "{}".into());
    let operation = argument(&mut args, "--operation-name")?;
    let profile = argument(&mut args, "--profile")?.unwrap_or_else(|| "default".into());
    // Only the legacy upgrade implements a read-only dry run. Every other
    // command would silently perform its real effect, so refuse it before any.
    if dry_run && !args.starts_with(&["db".into(), "upgrade".into()]) {
        return Err(Error(
            "--dry-run is only implemented by db upgrade; use db native-inspect or db native-plan for a read-only native preview".into(),
        ));
    }
    if args.starts_with(&["api".into(), "worker".into()]) {
        let once = flag(&mut args, "--once");
        let interval = argument(&mut args, "--interval")?
            .unwrap_or_else(|| "2".into())
            .parse::<f64>()
            .map_err(|_| Error("invalid API worker interval".into()))?;
        if !interval.is_finite() || interval <= 0.0 || interval > 86400.0 {
            return Err(Error("invalid API worker interval".into()));
        }
        if args != ["api", "worker"] {
            return Err(Error("unknown native API worker arguments".into()));
        }
        return catabolic_http::worker(
            &path,
            &profile,
            once,
            std::time::Duration::from_secs_f64(interval),
        );
    }
    if args.starts_with(&["api".into(), "serve".into()]) {
        if !flag(&mut args, "--experimental-native-http") {
            return Err(Error("native HTTP is unqualified; disposable loopback fixtures require --experimental-native-http".into()));
        }
        let host = argument(&mut args, "--host")?.unwrap_or_else(|| "127.0.0.1".into());
        let port = argument(&mut args, "--port")?
            .unwrap_or_else(|| "8421".into())
            .parse::<u16>()
            .map_err(|_| Error("port must be between 0 and 65535".into()))?;
        let read_only = !flag(&mut args, "--enable-processing");
        let mut config = catabolic_http::ServerConfig {
            host,
            port,
            read_only,
            ..Default::default()
        };
        let mut hosts = Vec::new();
        while let Some(host) = argument(&mut args, "--allowed-host")? {
            hosts.push(host);
        }
        if !hosts.is_empty() {
            config.allowed_hosts = hosts;
        }
        while let Some(origin) = argument(&mut args, "--cors-origin")? {
            config.origins.push(origin);
        }
        for (name, target) in [
            ("--streams", &mut config.streams),
            ("--streams-per-principal", &mut config.per_principal),
            ("--rate-limit", &mut config.rate),
        ] {
            if let Some(value) = argument(&mut args, name)? {
                *target = value
                    .parse()
                    .map_err(|_| Error(format!("invalid {name}")))?;
            }
        }
        if args != ["api", "serve"] {
            return Err(Error("unknown native API serve arguments".into()));
        }
        catabolic_http::serve(&path, config)?;
        return Ok(Value::Null);
    }
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
                "workflow",
                "rules",
                "layout",
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
        [command, action]
            if [
                "fallback-probe",
                catabolic_runtime::fallback::NATIVE_PROBE_COMMAND,
            ]
            .contains(&command.as_str())
                && action == "check" =>
        {
            use std::io::Read;
            let mut input = Vec::new();
            std::io::stdin()
                .lock()
                .take(1024 * 1024 + 1)
                .read_to_end(&mut input)?;
            if input.len() > 1024 * 1024 {
                return Err(Error("fallback probe input exceeds 1 MiB".into()));
            }
            let request = admitted_json_bytes(&input)?;
            let value = if command == catabolic_runtime::fallback::NATIVE_PROBE_COMMAND {
                catabolic_runtime::fallback::probe_check_v1(&request)?
            } else {
                catabolic_runtime::fallback::probe_check(&request)?
            };
            if value.to_string().len() >= 64 * 1024 {
                return Err(Error("fallback probe response exceeds 64 KiB".into()));
            }
            Ok(value)
        }
        [command, name] if command == catabolic_runtime::watchers::NATIVE_WORKER_COMMAND => {
            catabolic_runtime::watchers::worker_v1(
                &path,
                &profile,
                name,
                &admitted_json(&options)?,
            )
        }
        [command, name] if command == "watcher-worker" => {
            catabolic_runtime::watchers::worker_with_options(
                &path,
                &profile,
                name,
                &admitted_json(&options)?,
            )
        }
        [command, payload] if command == catabolic_runtime::observation::SHARED_WORKER_COMMAND => {
            catabolic_runtime::observation::shared_worker_v1(
                &path,
                &profile,
                &admitted_json(payload)?,
            )
        }
        [command, payload] if command == catabolic_runtime::observation::CLOSED_WORKER_COMMAND => {
            catabolic_runtime::observation::worker_v2(
                &path,
                &profile,
                &admitted_json(payload)?,
            )
        }
        [command, payload] if command == catabolic_runtime::observation::NATIVE_WORKER_COMMAND => {
            catabolic_runtime::observation::worker_v1(
                &path,
                &profile,
                &admitted_json(payload)?,
            )
        }
        [command, payload] if command == "observation-worker" => {
            catabolic_runtime::observation::worker(
                &path,
                &profile,
                &admitted_json(payload)?,
            )
        }
        [command] if command == "--version" || command == "version" => Ok(
            json!({"version":env!("CARGO_PKG_VERSION"),"schema":migration::SCHEMA_VERSION,"reference":migration::reference()["reference_commit"]}),
        ),
        [command] if command == "init" => migration::initialize(&path),
        [command, sub] if command == "db" && sub == "inspect" => migration::inspect(&path, full),
        [command, sub] if command == "db" && sub == "upgrade" => {
            migration::upgrade(&path, dry_run, backup.as_deref(), &|stage, _, version| {
                failure(stage, version)
            })
        }
        [command, sub] if command == "db" && sub == "native-inspect" => {
            migration::inspect_native(&path, full)
        }
        [command, sub] if command == "db" && sub == "native-plan" => {
            let out = plan_out.ok_or_else(|| Error("db native-plan requires --out PLAN.json".into()))?;
            let plan = migration::plan_native(&path)?;
            write_new_file(&out, &plan)?;
            Ok(json!({
                "database":plan["input"]["database"], "database_id":plan["input"]["database_id"],
                "plan":std::path::absolute(&out)?, "plan_fingerprint":plan["plan_fingerprint"],
                "input_fingerprint":plan["input_fingerprint"], "from_schema":plan["from_schema"],
                "to_schema":plan["to_schema"], "migrations":plan["migrations"],
                "work_settled":plan["work_settled"], "pending_work":plan["input"]["pending_work"],
                "journal_mode":plan["input"]["journal_mode"],
                "quiescence_required":plan["quiescence_required"],
                "quiescence_preconditions":plan["quiescence_preconditions"]
            }))
        }
        [command, sub] if command == "db" && sub == "native-upgrade" => {
            let plan = plan_in.ok_or_else(|| {
                Error("db native-upgrade requires --plan PLAN.json from db native-plan".into())
            })?;
            let quiescence = quiescence(&assertions)?;
            let plan = read_json_file(&plan)?;
            migration::upgrade_native(
                &path,
                &plan,
                quiescence,
                backup.as_deref(),
                &|stage, _, version| failure(stage, version),
            )
        }
        [command, sub] if command == "db" && sub == "identity-manifest" => {
            let manifest = catabolic_store::identity_manifest::manifest_path(&path)?;
            if let Some(out) = plan_out {
                write_new_file(&out, &manifest)?;
            }
            Ok(manifest)
        }
        [command, sub] if command == "db" && sub == "identity-diff" => {
            let (Some(before), Some(after)) = compared else {
                return Err(Error(
                    "db identity-diff requires --before MANIFEST.json and --after MANIFEST.json"
                        .into(),
                ));
            };
            let (before, after) = (read_json_file(&before)?, read_json_file(&after)?);
            catabolic_store::identity_manifest::require_preserved(&before, &after)?;
            Ok(catabolic_store::identity_manifest::diff(&before, &after))
        }
        [command, sub] if command == "db" && sub == "native-backup" => {
            catabolic_store::native_restore::backup(&path, backup.as_deref())
        }
        [command, sub] if command == "db" && sub == "native-restore" => {
            let source = restore_source.ok_or_else(|| {
                Error("db native-restore requires --backup DIRECTORY from db native-backup".into())
            })?;
            let quiescence = quiescence(&assertions)?;
            catabolic_store::native_restore::restore(&path, &source, quiescence)
        }
        [command, sub] if command == "db" && sub == "convert-journal" => {
            let target = journal_target
                .ok_or_else(|| Error("db convert-journal requires --to delete".into()))?;
            let quiescence = quiescence(&assertions)?;
            catabolic_store::journal_conversion::convert(&path, &target, quiescence)
        }
        [command, sub] if command == "db" && sub == "snapshot" => {
            let path = catabolic_store::database_path(Path::new(&path))?;
            let db = catabolic_store::connect(&path, false)?;
            db.execute_batch("BEGIN")?;
            migration::snapshot(&db)
        }
        [command] if command == "manifest-build" => {
            let store = catabolic_store::Store::open(&path, false, false)?;
            let request = admitted_json(&options)?;
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
            &admitted_json(&options)?,
        ),
        [command, surface, operation]
            if command == "operation" && surface == "import" && operation == "catalog" =>
        {
            let request = admitted_json(&options)?;
            catabolic_http::interruptible(move |cancel| {
                catabolic_runtime::api::execute_import_with_poll(
                    &path,
                    &profile,
                    &request,
                    &cancel,
                    &mut || {},
                )
            })
        }
        [command, surface, operation]
            if command == "operation" && surface == "plex-import" && operation == "run" =>
        {
            let request = admitted_json(&options)?;
            catabolic_http::interruptible(move |cancel| {
                catabolic_runtime::api::execute_plex_import_with_poll(
                    &path,
                    &profile,
                    &request,
                    &cancel,
                    &mut || {},
                )
            })
        }
        [command, surface, operation]
            if command == "operation" && surface == "plex-match" && operation == "run" =>
        {
            let request = admitted_json(&options)?;
            catabolic_http::interruptible(move |cancel| {
                catabolic_runtime::api::execute_plex_match_with_poll(
                    &path,
                    &profile,
                    &request,
                    &cancel,
                    &mut || {},
                )
            })
        }
        [command, surface, operation]
            if command == "operation" && surface == "plex-metadata" && operation == "run" =>
        {
            let request = admitted_json(&options)?;
            catabolic_runtime::plex_metadata::execute_cli(&path, &profile, &request)
        }
        [command, surface, operation] if command == "operation" && surface == "plex-login" => {
            catabolic_runtime::plex_login::execute_cli(
                operation,
                &admitted_json(&options)?,
            )
        }

        [command, surface, operation] if command == "operation" => catabolic_runtime::api::execute(
            &path,
            &profile,
            surface,
            operation,
            &admitted_json(&options)?,
        ),
        [command, action] if command == "observe" || command == "outputs" => {
            catabolic_runtime::api::execute(
                &path,
                &profile,
                command,
                action,
                &admitted_json(&options)?,
            )
        }
        [command, op, ids @ ..] if command == "media-enqueue" => {
            let store = catabolic_store::Store::open(&path, true, false)?;
            let request: Value = admitted_json(&options)?;
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
            catabolic_store::mutation::profile(&store, &profile)?;
            catabolic_runtime::operations::put(
                &store,
                &profile,
                name,
                &catabolic_core::decode_json(definition)?,
            )
        }
        [command, action] if command == "notify" => catabolic_runtime::api::execute(
            &path,
            &profile,
            "notify",
            action,
            &admitted_json(&options)?,
        ),
        [command, action, request] if command == "interchange" => {
            catabolic_store::interchange::dispatch(action, &admitted_json(request)?)
        }
        [command, action, request]
            if command == "definitions"
                || command == "curation"
                || command == "programs"
                || command == "plans"
                || command == "workflow"
                || command == "rules"
                || command == "layout" =>
        {
            catabolic_runtime::api::execute_with_source(
                &path,
                &profile,
                command,
                action,
                &admitted_json(request)?,
                request,
            )
        }
        [command, action] if command == "tag" => {
            let store = catabolic_store::Store::open(&path, true, false)?;
            catabolic_store::tagging::dispatch(
                &store,
                &profile,
                action,
                &admitted_json(&options)?,
            )
        }
        [command, id, catalog] if command == "layout-plan" => {
            let store = catabolic_store::Store::open(&path, false, false)?;
            let request: Value = admitted_json(&options)?;
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
                .execute(entity, &admitted_json(&options)?)
        }
        [command, _document] if command == "graphql" && http => Err(Error(
            "GraphQL HTTP evaluation requires authorization context".into(),
        )),
        [command, document] if command == "graphql" => catabolic_store::graphql::execute(
            &path,
            document,
            &profile,
            catabolic_store::graphql::InputVariables::from_json(&variables)?,
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
            "supported commands: init, db inspect, db upgrade, db snapshot, db native-inspect, db native-plan, db native-upgrade, db identity-manifest, db identity-diff, db convert-journal, db native-backup, db native-restore, encode, --version"
                .into(),
        )),
    }
}
/// Decode an external request. A build without the `failpoints` feature
/// refuses the test-only `failpoint`/`crash` fields here, before dispatch, so
/// no route can act on such a request first.
fn admitted_json(raw: &str) -> Result<Value> {
    let value = catabolic_core::decode_json(raw)?;
    catabolic_store::filesystem_mutation::admit_failpoints(&value)?;
    Ok(value)
}
fn admitted_json_bytes(raw: &[u8]) -> Result<Value> {
    admitted_json(std::str::from_utf8(raw).map_err(|e| Error(e.to_string()))?)
}
fn main() {
    // First: tool supervisor mode (never returns) or supervisor registration.
    catabolic_runtime::process::supervise_or_configure();
    if let Err(error) = std::env::current_exe()
        .map_err(Error::from)
        .and_then(|path| catabolic_runtime::helper::register(&path, None))
    {
        eprintln!("catabolic: {error}");
        std::process::exit(2);
    }
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args == [catabolic_runtime::notifications::WORKER_ARGUMENT] {
        catabolic_runtime::notifications::worker_main();
        return;
    }
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
            std::process::exit(
                if error.0 == catabolic_runtime::plex_matching::INTERRUPTED
                    || error.0 == catabolic_runtime::plex_import::INTERRUPTED
                    || error.0 == catabolic_runtime::plex_login::INTERRUPTED
                    || error.0 == catabolic_runtime::plex_metadata::INTERRUPTED
                {
                    130
                } else {
                    2
                },
            );
        }
    }
}
