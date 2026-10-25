//! Typed source40 preservation inputs retained by the consuming migration owner.
//! No external constructor, schema admission or bootstrap completion authority.
use super::*;

pub(super) struct SourcePreservation {
    plan_digest: String,
    ledger: Value,
    tables: Value,
    catalog: String,
    incarnation: String,
    generation: i64,
    file_identity: Value,
    catalog_state: Value,
}
impl SourcePreservation {
    /// Reconstruct descriptions only after the caller has validated closed41 and
    /// the retained exclusive owner. The transition authenticates the old plan;
    /// this is not new approval and does not establish current-table preservation.
    pub(super) fn recover_in(
        db: &Connection,
        original_plan: &Value,
        transition: &transition::Transition,
    ) -> Result<Self> {
        recovery_preflight(original_plan)?;
        for (value, keys) in [
            (
                original_plan,
                &[
                    "contract",
                    "from_schema",
                    "to_schema",
                    "input",
                    "input_fingerprint",
                    "migrations",
                    "migration_resources",
                    "native_schema_fingerprint",
                    "work_settled",
                    "quiescence_required",
                    "execution_journal_modes",
                    "quiescence_preconditions",
                    "rollback",
                    "plan_fingerprint",
                ][..],
            ),
            (
                &original_plan["input"],
                &[
                    "database",
                    "file_identity",
                    "database_id",
                    "schema",
                    "history",
                    "tables",
                    "sequences",
                    "pending_work",
                    "journal_mode",
                    "catalog_identity",
                    "qualified_pending_work",
                    "catalog_state",
                ][..],
            ),
        ] {
            let object = value
                .as_object()
                .ok_or_else(|| Error("recovery plan shape missing".into()))?;
            if object.len() != keys.len() || keys.iter().any(|key| !object.contains_key(*key)) {
                return Err(Error("recovery plan has unknown or missing fields".into()));
            }
        }
        if db.is_autocommit() {
            return Err(Error(
                "recovery requires retained writer transaction".into(),
            ));
        }
        // Validate the scalar transition shape as well as its already-authenticated
        // correspondence with this exact immutable row.
        transition.digest()?;
        if transition::read_in(db)?.as_ref() != Some(transition) {
            return Err(Error(
                "recovery transition differs from retained row".into(),
            ));
        }
        let mut unsigned = original_plan.clone();
        let object = unsigned
            .as_object_mut()
            .ok_or_else(|| Error("recovery plan must be an object".into()))?;
        let claimed = object
            .remove("plan_fingerprint")
            .ok_or_else(|| Error("recovery plan fingerprint absent".into()))?;
        if claimed.as_str() != Some(transition.approved_plan_sha256.as_str())
            || sha256(encode(&unsigned)) != transition.approved_plan_sha256
        {
            return Err(Error("recovery plan commitment differs".into()));
        }
        let input = &original_plan["input"];
        let expected_step = description(&closed41::Closed41Target.step());
        let mut resources = steps(40)?.iter().map(description).collect::<Vec<_>>();
        resources.push(expected_step.clone());
        let reference = crate::native41_resources::reference_sha256()?;
        if original_plan["contract"] != "catabolic-native-offline40-to41-v1"
            || original_plan["from_schema"] != 40
            || original_plan["to_schema"] != 41
            || original_plan["input_fingerprint"] != sha256(encode(input))
            || input["schema"] != 40
            || original_plan["migrations"] != json!([expected_step])
            || original_plan["migration_resources"] != json!(resources)
            || original_plan["native_schema_fingerprint"] != reference
            || transition.closed41_reference_sha256 != reference
            || transition.migration41_sql_sha256 != crate::native41_resources::sql_sha256()
            || transition.migrated_generation != transition.source_generation
        {
            return Err(Error(
                "recovery contract/resources/input commitment differs".into(),
            ));
        }
        let ledger = input
            .get("history")
            .filter(|v| v.is_array())
            .ok_or_else(|| Error("recovery full ledger missing".into()))?;
        let tables = input
            .get("tables")
            .filter(|v| v.is_object())
            .ok_or_else(|| Error("recovery source tables missing".into()))?;
        if sha256(encode(ledger)) != transition.source_full_ledger_sha256
            || sha256(encode(tables)) != transition.source_tables_sha256
        {
            return Err(Error(
                "recovery original ledger/table commitments differ".into(),
            ));
        }
        let actual_ledger = json!(rows(
            db,
            "SELECT version,filename,checksum,origin,applied_at FROM schema_migrations WHERE version<=40 ORDER BY version",
            &[]
        )?);
        if ledger.as_array().map(Vec::len) != Some(steps(40)?.len()) || *ledger != actual_ledger {
            return Err(Error("recovery retained old ledger differs".into()));
        }
        let identity = &input["catalog_identity"];
        let file_identity = &input["file_identity"];
        if input["database_id"].as_str() != Some(transition.catalog_id.as_str())
            || identity
                != &json!({"catalog_id":transition.catalog_id,"incarnation":transition.source_incarnation})
            || file_identity.as_object().map(|o| o.len()) != Some(2)
            || file_identity["device"].as_u64().is_none()
            || file_identity["inode"].as_u64().is_none()
            || !input["database"]
                .as_str()
                .is_some_and(|p| Path::new(p).is_absolute())
        {
            return Err(Error(
                "recovery source identity/file description malformed".into(),
            ));
        }
        let catalog_state = &input["catalog_state"];
        let state = catalog_state
            .as_array()
            .filter(|v| v.len() == 1)
            .and_then(|v| v[0].as_object())
            .ok_or_else(|| Error("recovery raw catalog state missing".into()))?;
        if state.get("id") != Some(&json!(1))
            || state.get("contract_version") != Some(&json!(1))
            || state.get("catalog_id").and_then(Value::as_str)
                != Some(transition.catalog_id.as_str())
            || state.get("incarnation").and_then(Value::as_str)
                != Some(transition.source_incarnation.as_str())
            || state.get("generation").and_then(Value::as_i64) != Some(transition.source_generation)
        {
            return Err(Error(
                "recovery original catalog state identity differs".into(),
            ));
        }
        let columns = db
            .prepare("PRAGMA table_info(catalog_state)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if state.len() != columns.len() || tables["catalog_state"]["columns"] != json!(columns) {
            return Err(Error("recovery raw catalog state columns differ".into()));
        }
        if tables["catalog_state"] != recovery_catalog_state_digest(state, &columns)? {
            return Err(Error(
                "recovery raw catalog state/table digest differs".into(),
            ));
        }
        Ok(Self {
            plan_digest: transition.approved_plan_sha256.clone(),
            ledger: ledger.clone(),
            tables: tables.clone(),
            catalog: transition.catalog_id.clone(),
            incarnation: transition.source_incarnation.clone(),
            generation: transition.source_generation,
            file_identity: file_identity.clone(),
            catalog_state: catalog_state.clone(),
        })
    }

    pub(super) fn capture(db: &Connection, plan: &Value) -> Result<Self> {
        if plan["from_schema"] != 40 || plan["to_schema"] != 41 || db.is_autocommit() {
            return Err(Error(
                "exact locked40-to41 preservation input required".into(),
            ));
        }
        let ledger = json!(rows(
            db,
            "SELECT version,filename,checksum,origin,applied_at FROM schema_migrations ORDER BY version",
            &[]
        )?);
        if ledger != plan["input"]["history"] {
            return Err(Error(
                "full source ledger differs from approved plan".into(),
            ));
        }
        if db.prepare("SELECT 1 FROM catalog_entity_revisions WHERE entity_kind IN ('catalog_nfo_states','catalog_nfo_state_revisions','catalog_nfo_identity_claims','catalog_nfo_file_attributions','catalog_nfo_bootstrap') LIMIT 1")?.exists([])? {
            return Err(Error("source40 has foreign future NFO entity heads".into()));
        }
        let catalog_state = json!(rows(db, "SELECT * FROM catalog_state ORDER BY id", &[])?);
        let state_columns = db
            .prepare("PRAGMA table_info(catalog_state)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if plan["input"]["catalog_state"] != catalog_state
            || digest_table(db, "catalog_state", &state_columns)?
                != plan["input"]["tables"]["catalog_state"]
        {
            return Err(Error("source catalog state differs from exact plan".into()));
        }
        let (catalog, incarnation, generation) = db.query_row(
            "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(Self {
            plan_digest: plan["plan_fingerprint"]
                .as_str()
                .ok_or_else(|| Error("approved plan fingerprint missing".into()))?
                .into(),
            ledger,
            tables: plan["input"]["tables"].clone(),
            catalog,
            incarnation,
            generation,
            file_identity: plan["input"]["file_identity"].clone(),
            catalog_state,
        })
    }
    pub(super) fn allocated_bytes(&self) -> Result<u64> {
        let mut bytes = std::mem::size_of::<Self>() as u64;
        for value in [
            &self.ledger,
            &self.tables,
            &self.file_identity,
            &self.catalog_state,
        ] {
            bytes = bytes
                .checked_add(allocation::value_bytes(value)?)
                .ok_or_else(|| Error("offline preservation capacity overflow".into()))?;
        }
        for text in [&self.plan_digest, &self.catalog, &self.incarnation] {
            bytes = bytes
                .checked_add(text.capacity() as u64)
                .ok_or_else(|| Error("offline preservation capacity overflow".into()))?;
        }
        Ok(bytes)
    }
    pub(super) fn verify_backup(
        &self,
        source: &Connection,
        allocation: &allocation::AllocationContext,
        path: &Path,
        expected_sha: &str,
    ) -> Result<backup::VerifiedBackup> {
        backup::VerifiedBackup::open_reserved(
            source,
            allocation,
            path,
            &self.catalog,
            &self.incarnation,
            &self.ledger,
            &self.tables,
            expected_sha,
        )
    }
    /// Only fixed native apply may lend its still-charged metadata workspace.
    /// Recovery and standalone verification keep the full existing charge above.
    pub(super) fn verify_backup_with_workspace(
        &self,
        workspace: &mut allocation::ApplyMetadataLoan<'_, '_>,
        path: &Path,
        expected_sha: &str,
    ) -> Result<backup::VerifiedBackup> {
        backup::VerifiedBackup::open_with_workspace(
            workspace,
            path,
            &self.catalog,
            &self.incarnation,
            &self.ledger,
            &self.tables,
            expected_sha,
        )
    }
    pub(super) fn verify_committed_in(
        &self,
        db: &Connection,
        verified_backup_sha: &str,
    ) -> Result<String> {
        if !db.is_autocommit() {
            return Err(Error("migration completion transaction still open".into()));
        }
        self.verify_preserved_in(db, verified_backup_sha)
    }
    pub(super) fn verify_preserved_in(
        &self,
        db: &Connection,
        verified_backup_sha: &str,
    ) -> Result<String> {
        closed41::Closed41Target.validate(db)?;
        preservation(db, &self.tables)?;
        let ledger = json!(rows(
            db,
            "SELECT version,filename,checksum,origin,applied_at FROM schema_migrations WHERE version<=40 ORDER BY version",
            &[]
        )?);
        if ledger != self.ledger {
            return Err(Error("committed full source ledger differs".into()));
        }
        let row =
            transition::read_in(db)?.ok_or_else(|| Error("committed transition missing".into()))?;
        if row.catalog_id != self.catalog
            || row.source_incarnation != self.incarnation
            || row.source_generation != self.generation
            || row.migrated_generation != self.generation
            || row.approved_plan_sha256 != self.plan_digest
            || row.source_full_ledger_sha256 != sha256(encode(&self.ledger))
            || row.source_tables_sha256 != sha256(encode(&self.tables))
            || row.verified_backup_image_sha256 != verified_backup_sha
            || row.migration41_sql_sha256 != crate::native41_resources::sql_sha256()
            || row.closed41_reference_sha256 != crate::native41_resources::reference_sha256()?
        {
            return Err(Error(
                "committed transition differs from retained migration proof".into(),
            ));
        }
        row.digest()
    }
    pub(super) fn identity(&self) -> (&str, &str) {
        (&self.catalog, &self.incarnation)
    }
    pub(super) fn validate_path(&self, path: &Path) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || json!({"device":metadata.dev(),"inode":metadata.ino()}) != self.file_identity
        {
            return Err(Error("retained source path identity changed".into()));
        }
        Ok(())
    }
    /// Invoked only AFTER the independently verified exact bootstrap receipt,
    /// complete NFO rows and exact new entity-head set have been established.
    pub(super) fn verify_bootstrap_delta(
        &self,
        db: &Connection,
        command: &catabolic_core::catalog_command::CommandEnvelope,
        completion: &crate::catalog_nfo_attribution::VerifiedBootstrapCompletion,
    ) -> Result<()> {
        let expected_generation = self
            .generation
            .checked_add(completion.generation_delta())
            .ok_or_else(|| Error("bootstrap generation delta overflow".into()))?;
        if command.expected_generation() != self.generation
            || completion.committed_generation() != expected_generation
        {
            return Err(Error(
                "bootstrap generation differs from exact inserted heads".into(),
            ));
        }
        let mut state = json!(rows(db, "SELECT * FROM catalog_state ORDER BY id", &[])?);
        if state.as_array().map(Vec::len) != Some(1)
            || state[0]["generation"] != expected_generation
        {
            return Err(Error("bootstrap catalog state differs".into()));
        }
        state[0]["generation"] = json!(self.generation);
        if state != self.catalog_state {
            return Err(Error("bootstrap changed catalog identity fields".into()));
        }
        for (table, expected) in self
            .tables
            .as_object()
            .ok_or_else(|| Error("preserved table capture missing".into()))?
        {
            if table == "catalog_state" {
                continue;
            }
            let columns = expected["columns"]
                .as_array()
                .ok_or_else(|| Error("preserved columns missing".into()))?
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| Error("preserved column malformed".into()))
                })
                .collect::<Result<Vec<_>>>()?;
            let actual = if table == "catalog_command_receipts" {
                digest_table_where_params(
                    db,
                    table,
                    &columns,
                    "WHERE NOT (catalog_id=?1 AND host_namespace=?2 AND operation_id=?3)",
                    &[
                        command.key().catalog_id().to_owned().into(),
                        command.key().host_namespace().to_owned().into(),
                        command.key().operation_id().to_owned().into(),
                    ],
                )?
            } else if table == "catalog_entity_revisions" {
                // Original40 capture proved these domains empty; completion
                // independently proves their exact new key/value membership.
                digest_table_where(
                    db,
                    table,
                    &columns,
                    "WHERE entity_kind NOT IN ('catalog_nfo_states','catalog_nfo_state_revisions','catalog_nfo_identity_claims','catalog_nfo_file_attributions','catalog_nfo_bootstrap')",
                )?
            } else {
                digest_table(db, table, &columns)?
            };
            if actual != *expected {
                return Err(Error(format!("bootstrap changed preserved table {table}")));
            }
        }
        Ok(())
    }
    pub(super) fn append_in(&self, db: &Connection, verified_backup_sha: &str) -> Result<()> {
        self.append_in_with_image(db, verified_backup_sha, None)
    }
    pub(super) fn append_in_with_image(
        &self,
        db: &Connection,
        verified_backup_sha: &str,
        image: Option<&allocation::PaidImageSqliteLoan<'_>>,
    ) -> Result<()> {
        // The shared migration runner has already applied and validated the exact
        // registered resource. Repeat complete preservation after observer return.
        preservation_with_image(db, &self.tables, image)?;
        let ledger = json!(rows_with_image(
            db,
            "SELECT version,filename,checksum,origin,applied_at FROM schema_migrations WHERE version<=40 ORDER BY version",
            &[], image
        )?);
        if ledger != self.ledger {
            return Err(Error("migration changed the full source40 ledger".into()));
        }
        closed41::Closed41Target.validate_with_image(db, image)?;
        let migrated_generation: i64 =
            db.query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })?;
        let row = transition::Transition {
            catalog_id: self.catalog.clone(),
            source_incarnation: self.incarnation.clone(),
            source_generation: self.generation,
            approved_plan_sha256: self.plan_digest.clone(),
            source_full_ledger_sha256: sha256(encode(&self.ledger)),
            source_tables_sha256: sha256(encode(&self.tables)),
            verified_backup_image_sha256: verified_backup_sha.into(),
            migration41_sql_sha256: crate::native41_resources::sql_sha256(),
            closed41_reference_sha256: crate::native41_resources::reference_sha256()?,
            migrated_generation,
        };
        transition::PreservedTransition::after_preservation(db, row)?.append()?;
        // The insertion cannot mutate old rows or the source ledger through an
        // unexpected trigger. No observer runs while the exact-row UDF exists.
        preservation_with_image(db, &self.tables, image)?;
        let after = json!(rows_with_image(
            db,
            "SELECT version,filename,checksum,origin,applied_at FROM schema_migrations WHERE version<=40 ORDER BY version",
            &[], image
        )?);
        if after != self.ledger {
            return Err(Error("transition insertion changed source ledger".into()));
        }
        Ok(())
    }
}

/// Parity with digest_table's SQLite storage-class tuples and column order.
/// Caller preflights the complete description before invoking this encoder.
fn recovery_catalog_state_digest(
    state: &serde_json::Map<String, Value>,
    columns: &[String],
) -> Result<Value> {
    use sha2::{Digest, Sha256};
    let typed = columns
        .iter()
        .map(|column| match state.get(column) {
            Some(Value::Null) => Ok(json!(["NoneType", null])),
            Some(Value::String(value)) => Ok(json!(["str", value])),
            Some(Value::Number(value)) if value.as_i64().is_some() => Ok(json!(["int", value])),
            _ => Err(Error("recovery raw catalog state value invalid".into())),
        })
        .collect::<Result<Vec<_>>>()?;
    let mut hash = Sha256::new();
    hash.update(encode_with(&Value::Array(typed), true, true).as_bytes());
    hash.update(b"\n");
    Ok(json!({"columns":columns,"rows":1,"sha256":format!("{:x}",hash.finalize())}))
}

/// Explicit private recovery cohort, before any plan clone or serialization.
/// Worst-case JSON escaping reserves six bytes per UTF-8 input byte; integers,
/// punctuation and per-node overhead are conservatively charged as well.
fn recovery_preflight(value: &Value) -> Result<()> {
    fn visit(value: &Value, depth: usize, nodes: &mut usize, bytes: &mut usize) -> Result<()> {
        if depth > 32 {
            return Err(Error("recovery plan depth exceeds32".into()));
        }
        *nodes = nodes.saturating_add(1);
        *bytes = bytes.saturating_add(32);
        if *nodes > 65_536 || *bytes > 4 * 1024 * 1024 {
            return Err(Error(
                "recovery plan exceeds private node/byte cohort".into(),
            ));
        }
        match value {
            Value::String(text) => *bytes = bytes.saturating_add(text.len().saturating_mul(6)),
            Value::Array(values) => {
                for value in values {
                    visit(value, depth + 1, nodes, bytes)?;
                }
            }
            Value::Object(values) => {
                for (key, value) in values {
                    *bytes = bytes.saturating_add(key.len().saturating_mul(6).saturating_add(4));
                    visit(value, depth + 1, nodes, bytes)?;
                }
            }
            _ => {}
        }
        if *bytes > 4 * 1024 * 1024 {
            return Err(Error("recovery plan exceeds private byte cohort".into()));
        }
        Ok(())
    }
    visit(value, 0, &mut 0, &mut 0)
}

#[cfg(test)]
mod recovery_preflight_tests {
    use super::*;
    #[test]
    fn bounded_recovery_description_refuses_depth_nodes_and_escaped_bytes() {
        assert!(recovery_preflight(&json!({"small":[1,"opaque catalog",null]})).is_ok());
        let mut deep = Value::Null;
        for _ in 0..34 {
            deep = json!([deep]);
        }
        assert!(recovery_preflight(&deep).is_err());
        assert!(recovery_preflight(&Value::Array(vec![Value::Null; 65_536])).is_err());
        assert!(recovery_preflight(&json!("x".repeat(700_000))).is_err());
    }
}

#[cfg(test)]
mod recovery_catalog_state_encoder_tests {
    use super::*;
    fn actual40() -> (
        tempfile::TempDir,
        serde_json::Map<String, Value>,
        Vec<String>,
        Value,
    ) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("encoder-parity.db");
        initialize_native_at(&path, 40).unwrap();
        let db = Connection::open(&path).unwrap();
        let mut raw = rows(&db, "SELECT * FROM catalog_state ORDER BY id", &[]).unwrap();
        assert_eq!(raw.len(), 1);
        let state = raw.remove(0).as_object().unwrap().clone();
        let columns = db
            .prepare("PRAGMA table_info(catalog_state)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        let expected = digest_table(&db, "catalog_state", &columns).unwrap();
        (root, state, columns, expected)
    }
    #[test]
    fn initialized40_catalog_state_encoder_matches_storage_digest() {
        let (_root, state, columns, expected) = actual40();
        assert_eq!(
            recovery_catalog_state_digest(&state, &columns).unwrap(),
            expected
        );
    }
    #[test]
    fn catalog_state_encoder_preserves_integer_versus_text_storage_class() {
        let (_root, mut state, columns, expected) = actual40();
        let generation = state["generation"].as_i64().unwrap();
        state.insert("generation".into(), json!(generation.to_string()));
        let changed = recovery_catalog_state_digest(&state, &columns).unwrap();
        assert_ne!(changed["sha256"], expected["sha256"]);
    }
    #[test]
    fn catalog_state_encoder_preserves_column_order() {
        let (_root, state, mut columns, expected) = actual40();
        let id = columns.iter().position(|c| c == "id").unwrap();
        let catalog = columns.iter().position(|c| c == "catalog_id").unwrap();
        columns.swap(id, catalog);
        let changed = recovery_catalog_state_digest(&state, &columns).unwrap();
        assert_ne!(changed["sha256"], expected["sha256"]);
        assert_ne!(changed["columns"], expected["columns"]);
    }
}
