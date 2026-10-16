// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Explicit logical-edit reference maps. A replacement is not a coordinate
//! transform, equivalence assertion, redirect, or permission to update a pin.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const LOGICAL_EDIT_CONTRACT_VERSION: u32 = 1;
pub const MAX_LOGICAL_REFERENCES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceKind {
    Item,
    Timeline,
    Version,
    Binding,
    Association,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactReference {
    pub kind: ReferenceKind,
    pub id: String,
    pub revision: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceReplacement {
    pub source: ExactReference,
    /// Exhaustive explicitly reviewed candidates. Never choose the first child.
    pub replacements: Vec<ExactReference>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceMap {
    pub contract_version: u32,
    /// Old IDs/revisions remain valid historical references. Consumers need a
    /// separate authorized decision before changing a saved reference.
    pub retain_historical_references: bool,
    /// Deliberately always false in v1. Even one-to-one does not imply matching
    /// timeline coordinates or equivalent media.
    pub implies_timeline_equivalence: bool,
    pub entries: Vec<ReferenceReplacement>,
}
impl ReferenceMap {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.contract_version != 1
            || !self.retain_historical_references
            || self.implies_timeline_equivalence
        {
            return Err("catalog_logical_invalid_mapping_contract");
        }
        if self.entries.is_empty() || self.entries.len() > MAX_LOGICAL_REFERENCES {
            return Err("catalog_logical_mapping_budget");
        }
        let mut sources = BTreeSet::new();
        let mut count = 0;
        for entry in &self.entries {
            validate_reference(&entry.source)?;
            if !sources.insert((entry.source.kind, &entry.source.id))
                || entry.replacements.is_empty()
            {
                return Err("catalog_logical_ambiguous_mapping");
            }
            let mut targets = BTreeSet::new();
            for target in &entry.replacements {
                validate_reference(target)?;
                if target.kind != entry.source.kind
                    || target.id == entry.source.id
                    || !targets.insert(&target.id)
                {
                    return Err("catalog_logical_invalid_mapping_target");
                }
                count += 1;
                if count > MAX_LOGICAL_REFERENCES {
                    return Err("catalog_logical_mapping_budget");
                }
            }
        }
        Ok(())
    }
}
fn validate_reference(r: &ExactReference) -> Result<(), &'static str> {
    if r.id.is_empty()
        || r.id.len() > 256
        || r.id.trim() != r.id
        || r.id.chars().any(char::is_control)
        || r.revision < 1
    {
        return Err("catalog_logical_invalid_reference");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> ReferenceMap {
        ReferenceMap {
            contract_version: 1,
            retain_historical_references: true,
            implies_timeline_equivalence: false,
            entries: vec![ReferenceReplacement {
                source: ExactReference {
                    kind: ReferenceKind::Item,
                    id: "old".into(),
                    revision: 2,
                },
                replacements: vec![
                    ExactReference {
                        kind: ReferenceKind::Item,
                        id: "left".into(),
                        revision: 1,
                    },
                    ExactReference {
                        kind: ReferenceKind::Item,
                        id: "right".into(),
                        revision: 1,
                    },
                ],
            }],
        }
    }
    #[test]
    fn split_preserves_multiple_explicit_children() {
        assert!(sample().validate().is_ok());
    }
    #[test]
    fn maps_never_assert_equivalence_or_discard_history() {
        let mut m = sample();
        m.implies_timeline_equivalence = true;
        assert!(m.validate().is_err());
        m.implies_timeline_equivalence = false;
        m.retain_historical_references = false;
        assert!(m.validate().is_err());
    }
    #[test]
    fn duplicate_sources_targets_and_wrong_kinds_fail() {
        let mut m = sample();
        m.entries.push(m.entries[0].clone());
        assert!(m.validate().is_err());
        let mut m = sample();
        m.entries[0].replacements[1] = m.entries[0].replacements[0].clone();
        assert!(m.validate().is_err());
        let mut m = sample();
        m.entries[0].replacements[0].kind = ReferenceKind::Version;
        assert!(m.validate().is_err());
    }
}
