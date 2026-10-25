//! Fixed permanent source-row storage. This module lives under migration so
//! the existing private allocation-context borrowed preflight remains intact.
//! A sealed lexical layout is representation evidence only, never admission.
use super::{native_session::allocation, value};
use crate::{
    Error, Result, catalog_identity_mutation::StructuralReadBudget, operation_budget::BudgetCharge,
};
use rusqlite::Connection;
use serde::{Serialize, Serializer, ser::SerializeMap};
use serde_json::Value;

const ROW_LIMIT: usize = 16_384;
const BYTE_LIMIT: usize = 8 * 1024 * 1024;
fn refused() -> Error {
    Error("source succession: fixed permanent row shape or bound differs".into())
}

pub(crate) trait RowView {
    fn get_field(&self, key: &str) -> Option<&Value>;
}
impl RowView for Value {
    fn get_field(&self, key: &str) -> Option<&Value> {
        self.get(key)
    }
}
#[derive(Debug)]
struct Layout {
    table: &'static str,
    physical: &'static [&'static str],
    // Every physical column exactly once, in strict canonical key order.
    lexical: &'static [(&'static str, usize)],
    count_sql: &'static str,
    rows_sql: &'static str,
}
const L0: Layout = Layout {
    table: "catalog_source_successions",
    physical: &[
        "profile",
        "source_id",
        "revision",
        "predecessor_digest",
        "record_digest",
        "catalog_id",
        "incarnation",
        "expected_generation",
        "committed_generation",
        "command_host_namespace",
        "command_operation_id",
        "payload_digest",
        "admission_digest",
        "old_binding_json",
        "new_binding_json",
        "policy_json",
        "old_mapping_json",
        "new_mapping_json",
        "closure_digest",
        "file_count",
        "context_count",
        "head_count",
        "scope_count",
        "evidence_count",
        "evidence_digest",
        "result_digest",
        "result_json",
    ],
    lexical: &[
        ("admission_digest", 12),
        ("catalog_id", 5),
        ("closure_digest", 18),
        ("command_host_namespace", 9),
        ("command_operation_id", 10),
        ("committed_generation", 8),
        ("context_count", 20),
        ("evidence_count", 23),
        ("evidence_digest", 24),
        ("expected_generation", 7),
        ("file_count", 19),
        ("head_count", 21),
        ("incarnation", 6),
        ("new_binding_json", 14),
        ("new_mapping_json", 17),
        ("old_binding_json", 13),
        ("old_mapping_json", 16),
        ("payload_digest", 11),
        ("policy_json", 15),
        ("predecessor_digest", 3),
        ("profile", 0),
        ("record_digest", 4),
        ("result_digest", 25),
        ("result_json", 26),
        ("revision", 2),
        ("scope_count", 22),
        ("source_id", 1),
    ],
    count_sql: r#"SELECT count(*),coalesce(sum(256+coalesce(length(CAST("profile" AS BLOB)),0)+coalesce(length(CAST("source_id" AS BLOB)),0)+coalesce(length(CAST("revision" AS BLOB)),0)+coalesce(length(CAST("predecessor_digest" AS BLOB)),0)+coalesce(length(CAST("record_digest" AS BLOB)),0)+coalesce(length(CAST("catalog_id" AS BLOB)),0)+coalesce(length(CAST("incarnation" AS BLOB)),0)+coalesce(length(CAST("expected_generation" AS BLOB)),0)+coalesce(length(CAST("committed_generation" AS BLOB)),0)+coalesce(length(CAST("command_host_namespace" AS BLOB)),0)+coalesce(length(CAST("command_operation_id" AS BLOB)),0)+coalesce(length(CAST("payload_digest" AS BLOB)),0)+coalesce(length(CAST("admission_digest" AS BLOB)),0)+coalesce(length(CAST("old_binding_json" AS BLOB)),0)+coalesce(length(CAST("new_binding_json" AS BLOB)),0)+coalesce(length(CAST("policy_json" AS BLOB)),0)+coalesce(length(CAST("old_mapping_json" AS BLOB)),0)+coalesce(length(CAST("new_mapping_json" AS BLOB)),0)+coalesce(length(CAST("closure_digest" AS BLOB)),0)+coalesce(length(CAST("file_count" AS BLOB)),0)+coalesce(length(CAST("context_count" AS BLOB)),0)+coalesce(length(CAST("head_count" AS BLOB)),0)+coalesce(length(CAST("scope_count" AS BLOB)),0)+coalesce(length(CAST("evidence_count" AS BLOB)),0)+coalesce(length(CAST("evidence_digest" AS BLOB)),0)+coalesce(length(CAST("result_digest" AS BLOB)),0)+coalesce(length(CAST("result_json" AS BLOB)),0)),0) FROM catalog_source_successions WHERE profile=? AND source_id=?"#,
    rows_sql: r#"SELECT * FROM catalog_source_successions WHERE profile=? AND source_id=? ORDER BY "profile" COLLATE BINARY,"source_id" COLLATE BINARY,"revision" COLLATE BINARY,"predecessor_digest" COLLATE BINARY,"record_digest" COLLATE BINARY,"catalog_id" COLLATE BINARY,"incarnation" COLLATE BINARY,"expected_generation" COLLATE BINARY,"committed_generation" COLLATE BINARY,"command_host_namespace" COLLATE BINARY,"command_operation_id" COLLATE BINARY,"payload_digest" COLLATE BINARY,"admission_digest" COLLATE BINARY,"old_binding_json" COLLATE BINARY,"new_binding_json" COLLATE BINARY,"policy_json" COLLATE BINARY,"old_mapping_json" COLLATE BINARY,"new_mapping_json" COLLATE BINARY,"closure_digest" COLLATE BINARY,"file_count" COLLATE BINARY,"context_count" COLLATE BINARY,"head_count" COLLATE BINARY,"scope_count" COLLATE BINARY,"evidence_count" COLLATE BINARY,"evidence_digest" COLLATE BINARY,"result_digest" COLLATE BINARY,"result_json" COLLATE BINARY"#,
};
const L1: Layout = Layout {
    table: "catalog_source_succession_files",
    physical: &[
        "profile",
        "source_id",
        "succession_revision",
        "file_id",
        "relative_path",
        "disposition",
        "old_scan_provenance_json",
        "old_observation_json",
        "old_occurrence_json",
        "new_occurrence_json",
        "old_ctime_ns",
        "new_ctime_ns",
        "content_sha256",
        "bytes",
    ],
    lexical: &[
        ("bytes", 13),
        ("content_sha256", 12),
        ("disposition", 5),
        ("file_id", 3),
        ("new_ctime_ns", 11),
        ("new_occurrence_json", 9),
        ("old_ctime_ns", 10),
        ("old_observation_json", 7),
        ("old_occurrence_json", 8),
        ("old_scan_provenance_json", 6),
        ("profile", 0),
        ("relative_path", 4),
        ("source_id", 1),
        ("succession_revision", 2),
    ],
    count_sql: r#"SELECT count(*),coalesce(sum(256+coalesce(length(CAST("profile" AS BLOB)),0)+coalesce(length(CAST("source_id" AS BLOB)),0)+coalesce(length(CAST("succession_revision" AS BLOB)),0)+coalesce(length(CAST("file_id" AS BLOB)),0)+coalesce(length(CAST("relative_path" AS BLOB)),0)+coalesce(length(CAST("disposition" AS BLOB)),0)+coalesce(length(CAST("old_scan_provenance_json" AS BLOB)),0)+coalesce(length(CAST("old_observation_json" AS BLOB)),0)+coalesce(length(CAST("old_occurrence_json" AS BLOB)),0)+coalesce(length(CAST("new_occurrence_json" AS BLOB)),0)+coalesce(length(CAST("old_ctime_ns" AS BLOB)),0)+coalesce(length(CAST("new_ctime_ns" AS BLOB)),0)+coalesce(length(CAST("content_sha256" AS BLOB)),0)+coalesce(length(CAST("bytes" AS BLOB)),0)),0) FROM catalog_source_succession_files WHERE profile=? AND source_id=? AND succession_revision=?"#,
    rows_sql: r#"SELECT * FROM catalog_source_succession_files WHERE profile=? AND source_id=? AND succession_revision=? ORDER BY "profile" COLLATE BINARY,"source_id" COLLATE BINARY,"succession_revision" COLLATE BINARY,"file_id" COLLATE BINARY,"relative_path" COLLATE BINARY,"disposition" COLLATE BINARY,"old_scan_provenance_json" COLLATE BINARY,"old_observation_json" COLLATE BINARY,"old_occurrence_json" COLLATE BINARY,"new_occurrence_json" COLLATE BINARY,"old_ctime_ns" COLLATE BINARY,"new_ctime_ns" COLLATE BINARY,"content_sha256" COLLATE BINARY,"bytes" COLLATE BINARY"#,
};
const L2: Layout = Layout {
    table: "catalog_source_succession_contexts",
    physical: &[
        "profile",
        "source_id",
        "succession_revision",
        "context_id",
        "head_count",
        "old_heads_json",
        "renewal_json",
        "result_json",
    ],
    lexical: &[
        ("context_id", 3),
        ("head_count", 4),
        ("old_heads_json", 5),
        ("profile", 0),
        ("renewal_json", 6),
        ("result_json", 7),
        ("source_id", 1),
        ("succession_revision", 2),
    ],
    count_sql: r#"SELECT count(*),coalesce(sum(256+coalesce(length(CAST("profile" AS BLOB)),0)+coalesce(length(CAST("source_id" AS BLOB)),0)+coalesce(length(CAST("succession_revision" AS BLOB)),0)+coalesce(length(CAST("context_id" AS BLOB)),0)+coalesce(length(CAST("head_count" AS BLOB)),0)+coalesce(length(CAST("old_heads_json" AS BLOB)),0)+coalesce(length(CAST("renewal_json" AS BLOB)),0)+coalesce(length(CAST("result_json" AS BLOB)),0)),0) FROM catalog_source_succession_contexts WHERE profile=? AND source_id=? AND succession_revision=?"#,
    rows_sql: r#"SELECT * FROM catalog_source_succession_contexts WHERE profile=? AND source_id=? AND succession_revision=? ORDER BY "profile" COLLATE BINARY,"source_id" COLLATE BINARY,"succession_revision" COLLATE BINARY,"context_id" COLLATE BINARY,"head_count" COLLATE BINARY,"old_heads_json" COLLATE BINARY,"renewal_json" COLLATE BINARY,"result_json" COLLATE BINARY"#,
};
const L3: Layout = Layout {
    table: "catalog_source_succession_evidence",
    physical: &[
        "profile",
        "source_id",
        "succession_revision",
        "file_id",
        "evidence_kind",
        "evidence_key",
        "predecessor_digest",
        "evidence_digest",
        "original_json",
        "renewed_json",
    ],
    lexical: &[
        ("evidence_digest", 7),
        ("evidence_key", 5),
        ("evidence_kind", 4),
        ("file_id", 3),
        ("original_json", 8),
        ("predecessor_digest", 6),
        ("profile", 0),
        ("renewed_json", 9),
        ("source_id", 1),
        ("succession_revision", 2),
    ],
    count_sql: r#"SELECT count(*),coalesce(sum(256+coalesce(length(CAST("profile" AS BLOB)),0)+coalesce(length(CAST("source_id" AS BLOB)),0)+coalesce(length(CAST("succession_revision" AS BLOB)),0)+coalesce(length(CAST("file_id" AS BLOB)),0)+coalesce(length(CAST("evidence_kind" AS BLOB)),0)+coalesce(length(CAST("evidence_key" AS BLOB)),0)+coalesce(length(CAST("predecessor_digest" AS BLOB)),0)+coalesce(length(CAST("evidence_digest" AS BLOB)),0)+coalesce(length(CAST("original_json" AS BLOB)),0)+coalesce(length(CAST("renewed_json" AS BLOB)),0)),0) FROM catalog_source_succession_evidence WHERE profile=? AND source_id=? AND succession_revision=?"#,
    rows_sql: r#"SELECT * FROM catalog_source_succession_evidence WHERE profile=? AND source_id=? AND succession_revision=? ORDER BY "profile" COLLATE BINARY,"source_id" COLLATE BINARY,"succession_revision" COLLATE BINARY,"file_id" COLLATE BINARY,"evidence_kind" COLLATE BINARY,"evidence_key" COLLATE BINARY,"predecessor_digest" COLLATE BINARY,"evidence_digest" COLLATE BINARY,"original_json" COLLATE BINARY,"renewed_json" COLLATE BINARY"#,
};
const L4: Layout = Layout {
    table: "catalog_source_succession_heads",
    physical: &[
        "profile",
        "source_id",
        "succession_revision",
        "entity_kind",
        "entity_key",
        "revision",
        "deleted",
    ],
    lexical: &[
        ("deleted", 6),
        ("entity_key", 4),
        ("entity_kind", 3),
        ("profile", 0),
        ("revision", 5),
        ("source_id", 1),
        ("succession_revision", 2),
    ],
    count_sql: r#"SELECT count(*),coalesce(sum(256+coalesce(length(CAST("profile" AS BLOB)),0)+coalesce(length(CAST("source_id" AS BLOB)),0)+coalesce(length(CAST("succession_revision" AS BLOB)),0)+coalesce(length(CAST("entity_kind" AS BLOB)),0)+coalesce(length(CAST("entity_key" AS BLOB)),0)+coalesce(length(CAST("revision" AS BLOB)),0)+coalesce(length(CAST("deleted" AS BLOB)),0)),0) FROM catalog_source_succession_heads WHERE profile=? AND source_id=? AND succession_revision=?"#,
    rows_sql: r#"SELECT * FROM catalog_source_succession_heads WHERE profile=? AND source_id=? AND succession_revision=? ORDER BY "profile" COLLATE BINARY,"source_id" COLLATE BINARY,"succession_revision" COLLATE BINARY,"entity_kind" COLLATE BINARY,"entity_key" COLLATE BINARY,"revision" COLLATE BINARY,"deleted" COLLATE BINARY"#,
};
const L5: Layout = Layout {
    table: "catalog_source_succession_context_heads",
    physical: &[
        "profile",
        "source_id",
        "succession_revision",
        "context_id",
        "entity_kind",
        "entity_key",
    ],
    lexical: &[
        ("context_id", 3),
        ("entity_key", 5),
        ("entity_kind", 4),
        ("profile", 0),
        ("source_id", 1),
        ("succession_revision", 2),
    ],
    count_sql: r#"SELECT count(*),coalesce(sum(256+coalesce(length(CAST("profile" AS BLOB)),0)+coalesce(length(CAST("source_id" AS BLOB)),0)+coalesce(length(CAST("succession_revision" AS BLOB)),0)+coalesce(length(CAST("context_id" AS BLOB)),0)+coalesce(length(CAST("entity_kind" AS BLOB)),0)+coalesce(length(CAST("entity_key" AS BLOB)),0)),0) FROM catalog_source_succession_context_heads WHERE profile=? AND source_id=? AND succession_revision=?"#,
    rows_sql: r#"SELECT * FROM catalog_source_succession_context_heads WHERE profile=? AND source_id=? AND succession_revision=? ORDER BY "profile" COLLATE BINARY,"source_id" COLLATE BINARY,"succession_revision" COLLATE BINARY,"context_id" COLLATE BINARY,"entity_kind" COLLATE BINARY,"entity_key" COLLATE BINARY"#,
};
const L6: Layout = Layout {
    table: "catalog_source_succession_scopes",
    physical: &[
        "profile",
        "source_id",
        "succession_revision",
        "scope_kind",
        "scope_key",
        "revision",
    ],
    lexical: &[
        ("profile", 0),
        ("revision", 5),
        ("scope_key", 4),
        ("scope_kind", 3),
        ("source_id", 1),
        ("succession_revision", 2),
    ],
    count_sql: r#"SELECT count(*),coalesce(sum(256+coalesce(length(CAST("profile" AS BLOB)),0)+coalesce(length(CAST("source_id" AS BLOB)),0)+coalesce(length(CAST("succession_revision" AS BLOB)),0)+coalesce(length(CAST("scope_kind" AS BLOB)),0)+coalesce(length(CAST("scope_key" AS BLOB)),0)+coalesce(length(CAST("revision" AS BLOB)),0)),0) FROM catalog_source_succession_scopes WHERE profile=? AND source_id=? AND succession_revision=?"#,
    rows_sql: r#"SELECT * FROM catalog_source_succession_scopes WHERE profile=? AND source_id=? AND succession_revision=? ORDER BY "profile" COLLATE BINARY,"source_id" COLLATE BINARY,"succession_revision" COLLATE BINARY,"scope_kind" COLLATE BINARY,"scope_key" COLLATE BINARY,"revision" COLLATE BINARY"#,
};
const L7: Layout = Layout {
    table: "catalog_source_required_scopes",
    physical: &[
        "profile",
        "source_id",
        "succession_revision",
        "scope_kind",
        "scope_key",
    ],
    lexical: &[
        ("profile", 0),
        ("scope_key", 4),
        ("scope_kind", 3),
        ("source_id", 1),
        ("succession_revision", 2),
    ],
    count_sql: r#"SELECT count(*),coalesce(sum(256+coalesce(length(CAST("profile" AS BLOB)),0)+coalesce(length(CAST("source_id" AS BLOB)),0)+coalesce(length(CAST("succession_revision" AS BLOB)),0)+coalesce(length(CAST("scope_kind" AS BLOB)),0)+coalesce(length(CAST("scope_key" AS BLOB)),0)),0) FROM catalog_source_required_scopes WHERE profile=? AND source_id=? AND succession_revision=?"#,
    rows_sql: r#"SELECT * FROM catalog_source_required_scopes WHERE profile=? AND source_id=? AND succession_revision=? ORDER BY "profile" COLLATE BINARY,"source_id" COLLATE BINARY,"succession_revision" COLLATE BINARY,"scope_kind" COLLATE BINARY,"scope_key" COLLATE BINARY"#,
};
const LAYOUTS: [&Layout; 8] = [&L0, &L1, &L2, &L3, &L4, &L5, &L6, &L7];
#[derive(Clone, Copy)]
pub(crate) enum Table {
    Header,
    Files,
    Contexts,
    Evidence,
    Heads,
    ContextHeads,
    Scopes,
    RequiredScopes,
}
impl Table {
    fn index(self) -> usize {
        match self {
            Self::Header => 0,
            Self::Files => 1,
            Self::Contexts => 2,
            Self::Evidence => 3,
            Self::Heads => 4,
            Self::ContextHeads => 5,
            Self::Scopes => 6,
            Self::RequiredScopes => 7,
        }
    }
}

/// No constructor from caller Values, column lists, SQL, or a digest.
#[derive(Debug)]
pub(crate) struct FixedRow {
    layout: &'static Layout,
    cells: Vec<Value>,
}
impl RowView for FixedRow {
    fn get_field(&self, key: &str) -> Option<&Value> {
        self.layout
            .lexical
            .binary_search_by(|(name, _)| name.cmp(&key))
            .ok()
            .map(|i| &self.cells[i])
    }
}
impl std::ops::Index<&str> for FixedRow {
    type Output = Value;
    fn index(&self, key: &str) -> &Value {
        self.get_field(key).unwrap_or(&Value::Null)
    }
}
impl Serialize for FixedRow {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.cells.len()))?;
        for (key, value) in self.entries() {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}
impl FixedRow {
    pub(crate) fn entries(&self) -> impl ExactSizeIterator<Item = (&'static str, &Value)> {
        self.layout
            .lexical
            .iter()
            .zip(&self.cells)
            .map(|((key, _), value)| (*key, value))
    }
}

// Authored immutable layout data is proven at compile time, not rediscovered
// for every row or operation. This says NOTHING about database schema/state.
const fn bytes_equal(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}
const fn before(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let mut i = 0;
    while i < a.len() && i < b.len() {
        if a[i] != b[i] {
            return a[i] < b[i];
        }
        i += 1;
    }
    a.len() < b.len()
}
const fn valid_layout(layout: &Layout) -> bool {
    if layout.physical.len() != layout.lexical.len() || layout.physical.len() > 27 {
        return false;
    }
    let mut seen = [false; 27];
    let mut i = 0;
    while i < layout.lexical.len() {
        let (key, slot) = layout.lexical[i];
        if slot >= layout.physical.len() || seen[slot] || !bytes_equal(key, layout.physical[slot]) {
            return false;
        }
        if i > 0 && !before(layout.lexical[i - 1].0, key) {
            return false;
        }
        seen[slot] = true;
        i += 1;
    }
    true
}
const _: () = {
    let mut i = 0;
    while i < LAYOUTS.len() {
        assert!(valid_layout(LAYOUTS[i]));
        i += 1;
    }
};

/// No remembered schema, connection, caller, budget or prior operation. Every
/// actual statement repeats complete physical-column equality before capture.
pub(crate) struct Reader;
impl Reader {
    pub(crate) fn new() -> Self {
        Self
    }
    pub(crate) fn rows(
        &mut self,
        db: &Connection,
        table: Table,
        profile: &str,
        source: &str,
        revision: Option<i64>,
        budget: &mut impl StructuralReadBudget,
    ) -> Result<Vec<FixedRow>> {
        let index = table.index();
        let layout = LAYOUTS[index];
        if (index == 0) != revision.is_none() {
            return Err(refused());
        }
        // Closed selectors and borrowed fixed parameters remove the previous
        // table-name search and Vec/String parameter copies. Admit actual fixed
        // argument slots/moves before constructing them; no caller SQL escapes.
        budget.charge(BudgetCharge {
            bytes: 3 * std::mem::size_of::<&dyn rusqlite::ToSql>(),
            work: 1 + (6 * std::mem::size_of::<&dyn rusqlite::ToSql>()).div_ceil(64),
            ..Default::default()
        })?;
        let args: [&dyn rusqlite::ToSql; 3] = [&profile, &source, &revision];
        let args = &args[..if revision.is_some() { 3 } else { 2 }];
        let (count, bytes): (i64, i64) =
            db.query_row(layout.count_sql, args, |r| Ok((r.get(0)?, r.get(1)?)))?;
        let count = usize::try_from(count).map_err(|_| refused())?;
        let bytes = usize::try_from(bytes).map_err(|_| refused())?;
        if count > ROW_LIMIT || bytes > BYTE_LIMIT {
            return Err(refused());
        }
        // Preserve the original complete-batch scalar admission. Constructors
        // have a compiled <=27-cell bound; conversion is the unchanged owner.
        budget.charge(BudgetCharge {
            items: count,
            values: count.checked_mul(4).ok_or_else(refused)?,
            bytes,
            work: count,
            ..Default::default()
        })?;
        let slots = count
            .checked_mul(
                std::mem::size_of::<FixedRow>()
                    + layout.physical.len() * std::mem::size_of::<Value>(),
            )
            .ok_or_else(refused)?;
        budget.charge(BudgetCharge {
            bytes: slots,
            work: 1,
            ..Default::default()
        })?;
        let mut statement = db.prepare(layout.rows_sql)?;
        if statement.column_count() != layout.physical.len() {
            return Err(refused());
        }
        for (i, expected) in layout.physical.iter().enumerate() {
            let actual = statement.column_name(i)?;
            budget.charge(BudgetCharge {
                work: 1 + actual.len().min(expected.len()).div_ceil(64),
                ..Default::default()
            })?;
            if actual != *expected {
                return Err(refused());
            }
        }
        let context = allocation::current();
        let mut reservation = context
            .as_ref()
            .map(|c| c.reserve(db, slots as u64))
            .transpose()?;
        let mut admitted = slots as u64;
        let mut rows = statement.query(args)?;
        let mut result = Vec::with_capacity(count);
        while let Some(row) = rows.next()? {
            if result.len() == count {
                return Err(refused());
            }
            if let Some(owned) = &mut reservation {
                let bytes = allocation::row_bytes(row, layout.physical.len())?;
                admitted = admitted
                    .checked_add(bytes)
                    .filter(|n| *n <= 32 * 1024 * 1024)
                    .ok_or_else(refused)?;
                owned.grow(db, bytes)?;
            }
            let mut cells = Vec::with_capacity(layout.lexical.len());
            for (_, slot) in layout.lexical {
                cells.push(value(row.get_ref(*slot)?));
            }
            result.push(FixedRow { layout, cells });
        }
        if result.len() != count {
            return Err(refused());
        }
        Ok(result)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use rusqlite::types::Value as SqlValue;
    #[derive(Default)]
    pub(crate) struct Meter {
        pub(crate) work: usize,
        bytes: usize,
        limit: Option<usize>,
        failed: bool,
    }
    impl StructuralReadBudget for Meter {
        fn charge(&mut self, c: BudgetCharge) -> Result<()> {
            if self.failed {
                return Err(refused());
            }
            self.work = self.work.checked_add(c.work).ok_or_else(refused)?;
            self.bytes = self.bytes.checked_add(c.bytes).ok_or_else(refused)?;
            if self.limit.is_some_and(|n| self.bytes > n) {
                self.failed = true;
                return Err(refused());
            }
            Ok(())
        }
        fn raw_json(&mut self, _: &str) -> Result<()> {
            Err(refused())
        }
    }
    pub(crate) const TABLES: [Table; 8] = [
        Table::Header,
        Table::Files,
        Table::Contexts,
        Table::Evidence,
        Table::Heads,
        Table::ContextHeads,
        Table::Scopes,
        Table::RequiredScopes,
    ];
    // Representation-only SQLite fixtures. They do not mint a Store, receipt,
    // completed admission, succession, or physical proof. Actual source tests
    // independently compare against mutation-minted permanent record digests.
    pub(crate) fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        let schema_db = Connection::open_in_memory().unwrap();
        for (index, layout) in LAYOUTS.into_iter().enumerate() {
            // Independently obtain the first seven physical schemas from the
            // actual SQL owners, not from the new projection/layout constants.
            let columns = if index < 7 {
                let sql = if index == 6 {
                    include_str!("../scope_history.rs")
                } else {
                    include_str!("../../source_succession_schema.sql")
                };
                let prefix = format!("CREATE TABLE {} (", layout.table);
                let body = sql
                    .split_once(&prefix)
                    .unwrap()
                    .1
                    .split_once("\n);")
                    .unwrap()
                    .0;
                schema_db
                    .execute_batch(&format!("{prefix}{body}\n);"))
                    .unwrap();
                schema_db
                    .prepare("SELECT name FROM pragma_table_info(?) ORDER BY cid")
                    .unwrap()
                    .query_map([layout.table], |r| r.get::<_, String>(0))
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap()
            } else {
                [
                    "profile",
                    "source_id",
                    "succession_revision",
                    "scope_kind",
                    "scope_key",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect()
            };
            assert_eq!(columns, layout.physical);
            db.execute_batch(&format!(
                "CREATE TABLE {} ({})",
                layout.table,
                columns.join(",")
            ))
            .unwrap();
            let parameters = vec!["?"; layout.physical.len()].join(",");
            for row in 0..2 {
                let cells = layout
                    .physical
                    .iter()
                    .enumerate()
                    .map(|(i, key)| match *key {
                        "profile" => SqlValue::Text("p".into()),
                        "source_id" => SqlValue::Text("s".into()),
                        "succession_revision" => SqlValue::Integer(1),
                        "file_id" => SqlValue::Text(if row == 0 { "z" } else { "a" }.into()),
                        _ if key.ends_with("_json") => {
                            SqlValue::Text("{\"z\": [null, 1], \"a\": \"\\\\\\n\\\"é\"}".into())
                        }
                        _ => match (i + row) % 8 {
                            0 => SqlValue::Null,
                            1 => SqlValue::Integer(i64::MIN),
                            2 => SqlValue::Integer(i64::MAX),
                            3 => SqlValue::Real(-0.0),
                            4 => SqlValue::Text("quote\" slash\\ newline\n雪".into()),
                            5 => SqlValue::Real(1.25e-17),
                            6 => SqlValue::Blob(vec![0, 1, 255]),
                            _ => SqlValue::Real(f64::INFINITY),
                        },
                    })
                    .collect::<Vec<_>>();
                db.execute(
                    &format!("INSERT INTO {} VALUES({parameters})", layout.table),
                    rusqlite::params_from_iter(cells),
                )
                .unwrap();
            }
        }
        db
    }
    pub(crate) fn legacy(db: &Connection, table: Table) -> Vec<Value> {
        let layout = LAYOUTS[table.index()];
        // Independent original producer: discover physical columns via PRAGMA,
        // include ALL columns, and reproduce its original complete row order.
        let columns: Vec<String> = db
            .prepare("SELECT name FROM pragma_table_info(?) ORDER BY cid")
            .unwrap()
            .query_map([layout.table], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let ordering = columns
            .iter()
            .map(|c| format!("\"{c}\" COLLATE BINARY"))
            .collect::<Vec<_>>()
            .join(",");
        let mut args = vec![SqlValue::Text("p".into()), SqlValue::Text("s".into())];
        let filter = if table.index() == 0 {
            "profile=? AND source_id=?"
        } else {
            args.push(SqlValue::Integer(1));
            "profile=? AND source_id=? AND succession_revision=?"
        };
        crate::migration::rows(
            db,
            &format!(
                "SELECT * FROM {} WHERE {filter} ORDER BY {ordering}",
                layout.table
            ),
            &args,
        )
        .unwrap()
    }
    pub(crate) fn fixed(db: &Connection, table: Table, reader: &mut Reader) -> Vec<FixedRow> {
        reader
            .rows(
                db,
                table,
                "p",
                "s",
                if table.index() == 0 { None } else { Some(1) },
                &mut Meter::default(),
            )
            .unwrap()
    }
    #[test]
    fn fixed_rows_match_all_legacy_columns_scalar_encodings_and_order() {
        let db = fixture();
        let mut reader = Reader::new();
        for table in TABLES {
            let old = legacy(&db, table);
            let rows = fixed(&db, table, &mut reader);
            assert_eq!(rows.len(), 2);
            for (actual, expected) in rows.into_iter().zip(old) {
                assert_eq!(serde_json::to_value(&actual).unwrap(), expected);
            }
        }
    }
    #[test]
    fn fixed_rows_refuse_extra_missing_reordered_shapes_and_wrong_revision_mode() {
        for alteration in [
            "ALTER TABLE catalog_source_succession_heads ADD COLUMN extra TEXT",
            "ALTER TABLE catalog_source_succession_heads RENAME COLUMN deleted TO absent",
        ] {
            let db = fixture();
            db.execute_batch(alteration).unwrap();
            assert!(
                Reader::new()
                    .rows(&db, Table::Heads, "p", "s", Some(1), &mut Meter::default())
                    .is_err()
            );
        }
        let db = fixture();
        db.execute_batch("ALTER TABLE catalog_source_succession_heads RENAME TO old_heads; CREATE TABLE catalog_source_succession_heads AS SELECT source_id,profile,succession_revision,entity_kind,entity_key,revision,deleted FROM old_heads").unwrap();
        assert!(
            Reader::new()
                .rows(&db, Table::Heads, "p", "s", Some(1), &mut Meter::default())
                .is_err()
        );
        assert!(
            Reader::new()
                .rows(&db, Table::Header, "p", "s", Some(1), &mut Meter::default())
                .is_err()
        );
    }
    #[test]
    fn fixed_rows_admit_storage_before_output_and_do_not_cache_statement_shape() {
        let db = fixture();
        let mut reader = Reader::new();
        let rows = fixed(&db, Table::Files, &mut reader);
        assert_eq!(rows.len(), 2);
        db.execute_batch("ALTER TABLE catalog_source_succession_files ADD COLUMN new_payload TEXT")
            .unwrap();
        assert!(
            reader
                .rows(&db, Table::Files, "p", "s", Some(1), &mut Meter::default())
                .is_err()
        );
        let mut denied = Meter {
            limit: Some(0),
            ..Default::default()
        };
        assert!(
            Reader::new()
                .rows(&db, Table::Header, "p", "s", None, &mut denied)
                .is_err()
        );
        assert!(denied.failed);
        assert!(denied.charge(BudgetCharge::default()).is_err());
    }
    #[test]
    fn authored_layout_proof_rejects_duplicate_out_of_range_mismatch_and_order() {
        for lexical in [
            &[("a", 0), ("a", 1)][..],
            &[("a", 0), ("b", 2)][..],
            &[("a", 1), ("b", 0)][..],
            &[("b", 1), ("a", 0)][..],
        ] {
            let bad = Layout {
                table: "test",
                physical: &["a", "b"],
                lexical,
                count_sql: "",
                rows_sql: "",
            };
            assert!(!valid_layout(&bad));
        }
        assert!(valid_layout(&Layout {
            table: "test",
            physical: &["b", "a"],
            lexical: &[("a", 1), ("b", 0)],
            count_sql: "",
            rows_sql: ""
        }));
    }
}
