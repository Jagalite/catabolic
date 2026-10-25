//! Complete semantic source history for the fixed completed-current-image owner.
//! These are additional export-only cohort ceilings, not source mutation limits
//! or a promise that every such cohort fits the inherited byte/work/deadline.
use super::*;
use serde_json::Value;

const MAX_EXPORT_RECORDS: usize = 256;
const MAX_EXPORT_SOURCES: usize = 64;
const MAX_EXPORT_ROWS: usize = 16_384;

/// Exact original effect descriptions, not command envelopes or new grants.
/// Motion must match its saved command and complete ACK to these native facts.
mod read_witness;
pub(crate) use read_witness::RetainedRestoredSourceHistory;

pub(crate) struct CapturedSourceHistory {
    effects: Vec<Value>,
    restored: Option<read_witness::UnsealedRestoredHistory>,
}
impl CapturedSourceHistory {
    pub(crate) fn effects(&self) -> &[Value] {
        &self.effects
    }
}

// All names and immediate-parent predicates are fixed closed-schema owners.
// Ordered prefixes use each table's primary-key prefix. LIMIT includes a
// sentinel; no truncated prefix is ever accepted as complete history.
const ROWS: [&str; 7] = [
    "SELECT profile,source_id,revision,1 FROM main.catalog_source_successions ORDER BY profile,source_id,revision LIMIT ?1",
    "SELECT r.profile,r.source_id,r.succession_revision,EXISTS(SELECT 1 FROM main.catalog_source_successions p WHERE p.profile=r.profile AND p.source_id=r.source_id AND p.revision=r.succession_revision) FROM main.catalog_source_succession_files r ORDER BY r.profile,r.source_id,r.succession_revision LIMIT ?1",
    "SELECT r.profile,r.source_id,r.succession_revision,EXISTS(SELECT 1 FROM main.catalog_source_successions p WHERE p.profile=r.profile AND p.source_id=r.source_id AND p.revision=r.succession_revision) FROM main.catalog_source_succession_contexts r ORDER BY r.profile,r.source_id,r.succession_revision LIMIT ?1",
    "SELECT r.profile,r.source_id,r.succession_revision,EXISTS(SELECT 1 FROM main.catalog_source_succession_files p WHERE p.profile=r.profile AND p.source_id=r.source_id AND p.succession_revision=r.succession_revision AND p.file_id=r.file_id) FROM main.catalog_source_succession_evidence r ORDER BY r.profile,r.source_id,r.succession_revision LIMIT ?1",
    "SELECT r.profile,r.source_id,r.succession_revision,EXISTS(SELECT 1 FROM main.catalog_source_successions p WHERE p.profile=r.profile AND p.source_id=r.source_id AND p.revision=r.succession_revision) FROM main.catalog_source_succession_heads r ORDER BY r.profile,r.source_id,r.succession_revision LIMIT ?1",
    "SELECT r.profile,r.source_id,r.succession_revision,EXISTS(SELECT 1 FROM main.catalog_source_succession_contexts p WHERE p.profile=r.profile AND p.source_id=r.source_id AND p.succession_revision=r.succession_revision AND p.context_id=r.context_id) AND EXISTS(SELECT 1 FROM main.catalog_source_succession_heads h WHERE h.profile=r.profile AND h.source_id=r.source_id AND h.succession_revision=r.succession_revision AND h.entity_kind=r.entity_kind AND h.entity_key=r.entity_key) FROM main.catalog_source_succession_context_heads r ORDER BY r.profile,r.source_id,r.succession_revision LIMIT ?1",
    "SELECT r.profile,r.source_id,r.succession_revision,EXISTS(SELECT 1 FROM main.catalog_source_successions p WHERE p.profile=r.profile AND p.source_id=r.source_id AND p.revision=r.succession_revision) FROM main.catalog_source_succession_scopes r ORDER BY r.profile,r.source_id,r.succession_revision LIMIT ?1",
];

fn preflight(
    db: &rusqlite::Connection,
    budget: &mut impl StructuralReadBudget,
) -> Result<(Vec<(String, String)>, usize)> {
    budget.charge(BudgetCharge {
        bytes: MAX_EXPORT_SOURCES * std::mem::size_of::<(String, String)>(),
        work: 1,
        ..Default::default()
    })?;
    let mut sources: Vec<(String, String)> = Vec::with_capacity(MAX_EXPORT_SOURCES);
    let mut total = 0usize;
    let mut records = 0usize;
    for (index, sql) in ROWS.iter().enumerate() {
        let limit = if index == 0 {
            MAX_EXPORT_RECORDS
        } else {
            MAX_EXPORT_ROWS - total
        };
        budget.charge(BudgetCharge {
            work: 1,
            ..Default::default()
        })?;
        let mut statement = db.prepare(sql)?;
        let mut rows = statement.query([(limit + 1) as i64])?;
        let mut selected = 0usize;
        loop {
            budget.charge(BudgetCharge {
                work: 1,
                ..Default::default()
            })?;
            let Some(row) = rows.next()? else { break };
            selected += 1;
            if selected > limit || total == MAX_EXPORT_ROWS {
                return Err(refuse(
                    "complete image source history exceeds export row cohort",
                ));
            }
            let profile = row
                .get_ref(0)?
                .as_str()
                .map_err(|_| refuse("source export profile is not TEXT"))?;
            let source = row
                .get_ref(1)?
                .as_str()
                .map_err(|_| refuse("source export source is not TEXT"))?;
            let revision = row.get::<_, i64>(2)?;
            let connected = row.get::<_, i64>(3)?;
            let bytes = profile
                .len()
                .checked_add(source.len())
                .ok_or_else(|| refuse("source export key length overflow"))?;
            budget.charge(BudgetCharge {
                items: 1,
                values: 4,
                bytes: bytes
                    .checked_add(16)
                    .ok_or_else(|| refuse("source export scalar size overflow"))?,
                work: 1 + bytes.div_ceil(64),
                ..Default::default()
            })?;
            if revision <= 0 || connected != 1 {
                return Err(refuse(
                    "complete image source history contains orphan effects",
                ));
            }
            total += 1;
            if index == 0 {
                records += 1;
                if let Some(last) = sources.last() {
                    budget.charge(BudgetCharge {
                        work: 2
                            + last.0.len().min(profile.len()).div_ceil(64)
                            + last.1.len().min(source.len()).div_ceil(64),
                        ..Default::default()
                    })?;
                    if last.0 == profile && last.1 == source {
                        continue;
                    }
                }
                if sources.len() == MAX_EXPORT_SOURCES {
                    return Err(refuse(
                        "complete image source history exceeds export source cohort",
                    ));
                }
                budget.charge(BudgetCharge {
                    bytes,
                    values: 2,
                    work: 1 + bytes.div_ceil(64),
                    ..Default::default()
                })?;
                sources.push((profile.to_owned(), source.to_owned()));
            }
        }
    }
    Ok((sources, records))
}

pub(crate) fn capture_complete_history_in(
    store: &Store,
    budget: &mut impl StructuralReadBudget,
) -> Result<CapturedSourceHistory> {
    capture_complete_history_in_with_control_workspace(store, budget, None)
}
pub(crate) fn capture_complete_history_in_with_control_workspace(
    store: &Store,
    budget: &mut impl StructuralReadBudget,
    control: Option<&crate::migration::BootstrapWorkspace<'_>>,
) -> Result<CapturedSourceHistory> {
    let _read = current::enter_read(store)?;
    if store.schema_version != 41
        || !store.writable
        || !store.owns_writer_lock()
        || store.db.is_autocommit()
        || store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
    {
        return Err(refuse(
            "complete source history requires retained completed41 writer",
        ));
    }
    store.transaction_available_with_control_workspace(control)?;
    capture_history_from(&store.db, ExportOwner::Current(store, control), budget)
}

pub(crate) fn capture_restored_source_history_in(
    db: &rusqlite::Connection,
    proof: &crate::catalog_restore_lineage::AdmittedRestoreHistory<'_>,
    budget: &mut impl StructuralReadBudget,
) -> Result<CapturedSourceHistory> {
    history::charge_restore_check(proof, budget)?;
    proof.check(db)?;
    let operation = crate::operation_budget::OperationBudget::current()
        .ok_or_else(|| refuse("restored source image requires inherited operation owner"))?;
    operation
        .charge(BudgetCharge::default())
        .map_err(|e| refuse(&format!("restored source operation refused: {e:?}")))?;
    capture_history_from(db, ExportOwner::Restored(proof), budget)
}
enum ExportOwner<'a, 'image, 'workspace> {
    Current(&'a Store, Option<&'a crate::migration::BootstrapWorkspace<'workspace>>),
    Restored(&'a crate::catalog_restore_lineage::AdmittedRestoreHistory<'image>),
}
fn capture_history_from(
    db: &rusqlite::Connection,
    owner: ExportOwner<'_, '_, '_>,
    budget: &mut impl StructuralReadBudget,
) -> Result<CapturedSourceHistory> {
    budget.charge(BudgetCharge {
        work: 1,
        ..Default::default()
    })?;
    if db
        .prepare("SELECT 1 FROM main.catalog_source_succession_permits LIMIT 1")?
        .exists([])?
    {
        return Err(refuse(
            "current image cannot contain an unfinished source permit",
        ));
    }
    let (sources, count) = preflight(db, budget)?;
    budget.charge(BudgetCharge {
        bytes: count
            .checked_mul(std::mem::size_of::<Value>())
            .ok_or_else(|| refuse("source export record allocation overflow"))?,
        work: 1,
        ..Default::default()
    })?;
    let mut effects = Vec::with_capacity(count);
    let mut restored = match &owner {
        ExportOwner::Current(..) => None,
        ExportOwner::Restored(_) => Some(read_witness::UnsealedRestoredHistory::new(
            db,
            sources.len(),
            budget,
        )?),
    };
    for (profile, source) in sources {
        let binding = match &owner {
            ExportOwner::Current(store, _) => {
                history::collect_records_in(store, &profile, &source, budget, &mut effects)?
            }
            ExportOwner::Restored(proof) => history::collect_restored_records_in(
                db,
                proof,
                &profile,
                &source,
                budget,
                &mut effects,
            )?,
        }
        .ok_or_else(|| refuse("enumerated source history disappeared"))?;
        require_current_binding_from(db, &profile, &source, &binding, budget)?;
        if let Some(restored) = &mut restored {
            restored.push(profile, source, binding, budget)?;
        }
    }
    if effects.len() != count {
        return Err(refuse("complete source history record coverage differs"));
    }
    match owner {
        ExportOwner::Current(store, control) => store.transaction_available_with_control_workspace(control)?,
        ExportOwner::Restored(proof) => {
            history::charge_restore_check(proof, budget)?;
            proof.check(db)?;
        }
    }
    Ok(CapturedSourceHistory { effects, restored })
}

#[cfg(test)]
mod tests {
    use super::*;
    // Isolated enumeration oracle only, deliberately not a genuine image or
    // command fixture. The semantic verifier remains mandatory in production.
    const CHILDREN: [&str; 6] = [
        "files",
        "contexts",
        "evidence",
        "heads",
        "context_heads",
        "scopes",
    ];
    fn db() -> rusqlite::Connection {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE catalog_source_successions(profile TEXT,source_id TEXT,revision INTEGER,PRIMARY KEY(profile,source_id,revision));").unwrap();
        for name in CHILDREN {
            db.execute_batch(&format!("CREATE TABLE catalog_source_succession_{name}(profile TEXT,source_id TEXT,succession_revision INTEGER,file_id TEXT DEFAULT '',context_id TEXT DEFAULT '',entity_kind TEXT DEFAULT '',entity_key TEXT DEFAULT '',PRIMARY KEY(profile,source_id,succession_revision,file_id,context_id,entity_kind,entity_key));")).unwrap();
        }
        db
    }
    #[derive(Default)]
    struct Meter {
        bytes: usize,
        work: usize,
        byte_limit: Option<usize>,
    }
    impl StructuralReadBudget for Meter {
        fn charge(&mut self, c: BudgetCharge) -> Result<()> {
            self.bytes = self
                .bytes
                .checked_add(c.bytes)
                .ok_or_else(|| refuse("test byte overflow"))?;
            self.work = self
                .work
                .checked_add(c.work)
                .ok_or_else(|| refuse("test work overflow"))?;
            if self.byte_limit.is_some_and(|limit| self.bytes > limit) {
                return Err(refuse("test allocation boundary"));
            }
            Ok(())
        }
        fn raw_json(&mut self, _: &str) -> Result<()> {
            Err(refuse("prefix preflight must not parse JSON"))
        }
    }
    #[test]
    fn export_prefix_bounds_refuse_sentinel_without_truncation() {
        let db = db();
        db.execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<256) INSERT INTO catalog_source_successions SELECT 'p','s',x FROM n;").unwrap();
        assert_eq!(preflight(&db, &mut Meter::default()).unwrap().1, 256);
        db.execute(
            "INSERT INTO catalog_source_successions VALUES('p','s',257)",
            [],
        )
        .unwrap();
        assert!(preflight(&db, &mut Meter::default()).is_err());
        db.execute("DELETE FROM catalog_source_successions", [])
            .unwrap();
        for n in 0..MAX_EXPORT_SOURCES {
            db.execute(
                "INSERT INTO catalog_source_successions VALUES('p',?,1)",
                [format!("s{n:03}")],
            )
            .unwrap();
        }
        assert_eq!(
            preflight(&db, &mut Meter::default()).unwrap().0.len(),
            MAX_EXPORT_SOURCES
        );
        db.execute(
            "INSERT INTO catalog_source_successions VALUES('p','overflow',1)",
            [],
        )
        .unwrap();
        assert!(preflight(&db, &mut Meter::default()).is_err());
        db.execute_batch("DELETE FROM catalog_source_successions; INSERT INTO catalog_source_successions VALUES('p','s',1); WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<16383) INSERT INTO catalog_source_succession_files(profile,source_id,succession_revision,file_id) SELECT 'p','s',1,printf('%08d',x) FROM n;").unwrap();
        assert_eq!(preflight(&db, &mut Meter::default()).unwrap().1, 1);
        db.execute("INSERT INTO catalog_source_succession_files(profile,source_id,succession_revision,file_id) VALUES('p','s',1,'overflow')", []).unwrap();
        assert!(preflight(&db, &mut Meter::default()).is_err());
    }
    #[test]
    fn export_preflight_covers_every_child_and_both_context_links() {
        for child in CHILDREN {
            let db = db();
            db.execute(&format!("INSERT INTO catalog_source_succession_{child}(profile,source_id,succession_revision) VALUES('p','s',1)"), []).unwrap();
            assert!(preflight(&db, &mut Meter::default()).is_err(), "{child}");
        }
        let db = db();
        db.execute_batch("INSERT INTO catalog_source_successions VALUES('p','s',1); INSERT INTO catalog_source_succession_contexts(profile,source_id,succession_revision) VALUES('p','s',1); INSERT INTO catalog_source_succession_context_heads(profile,source_id,succession_revision) VALUES('p','s',1);").unwrap();
        assert!(preflight(&db, &mut Meter::default()).is_err());
        db.execute_batch("INSERT INTO catalog_source_succession_heads(profile,source_id,succession_revision) VALUES('p','s',1);").unwrap();
        assert_eq!(preflight(&db, &mut Meter::default()).unwrap().1, 1);
    }
    #[test]
    fn export_prefix_storage_is_admitted_before_owned_keys() {
        let db = db();
        db.execute(
            "INSERT INTO catalog_source_successions VALUES('profile','source',1)",
            [],
        )
        .unwrap();
        let mut meter = Meter {
            byte_limit: Some(0),
            ..Default::default()
        };
        assert!(preflight(&db, &mut meter).is_err());
        let slots = MAX_EXPORT_SOURCES * std::mem::size_of::<(String, String)>();
        let mut meter = Meter {
            byte_limit: Some(slots + "profile".len() + "source".len() + 16),
            ..Default::default()
        };
        assert!(preflight(&db, &mut meter).is_err());
        assert_eq!(
            meter.bytes,
            slots + 2 * ("profile".len() + "source".len()) + 16
        );
        let mut meter = Meter {
            byte_limit: Some(meter.bytes),
            ..Default::default()
        };
        let (keys, count) = preflight(&db, &mut meter).unwrap();
        assert_eq!(keys, vec![("profile".to_owned(), "source".to_owned())]);
        assert_eq!(count, 1);
    }
}
