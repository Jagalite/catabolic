//! Private borrowed evidence reconstruction for the draft bootstrap owner.
//! Inputs are descriptions plus independently prepared, nonserde host audits.
//! Neither this entry nor a historical receipt mints current source authority.
use super::{
    NfoCreatedSourceAuthority, NfoImportAuthority, NfoImportedOriginAuthority, VerifiedNfoSource,
    bootstrap, proof_budget::ProofBudget, verify_created_no_origin_baseline_in,
    verify_imported_no_origin_baseline_in, verify_imported_origin_baseline_in,
    verify_imported_origin_in, verify_source_history_in,
};
use crate::{
    Error, Result, Store,
    catalog_commands::TrustedCommandAuthority,
    catalog_import::{ImportBatch, SnapshotManifest},
    migration::BootstrapWorkspace,
    operation_budget::BudgetCharge,
};
use catabolic_core::catalog_command::{CommandEnvelope, ReceiptVisibility};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Scope<'a> {
    pub catalog_id: &'a str,
    pub incarnation: &'a str,
    pub host_namespace: &'a str,
    pub expected_generation: i64,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct ImportPart<'a> {
    pub manifest: &'a SnapshotManifest,
    pub batches: &'a [ImportBatch],
    pub activation: &'a CommandEnvelope,
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum ImportOriginEvidence<'a> {
    NoOrigin,
    Physical {
        part: ImportPart<'a>,
        audit: &'a NfoImportedOriginAuthority,
    },
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum BaselineEvidence<'a> {
    NativeCreated {
        first: &'a CommandEnvelope,
        audit: &'a NfoCreatedSourceAuthority,
    },
    Imported {
        subject: &'a str,
        metadata: ImportPart<'a>,
        audit: &'a NfoImportAuthority,
        origin: ImportOriginEvidence<'a>,
    },
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct SourceEvidence<'a> {
    pub baseline: BaselineEvidence<'a>,
    /// Exact revision order after revision one. No receipt lookup invents the
    /// missing immutable request, and no subset of later commands is accepted.
    pub later_commands: &'a [CommandEnvelope],
}
impl SourceEvidence<'_> {
    fn subject(&self) -> &str {
        match self.baseline {
            BaselineEvidence::NativeCreated { audit, .. } => &audit.subject,
            BaselineEvidence::Imported { subject, .. } => subject,
        }
    }
    fn first(&self) -> &CommandEnvelope {
        match self.baseline {
            BaselineEvidence::NativeCreated { first, .. } => first,
            BaselineEvidence::Imported { metadata, .. } => metadata.activation,
        }
    }
}
fn refused(message: impl std::fmt::Display) -> Error {
    Error(format!("catalog_nfo_bootstrap_entry_refused: {message}"))
}
fn check_in(
    store: &Store,
    scope: Scope<'_>,
    authority: TrustedCommandAuthority<'_>,
    control: Option<&BootstrapWorkspace<'_>>,
) -> Result<()> {
    if store.schema_version != 41 || store.db.is_autocommit() || !store.writable {
        return Err(refused("candidate41 retained writer required"));
    }
    store.transaction_available_with_control_workspace(control)?;
    if authority.allow_new_commands
        || authority.receipt_visibility != ReceiptVisibility::FullResult
        || scope.host_namespace != authority.host_namespace
        || scope.host_namespace.is_empty()
        || scope.host_namespace.len() > 256
        || scope.host_namespace.chars().any(char::is_control)
        || scope.catalog_id != store.database_id
        || Some(scope.incarnation) != store.catalog_incarnation.as_deref()
    {
        return Err(refused(
            "exact current catalog and receipt-only authority required",
        ));
    }
    let exact: bool = store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM catalog_state WHERE id=1 AND catalog_id=? AND incarnation=? AND generation=?)",
        rusqlite::params![scope.catalog_id,scope.incarnation,scope.expected_generation],
        |row| row.get(0),
    )?;
    if !exact {
        return Err(refused("current catalog/incarnation/generation changed"));
    }
    Ok(())
}

/// Admit the whole borrowed list before allocating its index or output vector.
/// Existing per-source/root/history admission stays intact. Every child borrows
/// the SAME inherited budget; there is no per-subject operation-budget reset.
/// Shared import roots are borrowed, never cloned into each source entry. Their
/// existing verifiers still independently authenticate the complete manifests.
/// Only list/key bookkeeping is traversed here; full payloads are not cloned,
/// so their independent child admission is not gratuitously repeated here.
fn admit_list(store: &Store, sources: &[SourceEvidence<'_>], host: &str) -> Result<()> {
    let shared = store.catalog_operation_budget()?;
    let bytes = sources
        .len()
        .checked_mul(std::mem::size_of::<VerifiedNfoSource>() + 4 * std::mem::size_of::<&str>())
        .ok_or_else(|| refused("source-list allocation overflow"))?;
    shared
        .charge(BudgetCharge {
            items: sources.len(),
            bytes,
            values: sources.len(),
            work: sources.len(),
        })
        .map_err(|error| refused(format!("complete source-list admission: {error:?}")))?;
    let mut input = ProofBudget::new(shared);
    for source in sources {
        let first = source.first();
        input.serialized(
            &(
                source.subject(),
                first.key().catalog_id(),
                first.key().host_namespace(),
                first.key().operation_id(),
                first.digest().as_str(),
                source.later_commands.len(),
            ),
            4,
        )?;
        if source.first().key().host_namespace() != host {
            return Err(refused(
                "source evidence host differs from exact read authority",
            ));
        }
        for command in source.later_commands {
            input.serialized(
                &(
                    command.key().catalog_id(),
                    command.key().host_namespace(),
                    command.key().operation_id(),
                    command.digest().as_str(),
                ),
                4,
            )?;
            if command.key().host_namespace() != host {
                return Err(refused(
                    "later evidence host differs from exact read authority",
                ));
            }
        }
    }
    let mut seen = BTreeSet::new();
    for source in sources {
        if !seen.insert(source.subject()) {
            return Err(refused("duplicate source subject"));
        }
    }
    Ok(())
}

pub(crate) fn prepare_in(
    store: &Store,
    scope: Scope<'_>,
    evidence: &[SourceEvidence<'_>],
    authority: TrustedCommandAuthority<'_>,
) -> Result<bootstrap::BootstrapPlan> {
    prepare_in_with_control_workspace(store, scope, evidence, authority, None)
}

pub(crate) fn prepare_in_with_control_workspace(
    store: &Store,
    scope: Scope<'_>,
    evidence: &[SourceEvidence<'_>],
    authority: TrustedCommandAuthority<'_>,
    control: Option<&BootstrapWorkspace<'_>>,
) -> Result<bootstrap::BootstrapPlan> {
    check_in(store, scope, authority, control)?;
    // The fixed bootstrap workspace does not admit per-source proof work.
    // Keep the ordinary verifiers below unreachable from a controlled path.
    if control.is_some() && !evidence.is_empty() {
        return Err(refused("controlled bootstrap requires empty source evidence"));
    }
    admit_list(store, evidence, authority.host_namespace)?;
    let mut sources = Vec::new();
    sources.try_reserve_exact(evidence.len()).map_err(refused)?;
    for source in evidence {
        let baseline = match source.baseline {
            BaselineEvidence::NativeCreated { first, audit } => {
                verify_created_no_origin_baseline_in(
                    store,
                    first,
                    audit,
                    authority,
                    scope.expected_generation,
                )?
            }
            BaselineEvidence::Imported {
                subject,
                metadata,
                audit,
                origin,
            } => match origin {
                ImportOriginEvidence::NoOrigin => verify_imported_no_origin_baseline_in(
                    store,
                    subject,
                    metadata.manifest,
                    metadata.batches,
                    metadata.activation,
                    audit,
                    authority,
                    scope.expected_generation,
                )?,
                ImportOriginEvidence::Physical {
                    part,
                    audit: origin_audit,
                } => {
                    let origin = verify_imported_origin_in(
                        store,
                        metadata.activation,
                        part.manifest,
                        part.batches,
                        part.activation,
                        origin_audit,
                        authority,
                        scope.expected_generation,
                    )?;
                    verify_imported_origin_baseline_in(
                        store,
                        subject,
                        metadata.manifest,
                        metadata.batches,
                        metadata.activation,
                        audit,
                        &origin,
                        authority,
                        scope.expected_generation,
                    )?
                }
            },
        };
        sources.push(verify_source_history_in(
            store,
            &baseline,
            source.later_commands,
            authority,
            scope.expected_generation,
        )?);
    }
    // The supplied list never establishes inventory completeness. This owner
    // recaptures every current/historical/tag-only obligation independently.
    bootstrap::prepare_in_with_control_workspace(store, &sources, control)
}

/// Invoke only after the normal receipt engine chooses Execute, under its sole
/// retained writer. Replay/expiry must not call this function or rebuild audits.
/// A preview may create the exact digest payload, but its tokens are discarded;
/// this entry rebuilds all proofs at the command fence before any state writes.
pub(crate) fn apply_in(
    store: &Store,
    outer: &CommandEnvelope,
    evidence: &[SourceEvidence<'_>],
    authority: TrustedCommandAuthority<'_>,
) -> Result<Value> {
    apply_in_with_control_workspace(store, outer, evidence, authority, None)
}

pub(crate) fn apply_in_with_control_workspace(
    store: &Store,
    outer: &CommandEnvelope,
    evidence: &[SourceEvidence<'_>],
    authority: TrustedCommandAuthority<'_>,
    control: Option<&BootstrapWorkspace<'_>>,
) -> Result<Value> {
    if outer.key().host_namespace() != authority.host_namespace {
        return Err(refused(
            "outer command host differs from exact read authority",
        ));
    }
    let plan = prepare_in_with_control_workspace(
        store,
        Scope {
            catalog_id: outer.key().catalog_id(),
            incarnation: outer.admission_incarnation(),
            host_namespace: outer.key().host_namespace(),
            expected_generation: outer.expected_generation(),
        },
        evidence,
        authority,
        control,
    )?;
    bootstrap::apply_in_with_control_workspace(store, outer, plan, control)
}
