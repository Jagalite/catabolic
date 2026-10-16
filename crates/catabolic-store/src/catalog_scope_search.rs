//! Private scoped FTS owner. No global MATCH, BM25, request-defined authority,
//! or connection getter. Every canonical value is authorized before insertion.
use super::*;
use rusqlite::Connection;
#[path = "catalog_scope_search_rank.rs"]
mod rank;

const MAX_QUERY_BYTES: usize = 4096;
const MAX_VALUE_BYTES: usize = 65_536;
const MAX_PATH_BYTES: usize = 65_536;
const MAX_VALUES: usize = 100_000;
const SEARCH_CONTRACT: u32 = 1;

/// Exact SQLite FTS5 expression bytes. IDs and cursors only narrow an admitted
/// scope; they cannot grant metadata, namespace, tag, or alias-path authority.
pub struct ScopedSearchRequest {
    pub query: String,
    pub cursor: Option<String>,
    pub limit: usize,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Position {
    score: u16,
    item: String,
    membership: String,
}
pub(super) struct Prepared {
    query: String,
    position: Option<Position>,
    limit: usize,
    binding: Value,
}
impl Prepared {
    pub(super) fn has_cursor(&self) -> bool {
        self.position.is_some()
    }
}
pub(super) fn prepare(cap: &Capability, request: ScopedSearchRequest) -> Result<Prepared> {
    if request.query.trim().is_empty()
        || request.query.len() > MAX_QUERY_BYTES
        || !(1..=1000).contains(&request.limit)
    {
        return Err(fail(ScopeErrorCode::InvalidRequest));
    }
    shared_charge(BudgetCharge {
        bytes: request.query.len(),
        values: 1,
        work: request.query.len() / 64 + 1,
        ..Default::default()
    })?;
    let binding = json!({"schema":35,"search_contract":SEARCH_CONTRACT,"rank_contract":rank::RANK_CONTRACT_VERSION,
        "tokenizer":"unicode61 remove_diacritics 2","extractor":"native-search-values-v1",
        "surface":"native_scoped_search","query_digest":sha256(&request.query),"limit":request.limit,
        "capability":cap.capability_id,"order":"score_desc_id_binary"});
    let position = match request.cursor {
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
                &binding,
                Some(&cursor),
            )
            .map_err(|_| fail(ScopeErrorCode::InvalidCursor))?;
            let position: Position =
                serde_json::from_str(&raw).map_err(|_| fail(ScopeErrorCode::InvalidCursor))?;
            if position.item.is_empty()
                || position.item.len() > rank::MAX_ITEM_ID_BYTES
                || position.score == 0
                || position.score > 350
                || position.membership.len() != 64
                || !position
                    .membership
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(fail(ScopeErrorCode::InvalidCursor));
            }
            if position.membership != cap.membership_digest {
                return Err(fail(ScopeErrorCode::StaleCursor));
            }
            Some(position)
        }
    };
    Ok(Prepared {
        query: request.query,
        position,
        limit: request.limit,
        binding,
    })
}

pub(super) struct OwnedVm<'a> {
    db: &'a Connection,
    poisoned: &'a Cell<bool>,
    armed: bool,
}
impl<'a> OwnedVm<'a> {
    // Called only on ScopedReadSession's freshly opened, unescaped connection,
    // after Evaluation has finished and uninstalled its own bounded SQL hook.
    pub(super) fn install(
        db: &'a Connection,
        work: &OperationBudget,
        poisoned: &'a Cell<bool>,
    ) -> Result<Self> {
        work.check()
            .map_err(|cause| fail(evaluation_code(Some(cause))))?;
        let work = work.clone();
        db.progress_handler(
            1,
            Some(move || {
                work.charge(BudgetCharge {
                    work: 1,
                    ..Default::default()
                })
                .is_err()
            }),
        )?;
        Ok(Self {
            db,
            poisoned,
            armed: true,
        })
    }
    pub(super) fn finish(mut self) -> Result<()> {
        self.db
            .progress_handler(0, None::<fn() -> bool>)
            .map_err(|_| {
                self.poisoned.set(true);
                fail(ScopeErrorCode::Catalog)
            })?;
        self.armed = false;
        Ok(())
    }
}
impl Drop for OwnedVm<'_> {
    fn drop(&mut self) {
        if self.armed && self.db.progress_handler(0, None::<fn() -> bool>).is_err() {
            self.poisoned.set(true);
        }
    }
}
fn column(key: &str, class: &str) -> Result<usize> {
    match (key, class) {
        ("metadata:title", "title") => Ok(0),
        ("logical_aliases", "aliases") => Ok(1),
        (key, "accepted_identities")
            if key.strip_prefix("identity:").is_some_and(|n| !n.is_empty()) =>
        {
            Ok(2)
        }
        ("tags", "effective_tags") => Ok(3),
        ("metadata:description", "description") => Ok(4),
        _ => Err(fail(ScopeErrorCode::Catalog)),
    }
}
fn allowed_field(mask: &MemberVisibility, key: &str, column: usize) -> bool {
    match column {
        0 => mask.metadata.contains("title"),
        1 => mask.search.logical_aliases,
        2 => key
            .strip_prefix("identity:")
            .is_some_and(|n| mask.search.identity_namespaces.contains(n)),
        3 => mask.search.effective_tags,
        4 => mask.metadata.contains("description"),
        _ => false,
    }
}
fn source_projection(
    store: &Store,
    fields: &BTreeMap<String, MemberVisibility>,
    started: Instant,
    budget: ReadBudget,
) -> Result<()> {
    let db = &store.db;
    db.pragma_update(None, "query_only", false)?;
    db.execute_batch("CREATE VIRTUAL TABLE temp.native_scope_search USING fts5(item_id UNINDEXED,title,aliases,accepted_identities,effective_tags,description,tokenize='unicode61 remove_diacritics 2')")?;
    let mut count = 0usize;
    let mut bytes = 0usize;
    for (item, mask) in fields {
        remaining(started, budget)?;
        if !db
            .prepare("SELECT 1 FROM main.catalog_live_items WHERE id=?")?
            .exists([item])?
        {
            continue;
        }
        // Read bounded headers first. Denied value text never reaches MATCH or a
        // candidate prefilter, including the negative branch of a NOT query.
        let mut keys = Vec::new();
        for key in ["title", "description"] {
            if mask.metadata.contains(key) {
                keys.push(format!("metadata:{key}"));
            }
        }
        keys.extend(
            mask.search
                .identity_namespaces
                .iter()
                .map(|namespace| format!("identity:{namespace}")),
        );
        if mask.search.effective_tags {
            keys.push("tags".into());
        }
        if mask.search.logical_aliases {
            keys.push("logical_aliases".into());
        }
        let keys = encode(&json!(keys));
        shared_charge(BudgetCharge {
            bytes: keys.len(),
            values: 1,
            work: 1,
            ..Default::default()
        })?;
        let mut query=db.prepare("SELECT field_key,origin_key,rank_class,length(CAST(required_subjects_json AS BLOB)),length(CAST(value_text AS BLOB)),length(CAST(field_key AS BLOB)),length(CAST(origin_key AS BLOB)) FROM main.catalog_search_values WHERE item_id=? AND field_key IN (SELECT value FROM json_each(?)) ORDER BY field_key,origin_key")?;
        let mut rows = query.query(params![item, keys])?;
        let mut columns: [BTreeSet<String>; 5] = Default::default();
        while let Some(row) = rows.next()? {
            remaining(started, budget)?;
            count = count.saturating_add(1);
            if count > MAX_VALUES {
                return Err(fail(ScopeErrorCode::WorkBudget));
            }
            let path_len: i64 = row.get(3)?;
            let text_len: i64 = row.get(4)?;
            let key_len: i64 = row.get(5)?;
            let origin_len: i64 = row.get(6)?;
            if !(0..=MAX_PATH_BYTES as i64).contains(&path_len)
                || !(0..=MAX_VALUE_BYTES as i64).contains(&text_len)
                || !(1..=512).contains(&key_len)
                || !(1..=4096).contains(&origin_len)
            {
                return Err(fail(ScopeErrorCode::ByteBudget));
            }
            shared_charge(BudgetCharge {
                bytes: (key_len + origin_len) as usize,
                values: 1,
                work: 1,
                ..Default::default()
            })?;
            let key: String = row.get(0)?;
            let origin: String = row.get(1)?;
            let class: String = row.get(2)?;
            let col = column(&key, &class)?;
            if !allowed_field(mask, &key, col) {
                continue;
            }
            shared_charge(BudgetCharge {
                bytes: path_len as usize,
                values: 1,
                work: 1,
                ..Default::default()
            })?;
            let path:String=db.query_row("SELECT required_subjects_json FROM main.catalog_search_values WHERE item_id=? AND field_key=? AND origin_key=?",params![item,key,origin],|r|r.get(0))?;
            let path: Vec<String> =
                serde_json::from_str(&path).map_err(|_| fail(ScopeErrorCode::Catalog))?;
            if path.is_empty()
                || path.len() > 65
                || path.last() != Some(item)
                || (col != 1 && path != [item.clone()])
            {
                return Err(fail(ScopeErrorCode::Catalog));
            }
            if !path.iter().all(|id| {
                fields
                    .get(id)
                    .is_some_and(|visible| col != 1 || visible.search.logical_aliases)
            }) {
                continue;
            }
            bytes = bytes.saturating_add(text_len as usize + 1);
            if bytes > MAX_BYTES {
                return Err(fail(ScopeErrorCode::ByteBudget));
            }
            shared_charge(BudgetCharge {
                bytes: text_len as usize + 1,
                values: 1,
                work: 1,
                ..Default::default()
            })?;
            let text:String=db.query_row("SELECT value_text FROM main.catalog_search_values WHERE item_id=? AND field_key=? AND origin_key=?",params![item,key,origin],|r|r.get(0))?;
            columns[col].insert(text);
        }
        let joined = columns.map(|values| values.into_iter().collect::<Vec<_>>().join("\n"));
        // Joining is deterministic; v1 phrases may span adjacent authorized
        // values. There is deliberately no unqualified per-value phrase claim.
        shared_charge(BudgetCharge {
            items: 1,
            bytes: joined.iter().map(String::len).sum(),
            values: 5,
            work: 1,
        })?;
        db.execute(
            "INSERT INTO temp.native_scope_search VALUES(?,?,?,?,?,?)",
            params![item, joined[0], joined[1], joined[2], joined[3], joined[4]],
        )?;
    }
    db.pragma_update(None, "query_only", true)?;
    Ok(())
}
pub(super) fn execute(
    store: &Store,
    cap: &Capability,
    fields: &BTreeMap<String, MemberVisibility>,
    request: Prepared,
    started: Instant,
    budget: ReadBudget,
    poisoned: &Cell<bool>,
) -> Result<Value> {
    if store.schema_version < 35 {
        return Err(fail(ScopeErrorCode::Unsupported));
    }
    let work = OperationBudget::current().ok_or_else(|| fail(ScopeErrorCode::Catalog))?;
    let guard = OwnedVm::install(&store.db, &work, poisoned)?;
    let result = (|| {
        let status = crate::catalog_search_index::status(store)
            .map_err(|_| fail(ScopeErrorCode::Catalog))?;
        if !status.ready || status.contract_version != SEARCH_CONTRACT {
            return Err(fail(ScopeErrorCode::SearchNotReady));
        }
        source_projection(store, fields, started, budget)?;
        let mut statement=store.db.prepare("SELECT item_id,highlight(native_scope_search,1,'<hit>','</hit>')!=title,highlight(native_scope_search,2,'<hit>','</hit>')!=aliases,highlight(native_scope_search,3,'<hit>','</hit>')!=accepted_identities,highlight(native_scope_search,4,'<hit>','</hit>')!=effective_tags,highlight(native_scope_search,5,'<hit>','</hit>')!=description FROM temp.native_scope_search WHERE native_scope_search MATCH ?")?;
        let mut rows = statement
            .query([&request.query])
            .map_err(|_| fail(ScopeErrorCode::QueryRejected))?;
        let mut matches = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|_| fail(ScopeErrorCode::QueryRejected))?
        {
            remaining(started, budget)?;
            if matches.len() >= rank::MAX_MATCHES {
                return Err(fail(ScopeErrorCode::IdBudget));
            }
            let item_id: String = row.get(0)?;
            shared_charge(BudgetCharge {
                items: 1,
                bytes: item_id.len(),
                values: 6,
                work: 1,
            })?;
            matches.push(rank::DocumentMatch {
                item_id,
                hits: rank::FieldHits {
                    title: row.get(1)?,
                    aliases: row.get(2)?,
                    accepted_identities: row.get(3)?,
                    effective_tags: row.get(4)?,
                    description: row.get(5)?,
                },
            });
        }
        remaining(started, budget)?;
        shared_charge(BudgetCharge {
            work: matches.len().saturating_mul(16),
            ..Default::default()
        })?;
        let ranked = rank::rank(matches).map_err(|_| fail(ScopeErrorCode::Catalog))?;
        let total = ranked.len();
        let mut page = ranked
            .into_iter()
            .filter(|hit| {
                request.position.as_ref().is_none_or(|p| {
                    hit.score < p.score || (hit.score == p.score && hit.item_id > p.item)
                })
            })
            .take(request.limit + 1)
            .collect::<Vec<_>>();
        let has_more = page.len() > request.limit;
        if has_more {
            page.pop();
        }
        let cursor = if has_more {
            let last = page.last().ok_or_else(|| fail(ScopeErrorCode::Catalog))?;
            let raw = encode(
                &json!({"score":last.score,"item":last.item_id,"membership":cap.membership_digest}),
            );
            let cursor = crate::api_catalog::authenticated_cursor(
                &cap.cursor_key,
                &cap.fingerprint,
                &request.binding,
                &raw,
            )
            .map_err(|_| fail(ScopeErrorCode::Catalog))?;
            if cursor.len() > MAX_CURSOR_BYTES {
                return Err(fail(ScopeErrorCode::ByteBudget));
            }
            Some(cursor)
        } else {
            None
        };
        let value = json!({"scope":cap.fingerprint,"matches":page.into_iter().map(|hit|json!({"item_id":hit.item_id,"score":hit.score})).collect::<Vec<_>>(),"scoped_total":total,"has_more":has_more,"next_cursor":cursor});
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
    })();
    // Clear before fresh() rolls back, even after timeout/cancellation. SQL
    // cleanup must not inherit the exhausted owned callback.
    guard.finish()?;
    if let Some(cause) = work.failure() {
        return Err(fail(evaluation_code(Some(cause))));
    }
    result
}
#[cfg(test)]
#[path = "catalog_scope_search_tests.rs"]
mod tests;
