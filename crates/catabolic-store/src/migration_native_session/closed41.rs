//! Sealed offline target, not normal schema support or an application override.
use super::*;

pub(in crate::migration) struct Closed41Target;
impl Closed41Target {
    pub(in crate::migration) fn step(&self) -> Migration {
        Migration {
            version: 41,
            filename: "0041_native_integration.sql",
            sql: crate::native41_resources::sql(),
        }
    }
    pub(in crate::migration) fn plan(&self, db: &Connection, path: &Path) -> Result<Value> {
        if allocation::current().is_some() {
            allocation::catalog_preflight(db)?;
        }
        let mut plan = native_plan(db, path, 40)?;
        if plan["from_schema"] != 40 {
            return Err(Error(
                "offline41 requires exact closed40 predecessor".into(),
            ));
        }
        plan["input"]["catalog_state"] =
            json!(rows(db, "SELECT * FROM catalog_state ORDER BY id", &[])?);
        plan["input_fingerprint"] = json!(sha256(encode(&plan["input"])));
        plan["contract"] = json!("catabolic-native-offline40-to41-v1");
        plan["to_schema"] = json!(41);
        plan["migrations"] = json!([description(&self.step())]);
        let mut resources = steps(40)?.iter().map(description).collect::<Vec<_>>();
        resources.push(description(&self.step()));
        plan["migration_resources"] = json!(resources);
        plan["native_schema_fingerprint"] = json!(crate::native41_resources::reference_sha256()?);
        plan.as_object_mut()
            .expect("owner plan object")
            .remove("plan_fingerprint");
        plan["plan_fingerprint"] = json!(sha256(encode(&plan)));
        Ok(plan)
    }
    pub(in crate::migration) fn record(&self, db: &Connection) -> Result<()> {
        let step = self.step();
        db.execute("INSERT INTO schema_migrations(version,filename,checksum,origin) VALUES(41,?,?,'migrated')", params![step.filename,sha256(step.sql)])?;
        Ok(())
    }
    /// Closed structural/ledger/identity/settings validation only. It does not
    /// infer bootstrap completion, fresh initialization, or worker cleanup.
    pub(in crate::migration) fn validate_identity(&self, db: &Connection) -> Result<()> {
        self.validate_identity_with_image(db, None)
    }
    fn validate_identity_with_image(
        &self,
        db: &Connection,
        image: Option<&allocation::PaidImageSqliteLoan<'_>>,
    ) -> Result<()> {
        if allocation::current().is_some() {
            allocation::catalog_preflight(db)?;
        }
        let version: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version != 41
            || crate::native41_resources::schema(db)?
                != crate::native41_resources::reference()?.schema
        {
            return Err(Error("offline41 exact resource signature differs".into()));
        }
        let actual = match image {
            // A copied image cannot qualify as the original retained source
            // WRITE cursor. Preserve that path's exact former fallback SELECT.
            Some(image) => rows_with_image(db,
                "SELECT version,filename,checksum FROM schema_migrations ORDER BY version",
                &[], Some(image))?,
            None => allocation::identity_ledger_rows(db, allocation::IdentityLedger::Current)?,
        };
        let expected = crate::native41_resources::reference()?;
        let mut ledger = expected
            .predecessor_ledger
            .as_array()
            .ok_or_else(|| Error("closed40 ledger unavailable".into()))?
            .clone();
        ledger.push(json!([41, self.step().filename, sha256(self.step().sql)]));
        let observed = actual
            .iter()
            .map(|r| json!([r["version"], r["filename"], r["checksum"]]))
            .collect::<Vec<_>>();
        if observed != ledger {
            return Err(Error("offline41 complete ledger differs".into()));
        }
        let origin_ok: bool = db.query_row("SELECT count(*)=1 FROM schema_migrations WHERE version=41 AND origin='migrated' AND typeof(applied_at)='text' AND length(applied_at)>0",[],|r|r.get(0))?;
        if !origin_ok {
            return Err(Error(
                "offline41 migration ledger provenance differs".into(),
            ));
        }
        for (name, wanted) in [
            ("foreign_keys", 1_i64),
            ("recursive_triggers", 1),
            ("read_uncommitted", 0),
        ] {
            let value: i64 = db.pragma_query_value(None, name, |r| r.get(0))?;
            if value != wanted {
                return Err(Error("offline41 unsafe connection settings".into()));
            }
        }
        let id: String =
            db.query_row("SELECT value FROM meta WHERE key='database_id'", [], |r| {
                r.get(0)
            })?;
        let canonical = Uuid::parse_str(&id)
            .map_err(|_| Error("offline41 invalid database identity".into()))?
            .to_string();
        let valid:bool=db.query_row("SELECT count(*)=1 FROM catalog_state WHERE id=1 AND catalog_id=? AND typeof(generation)='integer' AND generation>=0 AND contract_version=1 AND length(CAST(incarnation AS BLOB))=32 AND incarnation NOT GLOB '*[^0-9a-f]*'",[canonical],|r|r.get(0))?;
        if !valid {
            return Err(Error("offline41 inconsistent native identity".into()));
        }
        Ok(())
    }
    pub(in crate::migration) fn validate(&self, db: &Connection) -> Result<()> {
        self.validate_with_image(db, None)
    }
    pub(in crate::migration) fn validate_with_image(
        &self,
        db: &Connection,
        image: Option<&allocation::PaidImageSqliteLoan<'_>>,
    ) -> Result<()> {
        self.validate_without_hook_change_with_image(db, image)?;
        crate::catalog_source_succession::authorizer::install_validated_closed41(db)?;
        Ok(())
    }
    pub(in crate::migration) fn validate_observed_plan(
        &self,
        db: &Connection,
        observer: &crate::catalog_source_succession::authorizer::PlannedReadObserver,
    ) -> Result<()> {
        observer.check(db)?;
        self.validate_without_hook_change(db)?;
        observer.check(db)
    }
    fn validate_without_hook_change(&self, db: &Connection) -> Result<()> {
        self.validate_without_hook_change_with_image(db, None)
    }
    fn validate_without_hook_change_with_image(
        &self,
        db: &Connection,
        image: Option<&allocation::PaidImageSqliteLoan<'_>>,
    ) -> Result<()> {
        self.validate_identity_with_image(db, image)?;
        if db.prepare("SELECT 1 FROM catalog_commit_barrier UNION ALL SELECT 1 FROM catalog_mutation_permits UNION ALL SELECT 1 FROM catalog_metadata_permits UNION ALL SELECT 1 FROM catalog_logical_edit_permits UNION ALL SELECT 1 FROM catalog_source_succession_permits UNION ALL SELECT 1 FROM catalog_nfo_source_effect_permits LIMIT 1")?.exists([])? {
            return Err(Error("offline41 unclosed owner permit".into()));
        }
        if db.prepare("SELECT 1 FROM native_execution_lifetimes WHERE state IS NOT 'owned' AND NOT (state IS 'quiescent' AND cleanup_kind IS 'builtin_no_child_v1' AND child_obligations IS 0 AND verified_retirements IS 0 AND capacity_released IS 1 AND sealed_at IS NOT NULL) LIMIT 1")?.exists([])? {
            return Err(Error("offline41 malformed terminal worker lifetime".into()));
        }
        crate::catalog_search_index::validate_closed(db)?;
        if db.prepare("PRAGMA foreign_key_check")?.exists([])? {
            return Err(Error("offline41 foreign key violation".into()));
        }
        let mut check = db.prepare("PRAGMA integrity_check")?;
        let result = check
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if result != ["ok"] {
            return Err(Error("offline41 integrity failure".into()));
        }
        Ok(())
    }
}
