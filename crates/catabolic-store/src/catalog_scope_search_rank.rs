// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Pure document-local ranking candidate. No matching, query parsing, storage,
//! authority, cursor, tokenization, global statistics, I/O or callbacks here.
//! Only the search owner may derive FieldHits from real authorized FTS5 matches.
//! Provisional fixed weights require external pinned-case relevance qualification.
use std::collections::BTreeSet;

pub const RANK_CONTRACT_VERSION: u32 = 1;
pub const MAX_MATCHES: usize = 10_000;
pub const MAX_ITEM_ID_BYTES: usize = 256;

/// Boolean query-hit presence in each visible indexed field. Additional matching
/// occurrences never increase a field's weight, limiting repetition effects.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FieldHits {
    pub title: bool,
    pub aliases: bool,
    pub accepted_identities: bool,
    pub effective_tags: bool,
    pub description: bool,
}
impl FieldHits {
    pub const fn any(self) -> bool {
        self.title
            || self.aliases
            || self.accepted_identities
            || self.effective_tags
            || self.description
    }
    pub const fn score(self) -> u16 {
        self.title as u16 * 100
            + self.aliases as u16 * 80
            + self.accepted_identities as u16 * 120
            + self.effective_tags as u16 * 40
            + self.description as u16 * 10
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentMatch {
    pub item_id: String,
    pub hits: FieldHits,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RankedHit {
    pub item_id: String,
    pub score: u16,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RankError {
    MatchLimit,
    InvalidItemId,
    DuplicateItemId,
    NoPositiveMatch,
}

/// Sort a complete, authorized, bounded FTS5 match set. The owner must refuse a
/// truncated/incomplete selection before this call. This function cannot turn an
/// untrusted client item list or fabricated field hits into search authority.
pub fn rank(matches: Vec<DocumentMatch>) -> Result<Vec<RankedHit>, RankError> {
    if matches.len() > MAX_MATCHES {
        return Err(RankError::MatchLimit);
    }
    let mut ids = BTreeSet::new();
    let mut result = Vec::with_capacity(matches.len());
    for document in matches {
        if document.item_id.is_empty()
            || document.item_id.len() > MAX_ITEM_ID_BYTES
            || document.item_id.trim() != document.item_id
            || document.item_id.chars().any(char::is_control)
        {
            return Err(RankError::InvalidItemId);
        }
        if !ids.insert(document.item_id.clone()) {
            return Err(RankError::DuplicateItemId);
        }
        if !document.hits.any() {
            return Err(RankError::NoPositiveMatch);
        }
        result.push(RankedHit {
            item_id: document.item_id,
            score: document.hits.score(),
        });
    }
    result.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.item_id.cmp(&b.item_id))
    });
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hit(id: &str, field: FieldHits) -> DocumentMatch {
        DocumentMatch {
            item_id: id.into(),
            hits: field,
        }
    }
    #[test]
    fn fixed_weights_have_no_corpus_or_length_argument() {
        assert_eq!(
            FieldHits {
                title: true,
                ..FieldHits::default()
            }
            .score(),
            100
        );
        assert_eq!(
            FieldHits {
                accepted_identities: true,
                ..FieldHits::default()
            }
            .score(),
            120
        );
        assert_eq!(
            FieldHits {
                title: true,
                aliases: true,
                accepted_identities: true,
                effective_tags: true,
                description: true
            }
            .score(),
            350
        );
    }
    #[test]
    fn stable_item_id_breaks_ties_independent_of_input_order() {
        let feature = FieldHits {
            title: true,
            ..FieldHits::default()
        };
        let first = rank(vec![hit("b", feature), hit("a", feature)]).unwrap();
        let second = rank(vec![hit("a", feature), hit("b", feature)]).unwrap();
        assert_eq!(first, second);
        assert_eq!(first[0].item_id, "a");
    }
    #[test]
    fn all_boolean_combinations_are_monotonic_and_bounded() {
        for bits in 1_u16..32 {
            let features = FieldHits {
                title: bits & 1 != 0,
                aliases: bits & 2 != 0,
                accepted_identities: bits & 4 != 0,
                effective_tags: bits & 8 != 0,
                description: bits & 16 != 0,
            };
            assert!(features.score() <= 350);
            assert_eq!(
                features.score(),
                (bits & 1) * 100
                    + ((bits >> 1) & 1) * 80
                    + ((bits >> 2) & 1) * 120
                    + ((bits >> 3) & 1) * 40
                    + ((bits >> 4) & 1) * 10
            );
        }
    }
    #[test]
    fn rejects_duplicate_invalid_empty_and_oversized_inputs() {
        let feature = FieldHits {
            title: true,
            ..FieldHits::default()
        };
        assert_eq!(
            rank(vec![hit("a", feature), hit("a", feature)]),
            Err(RankError::DuplicateItemId)
        );
        assert_eq!(rank(vec![hit("", feature)]), Err(RankError::InvalidItemId));
        assert_eq!(
            rank(vec![hit("a", FieldHits::default())]),
            Err(RankError::NoPositiveMatch)
        );
        assert_eq!(
            rank(vec![hit("a", feature); MAX_MATCHES + 1]),
            Err(RankError::MatchLimit)
        );
        assert!(rank(vec![]).unwrap().is_empty());
    }
}
