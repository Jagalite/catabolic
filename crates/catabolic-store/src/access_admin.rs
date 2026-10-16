// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Local access administration on the caller's existing native catalog owner.
//!
//! These are administrative business owners, not HTTP endpoints. JSON values
//! here preserve the frozen boundary's validation order and SQLite scalar
//! coercion; authorization continues to belong to `access::Access`. No secret
//! file, transport, or external account is created by this module.
use crate::{Error, Result, Store, embedding::Session, http_operations::truthy};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use catabolic_core::{encode, sha256};
use rusqlite::{OptionalExtension, params, types::Value as SqlValue};
use serde_json::{Value, json};

/// Only this module can construct an admission to the frozen administrative
/// selection preflight. It does not admit HTTP callers or authorize resources.
pub(crate) struct GrantPreflight {
    _private: (),
}

/// Administrative business failures are tagged at their decision sites. Native
/// storage/query/lifecycle diagnostics never acquire this tag from their text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessFailure {
    InvalidPrincipal,
    InvalidGrantFields,
    InvalidGrant,
    InvalidRevisions,
    InvalidCallbackUrl,
    InvalidCallbackOrigin,
    GrantRequiresCompleteItemSelection,
    InvalidTokenConfiguration,
    InvalidTokenGrant,
    UnknownPrincipal,
    PrincipalProfileConflict,
    GrantPrincipalConflict,
}
impl AccessFailure {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidPrincipal => "invalid_principal",
            Self::InvalidGrantFields => "invalid_grant_fields",
            Self::InvalidGrant => "invalid_grant",
            Self::InvalidRevisions => "invalid_revisions",
            Self::InvalidCallbackUrl => "invalid_callback_url",
            Self::InvalidCallbackOrigin => "invalid_callback_origin",
            Self::GrantRequiresCompleteItemSelection => "grant_requires_complete_item_selection",
            Self::InvalidTokenConfiguration => "invalid_token_configuration",
            Self::InvalidTokenGrant => "invalid_token_grant",
            Self::UnknownPrincipal => "unknown_principal",
            Self::PrincipalProfileConflict => "principal_profile_conflict",
            Self::GrantPrincipalConflict => "grant_principal_conflict",
        }
    }
    pub fn status(self) -> u16 {
        match self {
            Self::UnknownPrincipal => 404,
            Self::PrincipalProfileConflict | Self::GrantPrincipalConflict => 409,
            _ => 400,
        }
    }
}
#[derive(Debug)]
pub enum Failure {
    Access(AccessFailure),
    Native(Error),
}
pub type DetailedResult<T> = std::result::Result<T, Failure>;
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Access(error) => f.write_str(error.code()),
            Self::Native(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for Failure {}
impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self::Native(error)
    }
}
impl From<rusqlite::Error> for Failure {
    fn from(error: rusqlite::Error) -> Self {
        Self::Native(error.into())
    }
}
impl From<Failure> for Error {
    fn from(error: Failure) -> Self {
        match error {
            Failure::Access(error) => Error(error.code().into()),
            Failure::Native(error) => error,
        }
    }
}
fn fail<T>(error: AccessFailure) -> DetailedResult<T> {
    Err(Failure::Access(error))
}

// The frozen owners leave ordinary SQLite binding coercion to sqlite3. Do not
// introduce a new name/identifier policy where that contract permits scalars.
fn scalar(value: &Value, position: usize) -> Result<SqlValue> {
    Ok(match value {
        Value::Null => SqlValue::Null,
        Value::Bool(value) => SqlValue::Integer(i64::from(*value)),
        Value::String(value) => SqlValue::Text(value.clone()),
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                SqlValue::Integer(value)
            } else if value.is_f64() {
                SqlValue::Real(value.as_f64().ok_or_else(|| {
                    Error("Out of range float values are not JSON compliant".into())
                })?)
            } else {
                return Err(Error(
                    "Python int too large to convert to SQLite INTEGER".into(),
                ));
            }
        }
        Value::Array(_) | Value::Object(_) => {
            return Err(Error(format!(
                "Error binding parameter {position}: type '{}' is not supported",
                if value.is_array() { "list" } else { "dict" }
            )));
        }
    })
}

pub fn principal_put(store: &Store, profile: &Value, identifier: &Value) -> Result<Value> {
    principal_put_detailed(store, profile, identifier).map_err(Into::into)
}
pub fn principal_put_detailed(
    store: &Store,
    profile: &Value,
    identifier: &Value,
) -> DetailedResult<Value> {
    let Some(id) = identifier
        .as_str()
        .filter(|id| (1..=128).contains(&id.chars().count()))
    else {
        return fail(AccessFailure::InvalidPrincipal);
    };
    store.transaction_detailed(|db| -> DetailedResult<()> {
        db.execute(
            "INSERT INTO api_principals(id,profile) VALUES (?,?) ON CONFLICT(id) DO NOTHING",
            params![id, scalar(profile, 2)?],
        )?;
        let actual: String =
            db.query_row("SELECT profile FROM api_principals WHERE id=?", [id], |r| {
                r.get(0)
            })?;
        if profile != &Value::String(actual) {
            return fail(AccessFailure::PrincipalProfileConflict);
        }
        Ok(())
    })?;
    Ok(json!({"principal":identifier,"profile":profile}))
}

fn validate_grant(definition: &Value) -> DetailedResult<()> {
    const LISTS: &[&str] = &[
        "actions",
        "item_ids",
        "file_ids",
        "occurrence_ids",
        "metadata_fields",
        "operation_ids",
        "projection_ids",
        "report_ids",
        "fallback_policy_ids",
        "callback_origins",
    ];
    let Some(fields) = definition.as_object() else {
        return fail(AccessFailure::InvalidGrantFields);
    };
    if fields.keys().any(|key| {
        !LISTS.contains(&key.as_str())
            && !["query_id", "derivatives", "operator", "revisions"].contains(&key.as_str())
    }) {
        return fail(AccessFailure::InvalidGrantFields);
    }
    for key in LISTS {
        if let Some(value) = fields.get(*key) {
            let Some(values) = value.as_array() else {
                return fail(AccessFailure::InvalidGrant);
            };
            if values.len() > 10000
                || values
                    .iter()
                    .any(|v| !v.as_str().is_some_and(|s| s.chars().count() <= 256))
            {
                return fail(AccessFailure::InvalidGrant);
            }
        }
    }
    if !definition.get("actions").is_some_and(truthy)
        || ["operator", "derivatives"]
            .iter()
            .any(|key| fields.get(*key).is_some_and(|v| !v.is_boolean()))
    {
        return fail(AccessFailure::InvalidGrant);
    }
    if let Some(revisions) = fields.get("revisions")
        && !revisions
            .as_object()
            .is_some_and(|r| r.values().all(Value::is_string))
    {
        return fail(AccessFailure::InvalidRevisions);
    }
    if let Some(origins) = fields.get("callback_origins").and_then(Value::as_array) {
        for origin in origins {
            let origin = origin.as_str().expect("validated string list");
            crate::notifications::callback_origin(origin)
                .map_err(|_| Failure::Access(AccessFailure::InvalidCallbackUrl))?;
            // The shared validator has already parsed and checked the authority.
            // Empty '?' and '#' suffixes remain valid, as in urllib.urlsplit.
            let rest = origin.split_once("://").expect("validated scheme").1;
            let rest = rest.split('#').next().unwrap();
            let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
            let path = rest.find('/').map_or("", |at| &rest[at..]);
            if !matches!(path, "" | "/") || !query.is_empty() {
                return fail(AccessFailure::InvalidCallbackOrigin);
            }
        }
    }
    Ok(())
}

pub fn grant_put(
    session: &mut Session,
    principal: &Value,
    definition: &Value,
    identifier: Option<&Value>,
) -> Result<Value> {
    grant_put_detailed(session, principal, definition, identifier).map_err(Into::into)
}
pub fn grant_put_detailed(
    session: &mut Session,
    principal: &Value,
    definition: &Value,
    identifier: Option<&Value>,
) -> DetailedResult<Value> {
    validate_grant(definition)?;
    let profile: String = session
        .store()?
        .db
        .query_row(
            "SELECT profile FROM api_principals WHERE id=?",
            [scalar(principal, 1)?],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(Failure::Access(AccessFailure::UnknownPrincipal))?;
    if let Some(query) = definition.get("query_id").filter(|v| truthy(v)) {
        // Resolve using SQLite's ordinary scalar binding before lending the
        // exact owner to the existing bounded native selection engine.
        let query: String = session
            .store()?
            .db
            .query_row(
                "SELECT id FROM saved_queries WHERE id=? AND profile=?",
                params![scalar(query, 1)?, profile],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| Error("unknown query revision in this profile".into()))?;
        let selected =
            session.select_access_grant(&profile, &query, GrantPreflight { _private: () })?;
        if selected.entity != "item_id" || selected.report["complete"] != true {
            return fail(AccessFailure::GrantRequiresCompleteItemSelection);
        }
    }
    let identifier = identifier
        .filter(|id| truthy(id))
        .cloned()
        .unwrap_or_else(|| json!(uuid::Uuid::new_v4().to_string()));
    session.store()?.transaction_detailed(|db| -> DetailedResult<()> {
        let id = scalar(&identifier, 1)?;
        let old: Option<String> = db.query_row("SELECT principal FROM api_grants WHERE id=?", [&id], |r| r.get(0)).optional()?;
        if old.is_some_and(|old| principal != &Value::String(old)) {
            return fail(AccessFailure::GrantPrincipalConflict);
        }
        db.execute(
            "INSERT INTO api_grants(id,principal,definition) VALUES (?,?,?) ON CONFLICT(id) DO UPDATE SET definition=excluded.definition,revision=revision+1,enabled=1",
            params![id, scalar(principal, 2)?, encode(definition)],
        )?;
        db.execute("INSERT INTO api_audit(principal,action,resource) VALUES (?,'grant.put',?)", params![scalar(principal, 1)?, scalar(&identifier, 2)?])?;
        Ok(())
    })?;
    Ok(json!({"grant_id":identifier,"definition":definition}))
}

pub fn issue(store: &Store, principal: &Value, grants: &Value, ttl: &Value) -> Result<Value> {
    issue_detailed(store, principal, grants, ttl).map_err(Into::into)
}
pub fn issue_detailed(
    store: &Store,
    principal: &Value,
    grants: &Value,
    ttl: &Value,
) -> DetailedResult<Value> {
    issue_with_sources_detailed(
        store,
        principal,
        grants,
        ttl,
        |bytes| getrandom::fill(bytes).map_err(|e| Error(e.to_string())),
        crate::notifications::now,
    )
}

#[cfg(test)]
fn issue_with_sources(
    store: &Store,
    principal: &Value,
    grants: &Value,
    ttl: &Value,
    entropy: impl FnOnce(&mut [u8]) -> Result<()>,
    clock: impl FnOnce() -> f64,
) -> Result<Value> {
    issue_with_sources_detailed(store, principal, grants, ttl, entropy, clock).map_err(Into::into)
}

fn issue_with_sources_detailed(
    store: &Store,
    principal: &Value,
    grants: &Value,
    ttl: &Value,
    entropy: impl FnOnce(&mut [u8]) -> Result<()>,
    clock: impl FnOnce() -> f64,
) -> DetailedResult<Value> {
    let Some(ids) = grants
        .as_array()
        .filter(|ids| !ids.is_empty() && ids.len() <= 100)
    else {
        return fail(AccessFailure::InvalidTokenConfiguration);
    };
    let Some(ttl) = ttl.as_u64().filter(|ttl| (1..=2592000).contains(ttl)) else {
        return fail(AccessFailure::InvalidTokenConfiguration);
    };
    for id in ids {
        if !store
            .db
            .prepare("SELECT 1 FROM api_grants WHERE id=? AND principal=? AND enabled=1")?
            .exists(params![scalar(id, 1)?, scalar(principal, 2)?])?
        {
            return fail(AccessFailure::InvalidTokenGrant);
        }
    }
    // A disabled principal is deliberately not re-enabled or rejected here.
    // Frozen issuance checks grant enablement; authentication checks principal.
    let mut random = [0u8; 44];
    entropy(&mut random)?;
    let identifier = random[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let secret = format!("{identifier}.{}", URL_SAFE_NO_PAD.encode(&random[12..]));
    let digest = sha256(&secret);
    let expires = clock() + ttl as f64;
    store.transaction_detailed(|db| -> DetailedResult<()> {
        db.execute(
            "INSERT INTO api_tokens(id,principal,digest,grants,expires) VALUES (?,?,?,?,?)",
            params![
                identifier,
                scalar(principal, 2)?,
                digest,
                encode(grants),
                expires
            ],
        )?;
        db.execute(
            "INSERT INTO api_audit(principal,action,resource) VALUES (?,'token.issue',?)",
            params![scalar(principal, 1)?, identifier],
        )?;
        Ok(())
    })?;
    Ok(json!({"token_id":identifier,"token":secret,"expires":expires}))
}

#[cfg(test)]
#[path = "access_admin_tests.rs"]
mod tests;

/// Shared field-mask validation for trusted native hosts; no persisted principal.
pub(crate) fn validate_host_metadata_fields(fields: &[String]) -> Result<()> {
    validate_grant(&json!({"actions":["metadata:read"],"metadata_fields":fields}))
        .map_err(Into::into)
}
