// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Historical evidence for the planned NFO attribution owner. This module does
//! not register schema41, expose attribution writes, admit refresh, or change replay.
//! A proven historical command is not current mutation or filesystem authority.

mod first_source;
pub(crate) mod refresh;
pub(crate) use first_source::VerifiedFirstNfoAbsence;
mod proof_budget;
pub(crate) use proof_budget::ProofBudget;
mod provenance;
pub use provenance::{VerifiedCommittedCommand, verify_committed_command_in};

mod import_provenance;
pub(crate) use import_provenance::{
    NfoImportAuthority, verify_imported_no_origin_baseline_in, verify_imported_origin_baseline_in,
};

mod history;
pub(crate) use history::{VerifiedNfoSource, verify_source_history_in};

/// Exact independently established revision-one NFO provenance. Only attribution
/// children may construct this token; it is neither a serialized grant nor a
/// substitute for rechecking the retained command under the current writer.
#[derive(Debug)]
pub(crate) struct VerifiedNfoBaseline {
    subject: String,
    revision: i64,
    fields_json: String,
    evidence_json: String,
    raw_mutation_json: String,
    schema_fingerprint: String,
    source_kind: catabolic_core::catalog_metadata::SourceKind,
    command: catabolic_core::catalog_command::CommandEnvelope,
    automatic_identity: Option<String>,
    origin: Option<catabolic_core::catalog_metadata::nfo_refresh::FileOrigin>,
    attribution_audit_digest: String,
    origin_proof_digest: Option<String>,
    origin_physical: Option<crate::catalog_import_typed::PhysicalMapping>,
    /// Exact archived requested names, not proof of current assertion IDs or
    /// vocabulary lineage. None means this imported-name proof was not minted;
    /// it must never be interpreted as an empty-tag proof.
    imported_requested_names: Option<import_provenance::ImportedRequestedNames>,
    /// Historical IDs authenticated by the complete Activate mapping commitment.
    /// Missing retained output is an explicit current-ownership obligation.
    imported_tag_mapping: Option<import_provenance::ImportedTagMapping>,
}

mod physical_provenance;
pub(crate) use physical_provenance::{
    NfoImportedOriginAuthority, VerifiedImportedOrigin, verify_imported_origin_in,
};

mod native_created;
pub(crate) use native_created::{NfoCreatedSourceAuthority, verify_created_no_origin_baseline_in};

// Compile-only candidate: no dispatcher, host export or schema registration.
mod bootstrap;
mod bootstrap_completion;
pub(crate) use bootstrap_completion::{
    VerifiedBootstrapCompletion,
    verify_historical_receipt_in as verify_historical_bootstrap_receipt_in,
    verify_historical_receipt_in_with_control_workspace as verify_historical_bootstrap_receipt_in_with_control_workspace,
    verify_in as verify_bootstrap_completion_in,
    verify_in_with_control_workspace as verify_bootstrap_completion_in_with_control_workspace,
};
pub(crate) mod bootstrap_entry;
mod bootstrap_inventory;
pub mod inventory;

// Descriptive current-state candidate only; not a source-write capability.
mod state;

mod permit;
pub(crate) mod source_effect;
pub(crate) use permit::NfoWritePermit;

pub(crate) mod matching;
mod public_put;
mod publication;
pub(crate) mod retirement;

#[cfg(test)]
mod candidate_tests;

#[cfg(test)]
pub(crate) use candidate_tests::with_imported40_bootstrap_fixture;

pub(crate) use bootstrap_completion::verify_restored_historical_receipt_in;
