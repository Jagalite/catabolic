// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Versioned native derived search storage. Trusted maintenance APIs, not search
//! authority. Scoped matching must filter individual canonical values before FTS.
//! No query, receipt engine, host callbacks, network or filesystem effects here.
use crate::{
    Error, Store,
    operation_budget::{BudgetCharge, BudgetLimits, OperationBudget},
    selection::EvaluationFailure,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

pub const SEARCH_SCHEMA: u32 = 35;
pub const SEARCH_CONTRACT: u32 = 1;
const MAX_ITEMS: usize = 100_000;
const MAX_VALUES: usize = 1_000_000;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_STEPS: usize = 2_000_000;
const MAX_VALUE_BYTES: usize = 65_536;
const MAX_ALIAS_DEPTH: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchIndexErrorCode {
    UnsupportedSchema,
    AmbientTransaction,
    InvalidBudget,
    ItemBudget,
    ByteBudget,
    WorkBudget,
    Timeout,
    Cancelled,
    InvalidSource,
    AliasBudget,
    CorruptIndex,
    StoreFailure,
}
impl SearchIndexErrorCode {
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnsupportedSchema => "catalog_search_schema35_required",
            Self::AmbientTransaction => "catalog_search_ambient_transaction",
            Self::InvalidBudget => "catalog_search_invalid_budget",
            Self::ItemBudget => "catalog_search_item_budget",
            Self::ByteBudget => "catalog_search_byte_budget",
            Self::WorkBudget => "catalog_search_work_budget",
            Self::Timeout => "catalog_search_timeout",
            Self::Cancelled => "catalog_search_cancelled",
            Self::InvalidSource => "catalog_search_invalid_source",
            Self::AliasBudget => "catalog_search_alias_budget",
            Self::CorruptIndex => "catalog_search_corrupt_index",
            Self::StoreFailure => "catalog_search_store_failure",
        }
    }
}
#[derive(Debug)]
pub struct SearchIndexError {
    pub code: SearchIndexErrorCode,
    pub error: Error,
}
impl fmt::Display for SearchIndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.code(), self.error)
    }
}
impl std::error::Error for SearchIndexError {}
impl From<Error> for SearchIndexError {
    fn from(error: Error) -> Self {
        Self {
            code: SearchIndexErrorCode::StoreFailure,
            error,
        }
    }
}
impl From<rusqlite::Error> for SearchIndexError {
    fn from(error: rusqlite::Error) -> Self {
        let code = if error.sqlite_error_code() == Some(rusqlite::ErrorCode::OperationInterrupted) {
            SearchIndexErrorCode::Cancelled
        } else {
            SearchIndexErrorCode::StoreFailure
        };
        Self {
            code,
            error: error.into(),
        }
    }
}
type Result<T> = std::result::Result<T, SearchIndexError>;
fn fail<T>(code: SearchIndexErrorCode, message: &str) -> Result<T> {
    Err(SearchIndexError {
        code,
        error: Error(message.into()),
    })
}
fn as_store(error: SearchIndexError) -> Error {
    Error(error.to_string())
}

/// Trusted work bounds. This type does not deserialize a request or carry grants.
#[derive(Clone, Copy, Debug)]
pub struct SearchIndexLimits {
    pub max_items: usize,
    pub max_values: usize,
    pub max_text_bytes: usize,
    pub max_steps: usize,
    pub timeout: Duration,
}
impl Default for SearchIndexLimits {
    fn default() -> Self {
        Self {
            max_items: 10_000,
            max_values: 100_000,
            max_text_bytes: 16 * 1024 * 1024,
            max_steps: 500_000,
            timeout: Duration::from_secs(5),
        }
    }
}
fn budget_error(failure: EvaluationFailure) -> SearchIndexError {
    let code = match failure {
        EvaluationFailure::IdBudget => SearchIndexErrorCode::ItemBudget,
        EvaluationFailure::ByteBudget => SearchIndexErrorCode::ByteBudget,
        EvaluationFailure::Timeout => SearchIndexErrorCode::Timeout,
        EvaluationFailure::Cancelled => SearchIndexErrorCode::Cancelled,
        _ => SearchIndexErrorCode::WorkBudget,
    };
    SearchIndexError {
        code,
        error: Error(format!("shared operation budget: {failure:?}")),
    }
}
fn budget(limits: SearchIndexLimits) -> Result<OperationBudget> {
    if limits.max_items > MAX_ITEMS
        || limits.max_values > MAX_VALUES
        || limits.max_text_bytes > MAX_BYTES
        || limits.max_steps > MAX_STEPS
        || limits.timeout > Duration::from_secs(60)
    {
        return fail(
            SearchIndexErrorCode::InvalidBudget,
            "requested bounds exceed native maximum",
        );
    }
    OperationBudget::new(
        limits.timeout,
        BudgetLimits {
            items: limits.max_items,
            values: limits.max_values,
            bytes: limits.max_text_bytes,
            work: limits.max_steps,
        },
    )
    .map_err(budget_error)
}
pub(crate) fn default_operation_budget() -> crate::Result<OperationBudget> {
    budget(SearchIndexLimits::default()).map_err(as_store)
}
struct Work<'a> {
    budget: &'a OperationBudget,
    cancel: Option<&'a AtomicBool>,
}
impl Work<'_> {
    fn check(&self) -> Result<()> {
        if self.cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Err(budget_error(
                self.budget.record_failure(EvaluationFailure::Cancelled),
            ));
        }
        self.budget
            .charge(BudgetCharge {
                work: 1,
                ..Default::default()
            })
            .map_err(budget_error)
    }
    fn bytes(&self, bytes: usize) -> Result<()> {
        self.budget
            .charge(BudgetCharge {
                bytes,
                ..Default::default()
            })
            .map_err(budget_error)
    }
    fn value(&self, value: &CanonicalSearchValue) -> Result<()> {
        self.check()?;
        if value.text.len() > MAX_VALUE_BYTES {
            return fail(
                SearchIndexErrorCode::ByteBudget,
                "search value bytes exceed bound; no truncation",
            );
        }
        let bytes = value.text.len()
            + value.field_key.len()
            + value.origin_key.len()
            + value.provenance_json.len()
            + value.required_subjects_json.len();
        self.budget
            .charge(BudgetCharge {
                values: 1,
                bytes,
                ..Default::default()
            })
            .map_err(budget_error)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SearchIndexStatus {
    pub contract_version: u32,
    pub ready: bool,
    pub input_revision: i64,
    pub indexed_revision: i64,
    pub catalog_id: String,
    pub incarnation: String,
    pub generation: i64,
}
fn state(db: &Connection) -> Result<SearchIndexStatus> {
    let value = db.query_row("SELECT x.contract_version,x.ready,x.input_revision,x.indexed_revision,c.catalog_id,c.incarnation,c.generation,x.catalog_id,x.incarnation FROM catalog_search_state x JOIN catalog_state c ON c.id=1 WHERE x.id=1", [], |r| {
        Ok((r.get::<_,u32>(0)?,r.get::<_,bool>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,i64>(6)?,r.get::<_,Option<String>>(7)?,r.get::<_,Option<String>>(8)?))
    })?;
    let dirty: bool = db
        .prepare("SELECT 1 FROM catalog_search_dirty LIMIT 1")?
        .exists([])?;
    Ok(SearchIndexStatus {
        contract_version: value.0,
        ready: value.1
            && !dirty
            && value.2 == value.3
            && value.7.as_ref() == Some(&value.4)
            && value.8.as_ref() == Some(&value.5),
        input_revision: value.2,
        indexed_revision: value.3,
        catalog_id: value.4,
        incarnation: value.5,
        generation: value.6,
    })
}
fn admit(store: &Store, maintenance: bool) -> Result<()> {
    store.transaction_available()?;
    if store.schema_version < SEARCH_SCHEMA {
        return fail(
            SearchIndexErrorCode::UnsupportedSchema,
            "native schema35 is required",
        );
    }
    if !store.db.is_autocommit() && (maintenance || store.writable) {
        return fail(
            SearchIndexErrorCode::AmbientTransaction,
            "maintenance owns its Store transaction",
        );
    }
    Ok(())
}
/// Trusted diagnostic of the admitted Store snapshot (retained on read-only Store).
/// It is not an authority-scoped public search result or a current-generation lease.
pub fn status(store: &Store) -> Result<SearchIndexStatus> {
    admit(store, false)?;
    let before = state(&store.db)?;
    store.transaction_available()?;
    if state(&store.db)? != before {
        return fail(
            SearchIndexErrorCode::CorruptIndex,
            "search status changed during read",
        );
    }
    Ok(before)
}

/// Original-subject dependencies are evidence for the central capability owner,
/// never grants. Do not concatenate these values before field/path authorization.
#[derive(Clone, Debug)]
pub(crate) struct CanonicalSearchValue {
    pub field_key: String,
    pub origin_key: String,
    pub rank_class: &'static str,
    pub text: String,
    pub provenance_json: String,
    pub required_subjects_json: String,
}
fn add(
    values: &mut Vec<CanonicalSearchValue>,
    work: &mut Work<'_>,
    value: CanonicalSearchValue,
) -> Result<()> {
    work.value(&value)?;
    values.push(value);
    Ok(())
}
fn source_text(row: &rusqlite::Row<'_>, column: usize, work: &Work<'_>) -> Result<String> {
    let raw = row.get_ref(column)?;
    let text = raw.as_str().map_err(|error| SearchIndexError {
        code: SearchIndexErrorCode::InvalidSource,
        error: Error(error.to_string()),
    })?;
    if text.len() > MAX_VALUE_BYTES {
        return fail(
            SearchIndexErrorCode::ByteBudget,
            "search source text exceeds per-value bound",
        );
    }
    work.bytes(text.len())?;
    Ok(text.to_owned())
}
fn json_text(value: Value) -> String {
    catabolic_core::encode(&value)
}
fn metadata_values(
    db: &Connection,
    item: &str,
    values: &mut Vec<CanonicalSearchValue>,
    work: &mut Work<'_>,
) -> Result<()> {
    work.check()?;
    let managed: bool = db
        .prepare("SELECT 1 FROM catalog_metadata_subjects WHERE item_id=?")?
        .exists([item])?;
    let size_sql = if managed {
        "SELECT length(CAST(effective_json AS BLOB)) FROM catalog_metadata_projection WHERE item_id=?"
    } else {
        "SELECT length(CAST(metadata AS BLOB)) FROM items WHERE id=?"
    };
    let bytes: Option<i64> = db.query_row(size_sql, [item], |r| r.get(0)).optional()?;
    let Some(bytes) = bytes else {
        return fail(
            SearchIndexErrorCode::InvalidSource,
            "managed subject lacks effective projection",
        );
    };
    if bytes < 0 || bytes > MAX_VALUE_BYTES as i64 {
        return fail(
            SearchIndexErrorCode::ByteBudget,
            "metadata source too large",
        );
    }
    work.bytes(bytes as usize)?;
    let (raw, origin): (String, &'static str) = if managed {
        let raw: Option<String> = db
            .query_row(
                "SELECT effective_json FROM catalog_metadata_projection WHERE item_id=?",
                [item],
                |r| r.get(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            return fail(
                SearchIndexErrorCode::InvalidSource,
                "managed subject lacks effective projection",
            );
        };
        (raw, "managed_resolved")
    } else {
        (
            db.query_row("SELECT metadata FROM items WHERE id=?", [item], |r| {
                r.get(0)
            })?,
            "legacy_unmanaged",
        )
    };
    if raw.len() > MAX_VALUE_BYTES {
        return fail(
            SearchIndexErrorCode::ByteBudget,
            "metadata source too large",
        );
    }
    let value: Value = serde_json::from_str(&raw).map_err(|e| SearchIndexError {
        code: SearchIndexErrorCode::InvalidSource,
        error: Error(e.to_string()),
    })?;
    if !value.is_object() {
        return fail(
            SearchIndexErrorCode::InvalidSource,
            "metadata projection must be an object",
        );
    }
    for field in ["title", "description"] {
        if let Some(text) = value.get(field).and_then(Value::as_str) {
            add(
                values,
                work,
                CanonicalSearchValue {
                    field_key: format!("metadata:{field}"),
                    origin_key: origin.into(),
                    rank_class: field,
                    text: text.into(),
                    provenance_json: json_text(json!({"kind":origin,"item_id":item,"field":field})),
                    required_subjects_json: json_text(json!([item])),
                },
            )?;
        }
    }
    Ok(())
}
fn pins_values(
    db: &Connection,
    item: &str,
    values: &mut Vec<CanonicalSearchValue>,
    work: &mut Work<'_>,
) -> Result<()> {
    let mut statement=db.prepare("SELECT namespace,value,current_revision FROM catalog_identity_pins WHERE item_id=? AND value IS NOT NULL ORDER BY namespace LIMIT ?")?;
    let mut rows = statement.query(params![item, (MAX_VALUES + 1) as i64])?;
    while let Some(row) = rows.next()? {
        work.check()?;
        let namespace = source_text(row, 0, work)?;
        let value = source_text(row, 1, work)?;
        let revision: i64 = row.get(2)?;
        add(
            values,
            work,
            CanonicalSearchValue {
                field_key: format!("identity:{namespace}"),
                origin_key: namespace.clone(),
                rank_class: "accepted_identities",
                text: format!("{namespace}:{value}"),
                provenance_json: json_text(
                    json!({"kind":"accepted_pin","item_id":item,"namespace":namespace,"revision":revision}),
                ),
                required_subjects_json: json_text(json!([item])),
            },
        )?;
    }
    Ok(())
}
fn tag_values(
    db: &Connection,
    item: &str,
    values: &mut Vec<CanonicalSearchValue>,
    work: &mut Work<'_>,
) -> Result<()> {
    // Stream item-indexed assertion rows. No global tag-name scan or aggregate
    // sort is needed before the first bounded charge/cancellation checkpoint.
    let mut statement =
        db.prepare("SELECT tag_id FROM catalog_effective_item_tags WHERE item_id=? LIMIT ?")?;
    let mut rows = statement.query(params![item, (MAX_VALUES + 1) as i64])?;
    let mut tags = BTreeSet::new();
    let mut assertions = 0;
    while let Some(row) = rows.next()? {
        work.check()?;
        assertions += 1;
        if assertions > MAX_VALUES {
            return fail(
                SearchIndexErrorCode::WorkBudget,
                "tag assertion scan exceeds hard bound",
            );
        }
        tags.insert(source_text(row, 0, work)?);
    }
    for tag in tags {
        let mut statement = db.prepare("SELECT name FROM tag_names WHERE tag_id=? LIMIT ?")?;
        let mut rows = statement.query(params![tag, (MAX_VALUES + 1) as i64])?;
        while let Some(row) = rows.next()? {
            work.check()?;
            let name = source_text(row, 0, work)?;
            add(
                values,
                work,
                CanonicalSearchValue {
                    field_key: "tags".into(),
                    origin_key: name.clone(),
                    rank_class: "effective_tags",
                    text: name,
                    provenance_json: json_text(
                        json!({"kind":"effective_tag_name","item_id":item,"tag_id":tag}),
                    ),
                    required_subjects_json: json_text(json!([item])),
                },
            )?;
        }
    }
    Ok(())
}
fn alias_values(
    db: &Connection,
    item: &str,
    values: &mut Vec<CanonicalSearchValue>,
    work: &mut Work<'_>,
) -> Result<()> {
    let mut queue = VecDeque::from([vec![item.to_owned()]]);
    let mut seen = BTreeSet::from([item.to_owned()]);
    while let Some(path) = queue.pop_front() {
        work.check()?;
        let mut statement=db.prepare("SELECT item_id FROM catalog_logical_retirements WHERE kind='merge' AND survivor_item_id=? ORDER BY item_id LIMIT ?")?;
        let mut rows = statement.query(params![path[0], (MAX_VALUES + 1) as i64])?;
        while let Some(row) = rows.next()? {
            let source = source_text(row, 0, work)?;
            if path.len() >= MAX_ALIAS_DEPTH || !seen.insert(source.clone()) {
                return fail(
                    SearchIndexErrorCode::AliasBudget,
                    "cyclic or over-depth logical alias path",
                );
            }
            let mut full = vec![source.clone()];
            full.extend(path.clone());
            add(
                values,
                work,
                CanonicalSearchValue {
                    field_key: "logical_aliases".into(),
                    origin_key: source.clone(),
                    rank_class: "aliases",
                    text: source.clone(),
                    provenance_json: json_text(
                        json!({"kind":"logical_merge_alias","source_item_id":source,"survivor_item_id":item,"path":full}),
                    ),
                    required_subjects_json: json_text(json!(full)),
                },
            )?;
            queue.push_back(full);
        }
    }
    Ok(())
}
fn reindex(db: &Connection, item: &str, work: &mut Work<'_>) -> Result<()> {
    work.check()?;
    db.execute("DELETE FROM catalog_search_values WHERE item_id=?", [item])?;
    if !db
        .prepare("SELECT 1 FROM catalog_live_items WHERE id=?")?
        .exists([item])?
    {
        db.execute(
            "DELETE FROM catalog_search_documents WHERE item_id=?",
            [item],
        )?;
        return Ok(());
    }
    let mut values = Vec::new();
    metadata_values(db, item, &mut values, work)?;
    pins_values(db, item, &mut values, work)?;
    tag_values(db, item, &mut values, work)?;
    alias_values(db, item, &mut values, work)?;
    let mut columns: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for value in values {
        work.check()?;
        db.execute("INSERT INTO catalog_search_values(item_id,field_key,origin_key,rank_class,value_text,provenance_json,required_subjects_json) VALUES(?,?,?,?,?,?,?)",params![item,value.field_key,value.origin_key,value.rank_class,value.text,value.provenance_json,value.required_subjects_json])?;
        columns
            .entry(value.rank_class)
            .or_default()
            .insert(value.text);
    }
    work.bytes(
        columns
            .values()
            .flat_map(|values| values.iter())
            .map(|value| value.len() + 1)
            .sum(),
    )?;
    let text = |key: &str| {
        columns
            .get(key)
            .map(|values| values.iter().cloned().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default()
    };
    db.execute("INSERT INTO catalog_search_documents(item_id,title,aliases,accepted_identities,effective_tags,description) VALUES(?,?,?,?,?,?) ON CONFLICT(item_id) DO UPDATE SET title=excluded.title,aliases=excluded.aliases,accepted_identities=excluded.accepted_identities,effective_tags=excluded.effective_tags,description=excluded.description",params![item,text("title"),text("aliases"),text("accepted_identities"),text("effective_tags"),text("description")])?;
    Ok(())
}
fn ids(db: &Connection, rebuild: bool, work: &mut Work<'_>) -> Result<Vec<String>> {
    work.check()?;
    let sql = if rebuild {
        "SELECT id FROM catalog_live_items ORDER BY id LIMIT ?"
    } else {
        "SELECT item_id FROM catalog_search_dirty ORDER BY item_id LIMIT ?"
    };
    let mut statement = db.prepare(sql)?;
    let mut rows = statement.query([(MAX_ITEMS + 1) as i64])?;
    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        work.check()?;
        work.budget
            .charge(BudgetCharge {
                items: 1,
                ..Default::default()
            })
            .map_err(budget_error)?;
        result.push(source_text(row, 0, work)?);
        if result.len() > MAX_ITEMS {
            return fail(
                SearchIndexErrorCode::ItemBudget,
                "complete index input exceeds item bound",
            );
        }
    }
    Ok(result)
}
fn update(db: &Connection, rebuild: bool, work: &mut Work<'_>) -> Result<SearchIndexStatus> {
    if db.is_autocommit() {
        return fail(
            SearchIndexErrorCode::AmbientTransaction,
            "search changes require the existing writer transaction",
        );
    }
    work.check()?;
    let before = state(db)?;
    let items = ids(db, rebuild, work)?;
    if rebuild {
        db.execute("DELETE FROM catalog_search_values", [])?;
        db.execute("DELETE FROM catalog_search_documents", [])?;
    }
    for item in &items {
        reindex(db, item, work)?;
    }
    work.check()?;
    // Rebuild includes complete live membership; stale/deleted documents vanish.
    if rebuild {
        db.execute(
            "INSERT INTO catalog_search_fts(catalog_search_fts,rank) VALUES('integrity-check',1)",
            [],
        )?;
    }
    let after_revision: i64 = db.query_row(
        "SELECT input_revision FROM catalog_search_state WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    if after_revision != before.input_revision {
        return fail(
            SearchIndexErrorCode::CorruptIndex,
            "source changed during derived index finalization",
        );
    }
    db.execute("DELETE FROM catalog_search_dirty", [])?;
    db.execute(
        "UPDATE catalog_search_state SET indexed_revision=input_revision WHERE id=1",
        [],
    )?;
    if rebuild {
        db.execute("UPDATE catalog_search_state SET ready=1,catalog_id=?,incarnation=?,rebuilt_generation=? WHERE id=1",params![before.catalog_id,before.incarnation,before.generation])?;
    }
    work.check()?;
    // FTS5 xSync otherwise flushes pending terms from inside COMMIT. Keep this
    // potentially large VM work in the ordinary interruptible budget phase,
    // before receipt delivery and the private owner's prepaid fixed COMMIT.
    db.execute(
        "INSERT INTO catalog_search_fts(catalog_search_fts) VALUES('flush')",
        [],
    )?;
    work.check()?;
    state(db)
}
/// DB-only maintenance seam for the private writer VM admission. The caller owns
/// the transaction and passes the existing cumulative operation budget.
pub(crate) fn rebuild_connection(
    db: &Connection,
    budget: &OperationBudget,
) -> Result<SearchIndexStatus> {
    update(
        db,
        true,
        &mut Work {
            budget,
            cancel: None,
        },
    )
}

/// Rebuild only derived rows through Store's transaction owner. No receipt or
/// authoritative source rows are created. Caller must hold maintenance admission.
/// This legacy Store wrapper checks between bounded steps; it does not install or
/// replace an unknown SQLite progress handler. Full VM-step preemption requires
/// the private writer owner and its rebuild_connection seam.
pub fn rebuild(
    store: &Store,
    limits: SearchIndexLimits,
    cancel: Option<&AtomicBool>,
) -> Result<SearchIndexStatus> {
    admit(store, true)?;
    let budget = budget(limits)?;
    let _scope = budget.enter().map_err(budget_error)?;
    let result = store.transaction_detailed(|db| match cancel {
        None => rebuild_connection(db, &budget),
        Some(cancel) => update(
            db,
            true,
            &mut Work {
                budget: &budget,
                cancel: Some(cancel),
            },
        ),
    })?;
    store.transaction_available()?;
    Ok(result)
}
/// Fixed integration hook, never a public mutation callback. The transaction's
/// shared interruption bridge remains installed by its existing owner.
pub(crate) fn flush_before_commit(db: &Connection, budget: &OperationBudget) -> crate::Result<()> {
    budget
        .check()
        .map_err(|failure| as_store(budget_error(failure)))?;
    if !db
        .prepare("SELECT 1 FROM catalog_search_dirty LIMIT 1")?
        .exists([])?
    {
        // Known receipt replay and unchanged transactions perform no derived write.
        return validate_closed(db);
    }
    update(
        db,
        false,
        &mut Work {
            budget,
            cancel: None,
        },
    )
    .map(|_| ())
    .map_err(as_store)
}
pub(crate) fn validate_closed(db: &Connection) -> crate::Result<()> {
    let state = state(db).map_err(as_store)?;
    if db
        .prepare("SELECT 1 FROM catalog_search_dirty LIMIT 1")?
        .exists([])?
        || state.input_revision != state.indexed_revision
    {
        return Err(Error("catalog_search_unclosed_dirty_state".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("search-budget.db");
        crate::migration::initialize_native_at(&path, 35).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        (root, store)
    }
    #[test]
    fn outer_shared_budget_is_not_reset_by_nested_transactions_or_finalizer() {
        let (_root, store) = fixture();
        let budget = OperationBudget::new(
            Duration::from_secs(5),
            BudgetLimits {
                items: 1,
                values: 100,
                bytes: 10000,
                work: 1000,
            },
        )
        .unwrap();
        let _admission = budget.enter().unwrap();
        let outcome = store.transaction(|_| {
            crate::curation::item_put(
                &store,
                &json!({"id":"a","kind":"movie","metadata":{"title":"alpha"}}),
            )?;
            crate::curation::item_put(
                &store,
                &json!({"id":"b","kind":"movie","metadata":{"title":"beta"}}),
            )?;
            Ok(())
        });
        assert!(
            outcome
                .unwrap_err()
                .0
                .contains("catalog_search_item_budget")
        );
        assert_eq!(budget.failure(), Some(EvaluationFailure::IdBudget));
        assert_eq!(
            store
                .db
                .query_row("SELECT count(*) FROM items", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(store.db.is_autocommit());
    }
    #[test]
    fn rebuild_and_final_flush_borrow_one_budget_and_preserve_sticky_origin() {
        let (_root, store) = fixture();
        crate::curation::item_put(
            &store,
            &json!({"id":"a","kind":"movie","metadata":{"title":"alpha"}}),
        )
        .unwrap();
        let budget = OperationBudget::new(
            Duration::from_secs(5),
            BudgetLimits {
                items: 1,
                values: 100,
                bytes: 10000,
                work: 1000,
            },
        )
        .unwrap();
        let _admission = budget.enter().unwrap();
        let result: Result<()> = store.transaction_detailed(|db| {
            rebuild_connection(db, &budget)?;
            rebuild_connection(db, &budget)?;
            Ok(())
        });
        assert_eq!(result.unwrap_err().code, SearchIndexErrorCode::ItemBudget);
        assert_eq!(budget.failure(), Some(EvaluationFailure::IdBudget));
        assert!(!state(&store.db).unwrap().ready);
        assert!(store.db.is_autocommit());
    }
    #[test]
    fn writer_admission_uses_outer_absolute_deadline_not_a_new_timeout() {
        let (_root, store) = fixture();
        let budget = OperationBudget::new(
            Duration::from_millis(10),
            BudgetLimits {
                items: 100,
                values: 100,
                bytes: 10000,
                work: 1000,
            },
        )
        .unwrap();
        let deadline = budget.deadline();
        let _admission = budget.enter().unwrap();
        std::thread::sleep(Duration::from_millis(15));
        assert_eq!(budget.remaining_ms(), Err(EvaluationFailure::Timeout));
        let error = store.transaction(|_| Ok(())).unwrap_err();
        assert!(error.0.contains("Timeout"));
        assert_eq!(budget.deadline(), deadline);
        assert!(store.db.is_autocommit());
    }
}
