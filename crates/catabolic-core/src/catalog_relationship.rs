// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Pure custom-relationship input and bounded, per-kind graph validation.
//! The storage adapter, not this model, must prove its snapshot contains every
//! active edge of the requested kind in the same transaction as the mutation.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_RELATIONSHIP_GRAPH_EDGES: usize = 10_000;
pub const MAX_RELATIONSHIP_GRAPH_BYTES: usize = 4 * 1024 * 1024;
/// Conservative retained-graph accounting in addition to UTF-8 input bytes.
pub const RELATIONSHIP_EDGE_OVERHEAD: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationshipEdge {
    pub source: String,
    pub target: String,
    pub kind: String,
}

/// Trusted host policy, never inferred from a requested kind or metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelationshipKindRule {
    Directed,
    Acyclic,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum RelationshipMutation {
    Create {
        id: String,
        source: String,
        target: String,
        kind: String,
        metadata: Value,
    },
    Delete {
        id: String,
        expected_revision: i64,
        expected_edge: RelationshipEdge,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelationshipValidationError {
    InvalidInput,
    SelfEdge,
    DuplicateEdge,
    Cycle,
    GraphBudget,
    InconsistentGraph,
}
impl std::fmt::Display for RelationshipValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "catalog_relationship_invalid_input",
            Self::SelfEdge => "catalog_relationship_self_edge",
            Self::DuplicateEdge => "catalog_relationship_duplicate_edge",
            Self::Cycle => "catalog_relationship_cycle",
            Self::GraphBudget => "catalog_relationship_graph_budget",
            Self::InconsistentGraph => "catalog_relationship_inconsistent_graph",
        })
    }
}
impl std::error::Error for RelationshipValidationError {}

pub fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.trim() == value
        && !value.chars().any(char::is_control)
}
pub fn valid_custom_kind(value: &str) -> bool {
    value.strip_prefix("custom:").is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix.len() <= 63
            && suffix.bytes().enumerate().all(|(i, byte)| {
                byte.is_ascii_lowercase()
                    || i > 0 && (byte.is_ascii_digit() || b"_.-".contains(&byte))
            })
    })
}
impl RelationshipEdge {
    pub fn validate(&self) -> Result<(), RelationshipValidationError> {
        if !valid_identifier(&self.source)
            || !valid_identifier(&self.target)
            || !valid_custom_kind(&self.kind)
        {
            return Err(RelationshipValidationError::InvalidInput);
        }
        if self.source == self.target {
            return Err(RelationshipValidationError::SelfEdge);
        }
        Ok(())
    }
    pub fn graph_bytes(&self) -> usize {
        self.source
            .len()
            .saturating_add(self.target.len())
            .saturating_add(self.kind.len())
            .saturating_add(RELATIONSHIP_EDGE_OVERHEAD)
    }
}
impl RelationshipMutation {
    pub fn validate(&self) -> Result<(), RelationshipValidationError> {
        match self {
            Self::Create {
                id,
                source,
                target,
                kind,
                metadata,
            } => {
                if !valid_identifier(id) || !metadata.is_object() {
                    return Err(RelationshipValidationError::InvalidInput);
                }
                RelationshipEdge {
                    source: source.clone(),
                    target: target.clone(),
                    kind: kind.clone(),
                }
                .validate()
            }
            Self::Delete {
                id,
                expected_revision,
                expected_edge,
            } => {
                if !valid_identifier(id) || *expected_revision < 1 {
                    return Err(RelationshipValidationError::InvalidInput);
                }
                expected_edge.validate()
            }
        }
    }
}

/// Validate the *complete* active graph for this one kind. Neither mixed-kind
/// structural rules nor authorization-filtered neighboring edges are accepted.
/// The storage owner checks its deadline before and after this bounded work.
pub fn validate_create_graph(
    candidate: &RelationshipEdge,
    rule: RelationshipKindRule,
    active_edges: &[RelationshipEdge],
) -> Result<(), RelationshipValidationError> {
    candidate.validate()?;
    if active_edges.len() > MAX_RELATIONSHIP_GRAPH_EDGES {
        return Err(RelationshipValidationError::GraphBudget);
    }
    let mut bytes = 0usize;
    let mut unique = BTreeSet::new();
    let mut adjacency: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in active_edges {
        bytes = bytes.saturating_add(edge.graph_bytes());
        if bytes > MAX_RELATIONSHIP_GRAPH_BYTES {
            return Err(RelationshipValidationError::GraphBudget);
        }
        edge.validate()?;
        if edge.kind != candidate.kind || !unique.insert((&edge.source, &edge.target)) {
            return Err(RelationshipValidationError::InconsistentGraph);
        }
        if edge == candidate {
            return Err(RelationshipValidationError::DuplicateEdge);
        }
        adjacency
            .entry(&edge.source)
            .or_default()
            .push(&edge.target);
    }
    if rule == RelationshipKindRule::Acyclic {
        // Iterative traversal is bounded by the complete graph, not call stack.
        let mut visited = BTreeSet::new();
        let mut pending = vec![candidate.target.as_str()];
        while let Some(node) = pending.pop() {
            if node == candidate.source {
                return Err(RelationshipValidationError::Cycle);
            }
            if visited.insert(node)
                && let Some(targets) = adjacency.get(node)
            {
                pending.extend(targets);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn edge(source: &str, target: &str) -> RelationshipEdge {
        RelationshipEdge {
            source: source.into(),
            target: target.into(),
            kind: "custom:contains".into(),
        }
    }
    #[test]
    fn cycles_are_per_kind_and_directed_policy_allows_cycles() {
        let graph = [edge("a", "b"), edge("b", "c")];
        assert_eq!(
            validate_create_graph(&edge("c", "a"), RelationshipKindRule::Acyclic, &graph),
            Err(RelationshipValidationError::Cycle)
        );
        assert!(
            validate_create_graph(&edge("c", "a"), RelationshipKindRule::Directed, &graph).is_ok()
        );
        let mut wrong_kind = edge("a", "b");
        wrong_kind.kind = "custom:other".into();
        assert_eq!(
            validate_create_graph(
                &edge("c", "a"),
                RelationshipKindRule::Acyclic,
                &[wrong_kind]
            ),
            Err(RelationshipValidationError::InconsistentGraph)
        );
    }
    #[test]
    fn malformed_duplicate_and_over_budget_graphs_fail_closed() {
        assert_eq!(
            edge("a", "a").validate(),
            Err(RelationshipValidationError::SelfEdge)
        );
        assert_eq!(
            validate_create_graph(
                &edge("a", "b"),
                RelationshipKindRule::Directed,
                &[edge("a", "b")]
            ),
            Err(RelationshipValidationError::DuplicateEdge)
        );
        assert_eq!(
            validate_create_graph(
                &edge("a", "b"),
                RelationshipKindRule::Directed,
                &vec![edge("b", "c"); MAX_RELATIONSHIP_GRAPH_EDGES + 1]
            ),
            Err(RelationshipValidationError::GraphBudget)
        );
        let graph: Vec<_> = (0..7000)
            .map(|i| edge(&format!("{i:0256}"), &"z".repeat(256)))
            .collect();
        assert_eq!(
            validate_create_graph(&edge("a", "b"), RelationshipKindRule::Directed, &graph),
            Err(RelationshipValidationError::GraphBudget)
        );
    }
}
