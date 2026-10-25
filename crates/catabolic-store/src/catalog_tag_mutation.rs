// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Database-only tag extension. Import into the store only after R2 acceptance.
//! Only catalog_commands may call apply inside its top-level writer transaction;
//! it owns authority, canonical payloads, receipt replay, commit and rollback.
//! No logical media IDs are redirected. The mutation owner has no clocks, I/O
//! or callbacks. Public previews reuse Store admission/lifetime checks outside
//! any caller transaction; their snapshot reader remains database-only.
use crate::{Error as StoreError, Store, tagging};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const MAX_SUPPRESSIONS: usize = 128;
pub const MAX_MERGE_ROWS: usize = 1000;
pub const MAX_HIERARCHY_VISITS: usize = 4096;
pub const MAX_HISTORY_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_PREVIEW_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "id",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum TagSubject {
    Item(String),
    File(String),
}
impl TagSubject {
    fn kind(&self) -> &'static str {
        match self {
            Self::Item(_) => "item",
            Self::File(_) => "file",
        }
    }
    fn id(&self) -> &str {
        match self {
            Self::Item(id) | Self::File(id) => id,
        }
    }
}
/// Current trusted host scope. Never deserialize this from request JSON or infer
/// it from the requested IDs. The host must re-establish it for preview and apply.
/// This privileged contract is not an R4 lease or a capability/authentication system.
#[derive(Clone, Debug)]
pub struct TagAuthority {
    pub host_namespace: String,
    pub allowed_subjects: BTreeSet<TagSubject>,
    pub allow_global_merge: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuppressionChange {
    pub subject: TagSubject,
    pub canonical_tag_id: String,
    /// None asserts no current decision; an unsuppressed head still has a revision.
    pub expected_revision: Option<i64>,
    pub suppressed: bool,
    pub reason: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollisionEvidence {
    Source,
    Target,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssertionCollision {
    pub subject: TagSubject,
    pub source_namespace: String,
    /// Choose confidence/note on a same-provider collision. The target assertion
    /// ID survives; active is unioned, and both complete inputs remain history.
    pub evidence: CollisionEvidence,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TagMerge {
    pub source_tag_id: String,
    pub target_tag_id: String,
    pub expected_source_revision: i64,
    pub expected_target_revision: i64,
    /// Digest of the authorized read-only preview; the owner recomputes it.
    pub preview_digest: String,
    /// Exactly one explicit choice per collision, including equal-valued rows.
    pub collisions: Vec<AssertionCollision>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum TagMutation {
    SetSuppression {
        contract_version: u32,
        changes: Vec<SuppressionChange>,
    },
    Merge {
        contract_version: u32,
        merge: TagMerge,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagErrorCode {
    InvalidInput,
    AuthorityDenied,
    StaleIncarnation,
    PreviewMismatch,
    TransactionRequired,
    UnsupportedSchema,
    AmbientReadTransaction,
    StaleGeneration,
    StaleRevision,
    UnknownSubject,
    RetiredSubject,
    UnknownTag,
    RetiredTag,
    CollisionChoicesRequired,
    HierarchyCycle,
    LimitExceeded,
    StoreFailure,
}
impl TagErrorCode {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "catalog_tag_invalid_input",
            Self::AuthorityDenied => "catalog_tag_authority_denied",
            Self::StaleIncarnation => "catalog_tag_stale_incarnation",
            Self::PreviewMismatch => "catalog_tag_preview_mismatch",
            Self::TransactionRequired => "catalog_tag_transaction_required",
            Self::UnsupportedSchema => "catalog_tag_unsupported_schema",
            Self::AmbientReadTransaction => "catalog_tag_ambient_read_transaction",
            Self::StaleGeneration => "catalog_tag_stale_generation",
            Self::StaleRevision => "catalog_tag_stale_revision",
            Self::UnknownSubject => "catalog_tag_unknown_subject",
            Self::RetiredSubject => "catalog_tag_retired_subject",
            Self::UnknownTag => "catalog_tag_unknown_tag",
            Self::RetiredTag => "catalog_tag_retired",
            Self::CollisionChoicesRequired => "catalog_tag_collision_choices_required",
            Self::HierarchyCycle => "catalog_tag_hierarchy_cycle",
            Self::LimitExceeded => "catalog_tag_limit_exceeded",
            Self::StoreFailure => "catalog_tag_store_failure",
        }
    }
}
#[derive(Debug)]
pub struct TagError {
    pub code: TagErrorCode,
    pub error: StoreError,
}
impl std::fmt::Display for TagError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for TagError {}
impl From<rusqlite::Error> for TagError {
    fn from(e: rusqlite::Error) -> Self {
        Self {
            code: TagErrorCode::StoreFailure,
            error: e.into(),
        }
    }
}
impl From<serde_json::Error> for TagError {
    fn from(e: serde_json::Error) -> Self {
        Self {
            code: TagErrorCode::StoreFailure,
            error: e.into(),
        }
    }
}
impl From<StoreError> for TagError {
    fn from(error: StoreError) -> Self {
        Self {
            code: TagErrorCode::StoreFailure,
            error,
        }
    }
}
pub type Result<T> = std::result::Result<T, TagError>;
fn fail<T>(code: TagErrorCode, message: impl Into<String>) -> Result<T> {
    Err(TagError {
        code,
        error: StoreError(message.into()),
    })
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SuppressionResult {
    pub subject: TagSubject,
    pub canonical_tag_id: String,
    pub revision: i64,
    pub suppressed: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AssertionMapping {
    pub subject: TagSubject,
    pub previous_assertion_id: String,
    pub surviving_assertion_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TagMergeResult {
    pub retired_tag_id: String,
    pub canonical_tag_id: String,
    pub assertion_mappings: Vec<AssertionMapping>,
    pub suppressions: Vec<SuppressionResult>,
    pub redirected_tag_ids: Vec<String>,
    pub preview_digest: String,
    pub mappings: TagMergeMappings,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum TagMutationResult {
    SetSuppression { decisions: Vec<SuppressionResult> },
    Merge { result: Box<TagMergeResult> },
}
/// Trusted dispatcher context, never accepted from operation JSON. Reuse the
/// envelope generation and existing receipt key. A receipt must exist by commit.
pub(crate) struct TagCommandContext<'a> {
    pub catalog_id: &'a str,
    pub host_namespace: &'a str,
    pub operation_id: &'a str,
    pub expected_generation: i64,
    pub expected_incarnation: &'a str,
    pub recorded_at_us: i64,
}
fn id(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 || value.chars().any(tagging::unicode_other) {
        return fail(
            TagErrorCode::InvalidInput,
            "identifier must be 1..256 UTF-8 bytes without controls",
        );
    }
    Ok(())
}
fn tag_revision(db: &Connection, tag: &str) -> Result<i64> {
    id(tag)?;
    if db
        .query_row(
            "SELECT target_tag_id FROM catalog_tag_redirects WHERE source_tag_id=?",
            [tag],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .is_some()
    {
        return fail(
            TagErrorCode::RetiredTag,
            "tag ID is retired; obtain a new authorized preview",
        );
    }
    if !db.query_row("SELECT EXISTS(SELECT 1 FROM tags WHERE id=?)", [tag], |r| {
        r.get::<_, bool>(0)
    })? {
        return fail(TagErrorCode::UnknownTag, "canonical tag ID does not exist");
    }
    // Native migration seeds existing tag revisions; untracked live tags fail closed.
    db.query_row("SELECT revision FROM catalog_entity_revisions WHERE entity_kind='tags' AND entity_key=json_array(?) AND deleted=0", [tag], |r|r.get(0)).map_err(Into::into)
}
#[derive(Clone, Debug, Serialize)]
pub struct TagDecisionSnapshot {
    pub subject: TagSubject,
    pub tag_id: String,
    pub revision: i64,
    pub suppressed: bool,
    pub reason: String,
    pub recorded_at_us: i64,
    pub command_catalog_id: String,
    pub command_host_namespace: String,
    pub command_operation_id: String,
}
type Decision = TagDecisionSnapshot;
fn decision(db: &Connection, subject: &TagSubject, tag: &str) -> Result<Option<Decision>> {
    let kind = subject.kind();
    Ok(db.query_row(&format!("SELECT s.revision,h.suppressed,h.reason,h.recorded_at_us,h.command_catalog_id,h.command_host_namespace,h.command_operation_id FROM catalog_{kind}_tag_suppressions s JOIN catalog_tag_suppression_revisions h ON h.subject_type=s.subject_type AND h.subject_id=s.{kind}_id AND h.tag_id=s.tag_id AND h.revision=s.revision WHERE s.{kind}_id=? AND s.tag_id=?"), params![subject.id(),tag], |r| Ok(Decision {subject:subject.clone(),tag_id:tag.into(),revision:r.get(0)?,suppressed:r.get(1)?,reason:r.get(2)?,recorded_at_us:r.get(3)?,command_catalog_id:r.get(4)?,command_host_namespace:r.get(5)?,command_operation_id:r.get(6)?})).optional()?)
}
fn save_decision(
    db: &Connection,
    ctx: &TagCommandContext<'_>,
    subject: &TagSubject,
    tag: &str,
    old: Option<&Decision>,
    suppressed: bool,
    reason: &str,
) -> Result<SuppressionResult> {
    let revision = old
        .map_or(Some(1), |d| d.revision.checked_add(1))
        .ok_or_else(|| TagError {
            code: TagErrorCode::LimitExceeded,
            error: StoreError("suppression revision exhausted".into()),
        })?;
    db.execute(
        "INSERT INTO catalog_tag_suppression_revisions VALUES (?,?,?,?,?,?,?,?,?,?)",
        params![
            subject.kind(),
            subject.id(),
            tag,
            revision,
            suppressed,
            reason,
            ctx.recorded_at_us,
            ctx.catalog_id,
            ctx.host_namespace,
            ctx.operation_id
        ],
    )?;
    let kind = subject.kind();
    db.execute(&format!("INSERT INTO catalog_{kind}_tag_suppressions(subject_type,{kind}_id,tag_id,revision) VALUES (?,?,?,?) ON CONFLICT({kind}_id,tag_id) DO UPDATE SET revision=excluded.revision"), params![kind,subject.id(),tag,revision])?;
    Ok(SuppressionResult {
        subject: subject.clone(),
        canonical_tag_id: tag.into(),
        revision,
        suppressed,
    })
}
/// Crate-private DB owner: any error requires the caller to roll back its writer
/// transaction. It intentionally cannot commit or create a receipt itself.
pub(crate) fn apply(
    db: &Connection,
    ctx: &TagCommandContext<'_>,
    authority: &TagAuthority,
    mutation: &TagMutation,
) -> Result<TagMutationResult> {
    require_namespace(authority, ctx.host_namespace)?;
    match mutation {
        TagMutation::SetSuppression { changes, .. } => {
            if changes
                .iter()
                .any(|change| !authority.allowed_subjects.contains(&change.subject))
            {
                return authority_denied();
            }
        }
        TagMutation::Merge { merge, .. } => require_merge_scope(
            db,
            authority,
            ctx.host_namespace,
            &merge.source_tag_id,
            &merge.target_tag_id,
        )?,
    }
    if db.is_autocommit() {
        return fail(
            TagErrorCode::TransactionRequired,
            "catalog command writer transaction required",
        );
    }
    if !db.pragma_query_value(None, "foreign_keys", |r| r.get::<_, bool>(0))? {
        return fail(
            TagErrorCode::TransactionRequired,
            "foreign keys must be enabled",
        );
    }
    id(ctx.catalog_id)?;
    id(ctx.host_namespace)?;
    id(ctx.operation_id)?;
    if ctx.expected_generation < 0 || ctx.recorded_at_us < 0 {
        return fail(TagErrorCode::InvalidInput, "invalid command context");
    }
    let current = catalog_snapshot(db)?;
    let catalog = current.catalog_id;
    let generation = current.generation;
    if current.incarnation != ctx.expected_incarnation {
        return fail(
            TagErrorCode::StaleIncarnation,
            "catalog incarnation changed; preview again",
        );
    }
    if catalog != ctx.catalog_id {
        return fail(TagErrorCode::InvalidInput, "catalog identity mismatch");
    }
    if generation != ctx.expected_generation {
        return fail(
            TagErrorCode::StaleGeneration,
            "catalog affected set changed; preview again",
        );
    }
    match mutation {
        TagMutation::SetSuppression {
            contract_version,
            changes,
        } => {
            if *contract_version != 1 || changes.is_empty() || changes.len() > MAX_SUPPRESSIONS {
                return fail(
                    TagErrorCode::InvalidInput,
                    "unsupported contract or suppression batch size",
                );
            }
            let mut unique = BTreeSet::new();
            let mut before = Vec::new();
            // Validate the whole batch before its first write.
            for change in changes {
                id(change.subject.id())?;
                if let TagSubject::Item(item_id) = &change.subject
                    && db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id=?)",
                        [item_id],
                        |r| r.get::<_, bool>(0),
                    )?
                {
                    return fail(
                        TagErrorCode::RetiredSubject,
                        "item is retired; a new explicit logical-subject decision is required",
                    );
                }
                tag_revision(db, &change.canonical_tag_id)?;
                if !unique.insert((change.subject.clone(), change.canonical_tag_id.clone()))
                    || change.expected_revision.is_some_and(|v| v < 1)
                    || change.reason.len() > 4000
                    || change.reason.chars().any(tagging::unicode_other)
                {
                    return fail(
                        TagErrorCode::InvalidInput,
                        "duplicate suppression target, invalid revision or reason",
                    );
                }
                let kind = change.subject.kind();
                if !db.query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM {kind}s WHERE id=?)"),
                    [change.subject.id()],
                    |r| r.get::<_, bool>(0),
                )? {
                    return fail(
                        TagErrorCode::UnknownSubject,
                        "suppression subject does not exist",
                    );
                }
                let old = decision(db, &change.subject, &change.canonical_tag_id)?;
                if old.as_ref().map(|d| d.revision) != change.expected_revision {
                    return fail(TagErrorCode::StaleRevision, "suppression revision changed");
                }
                before.push(old);
            }
            let decisions = changes
                .iter()
                .zip(before.iter())
                .map(|(change, old)| {
                    save_decision(
                        db,
                        ctx,
                        &change.subject,
                        &change.canonical_tag_id,
                        old.as_ref(),
                        change.suppressed,
                        &change.reason,
                    )
                })
                .collect::<Result<_>>()?;
            Ok(TagMutationResult::SetSuppression { decisions })
        }
        TagMutation::Merge {
            contract_version,
            merge,
        } => {
            if *contract_version != 1 {
                return fail(TagErrorCode::InvalidInput, "unsupported tag contract");
            }
            Ok(TagMutationResult::Merge {
                result: Box::new(apply_merge(db, ctx, authority, merge)?),
            })
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct TagAssertionSnapshot {
    pub id: String,
    pub subject: TagSubject,
    pub tag_id: String,
    pub source: String,
    pub confidence: Option<f64>,
    pub note: String,
    pub active: bool,
    pub created_at: String,
    pub updated_at: String,
}
type Assertion = TagAssertionSnapshot;
fn assertions(db: &Connection, tag: &str) -> Result<Vec<Assertion>> {
    let mut rows = Vec::new();
    for kind in ["item", "file"] {
        let mut q=db.prepare(&format!("SELECT id,{kind}_id,tag_id,source,confidence,note,active,created_at,updated_at FROM {kind}_tags WHERE tag_id=? ORDER BY {kind}_id,source LIMIT ?"))?;
        let found = q.query_map(params![tag, (MAX_MERGE_ROWS + 1) as i64], |r| {
            let subject = r.get(1)?;
            Ok(Assertion {
                id: r.get(0)?,
                subject: if kind == "item" {
                    TagSubject::Item(subject)
                } else {
                    TagSubject::File(subject)
                },
                tag_id: r.get(2)?,
                source: r.get(3)?,
                confidence: r.get(4)?,
                note: r.get(5)?,
                active: r.get(6)?,
                created_at: r.get(7)?,
                updated_at: r.get(8)?,
            })
        })?;
        for row in found {
            rows.push(row?);
        }
    }
    Ok(rows)
}
fn all_decisions(db: &Connection, tag: &str) -> Result<Vec<Decision>> {
    let mut rows = Vec::new();
    for kind in ["item", "file"] {
        let mut q=db.prepare(&format!("SELECT {kind}_id FROM catalog_{kind}_tag_suppressions WHERE tag_id=? ORDER BY {kind}_id LIMIT ?"))?;
        let ids = q
            .query_map(params![tag, (MAX_MERGE_ROWS + 1) as i64], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for id in ids {
            let subject = if kind == "item" {
                TagSubject::Item(id)
            } else {
                TagSubject::File(id)
            };
            rows.push(decision(db, &subject, tag)?.ok_or_else(|| TagError {
                code: TagErrorCode::StoreFailure,
                error: StoreError("invalid suppression head".into()),
            })?);
        }
    }
    Ok(rows)
}
// Bound retained input bytes before materializing strings. SQL length works on
// BLOB byte lengths; final JSON serialization has its own stricter byte check.
fn guard_merge_input_size(db: &Connection, source: &str, target: &str) -> Result<()> {
    let mut rows = 0_i64;
    let mut bytes = 0_i64;
    for kind in ["item", "file"] {
        let (count,size):(i64,i64)=db.query_row(&format!("SELECT count(*),coalesce(sum(length(CAST(id AS BLOB))+length(CAST({kind}_id AS BLOB))+length(CAST(tag_id AS BLOB))+length(CAST(source AS BLOB))+length(CAST(note AS BLOB))+length(CAST(created_at AS BLOB))+length(CAST(updated_at AS BLOB))),0) FROM {kind}_tags WHERE tag_id IN (?,?)"),params![source,target],|r|Ok((r.get(0)?,r.get(1)?)))?;
        rows = rows.checked_add(count).unwrap_or(i64::MAX);
        bytes = bytes.checked_add(size).unwrap_or(i64::MAX);
        if rows > MAX_MERGE_ROWS as i64 || bytes > MAX_HISTORY_BYTES as i64 {
            return fail(
                TagErrorCode::LimitExceeded,
                "merge assertion inputs exceed bound",
            );
        }
    }
    let tag_bytes:i64=db.query_row("SELECT coalesce(sum(length(CAST(id AS BLOB))+length(CAST(name AS BLOB))+length(CAST(description AS BLOB))),0) FROM tags WHERE id IN (?,?)",params![source,target],|r|r.get(0))?;
    if bytes
        .checked_add(tag_bytes)
        .is_none_or(|n| n > MAX_HISTORY_BYTES as i64)
    {
        return fail(
            TagErrorCode::LimitExceeded,
            "merge tag inputs exceed byte bound",
        );
    }
    bytes = bytes.checked_add(tag_bytes).unwrap_or(i64::MAX);
    for query in [
        "SELECT coalesce(sum(length(CAST(name AS BLOB))+length(CAST(tag_id AS BLOB))),0) FROM tag_names WHERE tag_id IN (?1,?2)",
        "SELECT coalesce(sum(length(CAST(child_id AS BLOB))+length(CAST(parent_id AS BLOB))),0) FROM tag_parents WHERE child_id IN (?1,?2) OR parent_id IN (?1,?2)",
        "SELECT coalesce(sum(length(CAST(source_tag_id AS BLOB))+length(CAST(target_tag_id AS BLOB))),0) FROM catalog_tag_redirects WHERE target_tag_id=?1 AND ?2 IS NOT NULL",
        "SELECT coalesce(sum(length(CAST(h.subject_id AS BLOB))+length(CAST(h.tag_id AS BLOB))+length(CAST(h.reason AS BLOB))),0) FROM catalog_item_tag_suppressions s JOIN catalog_tag_suppression_revisions h ON h.subject_type=s.subject_type AND h.subject_id=s.item_id AND h.tag_id=s.tag_id AND h.revision=s.revision WHERE s.tag_id IN (?1,?2)",
        "SELECT coalesce(sum(length(CAST(h.subject_id AS BLOB))+length(CAST(h.tag_id AS BLOB))+length(CAST(h.reason AS BLOB))),0) FROM catalog_file_tag_suppressions s JOIN catalog_tag_suppression_revisions h ON h.subject_type=s.subject_type AND h.subject_id=s.file_id AND h.tag_id=s.tag_id AND h.revision=s.revision WHERE s.tag_id IN (?1,?2)",
    ] {
        let size: i64 = db.query_row(query, params![source, target], |r| r.get(0))?;
        bytes = bytes.checked_add(size).unwrap_or(i64::MAX);
        if bytes > MAX_HISTORY_BYTES as i64 {
            return fail(
                TagErrorCode::LimitExceeded,
                "merge retained inputs exceed byte bound",
            );
        }
    }
    Ok(())
}

fn load_merge_affected(
    db: &Connection,
    source: &String,
    target: &String,
) -> Result<TagMergeAffected> {
    if source == target {
        return fail(
            TagErrorCode::InvalidInput,
            "invalid merge identity, revision or size",
        );
    }
    let source_revision = tag_revision(db, source)?;
    let target_revision = tag_revision(db, target)?;
    guard_merge_input_size(db, source, target)?;
    let src_assertions = assertions(db, source)?;
    let dst_assertions = assertions(db, target)?;
    let src_suppressions = all_decisions(db, source)?;
    let dst_suppressions = all_decisions(db, target)?;
    let mut names = db.prepare(
        "SELECT name,tag_id FROM tag_names WHERE tag_id IN (?,?) ORDER BY name LIMIT 102",
    )?;
    let names = names
        .query_map(params![source, target], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if names.len() > 101 {
        return fail(TagErrorCode::LimitExceeded, "merge exceeds 100 alias limit");
    }
    let mut parents=db.prepare("SELECT child_id,parent_id FROM tag_parents WHERE child_id IN (?,?) OR parent_id IN (?,?) ORDER BY child_id,parent_id LIMIT ?")?;
    let parents = parents
        .query_map(
            params![source, target, source, target, (MAX_MERGE_ROWS + 1) as i64],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut redirects=db.prepare("SELECT source_tag_id FROM catalog_tag_redirects WHERE target_tag_id=? ORDER BY source_tag_id LIMIT ?")?;
    let redirects = redirects
        .query_map(params![source, (MAX_MERGE_ROWS + 1) as i64], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if src_assertions.len()
        + dst_assertions.len()
        + src_suppressions.len()
        + dst_suppressions.len()
        + names.len()
        + parents.len()
        + redirects.len()
        + 2 // Both vocabulary rows are part of the affected current snapshot.
        > MAX_MERGE_ROWS
    {
        return fail(
            TagErrorCode::LimitExceeded,
            "merge affected set exceeds row bound",
        );
    }
    // Validate the projected hierarchy before changing it. A new cycle must pass
    // through the surviving vertex. Deduped recursion is bounded, even on damage.
    let mut ancestors=db.prepare("WITH RECURSIVE mapped(child,parent) AS (SELECT CASE WHEN child_id=?1 THEN ?2 ELSE child_id END,CASE WHEN parent_id=?1 THEN ?2 ELSE parent_id END FROM tag_parents), edges(child,parent) AS (SELECT child,parent FROM mapped WHERE child != parent), ancestors(id) AS (SELECT parent FROM edges WHERE child=?2 UNION SELECT e.parent FROM edges e JOIN ancestors a ON e.child=a.id LIMIT ?3) SELECT id FROM ancestors")?;
    let ancestors = ancestors
        .query_map(
            params![source, target, (MAX_HIERARCHY_VISITS + 1) as i64],
            |r| r.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if ancestors.len() > MAX_HIERARCHY_VISITS {
        return fail(
            TagErrorCode::LimitExceeded,
            "merge hierarchy traversal exceeds bound",
        );
    }
    if ancestors.iter().any(|id| id == target) {
        return fail(
            TagErrorCode::HierarchyCycle,
            "tag merge would create a hierarchy cycle",
        );
    }
    let mapped_parents: BTreeSet<_> = parents
        .iter()
        .map(|(child, parent)| {
            (
                if child == source {
                    target.clone()
                } else {
                    child.clone()
                },
                if parent == source {
                    target.clone()
                } else {
                    parent.clone()
                },
            )
        })
        .filter(|(c, p)| c != p)
        .collect();
    for child in mapped_parents
        .iter()
        .map(|(c, _)| c)
        .collect::<BTreeSet<_>>()
    {
        let retained:i64=db.query_row("SELECT count(*) FROM tag_parents WHERE child_id=? AND child_id NOT IN (?,?) AND parent_id NOT IN (?,?)",params![child,source,target,source,target],|r|r.get(0))?;
        if retained as usize + mapped_parents.iter().filter(|(c, _)| c == child).count() > 100 {
            return fail(
                TagErrorCode::LimitExceeded,
                "merge exceeds direct-parent limit",
            );
        }
    }
    let mut tags =
        db.prepare("SELECT id,name,description FROM tags WHERE id IN (?,?) ORDER BY id")?;
    let tags = tags
        .query_map(params![source, target], |r| {
            Ok(TagVocabularySnapshot {
                id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(TagMergeAffected {
        source_tag_id: source.clone(),
        target_tag_id: target.clone(),
        source_revision,
        target_revision,
        source_assertions: src_assertions,
        target_assertions: dst_assertions,
        source_suppressions: src_suppressions,
        target_suppressions: dst_suppressions,
        tags,
        names: names
            .into_iter()
            .map(|(name, tag_id)| TagNameSnapshot { name, tag_id })
            .collect(),
        parents: parents
            .into_iter()
            .map(|(child_id, parent_id)| TagParentSnapshot {
                child_id,
                parent_id,
            })
            .collect(),
        redirects: redirects
            .into_iter()
            .map(|source_tag_id| TagRedirectSnapshot {
                source_tag_id,
                target_tag_id: source.clone(),
            })
            .collect(),
        resulting_parents: mapped_parents
            .into_iter()
            .map(|(child_id, parent_id)| TagParentSnapshot {
                child_id,
                parent_id,
            })
            .collect(),
    })
}

fn apply_merge(
    db: &Connection,
    ctx: &TagCommandContext<'_>,
    authority: &TagAuthority,
    merge: &TagMerge,
) -> Result<TagMergeResult> {
    if merge.collisions.len() > MAX_MERGE_ROWS
        || merge.expected_source_revision < 1
        || merge.expected_target_revision < 1
    {
        return fail(TagErrorCode::InvalidInput, "invalid merge revision or size");
    }
    let preview = preview_merge_connection(
        db,
        authority,
        ctx.host_namespace,
        &merge.source_tag_id,
        &merge.target_tag_id,
    )?;
    if preview.body.affected.source_revision != merge.expected_source_revision
        || preview.body.affected.target_revision != merge.expected_target_revision
    {
        return fail(TagErrorCode::StaleRevision, "tag revision changed");
    }
    if preview.digest != merge.preview_digest {
        return fail(
            TagErrorCode::PreviewMismatch,
            "merge preview does not match the current authorized affected set",
        );
    }
    let source = &merge.source_tag_id;
    let target = &merge.target_tag_id;
    let affected = &preview.body.affected;
    let src_assertions = &affected.source_assertions;
    let dst_assertions = &affected.target_assertions;
    if db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? == 41
        && src_assertions.iter().any(|row| row.source == "motion.nfo")
    {
        return fail(
            TagErrorCode::AuthorityDenied,
            "NFO assertion merge requires accounted source lineage",
        );
    }

    let src_suppressions = &affected.source_suppressions;
    let dst_suppressions = &affected.target_suppressions;
    let mut choices = BTreeSet::new();
    for choice in &merge.collisions {
        id(choice.subject.id())?;
        if choice.source_namespace.is_empty()
            || choice.source_namespace.len() > 800
            || !choices.insert((choice.subject.clone(), choice.source_namespace.clone()))
        {
            return fail(
                TagErrorCode::InvalidInput,
                "invalid or duplicate collision choice",
            );
        }
    }
    let required: BTreeSet<_> = src_assertions
        .iter()
        .filter(|s| {
            dst_assertions
                .iter()
                .any(|d| d.subject == s.subject && d.source == s.source)
        })
        .map(|s| (s.subject.clone(), s.source.clone()))
        .collect();
    if required != choices {
        return fail(
            TagErrorCode::CollisionChoicesRequired,
            "every assertion collision requires exactly one reviewed evidence choice",
        );
    }
    let snapshot = merge_history_json(merge, &preview)?;
    db.execute(
        "INSERT INTO catalog_tag_merges VALUES (?,?,?,?,?,?,?)",
        params![
            source,
            target,
            snapshot,
            ctx.recorded_at_us,
            ctx.catalog_id,
            ctx.host_namespace,
            ctx.operation_id
        ],
    )?;
    let mut mappings = Vec::new();
    for row in src_assertions {
        let kind = row.subject.kind();
        let survivor = if let Some(other) = dst_assertions
            .iter()
            .find(|d| d.subject == row.subject && d.source == row.source)
        {
            let choice = merge
                .collisions
                .iter()
                .find(|c| c.subject == row.subject && c.source_namespace == row.source)
                .expect("validated collision choice");
            let evidence = match choice.evidence {
                CollisionEvidence::Source => row,
                CollisionEvidence::Target => other,
            };
            // Provider timestamps stay provider evidence; command time is separate
            // retained merge history. Do not pretend a merge was a provider refresh.
            db.execute(&format!("UPDATE {kind}_tags SET confidence=?,note=?,active=?,created_at=?,updated_at=? WHERE id=?"),params![evidence.confidence,evidence.note,row.active||other.active,evidence.created_at,evidence.updated_at,other.id])?;
            db.execute(&format!("DELETE FROM {kind}_tags WHERE id=?"), [&row.id])?;
            other.id.clone()
        } else {
            db.execute(
                &format!("UPDATE {kind}_tags SET tag_id=? WHERE id=?"),
                params![target, row.id],
            )?;
            row.id.clone()
        };
        mappings.push(AssertionMapping {
            subject: row.subject.clone(),
            previous_assertion_id: row.id.clone(),
            surviving_assertion_id: survivor,
        });
    }
    let mut suppressions = Vec::new();
    for source_decision in src_suppressions {
        let destination = dst_suppressions
            .iter()
            .find(|d| d.subject == source_decision.subject);
        // Suppression union is privacy-conservative. An explicit unsuppress on
        // the canonical target remains available with its new checked revision.
        let suppressed = source_decision.suppressed || destination.is_some_and(|d| d.suppressed);
        let reason = destination
            .filter(|d| d.suppressed)
            .map_or(source_decision.reason.as_str(), |d| d.reason.as_str());
        suppressions.push(save_decision(
            db,
            ctx,
            &source_decision.subject,
            target,
            destination,
            suppressed,
            reason,
        )?);
        let kind = source_decision.subject.kind();
        db.execute(
            &format!("DELETE FROM catalog_{kind}_tag_suppressions WHERE {kind}_id=? AND tag_id=?"),
            params![source_decision.subject.id(), source],
        )?;
    }
    db.execute(
        "DELETE FROM tag_parents WHERE child_id=? OR parent_id=?",
        params![source, source],
    )?;
    for parent in &affected.resulting_parents {
        db.execute(
            "INSERT OR IGNORE INTO tag_parents(child_id,parent_id) VALUES (?,?)",
            params![parent.child_id, parent.parent_id],
        )?;
    }
    db.execute(
        "UPDATE tag_names SET tag_id=? WHERE tag_id=?",
        params![target, source],
    )?;
    db.execute(
        "UPDATE catalog_tag_redirects SET target_tag_id=? WHERE target_tag_id=?",
        params![target, source],
    )?;
    db.execute(
        "INSERT INTO catalog_tag_redirects VALUES (?,?)",
        params![source, target],
    )?;
    db.execute("DELETE FROM tags WHERE id=?", [source])?;
    Ok(TagMergeResult {
        retired_tag_id: source.clone(),
        canonical_tag_id: target.clone(),
        assertion_mappings: mappings,
        suppressions,
        redirected_tag_ids: affected
            .redirects
            .iter()
            .map(|r| r.source_tag_id.clone())
            .collect(),
        preview_digest: preview.digest,
        mappings: preview.body.mappings,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TagCatalogSnapshot {
    pub catalog_id: String,
    pub incarnation: String,
    pub generation: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TagVocabularySnapshot {
    pub id: String,
    pub name: String,
    pub description: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TagNameSnapshot {
    pub name: String,
    pub tag_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TagParentSnapshot {
    pub child_id: String,
    pub parent_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TagRedirectSnapshot {
    pub source_tag_id: String,
    pub target_tag_id: String,
}
/// Complete bounded current rows affected by the merge. Prior immutable history
/// is not rewritten and is therefore not included in the affected current set.
#[derive(Clone, Debug, Serialize)]
pub struct TagMergeAffected {
    pub source_tag_id: String,
    pub target_tag_id: String,
    pub source_revision: i64,
    pub target_revision: i64,
    pub tags: Vec<TagVocabularySnapshot>,
    pub names: Vec<TagNameSnapshot>,
    pub parents: Vec<TagParentSnapshot>,
    pub redirects: Vec<TagRedirectSnapshot>,
    pub source_assertions: Vec<TagAssertionSnapshot>,
    pub target_assertions: Vec<TagAssertionSnapshot>,
    pub source_suppressions: Vec<TagDecisionSnapshot>,
    pub target_suppressions: Vec<TagDecisionSnapshot>,
    pub resulting_parents: Vec<TagParentSnapshot>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TagCollisionOptions {
    pub subject: TagSubject,
    pub source_namespace: String,
    pub source_assertion_id: String,
    pub target_assertion_id: String,
    pub evidence_options: [CollisionEvidence; 2],
    pub resulting_active: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TagSuppressionMapping {
    pub subject: TagSubject,
    pub previous_tag_id: String,
    pub previous_revision: i64,
    pub canonical_tag_id: String,
    pub resulting_revision: i64,
    pub suppressed: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TagNameMapping {
    pub name: String,
    pub previous_tag_id: String,
    pub canonical_tag_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TagParentMapping {
    pub before: TagParentSnapshot,
    pub after: Option<TagParentSnapshot>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TagRedirectMapping {
    pub retired_tag_id: String,
    pub previous_target_tag_id: Option<String>,
    pub canonical_tag_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TagMergeMappings {
    pub retired_tag_id: String,
    pub canonical_tag_id: String,
    pub assertions: Vec<AssertionMapping>,
    pub suppressions: Vec<TagSuppressionMapping>,
    pub names: Vec<TagNameMapping>,
    pub parents: Vec<TagParentMapping>,
    pub redirects: Vec<TagRedirectMapping>,
}
#[derive(Clone, Debug, Serialize)]
pub struct TagMergePreviewBody {
    pub contract_version: u32,
    pub canonicalization_version: u32,
    pub host_namespace: String,
    pub catalog: TagCatalogSnapshot,
    pub affected_subjects: Vec<TagSubject>,
    /// Exact retired item IDs remain affected subjects; no survivor/child grant substitution.
    pub retired_item_ids: Vec<String>,
    pub affected: TagMergeAffected,
    pub collision_options: Vec<TagCollisionOptions>,
    pub mappings: TagMergeMappings,
}
#[derive(Clone, Debug, Serialize)]
pub struct TagMergePreview {
    pub body: TagMergePreviewBody,
    pub digest: String,
}

fn authority_denied<T>() -> Result<T> {
    // Never disclose IDs, counts, evidence, namespaces or digests on denial.
    fail(
        TagErrorCode::AuthorityDenied,
        "tag operation is outside the trusted host scope",
    )
}
fn require_namespace(authority: &TagAuthority, host_namespace: &str) -> Result<()> {
    if authority.host_namespace != host_namespace {
        return authority_denied();
    }
    id(host_namespace)?;
    Ok(())
}
fn require_merge_scope(
    db: &Connection,
    authority: &TagAuthority,
    host_namespace: &str,
    source: &str,
    target: &str,
) -> Result<()> {
    require_namespace(authority, host_namespace)?;
    if !authority.allow_global_merge {
        return authority_denied();
    }
    id(source)?;
    id(target)?;
    // Fixed SQL checks the complete current set, including inactive assertions
    // and unsuppressed decision heads. Only the boolean leaves SQLite. It does
    // not accept a caller-provided member list or truncate hidden members.
    let allowed = serde_json::to_string(&authority.allowed_subjects)?;
    let denied:bool=db.query_row("WITH affected(kind,id) AS (
      SELECT 'item',item_id FROM item_tags WHERE tag_id IN (?1,?2)
      UNION SELECT 'file',file_id FROM file_tags WHERE tag_id IN (?1,?2)
      UNION SELECT 'item',item_id FROM catalog_item_tag_suppressions WHERE tag_id IN (?1,?2)
      UNION SELECT 'file',file_id FROM catalog_file_tag_suppressions WHERE tag_id IN (?1,?2))
      SELECT EXISTS(SELECT 1 FROM affected a WHERE NOT EXISTS(
        SELECT 1 FROM json_each(?3) s WHERE json_extract(s.value,'$.kind')=a.kind AND json_extract(s.value,'$.id')=a.id))",
        params![source,target,allowed],|r|r.get(0))?;
    if denied {
        return authority_denied();
    }
    Ok(())
}
fn catalog_snapshot(db: &Connection) -> Result<TagCatalogSnapshot> {
    Ok(db.query_row(
        "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
        [],
        |r| {
            Ok(TagCatalogSnapshot {
                catalog_id: r.get(0)?,
                incarnation: r.get(1)?,
                generation: r.get(2)?,
            })
        },
    )?)
}
fn affected_subjects(affected: &TagMergeAffected) -> Vec<TagSubject> {
    affected
        .source_assertions
        .iter()
        .chain(&affected.target_assertions)
        .map(|a| a.subject.clone())
        .chain(
            affected
                .source_suppressions
                .iter()
                .chain(&affected.target_suppressions)
                .map(|d| d.subject.clone()),
        )
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
fn merge_mappings(affected: &TagMergeAffected) -> Result<TagMergeMappings> {
    let source = &affected.source_tag_id;
    let target = &affected.target_tag_id;
    let assertions = affected
        .source_assertions
        .iter()
        .chain(&affected.target_assertions)
        .map(|a| {
            let survivor = if &a.tag_id == source {
                affected
                    .target_assertions
                    .iter()
                    .find(|d| d.subject == a.subject && d.source == a.source)
                    .unwrap_or(a)
            } else {
                a
            };
            AssertionMapping {
                subject: a.subject.clone(),
                previous_assertion_id: a.id.clone(),
                surviving_assertion_id: survivor.id.clone(),
            }
        })
        .collect();
    let suppressions = affected
        .source_suppressions
        .iter()
        .chain(&affected.target_suppressions)
        .map(|old| {
            let src = affected
                .source_suppressions
                .iter()
                .find(|d| d.subject == old.subject);
            let dst = affected
                .target_suppressions
                .iter()
                .find(|d| d.subject == old.subject);
            let revision = if src.is_some() {
                dst.map_or(Some(1), |d| d.revision.checked_add(1))
            } else {
                dst.map(|d| d.revision)
            }
            .ok_or_else(|| TagError {
                code: TagErrorCode::LimitExceeded,
                error: StoreError("suppression revision exhausted".into()),
            })?;
            Ok(TagSuppressionMapping {
                subject: old.subject.clone(),
                previous_tag_id: old.tag_id.clone(),
                previous_revision: old.revision,
                canonical_tag_id: target.clone(),
                resulting_revision: revision,
                suppressed: src.is_some_and(|s| s.suppressed) || dst.is_some_and(|d| d.suppressed),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let names = affected
        .names
        .iter()
        .map(|n| TagNameMapping {
            name: n.name.clone(),
            previous_tag_id: n.tag_id.clone(),
            canonical_tag_id: target.clone(),
        })
        .collect();
    let parents = affected
        .parents
        .iter()
        .map(|before| {
            let child = if &before.child_id == source {
                target
            } else {
                &before.child_id
            };
            let parent = if &before.parent_id == source {
                target
            } else {
                &before.parent_id
            };
            TagParentMapping {
                before: before.clone(),
                after: if child == parent {
                    None
                } else {
                    Some(TagParentSnapshot {
                        child_id: child.clone(),
                        parent_id: parent.clone(),
                    })
                },
            }
        })
        .collect();
    let mut redirects: Vec<_> = affected
        .redirects
        .iter()
        .map(|r| TagRedirectMapping {
            retired_tag_id: r.source_tag_id.clone(),
            previous_target_tag_id: Some(r.target_tag_id.clone()),
            canonical_tag_id: target.clone(),
        })
        .collect();
    redirects.push(TagRedirectMapping {
        retired_tag_id: source.clone(),
        previous_target_tag_id: None,
        canonical_tag_id: target.clone(),
    });
    Ok(TagMergeMappings {
        retired_tag_id: source.clone(),
        canonical_tag_id: target.clone(),
        assertions,
        suppressions,
        names,
        parents,
        redirects,
    })
}
fn canonical_preview_digest(body: &TagMergePreviewBody) -> Result<String> {
    canonical_preview_value_digest(&serde_json::to_value(body)?)
}
/// The same descriptive digest over already bounded retained preview data.
/// This does not validate authority, admit a caller preview, or perform a merge.
pub(crate) fn canonical_preview_value_digest(body: &Value) -> Result<String> {
    // Canonicalization v1: compact JSON, recursively lexicographic object keys,
    // array order preserved, serde_json numeric/string encoding; domain-separated
    // SHA-256. Independent of serde_json preserve_order feature unification.
    fn encode(value: &Value, out: &mut Vec<u8>) -> serde_json::Result<()> {
        match value {
            Value::Object(map) => {
                out.push(b'{');
                let keys = map.keys().collect::<BTreeSet<_>>();
                for (index, key) in keys.into_iter().enumerate() {
                    if index > 0 {
                        out.push(b',');
                    }
                    serde_json::to_writer(&mut *out, key)?;
                    out.push(b':');
                    encode(&map[key], out)?;
                }
                out.push(b'}');
            }
            Value::Array(values) => {
                out.push(b'[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        out.push(b',');
                    }
                    encode(value, out)?;
                }
                out.push(b']');
            }
            _ => serde_json::to_writer(&mut *out, value)?,
        }
        Ok(())
    }
    let mut bytes = Vec::new();
    encode(body, &mut bytes)?;
    if bytes.len() > MAX_PREVIEW_BYTES {
        return fail(
            TagErrorCode::LimitExceeded,
            "merge preview exceeds byte bound",
        );
    }
    let mut hash = Sha256::new();
    hash.update(b"catabolic.tag-merge-preview.v1\0");
    hash.update(bytes);
    Ok(format!("{:x}", hash.finalize()))
}
/// Read-only privileged host entry point. It does not begin/commit transactions,
/// acquire leases, mint authority, create receipts, or accept caller affected sets.
/// The host must provide current independently resolved TagAuthority each time.
pub(crate) fn preview_merge_connection(
    db: &Connection,
    authority: &TagAuthority,
    host_namespace: &str,
    source_tag_id: &str,
    target_tag_id: &str,
) -> Result<TagMergePreview> {
    require_namespace(authority, host_namespace)?;
    if !authority.allow_global_merge {
        return authority_denied();
    }
    let before = catalog_snapshot(db)?;
    require_merge_scope(db, authority, host_namespace, source_tag_id, target_tag_id)?;
    let affected = load_merge_affected(db, &source_tag_id.to_owned(), &target_tag_id.to_owned())?;
    let subjects = affected_subjects(&affected);
    if subjects
        .iter()
        .any(|subject| !authority.allowed_subjects.contains(subject))
    {
        return authority_denied();
    }
    let mut retired_item_ids = Vec::new();
    for subject in &subjects {
        if let TagSubject::Item(item_id) = subject
            && db.query_row(
                "SELECT EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id=?)",
                [item_id],
                |r| r.get::<_, bool>(0),
            )?
        {
            retired_item_ids.push(item_id.clone());
        }
    }
    let collisions = affected
        .source_assertions
        .iter()
        .filter_map(|a| {
            affected
                .target_assertions
                .iter()
                .find(|d| d.subject == a.subject && d.source == a.source)
                .map(|d| TagCollisionOptions {
                    subject: a.subject.clone(),
                    source_namespace: a.source.clone(),
                    source_assertion_id: a.id.clone(),
                    target_assertion_id: d.id.clone(),
                    evidence_options: [CollisionEvidence::Source, CollisionEvidence::Target],
                    resulting_active: a.active || d.active,
                })
        })
        .collect();
    let mappings = merge_mappings(&affected)?;
    let body = TagMergePreviewBody {
        contract_version: 1,
        canonicalization_version: 1,
        host_namespace: host_namespace.into(),
        catalog: before.clone(),
        affected_subjects: subjects,
        retired_item_ids,
        affected,
        collision_options: collisions,
        mappings,
    };
    let digest = canonical_preview_digest(&body)?;
    let preview = TagMergePreview { body, digest };
    // All valid choices have the same encoded evidence-option length. Check the
    // exact retained wrapper size now so an oversized merge cannot get a usable
    // preview only to fail solely on history overhead at apply.
    let size_probe = TagMerge {
        source_tag_id: source_tag_id.into(),
        target_tag_id: target_tag_id.into(),
        expected_source_revision: preview.body.affected.source_revision,
        expected_target_revision: preview.body.affected.target_revision,
        preview_digest: preview.digest.clone(),
        collisions: preview
            .body
            .collision_options
            .iter()
            .map(|option| AssertionCollision {
                subject: option.subject.clone(),
                source_namespace: option.source_namespace.clone(),
                evidence: CollisionEvidence::Source,
            })
            .collect(),
    };
    merge_history_json(&size_probe, &preview)?;
    // Recheck the complete source membership and scope after materializing and
    // hashing. A statement boundary can observe a new SQLite snapshot in
    // autocommit mode; the exact generation/incarnation catches any such change.
    require_merge_scope(db, authority, host_namespace, source_tag_id, target_tag_id)?;
    let after = catalog_snapshot(db)?;
    if before.incarnation != after.incarnation || before.catalog_id != after.catalog_id {
        return fail(
            TagErrorCode::StaleIncarnation,
            "catalog incarnation changed; preview again",
        );
    }
    if before.generation != after.generation {
        return fail(
            TagErrorCode::StaleGeneration,
            "catalog affected set changed; preview again",
        );
    }
    Ok(preview)
}
fn merge_history_json(merge: &TagMerge, preview: &TagMergePreview) -> Result<String> {
    let snapshot =
        serde_json::to_string(&json!({"contract_version":1,"request":merge,"preview":preview}))?;
    if snapshot.len() > MAX_HISTORY_BYTES {
        return fail(
            TagErrorCode::LimitExceeded,
            "merge history exceeds byte bound",
        );
    }
    Ok(snapshot)
}

/// Authorized preview on an admitted native Store. This public boundary rejects
/// caller-owned read/write transactions so its final checks see current state.
/// It uses the existing Store lifecycle owner, never a second admission system.
pub fn preview_merge(
    store: &Store,
    authority: &TagAuthority,
    host_namespace: &str,
    source_tag_id: &str,
    target_tag_id: &str,
) -> Result<TagMergePreview> {
    require_namespace(authority, host_namespace)?;
    if !authority.allow_global_merge {
        return authority_denied();
    }
    if store.schema_version < 34 || store.catalog_incarnation.is_none() {
        return fail(
            TagErrorCode::UnsupportedSchema,
            "tag preview requires admitted native schema34 or newer",
        );
    }
    if !store.db.is_autocommit() {
        return fail(
            TagErrorCode::AmbientReadTransaction,
            "tag preview requires no caller-owned transaction",
        );
    }
    store.transaction_available()?;
    let result = preview_merge_connection(
        &store.db,
        authority,
        host_namespace,
        source_tag_id,
        target_tag_id,
    );
    // Revalidate lifetime even when the database-only reader failed. Never
    // return a materialized preview from a replaced/poisoned/disconnected Store.
    store.transaction_available()?;
    let preview = result?;
    require_merge_scope(
        &store.db,
        authority,
        host_namespace,
        source_tag_id,
        target_tag_id,
    )?;
    let current = catalog_snapshot(&store.db)?;
    if current.catalog_id != preview.body.catalog.catalog_id
        || current.incarnation != preview.body.catalog.incarnation
    {
        return fail(
            TagErrorCode::StaleIncarnation,
            "catalog incarnation changed; preview again",
        );
    }
    if current.generation != preview.body.catalog.generation {
        return fail(
            TagErrorCode::StaleGeneration,
            "catalog affected set changed; preview again",
        );
    }
    Ok(preview)
}
