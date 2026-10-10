//! Complete, bounded ID selections evaluated in one caller-owned SQLite snapshot.
use crate::{Error, Result, Store, graphql, migration::rows, sql};
use async_graphql_parser::{
    parse_query,
    types::{Field, OperationType, Selection},
};
use catabolic_core::encode;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

const ENTITIES: &[&str] = &[
    "item_id",
    "file_id",
    "association_id",
    "component_id",
    "occurrence_id",
];
#[derive(Clone)]
pub struct Selected {
    pub entity: String,
    pub ids: BTreeSet<String>,
    pub report: Value,
}
pub struct Evaluation {
    store: Option<Store>,
    pub profile: String,
    pub http: bool,
    deadline: Instant,
    maximum: usize,
    count: usize,
    nodes: usize,
    result_bytes: usize,
    cache: BTreeMap<String, Selected>,
}
impl Evaluation {
    pub fn new(
        store: Store,
        profile: &str,
        timeout: u64,
        maximum: usize,
        http: bool,
    ) -> Result<Self> {
        crate::query::CatalogQuery::new(&store, profile)?;
        Ok(Self {
            store: Some(store),
            profile: profile.into(),
            http,
            deadline: Instant::now() + Duration::from_millis(timeout),
            maximum,
            count: 0,
            nodes: 0,
            result_bytes: 0,
            cache: BTreeMap::new(),
        })
    }
    pub fn store(&self) -> &Store {
        self.store.as_ref().expect("evaluation owns snapshot")
    }
    fn remaining(&self, message: &str) -> Result<u64> {
        let ms = self
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        if ms < 1 {
            return Err(Error(message.into()));
        }
        Ok(ms)
    }
    fn graphql(
        &mut self,
        query: &str,
        profile: &str,
        variables: Value,
        timeout: u64,
    ) -> Result<Value> {
        if self.http {
            return Err(Error(
                "GraphQL HTTP evaluation requires authorization context".into(),
            ));
        }
        let (store, result) = graphql::execute_store(
            self.store.take().unwrap(),
            query,
            profile,
            variables,
            None,
            timeout,
        )?;
        self.store = Some(store);
        Ok(result)
    }
    pub fn select(&mut self, selection: &Value) -> Result<Selected> {
        validate(selection)?;
        if let Some(id) = selection["query_id"].as_str() {
            let profile = selection["profile"]
                .as_str()
                .unwrap_or("default")
                .to_owned();
            let previous = std::mem::replace(&mut self.profile, profile);
            let result = (|| {
                let definition = self.get(id)?["definition"].clone();
                self.root_budget(&definition);
                let cached = self.cache.contains_key(id);
                let selected = self.saved_select(id, &[])?;
                if !cached {
                    self.account(&selected)?;
                }
                Ok(selected)
            })();
            self.profile = previous;
            return result;
        }
        let profile = selection["profile"].as_str().unwrap_or("default");
        let language = selection["language"].as_str().unwrap();
        let query = selection["query"].as_str().unwrap();
        let timeout = selection["timeout_ms"].as_u64().unwrap_or(5000);
        let deadline = Instant::now()
            + Duration::from_millis(timeout.min(self.remaining("query composition timed out")?));
        let maximum = selection["max_ids"].as_u64().unwrap_or(10000) as usize;
        let mut ids = BTreeSet::new();
        let mut pages = 0;
        let mut returned = 0;
        let entity: String;
        if language == "sql" {
            let paged = selection["page_size"].as_u64();
            let size = paged.unwrap_or(maximum as u64) as usize;
            let mut after: Option<String> = None;
            let mut column = None;
            let params = selection.get("params").cloned().unwrap_or(json!({}));
            let query = query.trim().trim_end_matches(';');
            if paged.is_some()
                && (query.to_lowercase().contains("__catabolic_")
                    || params
                        .as_object()
                        .unwrap()
                        .keys()
                        .any(|k| k.starts_with("__catabolic_")))
            {
                return Err(Error(
                    "__catabolic_ is reserved for selection pagination".into(),
                ));
            }
            loop {
                let remaining = deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis() as u64;
                if remaining < 1 {
                    return Err(Error("selection SQL pagination timed out".into()));
                }
                let mut bound = params.clone();
                let text = if paged.is_some() {
                    bound["__catabolic_after"] = json!(after);
                    format!(
                        "SELECT DISTINCT * FROM ({query}){} ORDER BY 1 COLLATE BINARY LIMIT {}",
                        after
                            .as_ref()
                            .map(|_| format!(
                                " WHERE \"{}\" COLLATE BINARY > :__catabolic_after",
                                column.as_deref().unwrap()
                            ))
                            .unwrap_or_default(),
                        size + 1
                    )
                } else {
                    query.into()
                };
                let bound = encode(&bound);
                let result = sql::execute_store(
                    self.store(),
                    Some(&text),
                    &sql::Options {
                        profile,
                        params: Some(&bound),
                        max_rows: size + usize::from(paged.is_some()),
                        timeout_ms: remaining,
                        stable: true,
                        http: self.http,
                        ..Default::default()
                    },
                )?;
                if result["complete"] != true {
                    return Err(Error(
                        if paged.is_some() {
                            "selection page was truncated; no requirements or mappings were changed"
                        } else {
                            "selection query was truncated; no mappings were changed"
                        }
                        .into(),
                    ));
                }
                let columns = result["columns"].as_array().unwrap();
                if columns.len() != 1 || !ENTITIES.contains(&columns[0].as_str().unwrap_or("")) {
                    return Err(Error("selection SQL must return exactly one column: association_id, item_id, or file_id".into()));
                }
                column = Some(columns[0].as_str().unwrap().to_owned());
                let values = result["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| valid_id(&row[0]))
                    .collect::<Result<Vec<_>>>()?;
                let more = paged.is_some() && values.len() > size;
                let values = &values[..values.len().min(size)];
                returned += values.len();
                pages += 1;
                ids.extend(values.iter().cloned());
                if ids.len() > maximum || more && ids.len() >= maximum {
                    return Err(Error(
                        "selection exceeds max_ids; no requirements or mappings were changed"
                            .into(),
                    ));
                }
                if !more {
                    break;
                }
                let next = values
                    .last()
                    .ok_or_else(|| Error("selection pagination did not advance".into()))?;
                if after.as_ref().is_some_and(|a| next <= a) {
                    return Err(Error("selection pagination did not advance".into()));
                }
                after = Some(next.clone());
            }
            if paged.is_some() {
                returned = ids.len();
            }
            entity = column.unwrap();
        } else {
            let (kind, field) = graphql_contract(query)?;
            entity = kind;
            let mut cursor = Value::Null;
            let mut visited = BTreeSet::new();
            loop {
                let remaining = deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis() as u64;
                if remaining < 1 {
                    return Err(Error("selection GraphQL execution timed out".into()));
                }
                let mut variables = selection.get("variables").cloned().unwrap_or(json!({}));
                variables["after"] = cursor;
                let result = self.graphql(query, profile, variables, remaining)?;
                if let Some(error) = result["errors"].as_array().and_then(|v| v.first()) {
                    return Err(Error(format!(
                        "selection GraphQL failed: {}",
                        error["message"].as_str().unwrap_or("unknown error")
                    )));
                }
                let page = &result["data"][&field];
                let values = page["nodes"]
                    .as_array()
                    .ok_or_else(|| Error("selection GraphQL returned an incomplete page".into()))?
                    .iter()
                    .map(|row| valid_id(&row["id"]))
                    .collect::<Result<Vec<_>>>()?;
                if !page["pageInfo"].is_object() {
                    return Err(Error(
                        "selection GraphQL returned an incomplete page".into(),
                    ));
                }
                returned += values.len();
                pages += 1;
                if returned > maximum || pages > maximum {
                    return Err(Error(
                        "selection GraphQL exceeds the complete selection limit".into(),
                    ));
                }
                ids.extend(values.iter().cloned());
                let info = &page["pageInfo"];
                if info["hasNextPage"] == false {
                    if !info["endCursor"].is_null() {
                        return Err(Error(
                            "selection GraphQL returned inconsistent pagination".into(),
                        ));
                    }
                    break;
                }
                let next = info["endCursor"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| {
                        Error("selection GraphQL returned incomplete or repeated pagination".into())
                    })?;
                if info["hasNextPage"] != true
                    || values.is_empty()
                    || !visited.insert(next.to_owned())
                {
                    return Err(Error(
                        "selection GraphQL returned incomplete or repeated pagination".into(),
                    ));
                }
                cursor = json!(next);
            }
        }
        Ok(Selected {
            report: json!({"language":language,"profile":profile,"entity":entity,"selected_ids":ids.len(),"returned_rows":returned,"pages":pages,"complete":true}),
            entity,
            ids,
        })
    }
    pub fn get(&self, id: &str) -> Result<Value> {
        let records = rows(
            &self.store().db,
            "SELECT * FROM saved_queries WHERE id=? AND profile=?",
            &[id.to_owned().into(), self.profile.clone().into()],
        )?;
        let mut row = records
            .into_iter()
            .next()
            .ok_or_else(|| Error("unknown query revision in this profile".into()))?;
        crate::query::decode(&mut row, &["definition"])?;
        Ok(row)
    }
    fn saved_select(&mut self, id: &str, ancestors: &[String]) -> Result<Selected> {
        if ancestors.iter().any(|a| a == id) || ancestors.len() >= 16 {
            return Err(Error("query composition cycle or depth limit".into()));
        }
        if let Some(result) = self.cache.get(id) {
            return Ok(result.clone());
        }
        self.nodes += 1;
        if self.nodes > 64 {
            return Err(Error("query composition exceeds 64 definitions".into()));
        }
        let remaining = self.remaining("query composition timed out")?;
        let definition = self.get(id)?["definition"].clone();
        if definition["mode"] != "selection" {
            return Err(Error(
                "rule/projection requires a complete ID selection, not rows/document".into(),
            ));
        }
        let mut result = if let Some(combine) = definition["combine"].as_str() {
            let mut path = ancestors.to_vec();
            path.push(id.into());
            let mut children = vec![];
            for child in definition["queries"]
                .as_array()
                .ok_or_else(|| Error("composition requires query revision IDs".into()))?
            {
                children.push(
                    self.saved_select(
                        child
                            .as_str()
                            .ok_or_else(|| Error("invalid query revision ID".into()))?,
                        &path,
                    )?,
                );
            }
            let first = children
                .first()
                .ok_or_else(|| Error("composition requires query revision IDs".into()))?;
            let entity = first.entity.clone();
            let mut ids = first.ids.clone();
            for child in children.iter().skip(1) {
                if child.entity != entity {
                    return Err(Error("composed queries must select the same entity".into()));
                }
                ids = match combine {
                    "union" => ids.union(&child.ids).cloned().collect(),
                    "intersection" => ids.intersection(&child.ids).cloned().collect(),
                    "difference" => ids.difference(&child.ids).cloned().collect(),
                    _ => return Err(Error("invalid query composition".into())),
                };
            }
            let report = json!({"complete":true,"profile":self.profile,"entity":entity,"selected_ids":ids.len(),"language":"composition","components":children.iter().map(|c|json!({"query_id":c.report["query_id"],"selected_ids":c.ids.len()})).collect::<Vec<_>>()});
            Selected {
                entity,
                ids,
                report,
            }
        } else {
            let mut selection = definition["selection"].clone();
            selection["timeout_ms"] = json!(
                remaining
                    .min(definition["timeout_ms"].as_u64().unwrap_or(5000))
                    .min(selection["timeout_ms"].as_u64().unwrap_or(5000))
            );
            let result = self.select(&selection)?;
            self.count += result.ids.len();
            result
        };
        if self.count > self.maximum
            || result.ids.len() > definition["max_ids"].as_u64().unwrap_or(10000) as usize
        {
            return Err(Error(
                "complete query selection exceeds its ID budget".into(),
            ));
        }
        if definition["entity"]
            .as_str()
            .is_some_and(|entity| entity != result.entity)
        {
            return Err(Error(
                "query returned a different entity from its declared contract".into(),
            ));
        }
        self.remaining("query composition timed out")?;
        result.report["query_id"] = json!(id);
        self.cache.insert(id.into(), result.clone());
        Ok(result)
    }
    fn account(&mut self, result: &Selected) -> Result<()> {
        self.result_bytes += encode(&json!([result.entity, result.ids, result.report])).len();
        if self.result_bytes > 4 * 1024 * 1024 {
            return Err(Error("evaluation_result_byte_budget".into()));
        }
        Ok(())
    }
    fn root_budget(&mut self, definition: &Value) {
        self.deadline = self.deadline.min(
            Instant::now()
                + Duration::from_millis(definition["timeout_ms"].as_u64().unwrap_or(5000)),
        );
        self.maximum = self
            .maximum
            .min(definition["max_ids"].as_u64().unwrap_or(10000) as usize);
    }
    pub fn run(&mut self, id: &str, limit: usize) -> Result<Value> {
        if !(1..=1000).contains(&limit) {
            return Err(Error("limit must be between 1 and 1000".into()));
        }
        let definition = self.get(id)?["definition"].clone();
        self.root_budget(&definition);
        let timeout = definition["timeout_ms"]
            .as_u64()
            .unwrap_or(5000)
            .min(self.remaining("query composition timed out")?);
        if definition["mode"] == "selection" {
            let cached = self.cache.contains_key(id);
            let result = self.saved_select(id, &[])?;
            if !cached {
                self.account(&result)?;
            }
            let mut report = result.report;
            report["ids"] = json!(result.ids.iter().take(limit).collect::<Vec<_>>());
            report["details_truncated"] = json!(result.ids.len() > limit);
            report["entity"] = json!(result.entity);
            report["selection_complete"] = json!(true);
            return Ok(report);
        }
        let selection = &definition["selection"];
        if definition["mode"] == "rows" {
            let params = encode(selection.get("params").unwrap_or(&json!({})));
            sql::execute_store(
                self.store(),
                selection["query"].as_str(),
                &sql::Options {
                    profile: &self.profile,
                    params: Some(&params),
                    max_rows: limit,
                    timeout_ms: timeout,
                    http: self.http,
                    ..Default::default()
                },
            )
        } else {
            let profile = self.profile.clone();
            self.graphql(
                selection["query"]
                    .as_str()
                    .ok_or_else(|| Error("invalid query text".into()))?,
                &profile,
                selection.get("variables").cloned().unwrap_or(json!({})),
                timeout,
            )
        }
    }
    pub fn associations(
        &mut self,
        selection: &Value,
        admitted: &[Value],
    ) -> Result<(Vec<Value>, Value)> {
        let result = self.select(selection)?;
        let table = match result.entity.as_str() {
            "item_id" => "items",
            "file_id" => "files",
            "association_id" => "item_files",
            _ => return Err(Error(
                "component selections require package resolution; they are not file associations"
                    .into(),
            )),
        };
        let column = if result.entity == "association_id" {
            "id"
        } else {
            &result.entity
        };
        let values = result.ids.iter().cloned().collect::<Vec<_>>();
        let mut found = BTreeSet::new();
        let mut records = BTreeMap::new();
        for chunk in values.chunks(500) {
            let marks = vec!["?"; chunk.len()].join(",");
            let args = chunk.iter().cloned().map(Into::into).collect::<Vec<_>>();
            for row in rows(
                &self.store().db,
                &format!("SELECT id FROM {table} WHERE id IN ({marks})"),
                &args,
            )? {
                found.insert(row["id"].as_str().unwrap().to_owned());
            }
            for row in rows(
                &self.store().db,
                &format!(
                    "SELECT a.*,f.path,f.location FROM item_files a JOIN files f ON f.id=a.file_id WHERE a.active=1 AND a.{column} IN ({marks})"
                ),
                &args,
            )? {
                records.insert(row["id"].as_str().unwrap().to_owned(), row);
            }
            if records.len() > 100000 {
                return Err(Error(
                    "query selection expands beyond 100000 active associations".into(),
                ));
            }
        }
        for row in admitted {
            if result.ids.contains(row[column].as_str().unwrap_or("")) {
                records.insert(row["id"].as_str().unwrap().to_owned(), row.clone());
            }
        }
        if records.len() > 100000 {
            return Err(Error(
                "query selection expands beyond 100000 associations".into(),
            ));
        }
        if found != result.ids {
            return Err(Error(
                "selection returned unknown IDs; no mappings were changed".into(),
            ));
        }
        if result.entity == "association_id"
            && records.keys().cloned().collect::<BTreeSet<_>>() != result.ids
        {
            return Err(Error(
                "selection returned disabled associations; filter active=1".into(),
            ));
        }
        let mut report = result.report;
        report["selected_associations"] = json!(records.len());
        Ok((records.into_values().collect(), report))
    }
}
fn valid_id(value: &Value) -> Result<String> {
    value
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| Error("selection IDs must be nonempty strings".into()))
}
fn bounded(selection: &Value, key: &str, default: u64, max: u64, message: &str) -> Result<u64> {
    let value = selection
        .get(key)
        .map(Value::as_u64)
        .unwrap_or(Some(default));
    value
        .filter(|v| *v >= 1 && *v <= max)
        .ok_or_else(|| Error(message.into()))
}
pub fn validate(selection: &Value) -> Result<()> {
    let object = selection.as_object().ok_or_else(|| {
        Error(
            "selection supports language, query, params/variables, profile, and timeout_ms".into(),
        )
    })?;
    if object.contains_key("query_id") {
        if object
            .keys()
            .any(|k| !matches!(k.as_str(), "query_id" | "profile"))
            || selection["query_id"].as_str().is_none_or(str::is_empty)
        {
            return Err(Error(
                "selection reference requires a query_id and optional profile".into(),
            ));
        }
        return crate::query::name(selection["profile"].as_str().unwrap_or("default"));
    }
    if object.keys().any(|k| {
        !matches!(
            k.as_str(),
            "language"
                | "query"
                | "params"
                | "variables"
                | "profile"
                | "timeout_ms"
                | "page_size"
                | "max_ids"
        )
    }) {
        return Err(Error(
            "selection supports language, query, params/variables, profile, and timeout_ms".into(),
        ));
    }
    let language = selection["language"].as_str().unwrap_or("");
    if !matches!(language, "sql" | "graphql") {
        return Err(Error("selection language must be sql or graphql".into()));
    }
    let profile = selection
        .get("profile")
        .map(Value::as_str)
        .unwrap_or(Some("default"))
        .ok_or_else(|| Error("selection profile must be a name".into()))?;
    crate::query::name(profile)?;
    let max = bounded(
        selection,
        "max_ids",
        10000,
        100000,
        "selection max_ids must be 1..100000",
    )?;
    if object.contains_key("page_size") {
        bounded(
            selection,
            "page_size",
            100,
            9999,
            "selection page_size must be 1..9999",
        )?;
    }
    if max > 10000 && language == "sql" && !object.contains_key("page_size") {
        return Err(Error("larger SQL selections require page_size".into()));
    }
    bounded(
        selection,
        "timeout_ms",
        5000,
        60000,
        "selection timeout_ms must be between 1 and 60000",
    )?;
    let query = selection["query"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= sql::MAX_SQL_BYTES)
        .ok_or_else(|| Error("selection query must be nonempty and at most 1 MiB".into()))?;
    if language == "sql" {
        if object.contains_key("variables") {
            return Err(Error("SQL selection uses params, not variables".into()));
        }
        let params = selection.get("params").cloned().unwrap_or(json!({}));
        if !params.is_object() {
            return Err(Error("params must be a JSON object".into()));
        }
        for value in params.as_object().unwrap().values() {
            sql::scalar(value)?;
        }
    } else {
        if object.contains_key("params") {
            return Err(Error("GraphQL selection uses variables, not params".into()));
        }
        graphql_contract(query)?;
        let variables = selection.get("variables").cloned().unwrap_or(json!({}));
        if !variables.is_object() || variables.get("after").is_some() {
            return Err(Error(
                "GraphQL variables must be an object; after is reserved for complete pagination"
                    .into(),
            ));
        }
    }
    Ok(())
}
pub fn graphql_contract(query: &str) -> Result<(String, String)> {
    if query.len() > 131072 {
        return Err(Error("GraphQL selection exceeds the document limit".into()));
    }
    let ast = parse_query(query).map_err(|e| Error(format!("invalid GraphQL selection: {e}")))?;
    if ast.operations.iter().count() != 1 || !ast.fragments.is_empty() {
        return Err(Error(
            "GraphQL selection requires one query operation without fragments".into(),
        ));
    }
    let operation = &ast.operations.iter().next().unwrap().1.node;
    if operation.ty != OperationType::Query {
        return Err(Error(
            "only read-only query operations are supported".into(),
        ));
    }
    let roots = &operation.selection_set.node.items;
    let root = if roots.len() == 1 {
        if let Selection::Field(root) = &roots[0].node {
            &root.node
        } else {
            return Err(Error(
                "GraphQL selection must query one items, files, or associations collection".into(),
            ));
        }
    } else {
        return Err(Error(
            "GraphQL selection must query one items, files, or associations collection".into(),
        ));
    };
    let entity = match root.name.node.as_str() {
        "items" => "item_id",
        "files" => "file_id",
        "associations" => "association_id",
        "components" => "component_id",
        "componentOccurrences" => "occurrence_id",
        _ => {
            return Err(Error(
                "GraphQL selection must query one items, files, or associations collection".into(),
            ));
        }
    };
    if !root.arguments.iter().any(|(key, value)| {
        key.node == "after"
            && matches!(&value.node,async_graphql_value::Value::Variable(name) if name=="after")
    }) {
        return Err(Error(
            "GraphQL selection must declare $after: String and pass after: $after".into(),
        ));
    }
    if !root.directives.is_empty() || !operation.directives.is_empty() {
        return Err(Error(
            "GraphQL selection directives are not supported".into(),
        ));
    }
    fn children<'a>(field: &'a Field, names: &[&str]) -> Result<BTreeMap<String, &'a Field>> {
        let mut out = BTreeMap::new();
        for child in &field.selection_set.node.items {
            if let Selection::Field(child) = &child.node {
                let child = &child.node;
                if child.alias.is_some() || !child.directives.is_empty() {
                    return Err(Error("GraphQL selection must return nodes { id } and pageInfo { hasNextPage endCursor }".into()));
                }
                out.insert(child.name.node.to_string(), child);
            } else {
                return Err(Error("GraphQL selection must return nodes { id } and pageInfo { hasNextPage endCursor }".into()));
            }
        }
        if out.len() != names.len()
            || field.selection_set.node.items.len() != names.len()
            || names.iter().any(|n| !out.contains_key(*n))
        {
            return Err(Error(
                "GraphQL selection must return nodes { id } and pageInfo { hasNextPage endCursor }"
                    .into(),
            ));
        }
        Ok(out)
    }
    let fields = children(root, &["nodes", "pageInfo"])?;
    children(fields["nodes"], &["id"])?;
    children(fields["pageInfo"], &["hasNextPage", "endCursor"])?;
    Ok((
        entity.into(),
        root.alias.as_ref().unwrap_or(&root.name).node.to_string(),
    ))
}
