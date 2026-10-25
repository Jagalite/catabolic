//! Storage admission for real clones and moved fixed effect rows. Sorted keys
//! charge comparisons, growth and shifts explicitly. Canonical encoding and
//! hashing remain distinct operations with their own accounting.
use super::*;
use serde_json::Value;

const MAX_KEYS: usize = 16_384;
/// Private sorted set. Each insertion admits actual binary comparisons,
/// geometric allocation and element shifts before performing them. Insertion
/// may still be quadratic overall; the inherited work bound governs that cost.
#[derive(Debug, Default, PartialEq, Eq)]
#[cfg_attr(test, derive(Clone))]
pub(super) struct KeySet(Vec<(String, String)>);
impl KeySet {
    pub(super) fn new() -> Self {
        Self::default()
    }
    pub(super) fn len(&self) -> usize {
        self.0.len()
    }
}
impl IntoIterator for KeySet {
    type Item = (String, String);
    type IntoIter = std::vec::IntoIter<Self::Item>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

fn compare_key(
    left: &(String, String),
    right: &(String, String),
    budget: &mut impl StructuralReadBudget,
) -> Result<std::cmp::Ordering> {
    // Bound the actual compared prefix, including the terminator/length test,
    // before each byte comparison. No fixed UUID or short-string assumption.
    for (a, b) in [(&left.0, &right.0), (&left.1, &right.1)] {
        let bytes = a
            .len()
            .min(b.len())
            .checked_add(1)
            .ok_or_else(|| refuse("effect key comparison size overflow"))?;
        budget.charge(BudgetCharge {
            work: 1 + bytes.div_ceil(64),
            ..Default::default()
        })?;
        let order = a.cmp(b);
        if order != std::cmp::Ordering::Equal {
            return Ok(order);
        }
    }
    Ok(std::cmp::Ordering::Equal)
}

fn insert_key(
    keys: &mut KeySet,
    pair: (String, String),
    budget: &mut impl StructuralReadBudget,
) -> Result<()> {
    let (mut low, mut high) = (0usize, keys.len());
    while low < high {
        let mid = low + (high - low) / 2;
        match compare_key(&keys.0[mid], &pair, budget)? {
            std::cmp::Ordering::Less => low = mid + 1,
            std::cmp::Ordering::Greater => high = mid,
            std::cmp::Ordering::Equal => return Ok(()),
        }
    }
    if keys.len() >= MAX_KEYS {
        return Err(refuse("complete effect key set exceeds bound"));
    }
    let size = std::mem::size_of::<(String, String)>();
    let shift = (keys.len() - low)
        .checked_mul(size)
        .ok_or_else(|| refuse("effect key shift size overflow"))?;
    let grow = keys.len() == keys.0.capacity();
    let capacity = if grow {
        keys.0
            .capacity()
            .checked_mul(2)
            .map(|n| n.max(4).min(MAX_KEYS))
            .ok_or_else(|| refuse("effect key capacity overflow"))?
    } else {
        keys.0.capacity()
    };
    let bytes = if grow {
        capacity
            .checked_mul(size)
            .filter(|n| *n <= MAX_BYTES)
            .ok_or_else(|| refuse("effect key capacity bound"))?
    } else {
        0
    };
    let growth_work = if grow {
        1 + keys
            .len()
            .checked_mul(size)
            .ok_or_else(|| refuse("effect key relocation size overflow"))?
            .div_ceil(64)
    } else {
        0
    };
    // Every requested new allocation is charged while the old allocation is
    // still retained/charged, including the simultaneous growth peak. This is
    // requested storage accounting, not a total allocator/RSS guarantee.
    budget.charge(BudgetCharge {
        bytes,
        work: growth_work
            .checked_add(1)
            .and_then(|n| n.checked_add(shift.div_ceil(64)))
            .ok_or_else(|| refuse("effect key insertion work overflow"))?,
        ..Default::default()
    })?;
    if grow {
        keys.0
            .try_reserve_exact(capacity - keys.len())
            .map_err(|_| refuse("effect key allocation failed"))?;
    }
    keys.0.insert(low, pair);
    Ok(())
}

pub(super) fn copy_key(
    keys: &mut KeySet,
    kind: &str,
    key: &str,
    budget: &mut impl StructuralReadBudget,
) -> Result<()> {
    let text = kind
        .len()
        .checked_add(key.len())
        .ok_or_else(|| refuse("effect key size overflow"))?;
    budget.charge(BudgetCharge {
        bytes: Some(text)
            .filter(|n| *n <= MAX_BYTES)
            .ok_or_else(|| refuse("effect key allocation bound"))?,
        values: 2,
        work: 1 + text.div_ceil(64),
        ..Default::default()
    })?;
    insert_key(keys, (kind.to_owned(), key.to_owned()), budget)
}

pub(super) fn key_parts(
    keys: &mut KeySet,
    kind: &str,
    parts: &[&str],
    budget: &mut impl StructuralReadBudget,
) -> Result<()> {
    // Every reviewed entity/collection key has at most three string fields.
    // Reserve worst-case JSON escaping before serialization, not afterwards.
    if parts.len() > 3 {
        return Err(refuse("effect key shape exceeds bound"));
    }
    let mut bound = 2usize + parts.len().saturating_sub(1);
    for part in parts {
        bound = part
            .len()
            .checked_mul(6)
            .and_then(|n| n.checked_add(2))
            .and_then(|n| bound.checked_add(n))
            .ok_or_else(|| refuse("effect key encoding overflow"))?;
    }
    let bytes = bound
        .checked_add(kind.len())
        .filter(|n| *n <= MAX_BYTES)
        .ok_or_else(|| refuse("effect key allocation bound"))?;
    budget.charge(BudgetCharge {
        bytes,
        values: 2,
        // Encoding and the owned Vec-to-String UTF-8 validation are distinct
        // byte traversals; both precede set comparisons and insertion.
        work: 1 + parts.len() + 2 * bound.div_ceil(64) + kind.len().div_ceil(64),
        ..Default::default()
    })?;
    let mut encoded = Vec::with_capacity(bound);
    serde_json::to_writer(&mut encoded, parts)?;
    let key = String::from_utf8(encoded).map_err(|_| refuse("encoded effect key is not UTF-8"))?;
    insert_key(keys, (kind.to_owned(), key), budget)
}

pub(super) fn effect_rows(
    count: usize,
    budget: &mut impl StructuralReadBudget,
) -> Result<Vec<Value>> {
    if count > MAX_KEYS {
        return Err(refuse("effect row count exceeds bound"));
    }
    budget.charge(BudgetCharge {
        bytes: count
            .checked_mul(std::mem::size_of::<Value>())
            .ok_or_else(|| refuse("effect row vector allocation overflow"))?,
        work: 1,
        ..Default::default()
    })?;
    Ok(Vec::with_capacity(count))
}

/// Fixed compiled constructors only: seven head fields or six scope fields.
/// One bounded construction unit covers those constant-size map insertions;
/// variable key parsing, set insertion and SQL reads are separate operations.
#[allow(clippy::too_many_arguments)]
pub(super) fn effect_row(
    profile: &str,
    source: &str,
    succession: i64,
    kind: String,
    key: String,
    revision: i64,
    deleted: Option<i64>,
    budget: &mut impl StructuralReadBudget,
) -> Result<Value> {
    let names: &[&str] = if deleted.is_some() {
        &[
            "profile",
            "source_id",
            "succession_revision",
            "entity_kind",
            "entity_key",
            "revision",
            "deleted",
        ]
    } else {
        &[
            "profile",
            "source_id",
            "succession_revision",
            "scope_kind",
            "scope_key",
            "revision",
        ]
    };
    let copied = profile
        .len()
        .checked_add(source.len())
        .ok_or_else(|| refuse("effect row text overflow"))?;
    let bytes = copied
        .checked_add(names.iter().map(|n| n.len()).sum::<usize>())
        // Conservative map storage, including small-map minimum allocation,
        // and at most three formatted i64 Number Strings (32 bytes each).
        .and_then(|n| n.checked_add(names.len() * 1024 + 3 * 32))
        .filter(|n| *n <= MAX_BYTES)
        .ok_or_else(|| refuse("effect row allocation bound"))?;
    budget.charge(BudgetCharge {
        bytes,
        values: 1 + 2 * names.len(),
        work: 1 + copied.div_ceil(64),
        ..Default::default()
    })?;
    let mut row = serde_json::Map::new();
    row.insert(names[0].to_owned(), Value::String(profile.to_owned()));
    row.insert(names[1].to_owned(), Value::String(source.to_owned()));
    row.insert(names[2].to_owned(), Value::from(succession));
    row.insert(names[3].to_owned(), Value::String(kind));
    row.insert(names[4].to_owned(), Value::String(key));
    row.insert(names[5].to_owned(), Value::from(revision));
    if let Some(deleted) = deleted {
        row.insert(names[6].to_owned(), Value::from(deleted));
    }
    Ok(Value::Object(row))
}

pub(super) fn clone_value(value: &Value, budget: &mut impl StructuralReadBudget) -> Result<Value> {
    fn visit(value: &Value, depth: usize, budget: &mut impl StructuralReadBudget) -> Result<()> {
        if depth > 64 {
            return Err(refuse("source clone depth exceeds bound"));
        }
        let text = match value {
            Value::String(s) => s.len(),
            // arbitrary_precision Number owns its original decimal String.
            Value::Number(n) => n.as_str().len(),
            _ => 0,
        };
        budget.charge(BudgetCharge {
            bytes: std::mem::size_of::<Value>()
                .checked_add(text)
                .filter(|n| *n <= MAX_BYTES)
                .ok_or_else(|| refuse("source clone storage overflow"))?,
            values: 1,
            work: 2 + text.div_ceil(64),
            ..Default::default()
        })?;
        match value {
            Value::Array(values) => {
                // Each visited child reserves its actual Vec element slot.
                for value in values {
                    visit(value, depth + 1, budget)?;
                }
            }
            Value::Object(values) => {
                for (key, value) in values {
                    budget.charge(BudgetCharge {
                        // Use the same conservative 1KiB node allowance as the
                        // inherited offline allocator, including a small map's
                        // minimum node allocation. Child Value slots are also
                        // reserved by visit; this is not a total-RSS guarantee.
                        bytes: key
                            .len()
                            .checked_add(1024)
                            .filter(|n| *n <= MAX_BYTES)
                            .ok_or_else(|| refuse("source clone key storage overflow"))?,
                        values: 1,
                        work: 2 + key.len().div_ceil(64),
                        ..Default::default()
                    })?;
                    visit(value, depth + 1, budget)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    visit(value, 0, budget)?;
    // No clone or serialized representation is allocated before complete
    // admission. Sticky inherited refusal prevents reaching this expression.
    Ok(value.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Meter {
        bytes: usize,
        values: usize,
        work: usize,
        limit: Option<usize>,
        work_limit: Option<usize>,
        failed: bool,
    }
    impl StructuralReadBudget for Meter {
        fn raw_json(&mut self, _: &str) -> Result<()> {
            Err(refuse("allocation test does not admit JSON parsing"))
        }
        fn charge(&mut self, charge: BudgetCharge) -> Result<()> {
            if self.failed
                || self
                    .work_limit
                    .is_some_and(|limit| self.work + charge.work > limit)
                || self
                    .limit
                    .is_some_and(|limit| self.bytes + charge.bytes > limit)
            {
                self.failed = true;
                return Err(refuse("clone admission refused"));
            }
            self.bytes += charge.bytes;
            self.values += charge.values;
            self.work += charge.work;
            Ok(())
        }
    }
    #[test]
    fn effect_keys_match_compact_oracle_order_and_admit_before_insert() {
        let mut actual = KeySet::new();
        let mut expected = BTreeSet::new();
        let mut meter = Meter::default();
        for (kind, parts) in [
            ("files", vec!["z\n\"雪"]),
            ("bindings", vec!["default", "source", "α"]),
            ("files", vec!["a"]),
            ("files", vec!["a"]),
        ] {
            key_parts(&mut actual, kind, &parts, &mut meter).unwrap();
            expected.insert((
                kind.to_owned(),
                serde_json::to_string(&serde_json::json!(parts)).unwrap(),
            ));
        }
        assert_eq!(actual.0.iter().cloned().collect::<BTreeSet<_>>(), expected);
        let copied = ("items", "[\"item\"]");
        copy_key(&mut actual, copied.0, copied.1, &mut meter).unwrap();
        expected.insert((copied.0.to_owned(), copied.1.to_owned()));
        assert_eq!(actual.0.iter().cloned().collect::<BTreeSet<_>>(), expected);
        let before = actual.clone();
        let mut refused = Meter {
            limit: Some(0),
            ..Default::default()
        };
        assert!(key_parts(&mut actual, "files", &["new"], &mut refused).is_err());
        assert_eq!(actual, before);
        assert!(copy_key(&mut actual, "items", "[]", &mut refused).is_err());
        assert_eq!(actual, before);
        let mut short = Meter::default();
        let mut long = Meter::default();
        compare_key(
            &("x".into(), "a".into()),
            &("x".into(), "b".into()),
            &mut short,
        )
        .unwrap();
        compare_key(
            &("x".into(), "a".repeat(4096)),
            &("x".into(), "a".repeat(4096)),
            &mut long,
        )
        .unwrap();
        assert!(long.work > short.work);
    }

    #[test]
    fn effect_key_set_reverse_prefix_growth_and_refusal_match_tree() {
        let mut keys = KeySet::new();
        let mut expected = BTreeSet::new();
        let mut meter = Meter::default();
        for index in (0..40).rev().chain(0..40) {
            let key = format!("{}-{index:03}", "shared-prefix".repeat(32));
            copy_key(&mut keys, "items", &key, &mut meter).unwrap();
            expected.insert(("items".to_owned(), key));
            assert_eq!(keys.0.iter().cloned().collect::<BTreeSet<_>>(), expected);
            assert!(keys.0.windows(2).all(|pair| pair[0] < pair[1]));
        }
        let before = keys.clone();
        let capacity = keys.0.capacity();
        let mut denied = Meter {
            work_limit: Some(0),
            ..Default::default()
        };
        assert!(insert_key(&mut keys, ("items".into(), "earlier".into()), &mut denied).is_err());
        assert_eq!(keys, before);
        assert_eq!(keys.0.capacity(), capacity);
        let mut empty = KeySet::new();
        let mut no_allocation = Meter {
            limit: Some(0),
            ..Default::default()
        };
        assert!(insert_key(&mut empty, ("items".into(), "a".into()), &mut no_allocation).is_err());
        assert_eq!(empty.len(), 0);
        assert_eq!(empty.0.capacity(), 0);
        // Refusal after the exact comparisons, but before an element shift,
        // leaves both membership and capacity unchanged.
        let mut shift = KeySet(Vec::with_capacity(4));
        shift.0.push(("items".into(), "b".into()));
        let before = shift.clone();
        let mut denied = Meter {
            work_limit: Some(4),
            ..Default::default()
        };
        assert!(insert_key(&mut shift, ("items".into(), "a".into()), &mut denied).is_err());
        assert_eq!(shift, before);
        assert_eq!(shift.0.capacity(), 4);
        let mut full = KeySet(
            (0..MAX_KEYS)
                .map(|i| ("items".into(), format!("{i:05}")))
                .collect(),
        );
        let mut meter = Meter::default();
        assert!(insert_key(&mut full, ("items".into(), "00000".into()), &mut meter).is_ok());
        assert!(insert_key(&mut full, ("items".into(), "new".into()), &mut meter).is_err());
        assert_eq!(full.len(), MAX_KEYS);
        // Exact geometric requested allocations retain the prior admitted
        // capacity while requesting the next allocation; no accounting refund.
        let mut growth = KeySet::new();
        let mut meter = Meter::default();
        for i in 0..5 {
            insert_key(&mut growth, ("items".into(), i.to_string()), &mut meter).unwrap();
        }
        assert_eq!(
            meter.bytes,
            (4 + 8) * std::mem::size_of::<(String, String)>()
        );
    }

    #[test]
    fn effect_rows_move_strings_and_match_existing_canonical_domain() {
        for deleted in [None, Some(0), Some(1)] {
            let kind = "file_identity".to_owned();
            let key = "[\"escaped\\n雪\"]".to_owned();
            let kind_ptr = kind.as_ptr();
            let key_ptr = key.as_ptr();
            let expected = if let Some(deleted) = deleted {
                serde_json::json!({"profile":"p", "source_id":"s", "succession_revision":2,
                    "entity_kind":kind, "entity_key":key, "revision":9, "deleted":deleted})
            } else {
                serde_json::json!({"profile":"p", "source_id":"s", "succession_revision":2,
                    "scope_kind":kind, "scope_key":key, "revision":9})
            };
            let mut meter = Meter::default();
            let actual = effect_row("p", "s", 2, kind, key, 9, deleted, &mut meter).unwrap();
            assert_eq!(actual, expected);
            assert_eq!(
                catabolic_core::encode(&actual),
                catabolic_core::encode(&expected)
            );
            let (kind_name, key_name) = if deleted.is_some() {
                ("entity_kind", "entity_key")
            } else {
                ("scope_kind", "scope_key")
            };
            assert_eq!(actual[kind_name].as_str().unwrap().as_ptr(), kind_ptr);
            assert_eq!(actual[key_name].as_str().unwrap().as_ptr(), key_ptr);
            assert_eq!(meter.values, 1 + 2 * if deleted.is_some() { 7 } else { 6 });
            let mut exact = Meter {
                limit: Some(meter.bytes),
                ..Default::default()
            };
            assert!(
                effect_row(
                    "p",
                    "s",
                    2,
                    "file_identity".into(),
                    "[\"escaped\\n雪\"]".into(),
                    9,
                    deleted,
                    &mut exact
                )
                .is_ok()
            );
            let mut short = Meter {
                limit: Some(meter.bytes - 1),
                ..Default::default()
            };
            assert!(
                effect_row(
                    "p",
                    "s",
                    2,
                    "file_identity".into(),
                    "[\"escaped\\n雪\"]".into(),
                    9,
                    deleted,
                    &mut short
                )
                .is_err()
            );
        }
        let mut meter = Meter::default();
        let rows = effect_rows(7, &mut meter).unwrap();
        assert!(rows.is_empty());
        assert_eq!(rows.capacity(), 7);
        assert_eq!(meter.bytes, 7 * std::mem::size_of::<Value>());
        assert!(effect_rows(MAX_KEYS + 1, &mut meter).is_err());
    }

    #[test]
    fn raw_clone_counts_storage_and_preserves_arbitrary_precision() {
        let value: Value = serde_json::from_str(
            r#"{"a":[true,null,123456789012345678901234567890],"escaped":"\n\"雪"}"#,
        )
        .unwrap();
        let mut meter = Meter::default();
        let cloned = clone_value(&value, &mut meter).unwrap();
        assert_eq!(cloned, value);
        assert_eq!(
            catabolic_core::encode(&cloned),
            catabolic_core::encode(&value)
        );
        assert_eq!(meter.values, 8); // Six Values and two owned map keys.
        assert_eq!(
            meter.bytes,
            6 * std::mem::size_of::<Value>() + 2 * 1024 + 8 + 30 + 5
        );
        assert_eq!(meter.work, 20); // Both traversals and four copied text spans.
        assert_ne!(
            cloned["escaped"].as_str().unwrap().as_ptr(),
            value["escaped"].as_str().unwrap().as_ptr()
        );
    }
    #[test]
    fn raw_clone_admission_is_exact_cumulative_and_depth_bounded() {
        let value = serde_json::json!({"s": "\\\n雪".repeat(100), "v": []});
        let mut measured = Meter::default();
        clone_value(&value, &mut measured).unwrap();
        let mut exact = Meter {
            limit: Some(measured.bytes),
            ..Default::default()
        };
        clone_value(&value, &mut exact).unwrap();
        assert!(clone_value(&value, &mut exact).is_err());
        assert!(clone_value(&Value::Null, &mut exact).is_err());
        let mut short = Meter {
            limit: Some(measured.bytes - 1),
            ..Default::default()
        };
        assert!(clone_value(&value, &mut short).is_err());
        let mut deep = Value::Null;
        for _ in 0..65 {
            deep = Value::Array(vec![deep]);
        }
        assert!(clone_value(&deep, &mut Meter::default()).is_err());
    }
}
