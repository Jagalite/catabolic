//! Descriptive complete context for a single-file observation. This is NOT a
//! CatalogManifest, playable/current-version proof, or occurrence authority.
//! All structural validation is shared with the existing identity owner; only
//! occurrence freshness is deliberately left to the exact-file observation owner.
use super::*;
use crate::operation_budget::BudgetCharge;

/// Borrowed admission lets the caller retain its cumulative local proof budget
/// as well as the enclosing operation budget before owner decoding/allocation.
pub(crate) trait StructuralReadBudget {
    #[cfg(test)]
    fn trace_phase(&self, _phase: &str) {}
    fn charge(&mut self, charge: BudgetCharge) -> crate::Result<()>;
    fn raw_json(&mut self, raw: &str) -> crate::Result<()>;
}

pub(crate) struct StructuralContext {
    context_id: String,
    versions: Vec<ManifestVersion>,
    segmentation_sets: Vec<SegmentationSet>,
    heads: Vec<(String, String, i64)>,
}
impl StructuralContext {
    /// Transfer already admitted descriptive buffers without cloning or encoding
    /// them again. This consumes the description, not a mutation capability.
    pub(crate) fn into_source_parts(
        self,
    ) -> (String, Vec<ManifestVersion>, Vec<(String, String, i64)>) {
        (self.context_id, self.versions, self.heads)
    }
    pub(crate) fn context_id(&self) -> &str {
        &self.context_id
    }
    pub(crate) fn versions(&self) -> &[ManifestVersion] {
        &self.versions
    }
    pub(crate) fn into_heads(self) -> Vec<(String, String, i64)> {
        self.heads
    }
    pub(crate) fn heads(&self) -> &[(String, String, i64)] {
        &self.heads
    }
    pub(crate) fn segmentation_sets(&self) -> &[SegmentationSet] {
        &self.segmentation_sets
    }
}

/// Preserve complete context membership, associations, extents, timeline heads,
/// equivalence and segmentation validation without requiring any unrelated file
/// to be available. The caller must independently verify its selected occurrence.
pub(crate) fn load_structural_context_in(
    store: &Store,
    profile: &str,
    version_id: &str,
    admission: &mut impl StructuralReadBudget,
) -> crate::Result<StructuralContext> {
    load_structural_context_in_with_control_workspace(store, profile, version_id, admission, None)
}
pub(crate) fn load_structural_context_in_with_control_workspace(
    store: &Store,
    profile: &str,
    version_id: &str,
    admission: &mut impl StructuralReadBudget,
    control: Option<&crate::migration::BootstrapWorkspace<'_>>,
) -> crate::Result<StructuralContext> {
    // Descriptive-only read: every queried typed/ledger domain exists in40.
    // Mutation and authority revalidation remain separately41-only.
    if !matches!(store.schema_version, 40 | 41)
        || !store.writable
        || store.db.is_autocommit()
        || [profile, version_id]
            .iter()
            .any(|v| v.is_empty() || v.len() > 256 || v.chars().any(char::is_control))
    {
        return Err(StoreError(
            "invalid retained structural context request".into(),
        ));
    }
    store.transaction_available_with_control_workspace(control)?;
    let db = &store.db;
    let budget = store.catalog_operation_budget()?;
    budget
        .check()
        .map_err(|e| StoreError(format!("structural context budget: {e:?}")))?;
    require_live_version(db, version_id)?;
    let live = live_versions(db)?;
    let mut query = db.prepare(&format!(
        "SELECT segmentation_context_id FROM {live} WHERE id=?"
    ))?;
    let mut records = query.query([version_id])?;
    let row = records
        .next()?
        .ok_or_else(|| StoreError("structural context version missing".into()))?;
    let context = row
        .get_ref(0)?
        .as_str()
        .map_err(|_| StoreError("structural context ID is not text".into()))?;
    if context.is_empty() || context.len() > 256 {
        return Err(StoreError("structural context ID bound".into()));
    }
    admission.charge(BudgetCharge {
        bytes: context.len(),
        values: 1,
        work: 1,
        ..Default::default()
    })?;
    let context = context.to_owned();
    let members: i64 = db.query_row(
        &format!("SELECT count(*) FROM {live} WHERE segmentation_context_id=?"),
        [&context],
        |r| r.get(0),
    )?;
    let bindings:i64=db.query_row(&format!("SELECT count(*) FROM {live} v JOIN catalog_version_bindings m ON m.version_id=v.id AND m.version_revision=v.current_revision WHERE v.segmentation_context_id=?"),[&context],|r|r.get(0))?;
    if !(1..=64).contains(&members) || !(0..=1024).contains(&bindings) {
        return Err(StoreError(
            "structural complete context member/binding bound".into(),
        ));
    }
    // Same conservative row representation bounds as the selected-current owner:
    // six escaped JSON bytes per text byte plus numeric/field punctuation. The
    // raw equivalence body is separately precharged before its existing decoder.
    let revision_bytes:i64=db.query_row(&format!("SELECT coalesce(sum(1024+6*(length(CAST(v.id AS BLOB))+length(CAST(v.association_item_id AS BLOB))+length(CAST(t.edition_item_id AS BLOB))+length(CAST(r.timeline_id AS BLOB))+length(CAST(r.state AS BLOB))+coalesce(length(CAST(r.equivalence_basis AS BLOB)),0)+coalesce(length(CAST(r.equivalence_evidence_id AS BLOB)),0)+length(CAST(r.command_catalog_id AS BLOB))+length(CAST(r.command_host_namespace AS BLOB))+length(CAST(r.command_operation_id AS BLOB)))),0) FROM {live} v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision JOIN catalog_timelines t ON t.id=r.timeline_id WHERE v.segmentation_context_id=?"),[&context],|r|r.get(0))?;
    let binding_bytes:i64=db.query_row(&format!("SELECT coalesce(sum(1024+6*(length(CAST(b.association_id AS BLOB))+length(CAST(r.binding_id AS BLOB))+length(CAST(r.profile AS BLOB))+length(CAST(r.file_id AS BLOB))+length(CAST(r.file_revision AS BLOB))+length(CAST(r.source_kind AS BLOB))+coalesce(length(CAST(r.normalization_evidence_id AS BLOB)),0)+length(CAST(r.command_catalog_id AS BLOB))+length(CAST(r.command_host_namespace AS BLOB))+length(CAST(r.command_operation_id AS BLOB)))),0) FROM {live} v JOIN catalog_version_bindings m ON m.version_id=v.id AND m.version_revision=v.current_revision JOIN catalog_bindings b ON b.id=m.binding_id JOIN catalog_binding_revisions r ON r.binding_id=m.binding_id AND r.revision=m.binding_revision WHERE v.segmentation_context_id=?"),[&context],|r|r.get(0))?;
    // Existing validation also materializes complete primary associations and
    // timeline-history rows. Charge those independent inputs before decoding;
    // extra associations cannot hide behind the typed binding count.
    let (associations, association_bytes):(i64,i64)=db.query_row(&format!("SELECT count(*),coalesce(sum(256+6*(length(CAST(a.id AS BLOB))+length(CAST(a.file_id AS BLOB)))),0) FROM {live} v JOIN item_files a ON a.item_id=v.association_item_id AND a.role='primary' AND a.active=1 WHERE v.segmentation_context_id=?"),[&context],|r|Ok((r.get(0)?,r.get(1)?)))?;
    if associations != bindings {
        return Err(StoreError(
            "structural active association inventory differs".into(),
        ));
    }
    let timeline_bytes:i64=db.query_row(&format!("SELECT coalesce(sum(1024+6*(length(CAST(tr.timeline_id AS BLOB))+length(CAST(tr.identity_state AS BLOB))+length(CAST(tr.command_catalog_id AS BLOB))+length(CAST(tr.command_host_namespace AS BLOB))+length(CAST(tr.command_operation_id AS BLOB)))),0) FROM {live} v JOIN catalog_version_revisions vr ON vr.version_id=v.id AND vr.revision=v.current_revision JOIN catalog_timeline_revisions tr ON tr.timeline_id=vr.timeline_id AND tr.revision=vr.timeline_revision WHERE v.segmentation_context_id=?"),[&context],|r|r.get(0))?;
    let bytes = revision_bytes
        .checked_add(binding_bytes)
        .and_then(|n| n.checked_add(association_bytes))
        .and_then(|n| n.checked_add(timeline_bytes))
        .filter(|n| {
            [
                revision_bytes,
                binding_bytes,
                association_bytes,
                timeline_bytes,
            ]
            .iter()
            .all(|n| *n >= 0)
                && *n <= 4 * 1024 * 1024
        })
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| StoreError("structural complete context byte bound".into()))?;
    // Reserve SQL-row, typed-model, validation and segmentation nodes together.
    // Each version/binding has fewer than 64 scalar/container nodes across
    // those simultaneous representations; decoded evidence is separate above.
    let values = usize::try_from(members * 64 + bindings * 64)
        .map_err(|_| StoreError("structural row count overflow".into()))?;
    admission.charge(BudgetCharge {
        bytes,
        values,
        work: bytes / 64 + values + 1,
        items: members as usize,
    })?;
    let mut evidence = db.prepare(&format!("SELECT e.evidence_json FROM {live} v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision JOIN catalog_identity_evidence e ON e.id=r.equivalence_evidence_id WHERE v.segmentation_context_id=? ORDER BY v.id"))?;
    let mut evidence_rows = evidence.query([&context])?;
    while let Some(row) = evidence_rows.next()? {
        let raw = row
            .get_ref(0)?
            .as_str()
            .map_err(|_| StoreError("structural evidence is not text".into()))?;
        // validate_equivalence retains the raw string, decoded body, cloned
        // claims Value and typed claims at once. Four complete representations
        // conservatively cover the claims subset without first allocating it.
        for _ in 0..4 {
            admission.raw_json(raw)?;
        }
    }
    let heads = complete_heads(db, &live, &context, admission)?;
    validate_context(db, &context)?;
    let versions = load_context(db, profile, &context)?;
    if versions.len() != members as usize
        || versions
            .iter()
            .map(|v| v.version.bindings.len())
            .sum::<usize>()
            != bindings as usize
        || !versions.iter().any(|v| v.version.version_id == version_id)
    {
        return Err(StoreError(
            "structural complete context join differs".into(),
        ));
    }
    // An empty selected set means no occurrence checks, NOT no structural
    // validation. The same owner verifies every association and timeline here.
    validate_active_selected(db, profile, &versions, Some(&BTreeSet::new()))?;
    let segmentation_sets = build_sets(&context, &versions)?;
    budget
        .check()
        .map_err(|e| StoreError(format!("structural context completion: {e:?}")))?;
    store.transaction_available_with_control_workspace(control)?;
    Ok(StructuralContext {
        context_id: context,
        versions,
        segmentation_sets,
        heads,
    })
}

// Permanent counters bind the whole structural closure, including identical
// revisions and association/parent-edge away-and-back transitions. Work metadata
// is not part of this context DTO; the observation owner tracks that separately.
fn complete_heads_cte(live: &str) -> String {
    format!(
        r#"WITH versions AS (
      SELECT v.id,v.association_item_id,v.current_revision,r.timeline_id,r.timeline_revision,t.edition_item_id,r.equivalence_evidence_id
      FROM {live} v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision
      JOIN catalog_timelines t ON t.id=r.timeline_id WHERE v.segmentation_context_id=?1
    ), bindings AS (
      SELECT m.*,b.association_id FROM versions v JOIN catalog_version_bindings m ON m.version_id=v.id AND m.version_revision=v.current_revision JOIN catalog_bindings b ON b.id=m.binding_id
    ), wanted(kind,key) AS (
      SELECT 'catalog_segmentation_contexts',json_array(id) FROM catalog_segmentation_contexts WHERE id=?1
      UNION SELECT 'catalog_segmentation_revisions',json_array(id,current_revision) FROM catalog_segmentation_contexts WHERE id=?1
      UNION SELECT 'catalog_versions',json_array(id) FROM versions
      UNION SELECT 'catalog_version_revisions',json_array(id,current_revision) FROM versions
      UNION SELECT 'catalog_timelines',json_array(timeline_id) FROM versions
      UNION SELECT 'catalog_timeline_revisions',json_array(timeline_id,timeline_revision) FROM versions
      UNION SELECT 'items',json_array(association_item_id) FROM versions
      UNION SELECT 'items',json_array(edition_item_id) FROM versions
      UNION SELECT 'catalog_logical_retirements',json_array(r.item_id) FROM catalog_logical_retirements r WHERE r.item_id IN(SELECT association_item_id FROM versions UNION SELECT edition_item_id FROM versions)
      UNION SELECT 'item_files',json_array(association_id) FROM bindings
      UNION SELECT 'catalog_bindings',json_array(binding_id) FROM bindings
      UNION SELECT 'catalog_binding_revisions',json_array(binding_id,binding_revision) FROM bindings
      UNION SELECT 'catalog_version_bindings',json_array(version_id,version_revision,binding_id) FROM bindings
      UNION SELECT 'catalog_identity_evidence',json_array(equivalence_evidence_id) FROM versions WHERE equivalence_evidence_id IS NOT NULL
      UNION SELECT 'item_relationships',json_array(e.id) FROM item_relationships e WHERE e.active=1 AND (
        (e.kind='custom:motion_edition' AND e.target_id IN(SELECT edition_item_id FROM versions)) OR
        (e.kind='custom:motion_version' AND e.target_id IN(SELECT association_item_id FROM versions)))
    ) "#
    )
}

fn complete_heads(
    db: &Connection,
    live: &str,
    context: &str,
    admission: &mut impl StructuralReadBudget,
) -> crate::Result<Vec<(String, String, i64)>> {
    let cte = complete_heads_cte(live);
    let (count,bytes):(i64,i64)=db.query_row(&format!("{cte} SELECT count(*),coalesce(sum(256+6*(length(CAST(kind AS BLOB))+length(CAST(key AS BLOB)))),0) FROM wanted"),[context],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let count = usize::try_from(count)
        .ok()
        .filter(|count| *count <= catabolic_core::catalog_metadata::MAX_METADATA_JSON_NODES)
        .ok_or_else(|| StoreError("structural permanent head count bound".into()))?;
    let bytes = usize::try_from(bytes)
        .ok()
        .filter(|bytes| *bytes <= 4 * 1024 * 1024)
        .ok_or_else(|| StoreError("structural permanent head byte bound".into()))?;
    let values = count
        .checked_mul(4)
        .ok_or_else(|| StoreError("structural permanent head overflow".into()))?;
    admission.charge(BudgetCharge {
        bytes,
        values,
        work: bytes / 64 + values + 1,
        ..Default::default()
    })?;
    let mut query=db.prepare(&format!("{cte} SELECT w.kind,w.key,e.revision,e.deleted FROM wanted w LEFT JOIN catalog_entity_revisions e ON e.entity_kind=w.kind AND e.entity_key=w.key ORDER BY w.kind,w.key"))?;
    let mut rows = query.query([context])?;
    let mut heads = Vec::with_capacity(count);
    while let Some(row) = rows.next()? {
        let revision: Option<i64> = row.get(2)?;
        let deleted: Option<bool> = row.get(3)?;
        let revision = revision
            .filter(|revision| *revision > 0)
            .filter(|_| deleted == Some(false))
            .ok_or_else(|| StoreError("structural permanent head missing/inconsistent".into()))?;
        heads.push((row.get(0)?, row.get(1)?, revision));
    }
    if heads.len() != count {
        return Err(StoreError(
            "structural permanent head inventory changed".into(),
        ));
    }
    Ok(heads)
}

/// Same-writer terminal check against the COMPLETE current closure. Reuses the
/// initial decoded structural validation only after every required permanent
/// counter and the complete key set match, including added/removed parent edges.
pub(crate) fn revalidate_structural_heads_in(
    store: &Store,
    context: &str,
    expected: &[(String, String, i64)],
    admission: &mut impl StructuralReadBudget,
) -> crate::Result<()> {
    if store.schema_version != 41 || !store.writable || store.db.is_autocommit() {
        return Err(StoreError(
            "retained structural revalidation requires candidate writer".into(),
        ));
    }
    store.transaction_available()?;
    let live = live_versions(&store.db)?;
    let cte = complete_heads_cte(&live);
    // Preserve the same complete-set admission bounds as initial materialization.
    // Terminal comparison never allocates a second owned/JSON representation.
    let (count, encoded_bytes): (i64, i64) = store.db.query_row(
        &format!("{cte} SELECT count(*),coalesce(sum(256+6*(length(CAST(kind AS BLOB))+length(CAST(key AS BLOB)))),0) FROM wanted"),
        [context], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let count = usize::try_from(count)
        .ok()
        .filter(|n| *n <= catabolic_core::catalog_metadata::MAX_METADATA_JSON_NODES)
        .ok_or_else(|| StoreError("structural permanent head count bound".into()))?;
    if count != expected.len() || !(0..=4 * 1024 * 1024).contains(&encoded_bytes) {
        return Err(StoreError(
            "structural permanent head inventory changed or exceeded bound".into(),
        ));
    }
    let mut query = store.db.prepare(&format!("{cte} SELECT w.kind,w.key,e.revision,e.deleted FROM wanted w LEFT JOIN catalog_entity_revisions e ON e.entity_kind=w.kind AND e.entity_key=w.key ORDER BY w.kind,w.key"))?;
    let mut rows = query.query([context])?;
    compare_head_rows(&mut rows, expected, admission)?;
    store.transaction_available()
}

fn compare_head_rows(
    rows: &mut rusqlite::Rows<'_>,
    expected: &[(String, String, i64)],
    admission: &mut impl StructuralReadBudget,
) -> crate::Result<()> {
    let mut seen = 0usize;
    while let Some(row) = rows.next()? {
        let rusqlite::types::ValueRef::Text(kind) = row.get_ref(0)? else {
            return Err(StoreError("structural head kind is not text".into()));
        };
        let rusqlite::types::ValueRef::Text(key) = row.get_ref(1)? else {
            return Err(StoreError("structural head key is not text".into()));
        };
        let bytes = kind
            .len()
            .checked_add(key.len())
            .and_then(|n| n.checked_add(16))
            .filter(|n| *n <= 4 * 1024 * 1024)
            .ok_or_else(|| StoreError("structural head cell byte bound".into()))?;
        // Borrowed UTF-8 cells plus the two SQL integer scalars, not an escaped
        // JSON value or a cloned String/Vec. Every row still consumes cumulative
        // read bytes, nodes and work before its values are compared.
        admission.charge(BudgetCharge {
            bytes,
            values: 4,
            work: bytes / 64 + 5,
            ..Default::default()
        })?;
        let kind = std::str::from_utf8(kind)
            .map_err(|_| StoreError("invalid structural kind UTF-8".into()))?;
        let key = std::str::from_utf8(key)
            .map_err(|_| StoreError("invalid structural key UTF-8".into()))?;
        let revision: Option<i64> = row.get(2)?;
        let deleted: Option<bool> = row.get(3)?;
        let prior = expected
            .get(seen)
            .ok_or_else(|| StoreError("structural permanent head added".into()))?;
        if kind != prior.0
            || key != prior.1
            || revision != Some(prior.2)
            || prior.2 < 1
            || deleted != Some(false)
        {
            return Err(StoreError(
                "complete structural permanent head set changed".into(),
            ));
        }
        seen = seen
            .checked_add(1)
            .ok_or_else(|| StoreError("structural head count overflow".into()))?;
    }
    if seen != expected.len() {
        return Err(StoreError("structural permanent head removed".into()));
    }
    Ok(())
}

#[cfg(test)]
mod streaming_head_tests {
    use super::*;
    #[derive(Default)]
    struct Admission {
        bytes: usize,
        work: usize,
        max_work: usize,
    }
    impl StructuralReadBudget for Admission {
        fn charge(&mut self, charge: BudgetCharge) -> crate::Result<()> {
            self.bytes = self
                .bytes
                .checked_add(charge.bytes)
                .ok_or_else(|| StoreError("test cumulative bytes overflow".into()))?;
            self.work = self
                .work
                .checked_add(charge.work)
                .filter(|n| *n <= self.max_work)
                .ok_or_else(|| StoreError("test cumulative work refused".into()))?;
            Ok(())
        }
        fn raw_json(&mut self, _: &str) -> crate::Result<()> {
            panic!("streaming comparison must not materialize JSON")
        }
    }
    fn compare(
        sql: &str,
        expected: &[(String, String, i64)],
        budget: &mut Admission,
    ) -> crate::Result<()> {
        // Pure SQL-row comparator fixture, not a catalog or ownership grant.
        let db = Connection::open_in_memory()?;
        let mut query = db.prepare(sql)?;
        compare_head_rows(&mut query.query([])?, expected, budget)
    }
    fn expected() -> Vec<(String, String, i64)> {
        vec![("a".into(), "[1]".into(), 1), ("b".into(), "[2]".into(), 2)]
    }
    const EXACT: &str = "SELECT 'a','[1]',1,0 UNION ALL SELECT 'b','[2]',2,0";
    #[test]
    fn streaming_heads_preserve_complete_order_identity_revision_and_deletion() {
        let mut budget = Admission {
            max_work: usize::MAX,
            ..Default::default()
        };
        compare(EXACT, &expected(), &mut budget).unwrap();
        assert_eq!(budget.bytes, 40); // two borrowed text pairs and four i64 cells
        assert_eq!(budget.work, 10);
        for sql in [
            "SELECT 'a','[1]',1,0",
            "SELECT 'a','[1]',1,0 UNION ALL SELECT 'b','[2]',2,0 UNION ALL SELECT 'c','[3]',3,0",
            "SELECT 'b','[2]',2,0 UNION ALL SELECT 'a','[1]',1,0",
            "SELECT 'a','[1]',2,0 UNION ALL SELECT 'b','[2]',2,0",
            "SELECT 'a','[1]',1,1 UNION ALL SELECT 'b','[2]',2,0",
            "SELECT 'a','[1]',NULL,NULL UNION ALL SELECT 'b','[2]',2,0",
            "SELECT 'a','[9]',1,0 UNION ALL SELECT 'b','[2]',2,0",
        ] {
            assert!(
                compare(
                    sql,
                    &expected(),
                    &mut Admission {
                        max_work: usize::MAX,
                        ..Default::default()
                    }
                )
                .is_err()
            );
        }
    }
    #[test]
    fn streaming_heads_refuse_huge_cells_and_cumulative_overflow_before_comparison() {
        let mut budget = Admission {
            max_work: usize::MAX,
            ..Default::default()
        };
        assert!(
            compare(
                "SELECT 'a',CAST(zeroblob(4194305) AS TEXT),1,0",
                &expected(),
                &mut budget
            )
            .is_err()
        );
        assert_eq!(budget.bytes, 0);
        let mut budget = Admission {
            bytes: usize::MAX,
            max_work: usize::MAX,
            ..Default::default()
        };
        assert!(compare(EXACT, &expected(), &mut budget).is_err());
        let mut budget = Admission {
            max_work: 9,
            ..Default::default()
        };
        assert!(compare(EXACT, &expected(), &mut budget).is_err());
    }
}

/// Fixed candidate41 SQL descriptor for source succession membership only.
/// No caller-provided relation, context identifier, or SQL fragment is accepted.
pub(crate) fn source_succession_heads_cte() -> String {
    let sql = complete_heads_cte("catalog_live_versions");
    assert_eq!(
        sql.matches("?1").count(),
        3,
        "structural CTE parameter contract changed"
    );
    sql.replace("?1", "scope.context_id")
}
