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
fn denied() -> Error {
    Error("unauthorized".into())
}
impl Access {
    /// Revalidate the exact stored token, its principal, expiry and current grant
    /// revisions. Knowing a token id is not bearer authentication for ingress.
    pub fn from_stored_token(store: Store, token_id: &str) -> Result<Self> {
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
    pub(crate) fn members(&mut self, grant: &Value) -> Result<&BTreeSet<String>> {
        let id = grant["_id"].as_str().ok_or_else(denied)?;
        if !self.members.contains_key(id) {
            let mut members = BTreeSet::new();
            if let Some(ids) = grant.get("item_ids") {
                for value in ids.as_array().ok_or_else(denied)? {
                    members.insert(value.as_str().ok_or_else(denied)?.to_owned());
                }
            }
            if let Some(query) = grant.get("query_id").filter(|v| !v.is_null()) {
                let selection = self
                    .evaluation
                    .select(&json!({"query_id":query,"profile":self.profile}))?;
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
