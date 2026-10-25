//! Deliberately narrow inventory-only physical renewal cohort.
//!
//! Every registry file is still independently held, hashed and revalidated by
//! VerifiedContent. This proof grants no logical/media membership. To avoid
//! guessing at opaque historical references, ANY row in the fixed logical or
//! provenance tables below refuses, even for an unrelated source. In particular
//! libraries with native metadata history are outside this initial cohort.
//! Descriptive exact-file revisions and ordinary scan facts are not membership.
pub(crate) mod imported_library;
use super::*;
use crate::catalog_nfo_attribution::ProofBudget;

macro_rules! absence_queries {
    ($($table:literal),+ $(,)?) => { &[$(($table, concat!("SELECT EXISTS(SELECT 1 FROM main.", $table, " LIMIT 1)"))),+] };
}
const ABSENCE: &[(&str, &str)] = absence_queries![
    "item_files",
    "catalog_legacy_associations",
    "catalog_binding_revisions",
    "catalog_bindings",
    "catalog_version_bindings",
    "catalog_segment_members",
    "catalog_segmentation_sets",
    "component_occurrences",
    "component_assertions",
    "mappings",
    "media_outputs",
    "processing_artifacts",
    "fallback_entries",
    "item_requirements",
    "rule_jobs",
    "rule_processor_jobs",
    "rule_requirements",
    "proposals",
    "catalog_nfo_file_attributions",
    "catalog_nfo_state_revisions",
    "catalog_metadata_subjects",
    "catalog_metadata_sources",
    "catalog_metadata_history",
    "catalog_legacy_item_metadata",
    "catalog_identity_evidence",
];

impl CapturedSuccession {
    /// Called only immediately after the fixed pre-effect pass check. The pass
    /// remains live through all later callbacks/physical checks; its final
    /// schema/total-changes and sticky boundary checks precede the first write.
    /// Thus an intervening write (including a caught rollback attempt) cannot
    /// turn these absence observations into an effect. No proof escapes.
    pub(crate) fn admit_inventory_only_in(
        &self,
        store: &Store,
        revisions: &BTreeMap<String, (String, String)>,
        budget: &mut ProofBudget,
    ) -> Result<bool> {
        // Empty sources and all bound/mixed sources keep the previous planner.
        if self.files.is_empty() || !self.contexts.is_empty() {
            return Ok(false);
        }
        let operation = store.catalog_operation_budget()?;
        if !budget.uses_operation(&operation)
            || store.schema_version != 41
            || store.database_id != self.catalog_id
            || store.catalog_incarnation.as_deref() != Some(self.incarnation.as_str())
            || store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
        {
            return Err(refuse("inventory-only retained operation differs"));
        }
        StructuralReadBudget::charge(
            budget,
            BudgetCharge {
                values: 1,
                bytes: 8,
                work: 2,
                ..Default::default()
            },
        )?;
        let generation: i64 = store.db.query_row(
            "SELECT generation FROM main.catalog_state WHERE id=1",
            [],
            |r| r.get(0),
        )?;
        if generation != self.generation || self.files.len() != revisions.len() {
            return Err(refuse("inventory-only complete registry differs"));
        }
        require_complete_keys(&self.files, revisions, budget)?;
        // All referenced payloads are excluded without materializing or parsing
        // them. No active/current/profile filter can hide retired, detached,
        // cross-file, withdrawn or imported provenance. Constant LIMIT-1 probes
        // are charged before prepare and share the existing parent deadline.
        for &(table, sql) in ABSENCE {
            StructuralReadBudget::charge(
                budget,
                BudgetCharge {
                    values: 1,
                    bytes: 8,
                    work: 1 + sql.len().div_ceil(64),
                    ..Default::default()
                },
            )?;
            let present: bool = store.db.query_row(sql, [], |r| r.get(0))?;
            if present {
                return Err(refuse(&format!(
                    "inventory-only logical/provenance table is not empty: {table}"
                )));
            }
        }
        operation
            .check()
            .map_err(|e| refuse(&format!("inventory-only operation: {e:?}")))?;
        Ok(true)
    }
}

fn require_complete_keys(
    files: &BTreeMap<String, String>,
    revisions: &BTreeMap<String, (String, String)>,
    budget: &mut impl StructuralReadBudget,
) -> Result<()> {
    if files.len() != revisions.len() {
        return Err(refuse("inventory-only complete registry differs"));
    }
    for (registered, held) in files.keys().zip(revisions.keys()) {
        budget.charge(BudgetCharge {
            work: 1 + registered.len().max(held.len()).div_ceil(64),
            ..Default::default()
        })?;
        if registered != held {
            return Err(refuse("inventory-only complete registry differs"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_absence_queries_cover_all_rows_without_live_or_profile_filters() {
        // Predicate regression only, not fabricated source or effect authority.
        // Every table has a row shaped as hidden/retired/foreign provenance;
        // the fixed predicate must refuse without interpreting its payload.
        let db = rusqlite::Connection::open_in_memory().unwrap();
        let mut unique = BTreeSet::new();
        for &(table, sql) in ABSENCE {
            assert!(unique.insert(table));
            db.execute_batch(&format!(
                "CREATE TABLE {table}(active INTEGER, profile TEXT, payload TEXT)"
            ))
            .unwrap();
            assert!(!db.query_row(sql, [], |r| r.get::<_, bool>(0)).unwrap());
            db.execute(
                &format!(
                    "INSERT INTO {table} VALUES(0,'unrelated','retired detached opaque provenance')"
                ),
                [],
            )
            .unwrap();
            assert!(db.query_row(sql, [], |r| r.get::<_, bool>(0)).unwrap());
        }
        assert_eq!(unique.len(), 25);
        for required in [
            "catalog_legacy_associations",
            "catalog_binding_revisions",
            "catalog_segmentation_sets",
            "component_assertions",
            "catalog_nfo_state_revisions",
            "catalog_metadata_history",
        ] {
            assert!(unique.contains(required));
        }
    }

    #[test]
    fn fixed_absence_queries_match_the_actual_closed_candidate_schema() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("native.sqlite3");
        crate::migration::initialize_native_at(&path, 40).unwrap();
        crate::candidate41_test_support::install_candidate41_for_test(&path).unwrap();
        let store =
            crate::candidate41_test_support::open_candidate41_for_test(&path, true).unwrap();
        for &(_, sql) in ABSENCE {
            assert!(
                !store
                    .db
                    .query_row(sql, [], |r| r.get::<_, bool>(0))
                    .unwrap()
            );
        }
    }
    #[test]
    fn complete_registry_keys_refuse_partial_substituted_and_budget_failure() {
        struct Budget {
            left: usize,
            failed: bool,
        }
        impl StructuralReadBudget for Budget {
            fn charge(&mut self, c: BudgetCharge) -> Result<()> {
                if self.failed || c.work > self.left {
                    self.failed = true;
                    return Err(refuse("test budget exhausted"));
                }
                self.left -= c.work;
                Ok(())
            }
            fn raw_json(&mut self, _: &str) -> Result<()> {
                Err(refuse("no JSON parsing in key proof"))
            }
        }
        let files = BTreeMap::from([("a".into(), "path-a".into()), ("b".into(), "path-b".into())]);
        let full = BTreeMap::from([
            ("a".into(), ("old-a".into(), "new-a".into())),
            ("b".into(), ("old-b".into(), "new-b".into())),
        ]);
        let mut budget = Budget {
            left: 4,
            failed: false,
        };
        require_complete_keys(&files, &full, &mut budget).unwrap();
        assert_eq!(budget.left, 0);
        assert!(require_complete_keys(&files, &full, &mut budget).is_err());
        budget.left = 100;
        assert!(require_complete_keys(&files, &full, &mut budget).is_err());
        let mut fresh = Budget {
            left: 100,
            failed: false,
        };
        let mut partial = full.clone();
        partial.remove("b");
        assert!(require_complete_keys(&files, &partial, &mut fresh).is_err());
        partial.insert("c".into(), ("old-b".into(), "new-b".into()));
        assert!(require_complete_keys(&files, &partial, &mut fresh).is_err());
    }
}
