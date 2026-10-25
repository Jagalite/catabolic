//! UNREGISTERED schema41 bootstrap owner candidate. No dispatcher references this
//! module yet. All source proofs are native-produced, nonserialized tokens; a
//! caller's list or digest is never inventory completeness authority.
use super::{
    VerifiedNfoSource,
    bootstrap_inventory::{self, BootstrapInventory},
    proof_budget::ProofBudget,
};
use crate::{Error, Result, Store, migration::BootstrapWorkspace, operation_budget::BudgetCharge};
use catabolic_core::catalog_command::CommandEnvelope;
use rusqlite::params;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

fn refused(reason: impl std::fmt::Display) -> Error {
    Error(format!("catalog_nfo_bootstrap_refused: {reason}"))
}
#[derive(Debug, serde::Serialize)]
struct Seed {
    subject: String,
    source_revision: i64,
    public_incarnation: i64,
    public_revision: i64,
    document_identity: Option<String>,
    active_identity: Option<String>,
    origin_json: Option<String>,
    origin_file: Option<(String, String)>,
    origin_physical_json: Option<String>,
    origin_proof_digest: Option<String>,
    source_proof_digest: String,
    audit_digest: String,
}
/// This value cannot be deserialized or constructed outside this module. It is
/// not durable recovery state: retry resolves the saved command receipt first,
/// then rederives this plan under the same current writer for an absent receipt.
#[derive(Debug)]
pub(crate) struct BootstrapPlan {
    catalog_id: String,
    incarnation: String,
    generation: i64,
    digest: String,
    inventory: BootstrapInventory,
    rows: Vec<Seed>,
}
impl BootstrapPlan {
    pub(crate) fn payload(&self) -> Value {
        json!({"operation":"nfo_bootstrap","contract_version":1,"inventory_digest":self.digest})
    }
}
fn admission(store: &Store, control: Option<&BootstrapWorkspace<'_>>) -> Result<(String, i64)> {
    if store.schema_version != 41 || store.db.is_autocommit() {
        return Err(refused("explicit schema41 retained writer required"));
    }
    store.transaction_available_with_control_workspace(control)?;
    let (catalog, incarnation, generation): (String, String, i64) = store.db.query_row(
        "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if catalog != store.database_id || Some(&incarnation) != store.catalog_incarnation.as_ref() {
        return Err(refused("catalog/incarnation admission changed"));
    }
    let finished: bool = store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM catalog_nfo_bootstrap)",
        [],
        |r| r.get(0),
    )?;
    if finished {
        return Err(refused(
            "bootstrap already finalized; reconcile its receipt",
        ));
    }
    Ok((incarnation, generation))
}

fn hash_part(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value);
}

/// Only verified complete source histories enter this function. Their scope and
/// every actual head are independently rechecked, including subjects with no
/// current head: those are unresolved obligations, never omitted from the captured ledger.
pub(super) fn prepare_in(store: &Store, sources: &[VerifiedNfoSource]) -> Result<BootstrapPlan> {
    prepare_in_with_control_workspace(store, sources, None)
}

pub(super) fn prepare_in_with_control_workspace(
    store: &Store,
    sources: &[VerifiedNfoSource],
    control: Option<&BootstrapWorkspace<'_>>,
) -> Result<BootstrapPlan> {
    let (incarnation, generation) = admission(store, control)?;
    let inventory = bootstrap_inventory::capture_with_control_workspace(store, control)?;
    inventory.require_complete()?;
    let subjects = inventory.subjects();
    if sources.len() != subjects.len() {
        return Err(refused("source proof coverage is not complete"));
    }
    if control.is_some() && !sources.is_empty() {
        return Err(refused("controlled bootstrap requires empty source proofs"));
    }
    let mut by_subject = BTreeMap::new();
    for source in sources {
        if source.catalog_id() != store.database_id
            || source.incarnation() != incarnation
            || source.observed_generation() != generation
            || !subjects.contains_key(source.subject())
            || by_subject.insert(source.subject(), source).is_some()
        {
            return Err(refused("duplicate, foreign, extra or stale source proof"));
        }
    }
    let mut rows = Vec::new();
    let mut claims = BTreeSet::new();
    let mut hash = Sha256::new();
    for value in [
        "catabolic.nfo.bootstrap.v1",
        &store.database_id,
        &incarnation,
    ] {
        hash_part(&mut hash, value.as_bytes());
    }
    hash_part(&mut hash, &generation.to_le_bytes());
    hash_part(&mut hash, inventory.digest().as_bytes());
    for (subject, proof) in by_subject {
        // Historical identity/origin evidence is insufficient to import current
        // nonempty tag ownership. Require authenticated imported IDs and complete
        // closure through accounted replacements or tag-preserving v1 Patch.
        // Missing mappings and unexplained later transitions remain obligations.
        if let Some(admission) = proof.imported_tag_admission() {
            admission.require()?;
        }
        let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
        budget.serialized(proof.current_source().fields(), 32)?;
        budget.canonical_json(proof.current_source().evidence())?;
        let expected_fields = serde_json::to_string(proof.current_source().fields())?;
        let expected_evidence = catabolic_core::encode(proof.current_source().evidence());
        budget.charge(BudgetCharge {
            bytes: 128,
            values: 2,
            work: (expected_fields.len() + expected_evidence.len()) / 64 + 2,
            ..Default::default()
        })?;
        let description = &subjects[subject];
        if !description.source().is_some_and(|head| {
            head.matches(
                proof.current_source().revision(),
                proof.current_source().schema_fingerprint(),
                &expected_fields,
                &expected_evidence,
            )
        }) || description.schema().map(|schema| schema.fingerprint())
            != Some(proof.current_schema().fingerprint())
        {
            return Err(refused(
                "source/schema head no longer matches complete captured proof",
            ));
        }
        let retired = description.retired();
        // Charge every retained/duplicated scalar and the serialized origin
        // before allocating the per-source seed row.
        budget.serialized(
            &(
                subject,
                proof.current_source().revision(),
                proof.automatic_identity(),
                proof.automatic_identity(),
                proof.origin(),
                proof
                    .origin()
                    .map(|origin| (&origin.file.profile, &origin.file.file_id)),
                proof.origin_physical(),
                proof.origin_proof_digest(),
                proof.proof_digest(),
                proof.attribution_audit_digest(),
            ),
            64,
        )?;
        let identity = proof.automatic_identity().map(str::to_owned);
        let active = if retired { None } else { identity.clone() };
        if let Some(value) = &active {
            if !claims.insert(value.clone()) {
                return Err(refused("duplicate automatic identity"));
            }
            let conflict: bool=store.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM catalog_identity_pins WHERE namespace='nfo' AND value=? AND item_id<>?)",
                params![value,subject],|r|r.get(0))?;
            if conflict {
                return Err(refused(
                    "automatic identity conflicts with exact manual Pins owner",
                ));
            }
        }
        budget.serialized(&proof.origin(), 64)?;
        let origin_json = proof.origin().map(serde_json::to_string).transpose()?;
        budget.serialized(&proof.origin_physical(), 64)?;
        let origin_physical_json = proof
            .origin_physical()
            .map(serde_json::to_string)
            .transpose()?;
        if origin_physical_json
            .as_ref()
            .is_some_and(|raw| raw.len() > 65536)
        {
            return Err(refused(
                "imported physical origin exceeds retained batch bound",
            ));
        }
        let origin_proof_digest = proof.origin_proof_digest().map(str::to_owned);
        let origin_file = proof
            .origin()
            .map(|origin| (origin.file.profile.clone(), origin.file.file_id.clone()));
        let seed = Seed {
            subject: subject.into(),
            source_revision: proof.current_source().revision(),
            public_incarnation: 1,
            public_revision: proof.current_source().revision(),
            document_identity: identity,
            active_identity: active,
            origin_json,
            origin_file,
            origin_physical_json,
            origin_proof_digest,
            source_proof_digest: proof.proof_digest().into(),
            audit_digest: proof.attribution_audit_digest().into(),
        };
        // This digest commits the exact effects, not just inventory membership.
        let effects_digest = budget.serialized(&seed, 64)?;
        hash_part(&mut hash, subject.as_bytes());
        hash_part(&mut hash, effects_digest.as_bytes());
        rows.push(seed);
    }
    Ok(BootstrapPlan {
        catalog_id: store.database_id.clone(),
        incarnation,
        generation,
        digest: format!("{:x}", hash.finalize()),
        inventory,
        rows,
    })
}

/// Invoked only by the future durable command dispatcher after normal receipt
/// reconciliation. The outer owner supplies the sole transaction and receipt;
/// errors roll back every row. No metadata source or manual Pins are rewritten.
pub(super) fn apply_in(
    store: &Store,
    command: &CommandEnvelope,
    plan: BootstrapPlan,
) -> Result<Value> {
    apply_in_with_control_workspace(store, command, plan, None)
}

pub(super) fn apply_in_with_control_workspace(
    store: &Store,
    command: &CommandEnvelope,
    plan: BootstrapPlan,
    control: Option<&BootstrapWorkspace<'_>>,
) -> Result<Value> {
    let (incarnation, generation) = admission(store, control)?;
    if plan.catalog_id != store.database_id
        || plan.incarnation != incarnation
        || plan.generation != generation
        || command.key().catalog_id() != store.database_id
        || command.admission_incarnation() != incarnation
        || command.expected_generation() != generation
        || command.payload() != &plan.payload()
        || bootstrap_inventory::capture_with_control_workspace(store, control)?.digest()
            != plan.inventory.digest()
    {
        return Err(refused("bootstrap plan/command/inventory changed"));
    }
    let existing: bool=store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM catalog_nfo_states UNION ALL SELECT item_id FROM catalog_nfo_state_revisions UNION ALL SELECT item_id FROM catalog_nfo_identity_claims UNION ALL SELECT item_id FROM catalog_nfo_file_attributions)",[],|r|r.get(0))?;
    if existing {
        return Err(refused("bootstrap has uncommitted or foreign state"));
    }
    let source_count = i64::try_from(plan.rows.len())
        .map_err(|_| refused("bootstrap source count exceeds SQLite integer range"))?;
    if control.is_some() && source_count != 0 {
        return Err(refused("controlled bootstrap requires zero source count"));
    }
    for row in &plan.rows {
        store
            .catalog_operation_budget()?
            .charge(BudgetCharge {
                items: 1,
                values: 12,
                bytes: row.origin_json.as_ref().map_or(0, String::len)
                    + row.origin_physical_json.as_ref().map_or(0, String::len)
                    + 2048,
                work: 20,
            })
            .map_err(|error| refused(format!("{error:?}")))?;
        store.db.execute("INSERT INTO catalog_nfo_state_revisions(item_id,revision,source_revision,public_incarnation,public_revision,document_identity,active_identity,origin_json,origin_physical_json,origin_proof_digest,source_proof_digest,attribution_audit_digest,command_catalog_id,command_host_namespace,command_operation_id) VALUES(?,1,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![row.subject,row.source_revision,row.public_incarnation,row.public_revision,row.document_identity,row.active_identity,row.origin_json,row.origin_physical_json,row.origin_proof_digest,row.source_proof_digest,row.audit_digest,command.key().catalog_id(),command.key().host_namespace(),command.key().operation_id()])?;
        store.db.execute(
            "INSERT INTO catalog_nfo_states(item_id,current_revision) VALUES(?,1)",
            [&row.subject],
        )?;
        if let Some(value) = &row.active_identity {
            store.db.execute("INSERT INTO catalog_nfo_identity_claims(value,item_id,state_revision) VALUES(?,?,1)",params![value,row.subject])?;
        }
        if let Some((profile, file)) = &row.origin_file {
            store.db.execute("INSERT INTO catalog_nfo_file_attributions(item_id,profile,file_id,state_revision) VALUES(?,?,?,1)",params![row.subject,profile,file])?;
        }
    }
    store.db.execute("INSERT INTO catalog_nfo_bootstrap(id,catalog_id,incarnation,inventory_digest,source_count,command_catalog_id,command_host_namespace,command_operation_id) VALUES(1,?,?,?,?,?,?,?)",
        params![plan.catalog_id,plan.incarnation,plan.digest,source_count,command.key().catalog_id(),command.key().host_namespace(),command.key().operation_id()])?;
    Ok(json!({"contract_version":1,"source_count":plan.rows.len(),"inventory_digest":plan.digest}))
}
