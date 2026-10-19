// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Shared native authorization primitives. Stored-token authentication is only
//! for already-bound durable work; ingress must authenticate the bearer secret.
//! Grants and saved-query membership share the same owned SQLite read snapshot.
use crate::{Error, Result, Store, selection::Evaluation};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub struct Access {
    evaluation: Evaluation,
    principal: String,
    token_id: String,
    profile: String,
    digest: String,
    grants: Vec<Value>,
    members: BTreeMap<String, BTreeSet<String>>,
}
/// Complete scoped selection work that may survive a source-probe wait.
/// Fresh Access, exact authority fingerprint and unchanged catalog epoch are
/// mandatory on resume. The checkpoint contains no Store or connection.
pub(crate) struct HttpSelectionPolicy<'a> {
    pub id: &'a str,
    pub action: &'a str,
}
pub(crate) struct HttpSelectionCheckpoint {
    database_id: String,
    profile: String,
    fingerprint: String,
    digest: String,
    epoch: i64,
    policy: Option<Value>,
    members: BTreeMap<String, BTreeSet<String>>,
    evaluation: crate::selection::EvaluationCheckpoint,
}
impl HttpSelectionCheckpoint {
    pub(crate) fn scoped_materialized(&self) -> bool {
        self.evaluation.materialized()
    }
    pub(crate) fn after_detach(&mut self) {
        self.evaluation.after_detach();
    }
    pub(crate) fn account_value(&mut self, value: &Value) -> Result<()> {
        self.evaluation.account_value(value)
    }
}
/// Non-forgeable admission for an administrator's immutable saved selection.
/// Only complete IDs may cross this boundary, never documents or global rows.
pub(crate) struct SelectionAdmission {
    database_id: String,
    principal: String,
    token_id: String,
    digest: String,
    profile: String,
    grants: Vec<Value>,
    grant: Option<Value>,
    query: Value,
    policy: Option<(String, Value)>,
    epoch: i64,
}
impl SelectionAdmission {
    pub(crate) fn query_id(&self) -> Result<&str> {
        self.query["id"].as_str().ok_or_else(denied)
    }
    pub(crate) fn limits(&self) -> Result<(u64, usize)> {
        let definition = &self.query["definition"];
        let timeout = definition["timeout_ms"].as_u64().unwrap_or(5000);
        let maximum = definition["max_ids"].as_u64().unwrap_or(10000);
        if !(1..=60000).contains(&timeout) || !(1..=100000).contains(&maximum) {
            return Err(Error("invalid saved selection bounds".into()));
        }
        Ok((timeout, maximum as usize))
    }
    pub(crate) fn check(&self, store: &Store) -> Result<()> {
        if store.database_id != self.database_id {
            return Err(denied());
        }
        let (principal, digest, profile, grants) = Access::read_token(store, &self.token_id)?;
        if principal != self.principal
            || digest != self.digest
            || profile != self.profile
            || grants != self.grants
            || self
                .grant
                .as_ref()
                .is_some_and(|grant| !grants.contains(grant))
            || store.db.query_row(
                "SELECT generation FROM fallback_epoch WHERE id=1",
                [],
                |row| row.get::<_, i64>(0),
            )? != self.epoch
            || crate::definitions::query_get(store, &self.profile, self.query_id()?)? != self.query
        {
            return Err(denied());
        }
        if let Some((_, policy)) = &self.policy
            && crate::fallback_mutation::get(
                store,
                &self.profile,
                policy["id"].as_str().ok_or_else(denied)?,
            )? != *policy
        {
            return Err(denied());
        }
        Ok(())
    }
    pub(crate) fn operator(&self, action: &str) -> bool {
        self.grants.iter().any(|grant| {
            grant["operator"] == true
                && grant["actions"].as_array().is_some_and(|actions| {
                    actions.iter().any(|value| value == action || value == "*")
                })
        })
    }
    pub(crate) fn install_catalog(
        &self,
        store: Store,
    ) -> Result<(Store, crate::api_catalog::Authority)> {
        self.check(&store)?;
        let mut access = Access::from_stored_token(store, &self.token_id)?;
        let authority = crate::api_catalog::install(&mut access)?;
        Ok((access.into_store(), authority))
    }
    pub(crate) fn is_policy(&self) -> bool {
        self.policy.is_some()
    }
}
fn denied() -> Error {
    Error("unauthorized".into())
}
impl Access {
    /// Revalidate the exact stored token, its principal, expiry and current grant
    /// revisions. Knowing a token id is not bearer authentication for ingress.
    pub fn from_stored_token(store: Store, token_id: &str) -> Result<Self> {
        let (principal, digest, profile, grants) = Self::read_token(&store, token_id)?;
        Ok(Self {
            evaluation: Evaluation::new(store, &profile, 5000, 10000, true)?,
            principal,
            token_id: token_id.into(),
            profile,
            digest,
            grants,
            members: BTreeMap::new(),
        })
    }
    fn read_token(store: &Store, token_id: &str) -> Result<(String, String, String, Vec<Value>)> {
        let token=store.db.query_row("SELECT t.principal,t.digest,t.grants,t.expires,t.revoked,p.profile,p.enabled FROM api_tokens t JOIN api_principals p ON p.id=t.principal WHERE t.id=?",[token_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,f64>(3)?,r.get::<_,bool>(4)?,r.get::<_,String>(5)?,r.get::<_,bool>(6)?))).optional()?.ok_or_else(denied)?;
        let (principal, digest, ids, expires, revoked, profile, enabled) = token;
        if revoked || !enabled || !expires.is_finite() || expires <= crate::notifications::now() {
            return Err(denied());
        }
        let ids: Vec<String> = serde_json::from_str(&ids).map_err(|_| denied())?;
        let mut grants = Vec::new();
        for id in ids {
            let grant=store.db.query_row("SELECT definition,revision FROM api_grants WHERE id=? AND principal=? AND enabled=1",params![id,principal],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?))).optional()?;
            if let Some((definition, revision)) = grant {
                let mut definition: Value =
                    catabolic_core::decode_json(&definition).map_err(|_| denied())?;
                if !definition.is_object() || !definition["actions"].is_array() {
                    return Err(denied());
                }
                definition["_id"] = json!(id);
                definition["_revision"] = json!(revision);
                grants.push(definition);
            }
        }
        Ok((principal, digest, profile, grants))
    }
    /// An external probe discards all request membership caches, releases the
    /// writer, then authenticates the exact token on the reopened owner.
    pub fn detached<T>(&mut self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        self.members.clear();
        self.grants.clear();
        let result = self.evaluation.detached(operation);
        self.refresh_current()?;
        result
    }
    /// Revalidate this exact stored authority on the current owner snapshot.
    /// Used after a reopened effect boundary and before final policy output.
    /// A failed reload poisons all cached authorization rather than restoring it.
    pub(crate) fn refresh_current(&mut self) -> Result<()> {
        self.members.clear();
        self.grants.clear();
        let (principal, digest, profile, grants) = Self::read_token(self.store(), &self.token_id)?;
        if principal != self.principal || digest != self.digest || profile != self.profile {
            return Err(denied());
        }
        self.grants = grants;
        Ok(())
    }
    pub fn authenticate(store: Store, secret: &str) -> Result<Self> {
        if secret.is_empty() || secret.len() > 256 {
            return Err(denied());
        }
        let id = secret.split('.').next().ok_or_else(denied)?;
        let access = Self::from_stored_token(store, id)?;
        let actual = catabolic_core::sha256(secret.as_bytes());
        let expected = access.digest.as_bytes();
        // The stored digest is always 64 ASCII bytes. Never early-exit on a
        // mismatching byte of a same-length bearer digest.
        let mut difference = expected.len() ^ actual.len();
        for (left, right) in expected.iter().zip(actual.as_bytes()) {
            difference |= usize::from(left ^ right);
        }
        if difference != 0 {
            return Err(denied());
        }
        Ok(access)
    }
    pub(crate) fn cursor_key(&self) -> &[u8] {
        self.digest.as_bytes()
    }
    pub(crate) fn evaluation_mut(&mut self) -> &mut Evaluation {
        &mut self.evaluation
    }
    pub(crate) fn into_evaluation(self) -> Evaluation {
        self.evaluation
    }
    pub fn principal(&self) -> &str {
        &self.principal
    }
    pub fn token_id(&self) -> &str {
        &self.token_id
    }
    pub fn profile(&self) -> &str {
        &self.profile
    }
    pub fn store(&self) -> &Store {
        self.evaluation.store()
    }
    /// Keep authorization-dependent mutations and callback registration atomic
    /// on this request's exact catalog owner, including nested savepoint rollback.
    pub fn transaction<T>(&mut self, operation: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        let scope = self.store().begin_transaction()?;
        let result = operation(self);
        self.store().finish_transaction(scope, result)
    }
    pub fn into_store(self) -> Store {
        self.evaluation.into_store()
    }
    pub(crate) fn matching(&self, action: &str) -> Vec<Value> {
        self.grants
            .iter()
            .filter(|g| {
                g["actions"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|v| v == action || v == "*"))
            })
            .cloned()
            .collect()
    }
    pub(crate) fn grants(&self) -> &[Value] {
        &self.grants
    }
    pub fn require(&self, action: &str) -> Result<()> {
        if self.matching(action).is_empty() {
            return Err(Error("forbidden".into()));
        }
        Ok(())
    }
    pub fn operator(&self, action: &str) -> bool {
        self.matching(action).iter().any(|g| g["operator"] == true)
    }
    pub fn require_operator(&self, action: &str) -> Result<()> {
        if !self.operator(action) {
            return Err(Error("forbidden".into()));
        }
        Ok(())
    }
    pub(crate) fn scoped_selection(
        mut self,
        id: &str,
        timeout: u64,
        maximum: usize,
        mode: Option<HttpSelectionPolicy<'_>>,
        checkpoint: &mut Option<HttpSelectionCheckpoint>,
    ) -> Result<crate::selection::Selected> {
        let admission = mode
            .map(|policy| self.policy_admission(id, policy))
            .transpose()?;
        let policy = admission
            .as_ref()
            .and_then(|proof| proof.policy.as_ref())
            .map(|(action, value)| json!([action, value]));
        let database_id = self.store().database_id.clone();
        let profile = self.profile.clone();
        let fingerprint = self.fingerprint();
        let digest = self.digest.clone();
        let epoch = self.store().db.query_row(
            "SELECT generation FROM fallback_epoch WHERE id=1",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        if !(1..=60000).contains(&timeout) || !(1..=100000).contains(&maximum) {
            return Err(Error("invalid HTTP selection bounds".into()));
        }
        // Policy limits initialize this independent scoped session; defaults in
        // the authentication owner must not silently cap an approved20k policy.
        self.evaluation = Evaluation::new(
            self.evaluation.into_store(),
            &profile,
            timeout,
            maximum,
            true,
        )?;
        self.evaluation.use_session_limits();
        let mut materialized = false;
        if let Some(previous) = checkpoint.take() {
            if previous.database_id != database_id
                || previous.profile != profile
                || previous.fingerprint != fingerprint
                || previous.digest != digest
                || previous.epoch != epoch
                || previous.policy != policy
            {
                return Err(Error("authorization_snapshot_mismatch".into()));
            }
            materialized = previous.evaluation.materialized();
            self.evaluation.resume_checkpoint(previous.evaluation)?;
            self.members = previous.members;
        }
        self.evaluation.limit_budget(timeout, maximum);
        let scope = if admission.is_none() {
            Some(self.selection_admission(id, None)?)
        } else {
            None
        };
        let members = self.members;
        let mut evaluation = self.evaluation;
        let result = if let Some(proof) = admission {
            evaluation.select_policy(proof)
        } else {
            // Preserve frozen child order. SQL before the first GraphQL uses
            // raw operator authority; GraphQL installs metadata views lazily.
            // Once installed, later SQL still sees the scoped private tables.
            evaluation
                .defer_authority(scope.expect("scoped admission"), materialized)
                .and_then(|()| evaluation.select(&json!({"query_id":id,"profile":profile})))
        };
        *checkpoint = evaluation
            .into_checkpoint()
            .map(|evaluation| HttpSelectionCheckpoint {
                database_id,
                profile,
                fingerprint,
                digest,
                epoch,
                policy,
                members,
                evaluation,
            });
        result
    }
    fn selection_admission(&self, id: &str, grant: Option<Value>) -> Result<SelectionAdmission> {
        let proof = SelectionAdmission {
            database_id: self.store().database_id.clone(),
            principal: self.principal.clone(),
            token_id: self.token_id.clone(),
            digest: self.digest.clone(),
            profile: self.profile.clone(),
            grants: self.grants.clone(),
            grant,
            query: crate::definitions::query_get(self.store(), &self.profile, id)?,
            policy: None,
            epoch: self.store().db.query_row(
                "SELECT generation FROM fallback_epoch WHERE id=1",
                [],
                |row| row.get(0),
            )?,
        };
        proof.check(self.store())?;
        Ok(proof)
    }
    fn policy_admission(
        &self,
        query: &str,
        mode: HttpSelectionPolicy<'_>,
    ) -> Result<SelectionAdmission> {
        if !["metadata:read", "processing:request"].contains(&mode.action) {
            return Err(denied());
        }
        let policy = crate::fallback_resolution::require_policy(self, mode.id, mode.action)?;
        if !policy["definition"]["package"].is_null()
            || !catabolic_core::fallback::query_dependencies(&policy["definition"]).contains(query)
        {
            return Err(Error("query is not an ordinary policy dependency".into()));
        }
        let mut proof = self.selection_admission(query, None)?;
        proof.policy = Some((mode.action.into(), policy));
        proof.check(self.store())?;
        Ok(proof)
    }
    /// Frozen approved saved selections evaluate globally, then the HTTP owner
    /// intersects complete IDs with scoped views. No arbitrary document enters.
    pub(crate) fn approved_selection(&mut self, id: &str) -> Result<crate::selection::Selected> {
        self.require("metadata:read")?;
        let allowed = self.operator("sql:read")
            || self.operator("metadata:read")
            || self.matching("metadata:read").iter().any(|grant| {
                grant["query_id"] == id
                    || grant["report_ids"]
                        .as_array()
                        .is_some_and(|ids| ids.iter().any(|value| value == id))
            });
        if !allowed {
            return Err(Error("not_found".into()));
        }
        let proof = self.selection_admission(id, None)?;
        self.evaluation.select_admitted(proof)
    }
    pub(crate) fn members(&mut self, grant: &Value) -> Result<&BTreeSet<String>> {
        let id = grant["_id"].as_str().ok_or_else(denied)?;
        if !self.members.contains_key(id) {
            let mut members = BTreeSet::new();
            if let Some(ids) = grant.get("item_ids") {
                for value in ids.as_array().ok_or_else(denied)? {
                    members.insert(value.as_str().ok_or_else(denied)?.to_owned());
                }
            }
            if grant.get("query_id").is_some_and(|v| !v.is_null()) {
                let query_id = grant["query_id"].as_str().ok_or_else(denied)?;
                let proof = self.selection_admission(query_id, Some(grant.clone()))?;
                let selection = self.evaluation.select_admitted(proof)?;
                if selection.entity != "item_id" || selection.report["complete"] != true {
                    return Err(Error("incomplete_authorization_selection".into()));
                }
                members.extend(selection.ids);
            }
            self.members.insert(id.into(), members);
        }
        Ok(self.members.get(id).expect("inserted membership"))
    }
    pub fn processing(&mut self, body: &Value) -> Result<bool> {
        let item = body["item_id"].as_str().ok_or_else(denied)?;
        let operation = body["operation_id"].as_str().ok_or_else(denied)?;
        let source = body["source_file_id"].as_str().ok_or_else(denied)?;
        let revision = body["source_revision"].as_str().ok_or_else(denied)?;
        for grant in self.matching("processing:request") {
            let membership = grant["operator"] == true
                || (self.members(&grant)?.contains(item)
                    && grant["operation_ids"]
                        .as_array()
                        .is_some_and(|v| v.iter().any(|v| v == operation)));
            let pinned = grant.get("revisions").and_then(|v| v.get(source));
            if membership && pinned.is_none_or(|v| v == revision) {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub fn request_visible(&mut self, id: &str) -> Result<Value> {
        let row = self
            .store()
            .db
            .query_row(
                "SELECT state,body FROM api_requests WHERE id=? AND principal=? AND profile=?",
                params![id, self.principal, self.profile],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
            .ok_or_else(|| Error("not_found".into()))?;
        let body: Value = catabolic_core::decode_json(&row.1).map_err(|e| Error(e.to_string()))?;
        if !self.processing(&body)? {
            return Err(Error("not_found".into()));
        }
        // Python request status always resolves the current source revision,
        // except for a cancelled request. No source bytes are read here.
        if row.0 != "cancelled" {
            crate::media_mutation::snapshot(
                self.store(),
                &self.profile,
                body["source_file_id"].as_str().ok_or_else(denied)?,
            )?;
        }
        Ok(json!({"state":row.0,"body":body}))
    }
    pub fn allow_callback(&self, url: &str) -> Result<()> {
        let target = crate::notifications::callback_origin(url)
            .map_err(|_| Error("invalid_callback_url".into()))?;
        if self.operator("webhooks:manage") {
            return Ok(());
        }
        for grant in self.matching("processing:request") {
            if let Some(origins) = grant.get("callback_origins") {
                for allowed in origins.as_array().ok_or_else(denied)? {
                    let allowed =
                        crate::notifications::callback_origin(allowed.as_str().ok_or_else(denied)?)
                            .map_err(|_| denied())?;
                    if allowed == target {
                        return Ok(());
                    }
                }
            }
        }
        Err(Error("callback_origin_not_allowed".into()))
    }
}

#[cfg(test)]
mod transaction_tests {
    use super::*;
    fn fixture(writable: bool) -> (tempfile::TempDir, Access) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        store.transaction(|db|{
            db.execute("INSERT INTO api_principals VALUES ('principal','default',1)",[])?;
            db.execute("INSERT INTO api_tokens VALUES ('token','principal','digest','[]',4102444800,0)",[])?;
            Ok(())
        }).unwrap();
        drop(store);
        let store = Store::open(&path, writable, false).unwrap();
        (root, Access::from_stored_token(store, "token").unwrap())
    }
    #[test]
    fn readonly_detach_refreshes_token_and_keeps_catalog_identity() {
        let (_root, mut access) = fixture(false);
        let path = access.store().path.clone();
        let identity = access.store().database_id.clone();
        access
            .detached(|| {
                let writer = Store::open(&path, true, false)?;
                writer.transaction(|db| {
                    db.execute("INSERT INTO catalogs VALUES ('during-probe')", [])?;
                    Ok(())
                })
            })
            .unwrap();
        assert_eq!(access.store().database_id, identity);
        assert!(!access.store().db.is_autocommit());
        assert_eq!(
            access
                .store()
                .db
                .query_row(
                    "SELECT count(*) FROM catalogs WHERE id='during-probe'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        let error = access
            .detached(|| {
                let writer = Store::open(&path, true, false)?;
                writer.transaction(|db| {
                    db.execute("UPDATE api_tokens SET revoked=1 WHERE id='token'", [])?;
                    Ok(())
                })
            })
            .unwrap_err();
        assert_eq!(error.0, "unauthorized");
        assert!(access.grants.is_empty());
        assert_eq!(access.store().database_id, identity);
        assert!(!access.store().db.is_autocommit());
    }
    #[test]
    fn nested_rollback_uses_the_same_store_savepoints() {
        let (_root, mut access) = fixture(true);
        access
            .transaction(|access| {
                access
                    .store()
                    .db
                    .execute("INSERT INTO catalogs VALUES ('outer')", [])?;
                let result: Result<()> = access.transaction(|inner| {
                    inner.store().transaction(|db| {
                        db.execute("INSERT INTO catalogs VALUES ('inner')", [])?;
                        Ok(())
                    })?;
                    Err(Error("rollback inner".into()))
                });
                assert!(result.is_err());
                assert_eq!(
                    access.store().db.query_row(
                        "SELECT count(*) FROM catalogs WHERE id='inner'",
                        [],
                        |r| r.get::<_, i64>(0)
                    )?,
                    0
                );
                Ok(())
            })
            .unwrap();
        assert_eq!(
            access
                .store()
                .db
                .query_row("SELECT count(*) FROM catalogs WHERE id='outer'", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap(),
            1
        );
        let result: Result<()> = access.store().transaction(|db| {
            db.execute("INSERT INTO catalogs VALUES ('rolled_outer')", [])?;
            access.store().transaction(|db| {
                db.execute("INSERT INTO catalogs VALUES ('rolled_inner')", [])?;
                Ok(())
            })?;
            Err(Error("rollback outer".into()))
        });
        assert!(result.is_err());
        assert_eq!(
            access
                .store()
                .db
                .query_row(
                    "SELECT count(*) FROM catalogs WHERE id LIKE 'rolled_%'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }
    #[test]
    fn failed_commit_rolls_back_both_entry_points() {
        for through_access in [false, true] {
            let (_root, mut access) = fixture(true);
            let write = |store: &Store| -> Result<()> {
                store.db.execute_batch("PRAGMA defer_foreign_keys=ON; INSERT INTO files VALUES ('orphan','missing','path')")?;
                Ok(())
            };
            let result = if through_access {
                access.transaction(|a| write(a.store()))
            } else {
                access.store().transaction(|_| write(access.store()))
            };
            assert!(result.is_err());
            assert!(access.store().db.is_autocommit());
            assert_eq!(
                access
                    .store()
                    .db
                    .query_row("SELECT count(*) FROM files WHERE id='orphan'", [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }
    #[test]
    fn readonly_and_detached_owner_are_rejected_before_callback() {
        let (_root, mut access) = fixture(false);
        assert!(
            access
                .transaction::<()>(|_| panic!("readonly callback executed"))
                .is_err()
        );
        assert!(
            access
                .store()
                .transaction::<()>(|_| panic!("readonly callback executed"))
                .is_err()
        );
        let (_root, access) = fixture(true);
        let mut store = access.into_store();
        // This is the same owner state checked after detach loses/rejects its
        // writer lock, independent of the placeholder connection's authorizer.
        store.lock.take();
        let mut access = Access::from_stored_token(store, "token").unwrap();
        assert!(
            access
                .transaction::<()>(|_| panic!("detached callback executed"))
                .is_err()
        );
        assert!(
            access
                .store()
                .transaction::<()>(|_| panic!("detached callback executed"))
                .is_err()
        );
    }
}

#[cfg(test)]
mod selection_admission_tests {
    use super::*;
    use crate::graphql::tests::{fixture, limited};
    use catabolic_core::encode;
    fn reopened(root: &tempfile::TempDir) -> Access {
        Access::authenticate(
            Store::open(&root.path().join("catalog.sqlite3"), false, false).unwrap(),
            "token.secret",
        )
        .unwrap()
    }
    fn scoped_fixture() -> (tempfile::TempDir, Access) {
        let (root, access) = fixture(limited());
        let mut query = crate::definitions::query_get(
            access.store(),
            access.profile(),
            "selection",
        )
        .unwrap()["definition"]
            .clone();
        query["selection"]["query"] = json!(
            "query($after:String){items(first:100,after:$after){nodes{id} pageInfo{endCursor hasNextPage}}}"
        );
        access
            .store()
            .db
            .execute(
                "UPDATE saved_queries SET definition=? WHERE id='selection'",
                [encode(&query)],
            )
            .unwrap();
        (root, access)
    }
    #[test]
    fn global_admission_cache_cannot_escape_into_scoped_graphql() {
        let mut grant = limited();
        grant["report_ids"] = json!(["selection"]);
        let (_root, mut access) = fixture(grant);
        let selected = access.approved_selection("selection").unwrap();
        assert!(selected.ids.contains("hidden"));
        let authority = crate::api_catalog::install(&mut access).unwrap();
        let mut evaluation = access.into_evaluation().bind_authority(authority).unwrap();
        let selected = evaluation.select(&json!({"query_id":"selection"})).unwrap();
        assert_eq!(selected.ids, BTreeSet::from(["a".into(), "b".into()]));
    }
    #[test]
    fn selection_proof_rejects_changed_query_grant_epoch_and_database() {
        for mutation in [
            "UPDATE saved_queries SET revision=revision+1 WHERE id='selection'",
            "UPDATE api_grants SET revision=revision+1 WHERE id='grant'",
            "UPDATE api_tokens SET revoked=1 WHERE id='token'",
            "UPDATE items SET metadata='{}' WHERE id='a'",
        ] {
            let (_root, mut access) = fixture(limited());
            let proof = access.selection_admission("selection", None).unwrap();
            access.store().db.execute(mutation, []).unwrap();
            assert!(
                access.evaluation.select_admitted(proof).is_err(),
                "{mutation}"
            );
        }
        let (_root, access) = fixture(limited());
        let proof = access.selection_admission("selection", None).unwrap();
        let (_other, mut access) = fixture(limited());
        assert!(access.evaluation.select_admitted(proof).is_err());
    }
    #[test]
    fn failed_and_bound_admission_cannot_enable_arbitrary_http_documents() {
        let (_root, mut access) = fixture(limited());
        let proof = access.selection_admission("document", None).unwrap();
        assert!(access.evaluation.select_admitted(proof).is_err());
        assert_eq!(
            access.evaluation.run("document", 100).unwrap_err().0,
            "GraphQL HTTP evaluation requires authorization context"
        );
        let proof = access.selection_admission("selection", None).unwrap();
        crate::api_catalog::install(&mut access).unwrap();
        assert_eq!(
            access.evaluation.select_admitted(proof).err().unwrap().0,
            "recursive_authorization_selection"
        );
    }
    #[test]
    fn admission_preserves_writer_query_only_after_success_and_failure() {
        for id in ["selection", "document"] {
            let (_root, mut access) = fixture(limited());
            let before: bool = access
                .store()
                .db
                .pragma_query_value(None, "query_only", |row| row.get(0))
                .unwrap();
            let proof = access.selection_admission(id, None).unwrap();
            let result = access.evaluation.select_admitted(proof);
            assert_eq!(result.is_ok(), id == "selection");
            assert_eq!(
                access
                    .store()
                    .db
                    .pragma_query_value(None, "query_only", |row| row.get::<_, bool>(0))
                    .unwrap(),
                before
            );
            access
                .transaction(|access| {
                    access
                        .store()
                        .db
                        .execute("INSERT INTO catalogs VALUES ('after-membership')", [])?;
                    Ok(())
                })
                .unwrap();
        }
    }
    #[test]
    fn scoped_checkpoint_reuses_children_and_retains_spent_budgets() {
        let (root, access) = scoped_fixture();
        for (id, definition) in [
            (
                "combined",
                json!({"mode":"selection","combine":"union","queries":["selection","selection"]}),
            ),
            (
                "another",
                json!({"mode":"selection","selection":{"language":"graphql","query":"query($after:String){items(first:100,after:$after){nodes{id} pageInfo{endCursor hasNextPage}}}"}}),
            ),
        ] {
            access.store().db.execute("INSERT INTO saved_queries(id,profile,name,revision,definition,digest) VALUES (?,'default',?,1,?,?)",params![id,id,encode(&definition),id]).unwrap();
        }
        drop(access);
        let mut checkpoint = None;
        let first = reopened(&root)
            .scoped_selection("selection", 5000, 2, None, &mut checkpoint)
            .unwrap();
        assert_eq!(first.ids.len(), 2);
        let next = reopened(&root)
            .scoped_selection("combined", 5000, 2, None, &mut checkpoint)
            .unwrap();
        assert_eq!(next.ids, first.ids);
        checkpoint.as_mut().unwrap().after_detach();
        let error = reopened(&root)
            .scoped_selection("another", 5000, 2, None, &mut checkpoint)
            .err()
            .unwrap();
        assert_eq!(error.0, "complete query selection exceeds its ID budget");
        assert!(checkpoint.is_some());
    }
    #[test]
    fn policy_proof_pins_dependencies_mode_and_shared_cumulative_limits() {
        let (root, access) = fixture(limited());
        for (id, item) in [("first", "a"), ("second", "b")] {
            let definition = json!({"mode":"selection","max_ids":1,"selection":{"language":"sql","query":format!("SELECT id AS item_id FROM items WHERE id='{item}'")}});
            access.store().db.execute("INSERT INTO saved_queries(id,profile,name,revision,definition,digest) VALUES (?,'default',?,1,?,?)", params![id,id,encode(&definition),id]).unwrap();
        }
        let policy = json!({"fallbacks":[{"name":"first","query_id":"first"},{"name":"second","query_id":"second"}]});
        access
            .store()
            .db
            .execute(
                "UPDATE fallback_policies SET definition=? WHERE id='policy'",
                [encode(&policy)],
            )
            .unwrap();
        drop(access);
        let mut checkpoint = None;
        let mode = || {
            Some(HttpSelectionPolicy {
                id: "policy",
                action: "metadata:read",
            })
        };
        assert_eq!(
            reopened(&root)
                .scoped_selection("first", 5000, 2, mode(), &mut checkpoint)
                .unwrap()
                .ids,
            BTreeSet::from(["a".into()])
        );
        assert_eq!(
            reopened(&root)
                .scoped_selection("second", 5000, 2, mode(), &mut checkpoint)
                .unwrap()
                .ids,
            BTreeSet::from(["b".into()])
        );
        assert!(
            reopened(&root)
                .scoped_selection("selection", 5000, 2, mode(), &mut checkpoint)
                .is_err()
        );
        assert_eq!(
            reopened(&root)
                .scoped_selection("selection", 5000, 2, None, &mut checkpoint)
                .err()
                .unwrap()
                .0,
            "authorization_snapshot_mismatch"
        );
    }
    #[test]
    fn scoped_checkpoint_shares_evidence_bytes_and_rejects_token_replacement() {
        let (root, access) = scoped_fixture();
        drop(access);
        let mut checkpoint = None;
        reopened(&root)
            .scoped_selection("selection", 5000, 10000, None, &mut checkpoint)
            .unwrap();
        checkpoint
            .as_mut()
            .unwrap()
            .account_value(&json!("a".repeat(3 * 1024 * 1024)))
            .unwrap();
        assert_eq!(
            checkpoint
                .as_mut()
                .unwrap()
                .account_value(&json!("b".repeat(2 * 1024 * 1024)))
                .unwrap_err()
                .0,
            "evaluation_result_byte_budget"
        );
        let writer = Store::open(&root.path().join("catalog.sqlite3"), true, false).unwrap();
        writer
            .db
            .execute(
                "UPDATE api_tokens SET digest='replacement' WHERE id='token'",
                [],
            )
            .unwrap();
        drop(writer);
        let current = Access::from_stored_token(
            Store::open(&root.path().join("catalog.sqlite3"), false, false).unwrap(),
            "token",
        )
        .unwrap();
        assert_eq!(
            current
                .scoped_selection("selection", 5000, 10000, None, &mut checkpoint)
                .err()
                .unwrap()
                .0,
            "authorization_snapshot_mismatch"
        );
    }
    #[test]
    fn package_scope_tracks_real_execution_and_only_resets_after_detach() {
        let (root, access) = scoped_fixture();
        let definition = crate::definitions::query_get(
            access.store(),
            access.profile(),
            "selection",
        )
        .unwrap()["definition"]
            .clone();
        access.store().db.execute("INSERT INTO saved_queries(id,profile,name,revision,definition,digest) VALUES ('another','default','another',1,?,'another')",[encode(&definition)]).unwrap();
        drop(access);
        let mut checkpoint = None;
        reopened(&root)
            .scoped_selection("selection", 5000, 10000, None, &mut checkpoint)
            .unwrap();
        assert!(checkpoint.as_ref().unwrap().scoped_materialized());
        assert_eq!(
            reopened(&root)
                .scoped_selection("another", 5000, 10000, None, &mut checkpoint)
                .err()
                .unwrap()
                .0,
            "authorized_catalog_reinitialized"
        );
        assert!(checkpoint.as_ref().unwrap().scoped_materialized());
        checkpoint.as_mut().unwrap().after_detach();
        reopened(&root)
            .scoped_selection("selection", 5000, 10000, None, &mut checkpoint)
            .unwrap();
        assert!(
            !checkpoint.as_ref().unwrap().scoped_materialized(),
            "cached GraphQL does not install a new scope"
        );
        reopened(&root)
            .scoped_selection("another", 5000, 10000, None, &mut checkpoint)
            .unwrap();
        assert!(checkpoint.as_ref().unwrap().scoped_materialized());
    }
    #[test]
    fn invalid_graphql_is_validated_before_lazy_scope_construction() {
        let (root, access) = scoped_fixture();
        let definition = json!({"mode":"selection","selection":{"language":"graphql","query":"query($after:String){items(unknown:true,after:$after){nodes{id} pageInfo{endCursor hasNextPage}}}"}});
        access.store().db.execute("INSERT INTO saved_queries(id,profile,name,revision,definition,digest) VALUES ('invalid','default','invalid',1,?,'invalid')",[encode(&definition)]).unwrap();
        drop(access);
        let mut checkpoint = None;
        let first = reopened(&root)
            .scoped_selection("invalid", 5000, 10000, None, &mut checkpoint)
            .err()
            .unwrap()
            .0;
        assert!(first.starts_with("selection GraphQL failed:"), "{first}");
        assert!(!checkpoint.as_ref().unwrap().scoped_materialized());
        reopened(&root)
            .scoped_selection("selection", 5000, 10000, None, &mut checkpoint)
            .unwrap();
        assert!(checkpoint.as_ref().unwrap().scoped_materialized());
        assert_eq!(
            reopened(&root)
                .scoped_selection("invalid", 5000, 10000, None, &mut checkpoint)
                .err()
                .unwrap()
                .0,
            first
        );
    }
    #[test]
    fn scoped_checkpoint_refuses_changed_grants_and_catalog_epoch() {
        for mutation in [
            "UPDATE api_grants SET revision=revision+1 WHERE id='grant'",
            "UPDATE items SET metadata='{}' WHERE id='a'",
        ] {
            let (root, access) = scoped_fixture();
            drop(access);
            let mut checkpoint = None;
            reopened(&root)
                .scoped_selection("selection", 5000, 10000, None, &mut checkpoint)
                .unwrap();
            let writer = Store::open(&root.path().join("catalog.sqlite3"), true, false).unwrap();
            writer.db.execute(mutation, []).unwrap();
            drop(writer);
            assert_eq!(
                reopened(&root)
                    .scoped_selection("selection", 5000, 10000, None, &mut checkpoint)
                    .err()
                    .unwrap()
                    .0,
                "authorization_snapshot_mismatch"
            );
            assert!(checkpoint.is_none());
        }
    }
}
