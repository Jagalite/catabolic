// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded dependencies of a supported Motion work graph, not a universal
//! native-catalog export, ACL or playable manifest. Library definitions are
//! auxiliary and must not be included wholesale in a work dependency digest.
//! The embedding separately certifies immutable source mapping/membership and
//! retains its writer and native owner through durable revision issuance.
mod domains;

use crate::{Error, Result, Store, catalog_projection::TrustedProjectionAuthority};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

const MAX_ROWS: usize = 10_000;
const MAX_BYTES: usize = 4 * 1024 * 1024;

/// Constructible only by the native snapshot owner. Completeness is relative to
/// the explicitly supported graph below, not every native extension/domain.
/// Never deserialize caller JSON into this proof or use it as byte authority.
#[derive(Debug, Serialize)]
pub struct CompleteWorkCapture {
    catalog_id: String,
    incarnation: String,
    generation: i64,
    profile: String,
    work_id: String,
    dependencies: Value,
    library_definitions: Vec<Value>,
}
impl CompleteWorkCapture {
    pub fn catalog_id(&self) -> &str {
        &self.catalog_id
    }
    pub fn incarnation(&self) -> &str {
        &self.incarnation
    }
    pub fn generation(&self) -> i64 {
        self.generation
    }
    pub fn profile(&self) -> &str {
        &self.profile
    }
    pub fn work_id(&self) -> &str {
        &self.work_id
    }
    pub fn dependencies(&self) -> &Value {
        &self.dependencies
    }
    pub fn library_definitions(&self) -> &[Value] {
        &self.library_definitions
    }
}

fn fail(message: impl std::fmt::Display) -> Error {
    Error(format!("complete work capture: {message}"))
}
fn text<'a>(row: &'a Value, field: &str) -> Result<&'a str> {
    row.get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| fail("missing string identity"))
}
fn integer(row: &Value, field: &str) -> Result<i64> {
    row.get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| fail("missing integer revision"))
}
fn object_json(row: &Value, field: &str) -> Result<Value> {
    let value: Value = serde_json::from_str(text(row, field)?)?;
    if !value.is_object() {
        return Err(fail("unsupported non-object JSON"));
    }
    Ok(value)
}
fn strings(rows: &[Value], field: &str) -> Result<Vec<String>> {
    rows.iter()
        .map(|row| text(row, field).map(str::to_owned))
        .collect::<Result<BTreeSet<_>>>()
        .map(|ids| ids.into_iter().collect())
}
fn encoded(ids: &[String]) -> Result<String> {
    Ok(serde_json::to_string(ids)?)
}

struct Budget {
    rows: usize,
    bytes: usize,
    deadline: Instant,
    shared: crate::operation_budget::OperationBudget,
}
impl Budget {
    fn check(&self) -> Result<()> {
        self.shared
            .check()
            .map_err(|e| fail(format!("shared native operation budget: {e:?}")))?;
        if self.rows > MAX_ROWS
            || self.bytes > MAX_BYTES
            || Instant::now() >= self.deadline
            || crate::embedding::interrupt_requested()
        {
            return Err(fail("resource budget exhausted or capture cancelled"));
        }
        Ok(())
    }
    fn account(&mut self, value: &Value) -> Result<()> {
        self.rows = self.rows.saturating_add(1);
        let bytes = serde_json::to_vec(value)?.len();
        self.bytes = self.bytes.saturating_add(bytes);
        self.shared
            .charge(crate::operation_budget::BudgetCharge {
                values: 1,
                bytes,
                ..Default::default()
            })
            .map_err(|e| fail(format!("shared native operation budget: {e:?}")))?;
        self.check()
    }
    fn rows(&mut self, db: &Connection, sql: &str, args: &[String]) -> Result<Vec<Value>> {
        self.check()?;
        let mut statement = db.prepare(sql)?;
        let names: Vec<String> = statement
            .column_names()
            .iter()
            .map(|s| (*s).into())
            .collect();
        let mut rows = statement.query(rusqlite::params_from_iter(args))?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            self.check()?;
            let mut value = Map::new();
            for (i, name) in names.iter().enumerate() {
                let raw = row.get_ref(i)?;
                if matches!(raw, rusqlite::types::ValueRef::Text(s) | rusqlite::types::ValueRef::Blob(s) if s.len() > MAX_BYTES.saturating_sub(self.bytes))
                {
                    return Err(fail("one SQL value exceeds byte budget"));
                }
                match raw {
                    rusqlite::types::ValueRef::Text(bytes) => {
                        std::str::from_utf8(bytes)
                            .map_err(|_| fail("non-UTF-8 dependency text"))?;
                    }
                    rusqlite::types::ValueRef::Blob(_) => {
                        return Err(fail("unsupported binary dependency value"));
                    }
                    rusqlite::types::ValueRef::Real(value) if !value.is_finite() => {
                        return Err(fail("non-finite dependency value"));
                    }
                    _ => {}
                }
                value.insert(name.clone(), crate::migration::value(raw));
            }
            let value = Value::Object(value);
            self.account(&value)?;
            result.push(value);
        }
        Ok(result)
    }
}

/// Capture a closed active work -> edition -> version relationship graph, all
/// edition-owned timelines, current typed version/binding heads (including
/// retired versions), complete closed segmentation contexts and retained exact
/// evidence, current metadata provenance and recorded bound-source state.
///
/// Unknown graph edges, logical merge/split redirects, identity/pin/tag policy,
/// component/file-fact extensions and cross-owner contexts refuse the whole
/// capture. Missing/replaced/unobserved files are supported recorded states, not
/// evidence of availability. No filesystem path is opened. Fixed membership is
/// based on active primary associations, never arbitrary saved SQL/path matching.
pub fn capture(
    store: &Store,
    authority: &TrustedProjectionAuthority,
    profile: &str,
    work_id: &str,
) -> Result<CompleteWorkCapture> {
    if !matches!(store.schema_version, 39 | 40)
        || !store.db.is_autocommit()
        || work_id.is_empty()
        || work_id.len() > 256
        || work_id.chars().any(char::is_control)
        || profile.is_empty()
        || profile.len() > 256
        || profile.chars().any(char::is_control)
        || authority.expected_generation < 0
    {
        return Err(fail("unsupported request/schema or ambient transaction"));
    }
    store.transaction_available()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    store.transaction(|db| {
        // Borrow the enclosing operation budget. Never replace/reset an
        // unknown SQLite progress handler: private native writers retain their
        // VM/cancellation owner through commit and search-index flushing.
        let shared=store.catalog_operation_budget()?;
        shared.check().map_err(|e|fail(format!("shared native operation budget: {e:?}")))?;
        let (catalog, incarnation, generation): (String,String,i64) = db.query_row(
            "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1", [],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        if catalog != authority.catalog_id || incarnation != authority.incarnation || generation != authority.expected_generation {
            return Err(fail("catalog/incarnation/generation changed"));
        }
        if !db.prepare("SELECT 1 FROM profiles WHERE id=?")?.exists([profile])? {
            return Err(fail("unknown native profile"));
        }
        let mut budget = Budget { rows: 0, bytes: 0, deadline, shared };
        let work = budget.rows(db, "SELECT * FROM items WHERE id=? ORDER BY id", &[work_id.into()])?;
        if work.len() != 1 || !matches!(work[0]["kind"].as_str(), Some("video" | "custom:motion_audio")) {
            return Err(fail("unsupported work owner"));
        }
        let editions = budget.rows(db, "SELECT target_id FROM item_relationships WHERE source_id=? AND kind='custom:motion_edition' AND active=1 ORDER BY target_id", &[work_id.into()])?;
        let edition_ids = strings(&editions, "target_id")?;
        let encoded_editions = encoded(&edition_ids)?;
        let versions = budget.rows(db, "SELECT DISTINCT target_id FROM item_relationships WHERE source_id IN(SELECT value FROM json_each(?)) AND kind='custom:motion_version' AND active=1 ORDER BY target_id", &[encoded_editions.clone()])?;
        let version_owner_ids = strings(&versions, "target_id")?;
        let mut owner_ids = vec![work_id.to_owned()];
        owner_ids.extend(edition_ids.iter().cloned());
        owner_ids.extend(version_owner_ids.iter().cloned());
        if owner_ids.iter().collect::<BTreeSet<_>>().len() != owner_ids.len() {
            return Err(fail("repeated graph owner"));
        }
        let encoded_owners = encoded(&owner_ids)?;
        let owner_args = [encoded_owners.clone()];
        let owners = budget.rows(db, "SELECT i.*,r.revision AS native_item_revision FROM items i JOIN catalog_entity_revisions r ON r.entity_kind='items' AND r.entity_key=json_array(i.id) AND r.deleted=0 WHERE i.id IN(SELECT value FROM json_each(?)) ORDER BY i.id", &owner_args)?;
        if owners.len() != owner_ids.len() { return Err(fail("missing graph owner/revision")); }
        for owner in &owners {
            let id = text(owner, "id")?;
            object_json(owner, "metadata")?;
            if id != work_id && owner["kind"] != if edition_ids.iter().any(|e| e == id) { "custom:motion_edition" } else { "custom:motion_version" } {
                return Err(fail("unsupported child kind"));
            }
        }
        let relationships = budget.rows(db, "SELECT * FROM item_relationships WHERE source_id IN(SELECT value FROM json_each(?1)) OR target_id IN(SELECT value FROM json_each(?1)) ORDER BY id", &owner_args)?;
        let mut graph_pairs = BTreeSet::new();
        let mut relationship_endpoints = BTreeMap::new();
        for edge in &relationships {
            let source = text(edge, "source_id")?;
            let target = text(edge, "target_id")?;
            object_json(edge, "metadata")?;
            if crate::catalog_work_relationship::decode_record(edge)?.is_some() {
                if source != work_id && target != work_id {
                    return Err(fail("work relationship incident to structural child"));
                }
                // Capture every endpoint, even on inactive edges. This is a
                // principal-independent dependency, never a visibility grant.
                for endpoint in [source, target] {
                    let rows = budget.rows(db, "SELECT i.id,i.kind,EXISTS(SELECT 1 FROM catalog_logical_retirements r WHERE r.item_id=i.id) AS retired FROM items i WHERE i.id=?", &[endpoint.into()])?;
                    let row = rows.first().ok_or_else(|| fail("missing relationship endpoint"))?;
                    if rows.len()!=1 || !matches!(row["kind"].as_str(),Some("video"|"custom:motion_audio")) || row["retired"]!=0 {
                        return Err(fail("unsupported work relationship endpoint"));
                    }
                    relationship_endpoints.insert(endpoint.to_owned(),row.clone());
                }
                continue;
            }
            let edition_edge = source == work_id && edge["kind"] == "custom:motion_edition" && edition_ids.iter().any(|v| v == target);
            let version_edge = edition_ids.iter().any(|v| v == source) && edge["kind"] == "custom:motion_version" && version_owner_ids.iter().any(|v| v == target);
            if edge["active"] != 1 || !(edition_edge || version_edge) {
                return Err(fail("unsupported incident relationship or inactive graph edge"));
            }
            if version_edge { graph_pairs.insert((source.to_owned(), target.to_owned())); }
        }
        let unsupported: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM component_occurrences WHERE item_id IN(SELECT value FROM json_each(?1)))
             OR EXISTS(SELECT 1 FROM item_workflow WHERE item_id IN(SELECT value FROM json_each(?1)))
             OR EXISTS(SELECT 1 FROM item_requirements WHERE item_id IN(SELECT value FROM json_each(?1)))
             OR EXISTS(SELECT 1 FROM item_worklog WHERE item_id IN(SELECT value FROM json_each(?1)))
             OR EXISTS(SELECT 1 FROM expected_sets WHERE collection_id IN(SELECT value FROM json_each(?1)))
             OR EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id IN(SELECT value FROM json_each(?1)))
             OR EXISTS(SELECT 1 FROM catalog_timelines WHERE edition_item_id IN(SELECT value FROM json_each(?1)) AND edition_item_id NOT IN(SELECT value FROM json_each(?2)))",
            params![encoded_owners,encoded_editions], |r| r.get(0))?;
        if unsupported { return Err(fail("unsupported owner extension/retirement domain")); }

        let mut facts = Map::new();
        facts.insert("owners".into(), json!(owners));
        facts.insert("relationships".into(), json!(relationships));
        facts.insert("relationship_endpoints".into(), json!(relationship_endpoints.into_values().collect::<Vec<_>>()));
        for (name,sql) in [
            ("catalog_metadata_subjects", "SELECT * FROM catalog_metadata_subjects WHERE item_id IN(SELECT value FROM json_each(?)) ORDER BY item_id"),
            ("catalog_metadata_sources", "SELECT * FROM catalog_metadata_sources WHERE item_id IN(SELECT value FROM json_each(?)) ORDER BY item_id,namespace,record_id"),
            ("catalog_metadata_history", "SELECT h.* FROM catalog_metadata_history h JOIN catalog_metadata_sources s ON s.item_id=h.item_id AND s.namespace=h.namespace AND s.record_id=h.record_id AND s.revision=h.revision WHERE s.item_id IN(SELECT value FROM json_each(?)) ORDER BY h.item_id,h.namespace,h.record_id,h.revision"),
            ("catalog_metadata_projection", "SELECT * FROM catalog_metadata_projection WHERE item_id IN(SELECT value FROM json_each(?)) ORDER BY item_id"),
            ("catalog_legacy_item_metadata", "SELECT * FROM catalog_legacy_item_metadata WHERE item_id IN(SELECT value FROM json_each(?)) ORDER BY item_id"),
            ("catalog_logical_retirements", "SELECT * FROM catalog_logical_retirements WHERE item_id IN(SELECT value FROM json_each(?)) ORDER BY item_id"),
        ] { facts.insert(name.into(), json!(budget.rows(db,sql,&owner_args)?)); }
        domains::capture(db, &mut budget, &mut facts, &encoded_owners, store.schema_version)?;
        let corrupt_metadata: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM catalog_metadata_sources s LEFT JOIN catalog_metadata_history h ON h.item_id=s.item_id AND h.namespace=s.namespace AND h.record_id=s.record_id AND h.revision=s.revision WHERE s.item_id IN(SELECT value FROM json_each(?1)) AND (h.item_id IS NULL OR h.source_kind IS NOT s.source_kind OR h.schema_fingerprint IS NOT s.schema_fingerprint OR h.fields_json IS NOT s.fields_json OR h.evidence_json IS NOT s.evidence_json))",
            [&encoded_owners], |r| r.get(0))?;
        let corrupt_v1: bool = if store.schema_version == 39 {
            db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_metadata_subjects s LEFT JOIN catalog_metadata_projection p ON p.item_id=s.item_id AND p.schema_fingerprint=s.schema_fingerprint WHERE s.item_id IN(SELECT value FROM json_each(?1)) AND p.item_id IS NULL) OR EXISTS(SELECT 1 FROM catalog_metadata_sources s LEFT JOIN catalog_metadata_subjects m ON m.item_id=s.item_id AND m.schema_fingerprint=s.schema_fingerprint WHERE s.item_id IN(SELECT value FROM json_each(?1)) AND m.item_id IS NULL)", [&encoded_owners], |r| r.get(0))?
        } else { false };
        if corrupt_metadata || corrupt_v1 { return Err(fail("incomplete current metadata provenance")); }

        let timelines = budget.rows(db, "SELECT * FROM catalog_timelines WHERE edition_item_id IN(SELECT value FROM json_each(?)) ORDER BY id", &[encoded_editions.clone()])?;
        let timeline_ids = strings(&timelines, "id")?;
        let encoded_timelines = encoded(&timeline_ids)?;
        // The owner side is intentionally independent of the revision join: a
        // missing current head must be an error, never a silently omitted row.
        let typed_versions = budget.rows(db, "SELECT v.* FROM catalog_versions v WHERE v.association_item_id IN(SELECT value FROM json_each(?1)) OR EXISTS(SELECT 1 FROM catalog_version_revisions r WHERE r.version_id=v.id AND r.revision=v.current_revision AND r.timeline_id IN(SELECT value FROM json_each(?2))) ORDER BY v.id", &[encoded_owners.clone(),encoded_timelines.clone()])?;
        if typed_versions.len() != version_owner_ids.len() || typed_versions.iter().any(|v| !version_owner_ids.iter().any(|id| v["association_item_id"] == *id)) {
            return Err(fail("unlinked, missing or incorrectly owned typed version"));
        }
        let version_ids = strings(&typed_versions, "id")?;
        let encoded_versions = encoded(&version_ids)?;
        let context_ids = strings(&typed_versions, "segmentation_context_id")?;
        let encoded_contexts = encoded(&context_ids)?;
        let outside: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_versions WHERE segmentation_context_id IN(SELECT value FROM json_each(?1)) AND id NOT IN(SELECT value FROM json_each(?2)))", params![encoded_contexts,encoded_versions], |r| r.get(0))?;
        if outside { return Err(fail("cross-work segmentation context is unsupported")); }
        facts.insert("timelines".into(), json!(timelines));
        facts.insert("versions".into(), json!(typed_versions));
        for (name,sql,arg) in [
            ("timeline_heads", "SELECT r.* FROM catalog_timeline_revisions r JOIN catalog_timelines t ON t.id=r.timeline_id AND t.current_revision=r.revision WHERE t.id IN(SELECT value FROM json_each(?)) ORDER BY r.timeline_id,r.revision", &encoded_timelines),
            ("referenced_timeline_revisions", "SELECT DISTINCT r.* FROM catalog_timeline_revisions r JOIN catalog_version_revisions vr ON vr.timeline_id=r.timeline_id AND vr.timeline_revision=r.revision JOIN catalog_versions v ON v.id=vr.version_id AND v.current_revision=vr.revision WHERE v.id IN(SELECT value FROM json_each(?)) ORDER BY r.timeline_id,r.revision", &encoded_versions),
            ("version_heads", "SELECT r.* FROM catalog_version_revisions r JOIN catalog_versions v ON v.id=r.version_id AND v.current_revision=r.revision WHERE v.id IN(SELECT value FROM json_each(?)) ORDER BY r.version_id,r.revision", &encoded_versions),
            ("version_retirements", "SELECT * FROM catalog_version_retirements WHERE version_id IN(SELECT value FROM json_each(?)) ORDER BY version_id", &encoded_versions),
            ("contexts", "SELECT * FROM catalog_segmentation_contexts WHERE id IN(SELECT value FROM json_each(?)) ORDER BY id", &encoded_contexts),
            ("context_heads", "SELECT r.* FROM catalog_segmentation_revisions r JOIN catalog_segmentation_contexts c ON c.id=r.context_id AND c.current_revision=r.revision WHERE c.id IN(SELECT value FROM json_each(?)) ORDER BY r.context_id,r.revision", &encoded_contexts),
            ("sets", "SELECT * FROM catalog_segmentation_sets WHERE context_id IN(SELECT value FROM json_each(?)) ORDER BY id", &encoded_contexts),
            ("members", "SELECT m.* FROM catalog_segment_members m JOIN catalog_segmentation_contexts c ON c.id=m.context_id AND c.current_revision=m.context_revision WHERE c.id IN(SELECT value FROM json_each(?)) ORDER BY m.context_id,m.context_revision,m.binding_id", &encoded_contexts),
            ("associations", "SELECT * FROM item_files WHERE item_id IN(SELECT value FROM json_each(?)) ORDER BY id", &encoded_owners),
            ("version_bindings", "SELECT m.* FROM catalog_version_bindings m JOIN catalog_versions v ON v.id=m.version_id AND v.current_revision=m.version_revision WHERE v.id IN(SELECT value FROM json_each(?)) ORDER BY m.version_id,m.version_revision,m.binding_id", &encoded_versions),
            ("binding_owners", "SELECT b.* FROM catalog_bindings b WHERE b.version_id IN(SELECT value FROM json_each(?)) ORDER BY b.id", &encoded_versions),
            ("bindings", "SELECT b.id,b.version_id,b.association_id,r.* FROM catalog_version_bindings m JOIN catalog_versions v ON v.id=m.version_id AND v.current_revision=m.version_revision JOIN catalog_bindings b ON b.id=m.binding_id AND b.version_id=v.id JOIN catalog_binding_revisions r ON r.binding_id=m.binding_id AND r.revision=m.binding_revision WHERE v.id IN(SELECT value FROM json_each(?)) ORDER BY r.binding_id,r.revision", &encoded_versions),
        ] { facts.insert(name.into(), json!(budget.rows(db,sql,&[arg.clone()])?)); }
        let array = |name: &str| -> Result<&Vec<Value>> { facts.get(name).and_then(Value::as_array).ok_or_else(|| fail("missing captured table")) };
        if array("timeline_heads")?.len() != timeline_ids.len() || array("version_heads")?.len() != version_ids.len()
            || array("contexts")?.len() != context_ids.len() || array("context_heads")?.len() != context_ids.len()
            || array("version_bindings")?.len() != array("bindings")?.len()
            || array("binding_owners")?.len() != array("bindings")?.len()
            || array("associations")?.len() != array("bindings")?.len()
        { return Err(fail("incomplete current head/binding join")); }
        let malformed: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM catalog_versions v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision LEFT JOIN catalog_timelines t ON t.id=r.timeline_id LEFT JOIN catalog_timeline_revisions tr ON tr.timeline_id=r.timeline_id AND tr.revision=r.timeline_revision WHERE v.id IN(SELECT value FROM json_each(?1)) AND (t.id IS NULL OR tr.timeline_id IS NULL OR t.edition_item_id NOT IN(SELECT value FROM json_each(?2)) OR (NOT EXISTS(SELECT 1 FROM catalog_version_retirements x WHERE x.version_id=v.id) AND t.current_revision!=r.timeline_revision)))
             OR EXISTS(SELECT 1 FROM catalog_version_retirements x JOIN catalog_versions v ON v.id=x.version_id WHERE v.id IN(SELECT value FROM json_each(?1)) AND x.version_revision!=v.current_revision)",
            params![encoded_versions,encoded_editions], |r| r.get(0))?;
        if malformed { return Err(fail("missing/stale timeline or retirement head")); }
        let timeline_by_id: BTreeMap<_,_> = array("timelines")?.iter().map(|t| Ok((text(t,"id")?,text(t,"edition_item_id")?))).collect::<Result<_>>()?;
        let version_by_id: BTreeMap<_,_> = array("versions")?.iter().map(|v| Ok((text(v,"id")?,v))).collect::<Result<_>>()?;
        let native_pairs: BTreeSet<_> = array("version_heads")?.iter().map(|r| {
            let v = version_by_id.get(text(r,"version_id")?).ok_or_else(|| fail("missing version owner"))?;
            let edition = timeline_by_id.get(text(r,"timeline_id")?).ok_or_else(|| fail("timeline outside graph"))?;
            Ok(((*edition).to_owned(),text(v,"association_item_id")?.to_owned()))
        }).collect::<Result<_>>()?;
        if native_pairs != graph_pairs { return Err(fail("native/relationship parent-child pairs differ")); }
        let bindings = array("bindings")?.clone();
        let sets = array("sets")?.clone();
        for association in array("associations")? {
            object_json(association,"metadata")?;
            let matches: Vec<_> = bindings.iter().filter(|b| b["association_id"] == association["id"]).collect();
            if matches.len() != 1 { return Err(fail("association does not have exactly one current binding")); }
            let b = matches[0];
            let v = version_by_id.get(text(b,"version_id")?).ok_or_else(|| fail("binding owner missing"))?;
            let retired = array("version_retirements")?.iter().any(|r| r["version_id"] == b["version_id"]);
            if association["item_id"] != v["association_item_id"] || association["file_id"] != b["file_id"]
                || association["part"] != b["part"] || association["role"] != "primary"
                || (!retired && association["active"] != 1)
            { return Err(fail("unsupported or mismatched current association")); }
        }
        let context_mismatch: bool = db.query_row(
            "SELECT EXISTS(SELECT b.segmentation_context_id,b.binding_id,b.binding_revision FROM catalog_live_binding_revisions b WHERE b.segmentation_context_id IN(SELECT value FROM json_each(?1)) AND b.source_kind='slice' EXCEPT SELECT m.context_id,m.binding_id,m.binding_revision FROM catalog_segment_members m JOIN catalog_segmentation_contexts c ON c.id=m.context_id AND c.current_revision=m.context_revision WHERE c.id IN(SELECT value FROM json_each(?1)))
             OR EXISTS(SELECT m.context_id,m.binding_id,m.binding_revision FROM catalog_segment_members m JOIN catalog_segmentation_contexts c ON c.id=m.context_id AND c.current_revision=m.context_revision WHERE c.id IN(SELECT value FROM json_each(?1)) EXCEPT SELECT b.segmentation_context_id,b.binding_id,b.binding_revision FROM catalog_live_binding_revisions b WHERE b.segmentation_context_id IN(SELECT value FROM json_each(?1)) AND b.source_kind='slice')
             OR EXISTS(SELECT 1 FROM catalog_segment_members m JOIN catalog_segmentation_contexts c ON c.id=m.context_id AND c.current_revision=m.context_revision LEFT JOIN catalog_segmentation_sets s ON s.id=m.set_id AND s.context_id=m.context_id LEFT JOIN catalog_binding_revisions b ON b.binding_id=m.binding_id AND b.revision=m.binding_revision WHERE c.id IN(SELECT value FROM json_each(?1)) AND (s.id IS NULL OR b.binding_id IS NULL OR b.source_kind!='slice' OR s.profile IS NOT b.profile OR s.file_id IS NOT b.file_id OR s.file_revision IS NOT b.file_revision))",
            [&encoded_contexts], |r| r.get(0))?;
        if context_mismatch { return Err(fail("incomplete/mismatched current segmentation membership")); }
        // Borrowed table indexes must not outlive the following map insertions.
        drop(version_by_id);
        drop(timeline_by_id);

        let mut evidence_ids = BTreeSet::new();
        for row in facts["version_heads"].as_array().ok_or_else(|| fail("missing version heads"))? {
            if let Some(id) = row["equivalence_evidence_id"].as_str() { evidence_ids.insert(id.to_owned()); }
        }
        for row in &bindings {
            if let Some(id) = row["normalization_evidence_id"].as_str() { evidence_ids.insert(id.to_owned()); }
        }
        let evidence_ids: Vec<_> = evidence_ids.into_iter().collect();
        let evidence = budget.rows(db, "SELECT * FROM catalog_identity_evidence WHERE id IN(SELECT value FROM json_each(?)) ORDER BY id", &[encoded(&evidence_ids)?])?;
        if evidence.len() != evidence_ids.len() { return Err(fail("missing retained identity evidence")); }
        for row in &evidence { object_json(row,"evidence_json")?; }
        for head in facts["version_heads"].as_array().ok_or_else(|| fail("missing version heads"))? {
            if let Some(id) = head["equivalence_evidence_id"].as_str() {
                if !evidence.iter().any(|e| e["id"] == id && e["basis"] == head["equivalence_basis"]) {
                    return Err(fail("equivalence evidence basis mismatch"));
                }
            } else if !head["equivalence_basis"].is_null() { return Err(fail("incomplete equivalence reference")); }
        }
        facts.insert("identity_evidence".into(),json!(evidence));
        let association_ids = strings(&bindings,"association_id")?;
        facts.insert("catalog_legacy_associations".into(),json!(budget.rows(db,"SELECT * FROM catalog_legacy_associations WHERE association_id IN(SELECT value FROM json_each(?1)) OR original_item_id IN(SELECT value FROM json_each(?2)) ORDER BY association_id",&[encoded(&association_ids)?,encoded_owners.clone()])?));

        let mut exact_keys = BTreeSet::new();
        let mut bound_files = BTreeSet::new();
        for row in bindings.iter().chain(sets.iter()) {
            let p = text(row,"profile")?;
            if p != profile { return Err(fail("cross-profile binding/set is unsupported")); }
            let file = text(row,"file_id")?;
            exact_keys.insert((p.to_owned(),file.to_owned(),text(row,"file_revision")?.to_owned()));
        }
        for row in &bindings { bound_files.insert(text(row,"file_id")?.to_owned()); }
        let mut retained = Vec::new();
        for (p,file,revision) in exact_keys {
            let rows = budget.rows(db,"SELECT * FROM catalog_exact_file_revisions WHERE profile=? AND file_id=? AND revision=? ORDER BY profile,file_id,revision",&[p,file.clone(),revision.clone()])?;
            if rows.len() != 1 { return Err(fail("missing binding/set exact snapshot")); }
            let snapshot = object_json(&rows[0],"snapshot_json")?;
            if rows[0]["revision_contract_version"] != 1 || snapshot["id"] != file || crate::components::revision(&snapshot) != revision {
                return Err(fail("retained occurrence revision invalid"));
            }
            retained.extend(rows);
        }
        let file_ids: Vec<_> = bound_files.into_iter().collect();
        let encoded_files = encoded(&file_ids)?;
        let files = budget.rows(db,"SELECT * FROM files WHERE id IN(SELECT value FROM json_each(?)) ORDER BY id",&[encoded_files.clone()])?;
        if files.len() != file_ids.len() { return Err(fail("missing bound file owner")); }
        let sources = strings(&files,"location")?;
        let encoded_sources = encoded(&sources)?;
        let unsupported_files: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM file_tags WHERE file_id IN(SELECT value FROM json_each(?1)))
             OR EXISTS(SELECT 1 FROM catalog_file_tag_suppressions WHERE file_id IN(SELECT value FROM json_each(?1)))
             OR EXISTS(SELECT 1 FROM catalog_tag_suppression_revisions WHERE subject_type='file' AND subject_id IN(SELECT value FROM json_each(?1)))
             OR EXISTS(SELECT 1 FROM component_occurrences WHERE file_id IN(SELECT value FROM json_each(?1)))
             OR EXISTS(SELECT 1 FROM file_facts WHERE profile=?2 AND file_id IN(SELECT value FROM json_each(?1)))
             OR EXISTS(SELECT 1 FROM content_baselines WHERE profile=?2 AND file_id IN(SELECT value FROM json_each(?1)))
             OR EXISTS(SELECT 1 FROM generated_locations WHERE profile=?2 AND location IN(SELECT value FROM json_each(?3)))",
            params![encoded_files,profile,encoded_sources], |r| r.get(0))?;
        if unsupported_files { return Err(fail("unsupported physical tag/component/fact/generated-source domain")); }
        facts.insert("retained_occurrences".into(),json!(retained));
        facts.insert("files".into(),json!(files));
        for (name,sql,key) in [
            ("observations", "SELECT * FROM observations WHERE profile=?1 AND file_id IN(SELECT value FROM json_each(?2)) ORDER BY profile,file_id", &encoded_files),
            ("native_source_ctimes", "SELECT * FROM native_source_ctimes WHERE profile=?1 AND file_id IN(SELECT value FROM json_each(?2)) ORDER BY profile,file_id", &encoded_files),
            ("source_bindings", "SELECT * FROM bindings WHERE profile=?1 AND kind='source' AND owner IN(SELECT value FROM json_each(?2)) ORDER BY profile,kind,owner", &encoded_sources),
            ("source_volumes", "SELECT * FROM binding_volumes WHERE profile=?1 AND kind='source' AND owner IN(SELECT value FROM json_each(?2)) ORDER BY profile,kind,owner", &encoded_sources),
            ("source_identity_policies", "SELECT * FROM source_identity_policies WHERE profile=?1 AND location IN(SELECT value FROM json_each(?2)) ORDER BY profile,location", &encoded_sources),
            ("source_policy_history", "SELECT h.* FROM source_policy_history h JOIN source_identity_policies p ON p.profile=h.profile AND p.location=h.location AND p.revision=h.revision WHERE h.profile=?1 AND h.location IN(SELECT value FROM json_each(?2)) ORDER BY h.id", &encoded_sources),
            ("source_observation_policies", "SELECT * FROM source_observation_policies WHERE profile=?1 AND source IN(SELECT value FROM json_each(?2)) ORDER BY profile,source", &encoded_sources),
            ("source_observation_policy_history", "SELECT h.* FROM source_observation_policy_history h JOIN source_observation_policies p ON p.profile=h.profile AND p.source=h.source AND p.revision=h.revision WHERE h.profile=?1 AND h.source IN(SELECT value FROM json_each(?2)) ORDER BY h.profile,h.source,h.revision", &encoded_sources),
        ] { facts.insert(name.into(),json!(budget.rows(db,sql,&[profile.into(),key.clone()])?)); }
        for policy in facts["source_identity_policies"].as_array().ok_or_else(|| fail("missing source policies"))? {
            if !matches!(policy["identity_policy"].as_str(),Some("strict"|"path")) { return Err(fail("unknown source identity policy")); }
            if !policy["settings"].is_null() {
                let settings = object_json(policy,"settings")?;
                if settings.as_object().ok_or_else(|| fail("invalid settings"))?.iter().any(|(k,v)| !matches!(k.as_str(),"uuid"|"device"|"inode") || !matches!(v.as_str(),Some("check"|"skip"))) {
                    return Err(fail("unknown source identity settings"));
                }
            }
        }
        for policy in facts["source_observation_policies"].as_array().ok_or_else(|| fail("missing scan policies"))? { object_json(policy,"policy")?; }
        let missing_policy_history: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM source_identity_policies p LEFT JOIN source_policy_history h ON h.profile=p.profile AND h.location=p.location AND h.revision=p.revision WHERE p.profile=?1 AND p.location IN(SELECT value FROM json_each(?2)) AND h.id IS NULL)
             OR EXISTS(SELECT 1 FROM source_observation_policies p LEFT JOIN source_observation_policy_history h ON h.profile=p.profile AND h.source=p.source AND h.revision=p.revision WHERE p.profile=?1 AND p.source IN(SELECT value FROM json_each(?2)) AND (h.source IS NULL OR h.policy IS NOT p.policy))",
            params![profile,encoded_sources], |r| r.get(0))?;
        if missing_policy_history { return Err(fail("missing/mismatched current source policy provenance")); }
        let stale_ctime: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM native_source_ctimes c LEFT JOIN observations o ON o.profile=c.profile AND o.file_id=c.file_id WHERE c.profile=?1 AND c.file_id IN(SELECT value FROM json_each(?2)) AND (o.file_id IS NULL OR o.status!='present' OR o.size IS NOT c.size OR o.mtime_ns IS NOT c.mtime_ns OR o.device IS NOT c.device OR o.inode IS NOT c.inode))",params![profile,encoded_files],|r|r.get(0))?;
        if stale_ctime { return Err(fail("unbound source ctime companion")); }
        let mut occurrences = Vec::new();
        for file in &file_ids {
            // Native Components v1 intentionally excludes ctime from its hash;
            // this capture retains the bound ctime independently and in full.
            let current = crate::components::occurrence(db,profile,file)?;
            if !current["status"].is_null() && !matches!(current["status"].as_str(),Some("present"|"missing")) {
                return Err(fail("unknown recorded occurrence state"));
            }
            let ctime: Option<i64> = db.query_row("SELECT c.ctime_ns FROM native_source_ctimes c JOIN observations o ON o.profile=c.profile AND o.file_id=c.file_id AND o.status='present' AND o.size=c.size AND o.mtime_ns=c.mtime_ns AND o.device=c.device AND o.inode=c.inode WHERE c.profile=? AND c.file_id=?",[profile,file],|r|r.get(0)).optional()?;
            let value = json!({"profile":profile,"file_id":file,"exact_revision":crate::components::revision(&current),"current":current,"ctime_ns":ctime});
            budget.account(&value)?;
            occurrences.push(value);
        }
        facts.insert("current_occurrences".into(),json!(occurrences));
        facts.insert("profile".into(),json!(profile));
        facts.insert("supported_contract".into(),json!("motion_closed_active_graph_v1"));

        // Keep per-entity revisions, including known optional absence keys,
        // rather than using whole-catalog generation as a work revision.
        let mut revisions = BTreeMap::new();
        for (name,table,keys) in [
            ("owners","items",&["id"][..]),
            ("relationships","item_relationships",&["id"][..]),
            ("relationship_endpoints","items",&["id"][..]),
            ("catalog_metadata_subjects","catalog_metadata_subjects",&["item_id"][..]),
            ("catalog_metadata_sources","catalog_metadata_sources",&["item_id","namespace","record_id"][..]),
            ("catalog_metadata_history","catalog_metadata_history",&["item_id","namespace","record_id","revision"][..]),
            ("catalog_metadata_projection","catalog_metadata_projection",&["item_id"][..]),
            ("catalog_legacy_item_metadata","catalog_legacy_item_metadata",&["item_id"][..]),
            ("catalog_legacy_associations","catalog_legacy_associations",&["association_id"][..]),
            ("timelines","catalog_timelines",&["id"][..]),
            ("timeline_heads","catalog_timeline_revisions",&["timeline_id","revision"][..]),
            ("referenced_timeline_revisions","catalog_timeline_revisions",&["timeline_id","revision"][..]),
            ("versions","catalog_versions",&["id"][..]),
            ("version_heads","catalog_version_revisions",&["version_id","revision"][..]),
            ("version_retirements","catalog_version_retirements",&["version_id"][..]),
            ("contexts","catalog_segmentation_contexts",&["id"][..]),
            ("context_heads","catalog_segmentation_revisions",&["context_id","revision"][..]),
            ("sets","catalog_segmentation_sets",&["id"][..]),
            ("members","catalog_segment_members",&["context_id","context_revision","binding_id"][..]),
            ("associations","item_files",&["id"][..]),
            ("version_bindings","catalog_version_bindings",&["version_id","version_revision","binding_id"][..]),
            ("binding_owners","catalog_bindings",&["id"][..]),
            ("bindings","catalog_binding_revisions",&["binding_id","revision"][..]),
            ("identity_evidence","catalog_identity_evidence",&["id"][..]),
            ("retained_occurrences","catalog_exact_file_revisions",&["profile","file_id","revision"][..]),
            ("files","files",&["id"][..]),
            ("observations","observations",&["profile","file_id"][..]),
            ("native_source_ctimes","native_source_ctimes",&["profile","file_id"][..]),
            ("source_bindings","bindings",&["profile","kind","owner"][..]),
            ("source_volumes","binding_volumes",&["profile","kind","owner"][..]),
            ("source_identity_policies","source_identity_policies",&["profile","location"][..]),
            ("source_policy_history","source_policy_history",&["id"][..]),
            ("source_observation_policies","source_observation_policies",&["profile","source"][..]),
            ("source_observation_policy_history","source_observation_policy_history",&["profile","source","revision"][..]),
        ] {
            for row in facts[name].as_array().ok_or_else(|| fail("missing dependency table"))? {
                let key: Vec<Value> = keys.iter().map(|k| row.get(*k).cloned().ok_or_else(|| fail("missing dependency key"))).collect::<Result<_>>()?;
                capture_revision(db,&mut budget,&mut revisions,table,json!(key),true)?;
            }
        }
        for owner in &owner_ids {
            for table in ["catalog_metadata_subjects","catalog_metadata_projection","catalog_logical_retirements"] {
                capture_revision(db,&mut budget,&mut revisions,table,json!([owner]),false)?;
            }
        }
        for version in &version_ids { capture_revision(db,&mut budget,&mut revisions,"catalog_version_retirements",json!([version]),false)?; }
        for file in &file_ids {
            for table in ["observations","native_source_ctimes"] { capture_revision(db,&mut budget,&mut revisions,table,json!([profile,file]),false)?; }
        }
        for source in &sources {
            for table in ["bindings","binding_volumes"] { capture_revision(db,&mut budget,&mut revisions,table,json!([profile,"source",source]),false)?; }
            for table in ["source_identity_policies","source_observation_policies"] { capture_revision(db,&mut budget,&mut revisions,table,json!([profile,source]),false)?; }
        }
        domains::revisions(db, &mut budget, &facts, &mut revisions)?;
        facts.insert("entity_revisions".into(),json!(revisions.into_values().collect::<Vec<_>>()));
        // All definitions are auxiliary, including retired definitions. Their
        // descriptive source IDs are never accepted here as native authority.
        let libraries = budget.rows(db,"SELECT i.*,r.revision AS native_item_revision,l.kind AS retirement_kind,l.survivor_item_id,l.children_json,l.reference_map_json FROM items i LEFT JOIN catalog_entity_revisions r ON r.entity_kind='items' AND r.entity_key=json_array(i.id) AND r.deleted=0 LEFT JOIN catalog_logical_retirements l ON l.item_id=i.id WHERE i.kind='custom:motion_library' ORDER BY i.id",&[])?;
        for library in &libraries {
            if integer(library,"native_item_revision")? < 1 { return Err(fail("missing auxiliary library revision")); }
            object_json(library,"metadata")?;
        }
        let result = CompleteWorkCapture { catalog_id:catalog,incarnation,generation,profile:profile.into(),work_id:work_id.into(),dependencies:Value::Object(facts),library_definitions:libraries };
        if serde_json::to_vec(&result)?.len() > MAX_BYTES { return Err(fail("final byte budget exceeded")); }
        budget.check()?;
        Ok(result)
    })
}

fn capture_revision(
    db: &Connection,
    budget: &mut Budget,
    revisions: &mut BTreeMap<(String, String), Value>,
    table: &str,
    key: Value,
    required: bool,
) -> Result<()> {
    let encoded = serde_json::to_string(&key)?;
    let index = (table.to_owned(), encoded.clone());
    if revisions.contains_key(&index) {
        return Ok(());
    }
    let row: Option<(i64,i64)> = db.query_row("SELECT revision,deleted FROM catalog_entity_revisions WHERE entity_kind=? AND entity_key=?",params![table,encoded],|r| Ok((r.get(0)?,r.get(1)?))).optional()?;
    if required && !matches!(row,Some((revision,0)) if revision > 0) {
        return Err(fail("dependency entity revision missing/deleted"));
    }
    if !required && matches!(row, Some((_, 0))) {
        return Err(fail(
            "absent optional dependency has a live entity revision",
        ));
    }
    let value = match row {
        Some((revision, deleted)) => {
            json!({"entity_kind":table,"entity_key":key,"revision":revision,"deleted":deleted})
        }
        None => json!({"entity_kind":table,"entity_key":key,"revision":null,"deleted":null}),
    };
    budget.account(&value)?;
    revisions.insert(index, value);
    Ok(())
}
