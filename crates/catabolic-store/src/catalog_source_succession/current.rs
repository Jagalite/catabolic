//! One candidate41 evidence relation selection for typed and SQL readers.
//! Production40 SQL is returned byte-for-byte. This is part of the closed41
//! schema identity, which ordinary native40/41 scope snapshots already fence.
/// Candidate-only constant refusal, not a filesystem/database access function.
pub(crate) fn refusal_function(schema: u32, name: &str) -> bool {
    schema == 41
        && matches!(
            name,
            "catalog_source_evidence_refused" | "catalog_source_evidence_work"
        )
}
/// Borrow the actual admitted operation; never mint a new allowance or replace
/// a SQLite progress handler. A retained writer may enter its existing budget.
pub(crate) fn enter_read(
    store: &crate::Store,
) -> crate::Result<Option<crate::operation_budget::OperationBudgetGuard>> {
    if store.schema_version != 41 {
        return Ok(None);
    }
    let budget = if let Some(active) = crate::operation_budget::OperationBudget::current() {
        active
    } else {
        if store.db.is_autocommit() {
            // Baseline/no-transfer reads need no new source authority. If SQL
            // actually reaches transfer evidence, the UDF refuses absent scope.
            return Ok(None);
        }
        store.catalog_operation_budget()?
    };
    budget
        .enter()
        .map(Some)
        .map_err(|e| crate::Error(format!("source evidence read budget: {e:?}")))
}

/// Poll only after SQLite has returned. The work UDF itself intentionally
/// invokes no caller cancellation callback from inside SQLite.
pub(crate) fn finish_read() -> crate::Result<()> {
    if let Some(budget) = crate::operation_budget::OperationBudget::current() {
        budget
            .check()
            .map_err(|e| crate::Error(format!("source evidence read budget: {e:?}")))?;
    }
    Ok(())
}

/// SQLite emits database=None/column="" for a synthetic COUNT(*) view read.
/// Admit only these closed candidate41 relations, never a prefix or new table.
pub(crate) fn synthetic_read_view(schema: u32, name: &str) -> bool {
    schema == 41
        && matches!(
            name,
            "catalog_source_current_context_membership"
                | "catalog_source_required_scopes"
                | "catalog_source_current_scope_effects"
                | "catalog_source_current_head_effects"
                | "catalog_source_bounded_evidence"
                | "catalog_source_live_transfers"
                | "catalog_source_transferred_facts"
                | "catalog_source_effective_file_facts"
                | "catalog_source_transferred_headers"
                | "catalog_source_effective_headers"
                | "catalog_source_effective_assertions"
                | "catalog_source_recorded_occurrence_transfers"
                | "catalog_source_current_occurrence_transfers"
        )
}
pub(crate) fn relation(schema: u32, base: &'static str) -> &'static str {
    if schema != 41 {
        return base;
    }
    match base {
        "file_facts" => "catalog_source_effective_file_facts",
        "media_header_checks" => "catalog_source_effective_headers",
        "component_assertions" => "catalog_source_effective_assertions",
        _ => base,
    }
}
pub(crate) fn view(schema: u32, original: &str) -> String {
    if schema != 41 {
        return original.to_owned();
    }
    let mut sql = original.replace("h.scan_id=o.scan_id", "h.scan_id IS o.scan_id");
    for base in ["file_facts", "media_header_checks", "component_assertions"] {
        sql = sql.replace(
            &format!("main.{base}"),
            &format!("main.{}", relation(schema, base)),
        );
        for prefix in ["FROM ", "JOIN "] {
            sql = sql.replace(
                &format!("{prefix}{base} "),
                &format!("{prefix}{} ", relation(schema, base)),
            );
        }
    }
    // Do not pretend an old probe job ran against a successor snapshot. The
    // query retains historical occurrence rows but gates renewed currentness on
    // the exact permanent transfer and its still-current complete head closure.
    sql.replace(
        "coalesce(o.status='present' AND (a.active=1",
        "coalesce(o.status='present' AND (c.probe_job_id IS NULL OR j.operation<>'probe' OR c.snapshot=j.snapshot OR EXISTS (SELECT 1 FROM main.catalog_source_current_occurrence_transfers t WHERE t.profile=c.profile AND t.file_id=c.file_id AND t.occurrence_id=c.id)) AND (NOT EXISTS (SELECT 1 FROM main.catalog_source_recorded_occurrence_transfers t WHERE t.profile=c.profile AND t.file_id=c.file_id AND t.occurrence_id=c.id) OR EXISTS (SELECT 1 FROM main.catalog_source_current_occurrence_transfers t WHERE t.profile=c.profile AND t.file_id=c.file_id AND t.occurrence_id=c.id)) AND (a.active=1",
    )
}
/// Internal fixed selector, never a caller SQL fragment or mutation grant.
pub(crate) enum ReadSeed {
    Location,
    File,
}

/// Two leading bound values are (profile, location-or-file). Inline the exact
/// immutable reviewed relations so SQLite cannot resolve a stored main view
/// against unseeded main dependencies. Only the selected source's transfer
/// relation is materialized. Per-source recursive history remains budgeted,
/// not claimed linear; unrelated histories cannot enter these table inputs.
pub(crate) fn seeded_relation(relation: &str, seed: ReadSeed) -> crate::Result<String> {
    seeded_relation_inner(relation, seed, None)
}

#[path = "current/restored_sql.rs"]
mod restored_sql;
/// This fixed image owner always returns both relations. The standalone
/// test builder remains distinct; no ordinary caller's row limit is changed.
pub(super) fn restored_combined_select(
    limit: usize,
    proof: &crate::catalog_restore_lineage::AdmittedRestoreHistory<'_>,
    budget: &mut impl super::StructuralReadBudget,
) -> crate::Result<String> {
    if crate::operation_budget::OperationBudget::current().is_none() {
        return Err(crate::Error(
            "restored source SQL requires inherited operation".into(),
        ));
    }
    restored_sql::build_combined(
        limit,
        proof.ordered_origins().count(),
        ReadSeed::Location,
        budget,
    )
}

fn seeded_relation_inner(
    relation: &str,
    seed: ReadSeed,
    restored_origins: Option<usize>,
) -> crate::Result<String> {
    if !matches!(
        relation,
        "catalog_source_effective_headers" | "catalog_source_effective_file_facts"
    ) {
        return Err(crate::Error("unsupported seeded source relation".into()));
    }
    let registry_predicate = match seed {
        ReadSeed::Location => "f.location=request.selector",
        ReadSeed::File => "f.id=request.selector",
    };
    let tables = [
        "catalog_source_successions",
        "catalog_source_succession_files",
        "catalog_source_succession_contexts",
        "catalog_source_succession_heads",
        "catalog_source_succession_context_heads",
        "catalog_source_succession_scopes",
        "catalog_source_succession_evidence",
    ];
    // Charge a conservative bound before copying immutable bodies or building
    // CTE strings. 8192 covers the fixed wrappers below, not database contents.
    let bytes = crate::native41_resources::SOURCE_VIEWS
        .iter()
        .try_fold(8192usize, |n, name| {
            let body = crate::native41_resources::source_view_select(name)?;
            n.checked_add(body.len())
                .ok_or_else(|| crate::Error("source read SQL length overflow".into()))
        })?;
    let bytes = if let Some(count) = restored_origins {
        let body = crate::native41_resources::source_view_select("catalog_source_live_transfers")?;
        bytes
            .checked_add(
                body.len()
                    .checked_mul(4)
                    .ok_or_else(|| crate::Error("restored source SQL length overflow".into()))?,
            )
            .and_then(|n| n.checked_add(2048 + count * 128))
            .ok_or_else(|| crate::Error("restored source SQL length overflow".into()))?
    } else {
        bytes
    };
    // Restored assembly was admitted through its supplied structural budget,
    // which already charges the same parent. Ordinary assembly is unchanged.
    if restored_origins.is_none() {
        if let Some(budget) = crate::operation_budget::OperationBudget::current() {
            budget
                .charge(crate::operation_budget::BudgetCharge {
                    bytes,
                    work: bytes / 64 + 1,
                    ..Default::default()
                })
                .map_err(|e| crate::Error(format!("source read SQL budget: {e:?}")))?;
        }
    }
    let mut definitions = vec![
        "source_read_request(profile,selector) AS (VALUES (?,?))".to_owned(),
        format!("source_read_files AS (SELECT f.id,f.location FROM main.files f JOIN source_read_request request ON {registry_predicate})"),
        "source_read_seed AS MATERIALIZED (SELECT b.profile,b.owner AS source_id FROM main.bindings b JOIN source_read_request request ON request.profile=b.profile WHERE b.kind='source' AND EXISTS(SELECT 1 FROM source_read_files f WHERE f.location=b.owner))".to_owned(),
    ];
    if let Some(count) = restored_origins {
        let values = (0..count)
            .map(|rank| format!("({rank},?)"))
            .collect::<Vec<_>>()
            .join(",");
        definitions.insert(
            1,
            format!("source_read_origins(rank,incarnation) AS (VALUES {values})"),
        );
    }
    for name in tables {
        if name == "catalog_source_successions" {
            // Closed-schema primary key (profile, source_id, revision). Keep
            // the selected seed outside the indexed parent loop: otherwise
            // SQLite can charge scalar predicates on unrelated catalog parents
            // before applying the logically equivalent seed join. No full-row
            // materialization is introduced ahead of history admission.
            definitions.push("catalog_source_successions AS NOT MATERIALIZED (SELECT base.* FROM source_read_seed seed CROSS JOIN main.catalog_source_successions base INDEXED BY sqlite_autoindex_catalog_source_successions_2 ON seed.profile=base.profile AND seed.source_id=base.source_id)".to_owned());
        } else {
            definitions.push(format!("{name} AS NOT MATERIALIZED (SELECT base.* FROM main.{name} base JOIN source_read_seed seed ON seed.profile=base.profile AND seed.source_id=base.source_id)"));
        }
    }
    for name in ["file_facts", "media_header_checks"] {
        definitions.push(format!("{name} AS NOT MATERIALIZED (SELECT base.* FROM main.{name} base JOIN source_read_request request ON request.profile=base.profile WHERE EXISTS(SELECT 1 FROM source_read_files f WHERE f.id=base.file_id))"));
    }
    for name in crate::native41_resources::SOURCE_VIEWS {
        let body = crate::native41_resources::source_view_select(name)?;
        let body = if restored_origins.is_some() && *name == "catalog_source_live_transfers" {
            std::borrow::Cow::Owned(restored_live_transfers_body(body)?)
        } else {
            std::borrow::Cow::Borrowed(body)
        };
        let materialization = if *name == "catalog_source_live_transfers" {
            "MATERIALIZED"
        } else {
            "NOT MATERIALIZED"
        };
        definitions.push(format!("{name} AS {materialization} ({body})"));
    }
    Ok(format!(
        "(WITH {} SELECT * FROM {relation})",
        definitions.join(",\n")
    ))
}

// Only a fixed isolated image read can call this variant. Stored views and
// ordinary candidate41 semantics remain byte-for-byte unchanged.
fn restored_live_transfers_body(body: &str) -> crate::Result<String> {
    const CURRENT: &str = "JOIN catalog_state identity ON identity.id=1 AND identity.catalog_id=s.catalog_id AND identity.incarnation=s.incarnation";
    const HISTORY: &str = "x.catalog_id<>s.catalog_id OR x.incarnation<>s.incarnation";
    if body.matches(CURRENT).count() != 1 || body.matches(HISTORY).count() != 1 {
        return Err(crate::Error(
            "reviewed restored source predicate framing changed".into(),
        ));
    }
    let body = body.replacen(CURRENT,
        "JOIN catalog_state identity ON identity.id=1 AND identity.catalog_id=s.catalog_id AND EXISTS(SELECT 1 FROM source_read_origins origin WHERE origin.incarnation=s.incarnation AND s.committed_generation<=identity.generation)", 1);
    Ok(body.replacen(HISTORY,
        "x.catalog_id<>s.catalog_id OR NOT EXISTS(SELECT 1 FROM source_read_origins origin WHERE origin.incarnation=x.incarnation) OR x.committed_generation>identity.generation OR (x.revision>1 AND (SELECT rank FROM source_read_origins WHERE incarnation=prior.incarnation)>(SELECT rank FROM source_read_origins WHERE incarnation=x.incarnation))", 1))
}

/// Fourth named candidate schema part. Installer AND independent reference
/// composition must use these exact deterministic bytes.
pub(crate) fn membership_schema() -> String {
    let cte = crate::catalog_identity_mutation::source_succession_heads_cte();
    let max_heads = catabolic_core::catalog_metadata::MAX_METADATA_JSON_NODES;
    let retained = "SELECT entity_kind,entity_key FROM catalog_source_succession_context_heads WHERE profile=scope.profile AND source_id=scope.source_id AND succession_revision=scope.succession_revision AND context_id=scope.context_id";
    format!(
        r#"CREATE VIEW catalog_source_current_context_membership AS
 SELECT scope.profile,scope.source_id,scope.succession_revision,scope.context_id,
 CASE WHEN ({cte} SELECT count(*)<={max_heads} AND coalesce(sum(256+6*(length(CAST(kind AS BLOB))+length(CAST(key AS BLOB)))),0)<=4194304 FROM wanted)
 THEN scope.head_count=(SELECT count(*) FROM catalog_source_succession_context_heads h WHERE h.profile=scope.profile AND h.source_id=scope.source_id AND h.succession_revision=scope.succession_revision AND h.context_id=scope.context_id)
 AND NOT EXISTS({cte} SELECT kind,key FROM wanted EXCEPT {retained})
 AND NOT EXISTS({cte} {retained} EXCEPT SELECT kind,key FROM wanted)
 ELSE catalog_source_evidence_refused() END AS current
 FROM catalog_source_succession_contexts scope;"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn seeded_relations_use_exact_closed_bodies_and_bound_registry_selectors() {
        for (seed, predicate) in [
            (ReadSeed::Location, "f.location=request.selector"),
            (ReadSeed::File, "f.id=request.selector"),
        ] {
            let sql = seeded_relation("catalog_source_effective_headers", seed).unwrap();
            assert!(sql.contains(predicate));
            assert_eq!(sql.matches('?').count(), 2);
            assert!(sql.contains("FROM main.bindings b"));
            assert!(sql.contains("source_read_files f WHERE f.location=b.owner"));
            assert!(sql.contains("source_read_seed seed CROSS JOIN main.catalog_source_successions base INDEXED BY sqlite_autoindex_catalog_source_successions_2 ON seed.profile=base.profile AND seed.source_id=base.source_id"));
            assert!(sql.contains("catalog_source_live_transfers AS MATERIALIZED"));
            for name in crate::native41_resources::SOURCE_VIEWS {
                assert!(sql.contains(crate::native41_resources::source_view_select(name).unwrap()));
                assert!(!sql.contains(&format!("main.{name}")));
            }
        }
        assert!(seeded_relation("unreviewed", ReadSeed::File).is_err());
    }

    #[test]
    fn isolated_restore_changes_only_exact_incarnation_predicates() {
        let original =
            crate::native41_resources::source_view_select("catalog_source_live_transfers").unwrap();
        let restored = restored_live_transfers_body(original).unwrap();
        assert!(original.contains("identity.incarnation=s.incarnation"));
        assert!(!restored.contains("identity.incarnation=s.incarnation"));
        assert!(restored.contains("cr.admission_incarnation IS NOT x.incarnation"));
        assert!(restored.contains("r.admission_incarnation=s.incarnation"));
        assert!(restored.contains("x.expected_generation<prior.committed_generation"));
        assert!(restored.contains("incarnation=prior.incarnation)>(SELECT rank"));
        assert!(restored.contains("x.committed_generation>identity.generation"));
        assert!(
            restored_live_transfers_body(&original.replace(
                "identity.incarnation=s.incarnation",
                "identity.incarnation = s.incarnation"
            ))
            .is_err()
        );
        assert!(restored_live_transfers_body(&format!("{original} {original}")).is_err());
        assert_eq!(
            crate::native41_resources::source_view_select("catalog_source_live_transfers").unwrap(),
            original
        );
    }

    #[test]
    fn isolated_restore_origins_are_parameters_not_sql_authority() {
        let sql = seeded_relation_inner(
            "catalog_source_effective_headers",
            ReadSeed::Location,
            Some(3),
        )
        .unwrap();
        assert_eq!(sql.matches('?').count(), 5);
        assert!(
            sql.contains("source_read_origins(rank,incarnation) AS (VALUES (0,?),(1,?),(2,?))")
        );
        assert!(sql.contains("base INDEXED BY sqlite_autoindex_catalog_source_successions_2"));
        let ordinary =
            seeded_relation("catalog_source_effective_headers", ReadSeed::Location).unwrap();
        assert_eq!(ordinary.matches('?').count(), 2);
        assert!(!ordinary.contains("source_read_origins"));
        assert!(ordinary.contains("identity.incarnation=s.incarnation"));
    }

    #[test]
    fn evidence_work_requires_and_exhausts_the_same_real_budget() {
        use crate::operation_budget::{BudgetLimits, OperationBudget};
        let db = rusqlite::Connection::open_in_memory().unwrap();
        crate::catalog_source_succession::install(&db).unwrap();
        let run = || {
            db.query_row("SELECT catalog_source_evidence_work(64,1)", [], |r| {
                r.get::<_, i64>(0)
            })
        };
        assert!(run().is_err(), "absent scope granted work");
        let budget = OperationBudget::new(
            std::time::Duration::from_secs(5),
            BudgetLimits {
                items: 10,
                values: 10,
                bytes: 1024,
                work: 3,
            },
        )
        .unwrap();
        {
            let _scope = budget.enter().unwrap();
            assert_eq!(run().unwrap(), 64);
            assert_eq!(budget.usage().work, 2);
            assert!(run().is_err());
            assert!(run().is_err(), "exhaustion was reset");
        }
        assert!(run().is_err(), "scope escaped its owner");
    }

    #[test]
    fn candidate_relation_selection_leaves_production_reference_unchanged() {
        let membership = membership_schema();
        assert_eq!(membership, membership_schema());
        assert!(!membership.contains("?1"));
        assert!(membership.contains("scope.context_id"));
        assert_eq!(membership.matches("WITH versions AS").count(), 3);
        assert_eq!(membership.matches("EXCEPT").count(), 2);
        assert!(synthetic_read_view(41, "catalog_source_effective_headers"));
        assert!(!synthetic_read_view(40, "catalog_source_effective_headers"));
        assert!(!synthetic_read_view(41, "catalog_source_unreviewed"));
        assert!(!synthetic_read_view(41, "catalog_source_successions"));
        assert!(!refusal_function(40, "catalog_source_evidence_refused"));
        assert!(!refusal_function(41, "load_extension"));
        assert!(refusal_function(41, "catalog_source_evidence_refused"));
        for entry in crate::migration::reference()["views"].as_array().unwrap() {
            let original = entry["sql"].as_str().unwrap();
            assert_eq!(view(40, original), original);
            assert_eq!(view(39, original), original);
        }
        let original = crate::query::view_sql("catalog_component_occurrences");
        let candidate = view(41, original);
        assert!(candidate.contains("main.catalog_source_effective_assertions"));
        assert!(candidate.contains("main.catalog_source_effective_file_facts"));
        assert!(!candidate.contains("main.component_assertions"));
        assert!(!candidate.contains("main.file_facts"));
        assert!(candidate.contains("main.catalog_source_current_occurrence_transfers"));
        assert_eq!(view(41, &candidate), candidate);
    }
}

/// Candidate-only fixed observation provenance transition. Resource installation
/// and its independent reference must compose these identical reviewed bytes.
pub(crate) fn observation_schema() -> &'static str {
    include_str!("../source_observation_schema.sql")
}

/// Real scan publication replaces renewal provenance rather than attributing a
/// successor inode to a historical scan. Schema40 statements remain unchanged.
pub(crate) fn scan_observation_sql(schema: u32, original: &str) -> String {
    if schema != 41 {
        return original.to_owned();
    }
    original.replace("INSERT INTO observations VALUES", "INSERT INTO observations(profile,file_id,size,mtime_ns,device,inode,status,scan_id) VALUES")
        .replace("scan_id=excluded.scan_id", "scan_id=excluded.scan_id,observation_origin='scan',succession_source_id=NULL,succession_revision=NULL")
}
