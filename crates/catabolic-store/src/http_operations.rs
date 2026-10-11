// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Approved, pinned OpenAPI 3.1 operations. References resolve locally before
//! compilation; schema validation cannot access the filesystem or network.
use crate::{Error, Result, Store, notifications};
use catabolic_core::{encode, sha256};
use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

fn fail<T>(code: &str) -> Result<T> {
    Err(Error(format!("http_operation_{code}")))
}
fn decode(raw: &str) -> Result<Value> {
    catabolic_core::decode_json(raw).map_err(|e| Error(e.to_string()))
}
fn text(value: &str, maximum: usize) -> Result<()> {
    if value.is_empty() || value.chars().count() > maximum || value.chars().any(|c| c < ' ') {
        return Err(Error("invalid_configuration".into()));
    }
    Ok(())
}
pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
        Value::Number(v) => v.as_f64() != Some(0.0),
    }
}
struct NoRetrieval;
impl jsonschema::Retrieve for NoRetrieval {
    fn retrieve(
        &self,
        _uri: &jsonschema::Uri<String>,
    ) -> std::result::Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        Err("external schema retrieval is disabled".into())
    }
}
pub fn validator(schema: &Value) -> Result<jsonschema::Validator> {
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .should_validate_formats(false)
        .with_retriever(NoRetrieval)
        .build(schema)
        .map_err(|_| Error("http_operation_invalid_schema".into()))
}
pub fn resolve(document: &Value, value: &Value) -> Result<Value> {
    fn walk(
        document: &Value,
        value: &Value,
        trail: &mut Vec<String>,
        budget: &mut usize,
        depth: usize,
    ) -> Result<Value> {
        if *budget == 0 || depth > 32 {
            return fail("schema_budget");
        }
        *budget -= 1;
        if let Some(values) = value.as_array() {
            return Ok(Value::Array(
                values
                    .iter()
                    .map(|v| walk(document, v, trail, budget, depth + 1))
                    .collect::<Result<_>>()?,
            ));
        }
        let Some(object) = value.as_object() else {
            return Ok(value.clone());
        };
        if ["$dynamicRef", "$recursiveRef", "$id"]
            .iter()
            .any(|key| object.contains_key(*key))
        {
            return fail("unsupported_reference");
        }
        if object.contains_key("pattern") || object.contains_key("patternProperties") {
            return fail("unsupported_regex_schema");
        }
        if let Some(dialect) = object.get("$schema")
            && ![
                "https://json-schema.org/draft/2020-12/schema",
                "https://spec.openapis.org/oas/3.1/dialect/base",
            ]
            .iter()
            .any(|s| dialect == *s)
        {
            return fail("unsupported_schema_dialect");
        }
        if let Some(reference) = object.get("$ref") {
            let Some(reference) = reference.as_str().filter(|s| s.starts_with("#/")) else {
                return fail("external_or_recursive_reference");
            };
            if trail.iter().any(|r| r == reference) {
                return fail("external_or_recursive_reference");
            }
            if object
                .keys()
                .any(|k| !["$ref", "description", "summary"].contains(&k.as_str()))
            {
                return fail("reference_siblings");
            }
            let mut target = document;
            for part in reference[2..].split('/') {
                target = target
                    .get(part.replace("~1", "/").replace("~0", "~"))
                    .ok_or_else(|| Error("http_operation_unknown_reference".into()))?;
            }
            trail.push(reference.into());
            let result = walk(document, target, trail, budget, depth + 1);
            trail.pop();
            return result;
        }
        let mut result = Map::new();
        for (key, value) in object {
            result.insert(
                key.clone(),
                walk(document, value, trail, budget, depth + 1)?,
            );
        }
        Ok(Value::Object(result))
    }
    walk(document, value, &mut Vec::new(), &mut 4096, 0)
}
fn name(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
}
fn placeholders(path: &str) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    let mut rest = path;
    while let Some((before, after)) = rest.split_once('{') {
        if before.contains('}') {
            return fail("path_parameters_mismatch");
        }
        let Some((name, tail)) = after.split_once('}') else {
            return fail("path_parameters_mismatch");
        };
        if name.is_empty() || name.contains('{') {
            return fail("path_parameters_mismatch");
        }
        names.insert(name.into());
        rest = tail;
    }
    if rest.contains('}') {
        return fail("path_parameters_mismatch");
    }
    Ok(names)
}
pub fn compile(
    document: &Value,
    operation_id: &str,
    base_url: &str,
    environment: Option<&str>,
) -> Result<Value> {
    if !document.is_object()
        || !document["openapi"]
            .as_str()
            .is_some_and(|v| v.starts_with("3.1."))
    {
        return fail("requires_openapi_3_1");
    }
    if let Some(dialect) = document.get("jsonSchemaDialect")
        && ![
            "https://spec.openapis.org/oas/3.1/dialect/base",
            "https://json-schema.org/draft/2020-12/schema",
        ]
        .iter()
        .any(|s| dialect == *s)
    {
        return fail("unsupported_schema_dialect");
    }
    if encode(document).len() > 1024 * 1024 {
        return fail("spec_too_large");
    }
    text(operation_id, 255)?;
    if notifications::callback_origin(base_url).is_err() || base_url.contains(['?', '#', '{', '}'])
    {
        return fail("invalid_base_url");
    }
    let mut found = Vec::new();
    if let Some(paths) = document["paths"].as_object() {
        for (path, item) in paths {
            if let Some(methods) = item.as_object() {
                for (method, operation) in methods {
                    if operation.is_object() && operation["operationId"] == operation_id {
                        found.push((path, method, item, operation));
                    }
                }
            }
        }
    }
    if found.len() != 1 {
        return fail("operation_not_unique");
    }
    let (path, method, item, raw) = found[0];
    if !["post", "put", "patch"].contains(&method.as_str())
        || !path.starts_with('/')
        || path.starts_with("//")
        || path.contains(['?', '#', '\\'])
        || path.chars().any(|c| c <= ' ')
    {
        return fail("unsupported_method_or_path");
    }
    let parameters_default = json!([]);
    let body_default = json!({});
    let path_parameters = resolve(
        document,
        item.get("parameters").unwrap_or(&parameters_default),
    )?;
    let operation_parameters = resolve(
        document,
        raw.get("parameters").unwrap_or(&parameters_default),
    )?;
    let mut parameters = Map::new();
    for parameter in path_parameters
        .as_array()
        .ok_or_else(|| Error("http_operation_invalid_specification".into()))?
        .iter()
        .chain(
            operation_parameters
                .as_array()
                .ok_or_else(|| Error("http_operation_invalid_specification".into()))?,
        )
    {
        let location = parameter["in"].as_str().unwrap_or("");
        let key = parameter["name"].as_str().unwrap_or("");
        if !["path", "query"].contains(&location) || !name(key) {
            return fail("unsupported_parameter");
        }
        let schema = &parameter["schema"];
        if !schema.is_object()
            || !schema["type"]
                .as_str()
                .is_some_and(|t| ["string", "integer", "number", "boolean"].contains(&t))
        {
            return fail("requires_scalar_parameter");
        }
        let style = if location == "path" { "simple" } else { "form" };
        if parameter.get("style").is_some_and(|v| v != style) || truthy(&parameter["allowReserved"])
        {
            return fail("unsupported_parameter_encoding");
        }
        validator(schema)?;
        parameters.insert(
            format!("{location}:{key}"),
            json!({"schema":schema,"required":if location=="path" {json!(true)} else {parameter.get("required").cloned().unwrap_or(json!(false))}}),
        );
    }
    if placeholders(path)?
        != parameters
            .keys()
            .filter_map(|k| k.strip_prefix("path:").map(str::to_owned))
            .collect()
    {
        return fail("path_parameters_mismatch");
    }
    let request = resolve(document, raw.get("requestBody").unwrap_or(&body_default))?;
    if !request.is_object() {
        return fail("invalid_specification");
    }
    let has_request = request.as_object().is_some_and(|r| !r.is_empty());
    let schema = if has_request {
        let Some(content) = request["content"].get("application/json") else {
            return fail("requires_json_body");
        };
        let schema = content.get("schema").unwrap_or(&body_default);
        validator(schema)?;
        schema.clone()
    } else {
        Value::Null
    };
    let mut auth = Value::Null;
    let empty_security = json!([]);
    let security = raw
        .get("security")
        .or_else(|| document.get("security"))
        .unwrap_or(&empty_security);
    if truthy(security) {
        let Some(security) = security.as_array().filter(|s| s.len() == 1) else {
            return fail("requires_single_security_scheme");
        };
        let Some(requirements) = security[0].as_object().filter(|s| s.len() == 1) else {
            return fail("requires_single_security_scheme");
        };
        let (name, scopes) = requirements.iter().next().unwrap();
        if truthy(scopes) {
            return fail("unsupported_security_scopes");
        }
        let scheme = resolve(document, &document["components"]["securitySchemes"][name])?;
        if scheme["type"] == "http"
            && scheme["scheme"]
                .as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case("bearer"))
        {
            auth = json!({"header":"Authorization","prefix":"Bearer "});
        } else if scheme["type"] == "apiKey" && scheme["in"] == "header" {
            let header = scheme["name"].as_str().unwrap_or("");
            if header.is_empty()
                || header.len() > 100
                || !header.as_bytes()[0].is_ascii_alphabetic()
                || !header
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
                || [
                    "host",
                    "content-length",
                    "content-type",
                    "transfer-encoding",
                    "connection",
                    "idempotency-key",
                    "user-agent",
                ]
                .contains(&header.to_ascii_lowercase().as_str())
            {
                return fail("unsafe_auth_header");
            }
            auth = json!({"header":header,"prefix":""});
        } else {
            return fail("unsupported_security_scheme");
        }
        let environment = environment.ok_or_else(|| Error("invalid_configuration".into()))?;
        notifications::credential_reference(environment)?;
        auth["environment"] = json!(environment);
    } else if environment.is_some() {
        return fail("credential_without_security_scheme");
    }
    let mut responses = Map::new();
    if let Some(declarations) = raw["responses"].as_object() {
        for (status, response) in declarations {
            if status != "default"
                && status != "2XX"
                && !(status.len() == 3
                    && status.starts_with('2')
                    && status.bytes().all(|c| c.is_ascii_digit()))
            {
                continue;
            }
            let response = resolve(document, response)?;
            let content = response.get("content").unwrap_or(&body_default);
            if truthy(content) {
                let Some(content) = content.as_object() else {
                    return fail("requires_json_response");
                };
                if content.len() != 1 || !content.contains_key("application/json") {
                    return fail("requires_json_response");
                }
                let schema = content["application/json"]
                    .get("schema")
                    .unwrap_or(&body_default);
                validator(schema)?;
                responses.insert(status.clone(), json!({"schema":schema,"has_body":true}));
            } else {
                responses.insert(status.clone(), json!({"has_body":false}));
            }
        }
    }
    if responses.is_empty() {
        return fail("requires_success_response");
    }
    if encode(&json!(responses)).len() > 65536 {
        return fail("response_schema_budget");
    }
    Ok(
        json!({"operation_id":operation_id,"spec_digest":sha256(encode(document)),"base_url":base_url.trim_end_matches('/'),"method":method.to_uppercase(),"path":path,"parameters":parameters,"body_schema":schema,"body_required":request.get("required").cloned().unwrap_or(json!(false)),"auth":auth,"responses":responses}),
    )
}
pub fn get(store: &Store, profile: &str, id: &str) -> Result<Value> {
    let row=store.db.query_row("SELECT name,revision,definition,digest,enabled,principal,token_id FROM http_operations WHERE profile=? AND id=?",params![profile,id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,bool>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,Option<String>>(6)?))).optional()?.ok_or_else(||Error("not_found".into()))?;
    Ok(
        json!({"id":id,"profile":profile,"name":row.0,"revision":row.1,"definition":decode(&row.2)?,"digest":row.3,"enabled":row.4,"principal":row.5,"token_id":row.6}),
    )
}
pub fn present(operation: &Value) -> Value {
    json!({"id":operation["id"],"name":operation["name"],"revision":operation["revision"],"enabled":operation["enabled"],"operation_id":operation["definition"]["operation_id"],"spec_digest":operation["definition"]["spec_digest"],"method":operation["definition"]["method"]})
}

/// Revalidate both the operation approver and the enqueueing token in the same
/// snapshot. Local CLI approvals deliberately have no principal/token binding.
pub fn authorize(
    mut store: Store,
    profile: &str,
    operation: &Value,
    token: Option<&str>,
) -> Result<Store> {
    if operation["enabled"] != true {
        return Err(Error("invalid_destination".into()));
    }
    let mut tokens = BTreeSet::new();
    if let Some(id) = operation["token_id"].as_str() {
        tokens.insert(id);
    }
    if let Some(id) = token {
        tokens.insert(id);
    }
    for id in tokens {
        let access = crate::access::Access::from_stored_token(store, id)?;
        if access.profile() != profile {
            return Err(Error("unauthorized".into()));
        }
        access.require_operator("webhooks:manage")?;
        access.require_operator("sql:read")?;
        store = access.into_store();
    }
    Ok(store)
}
pub fn put(
    store: &Store,
    profile: &str,
    name: &str,
    document: &Value,
    operation_id: &str,
    base_url: &str,
    environment: Option<&str>,
) -> Result<Value> {
    text(name, 255)?;
    let definition = compile(document, operation_id, base_url, environment)?;
    let digest = sha256(encode(&definition));
    if let Some(id) = store
        .db
        .query_row(
            "SELECT id FROM http_operations WHERE profile=? AND name=? AND digest=?",
            params![profile, name, digest],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        return Ok(present(&get(store, profile, &id)?));
    }
    let id = uuid::Uuid::new_v4().to_string();
    store.transaction(|db|{
        let version:i64=db.query_row("SELECT coalesce(max(revision),0)+1 FROM http_operations WHERE profile=? AND name=?",params![profile,name],|r|r.get(0))?;
        db.execute("INSERT INTO http_operations(id,profile,name,revision,definition,digest,enabled) VALUES (?,?,?,?,?,?,1)",params![id,profile,name,version,encode(&definition),digest])?;
        db.execute("INSERT INTO notification_destinations(profile,id,credential_env,subscriptions,tags,min_severity) VALUES (?,?,'CATABOLIC_HTTP_OPERATION','[]','[]','info')",params![profile,format!("operation:{id}")])?;Ok(())
    })?;
    Ok(present(&get(store, profile, &id)?))
}
pub fn execute(
    store: &mut Store,
    profile: &str,
    operation: &str,
    request: &Value,
) -> Result<Value> {
    let field = |key| {
        request[key]
            .as_str()
            .ok_or_else(|| Error("invalid_configuration".into()))
    };
    match operation {
        "compile" => compile(
            &request["document"],
            field("operation_id")?,
            field("base_url")?,
            request["environment"].as_str(),
        ),
        "put" => put(
            store,
            profile,
            field("name")?,
            &request["document"],
            field("operation_id")?,
            field("base_url")?,
            request["environment"].as_str(),
        ),
        "get" => Ok(present(&get(store, profile, field("id")?)?)),
        "disable" | "retry" => {
            let id = field("id")?;
            let current = get(store, profile, id)?;
            if operation == "retry" && current["enabled"] != true {
                return fail("disabled");
            }
            store.transaction(|db|{
                notifications::control(store,profile,&format!("operation:{id}"),operation,notifications::now())?;
                if operation=="disable"{db.execute("UPDATE http_operations SET enabled=0 WHERE id=?",[id])?;}
                else{db.execute("UPDATE http_operations SET token_id=NULL WHERE id=?",[id])?;db.execute("UPDATE http_mapping_deliveries SET token_id=NULL WHERE operation_id=? AND event_id IN (SELECT event_id FROM notification_deliveries WHERE profile=? AND state NOT IN ('complete','cancelled'))",params![id,profile])?;}
                Ok(())
            })?;
            Ok(if operation == "disable" {
                json!({"id":id,"enabled":false})
            } else {
                json!({"id":id,"operation":"retry","complete":true})
            })
        }
        _ => Err(Error("unknown_http_operation".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validator_never_retrieves_http_or_file_resources() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("schema.json");
        std::fs::write(&file, r#"{"type":"string"}"#).unwrap();
        assert!(validator(&json!({"$ref":format!("file://{}",file.display())})).is_err());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        assert!(
            validator(&json!({"$ref":format!("http://{}/schema",listener.local_addr().unwrap())}))
                .is_err()
        );
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
    #[test]
    fn local_resolution_cycles_and_exponential_expansion_are_bounded() {
        let cycle = json!({"defs":{"node":{"$ref":"#/defs/node"}}});
        assert_eq!(
            resolve(&cycle, &json!({"$ref":"#/defs/node"}))
                .unwrap_err()
                .0,
            "http_operation_external_or_recursive_reference"
        );
        let mut document = json!({"defs":{"0":{"type":"string"}}});
        for i in 1..=14 {
            document["defs"][i.to_string()] = json!({"anyOf":[{"$ref":format!("#/defs/{}",i-1)},{"$ref":format!("#/defs/{}",i-1)}]});
        }
        assert_eq!(
            resolve(&document, &json!({"$ref":"#/defs/14"}))
                .unwrap_err()
                .0,
            "http_operation_schema_budget"
        );
        let schema = json!({"type":"string","format":"email"});
        assert!(validator(&schema).unwrap().is_valid(&json!("not-an-email")));
    }
}
