//! GraphQL transport over the same native snapshot and catalog query functions.
use crate::{
    Error, Result, Store,
    migration::{SCHEMA_VERSION, reference, rows},
    query::{CatalogQuery, decode, rendition_conditions, view_sql},
    sql::scalar,
};
use async_graphql::{ErrorExtensions, Name, Request, Value as GValue, Variables, dynamic::*};
use async_graphql_parser::{
    parse_query, parse_schema,
    types::{BaseType, Selection, SelectionSet, Type, TypeKind, TypeSystemDefinition},
};
use catabolic_core::encode;
use rusqlite::types::Value as SqlValue;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

const SDL: &str = include_str!("../resources/schema.graphql");
struct Budget;
impl async_graphql::extensions::ExtensionFactory for Budget {
    fn create(&self) -> Arc<dyn async_graphql::extensions::Extension> {
        Arc::new(Budget)
    }
}
#[async_trait::async_trait]
impl async_graphql::extensions::Extension for Budget {
    async fn resolve(
        &self,
        ctx: &async_graphql::extensions::ExtensionContext<'_>,
        info: async_graphql::extensions::ResolveInfo<'_>,
        next: async_graphql::extensions::NextResolve<'_>,
    ) -> async_graphql::ServerResult<Option<GValue>> {
        if info.parent_type.starts_with("__") || ["__schema", "__type"].contains(&info.name) {
            return next.run(ctx, info).await;
        }
        let state = ctx
            .data::<Arc<Mutex<Context>>>()
            .map_err(|e| e.into_server_error(async_graphql::Pos::default()))?;
        {
            let mut context = state.lock().expect("query context");
            context.fields += 1;
            if context.fields > 20000 {
                context.aborted = Some("query field budget exceeded".into());
            }
            context.check().map_err(|e| {
                async_graphql::Error::new(e.to_string())
                    .extend_with(|_, extensions| extensions.set("code", "LIMIT_EXCEEDED"))
                    .into_server_error(async_graphql::Pos::default())
            })?;
        }
        let introspection = info.is_for_introspection;
        let result = next.run(ctx, info).await?;
        if introspection && let Some(value) = &result {
            let mut context = state.lock().expect("query context");
            let charged = match value {
                GValue::List(values) => context.records(values.len()),
                GValue::String(_) => {
                    context.charge(&serde_json::to_value(value).expect("GraphQL value JSON"))
                }
                _ => context.check(),
            };
            charged.map_err(|e| {
                async_graphql::Error::new(e.to_string())
                    .extend_with(|_, extensions| extensions.set("code", "LIMIT_EXCEEDED"))
                    .into_server_error(async_graphql::Pos::default())
            })?;
        }
        Ok(result)
    }
}
struct Context {
    store: Store,
    profile: String,
    deadline: Instant,
    nodes: usize,
    fields: usize,
    bytes: usize,
    aborted: Option<String>,
    undefined: Vec<String>,
}
impl Context {
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
            "SELECT * FROM file_facts WHERE profile=? AND file_id=? AND operation=?",
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
        .map(|row| serde_json::from_str::<Value>(row["value"].as_str().unwrap()))
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
            .map(|r| serde_json::from_str::<Value>(r["definition"].as_str().unwrap()))
            .transpose()?
            .unwrap_or(Value::Null);
        let rendition = rows(
            &self.store.db,
            "SELECT definition FROM rendition_policies WHERE profile=? AND catalog=?",
            &[SqlValue::Text(self.profile.clone()), scalar(catalog)?],
        )?;
        result["rendition_policy"] = rendition
            .first()
            .map(|r| serde_json::from_str::<Value>(r["definition"].as_str().unwrap()))
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
        if Instant::now() > self.deadline {
            return Err(Error("query execution deadline exceeded".into()));
        }
        if let Some(error) = &self.aborted {
            return Err(Error(error.clone()));
        }
        Ok(())
    }
    fn charge(&mut self, value: &Value) -> Result<()> {
        if !value.is_null() {
            self.bytes += encode(value).len();
        }
        if self.bytes > 8 * 1024 * 1024 {
            self.aborted = Some("query output budget exceeded".into());
        }
        self.check()
    }
    fn records(&mut self, count: usize) -> Result<()> {
        self.nodes += count;
        if self.nodes > 10000 {
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
                    table = format!("({}) a", view_sql("catalog_workflow_checks"));
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
                        table = format!("({}) a", view_sql("catalog_rendition_state"));
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
                    table = format!(
                        "({}) a",
                        view_sql(if field == "components" {
                            "catalog_components"
                        } else {
                            "catalog_component_occurrences"
                        })
                    );
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
                    table = format!("({}) a", view_sql("catalog_work_inbox"));
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
                        format!("({})", view_sql("catalog_renditions"))
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
                    let first = args["first"]
                        .as_i64()
                        .filter(|n| (1..=1000).contains(n))
                        .ok_or_else(|| {
                            Error("resolution page size must be between 1 and 1000".into())
                        })?;
                    let mut records = rows(
                        &self.store.db,
                        "SELECT e.id,e.item_id,e.file_id,e.revision,e.policy_id,e.state,e.tier,e.generation,e.published_generation FROM fallback_entries e WHERE e.profile=? AND e.catalog=? AND e.active=1 AND e.id>? ORDER BY e.id LIMIT ?",
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
fn output(
    value: Value,
    ty: &Type,
    enums: &BTreeSet<String>,
    objects: &BTreeSet<String>,
) -> async_graphql::Result<FieldValue<'static>> {
    if value.is_null() {
        return Ok(FieldValue::NULL);
    }
    match &ty.base {
        BaseType::List(inner) => Ok(FieldValue::list(
            value
                .as_array()
                .ok_or_else(|| async_graphql::Error::new("expected list result"))?
                .iter()
                .cloned()
                .map(|v| output(v, inner, enums, objects))
                .collect::<async_graphql::Result<Vec<_>>>()?,
        )),
        BaseType::Named(name) if objects.contains(name.as_str()) => {
            Ok(FieldValue::owned_any(value))
        }
        BaseType::Named(name) if enums.contains(name.as_str()) => {
            Ok(FieldValue::value(GValue::Enum(Name::new(
                value
                    .as_str()
                    .ok_or_else(|| async_graphql::Error::new("expected enum result"))?,
            ))))
        }
        BaseType::Named(name) if name.as_str() == "BigInt" => {
            Ok(FieldValue::value(GValue::String(
                value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string()),
            )))
        }
        _ => Ok(FieldValue::value(GValue::from_json(value)?)),
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
fn schema() -> Result<&'static Schema> {
    static SCHEMA: OnceLock<std::result::Result<Schema, String>> = OnceLock::new();
    SCHEMA
        .get_or_init(|| build_schema().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| Error(e.clone()))
}
fn build_schema() -> Result<Schema> {
    let document = parse_schema(SDL).map_err(|e| Error(e.to_string()))?;
    let mut object_fields: BTreeMap<
        String,
        Vec<async_graphql_parser::Positioned<async_graphql_parser::types::FieldDefinition>>,
    > = BTreeMap::new();
    let mut enums = BTreeSet::new();
    let mut objects = BTreeSet::new();
    let mut descriptions = BTreeMap::new();
    let mut definitions = vec![];
    for definition in document.definitions {
        if let TypeSystemDefinition::Type(definition) = definition {
            let definition = definition.node;
            if let Some(description) = &definition.description {
                descriptions.insert(definition.name.node.to_string(), description.node.clone());
            }
            match &definition.kind {
                TypeKind::Object(obj) => {
                    objects.insert(definition.name.node.to_string());
                    object_fields
                        .entry(definition.name.node.to_string())
                        .or_default()
                        .extend(obj.fields.clone());
                }
                TypeKind::Enum(_) => {
                    enums.insert(definition.name.node.to_string());
                }
                _ => {}
            }
            definitions.push(definition);
        }
    }
    let enums = Arc::new(enums);
    let objects = Arc::new(objects);
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
            let name = field.name.node.to_string();
            let parent = parent.clone();
            let field_name = name.clone();
            let ty = field.ty.node.clone();
            let captured_enums = enums.clone();
            let captured_objects = objects.clone();
            let captured_defaults: serde_json::Map<String, Value> = field
                .arguments
                .iter()
                .filter_map(|argument| {
                    argument.node.default_value.as_ref().map(|value| {
                        (
                            argument.node.name.node.to_string(),
                            serde_json::to_value(&value.node).expect("constant input"),
                        )
                    })
                })
                .collect();
            let mut dynamic = Field::new(name, type_ref(&ty), move |ctx| {
                let parent = parent.clone();
                let name = field_name.clone();
                let ty = ty.clone();
                let enums = captured_enums.clone();
                let objects = captured_objects.clone();
                let defaults = captured_defaults.clone();
                FieldFuture::new(async move {
                    let state = ctx.data::<Arc<Mutex<Context>>>()?;
                    let mut state = state
                        .lock()
                        .map_err(|_| async_graphql::Error::new("query context poisoned"))?;
                    let source = ctx
                        .parent_value
                        .try_downcast_ref::<Value>()
                        .cloned()
                        .unwrap_or(Value::Null);
                    let mut args = serde_json::Map::new();
                    for (key, v) in ctx.args.iter() {
                        if let Some(argument) = ctx.item.node.get_argument(key.as_str())
                            && let async_graphql_value::Value::Variable(variable) = &argument.node
                            && state.undefined.iter().any(|name| name == variable.as_str())
                        {
                            if let Some(default) = defaults.get(key.as_str()) {
                                args.insert(key.to_string(), default.clone());
                            }
                            continue;
                        }
                        args.insert(key.to_string(), serde_json::to_value(v.as_value())?);
                    }
                    let result =
                        match state.resolve(&parent, &name, &source, &Value::Object(args)) {
                            Ok(result) => result,
                            Err(error) => {
                                let error = async_graphql::Error::new(error.to_string())
                                    .extend_with(|_, extensions| {
                                        extensions.set(
                                            "code",
                                            if state.aborted.is_some()
                                                || Instant::now() > state.deadline
                                            {
                                                "LIMIT_EXCEEDED"
                                            } else {
                                                "QUERY_ERROR"
                                            },
                                        )
                                    });
                                if !ty.nullable {
                                    return Err(error);
                                }
                                ctx.add_error(
                                    ctx.set_error_path(error.into_server_error(ctx.item.pos)),
                                );
                                return Ok(None);
                            }
                        };
                    if result.is_null() {
                        return Ok(None);
                    }
                    Ok(Some(output(result, &ty, &enums, &objects)?))
                })
            });
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
fn scalar_variable_errors(
    ast: &async_graphql_parser::types::ExecutableDocument,
    variables: &Value,
    selected: Option<&str>,
) -> Vec<Value> {
    let mut errors = vec![];
    for (name, operation) in ast.operations.iter() {
        if selected.is_some_and(|selected| name.is_some_and(|name| name.as_str() != selected)) {
            continue;
        }
        for definition in &operation.node.variable_definitions {
            let name = definition.node.name.node.as_str();
            let ty = &definition.node.var_type.node;
            let Some(value) = variables.get(name) else {
                continue;
            };
            let prefix = format!(
                "Variable '${name}' got invalid value {}",
                python_value(value)
            );
            let reason = if value.is_null() && !ty.nullable {
                Some(format!(
                    "Variable '${name}' of non-null type '{ty}' must not be null."
                ))
            } else if value.is_null() {
                None
            } else if let BaseType::Named(type_name) = &ty.base {
                let invalid = match type_name.as_str() {
                    "Int" if value.as_i64().is_none() => Some(format!(
                        "Int cannot represent non-integer value: {}",
                        python_value(value)
                    )),
                    "Int" if value.as_i64().is_some_and(|v| i32::try_from(v).is_err()) => {
                        Some(format!(
                            "Int cannot represent non 32-bit signed integer value: {}",
                            python_value(value)
                        ))
                    }
                    "String" if !value.is_string() => Some(format!(
                        "String cannot represent a non string value: {}",
                        python_value(value)
                    )),
                    "Boolean" if !value.is_boolean() => Some(format!(
                        "Boolean cannot represent a non boolean value: {}",
                        python_value(value)
                    )),
                    "Float" if !value.is_number() => Some(format!(
                        "Float cannot represent non numeric value: {}",
                        python_value(value)
                    )),
                    "ID" if !value.is_string() && !value.is_i64() && !value.is_u64() => Some(
                        format!("ID cannot represent value: {}", python_value(value)),
                    ),
                    _ => None,
                };
                invalid.map(|reason| format!("{prefix}; {reason}"))
            } else {
                None
            };
            if let Some(message) = reason {
                errors.push(json!({"message":message,"locations":[{"line":definition.pos.line,"column":definition.pos.column}]}));
            }
        }
    }
    errors
}
fn python_value(value: &Value) -> String {
    match value {
        Value::String(text) => {
            let quote = if text.contains('\'') && !text.contains('"') {
                '"'
            } else {
                '\''
            };
            let mut out = quote.to_string();
            for c in text.chars() {
                match c {
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if c == quote => {
                        out.push('\\');
                        out.push(c);
                    }
                    c => out.push(c),
                }
            }
            out.push(quote);
            out
        }
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        _ => encode(value),
    }
}
pub fn execute(
    path: &Path,
    document: &str,
    profile: &str,
    variables: Value,
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
    variables: Value,
    operation: Option<&str>,
    timeout_ms: u64,
) -> Result<(Store, Value)> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    if !(1..=60000).contains(&timeout_ms) {
        return Err(Error("timeout-ms must be between 1 and 60000".into()));
    }
    if document.len() > 131072 {
        return Err(Error("query document must be at most 131072 bytes".into()));
    }
    if !variables.is_object() {
        return Err(Error("variables must be a JSON object".into()));
    }
    if encode(&variables).len() > 131072 {
        return Err(Error("variables exceed input budget".into()));
    }
    CatalogQuery::new(&store, profile)?;
    if let Some(error) = crate::document::token_limit(document, 4000) {
        return Ok((store, error));
    }
    let ast = match parse_query(document) {
        Ok(ast) => ast,
        Err(error) => {
            return Ok((store, crate::validation::parse_error(document, &error)));
        }
    };
    if let Err(error) = check_document(&ast) {
        return Ok((
            store,
            json!({"data":null,"errors":[{"message":error.to_string()}]}),
        ));
    }
    if let Some(error) = crate::validation::validate(&ast) {
        return Ok((store, error));
    }
    let query_only: bool = store
        .db
        .pragma_query_value(None, "query_only", |row| row.get(0))?;
    store.db.pragma_update(None, "query_only", true)?;
    store
        .db
        .progress_handler(1000, Some(move || Instant::now() >= deadline))?;
    let state = Arc::new(Mutex::new(Context {
        store,
        profile: profile.into(),
        deadline,
        nodes: 0,
        fields: 0,
        bytes: 0,
        aborted: None,
        undefined: ast
            .operations
            .iter()
            .filter(|(name, _)| {
                operation.is_none_or(|selected| name.is_none_or(|name| name.as_str() == selected))
            })
            .flat_map(|(_, op)| op.node.variable_definitions.iter())
            .filter(|v| {
                v.node.var_type.node.nullable
                    && v.node.default_value.is_none()
                    && variables.get(v.node.name.node.as_str()).is_none()
            })
            .map(|v| v.node.name.node.to_string())
            .collect(),
    }));
    let mut coerced_variables = variables.clone();
    for (_, op) in ast.operations.iter() {
        for definition in &op.node.variable_definitions {
            let key = definition.node.name.node.as_str();
            if let BaseType::Named(name) = &definition.node.var_type.node.base
                && let Some(value) = variables[key]
                    .as_f64()
                    .filter(|v| v.is_finite() && v.fract() == 0.0)
            {
                if name == "Int" && (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&value) {
                    coerced_variables[key] = json!(value as i32);
                }
                if name == "ID" && variables[key].is_number() {
                    coerced_variables[key] = json!(
                        variables[key]
                            .as_i64()
                            .map(|v| v.to_string())
                            .or_else(|| variables[key].as_u64().map(|v| v.to_string()))
                            .unwrap_or_else(|| format!("{value:.0}"))
                    );
                }
            }
        }
    }
    let mut request = Request::new(document)
        .variables(Variables::from_json(coerced_variables))
        .data(state.clone());
    if let Some(operation) = operation {
        request = request.operation_name(operation);
    }
    request.set_parsed_query(ast.clone());
    let response = futures::executor::block_on(schema()?.execute(request));
    let mut result = serde_json::to_value(response)?;
    let mut coercion_error = false;
    if result["errors"][0]["message"] == "Operation name required in request." {
        result["errors"][0]["message"] =
            json!("Must provide operation name if query contains multiple operations.");
        coercion_error = true;
    }
    if result["errors"].as_array().is_some_and(|errors| {
        errors.iter().all(|error| {
            error["message"]
                .as_str()
                .is_some_and(|message| message.starts_with("Invalid value for argument "))
        })
    }) {
        let errors = scalar_variable_errors(&ast, &variables, operation);
        if !errors.is_empty() {
            result["errors"] = json!(errors);
            coercion_error = true;
        }
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
                        crate::introspection::field_suggestions(words[3], words[1])
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
                        error["locations"] =
                            json!([{"line":definition.pos.line,"column":definition.pos.column}]);
                        coercion_error = true;
                    }
                }
            }
        }
    }

    let state_handle = state;
    let mut state = state_handle
        .lock()
        .map_err(|_| Error("query context poisoned".into()))?;
    let result = (|| -> Result<Value> {
        let introspection =
            crate::introspection::apply(&ast, operation, &variables, &mut result, &mut |value| {
                state.fields += 1;
                if state.fields > 20000 {
                    state.aborted = Some("query field budget exceeded".into());
                }
                if let Some(list) = value.as_array() {
                    state.records(list.len())?;
                } else if value.is_string() {
                    state.charge(value)?;
                } else {
                    state.check()?;
                }
                Ok(())
            });
        if let Err(error) = introspection
            && state.aborted.is_none()
        {
            return Err(error);
        }

        if state.aborted.is_some() || Instant::now() > state.deadline {
            return Ok(
                json!({"data":null,"errors":[{"message":state.aborted.as_deref().unwrap_or("query execution deadline exceeded"),"extensions":{"code":"LIMIT_EXCEEDED"}}]}),
            );
        }
        if state.fields > 0 || coercion_error {
            result["extensions"] =
                json!({"interfaceVersion":1,"profile":profile,"records":state.nodes});
        }
        if encode(&result).len() > 8 * 1024 * 1024 {
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
    })();
    state.store.db.progress_handler(0, None::<fn() -> bool>)?;
    state
        .store
        .db
        .pragma_update(None, "query_only", query_only)?;
    drop(state);
    let context = Arc::try_unwrap(state_handle)
        .map_err(|_| Error("query context retained after execution".into()))?
        .into_inner()
        .map_err(|_| Error("query context poisoned".into()))?;
    Ok((context.store, result?))
}
