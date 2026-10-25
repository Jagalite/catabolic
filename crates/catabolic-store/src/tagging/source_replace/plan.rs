//! Pure assertion write ordering shared by the source owner and historical
//! verifier. Inputs describe already resolved IDs; none conveys authority.
use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Active<'a> {
    pub id: &'a str,
    pub tag_id: &'a str,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct Requested<'a> {
    pub name: &'a str,
    pub tag_id: &'a str,
    /// Lookup before this replacement, including inactive rows.
    pub existing: Option<&'a str>,
    pub insert_id: &'a str,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Write<'a> {
    Reactivate { id: &'a str, tag_id: &'a str },
    Insert { id: &'a str, tag_id: &'a str },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Counter {
    Insert,
    Advance(u64),
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Plan<'a> {
    pub deactivate: Vec<&'a str>,
    /// One instruction per sorted requested name, including alias repeats.
    pub writes: Vec<Write<'a>>,
    pub counters: BTreeMap<&'a str, Counter>,
}
fn invalid() -> Error {
    Error("invalid source assertion transition inputs".into())
}

pub(crate) fn plan<'a>(active: &[Active<'a>], requested: &[Requested<'a>]) -> Result<Plan<'a>> {
    if active.len() > 100 || requested.len() > 100 {
        return Err(invalid());
    }
    // Retained IDs are opaque legacy owner values, not public request names.
    // Callers admit their represented bytes; this ordering plan must not invent
    // a length, nonempty, UUID, or normalization policy for existing rows.
    let mut known = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let mut counters = BTreeMap::new();
    let mut deactivate = Vec::new();
    let mut previous = None;
    for row in active {
        if previous.is_some_and(|id| id >= row.id)
            || known.insert(row.tag_id, row.id).is_some()
            || !ids.insert(row.id)
        {
            return Err(invalid());
        }
        previous = Some(row.id);
        deactivate.push(row.id);
        counters.insert(row.id, Counter::Advance(1));
    }
    let mut original = BTreeMap::new();
    let mut prior_name = None;
    for row in requested {
        if row.name.is_empty()
            || row.name.len() > 100
            || prior_name.is_some_and(|name| name >= row.name)
        {
            return Err(invalid());
        }
        prior_name = Some(row.name);
        if let Some(previous) = original.insert(row.tag_id, (row.existing, row.insert_id)) {
            if previous != (row.existing, row.insert_id) {
                return Err(invalid());
            }
            continue;
        }
        match (known.get(row.tag_id), row.existing) {
            (Some(id), Some(existing)) if *id == existing => (),
            (Some(_), _) => return Err(invalid()),
            (None, Some(id)) => {
                if !ids.insert(id) {
                    return Err(invalid());
                }
                known.insert(row.tag_id, id);
            }
            (None, None) => {
                if !ids.insert(row.insert_id) {
                    return Err(invalid());
                }
            }
        }
    }
    let mut selected = BTreeSet::new();
    let mut writes = Vec::new();
    for row in requested {
        if let Some(id) = known.get(row.tag_id).copied() {
            writes.push(Write::Reactivate {
                id,
                tag_id: row.tag_id,
            });
            // First reactivation always changes active0→1. Alias repeats set
            // exactly the same active/note/timestamp and don't fire row triggers.
            if selected.insert(row.tag_id) {
                match counters.entry(id).or_insert(Counter::Advance(0)) {
                    Counter::Advance(delta) => *delta += 1,
                    Counter::Insert => return Err(invalid()),
                }
            }
        } else {
            known.insert(row.tag_id, row.insert_id);
            selected.insert(row.tag_id);
            writes.push(Write::Insert {
                id: row.insert_id,
                tag_id: row.tag_id,
            });
            counters.insert(row.insert_id, Counter::Insert);
        }
    }
    Ok(Plan {
        deactivate,
        writes,
        counters,
    })
}

/// Preserve the exact qualified owner's UUID domain and canonical encoding.
/// A stable ID does not prove that the corresponding row was ever inserted.
pub(crate) fn assertion_id(
    catalog: &uuid::Uuid,
    subject: &str,
    tag: &str,
    namespace: &str,
) -> String {
    uuid::Uuid::new_v5(
        catalog,
        catabolic_core::encode_with(
            &serde_json::json!(["metadata-tag-assertion", subject, tag, namespace]),
            false,
            true,
        )
        .as_bytes(),
    )
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_counter_matrix_and_alias_noop_order() {
        let active = [
            Active {
                id: "a",
                tag_id: "t1",
            },
            Active {
                id: "b",
                tag_id: "t2",
            },
        ];
        let requests = [
            Requested {
                name: "a",
                tag_id: "t1",
                existing: Some("a"),
                insert_id: "unused1",
            },
            Requested {
                name: "alias",
                tag_id: "t1",
                existing: Some("a"),
                insert_id: "unused1",
            },
            Requested {
                name: "inactive",
                tag_id: "t3",
                existing: Some("c"),
                insert_id: "unused3",
            },
            Requested {
                name: "new",
                tag_id: "t4",
                existing: None,
                insert_id: "d",
            },
            Requested {
                name: "newalias",
                tag_id: "t4",
                existing: None,
                insert_id: "d",
            },
        ];
        let plan = plan(&active, &requests).unwrap();
        assert_eq!(plan.deactivate, vec!["a", "b"]);
        assert_eq!(
            plan.counters,
            BTreeMap::from([
                ("a", Counter::Advance(2)),
                ("b", Counter::Advance(1)),
                ("c", Counter::Advance(1)),
                ("d", Counter::Insert)
            ])
        );
        assert_eq!(
            plan.writes,
            vec![
                Write::Reactivate {
                    id: "a",
                    tag_id: "t1"
                },
                Write::Reactivate {
                    id: "a",
                    tag_id: "t1"
                },
                Write::Reactivate {
                    id: "c",
                    tag_id: "t3"
                },
                Write::Insert {
                    id: "d",
                    tag_id: "t4"
                },
                Write::Reactivate {
                    id: "d",
                    tag_id: "t4"
                }
            ]
        );
    }
    #[test]
    fn empty_identical_and_reappearance_are_not_collapsed() {
        assert!(plan(&[], &[]).unwrap().counters.is_empty());
        let active = [Active {
            id: "a",
            tag_id: "t",
        }];
        assert_eq!(
            plan(&active, &[]).unwrap().counters["a"],
            Counter::Advance(1)
        );
        let requested = [Requested {
            name: "tag",
            tag_id: "t",
            existing: Some("a"),
            insert_id: "unused",
        }];
        assert_eq!(
            plan(&active, &requested).unwrap().counters["a"],
            Counter::Advance(2)
        );
        assert_eq!(
            plan(&[], &requested).unwrap().counters["a"],
            Counter::Advance(1)
        );
    }
    #[test]
    fn conflicting_missing_duplicate_and_unsorted_descriptions_refuse() {
        let active = [Active {
            id: "a",
            tag_id: "t",
        }];
        assert!(
            plan(
                &active,
                &[Requested {
                    name: "tag",
                    tag_id: "t",
                    existing: None,
                    insert_id: "b"
                }]
            )
            .is_err()
        );
        let requests = [
            Requested {
                name: "b",
                tag_id: "t",
                existing: Some("a"),
                insert_id: "x",
            },
            Requested {
                name: "a",
                tag_id: "t",
                existing: Some("a"),
                insert_id: "x",
            },
        ];
        assert!(plan(&active, &requests).is_err());
        let too_many = vec![
            Active {
                id: "a",
                tag_id: "t"
            };
            101
        ];
        assert!(plan(&too_many, &[]).is_err());
    }
    #[test]
    fn long_opaque_ids_preserve_effect_order_without_new_identity_policy() {
        let old_id = "a".repeat(1024);
        let tag_id = "t".repeat(2048);
        let new_id = "z".repeat(1024);
        let active = [Active {
            id: &old_id,
            tag_id: &tag_id,
        }];
        let requested = [
            Requested {
                name: "a",
                tag_id: &tag_id,
                existing: Some(&old_id),
                insert_id: &old_id,
            },
            Requested {
                name: "alias",
                tag_id: &tag_id,
                existing: Some(&old_id),
                insert_id: &old_id,
            },
            Requested {
                name: "new",
                tag_id: "new-tag",
                existing: None,
                insert_id: &new_id,
            },
        ];
        let result = plan(&active, &requested).unwrap();
        assert_eq!(result.deactivate, vec![old_id.as_str()]);
        assert_eq!(
            result.writes,
            vec![
                Write::Reactivate {
                    id: &old_id,
                    tag_id: &tag_id
                },
                Write::Reactivate {
                    id: &old_id,
                    tag_id: &tag_id
                },
                Write::Insert {
                    id: &new_id,
                    tag_id: "new-tag"
                },
            ]
        );
        assert_eq!(result.counters[old_id.as_str()], Counter::Advance(2));
        assert_eq!(result.counters[new_id.as_str()], Counter::Insert);
    }
    #[test]
    fn empty_opaque_retained_ids_preserve_deactivate_and_alias_reactivation() {
        // Native legacy tables permit empty text primary keys. The old source
        // writer addressed them exactly; new planning must not reject them.
        let active = [Active { id: "", tag_id: "" }];
        let requested = [
            Requested {
                name: "a",
                tag_id: "",
                existing: Some(""),
                insert_id: "",
            },
            Requested {
                name: "alias",
                tag_id: "",
                existing: Some(""),
                insert_id: "",
            },
        ];
        let result = plan(&active, &requested).unwrap();
        assert_eq!(result.deactivate, vec![""]);
        assert_eq!(
            result.writes,
            vec![
                Write::Reactivate { id: "", tag_id: "" },
                Write::Reactivate { id: "", tag_id: "" },
            ]
        );
        assert_eq!(result.counters[""], Counter::Advance(2));
        assert_eq!(
            plan(&[], &requested).unwrap().counters[""],
            Counter::Advance(1)
        );
    }
}
