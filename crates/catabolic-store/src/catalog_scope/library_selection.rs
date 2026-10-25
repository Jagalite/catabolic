//! Fixed Motion library selection specification. Never parses caller SQL or
//! grants access; only authenticates the exact already-saved source selection.
use super::*;
use crate::catalog_identity_mutation::StructuralReadBudget;

const PREFIX: &str = "SELECT DISTINCT w.item_id FROM catalog_items w JOIN catalog_relationships e ON e.source_id=w.item_id AND e.kind='custom:motion_edition' AND e.active=1 JOIN catalog_relationships v ON v.source_id=e.target_id AND v.kind='custom:motion_version' AND v.active=1 JOIN catalog_item_files a ON a.item_id=v.target_id AND a.active=1 WHERE json_type(w.metadata,'$.motion_catalog')='object' AND w.item_id=json_extract(w.metadata,'$.motion_catalog.item_id') AND a.profile=";
const SUFFIX: &str = " ORDER BY w.item_id";
fn refused() -> Error {
    Error("library source selection differs".into())
}
fn quoted_len(text: &str) -> crate::Result<usize> {
    text.len()
        .checked_add(text.bytes().filter(|b| *b == b'\'').count())
        .and_then(|n| n.checked_add(2))
        .ok_or_else(refused)
}
fn quote(out: &mut String, text: &str) {
    out.push('\'');
    for part in text.split_inclusive('\'') {
        out.push_str(part);
        if part.ends_with('\'') {
            out.push('\'');
        }
    }
    out.push('\'');
}
fn expected_sql(
    profile: &str,
    sources: &[&str],
    b: &mut impl StructuralReadBudget,
) -> crate::Result<String> {
    if profile.is_empty()
        || profile.len() > 256
        || sources.len() > 100
        || sources.iter().any(|s| s.is_empty() || s.len() > 256)
    {
        return Err(refused());
    }
    // Count the actual inspected input bytes before quote-length traversal.
    let inputs = sources.iter().try_fold(profile.len(), |n, s| {
        n.checked_add(s.len()).ok_or_else(refused)
    })?;
    b.charge(BudgetCharge {
        values: sources.len() + 1,
        bytes: inputs,
        work: 1 + inputs.div_ceil(64),
        ..Default::default()
    })?;
    let mut len = PREFIX
        .len()
        .checked_add(quoted_len(profile)?)
        .and_then(|n| n.checked_add(5 + SUFFIX.len()))
        .ok_or_else(refused)?;
    if sources.is_empty() {
        len = len.checked_add(1).ok_or_else(refused)?;
    } else {
        len = len
            .checked_add("a.location IN ()".len() + sources.len() - 1)
            .ok_or_else(refused)?;
        for source in sources {
            len = len.checked_add(quoted_len(source)?).ok_or_else(refused)?;
        }
    }
    // One exact-capacity final buffer. Emission, copying and later comparison
    // are independently admitted before allocation; no intermediate quoted Vec.
    b.charge(BudgetCharge {
        bytes: len,
        work: 1 + 2 * len.div_ceil(64),
        values: 1,
        ..Default::default()
    })?;
    let mut out = String::with_capacity(len);
    out.push_str(PREFIX);
    quote(&mut out, profile);
    out.push_str(" AND ");
    if sources.is_empty() {
        out.push('0');
    } else {
        out.push_str("a.location IN (");
        for (i, source) in sources.iter().enumerate() {
            if i != 0 {
                out.push(',');
            }
            quote(&mut out, source);
        }
        out.push(')');
    }
    out.push_str(SUFFIX);
    if out.len() != len {
        return Err(refused());
    }
    Ok(out)
}
/// Read and parse only the fixed saved definition under the caller's existing
/// structural and parent budgets. No selection execution or new grant occurs.
pub(crate) fn validate_library_selection_in(
    store: &Store,
    profile: &str,
    reference: &SavedQueryRef,
    sources: &[&str],
    b: &mut impl StructuralReadBudget,
) -> crate::Result<()> {
    let expected = expected_sql(profile, sources, b)?;
    let sql = "SELECT revision,digest,definition FROM main.saved_queries WHERE id=? AND profile=?";
    b.charge(BudgetCharge {
        work: 1 + sql.len().div_ceil(64),
        ..Default::default()
    })?;
    let mut stmt = store.db.prepare(sql)?;
    let mut rows = stmt.query(params![reference.id, profile])?;
    let row = rows.next()?.ok_or_else(refused)?;
    let revision: i64 = row.get(0)?;
    let digest = row.get_ref(1)?.as_str().map_err(|_| refused())?;
    let raw = row.get_ref(2)?.as_str().map_err(|_| refused())?;
    if raw.len() > MAX_DEFINITION_BYTES {
        return Err(refused());
    }
    b.raw_json(raw)?;
    b.charge(BudgetCharge {
        bytes: digest.len() + 64,
        values: 2,
        work: 2 + raw.len().div_ceil(64),
        ..Default::default()
    })?;
    if revision != reference.revision || digest != reference.digest || sha256(raw) != digest {
        return Err(refused());
    }
    let definition: Value = serde_json::from_str(raw)?;
    let object = definition.as_object().ok_or_else(refused)?;
    let selection = definition["selection"].as_object().ok_or_else(refused)?;
    // These are the exact normalized defaults used by the existing fixed host
    // producer. Refuse broader/alternate selection syntax, even with valid hash.
    if object.len() != 5
        || definition["version"] != 1
        || definition["mode"] != "selection"
        || definition["max_ids"] != 10000
        || definition["timeout_ms"] != 5000
        || selection.len() != 3
        || definition["selection"]["language"] != "sql"
        || definition["selection"]["profile"] != profile
        || definition["selection"]["query"].as_str() != Some(expected.as_str())
        || rows.next()?.is_some()
    {
        return Err(refused());
    }
    Ok(())
}

/// Descriptive equality only, with actual private string lengths. Dependency
/// readers may construct references internally, so do not infer public-new
/// length bounds or serialize merely to measure a comparison.
pub(crate) fn equal_saved_library_reference(
    left: &SavedQueryRef,
    right: &SavedQueryRef,
    budget: &mut impl StructuralReadBudget,
) -> crate::Result<bool> {
    budget.charge(BudgetCharge {
        bytes: 16,
        values: 2,
        work: 1,
        ..Default::default()
    })?;
    if left.revision != right.revision {
        return Ok(false);
    }
    for (a, b) in [(&left.id, &right.id), (&left.digest, &right.digest)] {
        let bytes = a.len().checked_add(b.len()).ok_or_else(refused)?;
        budget.charge(BudgetCharge {
            bytes,
            values: 2,
            work: 2 + bytes.div_ceil(64),
            ..Default::default()
        })?;
        if a != b {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_reference_equality_admits_actual_private_lengths_and_fields() {
        use crate::{
            catalog_nfo_attribution::ProofBudget,
            operation_budget::{BudgetLimits, OperationBudget},
        };
        let left = SavedQueryRef {
            id: "i".repeat(1024),
            revision: 7,
            digest: "d".repeat(512),
        };
        let make_budget = || {
            let shared = OperationBudget::new(
                std::time::Duration::from_secs(30),
                BudgetLimits {
                    items: usize::MAX,
                    values: usize::MAX,
                    bytes: usize::MAX,
                    work: usize::MAX,
                },
            )
            .unwrap();
            let local = ProofBudget::new(shared.clone());
            (shared, local)
        };
        let (shared, mut budget) = make_budget();
        assert!(equal_saved_library_reference(&left, &left, &mut budget).unwrap());
        assert_eq!(shared.usage().bytes, 16 + 2 * (1024 + 512));
        assert_eq!(
            shared.usage().work,
            1 + 2 + 2048usize.div_ceil(64) + 2 + 1024usize.div_ceil(64)
        );
        for changed in [
            SavedQueryRef {
                revision: 8,
                ..left.clone()
            },
            SavedQueryRef {
                id: "other".into(),
                ..left.clone()
            },
            SavedQueryRef {
                digest: "other".into(),
                ..left.clone()
            },
        ] {
            let (_, mut budget) = make_budget();
            assert!(!equal_saved_library_reference(&left, &changed, &mut budget).unwrap());
        }
    }
    struct Counter;
    impl StructuralReadBudget for Counter {
        fn charge(&mut self, _: BudgetCharge) -> crate::Result<()> {
            Ok(())
        }
        fn raw_json(&mut self, _: &str) -> crate::Result<()> {
            Err(refused())
        }
    }
    #[test]
    fn fixed_source_sql_matches_original_host_producer_empty_ordered_and_escaped() {
        for (profile, sources) in [
            ("default", vec![]),
            ("profile'quoted", vec!["b", "a'quote", "unicode-雪"]),
            ("default", vec!["media"; 100]),
        ] {
            let quote = |v: &str| format!("'{}'", v.replace('\'', "''"));
            let old = if sources.is_empty() {
                "0".into()
            } else {
                format!(
                    "a.location IN ({})",
                    sources
                        .iter()
                        .map(|s| quote(s))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            };
            assert_eq!(
                expected_sql(profile, &sources, &mut Counter).unwrap(),
                format!("{PREFIX}{} AND {old}{SUFFIX}", quote(profile))
            );
        }
        assert!(expected_sql("default", &vec!["media"; 101], &mut Counter).is_err());
    }
    #[test]
    fn valid_saved_reference_does_not_admit_broader_or_other_profile_selection() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        crate::migration::initialize_native_at(&path, 40).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let fixed = expected_sql("default", &["media"], &mut Counter).unwrap();
        for (name, sql, admitted) in [
            ("fixed", fixed.as_str(), true),
            ("broad", "SELECT item_id FROM catalog_items", false),
        ] {
            let saved = crate::definitions::query_put(
                &store,
                "default",
                name,
                &json!({"version":1,"mode":"selection","selection":{"language":"sql","query":sql}}),
            )
            .unwrap();
            let reference = SavedQueryRef::new(
                saved["id"].as_str().unwrap().into(),
                saved["revision"].as_i64().unwrap(),
                saved["digest"].as_str().unwrap().into(),
            )
            .unwrap();
            let result = store.transaction(|_| {
                let mut budget = crate::catalog_nfo_attribution::ProofBudget::new(
                    store.catalog_operation_budget()?,
                );
                validate_library_selection_in(
                    &store,
                    "default",
                    &reference,
                    &["media"],
                    &mut budget,
                )
            });
            assert_eq!(result.is_ok(), admitted, "{result:?}");
            assert!(
                store
                    .transaction(|_| {
                        let mut b = crate::catalog_nfo_attribution::ProofBudget::new(
                            store.catalog_operation_budget()?,
                        );
                        validate_library_selection_in(
                            &store,
                            "other",
                            &reference,
                            &["media"],
                            &mut b,
                        )
                    })
                    .is_err()
            );
        }
    }
}
