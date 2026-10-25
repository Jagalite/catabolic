//! Distinct named-current observation. All structural context members remain
//! validated; only requested versions may carry current occurrence facts.
use super::*;
use crate::operation_budget::BudgetCharge;

pub(crate) fn load_selected_in(
    store: &Store,
    profile: &str,
    ids: &[String],
) -> crate::Result<Value> {
    if store.db.is_autocommit()
        || ids.is_empty()
        || ids.len() > 64
        || ids.iter().collect::<BTreeSet<_>>().len() != ids.len()
    {
        return Err(StoreError(
            "invalid selected current version snapshot".into(),
        ));
    }
    let db = &store.db;
    let budget = store.catalog_operation_budget()?;
    let selected: BTreeSet<String> = ids.iter().cloned().collect();
    let mut context = None;
    for id in ids {
        require_live_version(db, id)?;
        let found: String = db.query_row(
            &format!(
                "SELECT segmentation_context_id FROM {} WHERE id=?",
                live_versions(db)?
            ),
            [id],
            |r| r.get(0),
        )?;
        if context.as_ref().is_some_and(|old| old != &found) {
            return Err(conflict("catalog_identity_context_scope_mismatch").into());
        }
        context = Some(found);
    }
    let context = context.ok_or_else(|| StoreError("selected context missing".into()))?;
    let live = live_versions(db)?;
    let context_members: i64 = db.query_row(
        &format!("SELECT count(*) FROM {live} WHERE segmentation_context_id=?"),
        [&context],
        |r| r.get(0),
    )?;
    if !(1..=64).contains(&context_members) {
        return Err(StoreError("selected complete context member bound".into()));
    }
    let bindings:i64=db.query_row(&format!("SELECT count(*) FROM {live} v JOIN catalog_version_bindings m ON m.version_id=v.id AND m.version_revision=v.current_revision WHERE v.segmentation_context_id=?"),[&context],|r|r.get(0))?;
    if !(0..=1024).contains(&bindings) {
        return Err(StoreError("selected complete context binding bound".into()));
    }
    // Conservative JSON upper bound: each text byte can expand to six escape
    // bytes, plus fixed keys/numeric punctuation per row. Check before rows()
    // allocates strings or load_context/evidence parsing creates model values.
    let revision_bytes:i64=db.query_row(&format!("SELECT coalesce(sum(1024+6*(length(CAST(v.id AS BLOB))+length(CAST(v.association_item_id AS BLOB))+length(CAST(t.edition_item_id AS BLOB))+length(CAST(r.timeline_id AS BLOB))+length(CAST(r.state AS BLOB))+coalesce(length(CAST(r.equivalence_basis AS BLOB)),0)+coalesce(length(CAST(r.equivalence_evidence_id AS BLOB)),0)+length(CAST(r.command_catalog_id AS BLOB))+length(CAST(r.command_host_namespace AS BLOB))+length(CAST(r.command_operation_id AS BLOB)))),0) FROM {live} v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision JOIN catalog_timelines t ON t.id=r.timeline_id WHERE v.segmentation_context_id=?"),[&context],|r|r.get(0))?;
    let binding_bytes:i64=db.query_row(&format!("SELECT coalesce(sum(1024+6*(length(CAST(b.association_id AS BLOB))+length(CAST(r.binding_id AS BLOB))+length(CAST(r.profile AS BLOB))+length(CAST(r.file_id AS BLOB))+length(CAST(r.file_revision AS BLOB))+length(CAST(r.source_kind AS BLOB))+coalesce(length(CAST(r.normalization_evidence_id AS BLOB)),0)+length(CAST(r.command_catalog_id AS BLOB))+length(CAST(r.command_host_namespace AS BLOB))+length(CAST(r.command_operation_id AS BLOB)))),0) FROM {live} v JOIN catalog_version_bindings m ON m.version_id=v.id AND m.version_revision=v.current_revision JOIN catalog_bindings b ON b.id=m.binding_id JOIN catalog_binding_revisions r ON r.binding_id=m.binding_id AND r.revision=m.binding_revision WHERE v.segmentation_context_id=?"),[&context],|r|r.get(0))?;
    let evidence_bytes:i64=db.query_row(&format!("SELECT coalesce(sum(length(CAST(e.evidence_json AS BLOB))),0) FROM catalog_identity_evidence e WHERE e.id IN(SELECT r.equivalence_evidence_id FROM {live} v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision WHERE v.segmentation_context_id=?)"),[&context],|r|r.get(0))?;
    let preflight = [revision_bytes, binding_bytes, evidence_bytes]
        .into_iter()
        .try_fold(0i64, |sum, n| if n < 0 { None } else { sum.checked_add(n) })
        .filter(|n| *n <= 4 * 1024 * 1024)
        .ok_or_else(|| StoreError("selected complete context byte bound".into()))?;
    budget
        .charge(BudgetCharge {
            items: context_members as usize,
            values: bindings as usize,
            bytes: preflight as usize,
            work: (context_members + bindings) as usize,
        })
        .map_err(|e| StoreError(format!("selected context budget: {e:?}")))?;
    validate_context(db, &context)?;
    let versions = load_context(db, profile, &context)?;
    if context_members as usize != versions.len()
        || bindings as usize
            != versions
                .iter()
                .map(|v| v.version.bindings.len())
                .sum::<usize>()
    {
        return Err(StoreError(
            "selected complete context member/binding join is incomplete".into(),
        ));
    }
    if serde_json::to_vec(&versions)?.len() > 4 * 1024 * 1024 {
        return Err(StoreError("selected parsed context byte bound".into()));
    }
    validate_active_selected(db, profile, &versions, Some(&selected))?;
    let sets = build_sets(&context, &versions)?;
    let selected_versions: Vec<_> = versions
        .iter()
        .filter(|v| selected.contains(&v.version.version_id))
        .collect();
    if selected_versions.len() != ids.len() {
        return Err(StoreError(
            "selected version missing from complete context".into(),
        ));
    }
    let context_heads = rows(
        db,
        &format!(
            "SELECT v.id AS version_id,v.association_item_id,v.current_revision AS version_revision,r.timeline_id,r.timeline_revision,t.current_revision AS current_timeline_revision,tr.identity_state AS timeline_identity_state,tr.coverage_start_us,tr.coverage_end_us,r.equivalence_basis,r.equivalence_evidence_id FROM {} v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision JOIN catalog_timelines t ON t.id=r.timeline_id JOIN catalog_timeline_revisions tr ON tr.timeline_id=t.id AND tr.revision=t.current_revision WHERE v.segmentation_context_id=? ORDER BY v.id LIMIT 65",
            live_versions(db)?
        ),
        &[context.clone().into()],
    )?;
    if context_heads.len() != versions.len() {
        return Err(StoreError("selected context head inventory changed".into()));
    }
    let mut occurrences = Vec::new();
    let mut seen = BTreeSet::new();
    for version in &selected_versions {
        for binding in &version.version.bindings {
            if !seen.insert(binding.file.file_id.clone()) {
                continue;
            }
            let mut snapshot = components::occurrence(db, profile, &binding.file.file_id)?;
            if snapshot["status"] != "present"
                || components::revision(&snapshot) != binding.file.revision
            {
                return Err(conflict("catalog_identity_stale_file_revision").into());
            }
            let ctime:Option<i64>=db.query_row("SELECT c.ctime_ns FROM native_source_ctimes c JOIN observations o ON o.profile=c.profile AND o.file_id=c.file_id AND o.size=c.size AND o.mtime_ns=c.mtime_ns AND o.device=c.device AND o.inode=c.inode WHERE c.profile=? AND c.file_id=? AND o.status='present'",params![profile,binding.file.file_id],|r|r.get(0)).optional()?;
            let ctime = ctime
                .ok_or_else(|| StoreError("selected occurrence lacks exact ctime proof".into()))?;
            snapshot["ctime_ns"] = json!(ctime);
            let current = crate::source::validate(&snapshot)?;
            if current["ctime_ns"] != ctime {
                return Err(StoreError("selected occurrence ctime changed".into()));
            }
            occurrences
                .push(json!({"file":binding.file,"snapshot":snapshot,"validated_stat":current}));
            budget
                .charge(BudgetCharge {
                    items: 1,
                    values: 1,
                    bytes: serde_json::to_vec(occurrences.last().unwrap())?.len(),
                    work: 1,
                })
                .map_err(|e| StoreError(format!("selected occurrence budget: {e:?}")))?;
        }
    }
    let current_context = rows(
        db,
        "SELECT c.id,c.authority_namespace,c.context_key,c.current_revision FROM catalog_segmentation_contexts c JOIN catalog_segmentation_revisions r ON r.context_id=c.id AND r.revision=c.current_revision WHERE c.id=?",
        &[context.into()],
    )?;
    if current_context.len() != 1 {
        return Err(StoreError("selected current context missing".into()));
    }
    budget
        .check()
        .map_err(|e| StoreError(format!("selected completion budget: {e:?}")))?;
    Ok(
        json!({"selected_versions":selected_versions,"current_occurrences":occurrences,"complete_context":{"owner":current_context[0],"version_heads":context_heads,"structural_versions":versions,"segmentation_sets":sets},"all_context_occurrences_current":false}),
    )
}
