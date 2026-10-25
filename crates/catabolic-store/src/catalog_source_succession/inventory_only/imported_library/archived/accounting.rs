//! Closed borrowed accounting for derived import structs and serde_json::Value.
//! No public/generic serialization entry point. These types use derived serde;
//! Value/Number use the pinned serde_json implementation. collect_str, floats,
//! arbitrary variants and unknown lengths refuse before downstream materialization.
//! This models logical requested storage, NOT allocator overhead or peak RSS.
use super::*;
use serde::ser::{self, Serialize, SerializeMap, SerializeSeq, SerializeStruct};
use std::mem::size_of;
// Explicit new accepted-shape limits of this disconnected library-only cohort.
// They are not a claim that arbitrary/mixed Motion export parts are supported.
const DEPTH: usize = 16;
const MAP: usize = 16;
const NUMBER: &str = "$serde_json::private::Number";
// Conservative one full B-tree node per entry, not average occupancy: eleven
// key/value slots, twelve child pointers and a generously aligned node header.
// This is the fixed logical map convention, not an allocator/layout assertion.
const MAP_ENTRY: usize =
    11 * size_of::<(String, Value)>() + 12 * size_of::<usize>() + 4 * size_of::<usize>();
#[derive(Default, Clone, Copy)]
struct Cost {
    nodes: usize,
    tree: usize,
    encoded: usize,
    encoding: usize,
    work: usize,
    text: usize,
    integer: bool,
}
#[derive(Debug)]
struct Refused(String);
impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for Refused {}
impl ser::Error for Refused {
    fn custom<T: std::fmt::Display>(v: T) -> Self {
        Self(v.to_string())
    }
}
type R<T> = std::result::Result<T, Refused>;
fn err() -> Refused {
    Refused("unsupported or overflowing library accounting shape".into())
}
fn add(a: usize, b: usize) -> R<usize> {
    a.checked_add(b).ok_or_else(err)
}
fn mul(a: usize, b: usize) -> R<usize> {
    a.checked_mul(b).ok_or_else(err)
}
fn debit(b: &mut impl StructuralReadBudget, work: usize) -> R<()> {
    b.charge(BudgetCharge {
        work,
        ..Default::default()
    })
    .map_err(|e| Refused(e.to_string()))
}
// Pinned serde_json::to_string begins with a 128-byte Vec, then grows. The
// canonical encoder simultaneously pushes quoted text into a second String.
fn scalar_encoding(encoded: usize) -> R<usize> {
    add(mul(encoded, 2)?.max(128), add(mul(encoded, 2)?, 8)?)
}
struct Measure<'a, B> {
    budget: &'a mut B,
    depth: usize,
    mixed: bool,
}
impl<'a, B: StructuralReadBudget> Measure<'a, B> {
    fn leaf(self, encoded: usize, text: usize, integer: bool) -> R<Cost> {
        self.budget
            .charge(BudgetCharge {
                values: 1,
                work: 1,
                ..Default::default()
            })
            .map_err(|e| Refused(e.to_string()))?;
        Ok(Cost {
            nodes: 1,
            tree: add(size_of::<Value>(), text)?,
            encoded,
            // Quoted scalar plus growing output coexist for strings; this
            // also conservatively admits scalar format buffers for numbers.
            encoding: scalar_encoding(encoded)?,
            work: add(2, encoded.div_ceil(64))?,
            text,
            integer,
        })
    }
    fn string(self, s: &str) -> R<Cost> {
        debit(self.budget, add(1, mul(3, s.len().div_ceil(64))?)?)?;
        let mut n = 2usize;
        for c in s.chars() {
            n = add(
                n,
                match c {
                    '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
                    c if c < ' ' => 6,
                    c => c.len_utf8(),
                },
            )?;
        }
        let integer = s.len() <= 20 && (s.parse::<i64>().is_ok() || s.parse::<u64>().is_ok());
        self.leaf(n, s.len(), integer)
    }
    fn number(self, n: u64, negative: bool) -> R<Cost> {
        let mut n = n;
        let mut len = 1 + usize::from(negative);
        while n >= 10 {
            debit(self.budget, 1)?;
            len += 1;
            n /= 10;
        }
        let mut cost = self.leaf(len, 0, true)?;
        // arbitrary_precision stores integer spelling in an owned String.
        cost.tree = add(cost.tree, 32)?;
        Ok(cost)
    }
    fn container(self, n: Option<usize>, map: bool, number: bool) -> R<Container<'a, B>> {
        let n = n.ok_or_else(err)?;
        if self.depth >= DEPTH || n > MAX_BATCHES || map && n > MAP {
            return Err(err());
        }
        self.budget
            .charge(BudgetCharge {
                values: 1,
                work: 1,
                ..Default::default()
            })
            .map_err(|e| Refused(e.to_string()))?;
        Ok(Container {
            budget: self.budget,
            depth: self.depth + 1,
            mixed: self.mixed,
            expected: n,
            seen: 0,
            map,
            number,
            pending: false,
            key_max: 0,
            cost: Cost {
                nodes: 1,
                tree: size_of::<Value>(),
                encoded: 2,
                ..Default::default()
            },
        })
    }
}
struct Container<'a, B> {
    budget: &'a mut B,
    depth: usize,
    mixed: bool,
    expected: usize,
    seen: usize,
    map: bool,
    number: bool,
    pending: bool,
    key_max: usize,
    cost: Cost,
}
impl<B: StructuralReadBudget> Container<'_, B> {
    fn child<T: Serialize + ?Sized>(&mut self, v: &T) -> R<Cost> {
        v.serialize(Measure {
            budget: self.budget,
            depth: self.depth,
            mixed: self.mixed,
        })
    }
    fn accumulate(&mut self, c: Cost) -> R<()> {
        self.cost.nodes = add(self.cost.nodes, c.nodes)?;
        self.cost.tree = add(self.cost.tree, c.tree)?;
        self.cost.encoding = add(self.cost.encoding, c.encoding)?;
        self.cost.work = add(self.cost.work, c.work)?;
        self.cost.encoded = add(self.cost.encoded, c.encoded)?;
        Ok(())
    }
    fn entry<T: Serialize + ?Sized>(&mut self, v: &T) -> R<()> {
        if self.seen >= self.expected || self.map && !self.pending {
            return Err(err());
        }
        let c = self.child(v)?;
        if self.number {
            // Pinned arbitrary_precision Number serializes one lexical string.
            // It is not a JSON user map and must be an exact bounded integer.
            if !c.integer || self.expected != 1 {
                return Err(err());
            }
            self.cost = Cost {
                nodes: 1,
                // Actual arbitrary_precision lexical String survives in Value.
                // Accepted i64/u64 text is at most20 bytes; reserve32 bytes.
                tree: add(size_of::<Value>(), 32)?,
                encoded: c.text,
                // NumberValueEmitter owns the lexical input, scans another
                // bounded integer String, then formats the retained Number.
                encoding: add(c.encoding, 3 * 32)?,
                work: add(c.work, 3)?,
                ..Default::default()
            };
        } else {
            self.accumulate(c)?;
        }
        self.seen += 1;
        self.pending = false;
        debit(self.budget, 1)
    }
    fn key<T: Serialize + ?Sized>(&mut self, v: &T) -> R<()> {
        if !self.map || self.pending || self.seen >= self.expected {
            return Err(err());
        }
        let c = self.child(v)?;
        self.key_max = self.key_max.max(c.text);
        if !self.number {
            self.accumulate(c)?;
        }
        self.pending = true;
        Ok(())
    }
    fn finish(mut self) -> R<Cost> {
        if self.seen != self.expected || self.pending {
            return Err(err());
        }
        debit(self.budget, 1)?;
        if self.number {
            return Ok(self.cost);
        }
        self.cost.encoded = add(self.cost.encoded, self.expected.saturating_sub(1))?;
        if self.map {
            self.cost.encoded = add(self.cost.encoded, self.expected)?;
        }
        let slots = mul(self.expected, if self.map { MAP_ENTRY } else { 0 })?;
        self.cost.tree = add(self.cost.tree, slots)?;
        // encode_with retains child String vector, joined String and outer
        // formatted String. Map also retains sorted key references. Summing
        // every nested container is conservative across depth and siblings.
        let vectors = mul(
            self.expected,
            add(
                size_of::<String>(),
                if self.map { size_of::<&String>() } else { 0 },
            )?,
        )?;
        self.cost.encoding = add(
            self.cost.encoding,
            add(vectors, add(mul(self.cost.encoded, 4)?, 16)?)?,
        )?;
        // Three distinct bounded-map costs: to_value BTree insertions,
        // canonical borrowed-key sorting, and canonical lookup per sorted key.
        // Each is conservatively bounded by n squared comparisons at n<=16;
        // every comparison includes the longest actual key, not just its count.
        let comparisons = if self.map {
            mul(mul(3, self.expected)?, self.expected)?
        } else {
            0
        };
        // Separate element movement from comparisons: bounded BTree insertion
        // shifts owned key/Value pairs; sorting moves borrowed key pointers.
        // Admit n squared pair moves plus three pointer moves per sort swap.
        let movement = if self.map {
            mul(
                mul(self.expected, self.expected)?,
                add(size_of::<(String, Value)>(), mul(3, size_of::<&String>())?)?,
            )?
            .div_ceil(64)
        } else {
            0
        };
        self.cost.work = add(
            add(self.cost.work, movement)?,
            add(
                mul(comparisons, add(1, self.key_max.div_ceil(64))?)?,
                add(self.expected, mul(3, self.cost.encoded.div_ceil(64))?)?,
            )?,
        )?;
        Ok(self.cost)
    }
}
impl<B: StructuralReadBudget> SerializeSeq for Container<'_, B> {
    type Ok = Cost;
    type Error = Refused;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, v: &T) -> R<()> {
        self.entry(v)
    }
    fn end(self) -> R<Cost> {
        self.finish()
    }
}
impl<B: StructuralReadBudget> SerializeMap for Container<'_, B> {
    type Ok = Cost;
    type Error = Refused;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, v: &T) -> R<()> {
        self.key(v)
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, v: &T) -> R<()> {
        self.entry(v)
    }
    fn end(self) -> R<Cost> {
        self.finish()
    }
}
impl<B: StructuralReadBudget> SerializeStruct for Container<'_, B> {
    type Ok = Cost;
    type Error = Refused;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, k: &'static str, v: &T) -> R<()> {
        if self.number && k != NUMBER {
            return Err(err());
        }
        self.key(k)?;
        self.entry(v)
    }
    fn end(self) -> R<Cost> {
        self.finish()
    }
}
macro_rules! signed {($($name:ident:$t:ty),*)=>{$(fn $name(self,v:$t)->R<Cost>{self.number((v as i64).unsigned_abs(),v<0)})*}}
macro_rules! unsigned {($($name:ident:$t:ty),*)=>{$(fn $name(self,v:$t)->R<Cost>{self.number(v as u64,false)})*}}
impl<'a, B: StructuralReadBudget> ser::Serializer for Measure<'a, B> {
    type Ok = Cost;
    type Error = Refused;
    type SerializeSeq = Container<'a, B>;
    type SerializeMap = Container<'a, B>;
    type SerializeStruct = Container<'a, B>;
    type SerializeTuple = ser::Impossible<Cost, Refused>;
    type SerializeTupleStruct = ser::Impossible<Cost, Refused>;
    type SerializeTupleVariant = ser::Impossible<Cost, Refused>;
    type SerializeStructVariant = ser::Impossible<Cost, Refused>;
    signed!(serialize_i8:i8,serialize_i16:i16,serialize_i32:i32,serialize_i64:i64);
    unsigned!(serialize_u8:u8,serialize_u16:u16,serialize_u32:u32,serialize_u64:u64);
    fn serialize_bool(self, v: bool) -> R<Cost> {
        self.leaf(if v { 4 } else { 5 }, 0, false)
    }
    fn serialize_str(self, v: &str) -> R<Cost> {
        self.string(v)
    }
    fn serialize_unit(self) -> R<Cost> {
        self.leaf(4, 0, false)
    }
    fn serialize_none(self) -> R<Cost> {
        self.serialize_unit()
    }
    fn serialize_some<T: Serialize + ?Sized>(self, v: &T) -> R<Cost> {
        v.serialize(self)
    }
    fn serialize_unit_variant(self, name: &'static str, _: u32, v: &'static str) -> R<Cost> {
        let supported = if self.mixed {
            (name == "ImportedSourceKind" && matches!(v, "local" | "provider" | "scan"))
                || (name == "EvidenceKind"
                    && matches!(
                        v,
                        "contribution"
                            | "accepted_decision"
                            | "physical_occurrence"
                            | "timeline_version"
                            | "application_reference"
                    ))
        } else {
            v == "scan"
        };
        if !supported {
            return Err(err());
        }
        self.string(v)
    }
    fn serialize_seq(self, n: Option<usize>) -> R<Self::SerializeSeq> {
        self.container(n, false, false)
    }
    fn serialize_map(self, n: Option<usize>) -> R<Self::SerializeMap> {
        self.container(n, true, false)
    }
    fn serialize_struct(self, name: &'static str, n: usize) -> R<Self::SerializeStruct> {
        self.container(Some(n), true, name == NUMBER)
    }
    fn serialize_f32(self, _: f32) -> R<Cost> {
        Err(err())
    }
    fn serialize_f64(self, _: f64) -> R<Cost> {
        Err(err())
    }
    fn serialize_char(self, _: char) -> R<Cost> {
        Err(err())
    }
    fn serialize_bytes(self, _: &[u8]) -> R<Cost> {
        Err(err())
    }
    fn serialize_unit_struct(self, _: &'static str) -> R<Cost> {
        Err(err())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _: &'static str, _: &T) -> R<Cost> {
        Err(err())
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> R<Cost> {
        Err(err())
    }
    fn serialize_tuple(self, _: usize) -> R<Self::SerializeTuple> {
        Err(err())
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> R<Self::SerializeTupleStruct> {
        Err(err())
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> R<Self::SerializeTupleVariant> {
        Err(err())
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> R<Self::SerializeStructVariant> {
        Err(err())
    }
    fn collect_str<T: std::fmt::Display + ?Sized>(self, _: &T) -> R<Cost> {
        Err(err())
    }
}
fn measure(value: &impl Serialize, budget: &mut impl StructuralReadBudget) -> Result<Cost> {
    value
        .serialize(Measure {
            budget,
            depth: 0,
            mixed: false,
        })
        .map_err(|e| fail(&format!("library accounting: {e}")))
}
fn reserve(cost: Cost, times: usize, budget: &mut impl StructuralReadBudget) -> Result<()> {
    let bytes = add(cost.tree, cost.encoding)
        .and_then(|n| mul(n, times))
        .map_err(|_| fail("canonical storage overflow"))?;
    let work = add(cost.work, cost.encoded.div_ceil(64))
        .and_then(|n| mul(n, times))
        .map_err(|_| fail("canonical work overflow"))?;
    let values = mul(cost.nodes, times).map_err(|_| fail("canonical nodes overflow"))?;
    budget.charge(BudgetCharge {
        bytes,
        work,
        values,
        ..Default::default()
    })
}

// The library-only wrapper keeps its explicit all-part producer restriction.
// Permanent Activate authenticates original semantic admission; canonical input
// is verified by the shared commitment path, without re-running typed.validate,
// constructing its ID sets, or encoding the batch a second time.
pub(super) fn require_library_only(
    part: HostImportPart<'_>,
    budget: &mut impl StructuralReadBudget,
) -> Result<()> {
    require(
        part.batches.len() <= catalog_import::MAX_IMPORT_BATCHES,
        "archive batch count exceeds finite cohort",
    )?;
    for batch in part.batches {
        let typed = batch
            .typed
            .as_ref()
            .ok_or_else(|| fail("library-only typed part required"))?;
        require(
            batch.unresolved_evidence.is_empty()
                && !batch.items.is_empty()
                && batch.items.len() == typed.metadata.len()
                && typed.native.is_none()
                && typed.pins.is_empty()
                && typed.tags.is_empty()
                && typed.relationships.is_empty()
                && typed.logical_aliases.is_empty(),
            "mixed archive owners unsupported",
        )?;
        for (item, m) in batch.items.iter().zip(&typed.metadata) {
            let identifiers = [
                item.item_id.as_str(),
                item.source_id.as_str(),
                item.kind.as_str(),
                m.item_id.as_str(),
            ];
            let input = identifiers
                .iter()
                .try_fold(0usize, |n, s| n.checked_add(s.len()))
                .ok_or_else(|| fail("producer comparison overflow"))?;
            budget.charge(BudgetCharge {
                work: 4 + input.div_ceil(64),
                ..Default::default()
            })?;
            require(
                item.item_id == m.item_id
                    && item.source_id == item.item_id
                    && item.kind == "custom:motion_library"
                    && item
                        .legacy_metadata
                        .as_object()
                        .is_some_and(|m| m.is_empty())
                    && m.contract_version == 2
                    && m.system_fields == ["motion_library"]
                    && m.contributions.len() == 1
                    && m.contributions[0].namespace == "motion.structure"
                    && m.contributions[0].record_id == "source_library"
                    && m.contributions[0].kind
                        == crate::catalog_import_typed::ImportedSourceKind::Scan,
                "non-library archive producer unsupported",
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Budget {
        work: usize,
        values: usize,
    }
    impl StructuralReadBudget for Budget {
        fn charge(&mut self, c: BudgetCharge) -> Result<()> {
            self.work = self
                .work
                .checked_add(c.work)
                .ok_or_else(|| fail("test overflow"))?;
            self.values = self
                .values
                .checked_add(c.values)
                .ok_or_else(|| fail("test overflow"))?;
            Ok(())
        }
        fn raw_json(&mut self, _: &str) -> Result<()> {
            panic!("borrowed accounting must not parse JSON")
        }
    }
    #[test]
    fn scalar_escaping_and_integer_pseudo_struct_match_canonical_lengths() {
        for v in [
            json!(17),
            json!(-17),
            json!(true),
            json!(null),
            json!("quote\"\n\\😀"),
            json!([1, "x"]),
            json!({"z":2,"a":[true]}),
        ] {
            let mut b = Budget::default();
            let c = measure(&v, &mut b).unwrap();
            assert_eq!(
                c.encoded,
                catabolic_core::encode_with(&v, false, true).len()
            );
            assert!(b.work > 0 && b.values > 0);
        }
        let mut b = Budget::default();
        let number = measure(&json!(17), &mut b).unwrap();
        let lookalike = json!({"$serde_json::private::Number":"17"});
        let object = measure(&lookalike, &mut b).unwrap();
        assert_eq!(
            object.encoded,
            catabolic_core::encode_with(&lookalike, false, true).len()
        );
        assert!(object.encoded > number.encoded);
        assert!(measure(&json!(1.5), &mut b).is_err());
        assert!(measure(&json!("x"), &mut b).unwrap().encoding >= 128);
        let long = json!("x".repeat(1024));
        let long_cost = measure(&long, &mut b).unwrap();
        assert!(long_cost.encoding >= 2 * long_cost.encoded + 128);
        assert!(number.tree >= size_of::<Value>() + 20);
        assert!(measure(&json!(u64::MAX), &mut b).unwrap().tree >= size_of::<Value>() + 20);
    }
    #[test]
    fn canonical_nested_buffers_and_checked_overflow_are_not_flat_raw_multipliers() {
        let mut b = Budget::default();
        let flat = measure(&json!("same"), &mut b).unwrap();
        let nested = measure(&json!([[["same"]]]), &mut b).unwrap();
        assert!(nested.encoding > flat.encoding && nested.work > flat.work);
        assert!(
            reserve(
                Cost {
                    tree: usize::MAX,
                    ..Default::default()
                },
                2,
                &mut b
            )
            .is_err()
        );
        let mut deep = json!("x");
        for _ in 0..=DEPTH {
            deep = json!([deep]);
        }
        assert!(measure(&deep, &mut b).is_err());
        let large_map: serde_json::Map<String, Value> =
            (0..=MAP).map(|i| (i.to_string(), json!(0))).collect();
        assert!(measure(&Value::Object(large_map), &mut b).is_err());
    }
}

// Separate canonical-only mode. Alias receipt strings remain opaque text.
pub(super) fn preflight_mixed(
    part: HostImportPart<'_>,
    subject: &str,
    budget: &mut impl StructuralReadBudget,
) -> Result<()> {
    require(
        part.manifest.contract_version == 2
            && part.manifest.source_namespace == "motion.schema68.export.v4"
            && !part.batches.is_empty()
            && part.batches.len() <= catalog_import::MAX_IMPORT_BATCHES
            && part.manifest.batches.len() == part.batches.len(),
        "mixed archive manifest cohort differs",
    )?;
    budget.charge(BudgetCharge {
        bytes: DEPTH
            * (size_of::<Container<'_, crate::catalog_nfo_attribution::ProofBudget>>()
                + size_of::<Cost>()),
        work: 1,
        ..Default::default()
    })?;
    let measured = part
        .manifest
        .serialize(Measure {
            budget,
            depth: 0,
            mixed: true,
        })
        .map_err(|e| fail(&format!("mixed archive accounting: {e}")))?;
    reserve(measured, 1, budget)?;
    // Manifest SHA text remains live while each batch descriptor is compared.
    // Admit fixed hash hex and the returned String owner independently of trees.
    budget.charge(BudgetCharge {
        bytes: 64 + size_of::<String>(),
        work: 2,
        ..Default::default()
    })?;
    for batch in part.batches {
        require(
            batch.items.len() <= catalog_import::MAX_IMPORT_ITEMS_PER_BATCH
                && batch.unresolved_evidence.len() <= catalog_import::MAX_IMPORT_EVIDENCE_PER_BATCH,
            "mixed archive row cohort differs",
        )?;
        require(
            super::commitment::typed_count(batch, budget)? <= 64,
            "mixed archive typed count exceeds cohort",
        )?;
        let measured = batch
            .serialize(Measure {
                budget,
                depth: 0,
                mixed: true,
            })
            .map_err(|e| fail(&format!("mixed archive accounting: {e}")))?;
        require(
            measured.encoded <= catalog_import::MAX_IMPORT_BATCH_BYTES,
            "mixed archive canonical bytes exceed cohort",
        )?;
        // One to_value + encode_with + hash; no extra validate_batch encoding
        // and no unrelated typed.validate semantic owner is executed.
        reserve(measured, 1, budget)?;
        budget.charge(BudgetCharge {
            bytes: 64 + size_of::<catalog_import::BatchDescriptor>(),
            work: 2,
            ..Default::default()
        })?;
        if let Some(typed) = &batch.typed {
            for metadata in &typed.metadata {
                charge(
                    budget,
                    metadata
                        .item_id
                        .len()
                        .checked_add(subject.len())
                        .ok_or_else(|| fail("subject comparison overflow"))?,
                )?;
                if metadata.item_id == subject {
                    // Selected input-derived expected mutation/evidence and
                    // returned definition copies still enter the strict tail.
                    for contribution in &metadata.contributions {
                        let fields = measure(&contribution.fields, budget)?;
                        reserve(fields, 3, budget)?;
                        let evidence = measure(&contribution.evidence, budget)?;
                        reserve(evidence, 2, budget)?;
                    }
                }
            }
        }
    }
    charge(budget, part.activation.canonical_payload().len())?;
    budget.raw_json(part.activation.canonical_payload())?;
    Ok(())
}
