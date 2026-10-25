//! Private first-source absence capability. Missing native rows are consistency
//! checks, never authority: the retained host must independently audit the whole
//! original/current identity and sidecar inventory before supplying this grant.
//! Native-created and untouched imported roots retain distinct verified
//! baselines. Neither kind is a substitute for restored/evolved ancestry.
pub(super) mod imported;
use super::{
    proof_budget::ProofBudget,
    state::{self, CurrentNfoState},
    verify_committed_command_in,
};
use crate::{
    Error, Result, Store,
    catalog_commands::TrustedCommandAuthority,
    catalog_metadata_store::{self as metadata, VerifiedCreatedOrigin},
    operation_budget::BudgetCharge,
};
use catabolic_core::{
    catalog_command::CommandEnvelope,
    catalog_metadata::{MAX_METADATA_DOCUMENT_BYTES, nfo_refresh::SubjectState},
};

/// Trusted embedding-host audit, not request data or a serialized permission.
/// The digest identifies an independently complete archived/current audit, not
/// an assertion that a missing row or empty JSON established absence.
#[derive(Debug)]
pub(crate) struct FirstNfoSourceAudit {
    pub catalog_id: String,
    pub incarnation: String,
    pub generation: i64,
    pub host_namespace: String,
    pub subject: String,
    pub outer_digest: String,
    pub creation_digest: String,
    pub complete_absence_audit_digest: String,
}

#[derive(Debug)]
enum Baseline {
    NativeCreated(CommandEnvelope),
    Imported(imported::VerifiedImportedRoot),
}

#[derive(Debug)]
pub(crate) struct VerifiedFirstNfoAbsence {
    current: CurrentNfoState,
    outer: CommandEnvelope,
    baseline: Baseline,
    audit_digest: String,
    proof_digest: String,
    legacy_raw: Option<String>,
    origin: Option<VerifiedCreatedOrigin>,
}
impl VerifiedFirstNfoAbsence {
    pub(super) fn current(&self) -> &CurrentNfoState {
        &self.current
    }
    pub(super) fn creation(&self) -> Result<&CommandEnvelope> {
        match &self.baseline {
            Baseline::NativeCreated(creation) => Ok(creation),
            Baseline::Imported(_) => Err(refused("imported root is not creation-title evidence")),
        }
    }
    pub(super) fn origin_creation_intent(&self) -> Option<&str> {
        self.origin.as_ref()?;
        match &self.baseline {
            Baseline::NativeCreated(creation) => Some(creation.canonical_payload()),
            Baseline::Imported(_) => None,
        }
    }
    pub(super) fn audit_digest(&self) -> &str {
        &self.audit_digest
    }
    pub(super) fn proof_digest(&self) -> &str {
        &self.proof_digest
    }
    pub(super) fn legacy_raw_json(&self) -> Option<&str> {
        self.legacy_raw.as_deref()
    }
    pub(super) fn origin(&self) -> Option<&VerifiedCreatedOrigin> {
        self.origin.as_ref()
    }
    pub(crate) fn validate_origin_in(
        &self,
        store: &Store,
        creation: &CommandEnvelope,
        subject: &str,
    ) -> Result<()> {
        if creation != self.creation()? || subject != self.current.presence().subject {
            return Err(refused("creation-title proof scope differs"));
        }
        self.validate_in(store, &self.outer)
    }
    /// Read-only evidence within the retained writer, never receipt replay or
    /// a reconstructed result. The full stored body is independently required.
    pub(crate) fn read_creation_result_in(
        &self,
        store: &Store,
        creation: &CommandEnvelope,
        subject: &str,
        authority: TrustedCommandAuthority<'_>,
    ) -> Result<serde_json::Value> {
        self.validate_origin_in(store, creation, subject)?;
        verify_committed_command_in(
            store,
            creation,
            authority,
            self.current.presence().fence.generation,
        )?;
        let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
        budget.charge(BudgetCharge {
            values: 2,
            work: 3,
            ..Default::default()
        })?;
        let mut query=store.db.prepare("SELECT result_state,result_json FROM catalog_command_receipts WHERE catalog_id=? AND host_namespace=? AND operation_id=?")?;
        let mut rows = query.query(rusqlite::params![
            creation.key().catalog_id(),
            creation.key().host_namespace(),
            creation.key().operation_id()
        ])?;
        let row = rows
            .next()?
            .ok_or_else(|| refused("retained full creation body absent"))?;
        if row
            .get_ref(0)?
            .as_str()
            .map_err(|_| refused("receipt state not text"))?
            != "full"
        {
            return Err(refused(
                "expired creation result cannot authorize title reconciliation",
            ));
        }
        let raw = row
            .get_ref(1)?
            .as_str()
            .map_err(|_| refused("creation result not text"))?;
        budget.raw_json(raw, catabolic_core::catalog_command::MAX_COMMAND_JSON_DEPTH)?;
        catabolic_core::json::parse(raw, true)
            .map_err(|_| refused("invalid duplicate-safe creation result"))
    }
    pub(super) fn validate_in(&self, store: &Store, outer: &CommandEnvelope) -> Result<()> {
        if outer != &self.outer {
            return Err(refused("exact outer envelope differs"));
        }
        let before = self.current.presence();
        let observed = state::read_in(
            store,
            &before.subject,
            before.fence.generation,
            before.aggregate_revision,
        )?;
        if observed.presence() != before
            || observed.native_source() != self.current.native_source()
            || observed.schema() != self.current.schema()
            || observed.origin_physical() != self.current.origin_physical()
            || observed.origin_proof_digest() != self.current.origin_proof_digest()
            || observed.source_proof_digest() != self.current.source_proof_digest()
            || observed.attribution_audit_digest() != self.current.attribution_audit_digest()
        {
            return Err(refused("audited current pre-state changed"));
        }
        if let Baseline::Imported(root) = &self.baseline {
            root.validate_in(store, outer, &observed)?;
        }
        let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
        require_absent_in(store, &observed, &mut budget)?;
        if let Some(expected) = &self.legacy_raw {
            if raw_metadata(store, &before.subject, &mut budget)? != *expected {
                return Err(refused("uninitialized native baseline changed"));
            }
        }
        Ok(())
    }
}
fn refused(message: impl std::fmt::Display) -> Error {
    Error(format!("catalog_nfo_first_source_refused: {message}"))
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn raw_metadata(store: &Store, subject: &str, budget: &mut ProofBudget) -> Result<String> {
    let mut query = store.db.prepare("SELECT metadata FROM items WHERE id=?")?;
    let mut rows = query.query([subject])?;
    let row = rows.next()?.ok_or_else(|| refused("native item absent"))?;
    let raw = row
        .get_ref(0)?
        .as_str()
        .map_err(|_| refused("native metadata not text"))?;
    if raw.len() > MAX_METADATA_DOCUMENT_BYTES {
        return Err(refused("native baseline byte bound"));
    }
    budget.raw_json(raw, 32)?;
    budget.charge(BudgetCharge {
        bytes: raw.len(),
        values: 1,
        work: raw.len() / 64 + 1,
        ..Default::default()
    })?;
    Ok(raw.to_owned())
}
fn require_absent_in(
    store: &Store,
    current: &CurrentNfoState,
    budget: &mut ProofBudget,
) -> Result<()> {
    let before = current.presence();
    if store.schema_version != 41
        || !store.writable
        || store.db.is_autocommit()
        || !matches!(before.head.subject_state, SubjectState::Active { .. })
        || before.head.source.is_some()
        || before.head.attribution.is_some()
        || before.presence.revision != 0
        || before.presence.incarnation != 0
        || before.presence.public_revision.is_some()
        || current.native_source().is_some()
        || current.origin_physical().is_some()
        || current.origin_proof_digest().is_some()
        || current.source_proof_digest().is_some()
        || current.attribution_audit_digest().is_some()
    {
        return Err(refused(
            "first-source consistency requires active never-present typed state",
        ));
    }
    store.transaction_available()?;
    budget.charge(BudgetCharge {
        values: 10,
        work: 12,
        ..Default::default()
    })?;
    let absent: bool = store.db.query_row("SELECT NOT EXISTS(SELECT 1 FROM catalog_metadata_sources WHERE item_id=?1 AND namespace='motion.nfo') AND NOT EXISTS(SELECT 1 FROM catalog_metadata_history WHERE item_id=?1 AND namespace='motion.nfo') AND NOT EXISTS(SELECT 1 FROM catalog_metadata_tag_history WHERE item_id=?1 AND namespace='motion.nfo') AND NOT EXISTS(SELECT 1 FROM item_tags WHERE item_id=?1 AND source='motion.nfo') AND NOT EXISTS(SELECT 1 FROM catalog_nfo_states WHERE item_id=?1) AND NOT EXISTS(SELECT 1 FROM catalog_nfo_state_revisions WHERE item_id=?1) AND NOT EXISTS(SELECT 1 FROM catalog_nfo_identity_claims WHERE item_id=?1) AND NOT EXISTS(SELECT 1 FROM catalog_nfo_file_attributions WHERE item_id=?1) AND NOT EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id=?1)", [&before.subject], |r|r.get(0))?;
    if !absent {
        return Err(refused(
            "retained NFO namespace/history/assertion/index obligation exists",
        ));
    }
    Ok(())
}

pub(super) fn prepare_in(
    store: &Store,
    outer: &CommandEnvelope,
    current: CurrentNfoState,
    creation: &CommandEnvelope,
    receipt_authority: TrustedCommandAuthority<'_>,
    audit: &FirstNfoSourceAudit,
) -> Result<VerifiedFirstNfoAbsence> {
    let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
    let before = current.presence();
    if audit.catalog_id != before.fence.catalog_id
        || audit.incarnation != before.fence.incarnation
        || audit.generation != before.fence.generation
        || audit.subject != before.subject
        || audit.host_namespace != outer.key().host_namespace()
        || audit.outer_digest != outer.digest().as_str()
        || audit.creation_digest != creation.digest().as_str()
        || !digest(&audit.complete_absence_audit_digest)
        || outer.key().catalog_id() != before.fence.catalog_id
        || outer.admission_incarnation() != before.fence.incarnation
        || outer.expected_generation() != before.fence.generation
        || creation.key().host_namespace() != audit.host_namespace
    {
        return Err(refused(
            "independently complete audit or exact outer scope differs",
        ));
    }
    require_absent_in(store, &current, &mut budget)?;
    let committed =
        verify_committed_command_in(store, creation, receipt_authority, before.fence.generation)?;
    budget.serialized(creation.payload(), 64)?;
    let payload = creation.payload();
    let items = payload
        .get("items")
        .and_then(|v| v.as_array())
        .ok_or_else(|| refused("creation items absent"))?;
    if payload["operation"] != "item_create_batch" {
        return Err(refused("baseline is not create-only"));
    }
    let mut matched = items
        .iter()
        .filter(|item| item["id"].as_str() == Some(before.subject.as_str()));
    let item = matched
        .next()
        .ok_or_else(|| refused("creation subject absent"))?;
    if matched.next().is_some() || !item["identities"].is_null() {
        return Err(refused(
            "creation subject is not unique without assigned identities",
        ));
    }
    let fields = item["metadata"]
        .as_object()
        .ok_or_else(|| refused("creation metadata absent"))?;
    let structure = fields
        .get("motion_catalog")
        .ok_or_else(|| refused("creation structure absent"))?;
    if fields.len() != 2
        || !fields.get("title").is_some_and(|v| v.is_string())
        || structure["contract_version"] != 1
        || structure["item_id"].as_str() != Some(before.subject.as_str())
        || !structure["external_ids"]
            .as_array()
            .is_some_and(Vec::is_empty)
    {
        return Err(refused(
            "creation is outside exact empty-identity Motion work cohort",
        ));
    }
    // This native command contract and the independent complete audit are BOTH
    // required. The marker alone, or a missing current source, grants nothing.
    let legacy_raw = if current.schema().is_none() {
        Some(raw_metadata(store, &before.subject, &mut budget)?)
    } else {
        None
    };
    let proof_digest = budget.serialized(
        &(
            "catabolic.nfo.first-source-absence.v1",
            before,
            outer.key(),
            outer.digest().as_str(),
            creation.key(),
            creation.digest().as_str(),
            committed.committed_generation(),
            &audit.complete_absence_audit_digest,
            &legacy_raw,
        ),
        64,
    )?;
    budget.raw_json(outer.canonical_payload(), 64)?;
    budget.raw_json(creation.canonical_payload(), 64)?;
    budget.serialized(
        &(
            outer.payload(),
            creation.payload(),
            outer.key(),
            creation.key(),
            &audit.complete_absence_audit_digest,
        ),
        64,
    )?;
    let mut verified = VerifiedFirstNfoAbsence {
        current,
        outer: outer.clone(),
        baseline: Baseline::NativeCreated(creation.clone()),
        audit_digest: audit.complete_absence_audit_digest.clone(),
        proof_digest,
        legacy_raw,
        origin: None,
    };
    verified.validate_in(store, outer)?;
    if verified.legacy_raw.is_some() {
        verified.origin = Some(
            metadata::verify_created_origin_nfo(
                store,
                creation,
                &verified.current.presence().subject,
                receipt_authority,
                &verified,
            )
            .map_err(refused)?,
        );
    }
    Ok(verified)
}

/// First publication on an untouched imported managed work. Original-root
/// absence and current absence consistency are separate requirements. An
/// Activate envelope is never usable as created-title reconciliation evidence.
pub(super) fn prepare_imported_in(
    store: &Store,
    outer: &CommandEnvelope,
    current: CurrentNfoState,
    parts: &[imported::ImportedFirstPart<'_>],
    audit: &imported::ImportedFirstRootAudit,
    authority: TrustedCommandAuthority<'_>,
) -> Result<VerifiedFirstNfoAbsence> {
    let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
    require_absent_in(store, &current, &mut budget)?;
    if current.schema().is_none() {
        return Err(refused(
            "imported first source requires an exact managed baseline",
        ));
    }
    let root = imported::verify_in(store, outer, &current, parts, audit, authority)?;
    // Admit retained envelope/proof strings before cloning. Root validation has
    // already admitted and verified the complete historical/current closure.
    budget.serialized(
        &(
            outer.payload(),
            outer.key(),
            root.audit_digest(),
            root.proof_digest(),
        ),
        64,
    )?;
    Ok(VerifiedFirstNfoAbsence {
        current,
        outer: outer.clone(),
        audit_digest: root.audit_digest().to_owned(),
        proof_digest: root.proof_digest().to_owned(),
        baseline: Baseline::Imported(root),
        legacy_raw: None,
        origin: None,
    })
}
