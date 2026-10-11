// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Legacy transport adaptation to production owners. No Python execution and no
//! automatic engine fallback, including after partially completed operations.
use super::parser::resource;
use catabolic_store::{Error, Result, Store};
use serde_json::{Value, json};
use std::{io::Read, path::Path};
pub fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
fn set(v: &mut Value, to: &str, from: &str) {
    v[to] = v[from].clone();
}
fn nonnull(v: &Value) -> Value {
    Value::Object(
        v.as_object()
            .unwrap()
            .iter()
            .filter(|(_, v)| !v.is_null())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    )
}
pub fn read_text(path: &str, maximum: usize) -> Result<String> {
    let mut bytes = Vec::new();
    if path == "-" {
        std::io::stdin()
            .lock()
            .take(maximum as u64 + 1)
            .read_to_end(&mut bytes)?;
    } else {
        std::fs::File::open(path)?
            .take(maximum as u64 + 1)
            .read_to_end(&mut bytes)?;
    }
    if bytes.len() > maximum {
        return Err(Error(format!("input exceeds {maximum} bytes")));
    }
    String::from_utf8(bytes).map_err(|e| Error(e.to_string()))
}
fn file(v: &Value, key: &str, maximum: usize) -> Result<Value> {
    catabolic_core::decode_json(&read_text(text(v, key), maximum)?).map_err(Into::into)
}
fn raw(v: &Value, key: &str) -> Result<Value> {
    catabolic_core::decode_json(text(v, key)).map_err(Into::into)
}
fn unavailable(command: &str, operation: &str) -> Result<Value> {
    Err(Error(format!(
        "native legacy CLI owner unavailable: {}{}; Python remains the default; no automatic fallback was attempted",
        command,
        if operation.is_empty() {
            String::new()
        } else {
            format!(" {operation}")
        }
    )))
}
fn runtime(a: &Value, surface: &str, operation: &str, r: &Value) -> Result<Value> {
    let mut request = nonnull(r);
    if surface == "outputs" && r.get("catalog").is_some_and(Value::is_null) {
        request["catalog"] = Value::Null;
    }
    catabolic_runtime::api::execute(
        Path::new(text(a, "db")),
        text(a, "profile"),
        surface,
        operation,
        &request,
    )
}
fn application(a: &Value, operation: &str, r: &Value, writable: bool) -> Result<Value> {
    let store = Store::open(Path::new(text(a, "db")), writable, false)?;
    catabolic_store::embedding::application(&store, text(a, "profile"), operation, r)
}
fn query(a: &Value, entity: &str, r: &Value) -> Result<Value> {
    let store = Store::open(Path::new(text(a, "db")), false, false)?;
    catabolic_store::query::CatalogQuery::new(&store, text(a, "profile"))?.execute(entity, r)
}
fn definitions(a: &Value, operation: &str, r: &Value) -> Result<Value> {
    runtime(a, "definitions", operation, r)
}
fn diff(before: &Value, after: &Value, path: &str, out: &mut Vec<String>) {
    if let (Some(a), Some(b)) = (before.as_object(), after.as_object()) {
        let keys = a
            .keys()
            .chain(b.keys())
            .collect::<std::collections::BTreeSet<_>>();
        for key in keys {
            let p = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
            match (a.get(key), b.get(key)) {
                (Some(x), Some(y)) => diff(x, y, &p, out),
                _ => out.push(p),
            }
        }
    } else if before != after {
        out.push(if path.is_empty() {
            "/".into()
        } else {
            path.into()
        });
    }
}
fn documentation(a: &Value) -> Result<Value> {
    let topic = a["topic"].as_str();
    let search = a["search"].as_str();
    if topic.is_some() && search.is_some() {
        return Err(Error(
            "choose a documentation topic or --search, not both".into(),
        ));
    }
    if let Some(topic) = topic {
        return resource()["docs"].get(topic).cloned().ok_or_else(|| {
            Error(format!(
                "unknown documentation topic: {topic}; run catabolic docs to list topics"
            ))
        });
    }
    let Some(search) = search else {
        return Ok(resource()["docs"][""].clone());
    };
    let terms = catabolic_store::sql::casefold(search)
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Err(Error("documentation search must not be empty".into()));
    }
    let mut topics = vec![];
    for entry in resource()["docs"][""]["topics"].as_array().unwrap() {
        let markdown = text(&resource()["docs"][text(entry, "topic")], "markdown");
        let haystack = catabolic_store::sql::casefold(&format!(
            "{} {} {} {markdown}",
            text(entry, "topic"),
            text(entry, "title"),
            text(entry, "summary")
        ));
        if terms.iter().all(|t| haystack.contains(t)) {
            let mut entry = entry.clone();
            let matches = markdown
                .lines()
                .enumerate()
                .filter(|(_, l)| {
                    terms
                        .iter()
                        .any(|t| catabolic_store::sql::casefold(l).contains(t))
                })
                .take(3)
                .map(|(i, l)| json!({"line":i+1,"text":l.chars().take(240).collect::<String>()}))
                .collect::<Vec<_>>();
            entry["matches"] = json!(matches);
            topics.push(entry);
        }
    }
    Ok(json!({"documentation_version":1,"query":search,"topics":topics}))
}
pub fn dispatch(a: &Value) -> Result<Value> {
    let command = text(a, "command");
    let operation = text(a, "operation");
    let profile = text(a, "profile");
    let path = Path::new(text(a, "db"));
    let mut r = a.clone();
    match (command, operation) {
        ("docs", _) => return documentation(a),
        ("target", "list" | "show") => {
            return catabolic_store::targets::describe(a["name"].as_str());
        }
        ("item", "types") => return Ok(resource()["types"].clone()),
        ("layout", "presets") => return Ok(json!({"presets":resource()["presets"]})),
        ("artifact", "capabilities") => return catabolic_runtime::rendering::capabilities(),
        ("operation", "types") => return Ok(resource()["operation_types"].clone()),
        ("watcher", "schema") => return Ok(resource()["extra_schemas"]["watcher"].clone()),
        ("operation" | "projection", "schema") => {
            return Ok(resource()["definition_schemas"][command].clone());
        }
        ("query", _) if a["sql"] == "contract" => {
            return Ok(resource()["definition_schemas"]["query"].clone());
        }
        ("spec", _) => {
            let version = a["format_version"].as_u64().unwrap_or(3).to_string();
            if matches!(operation, "schema" | "docs") {
                let v = &resource()["spec"][&version];
                if v.is_null() {
                    return Err(Error("unsupported manifest version".into()));
                }
                return Ok(if operation == "schema" {
                    v["schema"].clone()
                } else {
                    json!({"markdown":v["markdown"]})
                });
            }
            if operation == "check" {
                for v in 1..=3 {
                    let version = v.to_string();
                    for (suffix, key) in [("schema.json", "schema_text"), ("md", "markdown")] {
                        let name = format!("manifest-v{v}.{suffix}");
                        if catabolic_core::sha256(text(&resource()["spec"][&version], key))
                            != text(&resource()["spec_index"], &name)
                        {
                            return Err(Error(format!("frozen release artifact changed: {name}")));
                        }
                    }
                    let native =
                        catabolic_store::interchange::dispatch("schema", &json!({"version":v}))?;
                    if native != resource()["spec"][&version]["schema"] {
                        return Err(Error(format!(
                            "models differ from frozen v{v}; review a new format version rather than rewriting v{v}"
                        )));
                    }
                }
                return Ok(
                    json!({"compatible":true,"format_version":3,"schema_id":resource()["spec"]["3"]["schema"]["$id"],"generated_artifacts_match":true}),
                );
            }
            if operation == "diff" {
                let other = file(a, "against", 5 * 1024 * 1024)?;
                let mut changes = vec![];
                diff(&resource()["spec"]["3"]["schema"], &other, "", &mut changes);
                return Ok(
                    json!({"complete":changes.is_empty(),"compatibility":if changes.is_empty(){"identical"}else{"review_required"},"changed_paths":changes}),
                );
            }
            if matches!(operation, "validate" | "roundtrip") {
                let result = catabolic_store::interchange::decode(&read_text(
                    text(a, "file"),
                    128 * 1024 * 1024,
                )?)?;
                return Ok(if operation == "roundtrip" {
                    result
                } else {
                    json!({"valid":true,"format":result["format"],"format_version":result["format_version"],"content_sha256":result["content_sha256"],"counts":result["content"]["counts"]})
                });
            }
            return unavailable(command, operation);
        }
        ("consumer", "plex-login" | "plex-login-complete") => {
            set(&mut r, "login_id", "id");
            let op = if operation == "plex-login" {
                "start"
            } else {
                "complete"
            }
            .to_owned();
            return catabolic_runtime::plex_login::execute_cli(&op, &nonnull(&r));
        }
        ("graphql", _) => {
            let choices = usize::from(!a["document"].is_null())
                + usize::from(!a["file"].is_null())
                + usize::from(a["schema"] == true);
            if choices != 1 {
                return Err(Error(
                    "choose exactly one query document, --file, or --schema".into(),
                ));
            }
            if a["schema"] == true {
                if !a["variables"].is_null() || !a["operation_name"].is_null() {
                    return Err(Error(
                        "--schema cannot be combined with query variables or operation name".into(),
                    ));
                }
                return Ok(catabolic_store::graphql::description());
            }
            if text(a, "db").is_empty() {
                return Err(Error("select --db or set CATABOLIC_DB".into()));
            }
            let document = if !a["file"].is_null() {
                read_text(text(a, "file"), 1024 * 1024)?
            } else {
                text(a, "document").into()
            };
            let variables = if a["variables"].is_null()
                || catabolic_core::decode_json(text(a, "variables"))?.is_null()
            {
                // Frozen execute_graphql treats omitted and JSON-null variables
                // alike. Other transports retain their own validated adapters.
                json!({}).into()
            } else {
                catabolic_store::graphql::InputVariables::from_json(text(a, "variables"))?
            };
            let timeout = a["timeout_ms"]
                .as_u64()
                .ok_or_else(|| Error("timeout-ms must be between 1 and 60000".into()))?;
            return catabolic_store::graphql::execute(
                path,
                &document,
                profile,
                variables,
                a["operation_name"].as_str(),
                timeout,
            );
        }
        _ => (),
    }
    if text(a, "db").is_empty() {
        return Err(Error(
            if matches!(command, "consumer" | "notify") {
                "database_required"
            } else if matches!(
                command,
                "processor" | "api" | "watcher" | "supervise" | "catalog-refresh"
            ) {
                "select --db or set CATABOLIC_DB"
            } else {
                "select --db or set CATABOLIC_DB; no database is selected implicitly"
            }
            .into(),
        ));
    }
    match (command,operation){
        ("init",_)=>{if profile!="default"{return Err(Error("init creates the default profile; add additional profiles explicitly".into()));}catabolic_store::migration::initialize(path)}
        ("db","status")=>catabolic_store::migration::inspect(path,false),
        ("db","upgrade")=>catabolic_store::migration::upgrade(path,a["dry_run"]==true,a["backup_dir"].as_str().map(Path::new),&|_,_,_|Ok(())),
        ("inbox",_)=>{catabolic_store::work_inbox::validate_request(a)?;let store=Store::open(path,false,false)?;catabolic_store::work_inbox::execute(store,profile,operation,a).map(|(_,value)|value)},
        ("catalog","retained")=>{let store=Store::open(path,false,false)?;catabolic_store::work_inbox::retained(&store,profile,text(a,"name"),a)},
        ("status",_)=>application(a,"status",a,false),
        ("profile","add")=>{r["profile"]=a["name"].clone();application(a,"add_profile",&r,true)}
        ("profile","list")=>{let store=Store::open(path,false,false)?;Ok(json!({"profiles":catabolic_store::migration::rows(&store.db,"SELECT id FROM profiles ORDER BY id",&[])?}))}
        ("files",_)=>query(a,"files",a),
        ("item","list")=>query(a,"items",a),
        ("item","show")=>{set(&mut r,"item_id","id");query(a,"item",&r)},
        ("tag","list")=>query(a,"tags",a),
        ("tag","assignments")=>query(a,"taggings",a),
        ("association","list")=>query(a,"associations",a),
        ("relationship","list")=>query(a,"relationships",a),
        ("mapping","list")=>{if a["all_catalogs"]==true{r["catalog"]=Value::Null;}query(a,"mappings",&r)}
        ("item","put")=>{
            let mut identities=serde_json::Map::new();for identity in a["identity"].as_array().unwrap(){let (namespace,value)=identity.as_str().unwrap().split_once('=').filter(|(n,v)|!n.is_empty()&&!v.is_empty()).ok_or_else(||Error("identity must be NAMESPACE=VALUE".into()))?;
                if identities.get(namespace).is_some_and(|v|v!=value){return Err(Error("supply only one value per identity namespace".into()));}identities.insert(namespace.into(),json!(value));}
            r["identities"]=Value::Object(identities);r["metadata"]=raw(a,"metadata").map_err(|_|Error("metadata must be valid JSON".into()))?;application(a,"put_item",&r,true)
        }
        ("mapping","put")=>{set(&mut r,"file_id","file");set(&mut r,"item_id","item");application(a,"put_mapping",&r,true)}
        ("mapping","disable")=>application(a,"disable_mapping",&r,true),
        ("association","put")=>{set(&mut r,"file_id","file");set(&mut r,"item_id","item");if !a["metadata"].is_null(){r["metadata"]=raw(a,"metadata")?;}runtime(a,"curation","associate",&r)}
        ("relationship","put")=>{set(&mut r,"source_item_id","source");set(&mut r,"target_item_id","target");if !a["metadata"].is_null(){r["metadata"]=raw(a,"metadata")?;}runtime(a,"curation","relate",&r)}
        ("association"|"relationship","disable")=>runtime(a,"curation",&format!("{command}-disable"),&r),
        ("tag","put"|"rename"|"alias"|"parent")=>runtime(a,"tag",operation,&r),
        ("tag","add"|"remove")=>{r["remove"]=json!(operation=="remove");runtime(a,"tag","assign",&r)}
        ("item","status"|"note"|"worklog"|"require"|"requirements"|"resolve")=>{
            set(&mut r,"item_id","id");set(&mut r,"requirement_id","id");if r["note"].is_null(){r["note"]=json!("");}
            let op=match operation{"status"=>{if a["set_status"].is_null(){if !a["note"].is_null()||!a["expected_revision"].is_null()||a["actor"]!="user"{return Err(Error("status mutation options require --set".into()));}"get"}else{set(&mut r,"status","set_status");"status"}},"note"=>{r["body"]=if a["file"].is_null(){a["text"].clone()}else{json!(read_text(text(a,"file"),65536)?)};"note"},"worklog"=>"log","requirements"=>"checks",_=>operation};
            runtime(a,"workflow",op,&r)
        }
        ("query",_)=>sql(a),
        ("location"|"catalog","list")=>{let store=Store::open(path,false,false)?;catabolic_store::public_reads::bindings(&store,profile,if command=="location"{"source"}else{"output"})},
        ("location"|"catalog","bind")=>{r["kind"]=json!(if command=="location"{"source"}else{"output"});set(&mut r,"owner","name");let mut result=application(a,"bind",&r,true)?;if command=="catalog"{let mode=application(a,"link_mode",&json!({"catalog":a["name"]}),false)?;result["link_mode"]=mode;}Ok(result)}
        ("scan",_)=>{
            if !a["budgets"].is_null(){r["budgets"]=file(a,"budgets",1024*1024)?;}
            set(&mut r,"scopes","scope");set(&mut r,"continue_id","continue_request");
            let path=path.to_owned();let profile=profile.to_owned();
            catabolic_http::interruptible(move|cancel|{
                let check=cancel.clone();
                let result=catabolic_store::embedding::with_interrupt_check(move||check.load(std::sync::atomic::Ordering::Acquire),||{
                    let mut store=Store::open(&path,true,false)?;
                    catabolic_runtime::public_scan::execute(&mut store,&profile,&r)
                });
                if cancel.load(std::sync::atomic::Ordering::Acquire){Err(Error("native legacy CLI interrupted".into()))}else{result}
            })
        }
        ("location","trust")=>{set(&mut r,"source","name");set(&mut r,"policy","identity");r["overrides"]=nonnull(&json!({"uuid":a["uuid"],"device":a["device"],"inode":a["inode"]}));runtime(a,"observe","trust",&r)}
        ("location","scan-policy")=>{set(&mut r,"source","name");if !a["file"].is_null(){r["policy"]=file(a,"file",1024*1024)?;}runtime(a,"observe","scan_policy",&r)}
        ("remount",_)=>{set(&mut r,"trust_sources","trust_source");runtime(a,"observe","remount",&r)}
        ("sync"|"verify"|"recover",_)=>{if a["all_catalogs"]==true{r["catalog"]=Value::Null;}let op=if command=="sync"{if a["dry_run"]==true{"preview"}else{"apply"}}else{command};runtime(a,"outputs",op,&r)}
        ("proposal","put")=>{r["payload"]=file(a,"file",256*1024)?;r["evidence"]=raw(a,"evidence")?;runtime(a,"curation","proposal-put",&r)}
        ("proposal","show"|"list"|"history")=>runtime(a,"curation",if operation=="show"{"proposal-get"}else if operation=="list"{"proposal-list"}else{"proposal-history"},&r),
        ("proposal","accept"|"reject")=>{r["accept"]=json!(operation=="accept");runtime(a,"curation","proposal-decide",&r)}
        ("sidecar",_)=>runtime(a,"curation","sidecars",&r),
        ("expected","put")=>{r["value"]=file(a,"file",256*1024)?;runtime(a,"curation","expected-put",&r)}
        ("expected","report")=>{set(&mut r,"collection","id");runtime(a,"curation","completeness",&r)}
        ("copies","put")=>{r["value"]=file(a,"file",256*1024)?;definitions(a,"copy-put",&r)}
        ("copies","plan")=>{let store=Store::open(path,false,false)?;catabolic_store::public_controls::copies_plan(&store,profile,a)},
        ("copies","show")=>Ok(json!({"catalog":a["catalog"],"definition":definitions(a,"copy-get",&r)?})),
        ("layout","list"|"show")=>definitions(a,if operation=="list"{"layout-list"}else{"layout-get"},&r),
        ("layout","put")=>layout_put(a),
        ("layout","preview"|"apply")=>{set(&mut r,"id","name");runtime(a,"layout",operation,&r)},
        ("component"|"fallback","schema")=>Ok(resource()["extra_schemas"][command].clone()),
        ("component","show")=>{set(&mut r,"id","id");runtime(a,"component","get",&r)}
        ("component","extract")=>{set(&mut r,"operation_id","recipe");runtime(a,"component","extract",&r)}
        ("component","package")=>{set(&mut r,"policy_id","policy");set(&mut r,"item_id","item");r["apply"]=json!(true);runtime(a,"component","package",&r)}
        ("consumer",_)=>consumer(a),
        ("notify",_)=>{set(&mut r,"events","event");if operation=="put"{set(&mut r,"tags","tag");}runtime(a,"notify",operation,&r)}
        ("processor",_)=>processor(a),
        ("artifact",_)=>artifact(a),
        ("rendition",_)=>rendition(a),
        ("catalog-refresh","enable"|"disable")=>{r["enabled"]=json!(operation=="enable");runtime(a,"catalog-refresh","configure",&r)},
        ("catalog-refresh","pending"|"run")=>runtime(a,"catalog-refresh",operation,&r),
        ("rule",_)=>rule(a),
        ("watcher",_)=>watcher(a),
        ("fallback",_)=>fallback(a),
        ("export",_)|("target","import")=>export(a),
        ("operation","show")=>{let store=Store::open(path,false,false)?;catabolic_store::media_mutation::operation_get(&store,text(a,"id"))},
        ("operation","list")=>{let store=Store::open(path,false,false)?;let result=catabolic_store::public_reads::artifact(&store,profile,"recipes",a)?;Ok(json!({"operations":result["recipes"],"next_after":result["next_after"]}))},
        ("operation","put")=>{let definition=file(a,"definition",256*1024)?;if definition["kind"]=="render"{runtime(a,"artifact","recipe",&json!({"name":a["name"],"preset":definition["operation"],"options":definition.get("options").cloned().unwrap_or(json!({})),"output_definition_id":definition["output_definition_id"]}))}else if definition["kind"]=="analysis"{runtime(a,"media","operation-put",&json!({"name":a["name"],"definition":definition}))}else{unavailable("operation","put external definition")}},
        ("projection","mapping-capabilities")=>Ok(resource()["mapping_capabilities"].clone()),
        ("projection",op) if op.starts_with("http-operation-")=>http_operation(a),
        ("projection","put")=>{set(&mut r,"query_id","query");for key in ["copies","renditions"]{if !a[key].is_null(){r[key]=file(a,key,65536)?;}}definitions(a,"projection-put",&r)},
        ("projection","show")=>definitions(a,"projection-get",&r),
        ("projection","enable-auto"|"disable-auto")=>{r["enabled"]=json!(operation=="enable-auto");runtime(a,"catalog-refresh","configure",&r)},
        ("projection","verify"|"recover")=>runtime(a,"outputs",operation,&r),
        ("program",_)=>{if operation=="export"{for (to,from) in [("queries","query"),("rules","rule"),("projections","projection"),("fallback_policies","fallback_policy"),("watchers","watcher")]{set(&mut r,to,from);}}else{r["value"]=file(a,"file",4*1024*1024)?;if !a["bindings"].is_null(){r["bindings"]=file(a,"bindings",65536)?;}}runtime(a,"programs",operation,&r)}
        ("manifest",_)=>manifest(a),
        ("process","list"|"attempts"|"facts")|("content","duplicates"|"search")=>{let store=Store::open(path,false,false)?;catabolic_store::public_reads::processing(&store,profile,operation,a)},
        ("process","enqueue"|"run")=>processing(a),
        ("supervise",_)=>supervise(a),
        ("process","show"|"cancel"|"retry")=>runtime(a,"media",if operation=="show"{"get"}else{operation},&r),
        ("api","serve")=>Err(Error("native HTTP is unqualified; use catabolic-native api serve --experimental-native-http for disposable loopback fixtures".into())),
        _=>unavailable(command,operation),
    }
}
fn sql(a: &Value) -> Result<Value> {
    let statement = a["sql"].as_str();
    let mut r = a.clone();
    let operation = text(a, "sql");
    if ["save", "show", "list", "run"].contains(&operation) {
        if !a["file"].is_null() || a["schema"] == true || !a["params"].is_null() {
            return Err(Error(
                "saved queries use their pinned definition and parameters".into(),
            ));
        }
        if (operation == "list") != a["query_name"].is_null() {
            return Err(Error(
                "save requires a name; show/run require a revision ID".into(),
            ));
        }
        if (operation == "save") != !text(a, "definition").is_empty() {
            return Err(Error("only query save requires --definition FILE".into()));
        }
        set(&mut r, "id", "query_name");
        set(&mut r, "name", "query_name");
        if operation == "save" {
            r["value"] = file(a, "definition", 1024 * 1024)?;
            return definitions(a, "query-put", &r);
        }
        if operation == "run" {
            let store = Store::open(Path::new(text(a, "db")), false, false)?;
            let mut evaluation = catabolic_store::selection::Evaluation::new(
                store,
                text(a, "profile"),
                60000,
                100000,
                false,
            )?;
            let n = a["max_rows"]
                .as_u64()
                .filter(|n| *n <= 10000 && *n >= 1)
                .ok_or_else(|| Error("max-rows must be between 1 and 10000".into()))?;
            return evaluation.run(text(a, "query_name"), n as usize);
        }
        if operation == "list" {
            r["limit"] = a["max_rows"].clone();
        }
        return definitions(
            a,
            if operation == "show" {
                "query-get"
            } else {
                "query-list"
            },
            &r,
        );
    }
    if !a["query_name"].is_null() || !a["definition"].is_null() || !text(a, "after").is_empty() {
        return Err(Error(
            "saved query arguments require save, show, list or run".into(),
        ));
    }
    if usize::from(statement.is_some())
        + usize::from(!a["file"].is_null())
        + usize::from(a["schema"] == true)
        != 1
    {
        return Err(Error(
            "choose exactly one SQL statement, --file PATH, --file -, or --schema".into(),
        ));
    }
    let input = if !a["file"].is_null() {
        Some(read_text(text(a, "file"), 1024 * 1024)?)
    } else {
        statement.map(str::to_owned)
    };
    let max_rows = a["max_rows"]
        .as_u64()
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| Error("max-rows must be between 1 and 10000".into()))?;
    let timeout_ms = a["timeout_ms"]
        .as_u64()
        .ok_or_else(|| Error("timeout-ms must be between 1 and 60000".into()))?;
    catabolic_store::sql::execute(
        Path::new(text(a, "db")),
        input.as_deref(),
        &catabolic_store::sql::Options {
            profile: text(a, "profile"),
            params: a["params"].as_str(),
            describe: a["schema"] == true,
            max_rows,
            timeout_ms,
            stable: false,
            http: false,
        },
    )
}
fn layout_put(a: &Value) -> Result<Value> {
    let mut definition = if !a["preset"].is_null() {
        resource()["presets"][text(a, "preset")].clone()
    } else {
        file(a, "file", 1024 * 1024)?
    };
    if !definition.is_object() {
        return Err(Error("layout definition must be a JSON object".into()));
    }
    if !a["select_sql"].is_null() || !a["select_graphql"].is_null() {
        if a["file"] == "-" && (a["select_sql"] == "-" || a["select_graphql"] == "-") {
            return Err(Error(
                "stdin can supply only one definition or query".into(),
            ));
        }
        let is_sql = !a["select_sql"].is_null();
        if (is_sql && !a["variables"].is_null()) || (!is_sql && !a["params"].is_null()) {
            return Err(Error(
                "use --params for SQL or --variables for GraphQL".into(),
            ));
        }
        let mut selection = json!({"language":if is_sql{"sql"}else{"graphql"},"query":read_text(text(a,if is_sql{"select_sql"}else{"select_graphql"}),1024*1024)?,"profile":a["selection_profile"].as_str().unwrap_or("default")});
        let key = if is_sql { "params" } else { "variables" };
        if !a[key].is_null() {
            selection[key] = raw(a, key)?;
        }
        definition["selection"] = selection;
    } else if ["params", "variables", "selection_profile"]
        .iter()
        .any(|k| !a[k].is_null())
    {
        return Err(Error(
            "selection options require --select-sql or --select-graphql".into(),
        ));
    }
    definitions(
        a,
        "layout-put",
        &json!({"name":a["name"],"value":definition}),
    )
}
fn consumer(a: &Value) -> Result<Value> {
    let op = text(a, "operation");
    let mut r = a.clone();
    if a.get("limit").is_some() && a["limit"].as_u64().is_none_or(|n| !(1..=1000).contains(&n)) {
        return Err(Error("invalid_configuration".into()));
    }
    if op == "metadata" {
        for (to, from) in [
            ("item_ids", "item"),
            ("match_ids", "match"),
            ("fields", "field"),
            ("clear_fields", "clear_field"),
        ] {
            set(&mut r, to, from);
        }
        let path = Path::new(text(a, "db")).to_owned();
        let profile = text(a, "profile").to_owned();
        return catabolic_runtime::plex_metadata::execute_cli(&path, &profile, &nonnull(&r));
    }
    if op == "watch" {
        return unavailable("consumer", op);
    }
    if op == "import" {
        r["mappings"] = file(a, "map", 65536)?;
        let path = Path::new(text(a, "db")).to_owned();
        let profile = text(a, "profile").to_owned();
        return catabolic_http::interruptible(move |cancel| {
            catabolic_runtime::api::execute_plex_import_with_poll(
                &path,
                &profile,
                &nonnull(&r),
                &cancel,
                &mut || {},
            )
        });
    }
    if op == "metadata-match" {
        let path = Path::new(text(a, "db")).to_owned();
        let profile = text(a, "profile").to_owned();
        return catabolic_http::interruptible(move |cancel| {
            catabolic_runtime::api::execute_plex_match_with_poll(
                &path,
                &profile,
                &nonnull(&r),
                &cancel,
                &mut || {},
            )
        });
    }
    if op == "metadata-matches" {
        return runtime(a, "plex-match", "list", &r);
    }
    if op == "connection-put" && !a["credential_file"].is_null() {
        let raw = text(a, "credential_file");
        let expanded = raw
            .strip_prefix("~/")
            .map(|p| Path::new(&std::env::var("HOME").unwrap_or_default()).join(p))
            .unwrap_or_else(|| Path::new(raw).to_owned());
        let absolute = std::path::absolute(expanded)?;
        r["credential_env"] = json!(format!("file:{}", absolute.display()));
    }
    if op == "create" {
        r["spec"] = file(a, "spec", 65536)?;
    }
    let mut result = runtime(a, "consumers", op, &r)?;
    if op == "run" {
        result["notifications"] = runtime(
            a,
            "notify",
            "run",
            &json!({"limit":a["limit"].as_u64().unwrap_or(10).min(100)}),
        )?;
    }
    Ok(result)
}
fn processor(a: &Value) -> Result<Value> {
    let mut r = a.clone();
    let op = text(a, "operation");
    if a.get("name").is_some() {
        set(&mut r, "id", "name");
        set(&mut r, "processor", "name");
    }
    if op == "put" {
        r["definition"] = raw(a, "definition")?;
    }
    if op == "enqueue" {
        set(&mut r, "definition_id", "output_definition");
        r["configuration"] = raw(a, "configuration")?;
    }
    if matches!(op, "heartbeat" | "dispatch" | "complete") {
        let lease = file(a, "lease_file", 256 * 1024)?;
        if !lease["id"].is_string() || !lease["lease_token"].is_string() {
            return Err(Error(
                "lease file must contain id and lease_token from claim".into(),
            ));
        }
        r["id"] = lease["id"].clone();
        r["lease_token"] = lease["lease_token"].clone();
    }
    if op == "complete" {
        r["receipt"] = file(a, "receipt_file", 1024 * 1024)?;
    }
    if op == "dispatch" {
        let mut store = Store::open(Path::new(text(a, "db")), true, false)?;
        return catabolic_runtime::processors::dispatch(
            &mut store,
            text(a, "profile"),
            text(&r, "id"),
            text(&r, "lease_token"),
        );
    }
    runtime(a, "processor", if op == "show" { "get" } else { op }, &r)
}
fn artifact(a: &Value) -> Result<Value> {
    let mut r = a.clone();
    let op = text(a, "operation");
    match op {
        "recipe" => {
            r["options"] = raw(a, "options")?;
            set(&mut r, "output_definition_id", "output_definition");
            set(&mut r, "recipe_name", "name");
        }
        "enqueue" => set(&mut r, "recipe_id", "recipe"),
        _ => (),
    }
    let path = Path::new(text(a, "db")).to_owned();
    let profile = text(a, "profile").to_owned();
    if matches!(op, "enqueue" | "run" | "recover") {
        let operation = op.to_owned();
        return catabolic_http::interruptible(move |cancel| {
            let mut store = Store::open(&path, true, false)?;
            let result = catabolic_runtime::public_artifacts::execute(
                &mut store,
                &profile,
                &operation,
                &nonnull(&r),
                cancel.clone(),
            );
            if cancel.load(std::sync::atomic::Ordering::Acquire) {
                Err(Error("native legacy CLI interrupted".into()))
            } else {
                result
            }
        });
    }
    let store = Store::open(&path, matches!(op, "bind" | "recipe"), false)?;
    catabolic_runtime::public_artifacts::call(
        &store,
        &profile,
        if op == "show" { "get" } else { op },
        &nonnull(&r),
    )
}
fn processing(a: &Value) -> Result<Value> {
    let mut r = a.clone();
    let action = text(a, "operation").to_owned();
    if action == "enqueue" {
        set(&mut r, "operation", "kind");
        if !a["selection"].is_null() {
            if a["file_ids"].as_array().is_some_and(|v| !v.is_empty())
                || a["location"].as_str().is_some_and(|v| !v.is_empty())
                || !text(a, "after").is_empty()
            {
                return Err(Error(
                    "selection cannot be combined with file IDs, location or after".into(),
                ));
            }
            r["selection"] = file(a, "selection", 1024 * 1024)?;
            r["options_json"] = a["options"].clone();
        } else {
            r["options"] = raw(a, "options")?;
        }
    } else {
        r["storage_groups"] = raw(a, "storage_groups")?;
    }
    let path = Path::new(text(a, "db")).to_owned();
    let profile = text(a, "profile").to_owned();
    catabolic_http::interruptible(move |cancel| {
        let mut store = Store::open(&path, true, false)?;
        let check = cancel.clone();
        let result = catabolic_store::embedding::with_interrupt_check(
            move || check.load(std::sync::atomic::Ordering::Acquire),
            || {
                if action == "enqueue" && !r["selection"].is_null() {
                    catabolic_runtime::public_processing::enqueue_selection(
                        store,
                        &profile,
                        &r["selection"],
                        &r,
                        cancel.clone(),
                    )
                } else {
                    catabolic_runtime::public_processing::execute(
                        &mut store,
                        &profile,
                        &action,
                        &nonnull(&r),
                        cancel.clone(),
                    )
                }
            },
        );
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            Err(Error("native legacy CLI interrupted".into()))
        } else {
            result
        }
    })
}
fn supervise(a: &Value) -> Result<Value> {
    let request = a.clone();
    let path = Path::new(text(a, "db")).to_owned();
    let profile = text(a, "profile").to_owned();
    catabolic_http::interruptible(move |cancel| {
        let result = catabolic_runtime::source_events::supervise_with_poll(
            &path,
            &profile,
            &request,
            &cancel,
            &mut || {},
        );
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            Err(Error("native legacy CLI interrupted".into()))
        } else {
            result
        }
    })
}
fn rendition(a: &Value) -> Result<Value> {
    let mut r = a.clone();
    let op = text(a, "operation");
    match op {
        "exclude" | "allow" => {
            let store = Store::open(
                Path::new(text(a, "db")),
                op != "policy" || !a["definition"].is_null(),
                false,
            )?;
            catabolic_store::public_controls::rendition_decide(
                &store,
                text(a, "profile"),
                a,
                op == "exclude",
            )
        }

        "define" => {
            r["definition"] = if a["file"].is_null() {
                raw(a, "definition")?
            } else {
                file(a, "file", 65536)?
            };
            runtime(a, "output", "define", &r)
        }
        "definition" => runtime(a, "output", "definition-get", &r),
        "show" => runtime(a, "output", "get", &r),
        "register" => {
            set(&mut r, "definition_id", "output_definition");
            if !a["metadata"].is_null() {
                r["metadata"] = raw(a, "metadata")?;
            }
            runtime(a, "output", "register", &r)
        }
        "list" | "definitions" => {
            let store = Store::open(Path::new(text(a, "db")), false, false)?;
            catabolic_store::public_reads::output(
                &store,
                text(a, "profile"),
                op == "definitions",
                a,
            )
        }
        "receipt-source" => runtime(a, "receipt", "source", &r),
        "import-receipt" => {
            let store = Store::open(
                Path::new(text(a, "db")),
                op != "policy" || !a["definition"].is_null(),
                false,
            )?;
            let receipt = file(a, "file", 1024 * 1024)?;
            catabolic_runtime::receipts::import_receipt(&store, text(a, "profile"), &receipt)
        }
        "policy" => {
            let store = Store::open(
                Path::new(text(a, "db")),
                op != "policy" || !a["definition"].is_null(),
                false,
            )?;
            catabolic_store::mutation::profile(&store, text(a, "profile"))?;
            if a["definition"].is_null() {
                catabolic_store::definitions::dispatch(
                    &store,
                    text(a, "profile"),
                    "rendition-get",
                    &r,
                )
            } else {
                r["value"] = raw(a, "definition")?;
                catabolic_store::definitions::dispatch(
                    &store,
                    text(a, "profile"),
                    "rendition-put",
                    &r,
                )
            }
        }
        _ => unavailable("rendition", op),
    }
}
fn manifest(a: &Value) -> Result<Value> {
    let store = Store::open(Path::new(text(a, "db")), false, false)?;
    let mut evaluation = catabolic_store::selection::Evaluation::new(
        store,
        text(a, "profile"),
        60000,
        100000,
        false,
    )?;
    let extra = raw(a, "extra")?;
    let document =
        catabolic_store::manifest::build(&mut evaluation, text(a, "catalog"), Some(&extra), None)?;
    if a["in_catalog"] == true || a["output"].as_str().is_some_and(|v| v != "-") {
        let mut r = a.clone();
        r["document"] = document;
        runtime(a, "manifest", "write", &r)
    } else if a["replace"] == true {
        Err(Error("--replace requires --output or --in-catalog".into()))
    } else {
        Ok(document)
    }
}
fn rule(a: &Value) -> Result<Value> {
    let mut r = a.clone();
    let op = text(a, "operation");
    match op {
        "put" => {
            set(&mut r, "recipe_id", "recipe");
            r["selection"] = if a["query"].is_null() {
                file(a, "selection", 256 * 1024)?
            } else {
                json!({"query_id":a["query"]})
            };
            r["estimates"] = if a["estimate_video_kbps"].is_null() {
                json!({})
            } else {
                json!({"video_kbps":a["estimate_video_kbps"]})
            };
            definitions(a, "rule-put", &r)
        }
        "list" | "show" => definitions(
            a,
            if op == "show" {
                "rule-get"
            } else {
                "rule-list"
            },
            &r,
        ),
        "enable" | "disable" => {
            r["enabled"] = json!(op == "enable");
            definitions(a, "rule-enable", &r)
        }
        "stats" => runtime(a, "rules", "statistics", &r),
        "preview" | "apply" | "run" => {
            if !a["worker"].is_null() {
                return unavailable("rule", "run --worker");
            }
            runtime(a, "rules", op, &r)
        }
        _ => unavailable("rule", op),
    }
}
fn watcher(a: &Value) -> Result<Value> {
    let mut r = a.clone();
    let op = text(a, "operation");
    if op == "put" {
        r["definition"] = file(a, "file", 1024 * 1024)?;
    }
    match op {
        "list" | "pending" => Ok(json!({"watchers":runtime(a,"watcher","list",&r)?})),
        "history" => Ok(json!({"runs":runtime(a,"watcher","history",&r)?})),
        "preview" => {
            let row = runtime(a, "watcher", "get", &r)?;
            let request = json!({"reference":row["definition"]["plan"]});
            Ok(
                json!({"watcher":row,"plan":runtime(a,"plans","describe",&request)?,"observations":runtime(a,"plans","requirements",&request)?}),
            )
        }
        "put" | "show" | "enable" | "disable" | "run" => {
            runtime(a, "watcher", if op == "show" { "get" } else { op }, &r)
        }
        _ => unavailable("watcher", op),
    }
}
fn fallback(a: &Value) -> Result<Value> {
    let mut r = a.clone();
    let op = text(a, "operation");
    match op {
        "resolve" => {
            r["policy_id"] = a["policy"].clone();
            r["entries"] = json!([{"item_id":a["item"],"role":a["role"],"part":a["part"],"variant":a["variant"]}]);
            runtime(a, "fallback", "resolve", &r)
                .map(|v| catabolic_store::fallback_projection::present(&v))
        }
        "enable" | "disable" | "history" => {
            let store = Store::open(Path::new(text(a, "db")), true, false)?;
            catabolic_store::public_controls::fallback(&store, text(a, "profile"), op, &nonnull(&r))
        }

        "put" => {
            r["definition"] = file(a, "definition", 65536)?;
            runtime(a, "fallback-policy", "put", &r)
        }
        "show" | "list" => runtime(
            a,
            "fallback-policy",
            if op == "show" { "get" } else { "list" },
            &r,
        ),
        "bind" => {
            set(&mut r, "policy_id", "policy");
            set(&mut r, "query_id", "query");
            runtime(a, "fallback", "bind", &r)
        }
        "preview" | "execute" => {
            set(&mut r, "manual_failback", "failback");
            r["apply"] = json!(op == "execute");
            runtime(a, "fallback", "run", &r)
        }
        _ => unavailable("fallback", op),
    }
}
fn export(a: &Value) -> Result<Value> {
    let store = Store::open(Path::new(text(a, "db")), false, false)?;
    let mut evaluation = catabolic_store::selection::Evaluation::new(
        store,
        text(a, "profile"),
        60000,
        100000,
        false,
    )?;
    let document =
        catabolic_store::manifest::build(&mut evaluation, text(a, "catalog"), None, None)?;
    if a["command"] == "target" {
        let mut r = a.clone();
        r["document"] = document;
        set(&mut r, "target", "name");
        let path = Path::new(text(a, "db")).to_owned();
        let profile = text(a, "profile").to_owned();
        return catabolic_http::interruptible(move |cancel| {
            catabolic_runtime::api::execute_import_with_poll(
                &path,
                &profile,
                &nonnull(&r),
                &cancel,
                &mut || {},
            )
        });
    }
    let result =
        catabolic_store::exports::build(&document, text(a, "format"), a["base_url"].as_str())?;
    if !a["output"].is_null() {
        return runtime(
            a,
            "export",
            "publish",
            &json!({"document":document,"export":result,"output":a["output"]}),
        );
    }
    if a["raw"] == true {
        if a["format"] == "nfo" {
            return Err(Error(
                "NFO exports can contain multiple files; omit --raw or use --output".into(),
            ));
        }
        return Ok(json!({"raw":result["files"][0]["content"]}));
    }
    Ok(result)
}

fn http_operation(a: &Value) -> Result<Value> {
    let action = text(a, "operation")
        .strip_prefix("http-operation-")
        .unwrap();
    let mut r = a.clone();
    if action == "list" || action == "show" {
        let store = Store::open(Path::new(text(a, "db")), false, false)?;
        return catabolic_store::public_controls::http_operation(
            &store,
            text(a, "profile"),
            if action == "show" { "get" } else { "list" },
            &r,
        );
    }
    if action == "put" {
        r["document"] = file(a, "spec", 1024 * 1024)?;
        set(&mut r, "environment", "credential_env");
    }
    let result = runtime(a, "http-operations", action, &r)?;
    if action == "retry" {
        Ok(json!({"id":a["id"],"retry_scheduled":true}))
    } else {
        Ok(result)
    }
}
