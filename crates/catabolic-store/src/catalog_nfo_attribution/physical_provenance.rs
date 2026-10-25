//! Historical sidecar attribution linked to an exactly activated physical part.
//! This proof deliberately does not observe today's path, binding or file status.
//! It is neither current file custody nor permission to read or withdraw bytes.
use super::verify_committed_command_in;
use crate::{
    Error, Result, Store,
    catalog_commands::TrustedCommandAuthority,
    catalog_import::{self, ImportBatch, ImportMutation, ImportStep, SnapshotManifest},
    catalog_import_typed::PhysicalMapping,
    operation_budget::OperationBudget,
};
use catabolic_core::{
    catalog_command::CommandEnvelope,
    catalog_metadata::nfo_refresh::{FileKey, FileOrigin, OriginProvenance},
};
use serde::Serialize;

/// Constructed independently by the host's complete frozen-origin audit. The
/// host must prove the exact archived (subject, Motion file) row, resolve that
/// file through its immutable OccurrenceMapping, and establish both activated
/// parts in the same saved root cutover. Never deserialize a request into this.
#[derive(Debug)]
pub(crate) struct NfoImportedOriginAuthority {
    pub host_namespace: String,
    pub root_manifest_digest: Option<String>,
    pub metadata_import_id: String,
    pub metadata_input_digest: String,
    pub physical_import_id: String,
    pub physical_input_digest: String,
    pub physical_mapping_digest: String,
    pub origin_audit_digest: String,
    pub subject: String,
    pub motion_file_id: String,
    pub physical: PhysicalMapping,
}

/// Nonserializable descriptive proof. Retaining the exact imported revision is
/// essential: file ID alone must never turn a replaced file into the old origin.
#[derive(Debug)]
pub(crate) struct VerifiedImportedOrigin {
    subject: String,
    motion_file_id: String,
    metadata_command: CommandEnvelope,
    physical_command: CommandEnvelope,
    root_manifest_digest: Option<String>,
    origin_audit_digest: String,
    physical: PhysicalMapping,
    origin: FileOrigin,
    observed_generation: i64,
    proof_digest: String,
}
impl VerifiedImportedOrigin {
    pub(super) fn subject(&self) -> &str {
        &self.subject
    }
    pub(super) fn motion_file_id(&self) -> &str {
        &self.motion_file_id
    }
    pub(super) fn metadata_command(&self) -> &CommandEnvelope {
        &self.metadata_command
    }
    pub(super) fn physical_command(&self) -> &CommandEnvelope {
        &self.physical_command
    }
    pub(super) fn root_manifest_digest(&self) -> Option<&str> {
        self.root_manifest_digest.as_deref()
    }
    pub(super) fn origin_audit_digest(&self) -> &str {
        &self.origin_audit_digest
    }
    pub(super) fn physical(&self) -> &PhysicalMapping {
        &self.physical
    }
    pub(super) fn origin(&self) -> &FileOrigin {
        &self.origin
    }
    pub(super) fn observed_generation(&self) -> i64 {
        self.observed_generation
    }
    pub(super) fn proof_digest(&self) -> &str {
        &self.proof_digest
    }
}
fn refused(message: &str) -> Error {
    Error(format!(
        "catalog_nfo_physical_provenance_refused: {message}"
    ))
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn id(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

/// Bounded serialization hashes without allocating a second complete manifest.
fn charge_and_hash(store: &Store, value: &impl Serialize) -> Result<String> {
    bounded_hash(store.catalog_operation_budget()?, value)
}
fn bounded_hash(budget: OperationBudget, value: &impl Serialize) -> Result<String> {
    super::proof_budget::ProofBudget::new(budget).serialized(
        value,
        catabolic_core::catalog_command::MAX_COMMAND_JSON_DEPTH,
    )
}

fn activation(command: &CommandEnvelope) -> Result<(String, String, String)> {
    if command.payload().as_object().is_none_or(|v| v.len() != 2)
        || command.payload()["operation"] != "import"
    {
        return Err(refused("not an exact import activation"));
    }
    let mutation: ImportMutation = serde_json::from_value(command.payload()["mutation"].clone())?;
    let ImportMutation::Activate {
        import_id,
        input_digest,
        reference_ack_id,
        mapping_digest,
    } = mutation
    else {
        return Err(refused("historical command did not activate an import"));
    };
    if !id(&reference_ack_id)
        || !digest(&input_digest)
        || !digest(&mapping_digest)
        || command.key().operation_id()
            != catalog_import::operation_id(&import_id, ImportStep::Activate)
                .map_err(|e| refused(&e.to_string()))?
    {
        return Err(refused("invalid activation identity"));
    }
    Ok((import_id, input_digest, mapping_digest))
}

pub(crate) fn verify_imported_origin_in(
    store: &Store,
    metadata_activation: &CommandEnvelope,
    physical_manifest: &SnapshotManifest,
    physical_batches: &[ImportBatch],
    physical_activation: &CommandEnvelope,
    grant: &NfoImportedOriginAuthority,
    authority: TrustedCommandAuthority<'_>,
    expected_generation: i64,
) -> Result<VerifiedImportedOrigin> {
    if grant.host_namespace != authority.host_namespace
        || !id(&grant.subject)
        || !id(&grant.motion_file_id)
        || !digest(&grant.origin_audit_digest)
        || grant
            .root_manifest_digest
            .as_ref()
            .is_some_and(|value| !digest(value))
        || grant.physical.exact.validate().is_err()
    {
        return Err(refused("independent complete origin/part grant required"));
    }
    verify_committed_command_in(store, metadata_activation, authority, expected_generation)?;
    verify_committed_command_in(store, physical_activation, authority, expected_generation)?;
    let metadata = activation(metadata_activation)?;
    let physical = activation(physical_activation)?;
    if metadata.0 != grant.metadata_import_id
        || metadata.1 != grant.metadata_input_digest
        || physical.0 != grant.physical_import_id
        || physical.1 != grant.physical_input_digest
        || physical.2 != grant.physical_mapping_digest
        || physical_manifest.import_id != grant.physical_import_id
        || !matches!(
            physical_manifest.source_namespace.as_str(),
            "motion.schema68.export.v1"
                | "motion.schema68.export.v2"
                | "motion.schema68.export.v3"
                | "motion.schema68.export.v4"
        )
        || physical_batches.len() != physical_manifest.batches.len()
        || (grant.root_manifest_digest.is_none()
            && metadata_activation.canonical_payload() != physical_activation.canonical_payload())
    {
        return Err(refused(
            "activation differs from independently admitted root/parts",
        ));
    }
    let proof_digest = charge_and_hash(
        store,
        &(
            grant.root_manifest_digest.as_deref(),
            &grant.origin_audit_digest,
            &grant.subject,
            &grant.motion_file_id,
            &grant.physical,
            metadata_activation.canonical_payload(),
            physical_activation.canonical_payload(),
            physical_manifest,
            physical_batches,
        ),
    )?;
    if catalog_import::manifest_digest(physical_manifest).map_err(|e| refused(&e.to_string()))?
        != grant.physical_input_digest
    {
        return Err(refused("physical manifest digest changed"));
    }
    let mut found = None;
    for (batch, descriptor) in physical_batches.iter().zip(&physical_manifest.batches) {
        if &catalog_import::describe_batch(batch).map_err(|e| refused(&e.to_string()))?
            != descriptor
        {
            return Err(refused("physical batch descriptor changed"));
        }
        if let Some(native) = batch.typed.as_ref().and_then(|typed| typed.native.as_ref()) {
            for mapping in &native.occurrences {
                if mapping.source_occurrence_id == grant.physical.source_occurrence_id {
                    if found.replace(mapping).is_some() {
                        return Err(refused("duplicate historical occurrence"));
                    }
                }
            }
        }
    }
    if found != Some(&grant.physical) {
        return Err(refused(
            "exact historical physical mapping absent or changed",
        ));
    }
    let origin = FileOrigin {
        file: FileKey {
            profile: grant.physical.profile.clone(),
            file_id: grant.physical.exact.file_id.clone(),
        },
        provenance: OriginProvenance::Imported {
            import_id: grant.physical_import_id.clone(),
            source_occurrence_id: grant.physical.source_occurrence_id.clone(),
            mapping_digest: grant.physical_mapping_digest.clone(),
        },
    };
    Ok(VerifiedImportedOrigin {
        subject: grant.subject.clone(),
        motion_file_id: grant.motion_file_id.clone(),
        metadata_command: metadata_activation.clone(),
        physical_command: physical_activation.clone(),
        root_manifest_digest: grant.root_manifest_digest.clone(),
        origin_audit_digest: grant.origin_audit_digest.clone(),
        physical: grant.physical.clone(),
        origin,
        observed_generation: expected_generation,
        proof_digest,
    })
}

#[cfg(test)]
mod tests;
