//! Scoped directed relationships using the existing authoritative active edges.
//! Each edge needs explicit kind authority at both endpoints. Historical alias
//! traversal is not inferred; recursive approved reports see only these edges.
use super::*;

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct TrustedRelationshipRead {
    pub(super) by_kind: BTreeMap<String, BTreeSet<String>>,
}
impl TrustedRelationshipRead {
    pub fn new(kinds: Vec<String>, metadata_fields: Vec<String>) -> Result<Self> {
        if kinds.is_empty() || kinds.len() > 32 {
            return Err(fail(ScopeErrorCode::InvalidGrant));
        }
        crate::access_admin::validate_host_metadata_fields(&metadata_fields)
            .map_err(|_| fail(ScopeErrorCode::InvalidGrant))?;
        let fields: BTreeSet<String> = metadata_fields.into_iter().collect();
        let bytes = fields.iter().map(|field| field.len() + 4).sum::<usize>();
        if kinds
            .iter()
            .map(String::len)
            .sum::<usize>()
            .saturating_add(bytes.saturating_mul(kinds.len()))
            > MAX_BYTES
        {
            return Err(fail(ScopeErrorCode::ByteBudget));
        }
        let mut by_kind = BTreeMap::new();
        for kind in kinds {
            crate::curation::validate_relationship_read_kind(&kind)
                .map_err(|_| fail(ScopeErrorCode::InvalidGrant))?;
            by_kind.insert(kind, fields.clone());
        }
        Ok(Self { by_kind })
    }
}
pub(super) fn materialize(
    store: &Store,
    fields: &BTreeMap<String, MemberVisibility>,
    started: Instant,
    budget: ReadBudget,
) -> Result<()> {
    if !fields
        .values()
        .any(|mask| !mask.relationships.by_kind.is_empty())
    {
        return Ok(());
    }
    let db = &store.db;
    db.execute_batch("DROP VIEW IF EXISTS temp.catalog_relationships; CREATE TEMP TABLE item_relationships(id TEXT PRIMARY KEY,source_id TEXT NOT NULL,target_id TEXT NOT NULL,kind TEXT NOT NULL,position INTEGER,metadata TEXT NOT NULL,active INTEGER NOT NULL);CREATE TEMP VIEW catalog_relationships AS SELECT r.id AS relationship_id,r.source_id,s.kind AS source_kind,catalog_title(s.metadata) AS source_title,r.target_id,t.kind AS target_kind,catalog_title(t.metadata) AS target_title,r.kind,r.position,r.metadata,r.active FROM temp.item_relationships r JOIN temp.items s ON s.id=r.source_id JOIN temp.items t ON t.id=r.target_id;")?;
    let mut count = 0usize;
    let mut bytes = 0usize;
    for (source, source_mask) in fields {
        if source_mask.relationships.by_kind.is_empty() {
            continue;
        }
        remaining(started, budget)?;
        // Both endpoints must be live; an ordinary graph edge does not redirect
        // a historical ID or transfer a retired source's grants to a survivor.
        if !db
            .prepare("SELECT 1 FROM main.catalog_live_items WHERE id=?")?
            .exists([source])?
        {
            continue;
        }
        let kinds = encode(&json!(
            source_mask.relationships.by_kind.keys().collect::<Vec<_>>()
        ));
        shared_charge(BudgetCharge {
            bytes: kinds.len(),
            values: 1,
            work: 1,
            ..Default::default()
        })?;
        let mut statement=db.prepare("SELECT r.id,r.target_id,r.kind,r.position,length(CAST(r.metadata AS BLOB)),length(CAST(r.id AS BLOB)),length(CAST(r.target_id AS BLOB)),length(CAST(r.kind AS BLOB)) FROM main.item_relationships r JOIN main.catalog_live_items t ON t.id=r.target_id WHERE r.source_id=? AND r.active=1 AND r.kind IN (SELECT value FROM json_each(?)) ORDER BY r.id COLLATE BINARY")?;
        let mut rows = statement.query(params![source, kinds])?;
        while let Some(row) = rows.next()? {
            remaining(started, budget)?;
            let lengths = [row.get::<_, i64>(5)?, row.get(6)?, row.get(7)?];
            if lengths.iter().any(|n| !(1..=1024).contains(n)) {
                return Err(fail(ScopeErrorCode::ByteBudget));
            }
            shared_charge(BudgetCharge {
                bytes: lengths.iter().sum::<i64>() as usize,
                values: 3,
                work: 1,
                ..Default::default()
            })?;
            let target: String = row.get(1)?;
            let kind: String = row.get(2)?;
            let Some(target_fields) = fields
                .get(&target)
                .and_then(|mask| mask.relationships.by_kind.get(&kind))
            else {
                continue;
            };
            let source_fields = source_mask
                .relationships
                .by_kind
                .get(&kind)
                .ok_or_else(|| fail(ScopeErrorCode::Catalog))?;
            let id: String = row.get(0)?;
            let position: Option<i64> = row.get(3)?;
            count = count.saturating_add(1);
            if count > 100000 {
                return Err(fail(ScopeErrorCode::IdBudget));
            }
            let allowed = source_fields
                .intersection(target_fields)
                .collect::<BTreeSet<_>>();
            let metadata = if allowed.is_empty() {
                json!({})
            } else {
                let length: i64 = row.get(4)?;
                if !(0..=MAX_DEFINITION_BYTES as i64).contains(&length) {
                    return Err(fail(ScopeErrorCode::ByteBudget));
                }
                bytes = bytes.saturating_add(length as usize);
                if bytes > MAX_BYTES {
                    return Err(fail(ScopeErrorCode::ByteBudget));
                }
                shared_charge(BudgetCharge {
                    bytes: length as usize,
                    values: 1,
                    work: 1,
                    ..Default::default()
                })?;
                let raw: String = db.query_row(
                    "SELECT metadata FROM main.item_relationships WHERE id=?",
                    [&id],
                    |r| r.get(0),
                )?;
                let raw: Value = catabolic_core::decode_json(&raw)?;
                let object = raw
                    .as_object()
                    .ok_or_else(|| fail(ScopeErrorCode::Catalog))?;
                Value::Object(
                    allowed
                        .into_iter()
                        .filter_map(|key| object.get(key).map(|value| (key.clone(), value.clone())))
                        .collect(),
                )
            };
            let metadata = encode(&metadata);
            shared_charge(BudgetCharge {
                bytes: metadata.len() + id.len() + source.len() + target.len() + kind.len(),
                items: 1,
                values: 1,
                work: 1,
            })?;
            db.execute(
                "INSERT INTO temp.item_relationships VALUES(?,?,?,?,?,?,1)",
                params![id, source, target, kind, position, metadata],
            )?;
        }
    }
    Ok(())
}

pub(super) struct PreparedPage {
    item: String,
    position: Option<ItemCursorPosition>,
    limit: usize,
}
impl PreparedPage {
    pub(super) fn membership(&self) -> Option<&str> {
        self.position.as_ref().map(|p| p.membership.as_str())
    }
}
fn cursor_scope(cap: &Capability, item: &str, limit: usize) -> Value {
    json!({"contract_version":1,"surface":"native_scoped_relationships","order":"id_binary","capability":cap.capability_id,"item":item,"limit":limit})
}
pub(super) fn prepare(
    cap: &Capability,
    item: String,
    cursor: Option<String>,
    limit: usize,
) -> Result<PreparedPage> {
    if item.is_empty() || item.len() > 1024 || !(1..=1000).contains(&limit) {
        return Err(fail(ScopeErrorCode::InvalidRequest));
    }
    let position = match cursor {
        None => None,
        Some(cursor) => {
            if cursor.is_empty() || cursor.len() > MAX_CURSOR_BYTES {
                return Err(fail(ScopeErrorCode::InvalidCursor));
            }
            shared_charge(BudgetCharge {
                bytes: cursor.len(),
                values: 1,
                work: cursor.len() / 64 + 1,
                ..Default::default()
            })?;
            let raw = crate::api_catalog::authenticated_cursor_after(
                &cap.cursor_key,
                &cap.fingerprint,
                &cursor_scope(cap, &item, limit),
                Some(&cursor),
            )
            .map_err(|_| fail(ScopeErrorCode::InvalidCursor))?;
            let position: ItemCursorPosition =
                serde_json::from_str(&raw).map_err(|_| fail(ScopeErrorCode::InvalidCursor))?;
            if position.after.is_empty()
                || position.after.len() > 1024
                || position.membership.len() != 64
                || !position
                    .membership
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(fail(ScopeErrorCode::InvalidCursor));
            }
            Some(position)
        }
    };
    Ok(PreparedPage {
        item,
        position,
        limit,
    })
}
fn execute(
    store: &Store,
    cap: &Capability,
    statement: &str,
    bound: &str,
    maximum_rows: usize,
    started: Instant,
    budget: ReadBudget,
) -> Result<Value> {
    let failure = Cell::new(None);
    let result = sql::execute_materialized_scope(
        store,
        statement,
        &sql::Options {
            profile: &cap.profile,
            params: Some(bound),
            max_rows: maximum_rows,
            timeout_ms: remaining(started, budget)?,
            stable: true,
            http: true,
            ..Default::default()
        },
        &failure,
    );
    let code = match failure.get() {
        Some(sql::ExecutionFailure::RowBudget) => Some(ScopeErrorCode::IdBudget),
        Some(sql::ExecutionFailure::WorkBudget) => Some(ScopeErrorCode::WorkBudget),
        Some(sql::ExecutionFailure::ByteBudget) => Some(ScopeErrorCode::ByteBudget),
        Some(sql::ExecutionFailure::Timeout) => Some(ScopeErrorCode::Timeout),
        Some(sql::ExecutionFailure::Cancelled) => Some(ScopeErrorCode::Cancelled),
        None => None,
    };
    if let Some(code) = code {
        return Err(fail(code));
    }
    result.map_err(|_| fail(ScopeErrorCode::QueryRejected))
}
pub(super) fn read(
    store: &Store,
    cap: &Capability,
    request: PreparedPage,
    started: Instant,
    budget: ReadBudget,
) -> Result<Value> {
    if cap
        .members
        .get(&request.item)
        .is_none_or(|mask| mask.relationships.by_kind.is_empty())
        || !store
            .db
            .prepare("SELECT 1 FROM temp.items WHERE id=?")?
            .exists([&request.item])?
    {
        return Err(fail(ScopeErrorCode::NotFoundOrNotVisible));
    }
    let params = encode(
        &json!({"item":request.item,"after":request.position.as_ref().map(|p|&p.after),"limit":request.limit+1}),
    );
    let mut output = execute(
        store,
        cap,
        "SELECT relationship_id,source_id,source_kind,source_title,target_id,target_kind,target_title,kind,position,metadata,active FROM temp.catalog_relationships WHERE (source_id=:item OR target_id=:item) AND (:after IS NULL OR relationship_id COLLATE BINARY > :after) ORDER BY relationship_id COLLATE BINARY LIMIT :limit",
        &params,
        request.limit + 1,
        started,
        budget,
    )?;
    let count_params = encode(&json!({"item":request.item}));
    let total = execute(
        store,
        cap,
        "SELECT count(*) FROM temp.item_relationships WHERE source_id=:item OR target_id=:item",
        &count_params,
        1,
        started,
        budget,
    )?["rows"][0][0]
        .as_i64()
        .ok_or_else(|| fail(ScopeErrorCode::Catalog))?;
    let rows = output["rows"]
        .as_array_mut()
        .ok_or_else(|| fail(ScopeErrorCode::Catalog))?;
    let more = rows.len() > request.limit;
    if more {
        rows.pop();
    }
    let cursor = if more {
        let after = rows
            .last()
            .and_then(|row| row[0].as_str())
            .ok_or_else(|| fail(ScopeErrorCode::Catalog))?;
        let position = encode(&json!({"after":after,"membership":cap.membership_digest}));
        let cursor = crate::api_catalog::authenticated_cursor(
            &cap.cursor_key,
            &cap.fingerprint,
            &cursor_scope(cap, &request.item, request.limit),
            &position,
        )
        .map_err(|_| fail(ScopeErrorCode::Catalog))?;
        if cursor.len() > MAX_CURSOR_BYTES {
            return Err(fail(ScopeErrorCode::ByteBudget));
        }
        Some(cursor)
    } else {
        None
    };
    let count = rows.len();
    output["row_count"] = json!(count);
    output["max_rows"] = json!(request.limit);
    let value = json!({"scope":cap.fingerprint,"result":output,"scoped_total":total,"has_more":more,"next_cursor":cursor});
    let bytes = encode(&value).len();
    if bytes > MAX_BYTES {
        return Err(fail(ScopeErrorCode::ByteBudget));
    }
    shared_charge(BudgetCharge {
        bytes,
        values: 1,
        ..Default::default()
    })?;
    remaining(started, budget)?;
    Ok(value)
}
