//! GraphQL transport over the same native snapshot and catalog query functions.
pub use crate::graphql_input::InputVariables;
use crate::graphql_parse::parse_query;
use crate::operation_budget::{BudgetCharge, OperationBudget};
use crate::{
    Error, Result, Store,
    api_catalog::Authority,
    migration::{SCHEMA_VERSION, reference, rows},
    query::{CatalogQuery, decode, rendition_conditions, view_sql_for},
    sql::scalar,
};
use async_graphql::{ErrorExtensions, Request, Variables, dynamic::*};
use async_graphql_parser::{
    parse_schema,
    types::{BaseType, Selection, SelectionSet, Type, TypeKind, TypeSystemDefinition},
};
use catabolic_core::encode;
use rusqlite::types::Value as SqlValue;
use serde_json::{Value, json};
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

#[cfg(test)]
thread_local! {
    static PANIC_AFTER_SETUP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
const SDL: &str = include_str!("../resources/schema.graphql");
struct Budget;
impl async_graphql::extensions::ExtensionFactory for Budget {
    fn create(&self) -> Arc<dyn async_graphql::extensions::Extension> {
        Arc::new(Budget)
    }
}
#[async_trait::async_trait]
impl async_graphql::extensions::Extension for Budget {
    async fn validation(
        &self,
        ctx: &async_graphql::extensions::ExtensionContext<'_>,
        next: async_graphql::extensions::NextValidation<'_>,
    ) -> std::result::Result<async_graphql::ValidationResult, Vec<async_graphql::ServerError>> {
        match next.run(ctx).await {
            Err(mut errors) => {
                let state = ctx
                    .data_unchecked::<Arc<Mutex<Context>>>()
                    .lock()
                    .expect("query context");
                errors.retain(|error| !state.runtime_inputs.defers(error));
                if errors.is_empty() {
                    // Strict validation already enforced its complexity/depth
                    // limits. Only runtime-null errors at identified argument
                    // positions move to execution; no static rule is disabled.
                    Ok(async_graphql::ValidationResult {
                        cache_control: Default::default(),
                        complexity: 0,
                        depth: 0,
                    })
                } else {
                    Err(errors)
                }
            }
            result => result,
        }
    }
    async fn execute(
        &self,
        ctx: &async_graphql::extensions::ExtensionContext<'_>,
        operation: Option<&str>,
        _next: async_graphql::extensions::NextExecute<'_>,
    ) -> async_graphql::Response {
        // The original document has passed strict framework validation here.
        // Completion is native: async-graphql's dynamic container would invoke
        // merged fields separately and does not propagate its non-null chain.
        let state = ctx.data_unchecked::<Arc<Mutex<Context>>>();
        let (inputs, http) = {
            let context = state.lock().expect("query context");
            (context.runtime_inputs.clone(), context.authority.is_some())
        };
        let mut execution = Execution {
            state,
            inputs: &inputs,
            http,
            errors: vec![],
        };
        let data = match execution.selection(
            &Value::Null,
            "Query",
            inputs.operation(operation),
            &mut vec![],
        ) {
            Ok(value) => value,
            Err(error) => {
                execution.errors.push(error);
                Value::Null
            }
        };
        execution.errors.sort_by(|a, b| {
            a.locations
                .cmp(&b.locations)
                .then_with(|| path_order(&a.path, &b.path))
                .then_with(|| a.message.cmp(&b.message))
        });
        let mut response = async_graphql::Response::new(completed_value(data));
        response.errors = execution.errors;
        response
    }
}

// Both value types own the same serde_json::Number. The generic deserializer
// rejects i128/u128 and interprets larger arbitrary-precision numbers as private
// maps, so transfer variants directly instead. JSON scalar contents stay opaque,
// including ordinary object keys which resemble serde's private representation.
fn completed_value(value: Value) -> async_graphql::Value {
    use async_graphql::Value as Graphql;
    match value {
        Value::Null => Graphql::Null,
        Value::Bool(value) => Graphql::Boolean(value),
        Value::Number(value) => Graphql::Number(value),
        Value::String(value) => Graphql::String(value),
        Value::Array(values) => Graphql::List(values.into_iter().map(completed_value).collect()),
        Value::Object(values) => Graphql::Object(
            values
                .into_iter()
                .map(|(key, value)| (async_graphql::Name::new(key), completed_value(value)))
                .collect(),
        ),
    }
}

/// Exact bounded-execution evidence, kept out of frozen GraphQL responses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExecutionFailure {
    WorkBudget,
    ByteBudget,
    Timeout,
    Cancelled,
}
fn operation_failure(failure: crate::selection::EvaluationFailure) -> ExecutionFailure {
    use crate::selection::EvaluationFailure as E;
    match failure {
        E::ByteBudget => ExecutionFailure::ByteBudget,
        E::Timeout => ExecutionFailure::Timeout,
        E::Cancelled => ExecutionFailure::Cancelled,
        _ => ExecutionFailure::WorkBudget,
    }
}
fn operation_charge(
    charge: BudgetCharge,
    evidence: &Arc<Mutex<Option<ExecutionFailure>>>,
) -> Result<()> {
    if let Some(budget) = OperationBudget::current() {
        budget.charge_without_poll(charge).map_err(|cause| {
            record_failure(evidence, operation_failure(cause));
            Error("operation budget exhausted".into())
        })?;
    }
    Ok(())
}
fn record_failure(evidence: &Arc<Mutex<Option<ExecutionFailure>>>, failure: ExecutionFailure) {
    if let Some(budget) = OperationBudget::current() {
        use crate::selection::EvaluationFailure as E;
        budget.record_failure(match failure {
            ExecutionFailure::ByteBudget => E::ByteBudget,
            ExecutionFailure::Timeout => E::Timeout,
            ExecutionFailure::Cancelled => E::Cancelled,
            ExecutionFailure::WorkBudget => E::WorkBudget,
        });
    }
    evidence
        .lock()
        .expect("GraphQL failure lock")
        .get_or_insert(failure);
}

struct Context {
    failure: Arc<Mutex<Option<ExecutionFailure>>>,
    store: Store,
    profile: String,
    deadline: Instant,
    nodes: usize,
    fields: usize,
    bytes: usize,
    aborted: Option<String>,
    authority: Option<Authority>,
    runtime_inputs: Arc<crate::graphql_runtime_input::RuntimeInputs>,
    executed: bool,
}
impl Context {
    fn authorized_resolve(
        &mut self,
        parent: &str,
        field: &str,
        source: &Value,
        args: &Value,
    ) -> Result<Value> {
        let Some(authority) = self.authority.as_ref() else {
            return self.resolve(parent, field, source, args);
        };
        if !authority.operator("metadata:read") {
            let allowed: Option<&[&str]> = match parent {
                "Query" => Some(&[
                    "items",
                    "item",
                    "files",
                    "file",
                    "associations",
                    "relationships",
                    "mappings",
                    "catalogs",
                    "profile",
                    "schemaVersion",
                    "mediaTypes",
                    "fallbackPolicy",
                    "components",
                    "componentOccurrences",
                    "fallbackResolutions",
                ]),
                "Component" => Some(&["id", "kind", "itemIds"]),
                "ComponentOccurrence" => Some(&[
                    "id",
                    "componentId",
                    "itemId",
                    "fileId",
                    "kind",
                    "role",
                    "language",
                    "title",
                    "codec",
                    "defaultFlag",
                    "forced",
                    "commentary",
                    "hearingImpaired",
                    "visualImpaired",
                    "storage",
                    "locator",
                    "revision",
                    "current",
                    "technicallyVerified",
                    "conflicts",
                ]),
                "Item" => Some(&[
                    "id",
                    "kind",
                    "title",
                    "year",
                    "metadata",
                    "associations",
                    "relationships",
                    "mappings",
                ]),
                "File" => Some(&[
                    "id",
                    "size",
                    "mtimeNs",
                    "status",
                    "associations",
                    "mappings",
                ]),
                "Association" => Some(&[
                    "id", "fileId", "itemId", "role", "part", "active", "item", "file",
                ]),
                "Relationship" => Some(&[
                    "id", "sourceId", "targetId", "kind", "position", "active", "source", "target",
                ]),
                "Mapping" => Some(&[
                    "id", "catalog", "path", "active", "itemId", "fileId", "item", "file",
                ]),
                "Catalog" => Some(&["id", "mappings"]),
                _ => None,
            };
            if allowed.is_some_and(|allowed| !allowed.contains(&field)) {
                return Err(Error("field is not authorized".into()));
            }
            if [
                "hasRendition",
                "missingRendition",
                "curationStatus",
                "tags",
                "anyTags",
                "notTags",
                "descendants",
            ]
            .iter()
            .any(|key| {
                let value = &args[*key];
                !value.is_null() && value != false && !value.as_array().is_some_and(Vec::is_empty)
            }) {
                return Err(Error("filter is not authorized".into()));
            }
        }
        if parent == "Query"
            && field == "fallbackPolicy"
            && !authority.permits_fallback_policy(args["id"].as_str().unwrap_or(""))
        {
            return Err(Error("resource is not authorized".into()));
        }
        let scope = json!([parent, field, source.get("id").unwrap_or(&Value::Null)]);
        let mut args = args.clone();
        if !args["after"].is_null() && args["after"] != "" && args["after"] != 0 {
            let cursor = args["after"]
                .as_str()
                .ok_or_else(|| Error("invalid_cursor".into()))?;
            args["after"] = json!(authority.after(&scope, Some(cursor))?);
        }
        if parent == "Query" && field == "files" && args["unmapped"] != true {
            args["catalog"] = Value::Null;
        }
        let mut result = self.resolve(parent, field, source, &args)?;
        if let Some(cursor) = result["pageInfo"]["endCursor"]
            .as_str()
            .filter(|v| !v.is_empty())
        {
            let signed = self
                .authority
                .as_ref()
                .expect("authorized context")
                .cursor(&scope, cursor)?;
            result["pageInfo"]["endCursor"] = json!(signed);
        }
        Ok(result)
    }
    fn one(
        &self,
        table: &str,
        column: &str,
        id: &Value,
        scoped: bool,
        error: &str,
        keys: &[&str],
    ) -> Result<Value> {
        let mut params = vec![scalar(id)?];
        if scoped {
            params.push(SqlValue::Text(self.profile.clone()));
        }
        let records = rows(
            &self.store.db,
            &format!(
                "SELECT * FROM {table} WHERE {column}=?{}",
                if scoped { " AND profile=?" } else { "" }
            ),
            &params,
        )?;
        let mut row = records
            .into_iter()
            .next()
            .ok_or_else(|| Error(error.into()))?;
        decode(&mut row, keys)?;
        Ok(row)
    }
    fn fact(&self, file: &Value, operation: &str) -> Result<Value> {
        let records = rows(
            &self.store.db,
            &crate::catalog_source_succession::current::view(
                self.store.schema_version,
                "SELECT * FROM file_facts WHERE profile=? AND file_id=? AND operation=?",
            ),
            &[
                SqlValue::Text(self.profile.clone()),
                scalar(file)?,
                SqlValue::Text(operation.into()),
            ],
        )?;
        let Some(mut row) = records.into_iter().next() else {
            return Ok(Value::Null);
        };
        decode(&mut row, &["snapshot", "data"])?;
        let now = crate::components::occurrence(
            &self.store.db,
            &self.profile,
            file.as_str()
                .ok_or_else(|| Error("invalid file id".into()))?,
        )?;
        row["current"] = json!(
            row["status"] == "complete"
                && row["snapshot"].as_object().is_some_and(|snapshot| snapshot
                    .iter()
                    .all(|(k, v)| ["ctime_ns", "source_policy", "volume_uuid"]
                        .contains(&k.as_str())
                        || now[k] == *v))
        );
        Ok(row)
    }
    fn projection(&self, catalog: &Value) -> Result<Value> {
        CatalogQuery::new(&self.store, &self.profile)?.exists("catalogs", catalog)?;
        let records = rows(
            &self.store.db,
            "SELECT * FROM projection_bindings WHERE profile=? AND catalog=?",
            &[SqlValue::Text(self.profile.clone()), scalar(catalog)?],
        )?;
        let owner = rows(
            &self.store.db,
            "SELECT value FROM meta WHERE key=?",
            &[SqlValue::Text(format!(
                "layout-catalog:{}",
                catalog.as_str().unwrap()
            ))],
        )?
        .into_iter()
        .next()
        .map(|row| catabolic_core::decode_json(row["value"].as_str().unwrap()))
        .transpose()?
        .unwrap_or(Value::Null);
        let legacy = records.is_empty();
        let mut result=records.into_iter().next().unwrap_or_else(|| json!({"profile":self.profile,"catalog":catalog,"query_id":null,"layout":owner["layout"]}));
        result["legacy"] = json!(legacy);
        result["applied_layout"] = owner["layout"].clone();
        let copy = rows(
            &self.store.db,
            "SELECT definition FROM copy_policies WHERE catalog=?",
            &[scalar(catalog)?],
        )?;
        result["copy_policy"] = copy
            .first()
            .map(|r| catabolic_core::decode_json(r["definition"].as_str().unwrap()))
            .transpose()?
            .unwrap_or(Value::Null);
        let rendition = rows(
            &self.store.db,
            "SELECT definition FROM rendition_policies WHERE profile=? AND catalog=?",
            &[SqlValue::Text(self.profile.clone()), scalar(catalog)?],
        )?;
        result["rendition_policy"] = rendition
            .first()
            .map(|r| catabolic_core::decode_json(r["definition"].as_str().unwrap()))
            .transpose()?
            .unwrap_or(Value::Null);
        result["refresh"] = json!(rows(
            &self.store.db,
            "SELECT * FROM catalog_refresh_settings WHERE profile=? AND catalog=?",
            &[SqlValue::Text(self.profile.clone()), scalar(catalog)?]
        )?);
        Ok(result)
    }
    fn content_search(&mut self, args: &Value) -> Result<Value> {
        let text = args["text"]
            .as_str()
            .ok_or_else(|| Error("search requires 1..20 words and at most 4096 bytes".into()))?;
        let terms = crate::sql::casefold(text)
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if terms.is_empty() || terms.len() > 20 || text.len() > 4096 {
            return Err(Error(
                "search requires 1..20 words and at most 4096 bytes".into(),
            ));
        }
        let limit = args["first"]
            .as_i64()
            .filter(|n| (1..=1000).contains(n))
            .ok_or_else(|| Error("limit must be between 1 and 1000".into()))?;
        let after = args["after"].as_i64().unwrap_or(0);
        let mut params = vec![
            SqlValue::Text(terms[0].clone()),
            SqlValue::Integer(after),
            SqlValue::Text(self.profile.clone()),
        ];
        params.extend(terms.iter().skip(1).cloned().map(SqlValue::Text));
        params.push(SqlValue::Integer(limit + 1));
        let clauses = if terms.len() == 1 {
            "1".into()
        } else {
            vec![
                "EXISTS (SELECT 1 FROM text_terms t WHERE t.segment_id=s.id AND t.term=?)";
                terms.len() - 1
            ]
            .join(" AND ")
        };
        let mut result = rows(
            &self.store.db,
            &format!(
                "SELECT s.* FROM text_terms seed JOIN text_segments s ON s.id=seed.segment_id WHERE seed.term=? AND seed.segment_id>? AND s.profile=? AND {clauses} ORDER BY seed.segment_id LIMIT ?"
            ),
            &params,
        )?;
        let more = result.len() > limit as usize;
        result.truncate(limit as usize);
        for row in &mut result {
            decode(row, &["locator"])?;
            row["current"] = self.fact(&row["file_id"], "text")?["current"].clone();
            if row["current"].is_null() {
                row["current"] = json!(false);
            }
            row["text"] = json!(
                row["text"]
                    .as_str()
                    .unwrap()
                    .chars()
                    .take(2000)
                    .collect::<String>()
            );
        }
        self.records(result.len())?;
        let next = if more {
            result.last().unwrap()["id"].clone()
        } else {
            Value::Null
        };
        Ok(json!({"hits":result,"next_after":next,"match":"all normalized words"}))
    }
    fn check(&self) -> Result<()> {
        operation_charge(BudgetCharge::default(), &self.failure)?;
        if Instant::now() > self.deadline {
            record_failure(&self.failure, ExecutionFailure::Timeout);
            return Err(Error("query execution deadline exceeded".into()));
        }
        if let Some(error) = &self.aborted {
            return Err(Error(error.clone()));
        }
        Ok(())
    }
    fn charge(&mut self, value: &Value) -> Result<()> {
        operation_charge(
            BudgetCharge {
                bytes: encode(value).len(),
                values: 1,
                ..Default::default()
            },
            &self.failure,
        )?;
        if !value.is_null() {
            self.bytes += encode(value).len();
        }
        if self.bytes > 8 * 1024 * 1024 {
            record_failure(&self.failure, ExecutionFailure::ByteBudget);
            self.aborted = Some("query output budget exceeded".into());
        }
        self.check()
    }
    fn records(&mut self, count: usize) -> Result<()> {
        operation_charge(
            BudgetCharge {
                values: count,
                work: count,
                ..Default::default()
            },
            &self.failure,
        )?;
        self.nodes += count;
        if self.nodes > 10000 {
            record_failure(&self.failure, ExecutionFailure::WorkBudget);
            self.aborted = Some("query record budget exceeded".into());
        }
        self.check()
    }
    fn lookup(&mut self, entity: &str, id: &Value) -> Result<Value> {
        self.records(1)?;
        let args = [scalar(id)?];
        let mut result = if entity == "item" {
            rows(&self.store.db, "SELECT * FROM items WHERE id=?", &args)?
        } else {
            rows(
                &self.store.db,
                "SELECT f.*,o.size,o.mtime_ns,coalesce(o.status,'unknown') AS status,o.scan_id,s.finished_at AS observed_at FROM files f LEFT JOIN observations o ON o.file_id=f.id AND o.profile=? LEFT JOIN scans s ON s.id=o.scan_id WHERE f.id=?",
                &[SqlValue::Text(self.profile.clone()), scalar(id)?],
            )?
        };
        if entity == "item" {
            CatalogQuery::new(&self.store, &self.profile)?.decorate_items(&mut result)?;
        }
        Ok(result.into_iter().next().unwrap_or(Value::Null))
    }
    fn bound_path(&self, kind: &str, owner: &Value, path: Option<&str>) -> Result<Value> {
        let rows = rows(
            &self.store.db,
            "SELECT root FROM bindings WHERE profile=? AND kind=? AND owner=?",
            &[
                SqlValue::Text(self.profile.clone()),
                SqlValue::Text(kind.into()),
                scalar(owner)?,
            ],
        )?;
        Ok(rows
            .first()
            .map(|r| {
                if let Some(path) = path {
                    json!(format!(
                        "{}/{}",
                        r["root"].as_str().unwrap().trim_end_matches('/'),
                        path
                    ))
                } else {
                    r["root"].clone()
                }
            })
            .unwrap_or(Value::Null))
    }
    fn page(&mut self, field: &str, kwargs: &Value) -> Result<Value> {
        let mut args = serde_json::Map::new();
        for (key, value) in kwargs.as_object().unwrap() {
            args.insert(snake(key), value.clone());
        }
        let limit = args.remove("first").unwrap_or(json!(100));
        let cursor = args.remove("after").unwrap_or(Value::Null);
        args.insert("limit".into(), limit);
        args.insert("cursor".into(), cursor);
        for key in ["active", "direction", "sort", "status", "curation_status"] {
            if let Some(v) = args.get_mut(key)
                && let Some(text) = v.as_str()
            {
                *v = json!(text.to_lowercase());
            }
        }
        if args.remove("all_catalogs") == Some(json!(true)) {
            args.insert("catalog".into(), Value::Null);
        }
        let args = Value::Object(args);
        if args["limit"]
            .as_i64()
            .is_none_or(|n| !(1..=1000).contains(&n))
        {
            return Err(Error("first must be between 1 and 1000".into()));
        }
        let query = CatalogQuery::new(&self.store, &self.profile)?;
        let result = if [
            "items",
            "files",
            "associations",
            "relationships",
            "mappings",
            "tags",
            "taggings",
        ]
        .contains(&field)
        {
            query.execute(field, &args)?
        } else {
            let mut clauses = vec![];
            let mut values = vec![];
            let mut columns = String::from("a.*");
            let table: String;
            let mut id = "a.id".to_string();
            let mut context = json!([]);
            match field {
                "catalogs" => {
                    table = "catalogs a".into();
                }
                "savedQueries" | "operations" | "projections" => {
                    table = format!(
                        "{} a",
                        match field {
                            "savedQueries" => "saved_queries",
                            "operations" => "processing_recipes",
                            _ => "projection_bindings",
                        }
                    );
                    if field == "operations" {
                        columns = "a.*,a.id AS id".into();
                    }
                    if field != "operations" {
                        clauses.push("a.profile=?".into());
                        values.push(SqlValue::Text(self.profile.clone()));
                    }
                    if field == "savedQueries" {
                        columns="a.id,a.profile,a.name,a.revision,a.digest,a.created_at,CASE WHEN length(a.definition)<=8192 THEN a.definition END AS definition,length(a.definition)>8192 AS definition_omitted".into();
                    }
                    if field == "projections" {
                        id = "a.catalog".into();
                        columns = "a.*,a.catalog AS id".into();
                    }
                }
                "worklog" => {
                    query.exists("items", &args["item"])?;
                    table = "item_worklog a".into();
                    columns="CAST(a.id AS TEXT) AS id,a.item_id,a.profile,a.revision,a.kind,a.body,a.actor,a.data,a.created_at".into();
                    clauses.push("a.item_id=?".into());
                    values.push(scalar(&args["item"])?);
                    context = json!([args["item"]]);
                }
                "workflowChecks" => {
                    query.exists("items", &args["item"])?;
                    table = format!(
                        "({}) a",
                        view_sql_for(&self.store, "catalog_workflow_checks")
                    );
                    columns = "a.*,a.check_id AS id,coalesce(a.satisfied,0) AS passed".into();
                    id = "a.check_id".into();
                    clauses.extend(["a.item_id=?".into(), "a.profile=?".into()]);
                    values.extend([scalar(&args["item"])?, SqlValue::Text(self.profile.clone())]);
                    context = json!([args["item"]]);
                }
                "jobs" | "proposals" => {
                    table = format!(
                        "{} a",
                        if field == "jobs" {
                            "processing_jobs"
                        } else {
                            "proposals"
                        }
                    );
                    columns=if field=="jobs" {"a.id,a.profile,a.file_id,a.operation,a.state,a.attempts,a.error,a.created_at,a.finished_at"} else {"a.id,a.profile,a.file_id,a.kind,a.state,a.source,a.created_at"}.into();
                    clauses.push("a.profile=?".into());
                    values.push(SqlValue::Text(self.profile.clone()));
                    for (key, column) in [("state", "state"), ("file", "file_id")] {
                        if !args[key].is_null() {
                            clauses.push(format!("a.{column}=?"));
                            values.push(scalar(&args[key])?);
                        }
                    }
                    context = json!([args["state"], args["file"]]);
                }
                "artifacts" | "recipes" | "renditions" | "outputDefinitions" => {
                    if field == "artifacts" {
                        table =
                            "processing_artifacts a JOIN processing_jobs j ON j.id=a.job_id".into();
                        columns="a.id,a.job_id,a.attempt,a.state,a.file_id,a.item_id,a.role,a.location,a.path,a.size,a.sha256,a.error,j.file_id AS input_file_id,j.recipe_id".into();
                        clauses.push("a.profile=?".into());
                        values.push(SqlValue::Text(self.profile.clone()));
                        for (key, column) in [("state", "a.state"), ("file", "j.file_id")] {
                            if !args[key].is_null() {
                                clauses.push(format!("{column}=?"));
                                values.push(scalar(&args[key])?);
                            }
                        }
                    } else if field == "renditions" {
                        table = format!(
                            "({}) a",
                            view_sql_for(&self.store, "catalog_rendition_state")
                        );
                        clauses.push("a.profile=?".into());
                        values.push(SqlValue::Text(self.profile.clone()));
                        if !args["file"].is_null() {
                            clauses.push("a.source_file_id=?".into());
                            values.push(scalar(&args["file"])?);
                        }
                        if !args["matching"].is_null() {
                            let (extra, params) = rendition_conditions(&args["matching"], "a")?;
                            clauses.extend(extra);
                            values.extend(params);
                        }
                    } else {
                        table = format!(
                            "{} a",
                            if field == "recipes" {
                                "processing_recipes"
                            } else {
                                "output_definitions"
                            }
                        );
                    }
                    context = json!([args["state"], args["file"], args["matching"]]);
                }
                "components" | "componentOccurrences" => {
                    let column = if field == "components" {
                        "component_id"
                    } else {
                        "occurrence_id"
                    };
                    id = format!("a.{column}");
                    columns = format!("a.*,a.{column} AS id");
                    let view = if field == "components" {
                        "catalog_components"
                    } else {
                        "catalog_component_occurrences"
                    };
                    table = if self.authority.is_some() {
                        format!("{view} a")
                    } else {
                        format!("({}) a", view_sql_for(&self.store, view))
                    };
                    clauses.push("a.profile=?".into());
                    values.push(SqlValue::Text(self.profile.clone()));
                    for (key, column) in [
                        ("item", "item_id"),
                        ("file", "file_id"),
                        ("component", "component_id"),
                        ("kind", "kind"),
                        ("language", "language"),
                        ("forced", "forced"),
                        ("commentary", "commentary"),
                        ("current", "current"),
                        ("technically_verified", "technically_verified"),
                    ] {
                        if !args[key].is_null() {
                            clauses.push(if field == "components" && key == "item" {
                                "EXISTS (SELECT 1 FROM json_each(a.item_ids) WHERE value=?)".into()
                            } else {
                                format!("a.{column}=?")
                            });
                            values.push(scalar(&if key == "language" {
                                crate::components::language(&args[key])
                            } else {
                                args[key].clone()
                            })?);
                        }
                    }
                    let mut filtered = args.clone();
                    filtered.as_object_mut().unwrap().remove("limit");
                    filtered.as_object_mut().unwrap().remove("cursor");
                    context = json!([filtered]);
                }
                "workInbox" => {
                    table = format!("({}) a", view_sql_for(&self.store, "catalog_work_inbox"));
                    columns = "a.work_key AS id,a.*".into();
                    id = "a.work_key".into();
                    clauses.push("a.profile=?".into());
                    values.push(SqlValue::Text(self.profile.clone()));
                    let inactive = args["include_inactive"].as_bool().unwrap_or(false);
                    if !inactive {
                        clauses
                            .push("a.actionability IN ('ready','blocked','needs_decision')".into());
                    }
                    context = json!([inactive]);
                }
                _ => return Err(Error(format!("unknown GraphQL page: {field}"))),
            }
            let mut result = query.page(
                field,
                &columns,
                &table,
                &clauses,
                values,
                &[("id", &id)],
                &args,
                context,
            )?;
            for row in result[field].as_array_mut().unwrap() {
                decode(
                    row,
                    &[
                        "definition",
                        "metadata",
                        "data",
                        "item_ids",
                        "locator",
                        "observed",
                        "asserted",
                        "technical",
                        "compatibility",
                        "dependencies",
                        "provenance",
                        "conflicts",
                    ],
                )?;
                if field == "workInbox" {
                    decode(
                        row,
                        &[
                            "related_work",
                            "preconditions",
                            "suggested_actions",
                            "evidence",
                        ],
                    )?;
                }
                if field == "components" || field == "componentOccurrences" {
                    for key in [
                        "current",
                        "technically_verified",
                        "dependencies_complete",
                        "forced",
                        "default_flag",
                        "commentary",
                        "hearing_impaired",
                        "visual_impaired",
                    ] {
                        if !row[key].is_null() {
                            row[key] = json!(row[key].as_i64() != Some(0));
                        }
                    }
                }
            }
            self.charge(&result)?;
            result
        };
        self.records(result[field].as_array().unwrap().len())?;
        Ok(
            json!({"nodes":result[field],"pageInfo":{"endCursor":result["next_cursor"],"hasNextPage":!result["next_cursor"].is_null()}}),
        )
    }
    fn resolve(
        &mut self,
        parent: &str,
        field: &str,
        source: &Value,
        args: &Value,
    ) -> Result<Value> {
        self.check()?;
        if parent == "Query" {
            let result = match field {
                "profile" => json!(self.profile),
                "schemaVersion" => json!(SCHEMA_VERSION),
                "mediaTypes" => reference()["media_types"].clone(),
                "item" => {
                    if args["id"].is_null() == args["identity"].is_null() {
                        return Err(Error("supply exactly one item id or identity".into()));
                    }
                    if !args["identity"].is_null() {
                        let identities = rows(
                            &self.store.db,
                            "SELECT item_id FROM identities WHERE namespace=? AND value=?",
                            &crate::query::identity(&args["identity"])?,
                        )?;
                        if let Some(row) = identities.first() {
                            self.lookup("item", &row["item_id"])?
                        } else {
                            Value::Null
                        }
                    } else {
                        self.lookup("item", &args["id"])?
                    }
                }
                "file" => self.lookup("file", &args["id"])?,
                "savedQuery" | "operation" | "recipe" | "artifact" | "job" | "proposal"
                | "outputDefinition" | "rendition" | "fallbackPolicy" => {
                    self.records(1)?;
                    let (table, scoped, error, keys): (&str, bool, &str, &[&str]) = match field {
                        "savedQuery" => (
                            "saved_queries",
                            true,
                            "unknown query revision in this profile",
                            &["definition"],
                        ),
                        "operation" => (
                            "processing_recipes",
                            false,
                            "unknown immutable operation revision",
                            &["definition"],
                        ),
                        "recipe" => (
                            "processing_recipes",
                            false,
                            "unknown recipe ID; select an explicit immutable revision",
                            &[
                                "definition",
                                "binding",
                                "validation",
                                "publication_snapshot",
                            ],
                        ),
                        "artifact" => (
                            "processing_artifacts",
                            true,
                            "unknown artifact",
                            &[
                                "definition",
                                "binding",
                                "validation",
                                "publication_snapshot",
                            ],
                        ),
                        "job" => (
                            "processing_jobs",
                            true,
                            "unknown job",
                            &["snapshot", "options", "result"],
                        ),
                        "proposal" => (
                            "proposals",
                            true,
                            "unknown proposal in this profile",
                            &["payload", "evidence", "snapshot", "result"],
                        ),
                        "outputDefinition" => (
                            "output_definitions",
                            false,
                            "unknown output definition; use an immutable definition ID",
                            &["definition", "metadata"],
                        ),
                        "fallbackPolicy" => (
                            "fallback_policies",
                            true,
                            "unknown fallback policy revision in this profile",
                            &["definition"],
                        ),
                        _ => (
                            "",
                            true,
                            "unknown media output",
                            &["definition", "metadata"],
                        ),
                    };
                    let table = if field == "rendition" {
                        format!("({})", view_sql_for(&self.store, "catalog_renditions"))
                    } else {
                        table.to_string()
                    };
                    let row = self.one(&table, "id", &args["id"], scoped, error, keys)?;
                    if field == "recipe" && row["operation_kind"] != "render" {
                        return Err(Error(
                            "artifact execution requires a render operation".into(),
                        ));
                    }
                    if field == "fallbackPolicy" {
                        json!({"id":row["id"],"name":row["name"],"revision":row["revision"],"tiers":row["definition"]["fallbacks"].as_array().unwrap().iter().map(|v| v["name"].clone()).collect::<Vec<_>>()})
                    } else {
                        row
                    }
                }
                "projection" => {
                    self.records(1)?;
                    self.projection(&args["catalog"])?
                }
                "contentSearch" => self.content_search(args)?,
                "fallbackResolutions" => {
                    if self.authority.as_ref().is_some_and(|authority| {
                        !authority.permits_catalog(args["catalog"].as_str().unwrap_or(""))
                    }) {
                        return Err(Error("not_found".into()));
                    }
                    let restriction = if self.authority.is_some() {
                        " AND e.item_id IN (SELECT id FROM items) AND (e.file_id IS NULL OR e.file_id IN (SELECT id FROM files))"
                    } else {
                        ""
                    };
                    let first = args["first"]
                        .as_i64()
                        .filter(|n| (1..=1000).contains(n))
                        .ok_or_else(|| {
                            Error("resolution page size must be between 1 and 1000".into())
                        })?;
                    let mut records = rows(
                        &self.store.db,
                        &format!(
                            "SELECT e.id,e.item_id,e.file_id,e.revision,e.policy_id,e.state,e.tier,e.generation,e.published_generation FROM fallback_entries e WHERE e.profile=? AND e.catalog=? AND e.active=1 AND e.id>?{restriction} ORDER BY e.id LIMIT ?"
                        ),
                        &[
                            SqlValue::Text(self.profile.clone()),
                            scalar(&args["catalog"])?,
                            SqlValue::Text(args["after"].as_str().unwrap_or("").into()),
                            SqlValue::Integer(first + 1),
                        ],
                    )?;
                    let more = records.len() > first as usize;
                    records.truncate(first as usize);
                    self.records(records.len())?;
                    let next = if more {
                        records.last().unwrap()["id"].clone()
                    } else {
                        Value::Null
                    };
                    json!({"nodes":records,"pageInfo":{"hasNextPage":more,"endCursor":next}})
                }
                _ => return self.page(field, args),
            };
            self.charge(&result)?;
            return Ok(result);
        }
        if parent == "Item" && ["worklog", "workflowChecks"].contains(&field) {
            let mut args = args.clone();
            args["item"] = source["id"].clone();
            return self.page(field, &args);
        }
        if ["associations", "relationships", "mappings", "taggings"].contains(&field) {
            let mut args = args.clone();
            args[match parent {
                "Item" => "item",
                "File" => "file",
                _ => "catalog",
            }] = source["id"].clone();
            return self.page(field, &args);
        }
        if ["item", "file", "source", "target"].contains(&field)
            && ["Association", "Mapping", "Relationship"].contains(&parent)
        {
            return self.lookup(
                if field == "file" { "file" } else { "item" },
                &source[format!("{field}_id")],
            );
        }
        let result = if parent == "Item" && field == "title" {
            source["metadata"]["title"]
                .as_str()
                .map(|s| json!(s))
                .unwrap_or(Value::Null)
        } else if parent == "Item" && field == "year" {
            source["metadata"]["year"]
                .as_i64()
                .filter(|n| (1..=9999).contains(n))
                .map(|n| json!(n))
                .unwrap_or(Value::Null)
        } else if parent == "File" && field == "facts" {
            let mut facts = vec![];
            for operation in ["sniff", "hash", "verify", "probe", "text", "decode"] {
                let fact = self.fact(&source["id"], operation)?;
                if !fact.is_null() {
                    facts.push(fact);
                }
            }
            self.records(facts.len())?;
            json!(facts)
        } else if parent == "File" && field == "sourcePath" {
            self.bound_path("source", &source["location"], source["path"].as_str())?
        } else if parent == "Mapping" && field == "outputPath" {
            self.bound_path("output", &source["catalog"], source["path"].as_str())?
        } else if parent == "Catalog" && field == "root" {
            self.bound_path("output", &source["id"], None)?
        } else if parent == "Catalog" && field == "linkMode" {
            rows(&self.store.db,"SELECT coalesce((SELECT mode FROM catalog_link_modes WHERE catalog=?),'symlink') AS mode",&[scalar(&source["id"])?])?[0]["mode"].clone()
        } else {
            let mut value = source.get(field).unwrap_or(&source[snake(field)]).clone();
            if ["status", "requestedStatus"].contains(&field)
                && let Some(s) = value.as_str()
            {
                value = json!(s.to_uppercase());
            }
            if field == "active" {
                value = json!(value.as_bool().unwrap_or_else(|| value.as_i64() != Some(0)));
            }
            value
        };
        if !["nodes", "pageInfo", "identities"].contains(&field) {
            self.charge(&result)?;
        }
        Ok(result)
    }
}
// The schema is object-only. Collect each selection set before invoking any of
// its resolvers; merge by response key and complete the combined child set once.
// All source access remains in Context::authorized_resolve on the borrowed Store.
struct Execution<'a> {
    state: &'a Arc<Mutex<Context>>,
    inputs: &'a crate::graphql_runtime_input::RuntimeInputs,
    http: bool,
    errors: Vec<async_graphql::ServerError>,
}
fn path_order(
    a: &[async_graphql::PathSegment],
    b: &[async_graphql::PathSegment],
) -> std::cmp::Ordering {
    use async_graphql::PathSegment;
    for (a, b) in a.iter().zip(b) {
        let ordering = match (a, b) {
            (PathSegment::Field(a), PathSegment::Field(b)) => a.cmp(b),
            (PathSegment::Index(a), PathSegment::Index(b)) => a.cmp(b),
            (PathSegment::Field(_), PathSegment::Index(_)) => std::cmp::Ordering::Less,
            (PathSegment::Index(_), PathSegment::Field(_)) => std::cmp::Ordering::Greater,
        };
        if !ordering.is_eq() {
            return ordering;
        }
    }
    a.len().cmp(&b.len())
}
impl Execution<'_> {
    fn selection(
        &mut self,
        source: &Value,
        parent: &str,
        set: &SelectionSet,
        path: &mut Vec<async_graphql::PathSegment>,
    ) -> async_graphql::ServerResult<Value> {
        // Never invoke an outer signal handler while holding the resolver lock.
        if crate::embedding::poll_interrupt() {
            record_failure(
                &self.state.lock().expect("query context").failure,
                ExecutionFailure::Cancelled,
            );
            return Err(async_graphql::ServerError::new(
                "query execution interrupted",
                None,
            ));
        }
        self.state.lock().expect("query context").executed = true;
        let fields = self
            .inputs
            .fields(set, parent)
            .map_err(|error| error.server_error(path.clone()))?;
        let mut data = serde_json::Map::new();
        for field in fields {
            let name = field.field.node.name.node.as_str();
            let key = field.field.node.response_key().node.to_string();
            let definition = crate::graphql_runtime_input::field_def(parent, name, self.http)
                .expect("strictly validated field");
            let ty = crate::graphql_runtime_input::field_type(&definition);
            path.push(async_graphql::PathSegment::Field(key.clone()));
            let result = self
                .resolve(parent, source, &field, &definition, path)
                .and_then(|value| self.complete(value, &ty, parent, &field, path));
            path.pop();
            match result {
                Ok(value) => {
                    data.insert(key, value);
                }
                Err(error) if ty.nullable => {
                    self.errors.push(error);
                    data.insert(key, Value::Null);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(Value::Object(data))
    }
    fn error(
        message: impl Into<String>,
        field: &crate::graphql_runtime_input::CollectedField,
        path: &[async_graphql::PathSegment],
    ) -> async_graphql::ServerError {
        let mut error = async_graphql::ServerError::new(message, None);
        error.locations = field.locations.clone();
        error.path = path.to_vec();
        error
    }
    fn resolve(
        &self,
        parent: &str,
        source: &Value,
        field: &crate::graphql_runtime_input::CollectedField,
        definition: &Value,
        path: &[async_graphql::PathSegment],
    ) -> async_graphql::ServerResult<Value> {
        if crate::embedding::poll_interrupt() {
            record_failure(
                &self.state.lock().expect("query context").failure,
                ExecutionFailure::Cancelled,
            );
            return Err(Self::error("query execution interrupted", field, path));
        }
        // Argument coercion precedes resolver invocation, authorization and
        // middleware. The first encountered node supplies a merged field's args.
        if let Some(error) = self.inputs.argument_error(&field.field.node) {
            return Err(error.server_error(path.to_vec()));
        }
        let args = self.inputs.arguments(&field.field.node, definition);
        let name = field.field.node.name.node.as_str();
        let mut context = self.state.lock().expect("query context");
        context.fields += 1;
        if context.fields > 20000 {
            record_failure(&context.failure, ExecutionFailure::WorkBudget);
            context.aborted = Some("query field budget exceeded".into());
        }
        let result = context.check().and_then(|()| {
            if parent.starts_with("__") || name.starts_with("__") {
                let value = crate::introspection::resolve(parent, name, source, &args, self.http);
                // graphql-core's schema type map and deprecated-inclusive maps
                // return dict views. Count the field, but not list records.
                let dict_view = parent == "__Schema" && name == "types"
                    || args["includeDeprecated"] == true
                        && (parent == "__Type" && matches!(name, "enumValues" | "inputFields")
                            || parent == "__Field" && name == "args");
                if let Some(values) = value.as_array().filter(|_| !dict_view) {
                    context.records(values.len())?;
                } else if value.is_string() {
                    context.charge(&value)?;
                }
                Ok(value)
            } else {
                context.authorized_resolve(parent, name, source, &args)
            }
        });
        result.map_err(|error| {
            let message = error.to_string();
            let mut error = Self::error(&message, field, path);
            if context.authority.is_none() || message != "invalid_cursor" {
                let code = if context.aborted.is_some() || Instant::now() > context.deadline {
                    "LIMIT_EXCEEDED"
                } else if context.authority.is_some()
                    && [
                        "field is not authorized",
                        "filter is not authorized",
                        "resource is not authorized",
                    ]
                    .contains(&message.as_str())
                {
                    "FORBIDDEN"
                } else {
                    "QUERY_ERROR"
                };
                error.extensions = async_graphql::Error::new(message)
                    .extend_with(|_, extensions| extensions.set("code", code))
                    .extensions;
            }
            error
        })
    }
    fn complete(
        &mut self,
        value: Value,
        ty: &Type,
        parent: &str,
        field: &crate::graphql_runtime_input::CollectedField,
        path: &mut Vec<async_graphql::PathSegment>,
    ) -> async_graphql::ServerResult<Value> {
        if crate::embedding::poll_interrupt() {
            record_failure(
                &self.state.lock().expect("query context").failure,
                ExecutionFailure::Cancelled,
            );
            return Err(Self::error("query execution interrupted", field, path));
        }
        self.state
            .lock()
            .expect("query context")
            .check()
            .map_err(|error| Self::error(error.to_string(), field, path))?;
        if value.is_null() {
            return if ty.nullable {
                Ok(Value::Null)
            } else {
                Err(Self::error(
                    format!(
                        "Cannot return null for non-nullable field {parent}.{}.",
                        field.field.node.name.node
                    ),
                    field,
                    path,
                ))
            };
        }
        match &ty.base {
            BaseType::List(inner) => {
                let Some(values) = value.as_array() else {
                    return Err(Self::error(
                        format!(
                            "Expected Iterable, but did not find one for field '{parent}.{}'.",
                            field.field.node.name.node
                        ),
                        field,
                        path,
                    ));
                };
                let mut data = Vec::with_capacity(values.len());
                for (index, value) in values.iter().enumerate() {
                    path.push(async_graphql::PathSegment::Index(index));
                    let completed = self.complete(value.clone(), inner, parent, field, path);
                    path.pop();
                    match completed {
                        Ok(value) => data.push(value),
                        Err(error) if inner.nullable => {
                            self.errors.push(error);
                            data.push(Value::Null);
                        }
                        Err(error) => return Err(error),
                    }
                }
                Ok(Value::Array(data))
            }
            BaseType::Named(name) if !field.field.node.selection_set.node.items.is_empty() => self
                .selection(
                    &value,
                    name.as_str(),
                    &field.field.node.selection_set.node,
                    path,
                ),
            BaseType::Named(name) => {
                crate::graphql_runtime_input::serialize(name.as_str(), value, self.http)
                    .map_err(|message| Self::error(message, field, path))
            }
        }
    }
}

fn snake(value: &str) -> String {
    let mut out = String::new();
    for (i, c) in value.chars().enumerate() {
        if i > 0 && c.is_uppercase() {
            out.push('_');
        }
        out.extend(c.to_lowercase());
    }
    out
}
fn type_ref(ty: &Type) -> TypeRef {
    let base = match &ty.base {
        BaseType::Named(name) => TypeRef::named(name.to_string()),
        BaseType::List(ty) => TypeRef::List(Box::new(type_ref(ty))),
    };
    if ty.nullable {
        base
    } else {
        TypeRef::NonNull(Box::new(base))
    }
}
fn input(value: async_graphql_parser::types::InputValueDefinition) -> InputValue {
    let mut input = InputValue::new(value.name.node.to_string(), type_ref(&value.ty.node));
    if let Some(default) = value.default_value {
        input = input.default_value(default.node);
    }
    if let Some(description) = value.description {
        input = input.description(description.node);
    }
    input
}
fn schema(http: bool) -> Result<&'static Schema> {
    static SCHEMA: OnceLock<std::result::Result<Schema, String>> = OnceLock::new();
    static HTTP_SCHEMA: OnceLock<std::result::Result<Schema, String>> = OnceLock::new();
    let schema = if http { &HTTP_SCHEMA } else { &SCHEMA };
    schema
        .get_or_init(|| build_schema(http).map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| Error(e.clone()))
}
fn build_schema(http: bool) -> Result<Schema> {
    let document = parse_schema(SDL).map_err(|e| Error(e.to_string()))?;
    let mut object_fields: BTreeMap<
        String,
        Vec<async_graphql_parser::Positioned<async_graphql_parser::types::FieldDefinition>>,
    > = BTreeMap::new();
    let mut descriptions = BTreeMap::new();
    let mut definitions = vec![];
    for definition in document.definitions {
        if let TypeSystemDefinition::Type(definition) = definition {
            let definition = definition.node;
            if let Some(description) = &definition.description {
                descriptions.insert(definition.name.node.to_string(), description.node.clone());
            }
            match &definition.kind {
                TypeKind::Interface(_) | TypeKind::Union(_) => {
                    return Err(Error(
                        "native GraphQL completion requires concrete object types".into(),
                    ));
                }
                TypeKind::Object(obj) => {
                    object_fields
                        .entry(definition.name.node.to_string())
                        .or_default()
                        .extend(obj.fields.clone());
                }
                _ => {}
            }
            definitions.push(definition);
        }
    }
    let mut builder = Schema::build("Query", None, None)
        .extension(Budget)
        .limit_depth(20)
        .limit_complexity(20000);
    for definition in definitions {
        let name = definition.name.node.to_string();
        match definition.kind {
            TypeKind::Scalar => {
                let mut scalar = Scalar::new(name);
                if let Some(description) = definition.description {
                    scalar = scalar.description(description.node);
                }
                builder = builder.register(scalar);
            }
            TypeKind::Enum(enumeration) => {
                let mut enumeration_type = Enum::new(name.clone());
                if let Some(description) = descriptions.get(&name) {
                    enumeration_type = enumeration_type.description(description);
                }
                for item in enumeration.values {
                    let mut entry = EnumItem::new(item.node.value.node.to_string());
                    if let Some(description) = item.node.description {
                        entry = entry.description(description.node);
                    }
                    enumeration_type = enumeration_type.item(entry);
                }
                builder = builder.register(enumeration_type);
            }
            TypeKind::InputObject(obj) => {
                let mut input_object = InputObject::new(name.clone());
                if let Some(description) = descriptions.get(&name) {
                    input_object = input_object.description(description);
                }
                for field in obj.fields {
                    input_object = input_object.field(input(field.node));
                }
                builder = builder.register(input_object);
            }
            _ => {}
        }
    }
    for (parent, fields) in object_fields {
        let mut object = Object::new(&parent);
        if let Some(description) = descriptions.get(&parent) {
            object = object.description(description);
        }
        for field in fields {
            let field = field.node;
            if http && parent == "Query" && field.name.node == "workInbox" {
                continue;
            }
            let name = field.name.node.to_string();
            // Registered for strict validation only. Budget::execute owns
            // native field resolution and completion after validation succeeds.
            let mut dynamic =
                Field::new(name, type_ref(&field.ty.node), |_| FieldFuture::Value(None));
            for argument in field.arguments {
                dynamic = dynamic.argument(input(argument.node));
            }
            if let Some(description) = field.description {
                dynamic = dynamic.description(description.node);
            }
            object = object.field(dynamic);
        }
        builder = builder.register(object);
    }
    builder.finish().map_err(|e| Error(e.to_string()))
}
/// Frozen LOCAL_SDL bytes, without building a schema or opening a catalog.
pub fn schema_text() -> &'static str {
    SDL
}

pub fn description() -> Value {
    json!({"interface_version":1,"schema":SDL,"limits":{"document_bytes":131072,"page_size":1000,"returned_records":10000,"resolved_fields":20000,"depth":20,"result_bytes":8*1024*1024,"default_timeout_ms":5000}})
}
fn check_document(document: &async_graphql_parser::types::ExecutableDocument) -> Result<()> {
    fn visit(
        set: &SelectionSet,
        document: &async_graphql_parser::types::ExecutableDocument,
        depth: usize,
        seen: &BTreeSet<String>,
        count: &mut usize,
    ) -> Result<()> {
        if depth > 20 {
            return Err(Error("query depth exceeds 20".into()));
        }
        for selection in &set.items {
            *count += 1;
            if *count > 2000 {
                return Err(Error("query exceeds 2000 expanded selections".into()));
            }
            match &selection.node {
                Selection::Field(field) => visit(
                    &field.node.selection_set.node,
                    document,
                    depth + 1,
                    seen,
                    count,
                )?,
                Selection::InlineFragment(fragment) => visit(
                    &fragment.node.selection_set.node,
                    document,
                    depth,
                    seen,
                    count,
                )?,
                Selection::FragmentSpread(spread) => {
                    let name = spread.node.fragment_name.node.to_string();
                    if seen.contains(&name) {
                        return Err(Error("fragment cycle is not allowed".into()));
                    }
                    if let Some(fragment) = document.fragments.get(name.as_str()) {
                        let mut next = seen.clone();
                        next.insert(name);
                        visit(
                            &fragment.node.selection_set.node,
                            document,
                            depth,
                            &next,
                            count,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
    let mut count = 0;
    for (_, operation) in document.operations.iter() {
        if operation.node.ty != async_graphql_parser::types::OperationType::Query {
            return Err(Error(
                "only read-only query operations are supported".into(),
            ));
        }
        visit(
            &operation.node.selection_set.node,
            document,
            0,
            &BTreeSet::new(),
            &mut count,
        )?;
    }
    for fragment in document.fragments.values() {
        visit(
            &fragment.node.selection_set.node,
            document,
            0,
            &BTreeSet::new(),
            &mut count,
        )?;
    }
    Ok(())
}
// Operation selection and scalar rejection can finish before a resolver exists.
// They still participate in the same deadline and outer interruption bridge.
#[cfg(test)]
fn early_query_response(result: Value, deadline: Instant) -> Value {
    early_query_response_classified(result, deadline, &Arc::new(Mutex::new(None)))
}
fn early_query_response_classified(
    result: Value,
    deadline: Instant,
    failure: &Arc<Mutex<Option<ExecutionFailure>>>,
) -> Value {
    if crate::embedding::poll_interrupt() {
        record_failure(failure, ExecutionFailure::Cancelled);
        return json!({"data":null,"errors":[{"message":"query execution interrupted"}]});
    }
    if Instant::now() > deadline {
        record_failure(failure, ExecutionFailure::Timeout);
        return json!({"data":null,"errors":[{"message":"query execution deadline exceeded","extensions":{"code":"LIMIT_EXCEEDED"}}]});
    }
    result
}

pub fn execute(
    path: &Path,
    document: &str,
    profile: &str,
    variables: impl Into<InputVariables>,
    operation: Option<&str>,
    timeout_ms: u64,
) -> Result<Value> {
    execute_store(
        Store::open(path, false, false)?,
        document,
        profile,
        variables,
        operation,
        timeout_ms,
    )
    .map(|(_, result)| result)
}

pub fn execute_store(
    store: Store,
    document: &str,
    profile: &str,
    variables: impl Into<InputVariables>,
    operation: Option<&str>,
    timeout_ms: u64,
) -> Result<(Store, Value)> {
    execute_context(
        store, None, document, profile, variables, operation, timeout_ms,
    )
}

/// Execute only against the snapshot whose authorized TEMP views minted authority.
/// The returned store remains the same owned snapshot for composed evaluation.
pub fn execute_authorized_store(
    store: Store,
    authority: Authority,
    document: &str,
    variables: impl Into<InputVariables>,
    operation: Option<&str>,
    timeout_ms: u64,
) -> Result<(Store, Value)> {
    authority.check_store(&store)?;
    authority.require("metadata:read")?;
    let profile = authority.profile().to_owned();
    execute_context(
        store,
        Some(authority),
        document,
        &profile,
        variables,
        operation,
        timeout_ms,
    )
}

/// Frozen GraphQL validates the document before AuthorizedContext creates TEMP
/// scope. Package sessions must not materialize scope for invalid documents.
pub fn validate_before_context(
    document: &str,
    variables: &Value,
    timeout_ms: u64,
    http: bool,
) -> Result<Option<Value>> {
    if !(1..=60000).contains(&timeout_ms) {
        return Err(Error("timeout-ms must be between 1 and 60000".into()));
    }
    if document.len() > 131072 {
        return Err(Error("query document must be at most 131072 bytes".into()));
    }
    if !variables.is_object() {
        return Err(Error("variables must be a JSON object".into()));
    }
    if encode(variables).len() > 131072 {
        return Err(Error("variables exceed input budget".into()));
    }
    if let Some(error) = crate::document::token_limit(document, 4000) {
        // This bounded, iterative cursor can report only the versioned early
        // syntax boundaries. The full recursive AST parser stays after admission.
        return Ok(Some(
            crate::graphql_parse::before_token_limit(document, 4000).map_or(error, |error| {
                crate::validation::parse_error(document, &error)
            }),
        ));
    }
    let ast = match parse_query(document) {
        Ok(ast) => ast,
        Err(error) => return Ok(Some(crate::validation::parse_error(document, &error))),
    };
    if let Err(error) = check_document(&ast) {
        return Ok(Some(
            json!({"data":null,"errors":[{"message":error.to_string()}]}),
        ));
    }
    if let Some(error) = crate::validation::validate_for(&ast, document, http) {
        return Ok(Some(error));
    }
    Ok(crate::graphql_runtime_input::merged_selection_conflict(
        &ast, http,
    ))
}

#[allow(clippy::too_many_arguments)]
fn execute_context(
    store: Store,
    authority: Option<Authority>,
    document: &str,
    profile: &str,
    variables: impl Into<InputVariables>,
    operation: Option<&str>,
    timeout_ms: u64,
) -> Result<(Store, Value)> {
    let mut slot = Some(store);
    let result = execute_session(
        &mut slot, authority, document, profile, variables, operation, timeout_ms,
    )?;
    Ok((slot.expect("GraphQL restored its Store"), result))
}

/// Lend an existing Store to the synchronous GraphQL executor and restore that
/// exact owner on success, error, cancellation, and unwind. No database reopens.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_session(
    slot: &mut Option<Store>,
    authority: Option<Authority>,
    document: &str,
    profile: &str,
    variables: impl Into<InputVariables>,
    operation: Option<&str>,
    timeout_ms: u64,
) -> Result<Value> {
    execute_session_classified(
        slot,
        authority,
        document,
        profile,
        variables,
        operation,
        timeout_ms,
        &Cell::new(None),
    )
}

/// Add refusal evidence without changing diagnostics, response JSON or Store ownership.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_session_classified(
    slot: &mut Option<Store>,
    authority: Option<Authority>,
    document: &str,
    profile: &str,
    variables: impl Into<InputVariables>,
    operation: Option<&str>,
    timeout_ms: u64,
    failure: &Cell<Option<ExecutionFailure>>,
) -> Result<Value> {
    failure.set(None);
    let evidence = Arc::new(Mutex::new(None));
    let result = execute_session_inner(
        slot, authority, document, profile, variables, operation, timeout_ms, &evidence,
    );
    failure.set(*evidence.lock().expect("GraphQL failure lock"));
    result
}

#[allow(clippy::too_many_arguments)]
fn execute_session_inner(
    slot: &mut Option<Store>,
    authority: Option<Authority>,
    document: &str,
    profile: &str,
    variables: impl Into<InputVariables>,
    operation: Option<&str>,
    timeout_ms: u64,
    failure: &Arc<Mutex<Option<ExecutionFailure>>>,
) -> Result<Value> {
    let input_variables = variables.into();
    let variables = &input_variables.value;
    let store = slot
        .as_ref()
        .ok_or_else(|| Error("GraphQL Store is unavailable".into()))?;
    if let Some(authority) = &authority {
        authority.check_store(store)?;
        authority.require("metadata:read")?;
        if authority.profile() != profile {
            return Err(Error("evaluation_session_identity_mismatch".into()));
        }
    }
    let http = authority.is_some();
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    if let Some(error) = validate_before_context(document, variables, timeout_ms, http)? {
        return Ok(error);
    }
    CatalogQuery::new(store, profile)?;
    let mut ast = parse_query(document).map_err(|error| Error(error.to_string()))?;
    // async-graphql accepts wider integer variables than graphql-core. Coerce
    // all selected input variables before any resolver runs, preserving the frozen
    // validation/operation-selection precedence and the caller's Store.
    let operations = ast.operations.iter().collect::<Vec<_>>();
    let selected_exists = match operation {
        Some(selected) => operations
            .iter()
            .any(|(name, _)| name.is_some_and(|name| name.as_str() == selected)),
        None => operations.len() == 1,
    };
    if operation.is_none() && operations.len() > 1 {
        return Ok(early_query_response_classified(
            json!({"data":null,"errors":[{"message":"Must provide operation name if query contains multiple operations."}],"extensions":{"interfaceVersion":1,"profile":profile,"records":0}}),
            deadline,
            failure,
        ));
    }
    if !selected_exists && let Some(selected) = operation {
        return Ok(early_query_response_classified(
            json!({"data":null,"errors":[{"message":format!("Unknown operation named '{selected}'.")}],"extensions":{"interfaceVersion":1,"profile":profile,"records":0}}),
            deadline,
            failure,
        ));
    }
    let selected_operation = operations
        .iter()
        .find(|(name, _)| {
            operation.is_none_or(|selected| name.is_some_and(|name| name.as_str() == selected))
        })
        .expect("selected operation checked")
        .1;
    let (coerced_variables, errors) =
        crate::graphql_input::variables(&selected_operation.node, &input_variables);
    if !errors.is_empty() {
        return Ok(early_query_response_classified(
            json!({"data":null,"errors":errors,"extensions":{"interfaceVersion":1,"profile":profile,"records":0}}),
            deadline,
            failure,
        ));
    }
    let runtime_inputs = crate::graphql_runtime_input::RuntimeInputs::new(
        &ast,
        &coerced_variables,
        operation,
        http,
        document,
    );
    crate::graphql_input::normalize_document_literals(&mut ast, http);
    let query_only: bool = store
        .db
        .pragma_query_value(None, "query_only", |row| row.get(0))?;
    let state = Arc::new(Mutex::new(Context {
        failure: Arc::clone(failure),
        store: slot.take().expect("checked GraphQL Store"),
        profile: profile.into(),
        deadline,
        nodes: 0,
        fields: 0,
        bytes: 0,
        aborted: None,
        authority,
        runtime_inputs: Arc::new(runtime_inputs),
        executed: false,
    }));
    // Keep an external context reference so even a resolver panic cannot lose
    // the caller's transaction. All executor borrows end before restoration.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Value> {
        {
            let context = state
                .lock()
                .map_err(|_| Error("query context poisoned".into()))?;
            crate::sql::setup(&context.store.db)?;
            context.store.db.pragma_update(None, "query_only", true)?;
            let interrupted = Arc::clone(failure);
            let vm_budget = OperationBudget::current();
            context.store.db.progress_handler(
                1000,
                Some(move || {
                    if let Some(cause) = vm_budget.as_ref().and_then(|budget| {
                        budget
                            .charge(BudgetCharge {
                                work: 1000,
                                ..Default::default()
                            })
                            .err()
                    }) {
                        record_failure(&interrupted, operation_failure(cause));
                        return true;
                    }
                    if vm_budget.is_none() && crate::embedding::poll_interrupt() {
                        record_failure(&interrupted, ExecutionFailure::Cancelled);
                        true
                    } else if Instant::now() >= deadline {
                        record_failure(&interrupted, ExecutionFailure::Timeout);
                        true
                    } else {
                        false
                    }
                }),
            )?;
        }
        #[cfg(test)]
        if PANIC_AFTER_SETUP.with(|flag| flag.replace(false)) {
            panic!("injected GraphQL executor unwind");
        }
        let mut request = Request::new(document)
            .variables(Variables::from_json(coerced_variables.clone()))
            .data(state.clone());
        if let Some(operation) = operation {
            request = request.operation_name(operation);
        }
        request.set_parsed_query(ast.clone());
        let response = futures::executor::block_on(schema(http)?.execute(request));
        let mut result = serde_json::to_value(response)?;
        let mut coercion_error = false;
        if result["errors"][0]["message"] == "Operation name required in request." {
            result["errors"][0]["message"] =
                json!("Must provide operation name if query contains multiple operations.");
            coercion_error = true;
        }

        if let Some(errors) = result.get_mut("errors").and_then(Value::as_array_mut) {
            for error in errors {
                let message = error["message"].as_str().unwrap_or("").to_owned();
                if message.starts_with("Unknown field ") {
                    let words = message.split('"').collect::<Vec<_>>();
                    if words.len() >= 5 {
                        error["message"] = json!(format!(
                            "Cannot query field '{}' on type '{}'.{}",
                            words[1],
                            words[3],
                            crate::introspection::field_suggestions_for(words[3], words[1], http)
                        ));
                    }
                }
                if let Some(name) = message
                    .strip_prefix("Variable ")
                    .and_then(|m| m.strip_suffix(" is not defined."))
                {
                    for (_, operation) in ast.operations.iter() {
                        if let Some(definition) = operation
                            .node
                            .variable_definitions
                            .iter()
                            .find(|v| v.node.name.node.as_str() == name)
                        {
                            error["message"] = json!(format!(
                                "Variable '${name}' of required type '{}' was not provided.",
                                definition.node.var_type.node
                            ));
                            error["locations"] = json!([{"line":definition.pos.line,"column":definition.pos.column}]);
                            coercion_error = true;
                        }
                    }
                }
            }
        }

        let state = state
            .lock()
            .map_err(|_| Error("query context poisoned".into()))?;
        (|| -> Result<Value> {
            if state.aborted.is_some() || Instant::now() > state.deadline {
                if state.aborted.is_none() {
                    record_failure(failure, ExecutionFailure::Timeout);
                }
                return Ok(
                    json!({"data":null,"errors":[{"message":state.aborted.as_deref().unwrap_or("query execution deadline exceeded"),"extensions":{"code":"LIMIT_EXCEEDED"}}]}),
                );
            }
            let successful = result
                .get("errors")
                .is_none_or(|errors| errors.as_array().is_some_and(Vec::is_empty));
            if successful || state.fields > 0 || coercion_error || state.executed {
                result["extensions"] =
                    json!({"interfaceVersion":1,"profile":profile,"records":state.nodes});
            }
            operation_charge(
                BudgetCharge {
                    bytes: encode(&result).len(),
                    values: 1,
                    ..Default::default()
                },
                failure,
            )?;
            if encode(&result).len() > 8 * 1024 * 1024 {
                record_failure(failure, ExecutionFailure::ByteBudget);
                return Ok(
                    json!({"data":null,"errors":[{"message":"query output budget exceeded","extensions":{"code":"LIMIT_EXCEEDED"}}]}),
                );
            }
            if result
                .get("errors")
                .is_some_and(|v| v.as_array().is_some_and(Vec::is_empty))
            {
                result.as_object_mut().unwrap().remove("errors");
            }
            Ok(result)
        })()
    }));
    let context = Arc::try_unwrap(state)
        .map_err(|_| Error("query context retained after execution".into()))?
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // If cleanup fails, retire the connection instead of returning an unchecked
    // query-only/progress state to the persistent owner.
    context.store.db.progress_handler(0, None::<fn() -> bool>)?;
    context
        .store
        .db
        .pragma_update(None, "query_only", query_only)?;
    *slot = Some(context.store);
    match outcome {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[test]
    fn completion_handles_nullable_list_items_without_schema_assumptions() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("nullable-items.db");
        crate::migration::initialize(&path).unwrap();
        let document = "{alias:item(id:\"x\"){id}}";
        let ast = parse_query(document).unwrap();
        let inputs = Arc::new(crate::graphql_runtime_input::RuntimeInputs::new(
            &ast,
            &json!({}),
            None,
            false,
            document,
        ));
        let state = Arc::new(Mutex::new(Context {
            failure: Arc::new(Mutex::new(None)),
            store: Store::open(&path, true, false).unwrap(),
            profile: "default".into(),
            deadline: Instant::now() + Duration::from_secs(5),
            nodes: 0,
            fields: 0,
            bytes: 0,
            aborted: None,
            authority: None,
            runtime_inputs: inputs.clone(),
            executed: false,
        }));
        let mut execution = Execution {
            state: &state,
            inputs: &inputs,
            http: false,
            errors: vec![],
        };
        let field = inputs
            .fields(inputs.operation(None), "Query")
            .unwrap()
            .remove(0);
        let mut path = vec![async_graphql::PathSegment::Field("alias".into())];
        let values = json!([{"id":"x"},null,{"id":null}]);
        let result = execution
            .complete(
                values.clone(),
                &Type::new("[Item]").unwrap(),
                "Query",
                &field,
                &mut path,
            )
            .unwrap();
        assert_eq!(result, json!([{"id":"x"},null,null]));
        assert_eq!(execution.errors.len(), 1);
        assert_eq!(
            serde_json::to_value(&execution.errors[0]).unwrap()["path"],
            json!(["alias", 2, "id"])
        );
        let error = execution
            .complete(
                values,
                &Type::new("[Item!]").unwrap(),
                "Query",
                &field,
                &mut path,
            )
            .unwrap_err();
        assert_eq!(
            serde_json::to_value(error).unwrap()["path"],
            json!(["alias", 1])
        );
        assert_eq!(path.len(), 1);
    }
    #[test]
    fn early_query_response_honors_expired_deadline_and_outer_interrupt() {
        let ordinary = json!({"data":null,"errors":[{"message":"early query error"}]});
        let expired = crate::embedding::with_interrupt_check(
            || false,
            || {
                Ok(early_query_response(
                    ordinary.clone(),
                    Instant::now() - Duration::from_secs(1),
                ))
            },
        )
        .unwrap();
        assert_eq!(
            expired,
            json!({"data":null,"errors":[{"message":"query execution deadline exceeded","extensions":{"code":"LIMIT_EXCEEDED"}}]})
        );
        let interrupted = crate::embedding::with_interrupt_check(
            || true,
            || {
                Ok(early_query_response(
                    ordinary.clone(),
                    Instant::now() + Duration::from_secs(60),
                ))
            },
        )
        .unwrap();
        assert_eq!(
            interrupted,
            json!({"data":null,"errors":[{"message":"query execution interrupted"}]})
        );
        let result = crate::embedding::with_interrupt_check(
            || false,
            || {
                Ok(early_query_response(
                    ordinary.clone(),
                    Instant::now() + Duration::from_secs(60),
                ))
            },
        )
        .unwrap();
        assert_eq!(result, ordinary);
    }

    #[test]
    fn persistent_graphql_restores_same_owner_after_executor_unwind() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("query-unwind.db");
        crate::migration::initialize(&path).unwrap();
        let mut slot = Some(Store::open(&path, true, false).unwrap());
        slot.as_ref()
            .unwrap()
            .db
            .execute_batch("BEGIN IMMEDIATE; INSERT INTO profiles VALUES ('lent')")
            .unwrap();
        PANIC_AFTER_SETUP.with(|flag| flag.set(true));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            execute_session(
                &mut slot,
                None,
                "{profile}",
                "default",
                json!({}),
                None,
                5000,
            )
        }));
        assert!(result.is_err());
        let store = slot.as_ref().expect("GraphQL restored the exact owner");
        assert!(store.owns_writer_lock());
        assert!(!store.db.is_autocommit());
        assert!(
            !store
                .db
                .pragma_query_value(None, "query_only", |row| row.get::<_, bool>(0))
                .unwrap()
        );
        store
            .db
            .execute_batch("ROLLBACK; INSERT INTO profiles VALUES ('after')")
            .unwrap();
        assert_eq!(
            execute_session(
                &mut slot,
                None,
                "{profile}",
                "default",
                json!({}),
                None,
                5000
            )
            .unwrap()["data"]["profile"],
            "default"
        );
    }

    use crate::{access::Access, selection::Evaluation};
    use rusqlite::params;

    pub(crate) fn fixture(grant: Value) -> (tempfile::TempDir, Access) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.sqlite3");
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        store.transaction(|db| {
            db.execute_batch(r#"
                INSERT INTO locations VALUES ('media');
                INSERT INTO files VALUES ('fa','media','private-a.bin'),('fb','media','private-b.bin'),('fh','media','hidden.bin');
                INSERT INTO items VALUES ('a','movie','{"title":"Allowed A","year":2026,"secret":"DO_NOT_DISCLOSE"}'),('b','movie','{"title":"Allowed B","year":2025,"secret":"DO_NOT_DISCLOSE"}'),('hidden','movie','{"title":"Hidden title"}');
                INSERT INTO item_files(id,file_id,item_id,role,origin) VALUES ('aa','fa','a','primary','explicit'),('ab','fb','b','primary','explicit'),('ah','fh','hidden','primary','explicit'),('cross','fh','a','secondary','explicit');
                INSERT INTO item_relationships(id,source_id,target_id,kind) VALUES ('ra','a','b','related'),('rh','a','hidden','related');
                INSERT INTO identities VALUES ('test','visible','a'),('test','hidden','hidden');
                INSERT INTO catalogs VALUES ('allowed'),('hidden');
                INSERT INTO mappings(id,catalog,file_id,item_id,path) VALUES ('ma','allowed','fa','a','a.bin'),('mh','hidden','fh','hidden','hidden.bin');
                INSERT INTO media_components VALUES ('ca','audio'),('ch','audio');
                INSERT INTO component_occurrences(id,component_id,profile,file_id,association_id,item_id,revision,snapshot,association_evidence,item_evidence,storage,locator) VALUES ('oa','ca','default','fa','aa','a','r','{}','{"metadata":"{}"}','{}','external','{}'),('oh','ch','default','fh','ah','hidden','r','{}','{"metadata":"{}"}','{}','external','{}');
                INSERT INTO api_principals(id,profile) VALUES ('test','default');
            "#)?;
            db.execute("INSERT INTO api_grants(id,principal,definition) VALUES ('grant','test',?)", [encode(&grant)])?;
            db.execute("INSERT INTO api_tokens(id,principal,digest,grants,expires) VALUES ('token','test',?,'[\"grant\"]',?)", params![catabolic_core::sha256("token.secret"),crate::notifications::now()+1000.0])?;
            for (id, definition) in [
                ("selection", json!({"mode":"selection","selection":{"language":"graphql","query":"query($after:String) { items(first:1,after:$after) { nodes { id } pageInfo { endCursor hasNextPage } } }"}})),
                ("document", json!({"mode":"document","selection":{"language":"graphql","query":"{ items { nodes { id metadata } } }"}})),
                ("report", json!({"mode":"rows","selection":{"language":"sql","query":"SELECT item_id FROM catalog_items ORDER BY item_id"}})),
                ("unapproved", json!({"mode":"rows","selection":{"language":"sql","query":"SELECT item_id FROM catalog_items ORDER BY item_id"}})),
            ] {
                db.execute("INSERT INTO saved_queries(id,profile,name,revision,definition,digest) VALUES (?,'default',?,1,?,?)",params![id,id,encode(&definition),id])?;
            }
            db.execute_batch(r#"
                INSERT INTO fallback_policies VALUES ('policy','default','test',1,'{"fallbacks":[{"name":"main"}]}','policy');
                INSERT INTO fallback_bindings(profile,catalog,policy_id,query_id,layout) VALUES ('default','allowed','policy','selection','{}');
                INSERT INTO fallback_entries(id,profile,catalog,item_id,role,policy_id,file_id,state,generation,published_generation) VALUES ('0hidden','default','allowed','hidden','primary','policy','fh','ready',1,1),('1hidden-file','default','allowed','a','primary','policy','fh','ready',1,1),('2allowed','default','allowed','a','primary','policy','fa','ready',1,1),('3allowed-null','default','allowed','b','primary','policy',NULL,'unavailable',1,1);
            "#)?;
            Ok(())
        }).unwrap();
        (root, Access::authenticate(store, "token.secret").unwrap())
    }
    pub(crate) fn limited() -> Value {
        json!({"actions":["metadata:read","component:read"],"item_ids":["a","b"],"file_ids":["fa","fb"],"metadata_fields":["title","year"],"projection_ids":["allowed"],"occurrence_ids":["oa"],"fallback_policy_ids":["policy"],"report_ids":["report"]})
    }
    fn admitted(mut access: Access) -> (Store, Authority) {
        let authority = crate::api_catalog::install(&mut access).unwrap();
        (access.into_store(), authority)
    }
    fn query(
        store: Store,
        authority: &Authority,
        document: &str,
        variables: Value,
    ) -> (Store, Value) {
        execute_authorized_store(store, authority.clone(), document, variables, None, 5000).unwrap()
    }
    fn no_errors(result: &Value) {
        assert!(result.get("errors").is_none(), "{result}");
    }
    #[test]
    fn http_redaction_search_and_nested_joins_are_admitted_first() {
        let (_root, access) = fixture(limited());
        let (store, authority) = admitted(access);
        let (store, result) = query(
            store,
            &authority,
            r#"{ items { nodes { id title metadata associations { nodes { id file { id } } } relationships { nodes { id target { id } } } } pageInfo { hasNextPage } } files { nodes { id } } item(identity:"test=hidden") { id } }"#,
            json!({}),
        );
        no_errors(&result);
        assert_eq!(
            result["data"]["items"]["nodes"].as_array().unwrap().len(),
            2
        );
        assert_eq!(
            result["data"]["files"]["nodes"].as_array().unwrap().len(),
            2
        );
        assert!(result["data"]["item"].is_null());
        for denied in ["DO_NOT_DISCLOSE", "hidden", "cross", "private-a"] {
            assert!(!encode(&result).contains(denied), "{result}");
        }
        let (_, result) = query(
            store,
            &authority,
            r#"{ items(search:"DO_NOT_DISCLOSE") { nodes { id } } hidden:items(metadata:["secret=\"DO_NOT_DISCLOSE\""]) { nodes { id } } }"#,
            json!({}),
        );
        no_errors(&result);
        assert_eq!(result["data"]["items"]["nodes"], json!([]));
        assert_eq!(result["data"]["hidden"]["nodes"], json!([]));
    }
    #[test]
    fn http_fields_and_evidence_filters_fail_before_disclosure() {
        for document in [
            "{ files { nodes { id sourcePath } } }",
            "{ item(id:\"a\") { identities { namespace } } }",
            "{ items(tags:[\"private\"]) { nodes { id } } }",
            "{ items(hasRendition:{}) { nodes { id } } }",
            "{ items(descendants:true) { nodes { id } } }",
            "{ contentSearch(text:\"private\") }",
            "{ fallbackPolicy(id:\"not-granted\") { id } }",
        ] {
            let (_root, access) = fixture(limited());
            let (store, authority) = admitted(access);
            let (_, result) = query(store, &authority, document, json!({}));
            assert_eq!(
                result["errors"][0]["extensions"]["code"], "FORBIDDEN",
                "{result}"
            );
            assert!(!encode(&result).contains("private-a.bin"));
        }
    }
    #[test]
    fn http_signed_cursors_are_scope_and_authority_bound() {
        let (_root, access) = fixture(limited());
        let (store, authority) = admitted(access);
        let document = "query($after:String){ items(first:1,after:$after) { nodes { id } pageInfo { endCursor hasNextPage } } }";
        let (store, first) = query(store, &authority, document, json!({}));
        no_errors(&first);
        let cursor = first["data"]["items"]["pageInfo"]["endCursor"]
            .as_str()
            .unwrap()
            .to_owned();
        let (store, second) = query(store, &authority, document, json!({"after":cursor}));
        no_errors(&second);
        assert_eq!(second["data"]["items"]["nodes"][0]["id"], "b");
        let (store, wrong) = query(
            store,
            &authority,
            "query($after:String){ files(first:1,after:$after) { nodes { id } } }",
            json!({"after":cursor}),
        );
        assert_eq!(wrong["errors"][0]["message"], "invalid_cursor");
        assert!(wrong["errors"][0].get("extensions").is_none());
        let inner = authority
            .after(&json!(["Query", "items", null]), Some(&cursor))
            .unwrap();
        let (store, unsigned) = query(store, &authority, document, json!({"after":inner}));
        assert_eq!(unsigned["errors"][0]["message"], "invalid_cursor");
        let mut tampered = cursor.into_bytes();
        tampered[0] = if tampered[0] == b'A' { b'B' } else { b'A' };
        let (_, bad) = query(
            store,
            &authority,
            document,
            json!({"after":String::from_utf8(tampered).unwrap()}),
        );
        assert_eq!(bad["errors"][0]["message"], "invalid_cursor");
    }
    #[test]
    fn http_nonnull_private_fields_bubble_through_lists_and_aliases() {
        for document in [
            "{componentOccurrences{nodes{technical provenance}}}",
            "{alias:componentOccurrences{nodes{id private:technical}}}",
            "{alias:componentOccurrences{nodes{...Private}}} fragment Private on ComponentOccurrence {id technical}",
        ] {
            let (_root, access) = fixture(limited());
            let (store, authority) = admitted(access);
            let (_, result) = query(store, &authority, document, json!({}));
            let key = if document.contains("alias:") {
                "alias"
            } else {
                "componentOccurrences"
            };
            assert_eq!(result["data"], json!({key:Value::Null}), "{result}");
            assert_eq!(result["errors"].as_array().unwrap().len(), 1, "{result}");
            assert_eq!(result["errors"][0]["extensions"]["code"], "FORBIDDEN");
            assert_eq!(result["errors"][0]["path"][0], key);
            assert_eq!(result["errors"][0]["path"][2], 0);
        }
    }
    #[test]
    fn http_components_and_published_resolutions_filter_before_limit() {
        let (_root, access) = fixture(limited());
        let (store, authority) = admitted(access);
        let (store, result) = query(
            store,
            &authority,
            "{ components(first:1) { nodes { id itemIds } pageInfo { hasNextPage } } componentOccurrences(first:1) { nodes { id fileId itemId } pageInfo { hasNextPage } } fallbackPolicy(id:\"policy\") { id tiers } fallbackResolutions(catalog:\"allowed\",first:1) { nodes { id fileId } pageInfo { endCursor hasNextPage } } }",
            json!({}),
        );
        no_errors(&result);
        assert_eq!(
            result["data"]["components"]["nodes"],
            json!([{"id":"ca","itemIds":["a"]}])
        );
        assert_eq!(
            result["data"]["components"]["pageInfo"]["hasNextPage"],
            false
        );
        assert_eq!(
            result["data"]["componentOccurrences"]["nodes"][0]["id"],
            "oa"
        );
        assert_eq!(
            result["data"]["componentOccurrences"]["pageInfo"]["hasNextPage"],
            false
        );
        assert_eq!(
            result["data"]["fallbackResolutions"]["nodes"][0]["id"],
            "2allowed"
        );
        let cursor = result["data"]["fallbackResolutions"]["pageInfo"]["endCursor"].clone();
        let (store, next) = query(
            store,
            &authority,
            "query($after:String){fallbackResolutions(catalog:\"allowed\",first:1,after:$after){nodes{id fileId} pageInfo{hasNextPage}}}",
            json!({"after":cursor}),
        );
        no_errors(&next);
        assert_eq!(
            next["data"]["fallbackResolutions"]["nodes"][0]["id"],
            "3allowed-null"
        );
        let (_, denied) = query(
            store,
            &authority,
            "{fallbackResolutions(catalog:\"hidden\"){nodes{id}}}",
            json!({}),
        );
        assert_eq!(denied["errors"][0]["message"], "not_found");
    }
    #[test]
    fn http_operator_schema_omits_local_inbox_and_selects_operation() {
        let (_root, access) = fixture(json!({"actions":["*"],"operator":true}));
        let (store, authority) = admitted(access);
        let (store, result) = execute_authorized_store(
            store,
            authority.clone(),
            "query A { profile } query B { schemaVersion }",
            json!({}),
            Some("B"),
            5000,
        )
        .unwrap();
        no_errors(&result);
        assert_eq!(result["data"], json!({"schemaVersion":32}));
        let (store, result) = query(
            store,
            &authority,
            "{__type(name:\"Query\"){fields{name}}}",
            json!({}),
        );
        no_errors(&result);
        assert!(!encode(&result).contains("workInbox"));
        let (store, denied) = query(store, &authority, "{workInbox{nodes}}", json!({}));
        assert_eq!(
            denied["errors"][0]["message"],
            "Cannot query field 'workInbox' on type 'Query'. Did you mean 'worklog'?"
        );
        let (_, local) = execute_store(
            store,
            "{__type(name:\"Query\"){fields{name}}}",
            "default",
            json!({}),
            None,
            5000,
        )
        .unwrap();
        no_errors(&local);
        assert!(encode(&local).contains("workInbox"));
    }
    #[test]
    fn http_evaluation_requires_snapshot_authority_and_preserves_bootstrap_denial() {
        let (_root, access) = fixture(limited());
        let (store, authority) = admitted(access);
        let other = Store::open(&store.path, false, false).unwrap();
        assert!(
            execute_authorized_store(other, authority.clone(), "{profile}", json!({}), None, 5000)
                .is_err()
        );
        let mut evaluation = Evaluation::authorized(store, authority, 5000, 10000).unwrap();
        let selected = evaluation.select(&json!({"query_id":"selection"})).unwrap();
        assert_eq!(selected.ids, BTreeSet::from(["a".into(), "b".into()]));
        let document = evaluation.run("document", 100).unwrap();
        no_errors(&document);
        assert!(!encode(&document).contains("secret"));
        assert!(
            evaluation
                .select(&json!({"language":"sql","query":"SELECT item_id FROM catalog_items"}))
                .is_err()
        );
        assert!(evaluation.run("unapproved", 100).is_err());
        let report = evaluation.run("report", 100).unwrap();
        assert_eq!(report["rows"], json!([["a"], ["b"], ["hidden"]]));
        assert!(
            evaluation
                .select(&json!({"query_id":"selection","profile":"other"}))
                .is_err()
        );
        let store = evaluation.into_store();
        let mut bootstrap = Evaluation::new(store, "default", 5000, 10000, true).unwrap();
        assert_eq!(
            bootstrap.run("document", 100).unwrap_err().0,
            "GraphQL HTTP evaluation requires authorization context"
        );
    }
    #[test]
    fn http_authority_rejects_same_connection_snapshot_changes() {
        let (_root, access) = fixture(limited());
        let (store, authority) = admitted(access);
        store.db.pragma_update(None, "query_only", false).unwrap();
        store
            .db
            .execute("UPDATE main.items SET metadata='{}' WHERE id='a'", [])
            .unwrap();
        assert!(
            execute_authorized_store(
                store,
                authority,
                "{items{nodes{id}}}",
                json!({}),
                None,
                5000
            )
            .is_err()
        );
    }
    #[test]
    fn http_cursor_replay_by_another_principal_is_rejected() {
        let (root, access) = fixture(limited());
        let (store, authority) = admitted(access);
        let document = "query($after:String){items(first:1,after:$after){nodes{id} pageInfo{endCursor hasNextPage}}}";
        let (store, result) = query(store, &authority, document, json!({}));
        let cursor = result["data"]["items"]["pageInfo"]["endCursor"].clone();
        drop(store);
        let store = Store::open(&root.path().join("catalog.sqlite3"), true, false).unwrap();
        store.transaction(|db| {
            db.execute("INSERT INTO api_principals(id,profile) VALUES ('other','default')", [])?;
            db.execute("INSERT INTO api_grants(id,principal,definition) VALUES ('other-grant','other',?)", [encode(&limited())])?;
            db.execute("INSERT INTO api_tokens(id,principal,digest,grants,expires) VALUES ('other-token','other',?,'[\"other-grant\"]',?)", params![catabolic_core::sha256("other-token.secret"),crate::notifications::now()+1000.0])?;
            Ok(())
        }).unwrap();
        let (store, other) = admitted(Access::authenticate(store, "other-token.secret").unwrap());
        let (_, result) = query(store, &other, document, json!({"after":cursor}));
        assert_eq!(result["errors"][0]["message"], "invalid_cursor");
    }
}
