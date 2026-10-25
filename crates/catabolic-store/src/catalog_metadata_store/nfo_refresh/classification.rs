//! Exact original-v1 marker compatibility for the private candidate41 owner.
//! This proof establishes policy provenance, NEVER source-write authority.
//! Generic core compatibility and all ordinary40 owner semantics stay strict.
use super::super::*;
use crate::catalog_nfo_attribution::ProofBudget;
use catabolic_core::catalog_metadata::{
    MAX_METADATA_JSON_DEPTH, MAX_METADATA_JSON_NODES, revalidate_document,
    validate_schema_extension,
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, sync::Arc};
const FIELD: &str = "motion_catalog";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::catalog_metadata_store) struct ClassificationProof {
    subject: String,
    head: String,
    original: String,
    policy: FieldPolicy,
    ancestors: BTreeSet<String>,
    digest: String,
}
impl ClassificationProof {
    pub(super) fn digest(&self) -> &str {
        &self.digest
    }
    pub(super) fn permits(&self, schema: &FieldSchema) -> bool {
        schema.fingerprint() == self.head && schema.fields().get(FIELD) == Some(&self.policy)
    }
}
fn budget_error(error: impl std::fmt::Display) -> MetadataStoreError {
    fail(
        MetadataStoreErrorCode::StoreFailure,
        format!("original-v1 classification budget: {error}"),
    )
}
fn refused(message: &str) -> MetadataStoreError {
    fail(
        MetadataStoreErrorCode::AuthorityDenied,
        format!("original-v1 classification: {message}"),
    )
}
fn budget() -> Result<ProofBudget> {
    let shared = crate::operation_budget::OperationBudget::current()
        .ok_or_else(|| refused("retained operation budget absent"))?;
    Ok(ProofBudget::new(shared))
}
fn text<'a>(row: &'a rusqlite::Row<'_>, index: usize, maximum: usize) -> Result<&'a str> {
    let rusqlite::types::ValueRef::Text(raw) = row.get_ref(index)? else {
        return Err(refused("registered text absent"));
    };
    // Bound borrowed bytes BEFORE UTF-8 validation scans the SQLite value.
    if raw.len() > maximum {
        return Err(MetadataError::SnapshotBudget.into());
    }
    std::str::from_utf8(raw).map_err(|_| refused("registered text is not UTF-8"))
}
/// The raw preflight admits decoding the policy records. The constructor then
/// clones each field name into its output collection (and each system name into
/// the classification set); reserve those distinct output entries without a
/// second validation/hash traversal of the full retained JSON.
fn admit_output(
    budget: &mut ProofBudget,
    policies: &[PolicyField],
    systems: &BTreeSet<String>,
) -> Result<()> {
    struct Names<'a>(&'a [PolicyField]);
    impl serde::Serialize for Names<'_> {
        fn serialize<S: serde::Serializer>(
            &self,
            serializer: S,
        ) -> std::result::Result<S::Ok, S::Error> {
            use serde::ser::SerializeSeq;
            let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
            for policy in self.0 {
                sequence.serialize_element(&policy.field)?;
            }
            sequence.end()
        }
    }
    budget
        .serialized(&(Names(policies), systems), 4)
        .map_err(budget_error)?;
    Ok(())
}

fn hash(hash: &mut Sha256, value: &str) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value.as_bytes());
}
fn transition(old: &FieldSchema, new: &FieldSchema, policy: &FieldPolicy) -> Result<()> {
    if validate_schema_extension(old, new).is_ok() {
        return Ok(());
    }
    if old.contract_version() != 2
        || new.contract_version() != 2
        || old.system_fields().contains(FIELD)
        || !new.system_fields().contains(FIELD)
        || old.fields().get(FIELD) != Some(policy)
        || new.fields().get(FIELD) != Some(policy)
        || old
            .fields()
            .iter()
            .any(|(name, value)| new.fields().get(name) != Some(value))
        || old
            .system_fields()
            .iter()
            .any(|name| !new.system_fields().contains(name))
        || new.system_fields().iter().any(|name| {
            old.fields().contains_key(name) && name != FIELD && !old.system_fields().contains(name)
        })
    {
        return Err(refused(
            "policy/classification transition is not exact original marker promotion",
        ));
    }
    Ok(())
}

/// Capture the COMPLETE registered head-to-original chain before returning a
/// token. A v2-born root supplies no fallback. Immutable root bytes and every
/// registered fingerprint are independently reconstructed by the existing owner.
pub(in crate::catalog_metadata_store) fn capture_in(
    db: &Connection,
    subject: &str,
    current: &FieldSchema,
) -> Result<Option<Arc<ClassificationProof>>> {
    if db.is_autocommit()
        || db.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))? != 41
    {
        return Err(refused("exact candidate41 retained writer required"));
    }
    let Some(policy) = current.fields().get(FIELD).copied() else {
        return Ok(None);
    };
    let mut budget = budget()?;
    let mut query = db.prepare("SELECT contract_version,schema_fingerprint,policies_json FROM catalog_metadata_subjects WHERE item_id=?")?;
    let mut rows = query.query([subject])?;
    let row = rows
        .next()?
        .ok_or_else(|| refused("original metadata subject absent"))?;
    if row.get::<_, i64>(0)? != 1 {
        return Ok(None);
    }
    let root = text(row, 1, 64)?;
    let raw = text(row, 2, MAX_METADATA_SNAPSHOT_BYTES)?;
    budget
        .serialized(&(subject, root, current.fingerprint()), 4)
        .map_err(budget_error)?;
    budget
        .raw_json(raw, MAX_METADATA_JSON_DEPTH)
        .map_err(budget_error)?;
    let policies: Vec<PolicyField> = serde_json::from_str(raw)?;
    admit_output(&mut budget, &policies, &BTreeSet::new())?;
    let original = field_schema(&policies)?;
    if original.fingerprint() != root || original.fields().get(FIELD) != Some(&policy) {
        return Err(refused("original v1 marker policy differs"));
    }
    let root = root.to_owned();
    let exact_head: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_metadata_schema_heads WHERE item_id=? AND schema_fingerprint=?)",params![subject,current.fingerprint()],|row|row.get(0))?;
    if !exact_head {
        return Err(refused("current registered head differs"));
    }
    let count: i64 = db.query_row(
        "SELECT count(*) FROM (SELECT 1 FROM catalog_metadata_schemas WHERE item_id=? LIMIT ?)",
        params![subject, (MAX_METADATA_JSON_NODES + 1) as i64],
        |row| row.get(0),
    )?;
    let count = usize::try_from(count).map_err(budget_error)?;
    if count > MAX_METADATA_JSON_NODES {
        return Err(MetadataError::JsonNodeBudget.into());
    }
    let mut cursor = current.fingerprint().to_owned();
    let mut ancestors = BTreeSet::new();
    let mut newer: Option<FieldSchema> = None;
    let mut digest = Sha256::new();
    hash(&mut digest, "catabolic.original-v1-marker.v1");
    hash(&mut digest, subject);
    hash(&mut digest, raw);
    loop {
        budget.serialized(&cursor, 4).map_err(budget_error)?;
        if !ancestors.insert(cursor.clone()) {
            return Err(refused("registered ancestry cycle"));
        }
        let mut query = db.prepare("SELECT contract_version,policies_json,system_fields_json,prior_fingerprint FROM catalog_metadata_schemas WHERE item_id=? AND schema_fingerprint=?")?;
        let mut rows = query.query(params![subject, cursor])?;
        let row = rows
            .next()?
            .ok_or_else(|| refused("registered ancestry gap"))?;
        let policies = text(row, 1, MAX_METADATA_SNAPSHOT_BYTES)?;
        let system = text(row, 2, MAX_METADATA_SNAPSHOT_BYTES)?;
        let prior = if matches!(row.get_ref(3)?, rusqlite::types::ValueRef::Null) {
            None
        } else {
            Some(text(row, 3, 64)?)
        };
        for raw in [policies, system] {
            budget
                .raw_json(raw, MAX_METADATA_JSON_DEPTH)
                .map_err(budget_error)?;
        }
        budget
            .serialized(&(row.get::<_, i64>(0)?, prior), 4)
            .map_err(budget_error)?;
        let decoded: Vec<PolicyField> = serde_json::from_str(policies)?;
        let systems: BTreeSet<String> = serde_json::from_str(system)?;
        admit_output(&mut budget, &decoded, &systems)?;
        let schema =
            schema_registry::registered_from_policies(row.get(0)?, &cursor, decoded, systems)?;
        if schema.fields().get(FIELD) != Some(&policy) {
            return Err(refused("marker policy changed along ancestry"));
        }
        if let Some(newer) = &newer {
            transition(&schema, newer, &policy)?;
        } else if schema != *current {
            return Err(refused("current schema bytes differ"));
        }
        for value in [cursor.as_str(), policies, system, prior.unwrap_or("")] {
            hash(&mut digest, value);
        }
        match prior {
            Some(prior) => {
                if cursor == root {
                    return Err(refused("original root has a predecessor"));
                }
                cursor = prior.to_owned();
                newer = Some(schema);
            }
            None => {
                if cursor != root || schema != original || ancestors.len() != count {
                    return Err(refused(
                        "ancestry does not terminate at the exact complete original v1 root",
                    ));
                }
                break;
            }
        }
    }
    Ok(Some(Arc::new(ClassificationProof {
        subject: subject.to_owned(),
        head: current.fingerprint().to_owned(),
        original: root,
        policy,
        ancestors,
        digest: format!("{:x}", digest.finalize()),
    })))
}

pub(super) fn validate_planned(
    proof: &ClassificationProof,
    old: &FieldSchema,
    new: &FieldSchema,
) -> Result<()> {
    if !proof.permits(old) {
        return Err(refused("plan does not match captured head"));
    }
    transition(old, new, &proof.policy)
}

/// Core constructor rechecks the existing policy/type/name/document budgets.
/// Exact semantic equality refuses any unintended renormalization; no retained
/// source revision, fingerprint, value or evidence is persisted by this read.
fn copy_under(
    current: &FieldSchema,
    historical: &FieldSchema,
    document: &ContributionDocument,
) -> Result<ContributionDocument> {
    if document.schema_fingerprint() != historical.fingerprint() {
        return Err(MetadataError::SchemaMismatch.into());
    }
    let mut budget = budget()?;
    budget
        .serialized(document.fields(), MAX_METADATA_JSON_DEPTH)
        .map_err(budget_error)?;
    budget
        .canonical_serialized(document.evidence(), MAX_METADATA_JSON_DEPTH)
        .map_err(budget_error)?;
    let copy = ContributionDocument::from_json_with_system_fields(
        current,
        document.key().clone(),
        document.kind(),
        document.revision(),
        &serde_json::to_string(document.fields())?,
        &encode(document.evidence()),
    )?;
    if copy.fields() != document.fields()
        || copy.evidence() != document.evidence()
        || copy.key() != document.key()
        || copy.kind() != document.kind()
        || copy.revision() != document.revision()
    {
        return Err(refused(
            "retained document changed under identical policies",
        ));
    }
    Ok(copy)
}
pub(super) fn revalidate_planned(
    current: &FieldSchema,
    historical: &FieldSchema,
    document: &ContributionDocument,
    proof: Option<&ClassificationProof>,
) -> Result<ContributionDocument> {
    match revalidate_document(current, historical, document) {
        Ok(copy) => return Ok(copy),
        Err(error) if error != MetadataError::SchemaMismatch => return Err(error.into()),
        Err(_) => (),
    }
    let proof = proof.ok_or_else(|| refused("exact original-v1 proof absent"))?;
    validate_planned(proof, historical, current)?;
    copy_under(current, historical, document)
}

/// Shared READ compatibility, not a new classification planner or source grant.
/// The fallback is41-only and requires both historical and current fingerprints
/// on the complete, exact registered original-v1 chain.
pub(crate) fn revalidate_retained_document(
    db: &Connection,
    subject: &str,
    current: &FieldSchema,
    historical: &FieldSchema,
    document: &ContributionDocument,
) -> Result<ContributionDocument> {
    revalidate_retained_cached(db, subject, current, historical, document, &mut None)
}

pub(in crate::catalog_metadata_store) fn revalidate_retained_cached(
    db: &Connection,
    subject: &str,
    current: &FieldSchema,
    historical: &FieldSchema,
    document: &ContributionDocument,
    cached: &mut Option<Arc<ClassificationProof>>,
) -> Result<ContributionDocument> {
    match revalidate_document(current, historical, document) {
        Ok(copy) => return Ok(copy),
        Err(error) if error != MetadataError::SchemaMismatch => return Err(error.into()),
        Err(_) => (),
    }
    if db.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))? != 41 {
        return Err(MetadataError::SchemaMismatch.into());
    }
    if cached.is_none() {
        *cached = capture_in(db, subject, current)?;
    }
    let proof = cached
        .as_ref()
        .ok_or_else(|| refused("v2-born schema has no classification ancestry"))?;
    if proof.subject != subject
        || !proof.permits(current)
        || !proof.ancestors.contains(historical.fingerprint())
    {
        return Err(refused("retained source is outside original-v1 ancestry"));
    }
    transition(historical, current, &proof.policy)?;
    copy_under(current, historical, document)
}

#[cfg(test)]
mod tests;
