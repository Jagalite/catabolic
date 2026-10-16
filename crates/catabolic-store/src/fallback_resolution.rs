// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Native fallback evidence, globally bounded probe ownership and resolution.
use crate::{Error, Result, Store, filesystem_mutation, observation, source};
use rusqlite::params;
use serde_json::{Value, json};
use std::{
    os::unix::fs::FileExt,
    path::Path,
    time::{Duration, Instant},
};

pub fn epoch(store: &Store) -> Result<i64> {
    Ok(store.db.query_row(
        "SELECT generation FROM fallback_epoch WHERE id=1",
        [],
        |r| r.get(0),
    )?)
}
pub fn require_epoch(store: &Store, expected: i64) -> Result<()> {
    if epoch(store)? == expected {
        Ok(())
    } else {
        Err(Error("stale_resolution_plan".into()))
    }
}
fn writer(path: &Path, database_id: &str) -> Result<Store> {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match Store::open(path, true, false) {
            Ok(store) => {
                if store.database_id != database_id {
                    return Err(Error("catalog replaced during fallback probe".into()));
                }
                return Ok(store);
            }
            Err(error)
                if error.0 == "another Catabolic writer or upgrade is using this database"
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(error) => return Err(error),
        }
    }
}
pub fn process_alive(pid: i64) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal zero only observes the supplied process, never signals it.
    (unsafe { libc::kill(pid, 0) == 0 })
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}
#[derive(Clone, Debug)]
pub struct ProbeSlot {
    pub id: i64,
    pub owner: String,
}
pub fn reserve_probe(path: &Path, database_id: &str) -> Result<Option<ProbeSlot>> {
    let store = writer(path, database_id)?;
    let slots = crate::migration::rows(
        &store.db,
        "SELECT * FROM fallback_probe_slots ORDER BY id",
        &[],
    )?;
    let native = crate::migration::is_supported_native_schema(store.schema_version);
    let Some(row) = slots.iter().find(|r| {
        if native {
            r["owner"].is_null() && r["pid"].is_null()
        } else {
            r["pid"].as_i64().is_none_or(|pid| !process_alive(pid))
        }
    }) else {
        return Ok(None);
    };
    let slot = ProbeSlot {
        id: row["id"].as_i64().unwrap(),
        owner: uuid::Uuid::new_v4().to_string(),
    };
    store.transaction(|db| {
        db.execute(
            "UPDATE fallback_probe_slots SET owner=?,pid=? WHERE id=?",
            params![slot.owner, std::process::id(), slot.id],
        )?;
        Ok(())
    })?;
    Ok(Some(slot))
}
pub fn attach_probe(path: &Path, database_id: &str, slot: &ProbeSlot, pid: u32) -> Result<()> {
    let store = writer(path, database_id)?;
    store.transaction(|db| {
        let changed = db.execute(
            "UPDATE fallback_probe_slots SET pid=? WHERE id=? AND owner=?",
            params![pid, slot.id, slot.owner],
        )?;
        if changed != 1 {
            return Err(Error("fallback probe slot superseded".into()));
        }
        Ok(())
    })
}
pub fn release_probe(path: &Path, database_id: &str, slot: &ProbeSlot) -> Result<()> {
    let store = writer(path, database_id)?;
    store.transaction(|db| {
        db.execute(
            "UPDATE fallback_probe_slots SET owner=NULL,pid=NULL WHERE id=? AND owner=?",
            params![slot.id, slot.owner],
        )?;
        Ok(())
    })
}
pub fn probe_check(request: &Value) -> Result<Value> {
    request["snapshots"]
        .as_array()
        .filter(|v| (1..=65).contains(&v.len()))
        .ok_or_else(|| Error("probe_snapshot_limit".into()))?;
    let path = request["database"]
        .as_str()
        .ok_or_else(|| Error("invalid fallback probe request".into()))?;
    let store = Store::open(Path::new(path), false, false)?;
    probe_check_with_store(&store, request)
}
/// Retain the caller's already checked native catalog session through source IO.
pub fn probe_check_with_store(store: &Store, request: &Value) -> Result<Value> {
    store.validate_native_identity()?;
    let text = |key: &str| {
        request[key]
            .as_str()
            .ok_or_else(|| Error("invalid fallback probe request".into()))
    };
    let snapshots = request["snapshots"]
        .as_array()
        .filter(|v| (1..=65).contains(&v.len()))
        .ok_or_else(|| Error("probe_snapshot_limit".into()))?;
    if store.database_id != text("database_id")? {
        return Err(Error("catalog replaced during fallback probe".into()));
    }
    let profile = text("profile")?;
    for snapshot in snapshots {
        store.validate_native_identity()?;
        let checked = source::with_validated_source(snapshot, |file| {
            let mut magic = [0u8; 16];
            if file.read_at(&mut magic, 0)? == 16 && &magic == b"SQLite format 3\0" {
                return Err(Error("private_content".into()));
            }
            let revision = filesystem_mutation::version(&file.metadata()?);
            let header = observation::header_check(
                file,
                snapshot["path"]
                    .as_str()
                    .ok_or_else(|| Error("invalid source path".into()))?,
                &revision,
            )?;
            if header["status"] == "invalid" {
                return Ok(Some(header["reason"].clone()));
            }
            Ok(filesystem_mutation::recorded_reason(
                store,
                profile,
                snapshot["id"]
                    .as_str()
                    .ok_or_else(|| Error("invalid source identity".into()))?,
                &revision,
            )?
            .map(Value::String))
        });
        match checked {
            Ok(Some(reason)) => return Ok(json!({"usable":false,"reason":reason})),
            Ok(None) => {}
            Err(_) => return Ok(json!({"usable":false,"reason":"source_unavailable_or_changed"})),
        }
    }
    Ok(json!({"usable":true,"reason":null}))
}

use crate::{
    access::Access,
    selection::{Evaluation, Selected},
};
use catabolic_core::{
    encode,
    resolution::{Candidate, Failback, FailbackEffect, FailbackMode, Observation, TierEffect},
    sha256,
};
use std::collections::{BTreeMap, BTreeSet};
pub type Probe<'a> = dyn FnMut(&Path, &str, &str, &Value, u64) -> Result<Value> + 'a;
pub enum Context<'a> {
    Local(&'a mut Evaluation),
    Http(&'a mut Access),
    Borrowed(&'a Store, &'a str),
}
impl Context<'_> {
    pub fn store(&self) -> &Store {
        match self {
            Self::Local(e) => e.store(),
            Self::Http(a) => a.store(),
            Self::Borrowed(store, _) => store,
        }
    }
    pub fn profile(&self) -> &str {
        match self {
            Self::Local(e) => &e.profile,
            Self::Http(a) => a.profile(),
            Self::Borrowed(_, profile) => profile,
        }
    }
    fn http(&self) -> bool {
        matches!(self, Self::Http(_))
    }
    fn select(
        &mut self,
        id: &str,
        timeout: u64,
        maximum: usize,
        policy: Option<crate::access::HttpSelectionPolicy<'_>>,
        checkpoint: &mut Option<crate::access::HttpSelectionCheckpoint>,
    ) -> Result<Selected> {
        let profile = self.profile().to_owned();
        match self {
            Self::Borrowed(store, _) => {
                if !store.db.is_autocommit() {
                    return Err(Error(
                        "metadata fallback query requires an autocommit owner".into(),
                    ));
                }
                let snapshot = Store::open(&store.path, false, false)?;
                if snapshot.database_id != store.database_id {
                    return Err(Error("stale_resolution_plan".into()));
                }
                let mut evaluation = Evaluation::new(snapshot, &profile, timeout, maximum, false)?;
                evaluation.select(&json!({"query_id":id,"profile":profile}))
            }
            Self::Local(e) => {
                e.use_session_limits();
                e.limit_budget(timeout, maximum);
                e.select(&json!({"query_id":id,"profile":profile}))
            }
            Self::Http(a) => {
                // The principal's scoped query session ends before any probe;
                // the resolver retains only complete, epoch-pinned IDs.
                let store = Store::open(&a.store().path, false, false)?;
                if store.database_id != a.store().database_id {
                    return Err(Error("authorization_snapshot_mismatch".into()));
                }
                Access::from_stored_token(store, a.token_id())?
                    .scoped_selection(id, timeout, maximum, policy, checkpoint)
            }
        }
    }
    pub(crate) fn detached<T>(&mut self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        match self {
            Self::Local(e) => e.detached(operation),
            Self::Http(a) => a.detached(operation),
            Self::Borrowed(..) => Err(Error(
                "borrowed fallback evidence cannot detach or probe".into(),
            )),
        }
    }
    pub fn account_value(&mut self, value: &Value) -> Result<()> {
        match self {
            Self::Local(e) => e.account_value(value),
            Self::Http(a) => a.evaluation_mut().account_value(value),
            Self::Borrowed(..) => {
                if encode(value).len() > 4 * 1024 * 1024 {
                    Err(Error("evaluation_result_byte_budget".into()))
                } else {
                    Ok(())
                }
            }
        }
    }
}
pub struct Resolver<'a, 'b> {
    pub context: Context<'a>,
    pub primary_choices: BTreeMap<String, String>,
    pub operation_id: Option<String>,
    probe: &'b mut Probe<'b>,
    queries: BTreeMap<String, Selected>,
    checks: BTreeMap<String, bool>,
    suppressed: BTreeSet<String>,
    pub total_checks: usize,
    checked: usize,
    check_limit: usize,
    captured_epoch: i64,
    timeout_ms: u64,
    query_budget: Duration,
    query_maximum: usize,
    selected_ids: usize,
    package_policy: bool,
    policy_id: Option<String>,
    scoped_graphql_evidence: bool,
    native_identity_read: bool,
    http_checkpoint: Option<crate::access::HttpSelectionCheckpoint>,
}
impl<'a, 'b> Resolver<'a, 'b> {
    pub fn new(
        context: Context<'a>,
        probe: &'b mut Probe<'b>,
        operation_id: Option<String>,
    ) -> Self {
        Self {
            context,
            probe,
            operation_id,
            primary_choices: BTreeMap::new(),
            queries: BTreeMap::new(),
            checks: BTreeMap::new(),
            suppressed: BTreeSet::new(),
            total_checks: 0,
            checked: 0,
            check_limit: 0,
            captured_epoch: 0,
            timeout_ms: 1000,
            query_budget: Duration::from_secs(60),
            query_maximum: 100000,
            selected_ids: 0,
            package_policy: false,
            policy_id: None,
            scoped_graphql_evidence: false,
            native_identity_read: false,
            http_checkpoint: None,
        }
    }
    /// Only the authoritative persistent identity bridge may opt into typed
    /// metadata capture. This does not authorize publication or interval effects.
    pub(crate) fn native_identity_read(&mut self) {
        self.native_identity_read = true;
    }
    pub(crate) fn admit_binding_read(&self, association: &str) -> Result<()> {
        if self.native_identity_read {
            Ok(())
        } else {
            crate::catalog_identity_mutation::legacy_association_admission(
                self.store(),
                self.profile(),
                association,
            )
        }
    }
    pub fn store(&self) -> &Store {
        self.context.store()
    }
    pub fn profile(&self) -> &str {
        self.context.profile()
    }
    pub fn query(&mut self, id: &str) -> Result<Selected> {
        if let Some(selected) = self.queries.get(id) {
            return Ok(selected.clone());
        }
        let before = Instant::now();
        let maximum = if matches!(self.context, Context::Borrowed(..)) {
            self.query_maximum.saturating_sub(self.selected_ids)
        } else {
            self.query_maximum
        };
        let selected = self.context.select(
            id,
            self.query_budget.as_millis().max(1) as u64,
            maximum,
            if self.package_policy {
                None
            } else {
                self.policy_id
                    .as_deref()
                    .map(|id| crate::access::HttpSelectionPolicy {
                        id,
                        action: if self.operation_id.is_some() {
                            "processing:request"
                        } else {
                            "metadata:read"
                        },
                    })
            },
            &mut self.http_checkpoint,
        );
        if let Some(checkpoint) = &self.http_checkpoint {
            self.scoped_graphql_evidence = checkpoint.scoped_materialized();
        }
        self.query_budget = self.query_budget.saturating_sub(before.elapsed());
        if self.query_budget.is_zero() {
            return Err(Error("query composition timed out".into()));
        }
        let selected = selected?;
        if matches!(self.context, Context::Borrowed(..)) {
            self.selected_ids += selected.ids.len();
            if self.selected_ids > self.query_maximum {
                return Err(Error(
                    "complete query selection exceeds its ID budget".into(),
                ));
            }
        }
        self.queries.insert(id.into(), selected.clone());
        Ok(selected)
    }
    pub fn account_value(&mut self, value: &Value) -> Result<()> {
        if let Some(checkpoint) = &mut self.http_checkpoint {
            checkpoint.account_value(value)
        } else {
            self.context.account_value(value)
        }
    }
    /// Bounded owner SQL shares the resolver's elapsed query and evidence budget.
    pub fn rows(&mut self, sql: &str, parameters: &[rusqlite::types::Value]) -> Result<Vec<Value>> {
        self.rows_with_code(sql, parameters, "evaluation_expansion_budget")
    }
    fn rows_with_code(
        &mut self,
        sql: &str,
        parameters: &[rusqlite::types::Value],
        budget_error: &str,
    ) -> Result<Vec<Value>> {
        if self.query_budget.is_zero() {
            return Err(Error("query composition timed out".into()));
        }
        let before = Instant::now();
        let deadline = before + self.query_budget;
        self.store()
            .db
            .progress_handler(1000, Some(move || Instant::now() >= deadline))?;
        let mut bounded = parameters.to_vec();
        bounded.push(((self.query_maximum + 1) as i64).into());
        let result = crate::migration::rows(
            &self.store().db,
            &format!("SELECT * FROM ({sql}) LIMIT ?"),
            &bounded,
        );
        self.store().db.progress_handler(0, None::<fn() -> bool>)?;
        self.query_budget = self.query_budget.saturating_sub(before.elapsed());
        let rows = result.map_err(|e| {
            if e.0.contains("interrupted") {
                Error("evaluation_expansion_cancelled_or_timed_out".into())
            } else {
                e
            }
        })?;
        if rows.len() > self.query_maximum {
            return Err(Error(budget_error.into()));
        }
        self.account_value(&json!(rows))?;
        Ok(rows)
    }
    /// Frozen HTTP GraphQL package evaluation masks source path identity for
    /// non-metadata operators. A clean raw Store must not broaden that scoped
    /// evidence into an unscoped occurrence capture after the query completes.
    pub fn require_component_context(&self) -> Result<()> {
        if self.scoped_graphql_evidence
            && let Context::Http(access) = &self.context
            && !access.operator("metadata:read")
        {
            return Err(Error("stale_or_conflicting_component_occurrence".into()));
        }
        Ok(())
    }
    pub fn check(&mut self, snapshots: &Value, location: &str) -> Result<bool> {
        let key = encode(snapshots);
        if let Some(usable) = self.checks.get(&key) {
            return Ok(*usable);
        }
        if self.checked >= self.check_limit {
            return Err(Error("resolution_probe_budget".into()));
        }
        if self.suppressed.contains(location) {
            return Ok(false);
        }
        self.checked += 1;
        let path = self.store().path.clone();
        let database = self.store().database_id.clone();
        let profile = self.profile().to_owned();
        let timeout = self.timeout_ms;
        let probe = &mut self.probe;
        let result = self
            .context
            .detached(|| probe(&path, &profile, &database, snapshots, timeout))?;
        // Only actual detachment retires SQLite TEMP scope. Cached probe
        // answers preserve it, as do cached query leaves in the checkpoint.
        if let Some(checkpoint) = &mut self.http_checkpoint {
            checkpoint.after_detach();
        }
        self.scoped_graphql_evidence = false;
        require_epoch(self.store(), self.captured_epoch)?;
        let usable = result["usable"] == true;
        self.checks.insert(key, usable);
        if matches!(
            result["reason"].as_str(),
            Some("probe_timeout" | "probe_capacity")
        ) {
            self.suppressed.insert(location.into());
        }
        Ok(usable)
    }
    pub fn capture(&mut self, row: &Value, policy: &Value, catalog: &str) -> Result<Option<Value>> {
        self.capture_with_checks(row, policy, catalog, true)
    }
    pub fn capture_with_checks(
        &mut self,
        row: &Value,
        policy: &Value,
        catalog: &str,
        check_live: bool,
    ) -> Result<Option<Value>> {
        self.admit_binding_read(text(row, "id")?)?;
        self.package_policy = !policy["package"].is_null();
        self.query_budget = self.query_budget.min(Duration::from_millis(
            policy["budgets"]["query_timeout_ms"].as_u64().unwrap(),
        ));
        self.query_maximum = self
            .query_maximum
            .min(policy["budgets"]["max_candidates"].as_u64().unwrap() as usize);
        let file = text(row, "file_id")?;
        let revision = crate::content_access::revision_of(self.store(), self.profile(), file)?;
        if let Context::Http(access) = &mut self.context {
            if let Some(operation) = &self.operation_id {
                if !access.processing(&json!({"item_id":row["item_id"],"source_file_id":file,"source_revision":revision,"operation_id":operation}))?{return Ok(None);}
            } else {
                crate::content_access::authorize(access, file, &revision)?;
            }
        }
        let (ranking, rank) =
            crate::layout::rank(self.store(), self.profile(), file, &policy["within_tier"])?;
        if ranking["required_fields_failed"]
            .as_array()
            .is_some_and(|v| !v.is_empty())
        {
            return Ok(None);
        }
        let metadata = catabolic_core::decode_json(text(row, "metadata")?)?;
        if crate::exports::truth(&metadata["for_file_id"])
            && metadata["for_file_id"] != json!(self.primary_choices.get(text(row, "item_id")?))
        {
            return Ok(None);
        }
        let outputs = crate::migration::rows(
            &self.store().db,
            &format!(
                "SELECT * FROM ({}) WHERE profile=? AND file_id=?",
                crate::query::view_sql("catalog_renditions")
            ),
            &[self.profile().to_owned().into(), file.to_owned().into()],
        )?;
        if row["active"] == 0 && outputs.is_empty() {
            return Ok(None);
        }
        let mut checks = vec![crate::components::occurrence(
            &self.store().db,
            self.profile(),
            file,
        )?];
        let mut evidence = json!({"semantic_relationship":"curated_association","byte_relationship":"not_established","source_currentness":"not_applicable"});
        let mut rendition = Value::Null;
        if !outputs.is_empty() {
            if outputs.len() != 1 {
                return Err(Error("ambiguous_output_lineage".into()));
            }
            let output = &outputs[0];
            if output["item_id"] != row["item_id"]
                || row["role"] == "primary"
                    && !matches!(output["purpose"].as_str(), Some("transcode" | "remux"))
            {
                return Ok(None);
            }
            if self.store().db.query_row("SELECT EXISTS(SELECT 1 FROM rendition_decisions WHERE profile=? AND catalog=? AND output_id=? AND excluded=1)",params![self.profile(),catalog,output["id"].as_str()],|r|r.get::<_,bool>(0))?{return Ok(None);}
            let (captured, mut lineage) = crate::publication::fallback_evidence(
                self.store(),
                self.profile(),
                output,
                text(policy, "lineage_requirement")?,
                &[],
            )?;
            checks = captured;
            if self.context.http() {
                lineage
                    .as_object_mut()
                    .unwrap()
                    .remove("accepted_source_revision");
            }
            evidence["semantic_relationship"] = json!("accepted_derivative");
            for (key, value) in lineage.as_object().unwrap() {
                evidence[key] = value.clone();
            }
            evidence["byte_identity"] = json!({"algorithm":"sha256","digest":lineage["output_digest"],"size":checks[0]["size"].to_string()});
            rendition = output["id"].clone();
        } else {
            for operation in ["verify", "hash"] {
                let fact = crate::layout::fact(self.store(), self.profile(), file, operation)?;
                if fact["current"] == true
                    && fact["data"]["digest"]
                        .as_str()
                        .is_some_and(|v| !v.is_empty())
                    && fact["snapshot"].get("ctime_ns").is_some()
                {
                    checks[0]["ctime_ns"] = fact["snapshot"]["ctime_ns"].clone();
                    evidence["byte_identity"] = json!({"algorithm":"sha256","digest":fact["data"]["digest"],"size":checks[0]["size"].to_string()});
                    break;
                }
            }
        }
        let rank_value = Value::Array(
            rank.iter()
                .enumerate()
                .map(|(i, (missing, score))| {
                    if *missing != 0 || policy["within_tier"]["prefer"][i].get("values").is_some() {
                        json!([missing, *score as i64])
                    } else {
                        json!([missing, score])
                    }
                })
                .collect(),
        );
        let mut captured = row.clone();
        for (key, value) in [
            ("metadata", metadata),
            ("revision", json!(revision)),
            ("rank", rank_value),
            ("checks", json!(checks)),
            ("evidence", evidence),
            ("rendition_id", rendition),
        ] {
            captured[key] = value;
        }
        if !policy["package"].is_null() {
            return crate::component_selection::capture(self, captured, policy, check_live)
                .map_err(|error| {
                    // SQLite scope-installation failures escape the reference
                    // CatabolicError/PackageBlocked boundary to HTTP500.
                    if error.0 == "authorized_catalog_reinitialized" {
                        error
                    } else {
                        Error(format!("component_package_blocked:{}", error.0))
                    }
                });
        }
        Ok(Some(captured))
    }
    pub fn health(&self, entry: &str, candidate: &Value, catalog: &str) -> Result<Value> {
        let now = crate::notifications::now();
        let rows = crate::migration::rows(
            &self.store().db,
            "SELECT * FROM fallback_health WHERE profile=? AND catalog=? AND entry_id=? AND file_id=? AND revision=?",
            &[
                self.profile().to_owned().into(),
                catalog.to_owned().into(),
                entry.to_owned().into(),
                text(candidate, "file_id")?.to_owned().into(),
                text(candidate, "revision")?.to_owned().into(),
            ],
        )?;
        let old = rows.first();
        Ok(
            json!({"profile":self.profile(),"catalog":catalog,"entry_id":entry,"file_id":candidate["file_id"],"revision":candidate["revision"],"healthy_since":old.map_or(json!(now),|r|r["healthy_since"].clone()),"successes":old.and_then(|r|r["successes"].as_i64()).unwrap_or(0)+1,"last_check":now}),
        )
    }
    pub fn resolve(
        &mut self,
        entries: &[Value],
        policy_id: &str,
        catalog: &str,
        previous: &BTreeMap<String, Value>,
        manual_failback: bool,
    ) -> Result<Value> {
        if !self.store().writable || !self.store().db.is_autocommit() {
            return Err(Error(
                "resolution requires a short writable session outside a transaction".into(),
            ));
        }
        self.policy_id = Some(policy_id.to_owned());
        let policy =
            crate::fallback_mutation::get(self.store(), self.profile(), policy_id)?["definition"]
                .clone();
        self.package_policy = !policy["package"].is_null();
        let limits = &policy["budgets"];
        let maximum = limits["max_candidates"].as_u64().unwrap() as usize;
        self.query_budget = self.query_budget.min(Duration::from_millis(
            limits["query_timeout_ms"].as_u64().unwrap(),
        ));
        self.query_maximum = self.query_maximum.min(maximum);
        if entries.is_empty() || entries.len() > limits["max_entries"].as_u64().unwrap() as usize {
            return Err(Error("resolution_entry_budget".into()));
        }
        self.check_limit =
            (limits["max_checks"].as_u64().unwrap() as usize).saturating_sub(self.total_checks);
        self.checked = 0;
        self.checks.clear();
        self.suppressed.clear();
        self.timeout_ms = limits["probe_timeout_ms"].as_u64().unwrap();
        self.captured_epoch = epoch(self.store())?;
        let mut decisions = Vec::new();
        for value in entries {
            let entry = logical_entry(value)?;
            let id = entry_id(self.store(), self.profile(), catalog, &entry)?;
            let mut decision = entry;
            for (key, value) in [
                ("entry_id", json!(id)),
                ("policy_id", json!(policy_id)),
                ("state", json!("unresolved")),
                ("selected_tier", Value::Null),
                ("selected_tier_name", Value::Null),
                ("file_id", Value::Null),
                ("revision", Value::Null),
                ("rendition_id", Value::Null),
                ("content_path", Value::Null),
                ("evidence", json!({})),
                ("reasons", json!([])),
            ] {
                decision[key] = value;
            }
            if let Some(old) = decisions
                .iter_mut()
                .find(|d: &&mut Value| d["entry_id"] == decision["entry_id"])
            {
                *old = decision;
            } else {
                decisions.push(decision);
            }
        }
        let tier_defs = policy["fallbacks"].as_array().unwrap();
        let mut tiers = tier_defs
            .iter()
            .map(|t| json!({"name":t["name"],"query_id":t["query_id"],"state":"not_evaluated"}))
            .collect::<Vec<_>>();
        let mut captures = BTreeMap::<String, Value>::new();
        let mut health = Vec::new();
        let mut candidate_count = 0;
        for (index, tier) in tier_defs.iter().enumerate() {
            let pending = decisions
                .iter()
                .enumerate()
                .filter(|(_, d)| d["state"] == "unresolved")
                .map(|(i, _)| i)
                .collect::<Vec<_>>();
            if pending.is_empty() {
                break;
            }
            let loaded = (|| -> Result<Vec<Value>> {
                let selected = self.query(text(tier, "query_id")?)?;
                if selected.report["complete"] != true {
                    return Err(Error("incomplete_candidate_query".into()));
                }
                tiers[index] = json!({"name":tier["name"],"query_id":tier["query_id"],"state":"complete","selected_ids":selected.ids.len()});
                let mut entity = selected.entity.clone();
                let mut identifiers = selected.ids.clone();
                let mut component_selection = BTreeMap::<String, Vec<Value>>::new();
                if matches!(entity.as_str(), "component_id" | "occurrence_id") {
                    if policy["package"].is_null() {
                        return Err(Error(
                            "component candidates require a package policy".into(),
                        ));
                    }
                    let components = self.rows(
                        &format!(
                            "SELECT occurrence_id,association_id FROM ({}) WHERE profile=? AND kind='video' AND current=1 AND {entity} IN (SELECT value FROM json_each(?)) LIMIT ?",
                            crate::query::view_sql("catalog_component_occurrences")
                        ),
                        &[
                            self.profile().to_owned().into(),
                            encode(&json!(identifiers)).into(),
                            ((maximum + 1) as i64).into(),
                        ],
                    )?;
                    if components.len() > maximum {
                        return Err(Error("resolution_candidate_budget".into()));
                    }
                    for row in components {
                        component_selection
                            .entry(text(&row, "association_id")?.into())
                            .or_default()
                            .push(row["occurrence_id"].clone());
                    }
                    identifiers = component_selection.keys().cloned().collect();
                    entity = "association_id".into();
                }
                let column = match entity.as_str() {
                    "file_id" => "a.file_id",
                    "association_id" => "a.id",
                    "item_id" => "a.item_id",
                    _ => return Err(Error("invalid candidate selection entity".into())),
                };
                let items = pending
                    .iter()
                    .map(|i| text(&decisions[*i], "item_id").map(str::to_owned))
                    .collect::<Result<BTreeSet<_>>>()?;
                let rows = self.rows_with_code(
                    &format!(
                        "SELECT a.*,f.path,f.location FROM item_files a JOIN files f ON f.id=a.file_id WHERE (a.active=1 OR EXISTS (SELECT 1 FROM media_outputs mo JOIN output_definitions od ON od.id=mo.definition_id WHERE mo.file_id=a.file_id AND mo.item_id=a.item_id AND mo.profile=? AND a.role=json_extract(od.definition,'$.role'))) AND a.item_id IN (SELECT value FROM json_each(?)) AND {column} IN (SELECT value FROM json_each(?)) ORDER BY a.id LIMIT ?"
                    ),
                    &[
                        self.profile().to_owned().into(),
                        encode(&json!(items)).into(),
                        encode(&json!(identifiers)).into(),
                        ((maximum + 1) as i64).into(),
                    ],
                    "resolution_candidate_budget",
                )?;
                if rows.len() > maximum {
                    return Err(Error("resolution_candidate_budget".into()));
                }
                let rows = if !component_selection.is_empty() {
                    let mut expanded = Vec::new();
                    for row in rows {
                        for occurrence in &component_selection[text(&row, "id")?] {
                            let mut row = row.clone();
                            row["video_occurrence_id"] = occurrence.clone();
                            expanded.push(row);
                        }
                    }
                    expanded
                } else {
                    rows
                };
                candidate_count += rows.len();
                if candidate_count > maximum {
                    return Err(Error("resolution_candidate_budget".into()));
                }
                Ok(rows)
            })();
            let rows = match loaded {
                Ok(rows) => rows,
                Err(error) if error.0 == "authorized_catalog_reinitialized" => return Err(error),
                Err(error) => {
                    tiers[index]["state"] = json!("blocked");
                    tiers[index]["reason"] = json!(if self.context.http() {
                        "candidate_query_failed"
                    } else {
                        &error.0
                    });
                    for i in pending {
                        decisions[i]["state"] = json!("blocked");
                        decisions[i]["reasons"] = json!(["candidate_query_failed"]);
                    }
                    break;
                }
            };
            for decision_index in pending {
                let decision = &mut decisions[decision_index];
                let mut candidates = BTreeMap::<String, Value>::new();
                for row in &rows {
                    let metadata = catabolic_core::decode_json(text(row, "metadata")?)?;
                    if row["item_id"] != decision["item_id"]
                        || row["role"] != decision["role"]
                        || row["part"] != decision["part"]
                        || metadata.get("variant").unwrap_or(&json!("")) != &decision["variant"]
                    {
                        continue;
                    }
                    let candidate_key =
                        encode(&json!([row["file_id"], row["video_occurrence_id"]]));
                    if candidates.contains_key(&candidate_key) {
                        continue;
                    }
                    match self.capture(row, &policy, catalog) {
                        Ok(Some(c)) => {
                            candidates.insert(candidate_key, c);
                        }
                        Ok(None) => {}
                        Err(error) if error.0 == "authorized_catalog_reinitialized" => {
                            return Err(error);
                        }
                        Err(error) if error.0.starts_with("component_package_blocked:") => {
                            decision["state"] = json!("blocked");
                            decision["reasons"] = json!([if self.context.http() {
                                "component_package_blocked"
                            } else {
                                error.0.trim_start_matches("component_package_blocked:")
                            }]);
                            break;
                        }
                        Err(_) => {
                            decision["reasons"]
                                .as_array_mut()
                                .unwrap()
                                .push(json!("candidate_evidence_unavailable"));
                        }
                    }
                }
                if decision["state"] == "blocked" {
                    continue;
                }
                let mut ranked = candidates.into_values().collect::<Vec<_>>();
                ranked.sort_by(|a, b| {
                    rank_cmp(&rank_of(a), &rank_of(b))
                        .then_with(|| a["file_id"].as_str().cmp(&b["file_id"].as_str()))
                });
                let mut observations = ranked
                    .iter()
                    .map(|v| Candidate {
                        file_id: text(v, "file_id").unwrap().into(),
                        rank: rank_of(v),
                        observation: Observation::Unchecked,
                    })
                    .collect::<Vec<_>>();
                let chosen = loop {
                    match catabolic_core::resolution::tier(
                        &observations,
                        policy["within_tier"]["tie_break"] == "file_id",
                        !policy["package"].is_null(),
                    ) {
                        TierEffect::Probe(i) => {
                            match self.check(&ranked[i]["checks"], text(&ranked[i], "location")?) {
                                Ok(true) => observations[i].observation = Observation::Usable,
                                Ok(false) => observations[i].observation = Observation::Unavailable,
                                Err(error) if error.0 == "resolution_probe_budget" => {
                                    decision["state"] = json!("blocked");
                                    decision["reasons"] = json!([error.0]);
                                    break None;
                                }
                                Err(error) => return Err(error),
                            }
                        }
                        TierEffect::Select(i) => break Some(ranked[i].clone()),
                        TierEffect::Ambiguous => {
                            decision["state"] = json!("blocked");
                            decision["reasons"] = json!(["ambiguous_tier"]);
                            break None;
                        }
                        TierEffect::Unavailable => break None,
                    }
                };
                let Some(mut chosen) = chosen else {
                    continue;
                };
                let mut selected_index = index;
                if let Some(prior) = previous.get(text(decision, "entry_id")?)
                    && prior["file_id"] != chosen["file_id"]
                    && prior["tier"]
                        .as_u64()
                        .is_some_and(|tier| (index as u64) < tier)
                {
                    let old_index = prior["tier"].as_u64().unwrap() as usize;
                    if old_index < tier_defs.len()
                        && policy["failback"]["mode"] != "immediate"
                        && !manual_failback
                    {
                        let mut selected = self.query(text(&tier_defs[old_index], "query_id")?)?;
                        let prior_evidence = decode_evidence(&prior["evidence"])?;
                        let package = &prior_evidence["component_package"];
                        let old_file = package["video"].get("file_id").unwrap_or(&prior["file_id"]);
                        let old_video = &package["video"]["occurrence_id"];
                        if matches!(selected.entity.as_str(), "component_id" | "occurrence_id") {
                            let matches=self.rows(&format!("SELECT association_id FROM ({}) WHERE profile=? AND occurrence_id=? AND {} IN (SELECT value FROM json_each(?)) AND current=1",crate::query::view_sql("catalog_component_occurrences"),selected.entity),&[self.profile().to_owned().into(),crate::sql::scalar(old_video)?,encode(&json!(selected.ids)).into()])?;
                            selected.entity = "association_id".into();
                            selected.ids = matches
                                .iter()
                                .map(|r| text(r, "association_id").map(str::to_owned))
                                .collect::<Result<_>>()?;
                        }
                        if selected.report["complete"] != true {
                            return Err(Error("incomplete_candidate_query".into()));
                        }
                        tiers[old_index] = json!({"name":tier_defs[old_index]["name"],"query_id":tier_defs[old_index]["query_id"],"state":"complete","selected_ids":selected.ids.len()});
                        let old_rows = self.rows(
                            "SELECT a.*,f.path,f.location FROM item_files a JOIN files f ON f.id=a.file_id WHERE a.file_id=? AND a.item_id=? AND a.role=? AND a.part IS ?",
                            &[
                                crate::sql::scalar(old_file)?,
                                crate::sql::scalar(&decision["item_id"])?,
                                crate::sql::scalar(&decision["role"])?,
                                crate::sql::scalar(&decision["part"])?,
                            ],
                        )?;
                        let key = match selected.entity.as_str() {
                            "file_id" => "file_id",
                            "association_id" => "id",
                            "item_id" => "item_id",
                            _ => "",
                        };
                        let old = old_rows.into_iter().find(|row| {
                            row[key]
                                .as_str()
                                .is_some_and(|id| selected.ids.contains(id))
                                && catabolic_core::decode_json(row["metadata"].as_str().unwrap())
                                    .ok()
                                    .is_some_and(|v| {
                                        v.get("variant").unwrap_or(&json!(""))
                                            == &decision["variant"]
                                    })
                        });
                        let capture = if let Some(mut old) = old {
                            if !old_video.is_null() {
                                old["video_occurrence_id"] = old_video.clone();
                            }
                            match self.capture(&old, &policy, catalog) {
                                Ok(capture) => capture,
                                Err(error) if error.0 == "authorized_catalog_reinitialized" => {
                                    return Err(error);
                                }
                                Err(error) if error.0.starts_with("component_package_blocked:") => {
                                    decision["state"] = json!("blocked");
                                    decision["reasons"] = json!([error
                                        .0
                                        .trim_start_matches("component_package_blocked:")]);
                                    continue;
                                }
                                Err(_) => None,
                            }
                        } else {
                            None
                        };
                        let live = if let Some(capture) = &capture {
                            match self.check(&capture["checks"], text(capture, "location")?) {
                                Ok(live) => live,
                                Err(error) if error.0 == "resolution_probe_budget" => {
                                    decision["state"] = json!("blocked");
                                    decision["reasons"] = json!([error.0]);
                                    continue;
                                }
                                Err(error) => return Err(error),
                            }
                        } else {
                            false
                        };
                        if let Some(capture) = capture.filter(|_| live) {
                            let observation =
                                self.health(text(decision, "entry_id")?, &chosen, catalog)?;
                            let config = &policy["failback"];
                            let effect = catabolic_core::resolution::failback(Failback {
                                mode: if config["mode"] == "manual" {
                                    FailbackMode::Manual
                                } else {
                                    FailbackMode::Stable
                                },
                                manual_override: manual_failback,
                                improving_tier: true,
                                previous_eligible_and_live: true,
                                successful_checks: observation["successes"].as_u64().unwrap(),
                                minimum_successful_checks: config["minimum_successful_checks"]
                                    .as_u64()
                                    .unwrap(),
                                healthy_since: observation["healthy_since"].as_f64().unwrap(),
                                now: crate::notifications::now(),
                                minimum_healthy_seconds: config["minimum_healthy_seconds"]
                                    .as_u64()
                                    .unwrap(),
                            });
                            health.push(observation);
                            if effect == FailbackEffect::RetainPrevious {
                                chosen = capture;
                                selected_index = old_index;
                            }
                        }
                    }
                }
                if let Some(prior) = previous.get(text(decision, "entry_id")?)
                    && prior["file_id"] != chosen["file_id"]
                {
                    let previous_evidence = decode_evidence(&prior["evidence"])?;
                    if !previous_evidence["byte_identity"].is_null()
                        && !chosen["evidence"]["byte_identity"].is_null()
                    {
                        chosen["evidence"]["byte_relationship"] =
                            json!(if previous_evidence["byte_identity"]
                                == chosen["evidence"]["byte_identity"]
                            {
                                "recorded_matching_digest_and_length"
                            } else {
                                "known_different_content"
                            });
                        chosen["evidence"]["compared_revision"] = prior["revision"].clone();
                    }
                }
                decision["state"] = json!(if selected_index == 0 {
                    "resolved_preferred"
                } else {
                    "resolved_fallback"
                });
                decision["selected_tier"] = json!(selected_index);
                decision["selected_tier_name"] = tier_defs[selected_index]["name"].clone();
                for key in ["file_id", "revision", "rendition_id", "evidence"] {
                    decision[key] = chosen[key].clone();
                }
                decision["content_path"] = json!(format!(
                    "/v1/files/{}/content?revision={}",
                    text(&chosen, "file_id")?,
                    text(&chosen, "revision")?
                ));
                if chosen["evidence"]["component_package"]["packaging"]["ready"] == false {
                    decision["content_path"] = Value::Null;
                }
                captures.insert(text(decision, "entry_id")?.into(), chosen);
            }
        }
        for decision in &mut decisions {
            if decision["state"] == "unresolved" {
                decision["reasons"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!("no_usable_candidate"));
            }
        }
        let multipart = entries
            .iter()
            .filter(|v| !v["part"].is_null())
            .map(|v| text(v, "item_id").map(str::to_owned))
            .collect::<Result<BTreeSet<_>>>()?;
        for item in multipart {
            let group = decisions
                .iter()
                .filter(|d| d["item_id"] == item)
                .collect::<Vec<_>>();
            let groups = group
                .iter()
                .filter_map(|d| captures.get(d["entry_id"].as_str().unwrap()))
                .map(|c| encode(&c["metadata"]["representation_group"]))
                .collect::<BTreeSet<_>>();
            if groups.len() != 1
                || groups.contains("null")
                || group
                    .iter()
                    .any(|d| !d["state"].as_str().unwrap().starts_with("resolved"))
            {
                for d in decisions.iter_mut().filter(|d| d["item_id"] == item) {
                    d["state"] = json!("blocked");
                    d["file_id"] = Value::Null;
                    d["revision"] = Value::Null;
                    d["content_path"] = Value::Null;
                    d["reasons"] = json!(["multipart_coverage_ambiguous"]);
                }
            }
        }
        if !policy["package"].is_null() {
            for decision in &mut decisions {
                if !decision["state"].as_str().unwrap().starts_with("resolved") {
                    if decision["evidence"]["component_package"].is_null() {
                        decision["evidence"]["component_package"] =
                            json!({"version":1,"components":[],"dependencies":[]});
                    }
                    decision["evidence"]["component_package"]["packaging"] = json!({"action":"block","ready":false,"reason":decision["reasons"].as_array().and_then(|r|r.last()).cloned().unwrap_or(json!("no_coherent_component_package")),"exposes_unselected_components":false});
                }
            }
        }
        require_epoch(self.store(), self.captured_epoch)?;
        self.total_checks += self.checked;
        let mut result = json!({"policy_id":policy_id,"epoch":self.captured_epoch,"decisions":decisions,"tiers":tiers,"checks":self.checked,"health":health,"captures":captures});
        let mut signed = result.clone();
        signed.as_object_mut().unwrap().remove("health");
        result["plan_id"] = json!(sha256(encode(&signed)));
        Ok(result)
    }
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| Error(format!("invalid fallback {key}")))
}
fn rank_of(value: &Value) -> Vec<(u8, f64)> {
    value["rank"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| (v[0].as_u64().unwrap() as u8, v[1].as_f64().unwrap()))
        .collect()
}
pub fn decode_evidence(value: &Value) -> Result<Value> {
    if let Some(raw) = value.as_str() {
        Ok(catabolic_core::decode_json(raw)?)
    } else if value.is_object() {
        Ok(value.clone())
    } else {
        Ok(json!({}))
    }
}
pub fn logical_entry(value: &Value) -> Result<Value> {
    let object = value
        .as_object()
        .ok_or_else(|| Error("invalid logical entry".into()))?;
    if object
        .keys()
        .any(|k| !["item_id", "role", "part", "variant"].contains(&k.as_str()))
        || !value["item_id"].is_string()
        || value.get("role").is_some_and(|v| !v.is_string())
        || value.get("variant").is_some_and(|v| !v.is_string())
        || value
            .get("part")
            .is_some_and(|v| !v.is_null() && !v.is_i64() && !v.is_u64())
    {
        return Err(Error("invalid logical entry".into()));
    }
    let mut entry = json!({"item_id":value["item_id"],"role":"primary","part":null,"variant":""});
    for (key, value) in object {
        entry[key] = value.clone();
    }
    Ok(entry)
}
pub fn entry_id(store: &Store, profile: &str, catalog: &str, entry: &Value) -> Result<String> {
    Ok(uuid::Uuid::new_v5(
        &uuid::Uuid::parse_str(&store.database_id).map_err(|e| Error(e.to_string()))?,
        encode(&json!(["fallback", profile, catalog, entry])).as_bytes(),
    )
    .to_string())
}

pub fn require_policy(access: &Access, id: &str, action: &str) -> Result<Value> {
    if !access.matching(action).iter().any(|g| {
        g["operator"] == true
            || g["fallback_policy_ids"]
                .as_array()
                .is_some_and(|v| v.iter().any(|v| v == id))
    }) {
        return Err(Error("not_found".into()));
    }
    crate::fallback_mutation::get(access.store(), access.profile(), id)
        .map_err(|_| Error("not_found".into()))
}
pub fn resolve_access(
    access: &mut Access,
    item: &str,
    body: &Value,
    operation: Option<&str>,
    probe: &mut Probe<'_>,
) -> Result<Value> {
    let policy = text(body, "fallback_policy_id")?;
    let action = if operation.is_some() {
        "processing:request"
    } else {
        "metadata:read"
    };
    require_policy(access, policy, action)?;
    if operation.is_none() && !access.item(item, "metadata:read")? {
        return Err(Error("not_found".into()));
    }
    let entry = logical_entry(
        &json!({"item_id":item,"role":body.get("role").cloned().unwrap_or(json!("primary")),"part":body.get("part").cloned().unwrap_or(Value::Null),"variant":body.get("variant").cloned().unwrap_or(json!(""))}),
    )?;
    let catalog = format!("http:{}", access.principal());
    let result = {
        let mut resolver =
            Resolver::new(Context::Http(access), probe, operation.map(str::to_owned));
        resolver.resolve(&[entry], policy, &catalog, &BTreeMap::new(), false)?
    };
    access.refresh_current()?;
    require_policy(access, policy, action)?;
    let mut decision = result["decisions"][0].clone();
    decision["reasons"] = if decision["file_id"].is_null() {
        json!(["no_eligible_content"])
    } else {
        json!([])
    };
    if let Some(package) = decision["evidence"].get_mut("component_package") {
        package
            .as_object_mut()
            .ok_or_else(|| Error("invalid component package".into()))?
            .remove("queries");
        for entry in package["components"]
            .as_array_mut()
            .ok_or_else(|| Error("invalid component package".into()))?
        {
            let occurrence = &mut entry["occurrence"];
            if !access.component(text(occurrence, "occurrence_id")?) {
                return Err(Error("not_found".into()));
            }
            for key in ["snapshot", "technical", "provenance", "asserted"] {
                occurrence.as_object_mut().unwrap().remove(key);
            }
        }
        for dependency in package["dependencies"]
            .as_array_mut()
            .ok_or_else(|| Error("invalid component dependencies".into()))?
        {
            dependency.as_object_mut().unwrap().remove("snapshot");
        }
    }
    Ok(decision)
}

fn rank_cmp(a: &[(u8, f64)], b: &[(u8, f64)]) -> std::cmp::Ordering {
    for ((am, av), (bm, bv)) in a.iter().zip(b) {
        let order = am
            .cmp(bm)
            .then_with(|| av.partial_cmp(bv).unwrap_or(std::cmp::Ordering::Equal));
        if !order.is_eq() {
            return order;
        }
    }
    a.len().cmp(&b.len())
}
