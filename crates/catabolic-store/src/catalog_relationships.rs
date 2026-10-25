// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Narrow database-only owner for explicit, non-reusable custom relationships.
//! Only the durable command dispatcher calls this owner, inside its receipt
//! transaction. Authority is independent trusted host material, never JSON.
use crate::{
    Error, Store,
    catalog_commands::{CommandStoreError, CommandStoreErrorCode},
    operation_budget::{BudgetCharge, OperationBudget},
};
use catabolic_core::catalog_command::CommandEnvelope;
use catabolic_core::catalog_relationship::{
    MAX_RELATIONSHIP_GRAPH_BYTES, MAX_RELATIONSHIP_GRAPH_EDGES, RELATIONSHIP_EDGE_OVERHEAD,
    RelationshipValidationError, validate_create_graph,
};
pub use catabolic_core::catalog_relationship::{
    RelationshipEdge, RelationshipKindRule, RelationshipMutation,
};
use rusqlite::{Connection, OptionalExtension, params, types::Value as SqlValue};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

/// Independently granted exact persisted edge and revision. No item-membership,
/// requested ID, receipt, metadata, or kind by itself supplies this grant.
#[derive(Clone, Debug)]
pub struct AuthorizedRelationshipDelete {
    pub expected_revision: i64,
    pub edge: RelationshipEdge,
}
#[derive(Clone, Debug)]
pub struct RelationshipAuthority {
    pub host_namespace: String,
    pub create_edges: BTreeSet<RelationshipEdge>,
    pub kind_rules: BTreeMap<String, RelationshipKindRule>,
    pub delete_edges: BTreeMap<String, AuthorizedRelationshipDelete>,
}

type Result<T> = std::result::Result<T, CommandStoreError>;
fn failure(code: CommandStoreErrorCode, message: impl Into<String>) -> CommandStoreError {
    CommandStoreError {
        code,
        error: Error(message.into()),
    }
}
fn conflict(message: &str) -> CommandStoreError {
    failure(CommandStoreErrorCode::OperationConflict, message)
}
fn denied() -> CommandStoreError {
    failure(
        CommandStoreErrorCode::AuthorityDenied,
        "catalog_relationship_authority_denied",
    )
}
fn model_error(error: RelationshipValidationError) -> CommandStoreError {
    failure(
        match error {
            RelationshipValidationError::InvalidInput | RelationshipValidationError::SelfEdge => {
                CommandStoreErrorCode::InvalidOperation
            }
            _ => CommandStoreErrorCode::OperationConflict,
        },
        error.to_string(),
    )
}

/// No VM handler replacement on a caller-owned Store. Every query is checked
/// before and after evaluation; a late result cannot authorize a mutation.
/// Private writers additionally retain their existing VM interruption owner.
struct Budget {
    deadline: Instant,
    shared: Option<OperationBudget>,
    rows: usize,
    bytes: usize,
}
impl Budget {
    fn new(store: &Store) -> Result<Self> {
        let shared = if store.schema_version >= 35 {
            Some(store.catalog_operation_budget()?)
        } else {
            OperationBudget::current()
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        Ok(Self {
            deadline,
            shared,
            rows: 0,
            bytes: 0,
        })
    }
    fn check(&self) -> Result<()> {
        if Instant::now() >= self.deadline {
            return Err(conflict("catalog_relationship_deadline"));
        }
        if let Some(shared) = &self.shared {
            shared
                .check()
                .map_err(|error| conflict(&format!("catalog_relationship_budget:{error:?}")))?;
        }
        Ok(())
    }
    fn charge(&mut self, bytes: usize) -> Result<()> {
        self.check()?;
        self.rows = self.rows.saturating_add(1);
        self.bytes = self.bytes.saturating_add(bytes);
        if self.rows > MAX_RELATIONSHIP_GRAPH_EDGES || self.bytes > MAX_RELATIONSHIP_GRAPH_BYTES {
            return Err(conflict("catalog_relationship_graph_budget"));
        }
        if let Some(shared) = &self.shared {
            shared
                .charge(BudgetCharge {
                    values: 1,
                    bytes,
                    ..Default::default()
                })
                .map_err(|error| conflict(&format!("catalog_relationship_budget:{error:?}")))?;
        }
        Ok(())
    }
}
fn live_endpoint(db: &Connection, id: &str) -> Result<()> {
    if !db.prepare(
        "SELECT 1 FROM catalog_live_items i JOIN catalog_entity_revisions r ON r.entity_kind='items' AND r.entity_key=json_array(i.id) AND r.deleted=0 WHERE i.id=?",
    )?.exists([id])? {
        return Err(conflict("catalog_relationship_endpoint_not_live"));
    }
    Ok(())
}

/// Exhaust the unfiltered active requested-kind graph. No endpoint/grant joins:
/// an unauthorized or retired intermediate node must not hide a cycle.
fn complete_graph(
    db: &Connection,
    kind: &str,
    budget: &mut Budget,
) -> Result<Vec<RelationshipEdge>> {
    budget.check()?;
    let mut statement = db.prepare(
        "SELECT source_id,target_id FROM item_relationships WHERE active=1 AND kind=? LIMIT 10001",
    )?;
    let mut rows = statement.query([kind])?;
    let mut graph = Vec::new();
    while let Some(row) = rows.next()? {
        let source = row
            .get_ref(0)?
            .as_str()
            .map_err(|_| conflict("catalog_relationship_invalid_graph_row"))?;
        let target = row
            .get_ref(1)?
            .as_str()
            .map_err(|_| conflict("catalog_relationship_invalid_graph_row"))?;
        // Charge borrowed SQLite bytes before copying them into owned strings.
        budget.charge(
            source
                .len()
                .saturating_add(target.len())
                .saturating_add(kind.len())
                .saturating_add(RELATIONSHIP_EDGE_OVERHEAD),
        )?;
        graph.push(RelationshipEdge {
            source: source.into(),
            target: target.into(),
            kind: kind.into(),
        });
    }
    // Reaching EOF within the hard limit is the completeness witness.
    budget.check()?;
    Ok(graph)
}

struct StoredRelationship {
    edge: RelationshipEdge,
    revision: i64,
    position: Option<i64>,
    metadata_text: String,
    metadata: Value,
    active: i64,
}
fn stored(db: &Connection, id: &str, budget: &mut Budget) -> Result<StoredRelationship> {
    let mut statement = db.prepare(
        "SELECT e.source_id,e.target_id,e.kind,e.position,e.metadata,e.active,r.revision FROM item_relationships e JOIN catalog_entity_revisions r ON r.entity_kind='item_relationships' AND r.entity_key=json_array(e.id) AND r.deleted=0 WHERE e.id=?",
    )?;
    let mut rows = statement.query([id])?;
    let row = rows
        .next()?
        .ok_or_else(|| conflict("catalog_relationship_missing"))?;
    let source = row
        .get_ref(0)?
        .as_str()
        .map_err(|_| conflict("catalog_relationship_invalid_row"))?;
    let target = row
        .get_ref(1)?
        .as_str()
        .map_err(|_| conflict("catalog_relationship_invalid_row"))?;
    let kind = row
        .get_ref(2)?
        .as_str()
        .map_err(|_| conflict("catalog_relationship_invalid_row"))?;
    let metadata = row
        .get_ref(4)?
        .as_str()
        .map_err(|_| conflict("catalog_relationship_invalid_row"))?;
    budget.charge(
        source
            .len()
            .saturating_add(target.len())
            .saturating_add(kind.len())
            .saturating_add(metadata.len())
            .saturating_add(RELATIONSHIP_EDGE_OVERHEAD),
    )?;
    let value: Value = serde_json::from_str(metadata)
        .map_err(|error| failure(CommandStoreErrorCode::StoreFailure, error.to_string()))?;
    if !value.is_object() {
        return Err(conflict("catalog_relationship_invalid_metadata"));
    }
    Ok(StoredRelationship {
        edge: RelationshipEdge {
            source: source.into(),
            target: target.into(),
            kind: kind.into(),
        },
        revision: row.get(6)?,
        position: row.get(3)?,
        metadata_text: metadata.into(),
        metadata: value,
        active: row.get(5)?,
    })
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}
/// Inspect actual native FK declarations rather than relying on today's lack
/// of references. A future CASCADE/SET NULL must never broaden this mutation.
fn refuse_dependencies(
    db: &Connection,
    id: &str,
    edge: &StoredRelationship,
    budget: &mut Budget,
) -> Result<()> {
    budget.check()?;
    let mut statement = db.prepare(
        "SELECT m.name,f.id,f.\"from\",f.\"to\" FROM sqlite_schema m,pragma_foreign_key_list(m.name) f WHERE m.type='table' AND f.\"table\"='item_relationships' COLLATE NOCASE ORDER BY m.name,f.id,f.seq",
    )?;
    let mut rows = statement.query([])?;
    let mut constraints: BTreeMap<(String, i64), Vec<(String, String)>> = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let table: String = row.get(0)?;
        let from: String = row.get(2)?;
        let to: Option<String> = row.get(3)?;
        let to = to
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "id".into());
        budget.charge(
            table
                .len()
                .saturating_add(from.len())
                .saturating_add(to.len())
                .saturating_add(64),
        )?;
        constraints
            .entry((table, row.get(1)?))
            .or_default()
            .push((from, to));
    }
    drop(rows);
    drop(statement);
    for ((table, _), columns) in constraints {
        budget.check()?;
        let mut clauses = Vec::new();
        let mut values = Vec::new();
        for (from, to) in columns {
            let value = match to.to_ascii_lowercase().as_str() {
                "id" => SqlValue::Text(id.into()),
                "source_id" => SqlValue::Text(edge.edge.source.clone()),
                "target_id" => SqlValue::Text(edge.edge.target.clone()),
                "kind" => SqlValue::Text(edge.edge.kind.clone()),
                "position" => edge.position.map_or(SqlValue::Null, SqlValue::Integer),
                "metadata" => SqlValue::Text(edge.metadata_text.clone()),
                "active" => SqlValue::Integer(edge.active),
                _ => return Err(conflict("catalog_relationship_unsupported_dependency")),
            };
            clauses.push(format!("{} = ?", quote_identifier(&from)));
            values.push(value);
        }
        // SQL identifiers originate only in SQLite's schema, never caller SQL.
        let sql = format!(
            "SELECT 1 FROM {} WHERE {} LIMIT 1",
            quote_identifier(&table),
            clauses.join(" AND ")
        );
        if db
            .prepare(&sql)?
            .exists(rusqlite::params_from_iter(values))?
        {
            return Err(conflict("catalog_relationship_referenced"));
        }
        budget.check()?;
    }
    budget.check()
}

pub(crate) fn apply(
    store: &Store,
    command: &CommandEnvelope,
    mutation: &RelationshipMutation,
    authority: &RelationshipAuthority,
) -> Result<Value> {
    if store.db.is_autocommit() || authority.host_namespace != command.key().host_namespace() {
        return Err(denied());
    }
    mutation.validate().map_err(model_error)?;
    let mut budget = Budget::new(store)?;
    budget.check()?;
    let db = &store.db;
    match mutation {
        RelationshipMutation::Create {
            id,
            source,
            target,
            kind,
            metadata,
        } => {
            let edge = RelationshipEdge {
                source: source.clone(),
                target: target.clone(),
                kind: kind.clone(),
            };
            let rule = authority.kind_rules.get(kind).copied().ok_or_else(denied)?;
            if !authority.create_edges.contains(&edge) {
                return Err(denied());
            }
            live_endpoint(db, source)?;
            live_endpoint(db, target)?;
            if db.prepare("SELECT 1 FROM catalog_entity_revisions WHERE entity_kind='item_relationships' AND entity_key=json_array(?)")?.exists([id])?
                || db.prepare("SELECT 1 FROM item_relationships WHERE id=?")?.exists([id])? {
                return Err(conflict("catalog_relationship_id_already_used"));
            }
            // Includes inactive historical rows because the native uniqueness
            // contract is permanent until explicitly deleting the old edge.
            if db
                .prepare(
                    "SELECT 1 FROM item_relationships WHERE source_id=? AND target_id=? AND kind=?",
                )?
                .exists(params![source, target, kind])?
            {
                return Err(conflict("catalog_relationship_duplicate_edge"));
            }
            let graph = complete_graph(db, kind, &mut budget)?;
            validate_create_graph(&edge, rule, &graph).map_err(model_error)?;
            budget.check()?;
            db.execute(
                "INSERT INTO item_relationships(id,source_id,target_id,kind,position,metadata,active) VALUES(?,?,?,?,NULL,?,1)",
                params![id, source, target, kind, catabolic_core::encode(metadata)],
            )?;
            budget.check()?;
            let revision: i64 = db.query_row(
                "SELECT revision FROM catalog_entity_revisions WHERE entity_kind='item_relationships' AND entity_key=json_array(?) AND deleted=0",
                [id], |r| r.get(0),
            )?;
            budget.check()?;
            Ok(
                json!({"operation":"relationships","action":"create","relationship":{
                    "id":id,"source":source,"target":target,"kind":kind,"position":null,
                    "metadata":metadata,"active":true,"revision":revision,
                }}),
            )
        }
        RelationshipMutation::Delete {
            id,
            expected_revision,
            expected_edge,
        } => {
            let granted = authority.delete_edges.get(id).ok_or_else(denied)?;
            if granted.expected_revision != *expected_revision || granted.edge != *expected_edge {
                return Err(denied());
            }
            let existing = stored(db, id, &mut budget)?;
            if existing.revision != *expected_revision || existing.active != 1 {
                return Err(conflict("catalog_relationship_revision_conflict"));
            }
            if existing.edge != *expected_edge {
                return Err(conflict("catalog_relationship_edge_changed"));
            }
            live_endpoint(db, &existing.edge.source)?;
            live_endpoint(db, &existing.edge.target)?;
            refuse_dependencies(db, id, &existing, &mut budget)?;
            budget.check()?;
            if db.execute("DELETE FROM item_relationships WHERE id=?", [id])? != 1 {
                return Err(conflict("catalog_relationship_missing"));
            }
            budget.check()?;
            let deleted_revision: Option<i64> = db.query_row(
                "SELECT revision FROM catalog_entity_revisions WHERE entity_kind='item_relationships' AND entity_key=json_array(?) AND deleted=1",
                [id], |r| r.get(0),
            ).optional()?;
            let deleted_revision = deleted_revision
                .filter(|value| value > expected_revision)
                .ok_or_else(|| conflict("catalog_relationship_missing_tombstone"))?;
            budget.check()?;
            Ok(
                json!({"operation":"relationships","action":"delete","relationship":{
                "id":id,"source":existing.edge.source,"target":existing.edge.target,"kind":existing.edge.kind,
                "position":existing.position,"metadata":existing.metadata,"active":true,"revision":expected_revision,
            },"deleted_revision":deleted_revision}),
            )
        }
    }
}

#[cfg(test)]
#[path = "catalog_relationships_tests.rs"]
mod tests;
