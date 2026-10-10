//! Catalog entities, validated filters and context-bound keyset pagination.
use crate::{
    Error, Result, Store,
    migration::{reference, rows},
    sql::{casefold, install_functions, scalar},
};
use base64::{Engine, engine::general_purpose::URL_SAFE};
use catabolic_core::{encode, sha256};
use rusqlite::types::Value as SqlValue;
use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;

fn get(args: &Value, key: &str, default: Value) -> Value {
    args.get(key).cloned().unwrap_or(default)
}
fn text(args: &Value, key: &str, default: &str) -> Result<String> {
    get(args, key, json!(default))
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Error(format!("{key} must be text")))
}
fn boolean(args: &Value, key: &str) -> Result<bool> {
    get(args, key, json!(false))
        .as_bool()
        .ok_or_else(|| Error(format!("{key} must be boolean")))
}
pub fn decode(row: &mut Value, keys: &[&str]) -> Result<()> {
    for key in keys {
        if let Some(raw) = row[*key].as_str() {
            row[*key] = serde_json::from_str(raw)?;
        }
    }
    Ok(())
}
pub struct CatalogQuery<'a> {
    pub store: &'a Store,
    pub profile: &'a str,
}
impl<'a> CatalogQuery<'a> {
    pub fn new(store: &'a Store, profile: &'a str) -> Result<Self> {
        install_functions(&store.db)?;
        if !store
            .db
            .prepare("SELECT 1 FROM profiles WHERE id=?")?
            .exists([profile])?
        {
            return Err(Error(format!("unknown profile: {profile}")));
        }
        Ok(Self { store, profile })
    }
    pub fn exists(&self, table: &str, value: &Value) -> Result<()> {
        if !value.is_null()
            && !self
                .store
                .db
                .prepare(&format!("SELECT id FROM {table} WHERE id=?"))?
                .exists([scalar(value)?])?
        {
            return Err(Error(format!(
                "unknown {table}: {}",
                value.as_str().unwrap_or("")
            )));
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub fn page(
        &self,
        key: &str,
        select: &str,
        source: &str,
        conditions: &[String],
        parameters: Vec<SqlValue>,
        sorts: &[(&str, &str)],
        args: &Value,
        context: Value,
    ) -> Result<Value> {
        let limit = get(args, "limit", json!(100))
            .as_i64()
            .filter(|n| (1..=1000).contains(n))
            .ok_or_else(|| Error("limit must be between 1 and 1000".into()))?;
        let sort = text(args, "sort", sorts[0].0)?;
        let expression = sorts
            .iter()
            .find(|(k, _)| *k == sort)
            .ok_or_else(|| {
                Error(format!(
                    "sort must be one of: {}",
                    sorts.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", ")
                ))
            })?
            .1;
        let descending = boolean(args, "descending")?;
        let fingerprint = sha256(encode(&json!([
            self.store.database_id,
            self.profile,
            key,
            context,
            sort,
            descending
        ])));
        let mut values = parameters;
        let mut after = String::new();
        let direction = if descending { "DESC" } else { "ASC" };
        let comparison = if descending { "<" } else { ">" };
        if let Some(cursor) = args.get("cursor").filter(|v| !v.is_null()) {
            let invalid = || Error("invalid cursor or cursor belongs to another query".into());
            let cursor = cursor
                .as_str()
                .filter(|s| s.len() <= 16384)
                .ok_or_else(invalid)?;
            let token: Value =
                serde_json::from_slice(&URL_SAFE.decode(cursor).map_err(|_| invalid())?)
                    .map_err(|_| invalid())?;
            if token.as_array().is_none_or(|a| a.len() != 4)
                || token[0] != 1
                || token[1] != fingerprint
                || !(token[2].is_string() || token[2].is_i64())
                || !token[3].is_string()
            {
                return Err(invalid());
            }
            after = format!("WHERE (_sort {comparison} ? OR (_sort = ? AND id {comparison} ?))");
            values.extend([scalar(&token[2])?, scalar(&token[2])?, scalar(&token[3])?]);
        }
        values.push(SqlValue::Integer(limit + 1));
        let where_sql = if conditions.is_empty() {
            "1".into()
        } else {
            conditions.join(" AND ")
        };
        let mut records = rows(
            &self.store.db,
            &format!(
                "WITH matches AS (SELECT {select},{expression} AS _sort FROM {source} WHERE {where_sql}) SELECT * FROM matches {after} ORDER BY _sort {direction},id {direction} LIMIT ?"
            ),
            &values,
        )?;
        let more = records.len() > limit as usize;
        records.truncate(limit as usize);
        let cursor = if more {
            let last = records.last().unwrap();
            json!(URL_SAFE.encode(encode(&json!([1, fingerprint, last["_sort"], last["id"]]))))
        } else {
            Value::Null
        };
        for row in &mut records {
            row.as_object_mut().unwrap().remove("_sort");
        }
        Ok(json!({key:records,"next_cursor":cursor}))
    }
    fn item_filters(
        &self,
        args: &Value,
        search: bool,
        metadata: bool,
    ) -> Result<(Vec<String>, Vec<SqlValue>)> {
        let mut clauses = vec![];
        let mut values = vec![];
        if search && let Some(v) = args.get("search").filter(|v| !v.is_null()) {
            clauses.push("contains_text(title_key(i.metadata), ?)".into());
            values.push(scalar(v)?);
        }
        if let Some(v) = args.get("kind").filter(|v| !v.is_null()) {
            vocabulary(v, "media kind")?;
            clauses.push("i.kind=?".into());
            values.push(scalar(v)?);
        }
        if let Some(v) = args.get("year").filter(|v| !v.is_null()) {
            if v.as_i64().is_none_or(|n| !(1..=9999).contains(&n)) {
                return Err(Error("year must be between 1 and 9999".into()));
            }
            clauses.push("year_key(i.metadata)=?".into());
            values.push(scalar(v)?);
        }
        if let Some(v) = args.get("identity").filter(|v| !v.is_null()) {
            clauses.push(
                "i.id IN (SELECT x.item_id FROM identities x WHERE x.namespace=? AND x.value=?)"
                    .into(),
            );
            values.extend(identity(v)?);
        }
        if metadata && let Some(expressions) = args.get("metadata").filter(|v| !v.is_null()) {
            for expression in expressions
                .as_array()
                .ok_or_else(|| Error("metadata filters must be an array".into()))?
            {
                let invalid = || Error("metadata filter must be KEY=JSON_VALUE".into());
                let (key, raw) = expression
                    .as_str()
                    .and_then(|s| s.split_once('='))
                    .filter(|(k, _)| !k.is_empty())
                    .ok_or_else(invalid)?;
                let expected: Value = serde_json::from_str(raw).map_err(|_| invalid())?;
                clauses.push("metadata_matches(i.metadata,?,?)".into());
                values.extend([
                    SqlValue::Text(key.into()),
                    SqlValue::Text(encode(&expected)),
                ]);
            }
        }
        Ok((clauses, values))
    }
    fn tag(&self, value: &Value) -> Result<SqlValue> {
        let name = value.as_str().ok_or_else(|| {
            Error("tag name must be text without control characters, at most 255 characters".into())
        })?;
        if name.trim().is_empty()
            || name.chars().count() > 255
            || name.chars().any(char::is_control)
        {
            return Err(Error(
                "tag name must be text without control characters, at most 255 characters".into(),
            ));
        }
        let mut normalized = casefold(name.trim()).nfc().collect::<String>();
        if let Some((namespace, label)) = normalized.split_once(':') {
            let namespace = namespace.trim();
            let label = label.trim();
            if label.is_empty()
                || namespace.is_empty()
                || !namespace.chars().enumerate().all(|(i, c)| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || i > 0 && "_.-".contains(c)
                })
            {
                return Err(Error("namespaced tags require namespace:value; namespace uses letters, digits, dots, underscores or hyphens".into()));
            }
            normalized = format!("{namespace}:{label}");
        }
        let id = self
            .store
            .db
            .query_row(
                "SELECT tag_id FROM tag_names WHERE name=?",
                [normalized],
                |r| r.get::<_, String>(0),
            )
            .map_err(|_| Error(format!("unknown tag: {name}; create it with tag put")))?;
        Ok(SqlValue::Text(id))
    }
    fn tag_filters(
        &self,
        args: &Value,
        subject: &str,
        clauses: &mut Vec<String>,
        values: &mut Vec<SqlValue>,
    ) -> Result<()> {
        let groups = ["tags", "any_tags", "not_tags"].map(|k| get(args, k, json!([])));
        if groups
            .iter()
            .map(|g| g.as_array().map_or(0, Vec::len))
            .sum::<usize>()
            > 100
        {
            return Err(Error("at most 100 tag filters are allowed".into()));
        }
        let alias = if subject == "item" { "i" } else { "f" };
        for (index, group) in groups.iter().enumerate() {
            let empty = vec![];
            let names = if group.is_null() {
                &empty
            } else {
                group
                    .as_array()
                    .ok_or_else(|| Error("tag filters must be an array".into()))?
            };
            let selections = if index == 0 {
                names.iter().map(|v| vec![v]).collect::<Vec<_>>()
            } else if names.is_empty() {
                vec![]
            } else {
                vec![names.iter().collect()]
            };
            for selection in selections {
                let mut ids = selection
                    .into_iter()
                    .map(|v| self.tag(v))
                    .collect::<Result<Vec<_>>>()?;
                ids.sort_by_key(|v| format!("{v:?}"));
                ids.dedup();
                let marks = vec!["?"; ids.len()].join(",");
                let mut selected = format!("SELECT id FROM tags WHERE id IN ({marks})");
                if boolean(args, "descendants")? {
                    selected = format!(
                        "WITH RECURSIVE selected(id) AS ({selected} UNION SELECT p.child_id FROM tag_parents p JOIN selected s ON s.id=p.parent_id) SELECT id FROM selected"
                    );
                }
                clauses.push(format!("{alias}.id {}IN (SELECT a.{subject}_id FROM {subject}_tags a WHERE a.active=1 AND a.tag_id IN ({selected}))",if index==2 {"NOT "} else {""}));
                values.extend(ids);
            }
        }
        Ok(())
    }
    pub fn decorate_items(&self, records: &mut [Value]) -> Result<()> {
        let summary = view_sql("catalog_item_workflow");
        for batch in records.chunks_mut(400) {
            let ids: Vec<SqlValue> = batch
                .iter()
                .map(|row| scalar(&row["id"]))
                .collect::<Result<_>>()?;
            let placeholders = vec!["?"; ids.len()].join(",");
            let mut identities = std::collections::BTreeMap::<String, Vec<Value>>::new();
            for mut identity in rows(
                &self.store.db,
                &format!(
                    "SELECT item_id,namespace,value FROM identities WHERE item_id IN ({placeholders}) ORDER BY item_id,namespace,value"
                ),
                &ids,
            )? {
                let key = identity["item_id"].as_str().unwrap().to_string();
                identity.as_object_mut().unwrap().remove("item_id");
                identities.entry(key).or_default().push(identity);
            }
            let mut parameters = vec![SqlValue::Text(self.profile.into())];
            parameters.extend(ids);
            let mut workflows = std::collections::BTreeMap::new();
            for mut workflow in rows(
                &self.store.db,
                &format!(
                    "SELECT * FROM ({summary}) WHERE profile=? AND item_id IN ({placeholders})"
                ),
                &parameters,
            )? {
                let key = workflow["item_id"].as_str().unwrap().to_string();
                workflow.as_object_mut().unwrap().remove("item_id");
                workflows.insert(key, workflow);
            }
            for row in batch {
                decode(row, &["metadata"])?;
                let key = row["id"].as_str().unwrap().to_string();
                row["identities"] = json!(identities.remove(&key).unwrap_or_default());
                row["workflow"] = workflows.remove(&key).unwrap_or(Value::Null);
            }
        }
        Ok(())
    }
    pub fn execute(&self, key: &str, args: &Value) -> Result<Value> {
        if !args.is_object() {
            return Err(Error("catalog query options must be an object".into()));
        }
        match key {
            "status" => {
                let mut counts = serde_json::Map::new();
                for table in [
                    "locations",
                    "catalogs",
                    "files",
                    "items",
                    "mappings",
                    "journal",
                ] {
                    counts.insert(
                        table.into(),
                        json!(self.store.db.query_row(
                            &format!("SELECT count(*) FROM {table}"),
                            [],
                            |r| r.get::<_, i64>(0)
                        )?),
                    );
                }
                Ok(
                    json!({"database":self.store.path,"database_id":self.store.database_id,"profile":self.profile,"pending_operations":rows(&self.store.db,"SELECT * FROM journal WHERE profile=? ORDER BY rowid",&[self.profile.to_owned().into()])?,"counts":counts,"bindings":rows(&self.store.db,"SELECT * FROM bindings WHERE profile=? ORDER BY kind,owner",&[self.profile.to_owned().into()])?}),
                )
            }
            "items" => self.items(args),
            "files" => self.files(args),
            "mappings" | "associations" | "relationships" => self.relations(key, args),
            "tags" => self.tags(args),
            "taggings" => self.taggings(args),
            "item" => self.item(args),
            _ => Err(Error(format!("unknown catalog query: {key}"))),
        }
    }
    fn items(&self, args: &Value) -> Result<Value> {
        self.exists("catalogs", &args["catalog"])?;
        let (mut clauses, mut values) = self.item_filters(args, true, true)?;
        if let Some(status) = args.get("curation_status").filter(|v| !v.is_null()) {
            if ![
                "pending",
                "in_progress",
                "complete",
                "deferred",
                "ignored",
                "needs_attention",
            ]
            .contains(&status.as_str().unwrap_or(""))
            {
                return Err(Error("invalid curation status".into()));
            }
            clauses.push(format!(
                "i.id IN (SELECT item_id FROM ({}) WHERE profile=? AND status=?)",
                view_sql("catalog_item_workflow")
            ));
            values.extend([SqlValue::Text(self.profile.into()), scalar(status)?]);
        }
        for (key, negate) in [("has_rendition", false), ("missing_rendition", true)] {
            if let Some(wanted) = args.get(key).filter(|v| !v.is_null()) {
                let (extra, params) = rendition_conditions(wanted, "r")?;
                clauses.push(format!(
                    "{}EXISTS (SELECT 1 FROM ({}) r WHERE r.profile=? AND r.source_item_id=i.id{})",
                    if negate { "NOT " } else { "" },
                    view_sql("catalog_rendition_state"),
                    if extra.is_empty() {
                        String::new()
                    } else {
                        format!(" AND {}", extra.join(" AND "))
                    }
                ));
                values.push(SqlValue::Text(self.profile.into()));
                values.extend(params);
            }
        }
        if !args["catalog"].is_null() {
            clauses.push(
                "i.id IN (SELECT m.item_id FROM mappings m WHERE m.catalog=? AND m.active=1)"
                    .into(),
            );
            values.push(scalar(&args["catalog"])?);
        }
        self.tag_filters(args, "item", &mut clauses, &mut values)?;
        let context = json!([
            args["search"],
            args["kind"],
            args["year"],
            args["identity"],
            get(args, "metadata", json!([])),
            args["catalog"],
            args["curation_status"],
            args["has_rendition"],
            args["missing_rendition"],
            get(args, "tags", json!([])),
            get(args, "any_tags", json!([])),
            get(args, "not_tags", json!([])),
            boolean(args, "descendants")?
        ]);
        let mut result = self.page(
            "items",
            "i.*",
            "items i",
            &clauses,
            values,
            &[
                ("id", "i.id"),
                ("title", "title_key(i.metadata)"),
                ("year", "year_key(i.metadata)"),
            ],
            args,
            context,
        )?;
        self.decorate_items(result["items"].as_array_mut().unwrap())?;
        Ok(result)
    }
    fn files(&self, args: &Value) -> Result<Value> {
        let catalog = get(args, "catalog", json!("global"));
        self.exists("catalogs", &catalog)?;
        self.exists("locations", &args["location"])?;
        self.exists("items", &args["item"])?;
        let mut clauses = vec![];
        let mut values = vec![SqlValue::Text(self.profile.into())];
        for (key, expression) in [
            ("search", "contains_text(f.path, ?)"),
            ("location", "f.location=?"),
        ] {
            if !args[key].is_null() {
                clauses.push(expression.into());
                values.push(scalar(&args[key])?);
            }
        }
        if !args["status"].is_null() {
            if !["present", "missing", "unknown"].contains(&args["status"].as_str().unwrap_or("")) {
                return Err(Error("status must be present, missing, or unknown".into()));
            }
            clauses.push("coalesce(o.status,'unknown')=?".into());
            values.push(scalar(&args["status"])?);
        }
        if boolean(args, "unmapped")? {
            clauses.push(
                "f.id NOT IN (SELECT m.file_id FROM mappings m WHERE m.catalog=? AND m.active=1)"
                    .into(),
            );
            values.push(scalar(&catalog)?);
        }
        if boolean(args, "unidentified")? {
            clauses
                .push("f.id NOT IN (SELECT a.file_id FROM item_files a WHERE a.active=1)".into());
        }
        let (mut item_clauses, mut item_values) = self.item_filters(args, false, false)?;
        if !args["item"].is_null() {
            item_clauses.push("i.id=?".into());
            item_values.push(scalar(&args["item"])?);
        }
        if !item_clauses.is_empty() {
            clauses.push(format!("f.id IN (SELECT a.file_id FROM item_files a JOIN items i ON i.id=a.item_id WHERE a.active=1 AND {})",item_clauses.join(" AND ")));
            values.extend(item_values);
        }
        self.tag_filters(args, "file", &mut clauses, &mut values)?;
        let context = json!([
            catalog,
            boolean(args, "unmapped")?,
            boolean(args, "unidentified")?,
            args["search"],
            args["location"],
            args["status"],
            args["item"],
            args["kind"],
            args["year"],
            args["identity"],
            get(args, "tags", json!([])),
            get(args, "any_tags", json!([])),
            get(args, "not_tags", json!([])),
            boolean(args, "descendants")?
        ]);
        self.page("files","f.*,o.size,o.mtime_ns,coalesce(o.status,'unknown') AS status,o.scan_id,s.finished_at AS observed_at,coalesce(h.status,'unknown') AS media_header_status,h.reason AS media_header_reason","files f LEFT JOIN observations o ON o.file_id=f.id AND o.profile=? LEFT JOIN scans s ON s.id=o.scan_id LEFT JOIN media_header_checks h ON h.profile=o.profile AND h.file_id=f.id AND h.scan_id=o.scan_id AND o.status='present' AND h.size=o.size AND h.mtime_ns=o.mtime_ns AND h.device=o.device AND h.inode=o.inode",&clauses,values,&[("id","f.id"),("path","f.path"),("size","coalesce(o.size,-1)"),("mtime","coalesce(o.mtime_ns,-1)")],args,context)
    }
    fn relations(&self, key: &str, args: &Value) -> Result<Value> {
        self.exists("items", &args["item"])?;
        if key != "relationships" {
            self.exists("files", &args["file"])?;
        }
        let default_active = if key == "mappings" { "all" } else { "active" };
        let active = text(args, "active", default_active)?;
        if !["all", "active", "disabled"].contains(&active.as_str()) {
            return Err(Error("active must be all, active, or disabled".into()));
        }
        let mut clauses = vec![];
        let mut values = vec![];
        let select;
        let source;
        let sorts;
        let context;
        let mut page_args = args.clone();
        if key == "mappings" {
            let catalog = get(args, "catalog", json!("global"));
            self.exists("catalogs", &catalog)?;
            for (column, value) in [
                ("catalog", &catalog),
                ("item_id", &args["item"]),
                ("file_id", &args["file"]),
            ] {
                if !value.is_null() {
                    clauses.push(format!("m.{column}=?"));
                    values.push(scalar(value)?);
                }
            }
            if !args["search"].is_null() {
                clauses.push("contains_text(m.path, ?)".into());
                values.push(scalar(&args["search"])?);
            }
            if active != "all" {
                clauses.push("m.active=?".into());
                values.push(SqlValue::Integer(i64::from(active == "active")));
            }
            select = "m.*";
            source = "mappings m";
            sorts = vec![("id", "m.id"), ("path", "m.path"), ("catalog", "m.catalog")];
            page_args["sort"] = get(args, "sort", json!("path"));
            context = json!([catalog, args["item"], args["file"], active, args["search"]]);
        } else if key == "associations" {
            if !args["role"].is_null() {
                vocabulary(&args["role"], "file role")?;
            }
            values.extend([
                SqlValue::Text(self.profile.into()),
                SqlValue::Text(self.profile.into()),
            ]);
            for (column, value) in [
                ("item_id", &args["item"]),
                ("file_id", &args["file"]),
                ("role", &args["role"]),
            ] {
                if !value.is_null() {
                    clauses.push(format!("a.{column}=?"));
                    values.push(scalar(value)?);
                }
            }
            if active != "all" {
                clauses.push("a.active=?".into());
                values.push(SqlValue::Integer(i64::from(active == "active")));
            }
            select = "a.*,f.location,f.path AS source_relative_path,b.root AS source_root,CASE WHEN b.root IS NOT NULL THEN rtrim(b.root,'/') || '/' || f.path END AS source_path,o.size,coalesce(o.status,'unknown') AS status";
            source = "item_files a JOIN files f ON f.id=a.file_id LEFT JOIN observations o ON o.file_id=f.id AND o.profile=? LEFT JOIN bindings b ON b.profile=? AND b.kind='source' AND b.owner=f.location";
            sorts = vec![("part", "coalesce(a.part,0)")];
            context = json!([args["item"], args["file"], args["role"], active]);
        } else {
            let direction = text(args, "direction", "both")?;
            if !["both", "incoming", "outgoing"].contains(&direction.as_str()) {
                return Err(Error(
                    "direction must be both, outgoing, or incoming".into(),
                ));
            }
            if !args["kind"].is_null() {
                vocabulary(&args["kind"], "relationship kind")?;
                clauses.push("r.kind=?".into());
                values.push(scalar(&args["kind"])?);
            }
            if !args["item"].is_null() {
                let columns = if direction == "both" {
                    vec!["source_id", "target_id"]
                } else if direction == "outgoing" {
                    vec!["source_id"]
                } else {
                    vec!["target_id"]
                };
                clauses.push(format!(
                    "({})",
                    columns
                        .iter()
                        .map(|c| format!("r.{c}=?"))
                        .collect::<Vec<_>>()
                        .join(" OR ")
                ));
                for _ in columns {
                    values.push(scalar(&args["item"])?);
                }
            }
            if active != "all" {
                clauses.push("r.active=?".into());
                values.push(SqlValue::Integer(i64::from(active == "active")));
            }
            select = "r.*,s.kind AS source_kind,t.kind AS target_kind";
            source = "item_relationships r JOIN items s ON s.id=r.source_id JOIN items t ON t.id=r.target_id";
            sorts = vec![("position", "coalesce(r.position,0)")];
            context = json!([args["item"], direction, args["kind"], active]);
        }
        let mut result = self.page(
            key, select, source, &clauses, values, &sorts, &page_args, context,
        )?;
        if key != "mappings" {
            for row in result[key].as_array_mut().unwrap() {
                decode(row, &["metadata"])?;
            }
        }
        Ok(result)
    }
    fn tags(&self, args: &Value) -> Result<Value> {
        let mut clauses = vec![];
        let mut values = vec![];
        if !args["search"].is_null() {
            clauses.push("EXISTS (SELECT 1 FROM tag_names n WHERE n.tag_id=t.id AND contains_text(n.name,?))".into());
            values.push(scalar(&args["search"])?);
        }
        if let Some(namespace) = args["namespace"].as_str() {
            let prefix = format!("{}:", casefold(namespace.trim()));
            clauses.push("substr(t.name,1,?)=?".into());
            values.extend([
                SqlValue::Integer(prefix.chars().count() as i64),
                SqlValue::Text(prefix),
            ]);
        }
        for (key, column, other) in [
            ("parent", "child_id", "parent_id"),
            ("child", "parent_id", "child_id"),
        ] {
            if !args[key].is_null() {
                clauses.push(format!(
                    "t.id IN (SELECT {column} FROM tag_parents WHERE {other}=?)"
                ));
                values.push(self.tag(&args[key])?);
            }
        }
        let mut result = self.page(
            "tags",
            "t.*",
            "tags t",
            &clauses,
            values,
            &[("name", "t.name")],
            args,
            json!([
                args["search"],
                args["namespace"],
                args["parent"],
                args["child"]
            ]),
        )?;
        for row in result["tags"].as_array_mut().unwrap() {
            let id = scalar(&row["id"])?;
            row["aliases"] = json!(
                rows(
                    &self.store.db,
                    "SELECT name FROM tag_names WHERE tag_id=? ORDER BY name",
                    std::slice::from_ref(&id)
                )?
                .into_iter()
                .filter(|r| r["name"] != row["name"])
                .map(|r| r["name"].clone())
                .collect::<Vec<_>>()
            );
            row["parent_ids"] = json!(
                rows(
                    &self.store.db,
                    "SELECT parent_id FROM tag_parents WHERE child_id=? ORDER BY parent_id",
                    &[id]
                )?
                .into_iter()
                .map(|r| r["parent_id"].clone())
                .collect::<Vec<_>>()
            );
        }
        Ok(result)
    }
    fn taggings(&self, args: &Value) -> Result<Value> {
        if !args["item"].is_null() && !args["file"].is_null() {
            return Err(Error("choose item or file for an assignment query".into()));
        }
        self.exists("items", &args["item"])?;
        self.exists("files", &args["file"])?;
        let active = text(args, "active", "active")?;
        if !["active", "disabled", "all"].contains(&active.as_str()) {
            return Err(Error("active must be active, disabled, or all".into()));
        }
        let mut clauses = vec![];
        let mut values = vec![];
        if !args["tag"].is_null() {
            clauses.push("a.tag_id=?".into());
            values.push(self.tag(&args["tag"])?);
        }
        if active != "all" {
            clauses.push("a.active=?".into());
            values.push(SqlValue::Integer(i64::from(active == "active")));
        }
        if !args["source"].is_null() {
            clauses.push("a.source=?".into());
            values.push(scalar(&args["source"])?);
        }
        for subject in ["item", "file"] {
            if !args[subject].is_null() {
                clauses.push("a.subject_type=? AND a.subject_id=?".into());
                values.extend([SqlValue::Text(subject.into()), scalar(&args[subject])?]);
            }
        }
        let mut result=self.page("taggings","a.*,t.name AS tag_name","(SELECT id,'item' AS subject_type,item_id AS subject_id,tag_id,source,confidence,note,active,created_at,updated_at FROM item_tags UNION ALL SELECT id,'file',file_id,tag_id,source,confidence,note,active,created_at,updated_at FROM file_tags) a JOIN tags t ON t.id=a.tag_id",&clauses,values,&[("id","a.id")],args,json!([args["tag"],args["item"],args["file"],args["source"],active]))?;
        for row in result["taggings"].as_array_mut().unwrap() {
            row["active"] = json!(row["active"].as_i64() != Some(0));
        }
        Ok(result)
    }
    fn item(&self, args: &Value) -> Result<Value> {
        if args["item_id"].is_null() == args["identity"].is_null() {
            return Err(Error("supply either an item ID or --identity".into()));
        }
        let id = if !args["identity"].is_null() {
            rows(
                &self.store.db,
                "SELECT item_id FROM identities WHERE namespace=? AND value=?",
                &identity(&args["identity"])?,
            )?
            .first()
            .map(|r| r["item_id"].clone())
            .ok_or_else(|| {
                Error(format!(
                    "unknown identity: {}",
                    args["identity"].as_str().unwrap()
                ))
            })?
        } else {
            args["item_id"].clone()
        };
        self.exists("items", &id)?;
        self.exists("catalogs", &args["catalog"])?;
        let mut items = rows(
            &self.store.db,
            "SELECT * FROM items WHERE id=?",
            &[scalar(&id)?],
        )?;
        self.decorate_items(&mut items)?;
        let include = boolean(args, "include_disabled")?;
        let mut clauses = vec!["m.item_id=?".into()];
        let mut values = vec![SqlValue::Text(self.profile.into()); 4];
        values.push(scalar(&id)?);
        if !include {
            clauses.push("m.active=1".into());
        }
        if !args["catalog"].is_null() {
            clauses.push("m.catalog=?".into());
            values.push(scalar(&args["catalog"])?);
        }
        let mut result=self.page("occurrences","m.*, f.location, f.path AS source_relative_path, o.size, o.mtime_ns, coalesce(o.status,'unknown') AS status, o.scan_id, s.finished_at AS observed_at, sb.root AS source_root, ob.root AS output_root, ol.target AS recorded_link_target","mappings m JOIN files f ON f.id=m.file_id LEFT JOIN observations o ON o.file_id=f.id AND o.profile=? LEFT JOIN scans s ON s.id=o.scan_id LEFT JOIN bindings sb ON sb.kind='source' AND sb.owner=f.location AND sb.profile=? LEFT JOIN bindings ob ON ob.kind='output' AND ob.owner=m.catalog AND ob.profile=? LEFT JOIN owned_links ol ON ol.catalog=m.catalog AND ol.path=m.path AND ol.profile=?",&clauses,values,&[("id","m.id")],args,json!([id,args["catalog"],include]))?;
        for row in result["occurrences"].as_array_mut().unwrap() {
            for (output, root, path) in [
                ("source_path", "source_root", "source_relative_path"),
                ("output_path", "output_root", "path"),
            ] {
                row[output] = row[root]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(|s| {
                        json!(format!(
                            "{}/{}",
                            s.trim_end_matches('/'),
                            row[path].as_str().unwrap()
                        ))
                    })
                    .unwrap_or(Value::Null);
            }
        }
        result["item"] = items.remove(0);
        result["profile"] = json!(self.profile);
        let related = json!({"item":id,"active":if include {"all"} else {"active"},"limit":get(args,"limit",json!(100))});
        result["file_associations"] = self.relations("associations", &related)?;
        result["relationships"] = self.relations("relationships", &related)?;
        Ok(result)
    }
}
pub fn view_sql(name: &str) -> &'static str {
    reference()["views"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == name)
        .unwrap()["sql"]
        .as_str()
        .unwrap()
}
pub fn identity(value: &Value) -> Result<Vec<SqlValue>> {
    let raw = value
        .as_str()
        .ok_or_else(|| Error("identity must be NAMESPACE=VALUE".into()))?;
    let (namespace, value) = raw
        .split_once('=')
        .filter(|(_, v)| !v.trim().is_empty())
        .ok_or_else(|| Error("identity must be NAMESPACE=VALUE".into()))?;
    Ok(vec![
        SqlValue::Text(namespace.into()),
        SqlValue::Text(value.into()),
    ])
}
fn vocabulary(value: &Value, label: &str) -> Result<()> {
    let raw = value.as_str().unwrap_or("");
    let custom = raw.strip_prefix("custom:").is_some_and(|s| {
        !s.is_empty()
            && s.len() <= 63
            && s.chars().enumerate().all(|(i, c)| {
                c.is_ascii_lowercase() || i > 0 && (c.is_ascii_digit() || "_.-".contains(c))
            })
    });
    let allowed = match label {
        "file role" => [
            "primary",
            "cover",
            "subtitle",
            "transcript",
            "lyrics",
            "thumbnail",
            "extra",
            "source",
        ]
        .contains(&raw),
        "relationship kind" => [
            "part_of",
            "edition_of",
            "recording_of",
            "created_by",
            "performed_by",
            "narrated_by",
            "derived_from",
        ]
        .contains(&raw),
        _ => reference()["media_types"]["kinds"].get(raw).is_some(),
    };
    if !allowed && !custom {
        return Err(Error(format!(
            "unknown {label}: {raw}; use a built-in name or custom:lowercase_name"
        )));
    }
    Ok(())
}
pub fn rendition_conditions(wanted: &Value, alias: &str) -> Result<(Vec<String>, Vec<SqlValue>)> {
    if wanted.as_object().is_none_or(|o| {
        o.keys()
            .any(|k| !["purpose", "recipe", "definition", "current"].contains(&k.as_str()))
    }) {
        return Err(Error(
            "rendition filter accepts purpose, recipe, definition and current".into(),
        ));
    }
    let mut clauses = vec![];
    let mut params = vec![];
    for (key, column) in [
        ("purpose", "purpose"),
        ("recipe", "operation_id"),
        ("definition", "definition_id"),
        ("current", "recorded_current"),
    ] {
        if let Some(value) = wanted.get(key) {
            if key == "current" {
                if !value.is_boolean() {
                    return Err(Error("rendition current must be boolean".into()));
                }
            } else if value.as_str().is_none_or(str::is_empty) {
                return Err(Error(
                    "rendition filter identifiers must be nonempty strings".into(),
                ));
            }
            clauses.push(format!("{alias}.{column}=?"));
            params.push(scalar(value)?);
        }
    }
    Ok((clauses, params))
}

/// ASCII public definition/profile name contract.
pub fn name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
    {
        return Err(Error(
            "names must be 1–64 letters, digits, dots, underscores, or hyphens".into(),
        ));
    }
    Ok(())
}
