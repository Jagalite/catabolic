//! Borrowed canonical effect hashing. No cloned aggregate Value or output buffer.
use super::*;
use sha2::{Digest, Sha256};
use std::io::Write;

struct Sink<'a, B> {
    budget: &'a mut B,
    hash: Sha256,
    bytes: usize,
}
impl<B: StructuralReadBudget> Write for Sink<'_, B> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let next = self
            .bytes
            .checked_add(bytes.len())
            .filter(|n| *n <= MAX_BYTES)
            .ok_or_else(|| std::io::Error::other("canonical effect byte bound"))?;
        // Account before hashing. Work is actual visited nodes plus emitted
        // 64-byte quanta, not an artifact of serde's punctuation write splitting.
        self.budget
            .charge(BudgetCharge {
                bytes: bytes.len(),
                work: next / 64 - self.bytes / 64,
                ..Default::default()
            })
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        self.hash.update(bytes);
        self.bytes = next;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<B: StructuralReadBudget> Sink<'_, B> {
    fn node(&mut self, depth: usize) -> Result<()> {
        if depth > 64 {
            return Err(refuse("canonical effect depth bound"));
        }
        self.budget.charge(BudgetCharge {
            values: 1,
            work: 1,
            ..Default::default()
        })
    }
    fn text(&mut self, text: &str) -> Result<()> {
        // serde writes escaping directly into the charged hash sink.
        serde_json::to_writer(self, text)?;
        Ok(())
    }
    fn key(&mut self, key: &str) -> Result<()> {
        self.budget.charge(BudgetCharge {
            work: 1,
            ..Default::default()
        })?;
        self.text(key)?;
        self.write_all(b": ")?;
        Ok(())
    }
    fn array(&mut self, values: &[Value], depth: usize) -> Result<()> {
        self.node(depth)?;
        self.write_all(b"[")?;
        for (index, value) in values.iter().enumerate() {
            if index != 0 {
                self.write_all(b", ")?;
            }
            self.value(value, depth + 1)?;
        }
        self.write_all(b"]")?;
        Ok(())
    }
    fn object(
        &mut self,
        values: &serde_json::Map<String, Value>,
        omit_digest: bool,
        depth: usize,
    ) -> Result<()> {
        self.node(depth)?;
        // Verify lexical iteration without allocation first. If feature
        // unification changes Map storage, use a charged bounded sort instead.
        let count = values.len();
        self.budget.charge(BudgetCharge {
            work: count,
            ..Default::default()
        })?;
        if values.keys().is_sorted() {
            self.write_all(b"{")?;
            let mut first = true;
            for (key, value) in values {
                if omit_digest && key == "record_digest" {
                    continue;
                }
                if !first {
                    self.write_all(b", ")?;
                }
                first = false;
                self.key(key)?;
                self.value(value, depth + 1)?;
            }
        } else {
            let bytes = count
                .checked_mul(std::mem::size_of::<&str>())
                .filter(|n| *n <= MAX_BYTES)
                .ok_or_else(|| refuse("canonical key storage bound"))?;
            let work = count
                .checked_mul((usize::BITS - count.max(1).leading_zeros()) as usize)
                .ok_or_else(|| refuse("canonical key sorting overflow"))?;
            self.budget.charge(BudgetCharge {
                bytes,
                work,
                ..Default::default()
            })?;
            let mut keys: Vec<&str> = Vec::with_capacity(count);
            for key in values.keys() {
                if !omit_digest || key != "record_digest" {
                    keys.push(key.as_str());
                }
            }
            keys.sort_unstable();
            self.write_all(b"{")?;
            for (index, key) in keys.into_iter().enumerate() {
                if index != 0 {
                    self.write_all(b", ")?;
                }
                self.key(key)?;
                self.value(&values[key], depth + 1)?;
            }
        }
        self.write_all(b"}")?;
        Ok(())
    }
    fn value(&mut self, value: &Value, depth: usize) -> Result<()> {
        match value {
            Value::Array(values) => self.array(values, depth),
            Value::Object(values) => self.object(values, false, depth),
            Value::String(text) => {
                self.node(depth)?;
                self.text(text)
            }
            Value::Number(number) => {
                self.node(depth)?;
                // Existing scalar canonical formatter remains the oracle,
                // including negative zero and Python-compatible float spelling.
                // Its bounded scratch is charged before allocating it.
                let bound = number
                    .as_str()
                    .len()
                    .checked_add(128)
                    .filter(|n| *n <= MAX_BYTES)
                    .ok_or_else(|| refuse("canonical number bound"))?;
                self.budget.charge(BudgetCharge {
                    bytes: bound,
                    work: 1,
                    ..Default::default()
                })?;
                self.write_all(encode(value).as_bytes())?;
                Ok(())
            }
            Value::Bool(value) => {
                self.node(depth)?;
                self.write_all(if *value { b"true" } else { b"false" })?;
                Ok(())
            }
            Value::Null => {
                self.node(depth)?;
                self.write_all(b"null")?;
                Ok(())
            }
        }
    }

    fn string(&mut self, value: &str, depth: usize) -> Result<()> {
        self.node(depth)?;
        self.text(value)
    }

    fn integer(&mut self, value: &impl serde::Serialize, depth: usize) -> Result<()> {
        // Only fixed i64/u64 envelope fields call this method. Their ordinary
        // decimal JSON is exactly the canonical integer domain. serde formats
        // them on its stack, without a Number/Value or owned numeric String;
        // admit the node before formatting and each byte before hashing.
        self.node(depth)?;
        serde_json::to_writer(self, value)?;
        Ok(())
    }
}

/// Borrowed descriptive input only, never a content-verification capability.
pub(in crate::catalog_source_succession) struct BorrowedContentFile<'a> {
    pub(in crate::catalog_source_succession) id: &'a str,
    pub(in crate::catalog_source_succession) path: &'a str,
    pub(in crate::catalog_source_succession) old: &'a Value,
    pub(in crate::catalog_source_succession) candidate: &'a Value,
    pub(in crate::catalog_source_succession) sha256: &'a str,
}

/// The existing descriptor commitment's exact frozen canonical byte domain,
/// without constructing its file-entry, array or envelope Value copies.
#[allow(clippy::too_many_arguments)]
pub(in crate::catalog_source_succession) fn content_digest<'a>(
    candidate_device: u64,
    candidate_inode: u64,
    candidate_root: &str,
    catalog: &str,
    files: impl ExactSizeIterator<Item = BorrowedContentFile<'a>>,
    generation: i64,
    incarnation: &str,
    old_binding: &Value,
    profile: &str,
    source: &str,
    budget: &mut impl StructuralReadBudget,
) -> Result<String> {
    let expected_files = files.len();
    if expected_files > MAX_FILES {
        return Err(refuse("descriptor commitment file bound"));
    }
    let mut sink = Sink {
        budget,
        hash: Sha256::new(),
        bytes: 0,
    };
    sink.node(0)?;
    sink.write_all(b"{")?;
    sink.key("candidate_device")?;
    sink.integer(&candidate_device, 1)?;
    sink.write_all(b", ")?;
    sink.key("candidate_inode")?;
    sink.integer(&candidate_inode, 1)?;
    sink.write_all(b", ")?;
    sink.key("candidate_root")?;
    sink.string(candidate_root, 1)?;
    sink.write_all(b", ")?;
    sink.key("catalog")?;
    sink.string(catalog, 1)?;
    sink.write_all(b", ")?;
    sink.key("files")?;
    sink.node(1)?;
    sink.write_all(b"[")?;
    let mut visited_files = 0;
    for file in files {
        if visited_files >= expected_files {
            return Err(refuse("descriptor commitment file count changed"));
        }
        if visited_files != 0 {
            sink.write_all(b", ")?;
        }
        visited_files += 1;
        sink.node(2)?;
        sink.write_all(b"{")?;
        sink.key("candidate")?;
        sink.value(file.candidate, 3)?;
        sink.write_all(b", ")?;
        sink.key("id")?;
        sink.string(file.id, 3)?;
        sink.write_all(b", ")?;
        sink.key("old")?;
        sink.value(file.old, 3)?;
        sink.write_all(b", ")?;
        sink.key("path")?;
        sink.string(file.path, 3)?;
        sink.write_all(b", ")?;
        sink.key("sha256")?;
        sink.string(file.sha256, 3)?;
        sink.write_all(b"}")?;
    }
    if visited_files != expected_files {
        return Err(refuse("descriptor commitment file count changed"));
    }
    sink.write_all(b"]")?;
    sink.write_all(b", ")?;
    sink.key("generation")?;
    sink.integer(&generation, 1)?;
    sink.write_all(b", ")?;
    sink.key("incarnation")?;
    sink.string(incarnation, 1)?;
    sink.write_all(b", ")?;
    sink.key("old_binding")?;
    sink.value(old_binding, 1)?;
    sink.write_all(b", ")?;
    sink.key("profile")?;
    sink.string(profile, 1)?;
    sink.write_all(b", ")?;
    sink.key("source")?;
    sink.string(source, 1)?;
    sink.write_all(b"}")?;
    sink.budget.charge(BudgetCharge {
        bytes: 64,
        work: 1,
        ..Default::default()
    })?;
    let mut output = String::with_capacity(64);
    use std::fmt::Write as _;
    write!(&mut output, "{:x}", sink.hash.finalize()).expect("String formatting");
    Ok(output)
}

// Only the fixed SQL producer can establish this layout; generic callers keep
// the original Value path below, including its ordered-check/sorted fallback.
pub(super) fn fixed_digest(
    row: &Value,
    files: &[FixedRow],
    contexts: &[FixedRow],
    evidence: &[FixedRow],
    heads: &[FixedRow],
    context_heads: &[FixedRow],
    scopes: &[FixedRow],
    budget: &mut impl StructuralReadBudget,
) -> Result<String> {
    let mut sink = Sink {
        budget,
        hash: Sha256::new(),
        bytes: 0,
    };
    sink.node(0)?;
    sink.write_all(b"{")?;
    for (index, (key, values)) in [
        ("context_heads", context_heads),
        ("contexts", contexts),
        ("evidence", evidence),
        ("files", files),
        ("header", &[]),
        ("heads", heads),
        ("scopes", scopes),
    ]
    .into_iter()
    .enumerate()
    {
        if index != 0 {
            sink.write_all(b", ")?;
        }
        sink.key(key)?;
        if key == "header" {
            sink.object(
                row.as_object()
                    .ok_or_else(|| refuse("permanent header absent"))?,
                true,
                1,
            )?;
        } else {
            sink.node(1)?;
            sink.write_all(b"[")?;
            for (i, row) in values.iter().enumerate() {
                if i != 0 {
                    sink.write_all(b", ")?;
                }
                sink.fixed_object(row, 2)?;
            }
            sink.write_all(b"]")?;
        }
    }
    sink.write_all(b"}")?;
    sink.budget.charge(BudgetCharge {
        bytes: 64,
        work: 1,
        ..Default::default()
    })?;
    let mut output = String::with_capacity(64);
    use std::fmt::Write as _;
    write!(&mut output, "{:x}", sink.hash.finalize()).expect("String formatting");
    Ok(output)
}
impl<B: StructuralReadBudget> Sink<'_, B> {
    fn fixed_object(&mut self, row: &FixedRow, depth: usize) -> Result<()> {
        self.node(depth)?;
        self.write_all(b"{")?;
        let mut first = true;
        for (key, value) in row.entries() {
            if !first {
                self.write_all(b", ")?;
            }
            first = false;
            self.key(key)?;
            self.value(value, depth + 1)?;
        }
        self.write_all(b"}")?;
        Ok(())
    }
}

pub(in crate::catalog_source_succession) fn digest(
    row: &Value,
    files: &[Value],
    contexts: &[Value],
    evidence: &[Value],
    heads: &[Value],
    context_heads: &[Value],
    scopes: &[Value],
    budget: &mut impl StructuralReadBudget,
) -> Result<String> {
    let header = row
        .as_object()
        .ok_or_else(|| refuse("permanent header absent"))?;
    let mut sink = Sink {
        budget,
        hash: Sha256::new(),
        bytes: 0,
    };
    sink.node(0)?;
    sink.write_all(b"{")?;
    // Exact sorted keys of the existing frozen digest domain.
    for (index, (key, values)) in [
        ("context_heads", context_heads),
        ("contexts", contexts),
        ("evidence", evidence),
        ("files", files),
        ("header", &[]),
        ("heads", heads),
        ("scopes", scopes),
    ]
    .into_iter()
    .enumerate()
    {
        if index != 0 {
            sink.write_all(b", ")?;
        }
        sink.key(key)?;
        if key == "header" {
            sink.object(header, true, 1)?;
        } else {
            sink.array(values, 1)?;
        }
    }
    sink.write_all(b"}")?;
    // Reserve the exact hex output and poll the same sticky inherited operation
    // before allocation. No fresh budget or cancellation scope is created.
    sink.budget.charge(BudgetCharge {
        bytes: 64,
        work: 1,
        ..Default::default()
    })?;
    let mut output = String::with_capacity(64);
    use std::fmt::Write as _;
    write!(&mut output, "{:x}", sink.hash.finalize()).expect("String formatting");
    Ok(output)
}

/// Exact components::revision domain after the caller removed the separate
/// ctime field. Uses the already-qualified canonical sink, not an owned clone
/// and encoded buffer. Every node and emitted byte remains charged.
pub(in crate::catalog_source_succession) fn snapshot_revision(
    snapshot: &Value,
    budget: &mut impl StructuralReadBudget,
) -> Result<String> {
    if !snapshot.is_object() || snapshot.get("ctime_ns").is_some() {
        return Err(refuse("endpoint snapshot revision domain"));
    }
    let mut sink = Sink {
        budget,
        hash: Sha256::new(),
        bytes: 0,
    };
    sink.value(snapshot, 0)?;
    sink.budget.charge(BudgetCharge {
        bytes: 64,
        work: 1,
        ..Default::default()
    })?;
    let mut output = String::with_capacity(64);
    use std::fmt::Write as _;
    write!(&mut output, "{:x}", sink.hash.finalize()).expect("String formatting");
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Meter {
        used: BudgetCharge,
        limit: Option<usize>,
        failed: bool,
    }
    impl StructuralReadBudget for Meter {
        fn charge(&mut self, charge: BudgetCharge) -> Result<()> {
            if self.failed {
                return Err(refuse("sticky test meter"));
            }
            let next = self
                .used
                .bytes
                .checked_add(charge.bytes)
                .ok_or_else(|| refuse("meter overflow"))?;
            if self.limit.is_some_and(|limit| next > limit) {
                self.failed = true;
                return Err(refuse("test byte bound"));
            }
            self.used.bytes = next;
            self.used.values += charge.values;
            self.used.work += charge.work;
            Ok(())
        }
        fn raw_json(&mut self, _: &str) -> Result<()> {
            unreachable!()
        }
    }
    fn run(value: &Value, budget: &mut impl StructuralReadBudget) -> Result<String> {
        digest(
            &json!({"record_digest":"ignored","z":value}),
            &[value.clone()],
            &[],
            &[],
            &[],
            &[],
            &[],
            budget,
        )
    }
    fn run_content(
        files: &[Value],
        binding: &Value,
        budget: &mut impl StructuralReadBudget,
    ) -> Result<String> {
        content_digest(
            u64::MAX,
            0,
            "/candidate/\"\\\nCafé😀",
            "catalog\t\u{0000}",
            files.iter().map(|file| BorrowedContentFile {
                id: file["id"].as_str().unwrap(),
                path: file["path"].as_str().unwrap(),
                old: &file["old"],
                candidate: &file["candidate"],
                sha256: file["sha256"].as_str().unwrap(),
            }),
            i64::MIN,
            "incarnation\r\n",
            binding,
            "profile é",
            "source\\\"",
            budget,
        )
    }
    fn content_oracle(files: &[Value], binding: &Value) -> String {
        encode(&json!({
            "catalog":"catalog\t\u{0000}",
            "incarnation":"incarnation\r\n",
            "generation":i64::MIN,
            "profile":"profile é",
            "source":"source\\\"",
            "old_binding":binding,
            "candidate_root":"/candidate/\"\\\nCafé😀",
            "candidate_device":u64::MAX,
            "candidate_inode":0,
            "files":files,
        }))
    }
    #[test]
    fn borrowed_descriptor_commitment_matches_frozen_encoder() {
        let binding = json!({
            "root":"/old/é😀",
            "nested":{"z":null,"a":[true,false,{},[]]},
            "record_digest":"this binding field is not omitted",
        });
        let cases = [
            json!(null),
            json!({}),
            json!([[], {}, null]),
            json!({"z":"\"\\\n\r\t\u{0000}Café😀","a":[true,false,-0.0,1e-7,1e20,i64::MAX]}),
            catabolic_core::decode_json(
                r#"{"z":1e2,"a":{"c":[],"b":{}},"n":-0.0,"big":1208925819614629174706176,"small":1e-300,"large":1e300}"#,
            ).unwrap(),
            json!({"long":"\\\"\né😀".repeat(2048)}),
        ];
        for snapshot in cases {
            let files = [
                json!({"id":"file\\\"\n😀","path":"é/one.mkv","old":snapshot,"candidate":{"snapshot":snapshot,"size":u64::MAX},"sha256":"a".repeat(64)}),
                json!({"id":"other","path":"two.mkv","old":{},"candidate":[],"sha256":"b".repeat(64)}),
            ];
            let mut budget = Meter::default();
            let expected = sha256(content_oracle(&files, &binding));
            assert_eq!(
                run_content(&files, &binding, &mut budget).unwrap(),
                expected
            );
            assert!(budget.used.values > 0 && budget.used.work > 0 && budget.used.bytes > 0);
            assert_ne!(
                run_content(
                    &[files[1].clone(), files[0].clone()],
                    &binding,
                    &mut Meter::default()
                )
                .unwrap(),
                expected,
                "file array order remains committed",
            );
        }
        assert_eq!(
            run_content(&[], &binding, &mut Meter::default()).unwrap(),
            sha256(content_oracle(&[], &binding)),
        );
        let null_binding = json!(null);
        let mut empty = Meter::default();
        run_content(&[], &null_binding, &mut empty).unwrap();
        let encoded = content_oracle(&[], &null_binding);
        assert_eq!(empty.used.values, 11);
        assert_eq!(empty.used.bytes, encoded.len() + 64);
        assert_eq!(empty.used.work, 11 + 10 + encoded.len() / 64 + 1);
    }
    #[test]
    fn borrowed_descriptor_commitment_exact_bound_is_sticky() {
        let binding = json!({"root":"/old"});
        let files = [
            json!({"id":"file","path":"one","old":{"escaped":"\"\\\n".repeat(64)},"candidate":{"number":-0.0},"sha256":"a".repeat(64)}),
        ];
        let mut measured = Meter::default();
        let expected = run_content(&files, &binding, &mut measured).unwrap();
        let mut exact = Meter {
            limit: Some(measured.used.bytes),
            ..Default::default()
        };
        assert_eq!(run_content(&files, &binding, &mut exact).unwrap(), expected);
        let mut short = Meter {
            limit: Some(measured.used.bytes - 1),
            ..Default::default()
        };
        assert!(run_content(&files, &binding, &mut short).is_err());
        assert!(short.failed);
        assert!(run_content(&[], &json!(null), &mut short).is_err());
        let mut deep = json!(null);
        for _ in 0..65 {
            deep = json!([deep]);
        }
        assert!(run_content(&[], &deep, &mut Meter::default()).is_err());
        let files = vec![files[0].clone(); MAX_FILES + 1];
        let mut oversized = Meter::default();
        assert!(run_content(&files, &binding, &mut oversized).is_err());
        assert_eq!(oversized.used, BudgetCharge::default());
    }
    #[test]
    fn borrowed_descriptor_commitment_polls_inherited_cancellation() {
        use crate::operation_budget::{BudgetLimits, OperationBudget};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let shared = OperationBudget::new(
            std::time::Duration::from_secs(30),
            BudgetLimits {
                items: 16384,
                values: 16384,
                bytes: MAX_BYTES,
                work: 16384,
            },
        )
        .unwrap();
        let _scope = shared.enter().unwrap();
        let mut budget = crate::catalog_nfo_attribution::ProofBudget::new(shared.clone());
        let checks = Arc::new(AtomicUsize::new(0));
        let signal = checks.clone();
        let binding = json!({"payload":"abcdef".repeat(1024)});
        let result = crate::embedding::with_interrupt_check(
            move || signal.fetch_add(1, Ordering::SeqCst) >= 8,
            || run_content(&[], &binding, &mut budget),
        );
        assert!(result.is_err());
        assert!(shared.failure().is_some());
        assert!(checks.load(Ordering::SeqCst) >= 9);
        assert!(
            run_content(&[], &json!(null), &mut budget).is_err(),
            "another descriptor commitment cannot reset inherited cancellation",
        );
    }
    #[test]
    fn borrowed_canonical_effect_matches_frozen_encoder() {
        let cases = [
            json!(null),
            json!({}),
            json!([[], {}, null]),
            json!({"z":"\"\\\n\r\t\u{0000}Café😀","a":[true,false,-0.0,1e-7,1e20,9223372036854775807_i64]}),
            json!({"long":"\\\"\né😀".repeat(2048)}),
            catabolic_core::decode_json("{\"z\":1,\"a\":{\"c\":[],\"b\":{}},\"n\":-0.0}").unwrap(),
        ];
        for value in cases {
            let header = json!({"record_digest":"ignored","z":value});
            let files = [value];
            let mut budget = Meter::default();
            assert_eq!(
                digest(&header, &files, &[], &[], &[], &[], &[], &mut budget).unwrap(),
                super::super::digest(&header, &files, &[], &[], &[], &[], &[]).unwrap()
            );
            assert!(budget.used.values > 0 && budget.used.work > 0 && budget.used.bytes > 0);
        }
        let a = json!({"z":1,"a":2});
        let b = catabolic_core::decode_json("{\"a\":2,\"z\":1}").unwrap();
        assert_eq!(
            run(&a, &mut Meter::default()).unwrap(),
            run(&b, &mut Meter::default()).unwrap()
        );
        assert_ne!(
            run(&json!([1, 2]), &mut Meter::default()).unwrap(),
            run(&json!([2, 1]), &mut Meter::default()).unwrap()
        );
    }
    #[test]
    fn borrowed_canonical_effect_exact_bound_is_sticky() {
        let value = json!({"escaped":"\"\\\n".repeat(64)});
        let mut measured = Meter::default();
        let expected = run(&value, &mut measured).unwrap();
        let mut exact = Meter {
            limit: Some(measured.used.bytes),
            ..Default::default()
        };
        assert_eq!(run(&value, &mut exact).unwrap(), expected);
        let mut short = Meter {
            limit: Some(measured.used.bytes - 1),
            ..Default::default()
        };
        assert!(run(&value, &mut short).is_err());
        assert!(short.failed);
        assert!(run(&json!(null), &mut short).is_err());
        assert!(
            digest(
                &json!([]),
                &[],
                &[],
                &[],
                &[],
                &[],
                &[],
                &mut Meter::default()
            )
            .is_err()
        );
        let mut deep = json!(null);
        for _ in 0..65 {
            deep = json!([deep]);
        }
        assert!(run(&deep, &mut Meter::default()).is_err());
    }
    #[test]
    fn borrowed_canonical_effect_polls_inherited_cancellation() {
        use crate::operation_budget::{BudgetLimits, OperationBudget};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let shared = OperationBudget::new(
            std::time::Duration::from_secs(30),
            BudgetLimits {
                items: 16384,
                values: 16384,
                bytes: MAX_BYTES,
                work: 16384,
            },
        )
        .unwrap();
        let _scope = shared.enter().unwrap();
        let mut budget = crate::catalog_nfo_attribution::ProofBudget::new(shared.clone());
        let checks = Arc::new(AtomicUsize::new(0));
        let signal = checks.clone();
        let result = crate::embedding::with_interrupt_check(
            move || signal.fetch_add(1, Ordering::SeqCst) >= 8,
            || run(&json!({"payload":"abcdef".repeat(1024)}), &mut budget),
        );
        assert!(result.is_err());
        assert!(shared.failure().is_some());
        assert!(checks.load(Ordering::SeqCst) >= 9);
        assert!(
            run(&json!(null), &mut budget).is_err(),
            "cancellation cannot be reset by another digest"
        );
    }
    #[test]
    fn fixed_sql_complete_digest_matches_generic_all_fields_and_omission() {
        use crate::migration::source_history_rows::{Reader, Table, tests as rows};
        let db = rows::fixture();
        let mut reader = Reader::new();
        let batches = rows::TABLES[..7]
            .iter()
            .map(|t| rows::fixed(&db, *t, &mut reader))
            .collect::<Vec<_>>();
        let old = rows::TABLES[..7]
            .iter()
            .map(|t| rows::legacy(&db, *t))
            .collect::<Vec<_>>();
        for header_index in 0..2 {
            let actual = fixed_digest(
                &old[0][header_index],
                &batches[1],
                &batches[2],
                &batches[3],
                &batches[4],
                &batches[5],
                &batches[6],
                &mut Meter::default(),
            )
            .unwrap();
            let expected = digest(
                &old[0][header_index],
                &old[1],
                &old[2],
                &old[3],
                &old[4],
                &old[5],
                &old[6],
                &mut Meter::default(),
            )
            .unwrap();
            assert_eq!(actual, expected);
            assert_eq!(
                actual,
                super::super::digest(
                    &old[0][header_index],
                    &old[1],
                    &old[2],
                    &old[3],
                    &old[4],
                    &old[5],
                    &old[6]
                )
                .unwrap()
            );
        }
        // Only this named header field is omitted. JSON payload columns remain
        // strings, including noncanonical inner JSON, never spliced fragments.
        let before = fixed_digest(
            &old[0][0],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &mut Meter::default(),
        )
        .unwrap();
        db.execute(
            "UPDATE catalog_source_successions SET record_digest='changed'",
            [],
        )
        .unwrap();
        let after = rows::legacy(&db, Table::Header);
        // Header row ordering can change with the mutated omitted field. Match
        // by the retained revision scalar rather than assuming positional order.
        let after = after
            .iter()
            .find(|r| r["revision"] == old[0][0]["revision"])
            .unwrap();
        assert_eq!(
            before,
            fixed_digest(after, &[], &[], &[], &[], &[], &[], &mut Meter::default()).unwrap()
        );
    }
}
