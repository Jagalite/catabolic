//! Authenticate original IDs from the complete committed import output, then
//! account for unchanged assertions and exact recorded replacements. Historical
//! evidence is not current custody;
//! unavailable output and unsupported transitions remain explicit obligations.
mod replacement;
use super::{ImportedRequestedNames, NfoImportAuthority, ProofBudget};
use crate::{
    Error, Result, Store,
    catalog_commands::TrustedCommandAuthority,
    catalog_import::{
        self, ImportMutation, ImportReceipt, ImportStep, LogicalMapping, SnapshotManifest,
    },
    operation_budget::BudgetCharge,
};
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey, MAX_COMMAND_JSON_DEPTH};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const NAMESPACE: &str = "motion.nfo";
fn refused(reason: &str) -> Error {
    Error(format!(
        "catalog_nfo_imported_tag_mapping_refused: {reason}"
    ))
}
#[derive(Debug, Serialize)]
struct Assertion {
    requested_name: String,
    tag_id: String,
    assertion_id: String,
}
/// Original requested-name to ID mapping, authenticated by the complete output
/// commitment in the exact verified Activate envelope. No current state claim.
#[derive(Debug)]
pub(in crate::catalog_nfo_attribution) struct ImportedTagMapping {
    subject: String,
    source_revision: i64,
    activation_digest: String,
    assertions: BTreeMap<String, Assertion>,
    digest: String,
}
/// A historical source can remain verified while current ownership is unproved.
/// This enum is private/nonserde, not a caller-supplied admission flag.
#[derive(Debug)]
pub(in crate::catalog_nfo_attribution) enum ImportedTagAdmission {
    Accounted { digest: String },
    MissingMapping,
    UnprovedHistory,
    UnprovedVocabulary,
    ChangedAssertions,
}
impl ImportedTagAdmission {
    pub(in crate::catalog_nfo_attribution) fn digest(&self) -> Option<&str> {
        match self {
            Self::Accounted { digest } => Some(digest),
            _ => None,
        }
    }
    pub(in crate::catalog_nfo_attribution) fn require(&self) -> Result<()> {
        match self {
            Self::Accounted { .. } => Ok(()),
            Self::MissingMapping => Err(refused("complete imported tag mapping unavailable")),
            Self::UnprovedHistory => Err(refused("recorded imported tag history remains unproved")),
            Self::UnprovedVocabulary => Err(refused(
                "historical requested-name mapping remains unproved",
            )),
            Self::ChangedAssertions => {
                Err(refused("imported assertion transition remains unproved"))
            }
        }
    }
}

/// Select a full body only after exact current FullResult authority and retained
/// commit verification. Result bytes are candidate data, never replay or grants.
/// An expired body is never reconstructed, reinstated or written back.
fn full_receipt(
    store: &Store,
    command: &CommandEnvelope,
    authority: TrustedCommandAuthority<'_>,
    generation: i64,
    budget: &mut ProofBudget,
) -> Result<Option<ImportReceipt>> {
    budget.charge(BudgetCharge {
        items: 1,
        values: 2,
        work: 3,
        ..Default::default()
    })?;
    let present: bool = store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM catalog_command_receipts WHERE catalog_id=? AND host_namespace=? AND operation_id=?)",
        params![command.key().catalog_id(), command.key().host_namespace(), command.key().operation_id()],
        |row| row.get(0),
    )?;
    if !present {
        return Ok(None);
    }
    super::super::verify_committed_command_in(store, command, authority, generation)?;
    let mut query = store.db.prepare(
        "SELECT result_state,result_json FROM catalog_command_receipts WHERE catalog_id=? AND host_namespace=? AND operation_id=?",
    )?;
    let mut rows = query.query(params![
        command.key().catalog_id(),
        command.key().host_namespace(),
        command.key().operation_id()
    ])?;
    let row = rows
        .next()?
        .ok_or_else(|| refused("receipt disappeared in writer"))?;
    let state = row
        .get_ref(0)?
        .as_str()
        .map_err(|_| refused("receipt state is not text"))?;
    if state == "expired" {
        return Ok(None);
    }
    if state != "full" {
        return Err(refused("invalid receipt state"));
    }
    let raw = row
        .get_ref(1)?
        .as_str()
        .map_err(|_| refused("full receipt body absent"))?;
    budget.raw_json(raw, MAX_COMMAND_JSON_DEPTH)?;
    let value =
        catabolic_core::json::parse(raw, true).map_err(|_| refused("invalid full mapping body"))?;
    // Consumes the duplicate-safe tree. It never accepts a lossy Value parse.
    serde_json::from_value(value)
        .map(Some)
        .map_err(|_| refused("malformed full import result"))
}

#[derive(Serialize)]
struct CompleteMapping<'a> {
    contract_version: u32,
    mappings: &'a [LogicalMapping],
    typed_mappings: &'a [Value],
}
pub(super) fn authenticate_mapping(
    mappings: &[LogicalMapping],
    typed: &[Value],
    expected: &str,
    budget: &mut ProofBudget,
) -> Result<()> {
    // The existing owner constructs a JSON view and its serialized Value copy,
    // then a canonical string. Reserve each represented allocation first. This
    // borrowed view is accounting only; the owner's digest is the sole encoder.
    if typed.is_empty() {
        #[derive(Serialize)]
        struct LogicalView<'a> {
            contract_version: u32,
            mappings: &'a [LogicalMapping],
        }
        let view = LogicalView {
            contract_version: catalog_import::IMPORT_CONTRACT_VERSION,
            mappings,
        };
        budget.serialized(&view, MAX_COMMAND_JSON_DEPTH)?;
        budget.serialized(&view, MAX_COMMAND_JSON_DEPTH)?;
        budget.canonical_serialized(&view, MAX_COMMAND_JSON_DEPTH)?;
    } else {
        let view = CompleteMapping {
            contract_version: 2,
            mappings,
            typed_mappings: typed,
        };
        budget.serialized(&view, MAX_COMMAND_JSON_DEPTH)?;
        budget.serialized(&view, MAX_COMMAND_JSON_DEPTH)?;
        budget.canonical_serialized(&view, MAX_COMMAND_JSON_DEPTH)?;
    }
    if catalog_import::complete_mapping_digest(mappings, typed)
        .map_err(|_| refused("complete mapping encoding failed"))?
        != expected
    {
        return Err(refused("complete import mapping digest mismatch"));
    }
    Ok(())
}
fn identifier(value: &Value) -> Result<&str> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
        .ok_or_else(|| refused("invalid bounded mapped identity"))
}

pub(super) fn read_mapping_in(
    store: &Store,
    names: &ImportedRequestedNames,
    manifest: &SnapshotManifest,
    activation: &CommandEnvelope,
    grant: &NfoImportAuthority,
    authority: TrustedCommandAuthority<'_>,
    generation: i64,
    budget: &mut ProofBudget,
) -> Result<Option<ImportedTagMapping>> {
    // An explicit empty archive needs no original IDs. Its separate terminal
    // proof still requires zero complete current rows and no later tag history.
    if names.tags().is_empty() {
        return Ok(None);
    }
    let full = full_receipt(store, activation, authority, generation, budget)?;
    let (mappings, typed) = if let Some(full) = full {
        match full {
            ImportReceipt::Activated {
                contract_version,
                catalog_id,
                incarnation,
                import_id,
                input_digest,
                source_snapshot_id,
                reference_ack_id,
                mapping_digest,
                mappings,
                typed_results,
                ..
            } if contract_version == manifest.contract_version
                && catalog_id == activation.key().catalog_id()
                && incarnation == activation.admission_incarnation()
                && import_id == grant.import_id
                && input_digest == grant.input_digest
                && source_snapshot_id == manifest.source_snapshot_id
                && reference_ack_id == grant.reference_ack_id
                && mapping_digest == grant.mapping_digest =>
            {
                (mappings, typed_results)
            }
            _ => return Err(refused("full activation mapping scope mismatch")),
        }
    } else {
        // Validate's exact payload/generation is fixed by the verified Activate
        // contract. Its complete reviewed output is only a candidate until the
        // Activate mapping commitment authenticates it below.
        budget.charge(BudgetCharge {
            bytes: (grant.import_id.len()
                + grant.input_digest.len()
                + activation.key().catalog_id().len()
                + activation.key().host_namespace().len()
                + activation.admission_incarnation().len())
                * 8
                + 2048,
            values: 32,
            work: 64,
            ..Default::default()
        })?;
        let payload = json!({"operation":"import","mutation":ImportMutation::Validate {
            import_id:grant.import_id.clone(),input_digest:grant.input_digest.clone(),
        }});
        budget.serialized(&payload, MAX_COMMAND_JSON_DEPTH)?;
        let validate = CommandEnvelope::new(
            activation.contract_version(),
            CommandKey::new(
                activation.key().catalog_id(),
                activation.key().host_namespace(),
                catalog_import::operation_id(&grant.import_id, ImportStep::Validate)
                    .map_err(|_| refused("validate key"))?,
            )
            .map_err(|_| refused("validate key"))?,
            activation.admission_incarnation(),
            activation.expected_generation(),
            &payload.to_string(),
        )
        .map_err(|_| refused("validate envelope"))?;
        let Some(full) = full_receipt(store, &validate, authority, generation, budget)? else {
            return Ok(None);
        };
        match full {
            ImportReceipt::ValidatedStaging {
                contract_version,
                import_id,
                input_digest,
                validation_generation,
                proposed_mappings,
                unresolved_evidence: 0,
                activation_supported: true,
                mapping_digest,
                proposed_typed_mappings,
            } if contract_version == manifest.contract_version
                && import_id == grant.import_id
                && input_digest == grant.input_digest
                && validation_generation == activation.expected_generation()
                && mapping_digest == grant.mapping_digest =>
            {
                (proposed_mappings, proposed_typed_mappings)
            }
            _ => return Err(refused("full validated mapping scope mismatch")),
        }
    };
    authenticate_mapping(&mappings, &typed, &grant.mapping_digest, budget)?;
    let mut assertions = BTreeMap::new();
    let mut requested = BTreeSet::new();
    let mut snapshot = false;
    for value in &typed {
        budget.charge(BudgetCharge {
            work: 1,
            ..Default::default()
        })?;
        if value["item_id"] != names.subject() || value["namespace"] != NAMESPACE {
            continue;
        }
        match value["owner"].as_str() {
            Some("tags") => {
                let object = value
                    .as_object()
                    .ok_or_else(|| refused("mapped assertion is not object"))?;
                if object.len() != 7
                    || value["source_revision"].as_i64() != Some(names.source_revision())
                {
                    return Err(refused("mapped assertion fields/revision mismatch"));
                }
                let name = identifier(&value["tag"])?;
                let tag_id = identifier(&value["tag_id"])?;
                let id = identifier(&value["assertion_id"])?;
                if !names.tags().contains(name)
                    || !requested.insert(name)
                    || assertions.contains_key(id)
                {
                    return Err(refused("duplicate or unexpected imported tag mapping"));
                }
                budget.serialized(&(name, tag_id, id, id), MAX_COMMAND_JSON_DEPTH)?;
                assertions.insert(
                    id.to_owned(),
                    Assertion {
                        requested_name: name.to_owned(),
                        tag_id: tag_id.to_owned(),
                        assertion_id: id.to_owned(),
                    },
                );
            }
            Some("tag_source_snapshot") => {
                if snapshot
                    || value.as_object().is_none_or(|o| o.len() != 6)
                    || value["source_revision"].as_i64() != Some(names.source_revision())
                    || value["assertion_count"].as_u64() != Some(names.tags().len() as u64)
                    || value["suppression_count"] != 0
                {
                    return Err(refused("imported tag snapshot mismatch"));
                }
                snapshot = true;
            }
            Some("tag_suppressions") => {
                return Err(refused("NFO import contains suppression output"));
            }
            _ => (),
        }
    }
    if !snapshot || assertions.len() != names.tags().len() || requested.len() != names.tags().len()
    {
        return Err(refused("complete imported assertion mapping absent"));
    }
    let digest = budget.serialized(
        &(
            "catabolic.nfo.imported-tag-mapping.v1",
            names.digest(),
            activation.digest().as_str(),
            &grant.mapping_digest,
            &assertions,
        ),
        MAX_COMMAND_JSON_DEPTH,
    )?;
    budget.serialized(
        &(names.subject(), activation.digest().as_str()),
        MAX_COMMAND_JSON_DEPTH,
    )?;
    Ok(Some(ImportedTagMapping {
        subject: names.subject().into(),
        source_revision: names.source_revision(),
        activation_digest: activation.digest().as_str().into(),
        assertions,
        digest,
    }))
}

fn text(
    row: &rusqlite::Row<'_>,
    column: usize,
    maximum: usize,
    budget: &mut ProofBudget,
) -> Result<String> {
    let value = row
        .get_ref(column)?
        .as_str()
        .map_err(|_| refused("current assertion text malformed"))?;
    if value.len() > maximum {
        return Err(refused("current assertion text exceeds bound"));
    }
    budget.charge(BudgetCharge {
        bytes: value.len(),
        values: 1,
        work: value.len() / 64 + 1,
        ..Default::default()
    })?;
    Ok(value.to_owned())
}

pub(in crate::catalog_nfo_attribution) fn accounted_in(
    store: &Store,
    names: &ImportedRequestedNames,
    mapping: Option<&ImportedTagMapping>,
    activation: &CommandEnvelope,
    commands: &[CommandEnvelope],
    generation: i64,
) -> Result<ImportedTagAdmission> {
    replacement::accounted_in(store, names, mapping, activation, commands, generation)
}
