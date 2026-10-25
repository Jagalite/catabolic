//! Positive, exact post-commit reconciliation for the single known 39→40
//! transition. Observing schema40 alone is never a completion witness.
use super::*;
use std::collections::BTreeSet;
use std::os::unix::fs::MetadataExt;

/// Not deserializable. Minted only while the exact native file/admission/writer
/// and SQLite exclusive locks remain held through the caller's companion ACK.
pub struct ReconciledNativeUpgrade {
    proof: Value,
}
impl ReconciledNativeUpgrade {
    pub fn proof(&self) -> &Value {
        &self.proof
    }
}

/// Caller must already own the companion writer and have stopped every service
/// user. The callback may append its host transition ACK; it cannot use this
/// proof to upgrade another catalog or reinterpret a later native mutation.
pub fn with_reconciled_native_upgrade<T>(
    path: &Path,
    saved_plan: &Value,
    expected_catalog: &str,
    expected_incarnation: &str,
    quiescence: NativeUpgradeQuiescence,
    acknowledge: impl FnOnce(&ReconciledNativeUpgrade) -> Result<T>,
) -> Result<T> {
    quiescence.require()?;
    validate_saved_39_to_40_plan(saved_plan)?;
    let path = database_path(path)?;
    let admission = crate::native_admission_exclusive(&path)?;
    let _writer = writer_lock(&path)?;
    let db = connect(&path, true)?;
    require_native_journal(&db)?;
    db.execute_batch("BEGIN EXCLUSIVE")?;
    admission.validate_path()?;
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || saved_plan["input"]["database"] != json!(path)
        || saved_plan["input"]["file_identity"]
            != json!({"device":metadata.dev(),"inode":metadata.ino()})
    {
        return Err(Error("native upgrade catalog file identity changed".into()));
    }
    let info = validate(&db, true)?;
    let old_history = saved_plan["input"]["history"]
        .as_array()
        .ok_or_else(|| Error("upgrade history missing".into()))?;
    let history = info["history"]
        .as_array()
        .ok_or_else(|| Error("native history missing".into()))?;
    if info["schema"] != 40
        || info["database_id"] != saved_plan["input"]["database_id"]
        || old_history.len() != 39
        || history.len() != 40
        || &history[..39] != old_history.as_slice()
    {
        return Err(Error(
            "native upgrade history or database identity changed".into(),
        ));
    }
    let (catalog, incarnation): (String, String) = db.query_row(
        "SELECT catalog_id,incarnation FROM catalog_state WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if catalog != expected_catalog || incarnation != expected_incarnation {
        return Err(Error(
            "native upgrade catalog admission identity changed".into(),
        ));
    }
    let tables = saved_plan["input"]["tables"]
        .as_object()
        .ok_or_else(|| Error("native upgrade source table inventory missing".into()))?;
    let expected: BTreeSet<String> = native_reference_at(39)
        .expect("known schema39")
        .as_array()
        .expect("schema entries")
        .iter()
        .filter(|row| row[0] == "table")
        .filter_map(|row| row[1].as_str())
        .filter(|name| *name != "schema_migrations" && !name.starts_with("sqlite_"))
        .map(str::to_owned)
        .collect();
    if tables.keys().cloned().collect::<BTreeSet<_>>() != expected {
        return Err(Error(
            "native upgrade source table inventory is incomplete".into(),
        ));
    }
    for (table, value) in tables {
        let mut query = db.prepare(&format!("PRAGMA table_info({})", identifier(table)))?;
        let columns = query
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if value["columns"] != json!(columns) {
            return Err(Error(
                "native upgrade source column inventory changed".into(),
            ));
        }
    }
    schema40_preservation::preservation_schema40(&db, &saved_plan["input"]["tables"])?;
    let original_pending = native_pending_work(&db, 39)?;
    let current_pending = crate::native_upgrade_inventory::qualified_pending_work(&db, 40)?;
    let original_qualified = crate::native_upgrade_inventory::qualified_pending_work(&db, 39)?;
    if saved_plan["input"]["pending_work"] != original_pending
        || saved_plan["input"]
            .get("qualified_pending_work")
            .unwrap_or(&saved_plan["input"]["pending_work"])
            != &original_qualified
        || !work_is_settled(&current_pending)
    {
        return Err(Error(
            "native upgrade pending-work inventory differs from exact settled source".into(),
        ));
    }

    let sequences = if db
        .prepare("SELECT 1 FROM sqlite_schema WHERE type='table' AND name='sqlite_sequence'")?
        .exists([])?
    {
        rows(
            &db,
            "SELECT name,seq FROM sqlite_sequence ORDER BY name",
            &[],
        )?
    } else {
        vec![]
    };
    if saved_plan["input"]["sequences"] != json!(sequences) {
        return Err(Error("native upgrade sequence state changed".into()));
    }
    admission.validate_path()?;
    let proof = ReconciledNativeUpgrade {
        proof: json!({"contract_version":1,"catalog_id":catalog,"incarnation":incarnation,"from_schema":39,"to_schema":40,"plan_fingerprint":saved_plan["plan_fingerprint"],"input_fingerprint":saved_plan["input_fingerprint"],"native_schema_fingerprint":saved_plan["native_schema_fingerprint"],"history":info["history"]}),
    };
    let result = acknowledge(&proof);
    admission.validate_path()?;
    // The callback's companion transaction controls its own ACK durability.
    // No native data was changed by this positive proof owner.
    db.execute_batch("ROLLBACK")?;
    result
}

// Historical recovery is pinned to its saved target, never the latest target.
// This does not authorize a new migration or admit an already-upgraded41 file.
fn validate_saved_39_to_40_plan(saved_plan: &Value) -> Result<()> {
    if encode(saved_plan).len() > 1024 * 1024
        || saved_plan["contract"] != "catabolic-native-upgrade-v1"
        || saved_plan["from_schema"] != 39
        || saved_plan["to_schema"] != 40
        || !is_supported_native_schema(40)
        || saved_plan["input"]["schema"] != 39
    {
        return Err(Error(
            "unsupported or oversized exact native upgrade reconciliation plan".into(),
        ));
    }
    let mut unsigned = saved_plan.clone();
    let recorded = unsigned
        .as_object_mut()
        .ok_or_else(|| Error("upgrade plan must be an object".into()))?
        .remove("plan_fingerprint");
    if recorded != Some(json!(sha256(encode(&unsigned))))
        || saved_plan["input_fingerprint"] != sha256(encode(&saved_plan["input"]))
        || saved_plan["migration_resources"]
            != json!(steps(40)?.iter().map(description).collect::<Vec<_>>())
        || saved_plan["migrations"] != json!([description(&steps(40)?[39])])
        || saved_plan["native_schema_fingerprint"]
            != sha256(encode(native_reference_at(40).ok_or_else(|| {
                Error("closed schema40 reference is unavailable".into())
            })?))
        || !work_is_settled(
            saved_plan["input"]
                .get("qualified_pending_work")
                .unwrap_or(&saved_plan["input"]["pending_work"]),
        )
    {
        return Err(Error(
            "native upgrade plan digest, resources or original settled state changed".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod historical_target_tests {
    use super::*;
    fn seal(plan: &mut Value) {
        plan.as_object_mut().unwrap().remove("plan_fingerprint");
        plan["input_fingerprint"] = json!(sha256(encode(&plan["input"])));
        plan["plan_fingerprint"] = json!(sha256(encode(plan)));
    }
    // Only exercises the plan/resource check. This synthetic plan is not an
    // admitted database, quiescence witness, reconciliation proof, or ACK.
    fn plan() -> Value {
        let mut plan = json!({
            "contract":"catabolic-native-upgrade-v1", "from_schema":39,
            "to_schema":40,"input":{"schema":39,"pending_work":{}},
            "migration_resources":steps(40).unwrap().iter().map(description).collect::<Vec<_>>(),
            "migrations":[description(&steps(40).unwrap()[39])],
            "native_schema_fingerprint":sha256(encode(native_reference_at(40).unwrap()))
        });
        seal(&mut plan);
        plan
    }
    #[test]
    fn historical_plan_uses_closed40_resources_not_latest_target() {
        let plan = plan();
        validate_saved_39_to_40_plan(&plan).unwrap();
        assert_eq!(plan["migration_resources"].as_array().unwrap().len(), 40);
        assert_eq!(plan["migrations"][0]["version"], 40);
        assert_eq!(
            plan["native_schema_fingerprint"],
            sha256(encode(native_reference_at(40).unwrap()))
        );
        // A self-consistently rehashed different target cannot borrow this owner.
        let mut changed = plan.clone();
        changed["to_schema"] = json!(41);
        seal(&mut changed);
        assert!(validate_saved_39_to_40_plan(&changed).is_err());
    }
    #[test]
    fn historical_plan_still_refuses_changed_resources_signature_or_work() {
        for alteration in 0..5 {
            let mut changed = plan();
            match alteration {
                0 => {
                    changed["native_schema_fingerprint"] =
                        json!(sha256(encode(native_reference_at(39).unwrap())))
                }
                1 => changed["migration_resources"][39]["checksum"] = json!("0".repeat(64)),
                2 => changed["migrations"] = json!([]),
                3 => changed["input"]["schema"] = json!(40),
                _ => changed["input"]["pending_work"] = json!({"worker":1}),
            }
            seal(&mut changed);
            assert!(validate_saved_39_to_40_plan(&changed).is_err());
        }
        let mut changed = plan();
        changed["plan_fingerprint"] = json!("0".repeat(64));
        assert!(validate_saved_39_to_40_plan(&changed).is_err());
    }
}
