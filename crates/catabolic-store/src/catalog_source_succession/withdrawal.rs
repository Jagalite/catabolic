//! Database-only withdrawal facts from genuine completed native custody.
use crate::{
    Error, Result, Store,
    migration::CompletedSourceWithdrawalAdmission,
    operation_budget::{BudgetCharge, OperationBudget},
};
use serde_json::Value;
pub(crate) struct CapturedSourceWithdrawal {
    profile: String,
    source: String,
    motion_source: String,
    snapshot: Value,
    libraries: Vec<(String, i64, Value)>,
    memberships: u64,
    unresolved: bool,
}
impl CapturedSourceWithdrawal {
    pub(crate) fn profile(&self) -> &str {
        &self.profile
    }
    pub(crate) fn native_source(&self) -> &str {
        &self.source
    }
    pub(crate) fn motion_source_id(&self) -> &str {
        &self.motion_source
    }
    pub(crate) fn catalog_id(&self) -> &str {
        self.snapshot["catalog_id"]
            .as_str()
            .expect("validated capture")
    }
    pub(crate) fn incarnation(&self) -> &str {
        self.snapshot["catalog_incarnation"]
            .as_str()
            .expect("validated capture")
    }
    pub(crate) fn generation(&self) -> i64 {
        self.snapshot["generation"]
            .as_i64()
            .expect("validated capture")
    }
    pub(crate) fn binding(&self) -> &Value {
        &self.snapshot["binding"]
    }
    pub(crate) fn policy(&self) -> &Value {
        &self.snapshot["policy"]
    }
    pub(crate) fn retained_files(&self) -> u64 {
        self.snapshot["occurrences"]
            .as_array()
            .expect("validated capture")
            .len() as u64
    }
    pub(crate) fn logical_memberships(&self) -> u64 {
        self.memberships
    }
    pub(crate) fn native_ownership_unresolved(&self) -> bool {
        self.unresolved
    }
}
fn refused() -> Error {
    Error("completed source withdrawal facts refused or changed".into())
}
fn charge(b: &OperationBudget, c: BudgetCharge) -> Result<()> {
    b.charge(c).map_err(|_| refused())
}
// Measure borrowed cells before generic row allocation. Parsed JSON reserves a
// conservative node/storage bound; plain text reserves 8x for retained strings
// and worst-case six-byte escaping in the occurrence revision encoder. Not total RSS.
fn preflight(
    store: &Store,
    b: &mut impl crate::catalog_identity_mutation::StructuralReadBudget,
    sql: &str,
    profile: &str,
    source: &str,
    copies: usize,
) -> Result<()> {
    let mut stmt = store.db.prepare(sql)?;
    let cols = stmt.column_count();
    let mut rows = stmt.query([profile, source])?;
    while let Some(row) = rows.next()? {
        b.charge(BudgetCharge {
            items: copies,
            values: cols.saturating_mul(copies),
            work: cols.saturating_mul(copies),
            bytes: cols.saturating_mul(128).saturating_mul(copies),
        })?;
        for col in 0..cols {
            let cell = row.get_ref(col)?;
            if matches!(cell, rusqlite::types::ValueRef::Blob(_)) {
                return Err(refused());
            }
            if let rusqlite::types::ValueRef::Text(raw) = cell {
                let json = matches!(row.as_ref().column_name(col)?, "settings" | "policy");
                let bytes = raw
                    .len()
                    .checked_mul(copies)
                    .and_then(|n| n.checked_mul(if json { 128 } else { 8 }))
                    .ok_or_else(refused)?;
                b.charge(BudgetCharge {
                    bytes,
                    values: if json {
                        raw.len().saturating_mul(copies)
                    } else {
                        0
                    },
                    work: (raw.len() / 64 + 1).saturating_mul(copies),
                    ..Default::default()
                })?;
            }
        }
    }
    Ok(())
}
// This adapter preserves withdrawal's original parent-only charges and limits.
// Endpoint callers instead pass their existing local budget, never a fresh owner.
struct ParentOccurrenceBudget<'a>(&'a OperationBudget);
impl crate::catalog_identity_mutation::StructuralReadBudget for ParentOccurrenceBudget<'_> {
    fn charge(&mut self, c: BudgetCharge) -> Result<()> {
        charge(self.0, c)
    }
    fn raw_json(&mut self, _: &str) -> Result<()> {
        Err(refused())
    }
}
/// Complete current occurrences, with the original withdrawal preflight before
/// the existing native capture owner allocates. No admission or read grant.
pub(super) fn capture_occurrences_admitted_in(
    store: &Store,
    profile: &str,
    source: &str,
    additional_retained_bytes: usize,
    b: &mut impl crate::catalog_identity_mutation::StructuralReadBudget,
) -> Result<Value> {
    let count = preflight_occurrences_in(
        store,
        profile,
        source,
        additional_retained_bytes,
        OccurrenceHeader::Owned,
        b,
    )?;
    let snapshot =
        crate::catalog_import_typed::capture_source_occurrences_in(store, profile, source)?;
    if snapshot["catalog_id"].as_str() != Some(store.database_id.as_str())
        || snapshot["catalog_incarnation"].as_str() != store.catalog_incarnation.as_deref()
        || snapshot["generation"].as_i64().is_none()
        || snapshot["occurrences"].as_array().map(Vec::len) != Some(count as usize)
    {
        return Err(refused());
    }
    Ok(snapshot)
}

// This switch is private accounting, not admission. The only borrowed-header
// caller below first checks the unconstructible operation-bound fresh facts.
enum OccurrenceHeader {
    Owned,
    BorrowedFresh,
}
fn preflight_occurrences_in(
    store: &Store,
    profile: &str,
    source: &str,
    additional_retained_bytes: usize,
    header: OccurrenceHeader,
    b: &mut impl crate::catalog_identity_mutation::StructuralReadBudget,
) -> Result<i64> {
    if !store.owns_writer_lock()
        || store.db.is_autocommit()
        || store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
    {
        return Err(refused());
    }
    let count: i64 = store.db.query_row(
        "SELECT count(*) FROM files WHERE location=?",
        [source],
        |r| r.get(0),
    )?;
    if !(0..=100_000).contains(&count) {
        return Err(refused());
    }
    preflight(
        store,
        b,
        "SELECT f.id,f.location,f.path,o.size,o.mtime_ns,o.device,o.inode,o.status FROM files f LEFT JOIN observations o ON o.file_id=f.id AND o.profile=?1 WHERE f.location=?2",
        profile,
        source,
        2,
    )?;
    let copies = usize::try_from(count)
        .map_err(|_| refused())?
        .checked_add(3)
        .ok_or_else(refused)?;
    for sql in [
        "SELECT * FROM bindings WHERE profile=?1 AND kind='source' AND owner=?2",
        "SELECT * FROM source_identity_policies WHERE profile=?1 AND location=?2",
        "SELECT * FROM binding_volumes WHERE profile=?1 AND kind='source' AND owner=?2",
    ] {
        preflight(store, b, sql, profile, source, copies)?;
    }
    if matches!(header, OccurrenceHeader::Owned) {
        preflight(
            store,
            b,
            "SELECT * FROM source_observation_policies WHERE profile=?1 AND source=?2",
            profile,
            source,
            3,
        )?;
    }
    b.charge(BudgetCharge {
        bytes: copies
            .saturating_mul(4096)
            .saturating_add(profile.len())
            .saturating_add(source.len())
            .saturating_add(additional_retained_bytes),
        values: copies.saturating_mul(64),
        work: copies.saturating_mul(64),
        ..Default::default()
    })?;
    Ok(count)
}
/// Same complete occurrence rows with the genuine operation's fresh header.
/// Existing per-file binding/identity-policy/volume and constructor reservations
/// remain conservative and unchanged. Only the absent top-level scan-policy
/// read/parse is omitted; no per-file proof or full-set bound is weakened.
pub(super) fn capture_endpoint_occurrences_in(
    store: &Store,
    facts: &crate::migration::EndpointFreshFacts<'_, '_>,
    source: &str,
    b: &mut crate::catalog_nfo_attribution::ProofBudget,
) -> Result<Vec<Value>> {
    facts.require_source(store, source, b)?;
    let count = preflight_occurrences_in(
        store,
        facts.scope().profile(),
        source,
        0,
        OccurrenceHeader::BorrowedFresh,
        b,
    )?;
    let rows = crate::catalog_import_typed::capture_source_occurrence_rows_for_endpoint_in(
        store, facts, source, b,
    )?;
    if rows.len() != count as usize {
        return Err(refused());
    }
    Ok(rows)
}

fn capture_in(
    store: &Store,
    a: &CompletedSourceWithdrawalAdmission<'_>,
    profile: &str,
    source: &str,
    motion_source: &str,
) -> Result<CapturedSourceWithdrawal> {
    a.check(store, profile, source, motion_source)?;
    if !store.owns_writer_lock()
        || store.db.is_autocommit()
        || store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
    {
        return Err(refused());
    }
    let b = store.catalog_operation_budget()?;
    crate::query::name(profile)?;
    crate::query::name(source)?;
    let snapshot = capture_occurrences_admitted_in(
        store,
        profile,
        source,
        motion_source.len(),
        &mut ParentOccurrenceBudget(&b),
    )?;
    let inv = crate::native_worker_inventory::inspect_completed_source_withdrawal(
        store,
        a,
        profile,
        source,
        motion_source,
    )?;
    if !inv.native {
        return Err(refused());
    }
    let (libraries, memberships) = libraries_in(store, &b, motion_source)?;
    Ok(CapturedSourceWithdrawal {
        profile: profile.into(),
        source: source.into(),
        motion_source: motion_source.into(),
        snapshot,
        libraries,
        memberships,
        unresolved: !inv.admits_new_work(),
    })
}
// Exact selected columns of the reviewed catalog_items reference. Offline
// custody does not install temporary SQL views or change query_only settings.
const LIBRARIES: &str = "SELECT id AS item_id,metadata FROM main.items WHERE kind='custom:motion_library' AND json_type(metadata,'$.motion_library')='object' ORDER BY item_id";
fn libraries_in(
    store: &Store,
    b: &OperationBudget,
    source: &str,
) -> Result<(Vec<(String, i64, Value)>, u64)> {
    let mut stmt = store.db.prepare(LIBRARIES)?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    let mut bytes = 0usize;
    let mut memberships = 0;
    while let Some(row) = rows.next()? {
        if out.len() >= 10_000 {
            return Err(refused());
        }
        let id = row.get_ref(0)?.as_str().map_err(|_| refused())?;
        let raw = row.get_ref(1)?.as_str().map_err(|_| refused())?;
        bytes = bytes
            .checked_add(id.len())
            .and_then(|n| n.checked_add(raw.len()))
            .ok_or_else(refused)?;
        if bytes > 4 * 1024 * 1024 {
            return Err(refused());
        }
        charge(
            b,
            BudgetCharge {
                items: 1,
                bytes: raw
                    .len()
                    .checked_mul(128)
                    .and_then(|n| n.checked_add(id.len() + 512))
                    .ok_or_else(refused)?,
                values: raw.len() + 2,
                work: raw.len() + 12,
            },
        )?;
        let(revision,rich):(Option<i64>,bool)=store.db.query_row("SELECT (SELECT revision FROM catalog_entity_revisions WHERE entity_kind='items' AND entity_key=json_array(i.id) AND deleted=0), EXISTS(SELECT 1 FROM identities WHERE item_id=i.id) OR EXISTS(SELECT 1 FROM item_files WHERE item_id=i.id) OR EXISTS(SELECT 1 FROM item_relationships WHERE source_id=i.id OR target_id=i.id) OR EXISTS(SELECT 1 FROM catalog_timelines WHERE edition_item_id=i.id) OR EXISTS(SELECT 1 FROM catalog_versions WHERE association_item_id=i.id) OR EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id=i.id) FROM items i WHERE id=?",[id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let metadata: Value = serde_json::from_str(raw)?;
        let object = metadata.as_object().ok_or_else(refused)?;
        if rich
            || revision.is_none_or(|n| n < 1)
            || object.keys().any(|k| k != "title" && k != "motion_library")
        {
            return Err(refused());
        }
        let sources = metadata["motion_library"]["source_ids"]
            .as_array()
            .ok_or_else(refused)?;
        let mut contains = false;
        for value in sources {
            contains |= value.as_str().ok_or_else(refused)? == source;
        }
        memberships += u64::from(contains);
        out.push((id.to_owned(), revision.ok_or_else(refused)?, metadata));
    }
    Ok((out, memberships))
}
pub(crate) fn capture_completed_withdrawal(
    store: &Store,
    a: &CompletedSourceWithdrawalAdmission<'_>,
    profile: &str,
    source: &str,
    motion_source: &str,
) -> Result<CapturedSourceWithdrawal> {
    a.check(store, profile, source, motion_source)?;
    if !store.db.is_autocommit() {
        return Err(refused());
    }
    store.transaction_detailed(|_| capture_in(store, a, profile, source, motion_source))
}
pub(crate) fn revalidate_completed_withdrawal(
    store: &Store,
    a: &CompletedSourceWithdrawalAdmission<'_>,
    facts: &CapturedSourceWithdrawal,
) -> Result<()> {
    let now = capture_completed_withdrawal(
        store,
        a,
        facts.profile(),
        facts.native_source(),
        facts.motion_source_id(),
    )?;
    if now.snapshot != facts.snapshot
        || now.libraries != facts.libraries
        || now.memberships != facts.memberships
        || now.unresolved != facts.unresolved
    {
        return Err(refused());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture(metadata: Value) -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        crate::migration::initialize_native_at(&path, 40).unwrap();
        crate::candidate41_test_support::install_candidate41_for_test(&path).unwrap();
        let store =
            crate::candidate41_test_support::open_candidate41_for_test(&path, true).unwrap();
        crate::curation::item_put(
            &store,
            &json!({"id":"logical-library","kind":"custom:motion_library","metadata":metadata}),
        )
        .unwrap();
        (root, store)
    }
    #[test]
    fn library_membership_counts_complete_definition_once_without_query_temp_views() {
        let (_root, store) = fixture(
            json!({"title":"Library","motion_library":{"source_ids":["physical","physical","other"]}}),
        );
        let n: i64 = store
            .db
            .query_row(
                "SELECT count(*) FROM sqlite_temp_schema WHERE name='catalog_items'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
        store
            .transaction(|_| {
                let b = store.catalog_operation_budget()?;
                let (rows, n) = libraries_in(&store, &b, "physical")?;
                assert_eq!(rows.len(), 1);
                assert_eq!(n, 1);
                assert_eq!(libraries_in(&store, &b, "missing")?.1, 0);
                Ok(())
            })
            .unwrap();
    }
    #[test]
    fn matching_malformed_library_is_not_silently_omitted() {
        let (_root, store) = fixture(json!({"motion_library":{"source_ids":["physical",42]}}));
        assert!(
            store
                .transaction(|_| libraries_in(
                    &store,
                    &store.catalog_operation_budget()?,
                    "physical"
                ))
                .is_err()
        );
    }
    #[test]
    fn unsupported_library_metadata_shape_refuses_complete_capture() {
        let (_root, store) = fixture(json!({"motion_library":{"source_ids":[]},"unreviewed":{}}));
        assert!(
            store
                .transaction(|_| libraries_in(
                    &store,
                    &store.catalog_operation_budget()?,
                    "physical"
                ))
                .is_err()
        );
    }
}

#[cfg(test)]
pub(crate) fn assert_occurrence_capture_parity_for_test(
    store: &Store,
    profile: &str,
    source: &str,
) {
    store
        .transaction_detailed(|_| -> crate::Result<()> {
            // Exact original occurrence owner versus the extracted admission path,
            // in one retained snapshot/operation. This is only an output oracle.
            let expected =
                crate::catalog_import_typed::capture_source_occurrences_in(store, profile, source)?;
            let parent = store.catalog_operation_budget()?;
            let actual = capture_occurrences_admitted_in(
                store,
                profile,
                source,
                0,
                &mut ParentOccurrenceBudget(&parent),
            )?;
            assert_eq!(actual, expected);
            Ok(())
        })
        .unwrap();
}
