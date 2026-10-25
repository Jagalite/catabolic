//! Private cumulative admission for historical NFO proofs, never authority.
use crate::{
    Error, Result,
    operation_budget::{BudgetCharge, OperationBudget},
    selection::EvaluationFailure,
};
use catabolic_core::catalog_metadata::{MAX_METADATA_JSON_NODES, MAX_METADATA_SNAPSHOT_BYTES};
use serde::Serialize;
use sha2::{Digest, Sha256};
fn refused(message: &str) -> Error {
    Error(format!("catalog_nfo_proof_budget_refused: {message}"))
}
// Fixed source-succession domain only. 2 MiB / 64 byte quanta plus four
// 8192-node passes motivates this independent ceiling; it is NOT a guarantee
// that every byte/node-valid proof fits. History, comparisons, SQL and repeated
// validation continue to consume the unchanged cumulative parent budget.
const SOURCE_SUCCESSION_WORK: usize = 64 * 1024;
pub(crate) struct ProofBudget {
    shared: OperationBudget,
    used: BudgetCharge,
    source_work: bool,
}
impl ProofBudget {
    #[cfg(test)]
    pub(crate) fn trace_phase(&self, phase: &str) {
        eprintln!(
            "source_phase={phase}; local_used={:?}; shared_used={:?}; shared_failure={:?}",
            self.used,
            self.shared.usage(),
            self.shared.failure()
        );
    }
    pub(crate) fn uses_operation(&self, owner: &OperationBudget) -> bool {
        self.shared.same_owner(owner)
    }
    pub(crate) fn new(shared: OperationBudget) -> Self {
        Self {
            shared,
            used: BudgetCharge::default(),
            source_work: false,
        }
    }
    /// Only the fixed source preparation/receipt/mutation owner selects this
    /// profile. It never creates, replaces or resets the inherited operation.
    pub(crate) fn source_succession(shared: OperationBudget) -> Self {
        let mut proof = Self::new(shared);
        proof.source_work = true;
        proof
    }
    fn source_failure(&self, failure: EvaluationFailure) {
        if self.source_work {
            self.shared.record_failure(failure);
        }
    }
    #[cfg_attr(test, track_caller)]
    pub(super) fn charge(&mut self, charge: BudgetCharge) -> Result<()> {
        let next = BudgetCharge {
            bytes: self.used.bytes.saturating_add(charge.bytes),
            values: self.used.values.saturating_add(charge.values),
            work: self.used.work.saturating_add(charge.work),
            items: 0,
        };
        if next.bytes > MAX_METADATA_SNAPSHOT_BYTES
            || next.values > MAX_METADATA_JSON_NODES
            || next.work
                > if self.source_work {
                    SOURCE_SUCCESSION_WORK
                } else {
                    MAX_METADATA_JSON_NODES
                }
        {
            self.source_failure(if next.bytes > MAX_METADATA_SNAPSHOT_BYTES {
                EvaluationFailure::ByteBudget
            } else {
                EvaluationFailure::WorkBudget
            });
            #[cfg(test)]
            return Err(Error(format!(
                "catalog_nfo_proof_budget_refused: cumulative bytes/nodes/work; caller={}; attempted={charge:?}; local_used={:?}; local_next={next:?}; shared_used={:?}; shared_failure={:?}",
                std::panic::Location::caller(),
                self.used,
                self.shared.usage(),
                self.shared.failure()
            )));
            #[cfg(not(test))]
            return Err(refused("cumulative bytes/nodes/work"));
        }
        self.shared
            .charge(charge)
            .map_err(|failure| {
                #[cfg(test)]
                { Error(format!("catalog_nfo_proof_budget_refused: inherited operation budget; failure={failure:?}; caller={}; attempted={charge:?}; local_used={:?}; shared_used={:?}; shared_failure={:?}", std::panic::Location::caller(), self.used, self.shared.usage(), self.shared.failure())) }
                #[cfg(not(test))]
                { let _ = failure; refused("inherited operation budget") }
            })?;
        self.used = next;
        Ok(())
    }
    #[cfg_attr(test, track_caller)]
    pub(crate) fn serialized(
        &mut self,
        value: &impl Serialize,
        max_depth: usize,
    ) -> Result<String> {
        #[cfg(test)]
        let caller = std::panic::Location::caller();
        let result = self.stream(max_depth, |sink| {
            serde_json::to_writer(sink, value).map_err(|error| {
                #[cfg(test)]
                {
                    Error(format!(
                        "catalog_nfo_proof_budget_refused: serialization or budget; serde={error}"
                    ))
                }
                #[cfg(not(test))]
                {
                    let _ = error;
                    refused("serialization or budget")
                }
            })
        });
        #[cfg(test)]
        {
            result.map_err(|error| Error(format!(
                "{}; caller={}; depth_limit={max_depth}; local_used={:?}; shared_used={:?}; shared_failure={:?}",
                error.0, caller, self.used,
                self.shared.usage(), self.shared.failure()
            )))
        }
        #[cfg(not(test))]
        {
            result
        }
    }
    /// Reserve the legacy canonical encoder's output before calling it. Beyond
    /// compact serde JSON, container separators add at most two bytes per value
    /// node. A finite f64's shortest canonical spelling (including exponent sign)
    /// is below32 bytes; non-f64 integer tokens and string escaping are unchanged.
    /// Sixty-four extra bytes per value node bounds both expansions together.
    pub(super) fn canonical_json(&mut self, value: &serde_json::Value) -> Result<()> {
        self.canonical_serialized(
            value,
            catabolic_core::catalog_metadata::MAX_METADATA_JSON_DEPTH,
        )
    }
    /// Allocation admission for a borrowed canonical-encoding input. This does
    /// not encode or define canonical semantics; the existing owner does that.
    pub(crate) fn canonical_serialized(
        &mut self,
        value: &impl Serialize,
        max_depth: usize,
    ) -> Result<()> {
        let before = self.used.values;
        self.serialized(value, max_depth)?;
        let nodes = self.used.values - before;
        let extra = nodes
            .checked_mul(64)
            .ok_or_else(|| refused("canonical encoding overflow"))?;
        self.charge(BudgetCharge {
            bytes: extra,
            work: extra / 64 + 1,
            ..Default::default()
        })
    }
    pub(crate) fn raw_json(&mut self, raw: &str, max_depth: usize) -> Result<()> {
        self.stream(max_depth, |sink| {
            sink.write_all(raw.as_bytes())
                .map_err(|_| refused("raw JSON or budget"))
        })
        .map(|_| ())
    }
    fn stream(
        &mut self,
        max_depth: usize,
        write: impl FnOnce(&mut dyn std::io::Write) -> Result<()>,
    ) -> Result<String> {
        struct Sink<'a> {
            budget: &'a mut ProofBudget,
            bytes: usize,
            work: usize,
            hash: Sha256,
            values: usize,
            quoted: bool,
            escaped: bool,
            atom: bool,
            // Some(true): object key; Some(false): object value; None: array.
            stack: Vec<Option<bool>>,
            max_depth: usize,
        }
        impl std::io::Write for Sink<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                let total_bytes = self.bytes.saturating_add(bytes.len());
                if total_bytes > MAX_METADATA_SNAPSHOT_BYTES {
                    self.budget.source_failure(EvaluationFailure::ByteBudget);
                    return Err(std::io::Error::other("physical proof byte bound"));
                }
                let old_values = self.values;
                // Lexical state spans serializer writes. Object keys and quoted
                // punctuation are not JSON value nodes. No complete copy is made.
                for byte in bytes {
                    if self.quoted {
                        if self.escaped {
                            self.escaped = false;
                        } else if *byte == b'\\' {
                            self.escaped = true;
                        } else if *byte == b'"' {
                            self.quoted = false;
                        }
                    } else {
                        match byte {
                            b'"' => {
                                if self.stack.last() != Some(&Some(true)) {
                                    self.values += 1;
                                }
                                self.quoted = true;
                                self.atom = false;
                            }
                            b'{' | b'[' => {
                                self.values += 1;
                                if self.stack.len() >= self.max_depth {
                                    self.budget.source_failure(EvaluationFailure::WorkBudget);
                                    return Err(std::io::Error::other(
                                        "physical proof depth bound",
                                    ));
                                }
                                self.stack
                                    .push(if *byte == b'{' { Some(true) } else { None });
                                self.atom = false;
                            }
                            b'}' | b']' => {
                                self.stack.pop();
                                self.atom = false;
                            }
                            b':' | b',' => {
                                if let Some(Some(key)) = self.stack.last_mut() {
                                    *key = *byte == b',';
                                }
                                self.atom = false;
                            }
                            b' ' | b'\n' | b'\r' | b'\t' => self.atom = false,
                            _ if !self.atom => {
                                self.values += 1;
                                self.atom = true;
                            }
                            _ => {}
                        }
                    }
                    if self.values > MAX_METADATA_JSON_NODES {
                        self.budget.source_failure(EvaluationFailure::WorkBudget);
                        return Err(std::io::Error::other("physical proof node bound"));
                    }
                }
                let total_work = total_bytes / 64 + self.values + 1;
                if total_work > MAX_METADATA_JSON_NODES {
                    self.budget.source_failure(EvaluationFailure::WorkBudget);
                    return Err(std::io::Error::other("physical proof work bound"));
                }
                // Charge the inherited operation cumulatively BEFORE hashing each
                // chunk. Deltas avoid serializer chunk-size-dependent overcharging.
                self.budget
                    .charge(BudgetCharge {
                        bytes: bytes.len(),
                        values: self.values - old_values,
                        work: total_work - self.work,
                        ..Default::default()
                    })
                    .map_err(|error| {
                        #[cfg(test)]
                        {
                            std::io::Error::other(error.0)
                        }
                        #[cfg(not(test))]
                        {
                            let _ = error;
                            std::io::Error::other("physical proof operation budget")
                        }
                    })?;
                self.bytes = total_bytes;
                self.work = total_work;
                self.hash.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut sink = Sink {
            budget: self,
            bytes: 0,
            work: 0,
            hash: Sha256::new(),
            values: 0,
            quoted: false,
            escaped: false,
            atom: false,
            stack: Vec::with_capacity(max_depth),
            max_depth,
        };
        write(&mut sink)?;
        if sink.quoted || !sink.stack.is_empty() {
            return Err(refused("incomplete proof serialization"));
        }
        Ok(format!("{:x}", sink.hash.finalize()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation_budget::BudgetLimits;
    use std::time::Duration;
    fn budget() -> ProofBudget {
        ProofBudget::new(
            OperationBudget::new(
                Duration::from_secs(30),
                BudgetLimits {
                    items: usize::MAX,
                    bytes: usize::MAX,
                    values: usize::MAX,
                    work: usize::MAX,
                },
            )
            .unwrap(),
        )
    }
    #[test]
    fn local_proof_admission_is_cumulative_across_serialized_and_raw_inputs() {
        let mut proof = budget();
        proof.serialized(&vec![0; 4000], 64).unwrap();
        assert!(
            proof
                .raw_json(&serde_json::to_string(&vec![0; 4000]).unwrap(), 32)
                .is_err()
        );
    }
    #[test]
    fn raw_document_depth_is_distinct_from_aggregate_envelope_depth() {
        let raw = format!("{}0{}", "[".repeat(33), "]".repeat(33));
        assert!(budget().raw_json(&raw, 32).is_err());
        assert!(budget().raw_json(&raw, 64).is_ok());
    }
    #[test]
    fn canonical_output_allowance_covers_spacing_and_float_reencoding() {
        let value: serde_json::Value =
            serde_json::from_str(r#"{"a":[1e2,1e-300,-1e300,0.0],"escaped":"a\\b\"c","empty":{}}"#)
                .unwrap();
        let mut proof = budget();
        proof.canonical_json(&value).unwrap();
        let encoded = catabolic_core::encode(&value);
        assert!(proof.used.bytes >= encoded.len());
        assert!(proof.used.values > 1);
    }
}

#[cfg(test)]
mod source_profile_tests {
    use super::*;
    use crate::operation_budget::BudgetLimits;
    use std::time::Duration;
    fn parent(work: usize) -> OperationBudget {
        OperationBudget::new(
            Duration::from_secs(5),
            BudgetLimits {
                items: usize::MAX,
                values: usize::MAX,
                bytes: usize::MAX,
                work,
            },
        )
        .unwrap()
    }
    #[test]
    fn source_work_profile_is_fixed_and_nfo_profile_is_unchanged() {
        let nfo_parent = parent(500_000);
        let mut nfo = ProofBudget::new(nfo_parent.clone());
        nfo.charge(BudgetCharge {
            work: MAX_METADATA_JSON_NODES,
            ..Default::default()
        })
        .unwrap();
        assert!(
            nfo.charge(BudgetCharge {
                work: 1,
                ..Default::default()
            })
            .is_err()
        );
        assert_eq!(
            nfo_parent.failure(),
            None,
            "legacy NFO failure behavior changed"
        );
        let parent = parent(500_000);
        let mut source = ProofBudget::source_succession(parent.clone());
        assert!(source.uses_operation(&parent));
        assert_eq!(source.shared.deadline(), parent.deadline());
        source
            .charge(BudgetCharge {
                work: SOURCE_SUCCESSION_WORK,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(parent.usage().work, SOURCE_SUCCESSION_WORK);
        assert!(
            source
                .charge(BudgetCharge {
                    work: 1,
                    ..Default::default()
                })
                .is_err()
        );
        assert_eq!(parent.failure(), Some(EvaluationFailure::WorkBudget));
        assert!(
            ProofBudget::source_succession(parent.clone())
                .charge(BudgetCharge::default())
                .is_err()
        );
        assert!(
            ProofBudget::new(parent)
                .charge(BudgetCharge::default())
                .is_err()
        );
    }
    #[test]
    fn source_local_byte_and_node_bounds_remain_exact_and_sticky() {
        for (boundary, extra, expected) in [
            (
                BudgetCharge {
                    bytes: MAX_METADATA_SNAPSHOT_BYTES,
                    ..Default::default()
                },
                BudgetCharge {
                    bytes: 1,
                    ..Default::default()
                },
                EvaluationFailure::ByteBudget,
            ),
            (
                BudgetCharge {
                    values: MAX_METADATA_JSON_NODES,
                    ..Default::default()
                },
                BudgetCharge {
                    values: 1,
                    ..Default::default()
                },
                EvaluationFailure::WorkBudget,
            ),
        ] {
            let parent = parent(500_000);
            let mut source = ProofBudget::source_succession(parent.clone());
            source.charge(boundary).unwrap();
            assert!(source.charge(extra).is_err());
            assert_eq!(parent.failure(), Some(expected));
            assert_eq!(
                parent.usage(),
                boundary,
                "rejected charge must not be admitted"
            );
            assert!(
                ProofBudget::source_succession(parent)
                    .charge(BudgetCharge::default())
                    .is_err()
            );
        }
    }
    #[test]
    fn source_profile_never_resets_a_tighter_or_failed_parent() {
        let parent = parent(100);
        let mut first = ProofBudget::source_succession(parent.clone());
        first
            .charge(BudgetCharge {
                work: 60,
                ..Default::default()
            })
            .unwrap();
        let mut second = ProofBudget::source_succession(parent.clone());
        second
            .charge(BudgetCharge {
                work: 40,
                ..Default::default()
            })
            .unwrap();
        assert!(
            second
                .charge(BudgetCharge {
                    work: 1,
                    ..Default::default()
                })
                .is_err()
        );
        assert_eq!(parent.usage().work, 100);
        assert_eq!(parent.failure(), Some(EvaluationFailure::WorkBudget));
        assert!(first.charge(BudgetCharge::default()).is_err());
        for failure in [EvaluationFailure::Cancelled, EvaluationFailure::Timeout] {
            let owner = self::parent(500_000);
            owner.record_failure(failure);
            let mut source = ProofBudget::source_succession(owner.clone());
            assert!(source.charge(BudgetCharge::default()).is_err());
            assert_eq!(owner.failure(), Some(failure));
            assert_eq!(owner.usage(), BudgetCharge::default());
        }
    }
    #[test]
    fn source_raw_stream_limits_do_not_inherit_the_larger_work_ceiling() {
        let cases = [
            (
                format!("{}0{}", "[".repeat(33), "]".repeat(33)),
                EvaluationFailure::WorkBudget,
            ),
            (
                format!("\"{}\"", "x".repeat(MAX_METADATA_SNAPSHOT_BYTES)),
                EvaluationFailure::ByteBudget,
            ),
            (
                format!("[{}]", vec!["0"; MAX_METADATA_JSON_NODES].join(",")),
                EvaluationFailure::WorkBudget,
            ),
            (
                format!("\"{}\"", "x".repeat(MAX_METADATA_JSON_NODES * 64)),
                EvaluationFailure::WorkBudget,
            ),
        ];
        for (raw, expected) in cases {
            let parent = parent(500_000);
            let mut source = ProofBudget::source_succession(parent.clone());
            assert!(source.raw_json(&raw, 32).is_err());
            assert_eq!(parent.failure(), Some(expected));
            assert!(
                ProofBudget::source_succession(parent)
                    .charge(BudgetCharge::default())
                    .is_err()
            );
        }
    }
}
