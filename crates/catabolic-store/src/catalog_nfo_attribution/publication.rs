//! Private schema41 publication after a verified existing or audited-first PublicPut.
//! No transaction, receipt, aggregate guard, authorization grant or filesystem
//! authority is created here. The outer dispatcher must roll back on ANY error.
//! Its deferred metadata barrier remains until all source/state/index links pass.
use super::{permit::VerifiedNfoPut, proof_budget::ProofBudget};
use crate::{
    Error, Result, Store, catalog_import_typed::PhysicalMapping, operation_budget::BudgetCharge,
};
use catabolic_core::catalog_metadata::{
    ContributionDocument, MAX_METADATA_DOCUMENT_BYTES, MAX_METADATA_JSON_DEPTH,
    nfo_refresh::{
        AttributionHead, DocumentEffect, DocumentState, Fence, FileOrigin, PinHead, PresenceEffect,
        PresenceHeadEffect, SourcePresence, SubjectState,
    },
};
use rusqlite::{Row, params, types::ValueRef};
use serde::Serialize;

const NAMESPACE: &str = "motion.nfo";
const RECORD: &str = "document";

/// Observed within the still-uncommitted writer, never a receipt or an issued
/// Motion aggregate revision. Only the outer dispatcher can make it durable.
#[derive(Debug)]
pub(super) struct PublishedNfoPut {
    subject: String,
    source_revision: i64,
    attribution_revision: i64,
    presence: SourcePresence,
    schema_fingerprint: String,
    item_revision: i64,
    observed_generation: i64,
    source_proof_digest: String,
}
impl PublishedNfoPut {
    pub(super) fn subject(&self) -> &str {
        &self.subject
    }
    pub(super) fn source_revision(&self) -> i64 {
        self.source_revision
    }
    pub(super) fn attribution_revision(&self) -> i64 {
        self.attribution_revision
    }
    pub(super) fn presence(&self) -> &SourcePresence {
        &self.presence
    }
    pub(super) fn schema_fingerprint(&self) -> &str {
        &self.schema_fingerprint
    }
    pub(super) fn item_revision(&self) -> i64 {
        self.item_revision
    }
    pub(super) fn observed_generation(&self) -> i64 {
        self.observed_generation
    }
    pub(super) fn source_proof_digest(&self) -> &str {
        &self.source_proof_digest
    }
}
fn refused(message: impl std::fmt::Display) -> Error {
    Error(format!("catalog_nfo_publication_refused: {message}"))
}
fn charge(budget: &mut ProofBudget, bytes: usize, values: usize) -> Result<()> {
    budget.charge(BudgetCharge {
        bytes,
        values,
        work: bytes / 64 + values + 1,
        ..Default::default()
    })
}
fn text(row: &Row<'_>, index: usize, maximum: usize, budget: &mut ProofBudget) -> Result<String> {
    let ValueRef::Text(raw) = row.get_ref(index)? else {
        return Err(refused("stored text absent"));
    };
    if raw.len() > maximum {
        return Err(refused("stored text byte bound"));
    }
    charge(budget, raw.len(), 1)?;
    Ok(std::str::from_utf8(raw).map_err(refused)?.to_owned())
}
fn optional_text(
    row: &Row<'_>,
    index: usize,
    maximum: usize,
    budget: &mut ProofBudget,
) -> Result<Option<String>> {
    if matches!(row.get_ref(index)?, ValueRef::Null) {
        charge(budget, 0, 1)?;
        Ok(None)
    } else {
        text(row, index, maximum, budget).map(Some)
    }
}
fn fence(store: &Store, budget: &mut ProofBudget) -> Result<Fence> {
    if store.schema_version != 41 || !store.writable || store.db.is_autocommit() {
        return Err(refused("schema41 retained writer required"));
    }
    store.transaction_available()?;
    charge(budget, 0, 3)?;
    let mut query = store
        .db
        .prepare("SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1")?;
    let mut rows = query.query([])?;
    let row = rows
        .next()?
        .ok_or_else(|| refused("current fence absent"))?;
    let catalog_id = text(row, 0, 256, budget)?;
    let incarnation = text(row, 1, 256, budget)?;
    let generation: i64 = row.get(2)?;
    if catalog_id != store.database_id
        || Some(incarnation.as_str()) != store.catalog_incarnation.as_deref()
        || generation < 0
    {
        return Err(refused("current native identity differs"));
    }
    Ok(Fence {
        catalog_id,
        incarnation,
        generation,
    })
}
fn pending_barrier(
    store: &Store,
    verified: &VerifiedNfoPut,
    budget: &mut ProofBudget,
) -> Result<()> {
    charge(budget, 0, 2)?;
    let command = verified.outer();
    let subject = &verified.original().presence().subject;
    let exact:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_metadata_permits WHERE item_id=? AND incarnation=? AND barrier_id=1) AND NOT EXISTS(SELECT 1 FROM catalog_commit_barrier WHERE id=1) AND NOT EXISTS(SELECT 1 FROM catalog_command_receipts WHERE catalog_id=? AND host_namespace=? AND operation_id=?)",params![subject,command.admission_incarnation(),command.key().catalog_id(),command.key().host_namespace(),command.key().operation_id()],|r|r.get(0))?;
    if !exact {
        return Err(refused(
            "exact pending outer metadata barrier/receipt differs",
        ));
    }
    Ok(())
}
struct Previous {
    origin_json: Option<String>,
    physical_json: Option<String>,
    origin_proof: Option<String>,
    source_proof: String,
    audit: String,
}
fn previous(
    store: &Store,
    verified: &VerifiedNfoPut,
    budget: &mut ProofBudget,
) -> Result<Previous> {
    let original = verified.original();
    let before = original.presence();
    if let Some(first) = verified.first() {
        // The absence token was independently prepared before metadata effects.
        // Current no-row facts only verify that its typed predecessor is intact.
        charge(budget, 0, 5)?;
        let absent: bool = store.db.query_row("SELECT NOT EXISTS(SELECT 1 FROM catalog_nfo_states WHERE item_id=?1) AND NOT EXISTS(SELECT 1 FROM catalog_nfo_state_revisions WHERE item_id=?1) AND NOT EXISTS(SELECT 1 FROM catalog_entity_revisions WHERE entity_kind='catalog_nfo_states' AND entity_key=json_array(?1)) AND NOT EXISTS(SELECT 1 FROM catalog_nfo_identity_claims WHERE item_id=?1) AND NOT EXISTS(SELECT 1 FROM catalog_nfo_file_attributions WHERE item_id=?1)",[&before.subject],|r|r.get(0))?;
        if !absent
            || original.native_source().is_some()
            || before.head.attribution.is_some()
            || original.origin_physical().is_some()
            || original.origin_proof_digest().is_some()
        {
            return Err(refused("first typed predecessor changed"));
        }
        budget.serialized(&(first.proof_digest(), first.audit_digest()), 64)?;
        return Ok(Previous {
            origin_json: None,
            physical_json: None,
            origin_proof: None,
            source_proof: first.proof_digest().into(),
            audit: first.audit_digest().into(),
        });
    }
    let source = original
        .native_source()
        .ok_or_else(|| refused("original source absent"))?;
    let attribution = before
        .head
        .attribution
        .as_ref()
        .ok_or_else(|| refused("original attribution absent"))?;
    charge(budget, 0, 14)?;
    let mut query=store.db.prepare("SELECT h.source_revision,h.public_incarnation,h.public_revision,h.document_identity,h.active_identity,h.origin_json,h.origin_physical_json,h.origin_proof_digest,h.source_proof_digest,h.attribution_audit_digest FROM catalog_nfo_states s JOIN catalog_nfo_state_revisions h ON h.item_id=s.item_id AND h.revision=s.current_revision WHERE s.item_id=? AND s.current_revision=? AND h.source_namespace='motion.nfo' AND h.source_record_id='document'")?;
    let mut rows = query.query(params![before.subject, attribution.revision])?;
    let row = rows
        .next()?
        .ok_or_else(|| refused("original typed owner head changed"))?;
    let source_revision: i64 = row.get(0)?;
    let incarnation: i64 = row.get(1)?;
    let public_revision: Option<i64> = row.get(2)?;
    let document_identity = optional_text(row, 3, 1024, budget)?;
    let active_identity = optional_text(row, 4, 1024, budget)?;
    let origin_json = optional_text(row, 5, MAX_METADATA_DOCUMENT_BYTES, budget)?;
    let physical_json = optional_text(row, 6, MAX_METADATA_DOCUMENT_BYTES, budget)?;
    let origin_proof = optional_text(row, 7, 64, budget)?;
    let source_proof = text(row, 8, 64, budget)?;
    let audit = text(row, 9, 64, budget)?;
    let expected_identity = match &before
        .head
        .source
        .as_ref()
        .ok_or_else(|| refused("original document absent"))?
        .document
    {
        DocumentState::Present {
            external_identity, ..
        } => external_identity.as_deref(),
        DocumentState::Withdrawn => None,
    };
    if source_revision != source.revision()
        || incarnation != before.presence.incarnation
        || public_revision != before.presence.public_revision
        || document_identity.as_deref() != expected_identity
        || active_identity != attribution.active_identity
        || origin_proof.as_deref() != original.origin_proof_digest()
        || Some(source_proof.as_str()) != original.source_proof_digest()
        || Some(audit.as_str()) != original.attribution_audit_digest()
    {
        return Err(refused("retained original presence/proof/audit differs"));
    }
    let origin = origin_json
        .as_ref()
        .map(|raw| {
            budget.raw_json(raw, MAX_METADATA_JSON_DEPTH)?;
            serde_json::from_str::<FileOrigin>(raw).map_err(Error::from)
        })
        .transpose()?;
    let physical = physical_json
        .as_ref()
        .map(|raw| {
            budget.raw_json(raw, MAX_METADATA_JSON_DEPTH)?;
            serde_json::from_str::<PhysicalMapping>(raw).map_err(Error::from)
        })
        .transpose()?;
    if origin.as_ref() != attribution.origin.as_ref()
        || physical.as_ref() != original.origin_physical()
    {
        return Err(refused(
            "retained exact origin mapping contradicts verified proof",
        ));
    }
    let terminal:bool=store.db.query_row("SELECT NOT EXISTS(SELECT 1 FROM catalog_nfo_state_revisions WHERE item_id=?1 AND revision>?2) AND EXISTS(SELECT 1 FROM catalog_entity_revisions WHERE entity_kind='catalog_nfo_states' AND entity_key=json_array(?1) AND revision=?2 AND deleted=0)",params![before.subject,attribution.revision],|r|r.get(0))?;
    if !terminal {
        return Err(refused(
            "original attribution is not the exact terminal owner revision",
        ));
    }
    Ok(Previous {
        origin_json,
        physical_json,
        origin_proof,
        source_proof,
        audit,
    })
}
fn source_matches(
    store: &Store,
    verified: &VerifiedNfoPut,
    fields: &str,
    evidence: &str,
    budget: &mut ProofBudget,
) -> Result<()> {
    charge(budget, 0, 5)?;
    let subject = &verified.original().presence().subject;
    let source = verified.source();
    let command = verified.outer();
    let exact:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_metadata_sources s JOIN catalog_metadata_schema_heads sh ON sh.item_id=s.item_id JOIN catalog_metadata_history h ON h.item_id=s.item_id AND h.namespace=s.namespace AND h.record_id=s.record_id AND h.revision=s.revision WHERE s.item_id=?1 AND s.namespace='motion.nfo' AND s.record_id='document' AND s.source_kind='provider' AND s.revision=?2 AND s.schema_fingerprint=?3 AND sh.schema_fingerprint=?3 AND s.fields_json=?4 AND s.evidence_json=?5 AND h.source_kind=s.source_kind AND h.schema_fingerprint=s.schema_fingerprint AND h.fields_json=s.fields_json AND h.evidence_json=s.evidence_json AND h.command_catalog_id=?6 AND h.command_host_namespace=?7 AND h.command_operation_id=?8) AND ?2=(SELECT max(revision) FROM catalog_metadata_history WHERE item_id=?1 AND namespace='motion.nfo' AND record_id='document') AND EXISTS(SELECT 1 FROM catalog_entity_revisions WHERE entity_kind='items' AND entity_key=json_array(?1) AND revision=?9 AND deleted=0) AND NOT EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id=?1)",params![subject,source.revision(),verified.schema().fingerprint(),fields,evidence,command.key().catalog_id(),command.key().host_namespace(),command.key().operation_id(),verified.item_revision()],|r|r.get(0))?;
    if !exact {
        return Err(refused(
            "actual normalized source/schema/item facts changed",
        ));
    }
    Ok(())
}
fn pins_unchanged(
    store: &Store,
    verified: &VerifiedNfoPut,
    budget: &mut ProofBudget,
) -> Result<()> {
    charge(budget, 0, 2)?;
    let before = verified.original().presence();
    let exact=match &before.head.pin {
        PinHead::NeverDecided=>store.db.query_row("SELECT NOT EXISTS(SELECT 1 FROM catalog_identity_pins WHERE item_id=?1 AND namespace='nfo') AND NOT EXISTS(SELECT 1 FROM catalog_identity_pin_revisions WHERE item_id=?1 AND namespace='nfo')",[&before.subject],|r|r.get::<_,bool>(0))?,
        PinHead::Decision{revision,value}=>store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_identity_pins p JOIN catalog_identity_pin_revisions h ON h.item_id=p.item_id AND h.namespace=p.namespace AND h.revision=p.current_revision WHERE p.item_id=?1 AND p.namespace='nfo' AND p.current_revision=?2 AND p.value IS ?3 AND h.value IS ?3) AND ?2=(SELECT max(revision) FROM catalog_identity_pin_revisions WHERE item_id=?1 AND namespace='nfo')",params![before.subject,revision,value],|r|r.get::<_,bool>(0))?,
    };
    if !exact {
        return Err(refused("independent manual pin changed"));
    }
    Ok(())
}
fn indexes_match(
    store: &Store,
    subject: &str,
    attribution: &AttributionHead,
    budget: &mut ProofBudget,
) -> Result<()> {
    charge(budget, 0, 4)?;
    let claim=match &attribution.active_identity {
        Some(value)=>store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_nfo_identity_claims WHERE item_id=? AND value=? AND state_revision=?) AND NOT EXISTS(SELECT 1 FROM catalog_identity_pins WHERE namespace='nfo' AND value=? AND item_id<>?)",params![subject,value,attribution.revision,value,subject],|r|r.get::<_,bool>(0))?,
        None=>store.db.query_row("SELECT NOT EXISTS(SELECT 1 FROM catalog_nfo_identity_claims WHERE item_id=?)",[subject],|r|r.get::<_,bool>(0))?,
    };
    let file=match &attribution.origin {
        Some(origin)=>store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_nfo_file_attributions WHERE item_id=? AND profile=? AND file_id=? AND state_revision=?)",params![subject,origin.file.profile,origin.file.file_id,attribution.revision],|r|r.get::<_,bool>(0))?,
        None=>store.db.query_row("SELECT NOT EXISTS(SELECT 1 FROM catalog_nfo_file_attributions WHERE item_id=?)",[subject],|r|r.get::<_,bool>(0))?,
    };
    if !claim || !file {
        return Err(refused("owned claim/file index is inconsistent"));
    }
    Ok(())
}
struct Planned<'a> {
    attribution: &'a AttributionHead,
    presence: &'a SourcePresence,
    identity: Option<&'a str>,
}
fn planned<'a>(verified: &'a VerifiedNfoPut, budget: &mut ProofBudget) -> Result<Planned<'a>> {
    let original = verified.original().presence();
    let prior = original.head.attribution.as_ref();
    let plan = verified.plan();
    budget.serialized(original, 64)?;
    if plan.expected != original.stamp()
        || plan.final_snapshot.fence != original.fence
        || plan.final_snapshot.subject != original.subject
        || !matches!(original.head.subject_state, SubjectState::Active { .. })
        || plan.final_snapshot.head.pin != original.head.pin
    {
        return Err(refused(
            "verified pure plan no longer describes original pre-state",
        ));
    }
    let PresenceEffect::Changed {
        document: DocumentEffect::Replace { head },
        presence: PresenceHeadEffect::Replace { head: presence },
        attribution,
    } = &plan.effect
    else {
        return Err(refused(
            "publication is only a verified PublicPut replacement",
        ));
    };
    let DocumentState::Present {
        contribution,
        external_identity,
    } = &head.document
    else {
        return Err(refused("PublicPut must publish a present document"));
    };
    let prior_source = original.head.source.as_ref();
    let prior_identity = match prior_source.map(|source| &source.document) {
        Some(DocumentState::Present {
            external_identity, ..
        }) => external_identity.as_deref(),
        Some(DocumentState::Withdrawn) | None => None,
    };
    if (prior.is_none() || prior_source.is_none()) && verified.first().is_none() {
        return Err(refused(
            "first publication lacks positive absence capability",
        ));
    }
    if prior.map_or(Some(1), |p| p.revision.checked_add(1)) != Some(attribution.revision)
        || prior_source.map_or(Some(1), |p| p.revision.checked_add(1)) != Some(head.revision)
        || head.revision != verified.source().revision()
        || contribution.fields != *verified.source().fields()
        || attribution.document_revision != head.revision
        || attribution.origin.as_ref() != prior.and_then(|p| p.origin.as_ref())
        || attribution.active_identity.as_deref() != prior_identity
        || external_identity.as_deref() != prior_identity
        || plan.final_snapshot.head.source.as_ref() != Some(head)
        || plan.final_snapshot.head.attribution.as_ref() != Some(attribution)
        || plan.final_snapshot.presence != *presence
        || presence.revision != head.revision
        || presence.public_revision.is_none()
    {
        return Err(refused(
            "verified PublicPut source/presence/identity effects disagree",
        ));
    }
    Ok(Planned {
        attribution,
        presence,
        identity: external_identity.as_deref(),
    })
}

pub(super) fn publish_put_in(store: &Store, verified: VerifiedNfoPut) -> Result<PublishedNfoPut> {
    let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
    let start = fence(store, &mut budget)?;
    let original = verified.original();
    let before = original.presence();
    let outer = verified.outer();
    if start.catalog_id != before.fence.catalog_id
        || start.incarnation != before.fence.incarnation
        || start.generation != verified.post_metadata_generation()
        || outer.key().catalog_id() != start.catalog_id
        || outer.admission_incarnation() != start.incarnation
        || outer.expected_generation() != before.fence.generation
    {
        return Err(refused(
            "publication token catalog/incarnation/post-metadata generation changed",
        ));
    }
    pending_barrier(store, &verified, &mut budget)?;
    let previous = previous(store, &verified, &mut budget)?;
    let planned = planned(&verified, &mut budget)?;
    let prior = before.head.attribution.as_ref();
    pins_unchanged(store, &verified, &mut budget)?;
    if let Some(prior) = prior {
        indexes_match(store, &before.subject, prior, &mut budget)?;
    }
    budget.serialized(verified.source().fields(), MAX_METADATA_JSON_DEPTH)?;
    budget.canonical_json(verified.source().evidence())?;
    let fields = serde_json::to_string(verified.source().fields())?;
    let evidence = catabolic_core::encode(verified.source().evidence());
    source_matches(store, &verified, &fields, &evidence, &mut budget)?;
    #[derive(Serialize)]
    struct Proof<'a> {
        domain: &'static str,
        prior_source_proof: &'a str,
        attribution_audit: &'a str,
        original: &'a catabolic_core::catalog_metadata::nfo_refresh::PresenceSnapshot,
        outer_key: &'a catabolic_core::catalog_command::CommandKey,
        outer_digest: &'a str,
        outer_incarnation: &'a str,
        outer_expected_generation: i64,
        source: &'a ContributionDocument,
        schema_contract: u32,
        schema_fingerprint: &'a str,
        schema_fields:
            &'a std::collections::BTreeMap<String, catabolic_core::catalog_metadata::FieldPolicy>,
        system_fields: &'a std::collections::BTreeSet<String>,
        plan: &'a catabolic_core::catalog_metadata::nfo_refresh::PresencePlan,
        origin_physical: Option<&'a PhysicalMapping>,
        origin_proof_digest: Option<&'a str>,
        post_metadata_generation: i64,
        item_revision: i64,
    }
    let source_proof_digest = budget.serialized(
        &Proof {
            domain: "catabolic.nfo.public_put.state.v1",
            prior_source_proof: &previous.source_proof,
            attribution_audit: &previous.audit,
            original: before,
            outer_key: outer.key(),
            outer_digest: outer.digest().as_str(),
            outer_incarnation: outer.admission_incarnation(),
            outer_expected_generation: outer.expected_generation(),
            source: verified.source(),
            schema_contract: verified.schema().contract_version(),
            schema_fingerprint: verified.schema().fingerprint(),
            schema_fields: verified.schema().fields(),
            system_fields: verified.schema().system_fields(),
            plan: verified.plan(),
            origin_physical: original.origin_physical(),
            origin_proof_digest: original.origin_proof_digest(),
            post_metadata_generation: start.generation,
            item_revision: verified.item_revision(),
        },
        64,
    )?;
    // Reserve retained DB values and all returned copies before any effects.
    budget.serialized(
        &(
            &before.subject,
            planned.attribution,
            planned.presence,
            planned.identity,
            &previous.origin_json,
            &previous.physical_json,
            &previous.origin_proof,
            &source_proof_digest,
            &previous.audit,
            outer.key(),
        ),
        64,
    )?;
    budget.serialized(
        &(
            &before.subject,
            planned.presence,
            verified.schema().fingerprint(),
            &source_proof_digest,
        ),
        64,
    )?;
    let mut published = PublishedNfoPut {
        subject: before.subject.clone(),
        source_revision: verified.source().revision(),
        attribution_revision: planned.attribution.revision,
        presence: planned.presence.clone(),
        schema_fingerprint: verified.schema().fingerprint().into(),
        item_revision: verified.item_revision(),
        observed_generation: 0,
        source_proof_digest,
    };
    // INSERT history before its head: the schema41 sequence trigger checks the
    // previous current_revision, and deferred FKs keep the compound atomic.
    store.db.execute("INSERT INTO catalog_nfo_state_revisions(item_id,revision,source_revision,source_namespace,source_record_id,public_incarnation,public_revision,document_identity,active_identity,origin_json,origin_physical_json,origin_proof_digest,source_proof_digest,attribution_audit_digest,command_catalog_id,command_host_namespace,command_operation_id) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",params![before.subject,planned.attribution.revision,verified.source().revision(),NAMESPACE,RECORD,planned.presence.incarnation,planned.presence.public_revision,planned.identity,planned.attribution.active_identity,previous.origin_json,previous.physical_json,previous.origin_proof,published.source_proof_digest,previous.audit,outer.key().catalog_id(),outer.key().host_namespace(),outer.key().operation_id()])?;
    if let Some(prior) = prior {
        if store.db.execute(
        "UPDATE catalog_nfo_states SET current_revision=? WHERE item_id=? AND current_revision=?",
        params![planned.attribution.revision, before.subject, prior.revision],
    )? != 1
    {
        return Err(refused("attribution compare-and-swap failed"));
    }
        let removed = store.db.execute(
            "DELETE FROM catalog_nfo_identity_claims WHERE item_id=? AND state_revision=?",
            params![before.subject, prior.revision],
        )?;
        if removed != usize::from(prior.active_identity.is_some()) {
            return Err(refused("exact prior automatic claim removal differs"));
        }
        let removed = store.db.execute(
            "DELETE FROM catalog_nfo_file_attributions WHERE item_id=? AND state_revision=?",
            params![before.subject, prior.revision],
        )?;
        if removed != usize::from(prior.origin.is_some()) {
            return Err(refused("exact prior file attribution removal differs"));
        }
    } else {
        if verified.first().is_none() || planned.attribution.revision != 1 {
            return Err(refused("first head insertion lacks positive predecessor"));
        }
        store.db.execute(
            "INSERT INTO catalog_nfo_states(item_id,current_revision) VALUES (?,1)",
            [&before.subject],
        )?;
    }
    if let Some(identity) = &planned.attribution.active_identity {
        store.db.execute(
            "INSERT INTO catalog_nfo_identity_claims(value,item_id,state_revision) VALUES(?,?,?)",
            params![identity, before.subject, planned.attribution.revision],
        )?;
    }
    if let Some(origin) = &planned.attribution.origin {
        store.db.execute("INSERT INTO catalog_nfo_file_attributions(item_id,profile,file_id,state_revision) VALUES(?,?,?,?)",params![before.subject,origin.file.profile,origin.file.file_id,planned.attribution.revision])?;
    }
    // All checks remain before barrier release. In particular a swallowed
    // verification/publication error cannot commit a metadata-only half-effect.
    source_matches(store, &verified, &fields, &evidence, &mut budget)?;
    pins_unchanged(store, &verified, &mut budget)?;
    indexes_match(store, &before.subject, planned.attribution, &mut budget)?;
    charge(&mut budget, 0, 2)?;
    let exact:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_nfo_states s JOIN catalog_nfo_state_revisions h ON h.item_id=s.item_id AND h.revision=s.current_revision WHERE s.item_id=?1 AND s.current_revision=?2 AND h.source_revision=?3 AND h.source_namespace='motion.nfo' AND h.source_record_id='document' AND h.public_incarnation=?4 AND h.public_revision IS ?5 AND h.document_identity IS ?6 AND h.active_identity IS ?7 AND h.origin_json IS ?8 AND h.origin_physical_json IS ?9 AND h.origin_proof_digest IS ?10 AND h.source_proof_digest=?11 AND h.attribution_audit_digest=?12 AND h.command_catalog_id=?13 AND h.command_host_namespace=?14 AND h.command_operation_id=?15) AND EXISTS(SELECT 1 FROM catalog_entity_revisions WHERE entity_kind='catalog_nfo_states' AND entity_key=json_array(?1) AND revision=?2 AND deleted=0) AND NOT EXISTS(SELECT 1 FROM catalog_nfo_state_revisions WHERE item_id=?1 AND revision>?2)",params![before.subject,planned.attribution.revision,verified.source().revision(),planned.presence.incarnation,planned.presence.public_revision,planned.identity,planned.attribution.active_identity,previous.origin_json,previous.physical_json,previous.origin_proof,published.source_proof_digest,previous.audit,outer.key().catalog_id(),outer.key().host_namespace(),outer.key().operation_id()],|r|r.get(0))?;
    if !exact {
        return Err(refused("final exact state/source/proof links differ"));
    }
    pending_barrier(store, &verified, &mut budget)?;
    let end = fence(store, &mut budget)?;
    if end.catalog_id != start.catalog_id
        || end.incarnation != start.incarnation
        || end.generation <= start.generation
    {
        return Err(refused(
            "publication fence did not advance in the same catalog",
        ));
    }
    published.observed_generation = end.generation;
    super::source_effect::finish_publication_in(
        store,
        outer,
        &before.subject,
        verified.source().revision(),
    )?;
    // This is deliberately the last fallible mutation. No other subject's
    // barrier is removed, and no cleanup/commit witness is manufactured.
    if store.db.execute(
        "DELETE FROM catalog_metadata_permits WHERE item_id=? AND incarnation=? AND barrier_id=1",
        params![before.subject, start.incarnation],
    )? != 1
    {
        return Err(refused("exact metadata barrier release failed"));
    }
    Ok(published)
}
