//! Archived requested names only. This proof neither establishes historical
//! assertion/vocabulary IDs nor confers current tag, source or suppression
//! authority. Its digest is retained by the imported baseline and source-history
//! proof; current assertion/vocabulary closure still needs independent proof.
use super::ProofBudget;
use crate::{
    Error, Result,
    catalog_import::{ImportBatch, MAX_IMPORT_BATCHES},
    operation_budget::BudgetCharge,
};
use catabolic_core::{
    catalog_command::MAX_COMMAND_JSON_DEPTH, catalog_metadata::MAX_METADATA_JSON_DEPTH,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;

const NAMESPACE: &str = "motion.nfo";
const MAX_TAGS: usize = 100;
const MAX_TAG_BYTES: usize = 100;

/// Nonserializable semantic evidence about one already verified frozen archive.
/// The optional location identifies an exact typed tag record, not an assertion
/// ID. None proves omission only within the complete supplied manifest closure.
#[derive(Debug)]
pub(in crate::catalog_nfo_attribution) struct ImportedRequestedNames {
    subject: String,
    source_revision: i64,
    tags: BTreeSet<String>,
    matching_record: Option<(usize, usize)>,
    digest: String,
}
impl ImportedRequestedNames {
    pub(in crate::catalog_nfo_attribution) fn subject(&self) -> &str {
        &self.subject
    }
    pub(in crate::catalog_nfo_attribution) fn source_revision(&self) -> i64 {
        self.source_revision
    }
    pub(in crate::catalog_nfo_attribution) fn tags(&self) -> &BTreeSet<String> {
        &self.tags
    }
    pub(in crate::catalog_nfo_attribution) fn matching_record(&self) -> Option<(usize, usize)> {
        self.matching_record
    }
    pub(in crate::catalog_nfo_attribution) fn digest(&self) -> &str {
        &self.digest
    }
}

fn refused(message: &str) -> Error {
    Error(format!("catalog_nfo_imported_tags_refused: {message}"))
}

pub(in crate::catalog_nfo_attribution) fn reserve_normalization(
    name: &str,
    budget: &mut ProofBudget,
) -> Result<()> {
    // Use the same frozen mapping as sql::casefold, but borrow mapped strings
    // and encode lookup keys on the stack. Canonical decomposition is streamed
    // without building the owner's folded or normalized strings first.
    let folding = &crate::migration::reference()["casefold"];
    let mut folded_bytes = 0usize;
    let mut decomposed_scalars = Some(0usize);
    for scalar in name.trim().chars() {
        let mut utf8 = [0u8; 4];
        let key = scalar.encode_utf8(&mut utf8);
        let folded = folding.get(&*key).and_then(Value::as_str).unwrap_or(key);
        folded_bytes = folded_bytes
            .checked_add(folded.len())
            .ok_or_else(|| refused("casefold reservation overflow"))?;
        for scalar in folded.chars() {
            unicode_normalization::char::decompose_canonical(scalar, |_| {
                decomposed_scalars = decomposed_scalars.and_then(|n| n.checked_add(1));
            });
        }
    }
    // Native casefold creates per-input lookup keys, owned mapped chunks and
    // their collected result. NFC has at most the canonical decomposition's
    // scalar count, each scalar at most four UTF-8 bytes. The existing Unicode
    // implementation retains (combining class, scalar) decomposition entries,
    // stable-sort scratch and a scalar recomposition buffer. Reserve these plus
    // NFC output, namespace formatting and JSON validation copies. This counts
    // represented buffers, not allocator capacity overhead.
    let bytes_per_decomposed_scalar =
        2 * std::mem::size_of::<(u8, char)>() + std::mem::size_of::<char>() + 3 * 4;
    let bytes = decomposed_scalars
        .and_then(|n| n.checked_mul(bytes_per_decomposed_scalar))
        .and_then(|n| folded_bytes.checked_mul(2).and_then(|f| n.checked_add(f)))
        .and_then(|n| n.checked_add(name.len()))
        .ok_or_else(|| refused("normalization reservation overflow"))?;
    budget.charge(BudgetCharge {
        bytes,
        values: 8,
        work: bytes / 64 + 9,
        ..Default::default()
    })
}

fn requested_names<'a>(
    values: impl ExactSizeIterator<Item = &'a Value>,
    budget: &mut ProofBudget,
) -> Result<BTreeSet<&'a str>> {
    if values.len() > MAX_TAGS {
        return Err(refused("requested tag count exceeds source bound"));
    }
    let mut names = BTreeSet::new();
    for value in values {
        let name = value
            .as_str()
            .filter(|name| !name.is_empty() && name.len() <= MAX_TAG_BYTES)
            .ok_or_else(|| refused("requested tag must be bounded text"))?;
        // Bound the complete native normalization even when an expanded spelling
        // will be rejected. No second implementation makes the identity decision.
        budget.serialized(value, MAX_METADATA_JSON_DEPTH)?;
        reserve_normalization(name, budget)?;
        budget.charge(BudgetCharge {
            values: 1,
            work: 1,
            ..Default::default()
        })?;
        if crate::tagging::tag_name(value)? != name || !names.insert(name) {
            return Err(refused("duplicate or noncanonical requested tag"));
        }
    }
    Ok(names)
}

/// The caller must first validate the exact complete manifest/activation and
/// duplicate-safe parse its original document with the existing raw byte/depth
/// admission. A Value alone cannot prove that duplicate keys were not discarded.
/// This helper recharges its complete traversal, not just the matching record.
/// It does not validate an activation or turn caller-supplied batches into one.
/// A tag record in a different activated part needs a separate future proof;
/// its absence here never authorizes looking up current assertions instead.
pub(super) fn verify_requested_names(
    subject: &str,
    source_revision: i64,
    original: &Value,
    complete_batches: &[ImportBatch],
    budget: &mut ProofBudget,
) -> Result<ImportedRequestedNames> {
    if subject.trim().is_empty()
        || subject.len() > 256
        || subject.chars().any(char::is_control)
        || source_revision < 1
        || complete_batches.is_empty()
        || complete_batches.len() > MAX_IMPORT_BATCHES
    {
        return Err(refused("invalid subject, source revision or batch closure"));
    }
    let document_digest = budget.serialized(original, MAX_METADATA_JSON_DEPTH)?;
    let batches_digest = budget.serialized(&complete_batches, MAX_COMMAND_JSON_DEPTH)?;
    let object = original
        .as_object()
        .ok_or_else(|| refused("original document must be an object"))?;
    let tags = object
        .get("tags")
        .and_then(Value::as_array)
        .ok_or_else(|| refused("original requested tags must be explicit"))?;
    let excluded = object
        .get("excluded_tags")
        .and_then(Value::as_array)
        .ok_or_else(|| refused("original provider exclusions must be explicit"))?;
    if !excluded.is_empty() {
        return Err(refused("NFO source cannot own provider exclusions"));
    }
    let tags = requested_names(tags.iter(), budget)?;
    let mut matching_record = None;
    for (batch_index, batch) in complete_batches.iter().enumerate() {
        let Some(typed) = &batch.typed else {
            continue;
        };
        for (record_index, record) in typed.tags.iter().enumerate() {
            budget.charge(BudgetCharge {
                work: 1,
                ..Default::default()
            })?;
            // Even unrelated records were fully charged above. Local source
            // exclusions remain independent and are not claimed by this proof.
            if record.item_id != subject || record.namespace != NAMESPACE {
                continue;
            }
            if matching_record
                .replace((batch_index, record_index))
                .is_some()
            {
                return Err(refused("multiple imported NFO tag records"));
            }
            if record.source_revision != source_revision
                || !record.excluded_tags.is_empty()
                || record.tags.len() > MAX_TAGS
            {
                return Err(refused(
                    "typed source revision or provider exclusions differ",
                ));
            }
            let mut imported = BTreeSet::new();
            for name in &record.tags {
                budget.charge(BudgetCharge {
                    values: 1,
                    work: 1,
                    ..Default::default()
                })?;
                if name.is_empty() || name.len() > MAX_TAG_BYTES {
                    return Err(refused("typed requested tag exceeds source bound"));
                }
                // The original set was validated with the native normalizer.
                // Exact membership therefore proves normalization equality here
                // without copying typed strings into a second JSON tree.
                if !tags.contains(name.as_str()) || !imported.insert(name.as_str()) {
                    return Err(refused("typed requested tags differ or contain duplicates"));
                }
            }
            if imported != tags {
                return Err(refused("typed requested tags are incomplete"));
            }
        }
    }
    if matching_record.is_none() && !tags.is_empty() {
        return Err(refused(
            "nonempty original tags lack their typed import record",
        ));
    }
    #[derive(Serialize)]
    struct Proof<'a> {
        domain: &'static str,
        subject: &'a str,
        namespace: &'static str,
        source_revision: i64,
        document_digest: &'a str,
        batches_digest: &'a str,
        tags: &'a BTreeSet<&'a str>,
        excluded_tags: [&'a str; 0],
        matching_record: Option<(usize, usize)>,
    }
    let digest = budget.serialized(
        &Proof {
            domain: "catabolic.nfo.imported-requested-tags.v1",
            subject,
            namespace: NAMESPACE,
            source_revision,
            document_digest: &document_digest,
            batches_digest: &batches_digest,
            tags: &tags,
            excluded_tags: [],
            matching_record,
        },
        MAX_COMMAND_JSON_DEPTH,
    )?;
    // Reserve retained copies before creating the private token. These are
    // descriptive names only; no current name-to-ID lookup occurs anywhere.
    let bytes = tags
        .iter()
        .try_fold(subject.len() + digest.len(), |sum, name| {
            sum.checked_add(name.len())
                .ok_or_else(|| refused("retained requested-name size overflow"))
        })?;
    budget.charge(BudgetCharge {
        bytes,
        values: tags.len() + 3,
        work: bytes / 64 + tags.len() + 1,
        ..Default::default()
    })?;
    Ok(ImportedRequestedNames {
        subject: subject.to_owned(),
        source_revision,
        tags: tags.into_iter().map(str::to_owned).collect(),
        matching_record,
        digest,
    })
}

#[cfg(test)]
mod tests;
