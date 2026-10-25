//! One charged comparison traversal; no size-only prewalk plus opaque tree Eq.
//! Objects prove paired alignment or use a fully admitted order-independent
//! borrowed-entry fallback. No map-order invariant or owned Value/key clones.
use crate::{
    Error, Result, catalog_identity_mutation::StructuralReadBudget, operation_budget::BudgetCharge,
};
use serde_json::Value;
pub(in super::super) fn equal_value(
    a: &Value,
    b: &Value,
    budget: &mut impl StructuralReadBudget,
) -> Result<bool> {
    equal_inner(a, b, budget, 0)
}
pub(in super::super) fn equal_text(
    a: &str,
    b: &str,
    budget: &mut impl StructuralReadBudget,
) -> Result<bool> {
    Ok(compare_keys(a, b, budget)? == std::cmp::Ordering::Equal)
}
fn equal_inner(
    a: &Value,
    b: &Value,
    budget: &mut impl StructuralReadBudget,
    depth: usize,
) -> Result<bool> {
    use crate::catalog_identity_mutation::StructuralReadBudget;
    use crate::operation_budget::BudgetCharge;
    if depth > 64 {
        return Err(Error("scope/endpoint equality depth exceeded".into()));
    }
    let bytes = match (a, b) {
        (Value::String(a), Value::String(b)) => a.len().checked_add(b.len()),
        // The locked serde_json build enables arbitrary_precision: a Number
        // can own arbitrarily long decimal text. Compare its exact serde
        // representation, charging borrowed text before Number::eq traverses it.
        (Value::Number(a), Value::Number(b)) => a.as_str().len().checked_add(b.as_str().len()),
        _ => Some(16),
    }
    .ok_or_else(|| Error("scope/endpoint equality overflow".into()))?;
    StructuralReadBudget::charge(
        budget,
        BudgetCharge {
            bytes,
            values: 2,
            work: 2 + bytes.div_ceil(64),
            ..Default::default()
        },
    )?;
    match (a, b) {
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                return Ok(false);
            };
            for (a, b) in a.iter().zip(b) {
                if !equal_inner(a, b, budget, depth + 1)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (Value::Object(a), Value::Object(b)) => {
            // Equal paired keys establish their own alignment. Do not assume
            // serde_json iteration order or allocate/sort an already aligned
            // representation. A late order difference retains ALL work already
            // spent and falls back to the complete order-independent comparison.
            if let Some(equal) = aligned_entries(
                a.iter().map(|(key, value)| (key.as_str(), value)),
                b.iter().map(|(key, value)| (key.as_str(), value)),
                budget,
                depth,
            )? {
                return Ok(equal);
            }
            sorted_entries_equal(
                a.iter().map(|(key, value)| (key.as_str(), value)),
                b.iter().map(|(key, value)| (key.as_str(), value)),
                budget,
                depth,
            )
        }
        _ => Ok(a == b),
    }
}
type Entry<'a> = (&'a str, &'a Value);

/// A checked fast path, not an ordering invariant. Production iterators come
/// from maps with unique keys. An equal key with a different value disproves
/// equality regardless of subsequent order; a different key requires fallback.
/// The explicit iterator seam also tests order divergence when serde_json's
/// configured map implementation normalizes insertion order.
fn aligned_entries<'a, 'b>(
    left: impl ExactSizeIterator<Item = Entry<'a>>,
    right: impl ExactSizeIterator<Item = Entry<'b>>,
    budget: &mut impl StructuralReadBudget,
    depth: usize,
) -> Result<Option<bool>> {
    budget.charge(BudgetCharge {
        work: 1,
        ..Default::default()
    })?;
    if left.len() != right.len() {
        return Ok(Some(false));
    }
    for ((key, value), (other_key, other)) in left.zip(right) {
        if compare_keys(key, other_key, budget)? != std::cmp::Ordering::Equal {
            return Ok(None);
        }
        if !equal_inner(value, other, budget, depth + 1)? {
            return Ok(Some(false));
        }
    }
    Ok(Some(true))
}

/// Complete fallback for any differing iteration order. The failed aligned
/// attempt is never refunded, and each array, comparison and move is admitted.
fn sorted_entries_equal<'a, 'b>(
    a: impl ExactSizeIterator<Item = Entry<'a>>,
    b: impl ExactSizeIterator<Item = Entry<'b>>,
    budget: &mut impl StructuralReadBudget,
    depth: usize,
) -> Result<bool> {
    budget.charge(BudgetCharge {
        work: 1,
        ..Default::default()
    })?;
    if a.len() != b.len() {
        return Ok(false);
    }
    // Materialize only borrowed entries, never Value/key clones. Admit
    // BOTH arrays and the single swap temporary before either allocation.
    // No dependency on serde_json's map implementation or iteration order.
    let entries = a
        .len()
        .checked_mul(2)
        .ok_or_else(|| Error("scope/endpoint equality entry overflow".into()))?;
    let bytes = entries
        .checked_add(1)
        .and_then(|n| n.checked_mul(std::mem::size_of::<Entry<'_>>()))
        .and_then(|n| n.checked_add(2 * std::mem::size_of::<Vec<Entry<'_>>>()))
        .ok_or_else(|| Error("scope/endpoint equality entry bytes overflow".into()))?;
    StructuralReadBudget::charge(
        budget,
        BudgetCharge {
            bytes,
            values: entries,
            items: 2,
            work: entries
                .checked_add(bytes.div_ceil(64))
                .and_then(|n| n.checked_add(1))
                .ok_or_else(|| Error("scope/endpoint equality entry work overflow".into()))?,
        },
    )?;
    let mut left = Vec::with_capacity(a.len());
    let mut right = Vec::with_capacity(b.len());
    for (key, value) in a {
        left.push((key, value));
    }
    for (key, value) in b {
        right.push((key, value));
    }
    sort_entries(&mut left, budget)?;
    sort_entries(&mut right, budget)?;
    for ((key, value), (other_key, other)) in left.iter().zip(&right) {
        if compare_keys(key, other_key, budget)? != std::cmp::Ordering::Equal
            || !equal_inner(value, other, budget, depth + 1)?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn compare_keys(
    a: &str,
    b: &str,
    budget: &mut impl StructuralReadBudget,
) -> crate::Result<std::cmp::Ordering> {
    let bytes = a
        .len()
        .checked_add(b.len())
        .ok_or_else(|| Error("scope/endpoint equality key bytes overflow".into()))?;
    budget.charge(BudgetCharge {
        bytes,
        values: 2,
        work: 2 + bytes.div_ceil(64),
        ..Default::default()
    })?;
    Ok(a.cmp(b))
}

/// Fallible insertion sort has no opaque comparator or allocation. Every actual
/// comparison and three-tuple swap is admitted before execution. Its quadratic
/// worst case remains bounded by the SAME cumulative proof budget; already
/// ordered inputs still pay all order checks, without assuming they are ordered.
fn sort_entries(
    entries: &mut [Entry<'_>],
    budget: &mut impl StructuralReadBudget,
) -> crate::Result<()> {
    for i in 1..entries.len() {
        let mut j = i;
        while j > 0 {
            if compare_keys(entries[j - 1].0, entries[j].0, budget)? != std::cmp::Ordering::Greater
            {
                break;
            }
            let bytes = std::mem::size_of::<Entry<'_>>()
                .checked_mul(3)
                .ok_or_else(|| Error("scope/endpoint equality swap overflow".into()))?;
            budget.charge(BudgetCharge {
                bytes,
                work: 1 + bytes.div_ceil(64),
                ..Default::default()
            })?;
            entries.swap(j - 1, j);
            j -= 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        catalog_nfo_attribution::ProofBudget,
        operation_budget::{BudgetLimits, OperationBudget},
    };
    use serde_json::json;
    use std::time::Duration;
    fn budget(work: usize, bytes: usize) -> (OperationBudget, ProofBudget) {
        let shared = OperationBudget::new(
            Duration::from_secs(30),
            BudgetLimits {
                items: usize::MAX,
                values: usize::MAX,
                bytes,
                work,
            },
        )
        .unwrap();
        let local = ProofBudget::new(shared.clone());
        (shared, local)
    }
    #[test]
    fn aligned_scope_endpoint_equality_admits_exact_work_without_entry_arrays() {
        let value = json!({"a":1,"b":null,"c":[true,"actual text"]});
        let (oracle, mut b) = budget(usize::MAX, usize::MAX);
        assert!(equal_value(&value, &value, &mut b).unwrap());
        let usage = oracle.usage();
        assert_eq!(usage.items, 0);
        let (exact, mut b) = budget(usage.work, usage.bytes);
        assert!(equal_value(&value, &value, &mut b).unwrap());
        assert_eq!(exact.usage().work, usage.work);
        assert_eq!(exact.usage().bytes, usage.bytes);
        let (short, mut b) = budget(usage.work - 1, usize::MAX);
        assert!(equal_value(&value, &value, &mut b).is_err());
        assert!(short.failure().is_some());
        assert!(equal_value(&Value::Null, &Value::Null, &mut b).is_err());
    }
    #[test]
    fn scope_endpoint_equality_matches_exact_serde_values_and_number_text() {
        let large: Value = serde_json::from_str(&"9".repeat(1024)).unwrap();
        let another: Value = serde_json::from_str(&format!("{}8", "9".repeat(1023))).unwrap();
        let values = [
            Value::Null,
            json!(true),
            json!(false),
            json!(1),
            json!(1.0),
            json!("1"),
            json!("λ"),
            json!([1, 2]),
            json!([2, 1]),
            json!({"a":1,"b":null}),
            json!({"a":2,"b":null}),
            json!({"a":1}),
            json!({"a":1,"b":null,"c":true}),
            large,
            another,
        ];
        for left in &values {
            for right in &values {
                let (_, mut b) = budget(usize::MAX, usize::MAX);
                assert_eq!(equal_value(left, right, &mut b).unwrap(), left == right);
            }
        }
        let (shared, mut b) = budget(usize::MAX, usize::MAX);
        assert!(equal_value(&values[13], &values[13], &mut b).unwrap());
        assert_eq!(shared.usage().bytes, 2048);
        assert_eq!(shared.usage().work, 2 + 2048usize.div_ceil(64));
    }
    #[test]
    fn late_entry_order_divergence_retains_prefix_work_and_sorted_fallback() {
        // Explicit iterator inputs force actual late divergence even when the
        // locked serde map normalizes insertion order. All keys remain unique.
        let nested = json!({"z":[1,{"b":null,"a":true}],"a":0});
        let reordered = json!({"a":0,"z":[1,{"a":true,"b":null}]});
        let null = Value::Null;
        let left = [("a", &null), ("b", &nested), ("c", &null)];
        let right = [("a", &null), ("c", &null), ("b", &reordered)];
        let (shared, mut b) = budget(usize::MAX, usize::MAX);
        assert_eq!(
            aligned_entries(left.into_iter(), right.into_iter(), &mut b, 0).unwrap(),
            None
        );
        let prefix = shared.usage();
        assert_eq!(prefix.work, 10);
        assert!(sorted_entries_equal(left.into_iter(), right.into_iter(), &mut b, 0).unwrap());
        let (oracle, mut only_fallback) = budget(usize::MAX, usize::MAX);
        assert!(
            sorted_entries_equal(left.into_iter(), right.into_iter(), &mut only_fallback, 0)
                .unwrap()
        );
        assert_eq!(shared.usage().work, prefix.work + oracle.usage().work);
        assert_eq!(shared.usage().bytes, prefix.bytes + oracle.usage().bytes);
        assert_eq!(oracle.usage().items, 2);
        let changed = Value::Bool(true);
        let right = [("a", &null), ("c", &null), ("b", &changed)];
        let (_, mut b) = budget(usize::MAX, usize::MAX);
        assert!(!sorted_entries_equal(left.into_iter(), right.into_iter(), &mut b, 0).unwrap());
    }
    #[test]
    fn fallback_refusal_preserves_spent_work_and_parent_failure() {
        let null = Value::Null;
        let left = [("a", &null), ("b", &null), ("c", &null)];
        let right = [("a", &null), ("c", &null), ("b", &null)];
        let (shared, mut b) = budget(10, usize::MAX);
        assert_eq!(
            aligned_entries(left.into_iter(), right.into_iter(), &mut b, 0).unwrap(),
            None
        );
        assert_eq!(shared.usage().work, 10);
        assert!(sorted_entries_equal(left.into_iter(), right.into_iter(), &mut b, 0).is_err());
        assert!(shared.failure().is_some());
        assert!(aligned_entries(left.into_iter(), left.into_iter(), &mut b, 0).is_err());
        // Admission must refuse before allocating either borrowed-entry array.
        let (shared, mut b) = budget(usize::MAX, 0);
        assert!(sorted_entries_equal(left.into_iter(), right.into_iter(), &mut b, 0).is_err());
        assert!(shared.failure().is_some());
    }
    #[test]
    fn explicit_entry_alignment_rejects_lengths_and_matching_key_value_mismatch() {
        let null = Value::Null;
        let changed = Value::Bool(true);
        for right in [vec![("a", &null)], vec![("a", &changed), ("b", &null)]] {
            let (shared, mut b) = budget(usize::MAX, usize::MAX);
            let left = [("a", &null), ("b", &null)];
            assert_eq!(
                aligned_entries(left.into_iter(), right.into_iter(), &mut b, 0).unwrap(),
                Some(false)
            );
            assert_eq!(shared.usage().items, 0);
        }
    }
    #[test]
    fn equality_refuses_beyond_existing_decoded_json_depth() {
        let mut value = Value::Null;
        for _ in 0..66 {
            value = Value::Array(vec![value]);
        }
        let (_, mut b) = budget(usize::MAX, usize::MAX);
        assert_eq!(
            equal_value(&value, &value, &mut b).unwrap_err().to_string(),
            "scope/endpoint equality depth exceeded"
        );
    }
}
