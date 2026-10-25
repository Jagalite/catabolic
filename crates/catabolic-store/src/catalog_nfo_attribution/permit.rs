//! Private, unregistered consistency corridor for an existing or positively
//! audited first NFO source PUT.
//! This is not curation, source, schema, tag, filesystem or receipt authority.
//! The outer dispatcher owns the sole writer/receipt and rolls back on ANY error,
//! including a verification failure after the metadata owner has written.
use super::{
    first_source::VerifiedFirstNfoAbsence, proof_budget::ProofBudget, state::CurrentNfoState,
};
use crate::{
    Error, Result, Store,
    catalog_metadata_store::{
        ContributionReplace, MetadataAccess, MetadataAuthority, MetadataInitializeV2,
        PolicyNormalization, PolicyValueType, VerifiedCreatedOrigin, read_registered_schema,
    },
    operation_budget::BudgetCharge,
};
use catabolic_core::{
    catalog_command::{COMMAND_CONTRACT_VERSION, CommandEnvelope},
    catalog_metadata::{
        ContributionDocument, ContributionKey, ContributionReplacement, FieldPolicy, FieldSchema,
        MAX_METADATA_DOCUMENT_BYTES, MAX_METADATA_EVIDENCE_BYTES, MAX_METADATA_JSON_DEPTH,
        MAX_METADATA_SNAPSHOT_BYTES, Normalization, SourceKind, ValueType,
        nfo_refresh::{
            Contribution, DocumentEffect, DocumentState, Fence, PRESENCE_CONTRACT_VERSION, PinHead,
            PresenceChange, PresenceEffect, PresencePlan, PresenceRequest, SubjectState,
            plan_presence,
        },
        replacement_fields, validate_schema_extension,
    },
};
use rusqlite::{OptionalExtension, Row, params, types::ValueRef};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const NAMESPACE: &str = "motion.nfo";
const RECORD: &str = "document";
const MAX_RAW_REQUEST_BYTES: usize = 256 * 1024;

#[derive(Debug)]
enum Predecessor {
    Existing(CurrentNfoState),
    First(VerifiedFirstNfoAbsence),
}
impl Predecessor {
    fn current(&self) -> &CurrentNfoState {
        match self {
            Self::Existing(current) => current,
            Self::First(first) => first.current(),
        }
    }
    fn first(&self) -> Option<&VerifiedFirstNfoAbsence> {
        match self {
            Self::First(first) => Some(first),
            Self::Existing(_) => None,
        }
    }
}

/// Only this module can mint a permit. In particular, deserialized request flags
/// or an operation name cannot bypass the generic owner's NFO write exclusion.
#[derive(Debug)]
pub(crate) struct NfoWritePermit {
    predecessor: Predecessor,
    outer: CommandEnvelope,
    request: ContributionReplace,
    raw_request: String,
    expected_public_revision: i64,
    tags: BTreeSet<String>,
    suppression_digest: String,
    initial_system_fields: BTreeSet<String>,
}

/// Verified intermediate state, not a committed result or an issued aggregate
/// guard. The publication owner consumes this once under the same writer, checks
/// post_metadata_generation again, and retains all original audit/origin proof.
#[derive(Debug)]
pub(super) struct VerifiedNfoPut {
    predecessor: Predecessor,
    outer: CommandEnvelope,
    plan: PresencePlan,
    source: ContributionDocument,
    schema: FieldSchema,
    post_metadata_generation: i64,
    item_revision: i64,
}
impl VerifiedNfoPut {
    pub(super) fn original(&self) -> &CurrentNfoState {
        self.predecessor.current()
    }
    pub(super) fn first(&self) -> Option<&VerifiedFirstNfoAbsence> {
        self.predecessor.first()
    }
    pub(super) fn outer(&self) -> &CommandEnvelope {
        &self.outer
    }
    pub(super) fn plan(&self) -> &PresencePlan {
        &self.plan
    }
    pub(super) fn source(&self) -> &ContributionDocument {
        &self.source
    }
    pub(super) fn schema(&self) -> &FieldSchema {
        &self.schema
    }
    pub(super) fn post_metadata_generation(&self) -> i64 {
        self.post_metadata_generation
    }
    pub(super) fn item_revision(&self) -> i64 {
        self.item_revision
    }
}
fn refused(message: impl std::fmt::Display) -> Error {
    Error(format!("catalog_nfo_write_permit_refused: {message}"))
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
    let ValueRef::Text(bytes) = row.get_ref(index)? else {
        return Err(refused("stored text missing"));
    };
    if bytes.len() > maximum {
        return Err(refused("stored text byte bound"));
    }
    charge(budget, bytes.len(), 1)?;
    Ok(std::str::from_utf8(bytes).map_err(refused)?.to_owned())
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
        return Err(refused("current catalog/incarnation differs"));
    }
    Ok(Fence {
        catalog_id,
        incarnation,
        generation,
    })
}
fn absent_receipt(store: &Store, outer: &CommandEnvelope, budget: &mut ProofBudget) -> Result<()> {
    charge(budget, 0, 1)?;
    let exists:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_command_receipts WHERE catalog_id=? AND host_namespace=? AND operation_id=?)",params![outer.key().catalog_id(),outer.key().host_namespace(),outer.key().operation_id()],|r|r.get(0))?;
    if exists {
        return Err(refused(
            "outer receipt must be resolved before mutation admission",
        ));
    }
    Ok(())
}
/// Bound the raw-history encoding before every append, including escaped raw
/// JSON strings. The preflight charges the represented bytes/nodes and work.
fn request_json(request: &ContributionReplace, budget: &mut ProofBudget) -> Result<String> {
    budget.serialized(request, 64)?;
    struct Bounded(Vec<u8>);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0.len().saturating_add(bytes.len()) > MAX_RAW_REQUEST_BYTES {
                return Err(std::io::Error::other("raw replacement history bound"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Bounded(Vec::new());
    serde_json::to_writer(&mut output, request)?;
    String::from_utf8(output.0).map_err(refused)
}
fn matches_outer(
    outer: &CommandEnvelope,
    request: &ContributionReplace,
    expected_public_revision: i64,
    budget: &mut ProofBudget,
) -> Result<()> {
    let payload = outer
        .payload()
        .as_object()
        .ok_or_else(|| refused("outer payload is not an object"))?;
    if outer.contract_version() != COMMAND_CONTRACT_VERSION
        || payload.len() != 4
        || payload.get("operation").and_then(|v| v.as_str()) != Some("nfo_public_put")
        || payload.get("contract_version").and_then(|v| v.as_u64()) != Some(1)
        || payload
            .get("expected_public_revision")
            .and_then(|v| v.as_i64())
            != Some(expected_public_revision)
    {
        return Err(refused("closed NFO PublicPut envelope differs"));
    }
    budget.serialized(request, 64)?;
    let expected = serde_json::to_value(request)?;
    if payload.get("request") != Some(&expected) {
        return Err(refused("exact outer replacement payload differs"));
    }
    Ok(())
}
fn same_state(a: &CurrentNfoState, b: &CurrentNfoState) -> bool {
    a.presence() == b.presence()
        && a.native_source() == b.native_source()
        && a.schema() == b.schema()
        && a.origin_physical() == b.origin_physical()
        && a.origin_proof_digest() == b.origin_proof_digest()
        && a.source_proof_digest() == b.source_proof_digest()
        && a.attribution_audit_digest() == b.attribution_audit_digest()
}
/// Hash exact item-wide decisions/ownership independently of the source tags.
/// A metadata PUT must not clear, adopt or otherwise mutate a foreign decision.
fn suppressions(store: &Store, subject: &str, budget: &mut ProofBudget) -> Result<String> {
    let mut hash = Sha256::new();
    let mut query=store.db.prepare("SELECT s.tag_id,s.revision,h.revision,h.suppressed,o.namespace,o.revision FROM catalog_item_tag_suppressions s LEFT JOIN catalog_tag_suppression_revisions h ON h.subject_type='item' AND h.subject_id=s.item_id AND h.tag_id=s.tag_id AND h.revision=s.revision LEFT JOIN catalog_metadata_tag_suppressions o ON o.item_id=s.item_id AND o.tag_id=s.tag_id WHERE s.item_id=? ORDER BY s.tag_id")?;
    let mut rows = query.query([subject])?;
    while let Some(row) = rows.next()? {
        charge(budget, 0, 6)?;
        let tag = text(row, 0, 256, budget)?;
        let revision: i64 = row.get(1)?;
        let historical: Option<i64> = row.get(2)?;
        let suppressed: Option<i64> = row.get(3)?;
        let owned = if matches!(row.get_ref(4)?, ValueRef::Null) {
            None
        } else {
            Some(text(row, 4, 256, budget)?)
        };
        let owned_revision: Option<i64> = row.get(5)?;
        if revision < 1
            || historical != Some(revision)
            || !matches!(suppressed, Some(0 | 1))
            || owned.is_some() != owned_revision.is_some()
            || owned.as_deref().is_some_and(|s| s != "motion.local")
            || owned_revision.is_some_and(|r| r < 1 || r > revision)
        {
            return Err(refused("suppression ownership/head is inconsistent"));
        }
        let digest = budget.serialized(&(tag, revision, suppressed, owned, owned_revision), 64)?;
        hash.update(digest.as_bytes());
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(super) fn prepare_put_in(
    store: &Store,
    current: CurrentNfoState,
    outer: &CommandEnvelope,
    request: &ContributionReplace,
    expected_public_revision: i64,
) -> Result<NfoWritePermit> {
    prepare_in(
        store,
        Predecessor::Existing(current),
        outer,
        request,
        expected_public_revision,
        BTreeSet::new(),
    )
}

pub(super) fn prepare_first_put_in(
    store: &Store,
    first: VerifiedFirstNfoAbsence,
    outer: &CommandEnvelope,
    request: &ContributionReplace,
    expected_public_revision: i64,
    initialization: &MetadataAuthority,
) -> Result<NfoWritePermit> {
    first.validate_in(store, outer)?;
    let MetadataAccess::InitializeV2 { system_fields } = &initialization.access else {
        return Err(refused(
            "first source requires explicit initialization classification",
        ));
    };
    if initialization.subject != request.subject
        || initialization.host_namespace != outer.key().host_namespace()
    {
        return Err(refused("first initialization grant scope differs"));
    }
    let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
    budget.serialized(system_fields, MAX_METADATA_JSON_DEPTH)?;
    prepare_in(
        store,
        Predecessor::First(first),
        outer,
        request,
        expected_public_revision,
        system_fields.clone(),
    )
}

fn prepare_in(
    store: &Store,
    predecessor: Predecessor,
    outer: &CommandEnvelope,
    request: &ContributionReplace,
    expected_public_revision: i64,
    initial_system_fields: BTreeSet<String>,
) -> Result<NfoWritePermit> {
    let current = predecessor.current();
    let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
    let current_fence = fence(store, &mut budget)?;
    let before = current.presence();
    if current_fence != before.fence
        || outer.key().catalog_id() != before.fence.catalog_id
        || outer.admission_incarnation() != before.fence.incarnation
        || outer.expected_generation() != before.fence.generation
    {
        return Err(refused("outer command differs from current typed fence"));
    }
    let schema = current.schema();
    match &predecessor {
        Predecessor::Existing(_) => {
            let source = current
                .native_source()
                .ok_or_else(|| refused("existing source absent"))?;
            if source.kind() != SourceKind::Provider
                || request.expected_revision != source.revision()
                || request.expected_revision < 1
                || schema.is_none()
                || before.head.attribution.is_none()
                || request.origin_creation_intent.is_some()
            {
                return Err(refused("existing source preconditions differ"));
            }
        }
        Predecessor::First(first) => {
            if current.native_source().is_some()
                || before.head.attribution.is_some()
                || request.expected_revision != 0
                || expected_public_revision != 0
                || before.presence.revision != 0
                || before.presence.incarnation != 0
                || before.presence.public_revision.is_some()
                || request.origin_creation_intent.as_deref() != first.origin_creation_intent()
            {
                return Err(refused("audited first-source preconditions differ"));
            }
        }
    }
    if !matches!(before.head.subject_state, SubjectState::Active { .. })
        || request.contract_version != 2
        || request.subject != before.subject
        || request.namespace != NAMESPACE
        || request.record_id != RECORD
        || request.expected_schema_fingerprint.as_deref() != schema.map(FieldSchema::fingerprint)
        || !request.excluded_tags.is_empty()
        || expected_public_revision < 0
        || before.presence.public_revision.unwrap_or(0) != expected_public_revision
    {
        return Err(refused("NFO public CAS/schema/active preconditions differ"));
    }
    if request.fields_json.len() > MAX_METADATA_DOCUMENT_BYTES
        || request.evidence_json.len() > MAX_METADATA_EVIDENCE_BYTES
        || request.tags.len() > 100
    {
        return Err(refused("replacement input bound"));
    }
    budget.raw_json(&request.fields_json, MAX_METADATA_JSON_DEPTH)?;
    budget.raw_json(&request.evidence_json, MAX_METADATA_JSON_DEPTH)?;
    let fields = replacement_fields(&request.fields_json).map_err(refused)?;
    if fields.keys().any(|f| {
        f.starts_with("motion_")
            || schema.is_some_and(|s| s.system_fields().contains(f))
            || initial_system_fields.contains(f)
    }) {
        return Err(refused("NFO replacement cannot write protected fields"));
    }
    let mut tags = BTreeSet::new();
    for tag in &request.tags {
        if tag.is_empty() || tag.len() > 100 {
            return Err(refused("tag byte bound"));
        }
        // The existing owner validates canonical spelling before writing. Keep
        // these exact bounded names; do not normalize away the admitted bytes.
        charge(&mut budget, tag.len(), 1)?;
        if !tags.insert(tag.clone()) {
            return Err(refused("duplicate tag"));
        }
    }
    matches_outer(outer, request, expected_public_revision, &mut budget)?;
    absent_receipt(store, outer, &mut budget)?;
    let raw_request = request_json(request, &mut budget)?;
    let suppression_digest = suppressions(store, &before.subject, &mut budget)?;
    // Reserve every retained clone before allocation. CurrentNfoState itself is
    // moved, preserving its independent audit/provenance without duplication.
    budget.raw_json(outer.canonical_payload(), 64)?;
    budget.serialized(outer.payload(), 64)?;
    budget.serialized(
        &(
            outer.key(),
            outer.admission_incarnation(),
            outer.digest().as_str(),
        ),
        64,
    )?;
    budget.serialized(request, 64)?;
    Ok(NfoWritePermit {
        predecessor,
        outer: outer.clone(),
        request: request.clone(),
        raw_request,
        expected_public_revision,
        tags,
        suppression_digest,
        initial_system_fields,
    })
}

impl NfoWritePermit {
    pub(crate) fn begin_source_effect_in(
        &self,
        store: &Store,
        request: &ContributionReplace,
        outer: &CommandEnvelope,
    ) -> Result<super::source_effect::NfoSourceEffect> {
        if self.is_first() {
            self.begin_first_metadata_in(store, request, outer)?;
        } else {
            self.authorize_metadata_in(store, request, outer)?;
            store.db.execute(
                "INSERT INTO catalog_metadata_permits(item_id,incarnation) VALUES (?,?)",
                params![self.request.subject, outer.admission_incarnation()],
            )?;
        }
        let revision = request
            .expected_revision
            .checked_add(1)
            .ok_or_else(|| refused("source revision exhausted"))?;
        super::source_effect::begin_publication_in(
            store,
            outer,
            &request.subject,
            revision,
            &self.raw_request,
        )
    }
    /// Called by the private replacement wrapper before ANY metadata effects.
    /// Other source/init/schema/tag grants are still independently mandatory.
    pub(crate) fn authorize_metadata_in(
        &self,
        store: &Store,
        request: &ContributionReplace,
        outer: &CommandEnvelope,
    ) -> Result<()> {
        let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
        if outer != &self.outer {
            return Err(refused("full outer command identity/digest differs"));
        }
        matches_outer(outer, request, self.expected_public_revision, &mut budget)?;
        if request_json(request, &mut budget)? != self.raw_request {
            return Err(refused("exact inner raw replacement differs"));
        }
        if let Some(first) = self.predecessor.first() {
            first.validate_in(store, outer)?;
        }
        let before = self.predecessor.current().presence();
        let current = super::state::read_in(
            store,
            &before.subject,
            before.fence.generation,
            before.aggregate_revision,
        )?;
        if !same_state(&current, self.predecessor.current()) {
            return Err(refused("typed pre-state changed before metadata effect"));
        }
        if suppressions(store, &before.subject, &mut budget)? != self.suppression_digest {
            return Err(refused("item suppression dependencies changed"));
        }
        absent_receipt(store, outer, &mut budget)
    }

    pub(crate) fn first_origin(&self) -> Option<&VerifiedCreatedOrigin> {
        self.predecessor
            .first()
            .and_then(VerifiedFirstNfoAbsence::origin)
    }
    pub(crate) fn is_first(&self) -> bool {
        self.predecessor.first().is_some()
    }

    /// Install before initialization. This is the existing deferred FK barrier,
    /// not a new commit witness, and publication alone may remove it.
    pub(crate) fn begin_first_metadata_in(
        &self,
        store: &Store,
        request: &ContributionReplace,
        outer: &CommandEnvelope,
    ) -> Result<()> {
        if !self.is_first() {
            return Ok(());
        }
        self.authorize_metadata_in(store, request, outer)?;
        store.db.execute(
            "INSERT INTO catalog_metadata_permits(item_id,incarnation) VALUES (?,?)",
            params![self.request.subject, outer.admission_incarnation()],
        )?;
        self.require_retained_barrier_in(store, outer)
    }
    pub(crate) fn require_retained_barrier_in(
        &self,
        store: &Store,
        outer: &CommandEnvelope,
    ) -> Result<()> {
        if outer != &self.outer {
            return Err(refused("retained barrier outer differs"));
        }
        let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
        charge(&mut budget, 0, 2)?;
        let exact: bool = store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_metadata_permits WHERE item_id=? AND incarnation=? AND barrier_id=1) AND NOT EXISTS(SELECT 1 FROM catalog_commit_barrier WHERE id=1)",params![self.request.subject,outer.admission_incarnation()],|r|r.get(0))?;
        if !exact {
            return Err(refused("exact retained first-source barrier absent"));
        }
        Ok(())
    }
    pub(crate) fn authorize_initialization_in(
        &self,
        store: &Store,
        request: &MetadataInitializeV2,
        outer: &CommandEnvelope,
    ) -> Result<()> {
        let first = self
            .predecessor
            .first()
            .ok_or_else(|| refused("initialization needs first-source proof"))?;
        first.validate_in(store, outer)?;
        self.require_retained_barrier_in(store, outer)?;
        let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
        let expected = initial_request(self, &mut budget)?;
        budget.serialized(request, 64)?;
        budget.serialized(&expected, 64)?;
        if serde_json::to_string(request)? != serde_json::to_string(&expected)? {
            return Err(refused(
                "initialization differs from audited raw baseline/classification",
            ));
        }
        Ok(())
    }

    pub(super) fn verify_applied_in(
        self,
        store: &Store,
        outer: &CommandEnvelope,
    ) -> Result<VerifiedNfoPut> {
        let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
        if outer != &self.outer {
            return Err(refused("post-effect outer identity/digest differs"));
        }
        let post = fence(store, &mut budget)?;
        let before = self.predecessor.current().presence();
        if post.catalog_id != before.fence.catalog_id
            || post.incarnation != before.fence.incarnation
            || post.generation <= before.fence.generation
        {
            return Err(refused(
                "metadata effect lacks same-incarnation generation advance",
            ));
        }
        absent_receipt(store, outer, &mut budget)?;
        unchanged_owner_dependencies(store, self.predecessor.current(), &mut budget)?;
        if suppressions(store, &before.subject, &mut budget)? != self.suppression_digest {
            return Err(refused("NFO PUT altered independent item suppressions"));
        }
        let (source, schema) = read_applied_source(store, &self, &mut budget)?;
        let actual_tags = verify_applied_tags(store, &self, source.revision(), &mut budget)?;
        let old_item_revision = match before.head.subject_state {
            SubjectState::Active { item_revision } => item_revision,
            _ => return Err(refused("retired PUT")),
        };
        charge(&mut budget, 0, 2)?;
        let item:Option<(i64,bool)>=store.db.query_row("SELECT revision,deleted FROM catalog_entity_revisions WHERE entity_kind='items' AND entity_key=json_array(?)",[&before.subject],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let item_revision = match item {
            Some((revision, false)) if revision >= old_item_revision => revision,
            _ => return Err(refused("post-metadata item revision absent/regressed")),
        };
        let identity = match before.head.source.as_ref().map(|h| &h.document) {
            Some(DocumentState::Present {
                external_identity, ..
            }) => external_identity.as_ref(),
            Some(DocumentState::Withdrawn) => None,
            None if self.predecessor.first().is_some() => None,
            None => return Err(refused("first source requires independent absence proof")),
        };
        budget.serialized(&(source.fields(), &actual_tags, identity, before), 64)?;
        let request = PresenceRequest {
            contract_version: PRESENCE_CONTRACT_VERSION,
            fence: before.fence.clone(),
            subject: before.subject.clone(),
            expected: before.stamp(),
            change: PresenceChange::PublicPut {
                expected_public_revision: self.expected_public_revision,
                contribution: Contribution {
                    fields: source.fields().clone(),
                    tags: actual_tags,
                },
                external_identity: identity.cloned(),
            },
        };
        // Metadata normalization is already complete. The pure presence planner
        // now reasons about the exact persisted fields, not the raw PUT spelling.
        budget.serialized(&(before, &request), 64)?;
        let plan = plan_presence(before, &request).map_err(refused)?;
        match &plan.effect {
            PresenceEffect::Changed {
                document: DocumentEffect::Replace { head },
                ..
            } if head.revision == source.revision() => {}
            _ => {
                return Err(refused(
                    "PublicPut did not yield exact native source replacement",
                ));
            }
        }
        let end = fence(store, &mut budget)?;
        if end != post {
            return Err(refused("post-metadata fence changed during verification"));
        }
        Ok(VerifiedNfoPut {
            predecessor: self.predecessor,
            outer: self.outer,
            plan,
            source,
            schema,
            post_metadata_generation: post.generation,
            item_revision,
        })
    }
}

fn unchanged_owner_dependencies(
    store: &Store,
    current: &CurrentNfoState,
    budget: &mut ProofBudget,
) -> Result<()> {
    let before = current.presence();
    let attribution = before.head.attribution.as_ref();
    charge(budget, 0, 8)?;
    let unchanged = match (attribution, current.native_source()) {
        (Some(attribution), Some(source)) => store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_nfo_states s JOIN catalog_nfo_state_revisions h ON h.item_id=s.item_id AND h.revision=s.current_revision WHERE s.item_id=?1 AND s.current_revision=?2 AND h.source_revision=?3) AND NOT EXISTS(SELECT 1 FROM catalog_nfo_state_revisions WHERE item_id=?1 AND revision>?2) AND NOT EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id=?1) AND EXISTS(SELECT 1 FROM items WHERE id=?1)",params![before.subject,attribution.revision,source.revision()],|r|r.get::<_,bool>(0))?,
        (None, None) => store.db.query_row("SELECT NOT EXISTS(SELECT 1 FROM catalog_nfo_states WHERE item_id=?1) AND NOT EXISTS(SELECT 1 FROM catalog_nfo_state_revisions WHERE item_id=?1) AND NOT EXISTS(SELECT 1 FROM catalog_entity_revisions WHERE entity_kind='catalog_nfo_states' AND entity_key=json_array(?1)) AND NOT EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id=?1) AND EXISTS(SELECT 1 FROM items WHERE id=?1)",[&before.subject],|r|r.get::<_,bool>(0))?,
        _ => false,
    };
    if !unchanged {
        return Err(refused(
            "attribution/retirement changed during metadata composition",
        ));
    }
    let pin_matches=match &before.head.pin {
        PinHead::NeverDecided=>store.db.query_row("SELECT NOT EXISTS(SELECT 1 FROM catalog_identity_pins WHERE item_id=?1 AND namespace='nfo') AND NOT EXISTS(SELECT 1 FROM catalog_identity_pin_revisions WHERE item_id=?1 AND namespace='nfo')",[&before.subject],|r|r.get::<_,bool>(0))?,
        PinHead::Decision{revision,value}=>store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_identity_pins WHERE item_id=?1 AND namespace='nfo' AND current_revision=?2 AND value IS ?3) AND ?2=(SELECT max(revision) FROM catalog_identity_pin_revisions WHERE item_id=?1 AND namespace='nfo')",params![before.subject,revision,value],|r|r.get::<_,bool>(0))?,
    };
    if !pin_matches {
        return Err(refused("independent manual pin changed"));
    }
    let mut query = store
        .db
        .prepare("SELECT value,state_revision FROM catalog_nfo_identity_claims WHERE item_id=?")?;
    let mut rows = query.query([&before.subject])?;
    let claim = rows
        .next()?
        .map(|r| Ok::<_, Error>((text(r, 0, 1024, budget)?, r.get::<_, i64>(1)?)))
        .transpose()?;
    let expected = attribution.and_then(|a| {
        a.active_identity
            .as_deref()
            .map(|value| (value, a.revision))
    });
    if claim
        .as_ref()
        .map(|(value, revision)| (value.as_str(), *revision))
        != expected
    {
        return Err(refused("automatic claim changed before publication"));
    }
    let mut query = store.db.prepare(
        "SELECT profile,file_id,state_revision FROM catalog_nfo_file_attributions WHERE item_id=?",
    )?;
    let mut rows = query.query([&before.subject])?;
    let origin = rows
        .next()?
        .map(|r| {
            Ok::<_, Error>((
                text(r, 0, 256, budget)?,
                text(r, 1, 256, budget)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .transpose()?;
    let expected = attribution.and_then(|attribution| {
        attribution.origin.as_ref().map(|origin| {
            (
                origin.file.profile.as_str(),
                origin.file.file_id.as_str(),
                attribution.revision,
            )
        })
    });
    if origin
        .as_ref()
        .map(|(p, f, r)| (p.as_str(), f.as_str(), *r))
        != expected
    {
        return Err(refused("file attribution changed before publication"));
    }
    Ok(())
}

/// Exact initial schema and raw-history wire are derived from the audited item
/// baseline, never from the post-effect source or current projected item JSON.
fn initial_request(
    permit: &NfoWritePermit,
    budget: &mut ProofBudget,
) -> Result<MetadataInitializeV2> {
    let first = permit
        .predecessor
        .first()
        .ok_or_else(|| refused("first proof absent"))?;
    let raw = first
        .legacy_raw_json()
        .ok_or_else(|| refused("audited initial raw baseline absent"))?;
    budget.raw_json(raw, MAX_METADATA_JSON_DEPTH)?;
    let value = catabolic_core::json::parse(raw, true).map_err(refused)?;
    let object = value
        .as_object()
        .ok_or_else(|| refused("initial baseline not object"))?;
    // Stream the exact policy wire before allocating its owned names/vector.
    // Raw values can be tiny while every policy has several wrapper fields.
    #[derive(Serialize)]
    struct Policy<'a> {
        field: &'a str,
        value_type: PolicyValueType,
        nullable: bool,
        normalization: PolicyNormalization,
    }
    struct Policies<'a>(&'a serde_json::Map<String, serde_json::Value>);
    impl Serialize for Policies<'_> {
        fn serialize<S: serde::Serializer>(
            &self,
            serializer: S,
        ) -> std::result::Result<S::Ok, S::Error> {
            use serde::ser::SerializeSeq;
            let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
            for field in self.0.keys() {
                sequence.serialize_element(&Policy {
                    field,
                    value_type: PolicyValueType::Json,
                    nullable: false,
                    normalization: PolicyNormalization::Exact,
                })?;
            }
            sequence.end()
        }
    }
    #[derive(Serialize)]
    struct Initial<'a> {
        contract_version: u32,
        subject: &'a str,
        policies: Policies<'a>,
        system_fields: &'a BTreeSet<String>,
    }
    budget.serialized(
        &Initial {
            contract_version: 2,
            subject: &permit.request.subject,
            policies: Policies(object),
            system_fields: &permit.initial_system_fields,
        },
        64,
    )?;
    Ok(MetadataInitializeV2 {
        contract_version: 2,
        subject: permit.request.subject.clone(),
        policies: object
            .keys()
            .map(|field| crate::catalog_metadata_store::PolicyField {
                field: field.clone(),
                value_type: PolicyValueType::Json,
                nullable: false,
                normalization: PolicyNormalization::Exact,
            })
            .collect(),
        system_fields: permit.initial_system_fields.clone(),
    })
}
fn verify_initialized_schema(
    store: &Store,
    permit: &NfoWritePermit,
    budget: &mut ProofBudget,
) -> Result<FieldSchema> {
    let request = initial_request(permit, budget)?;
    let mut external = Vec::new();
    let mut internal = Vec::new();
    budget.serialized(&request, 64)?;
    for field in &request.policies {
        let pair = (
            field.field.clone(),
            FieldPolicy {
                value_type: ValueType::Json,
                nullable: false,
                normalization: Normalization::Exact,
            },
        );
        if request.system_fields.contains(&field.field) {
            internal.push(pair);
        } else {
            external.push(pair);
        }
    }
    let schema = FieldSchema::new_v2(external, internal).map_err(refused)?;
    let raw = permit
        .predecessor
        .first()
        .and_then(VerifiedFirstNfoAbsence::legacy_raw_json)
        .ok_or_else(|| refused("initial raw absent"))?;
    budget.serialized(&request.policies, 64)?;
    budget.serialized(&request.system_fields, 64)?;
    let policies = serde_json::to_string(&request.policies)?;
    let system = serde_json::to_string(&request.system_fields)?;
    let command = &permit.outer;
    charge(budget, 0, 2)?;
    let exact: bool = store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_metadata_subjects WHERE item_id=?1 AND contract_version=2 AND schema_fingerprint=?2 AND policies_json=?3 AND legacy_raw_json=?4 AND command_catalog_id=?5 AND command_host_namespace=?6 AND command_operation_id=?7) AND EXISTS(SELECT 1 FROM catalog_metadata_schemas WHERE item_id=?1 AND schema_fingerprint=?2 AND contract_version=2 AND policies_json=?3 AND system_fields_json=?8 AND prior_fingerprint IS NULL AND command_catalog_id=?5 AND command_host_namespace=?6 AND command_operation_id=?7)",params![request.subject,schema.fingerprint(),policies,raw,command.key().catalog_id(),command.key().host_namespace(),command.key().operation_id(),system],|r|r.get(0))?;
    if !exact {
        return Err(refused("exact audited initialization root differs"));
    }
    let evidence = r#"{"origin":"unknown","import":"items.metadata"}"#;
    budget.raw_json(raw, MAX_METADATA_JSON_DEPTH)?;
    budget.raw_json(evidence, MAX_METADATA_JSON_DEPTH)?;
    let legacy = ContributionDocument::from_json_with_system_fields(
        &schema,
        ContributionKey::new(
            &request.subject,
            crate::catalog_metadata_store::LEGACY_NAMESPACE,
            crate::catalog_metadata_store::LEGACY_RECORD_ID,
        )
        .map_err(refused)?,
        SourceKind::LegacyImport,
        1,
        raw,
        evidence,
    )
    .map_err(refused)?;
    budget.serialized(legacy.fields(), MAX_METADATA_JSON_DEPTH)?;
    budget.canonical_json(legacy.evidence())?;
    budget.serialized(&request, 64)?;
    let fields = serde_json::to_string(legacy.fields())?;
    let evidence = catabolic_core::encode(legacy.evidence());
    let mutation = serde_json::to_string(&request)?;
    charge(budget, 0, 1)?;
    let exact: bool = store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_metadata_history WHERE item_id=?1 AND namespace=?2 AND record_id=?3 AND revision=1 AND source_kind='legacy_import' AND schema_fingerprint=?4 AND fields_json=?5 AND evidence_json=?6 AND raw_mutation_json=?7 AND command_catalog_id=?8 AND command_host_namespace=?9 AND command_operation_id=?10)",params![request.subject,crate::catalog_metadata_store::LEGACY_NAMESPACE,crate::catalog_metadata_store::LEGACY_RECORD_ID,schema.fingerprint(),fields,evidence,mutation,command.key().catalog_id(),command.key().host_namespace(),command.key().operation_id()],|r|r.get(0))?;
    if !exact {
        return Err(refused(
            "initial legacy history differs from audited raw baseline",
        ));
    }
    Ok(schema)
}

fn read_applied_source(
    store: &Store,
    permit: &NfoWritePermit,
    budget: &mut ProofBudget,
) -> Result<(ContributionDocument, FieldSchema)> {
    let request = &permit.request;
    let initialized_schema;
    let old_schema = if let Some(schema) = permit.predecessor.current().schema() {
        schema
    } else {
        initialized_schema = verify_initialized_schema(store, permit, budget)?;
        &initialized_schema
    };
    let expected_revision = request
        .expected_revision
        .checked_add(1)
        .ok_or_else(|| refused("native source revision exhausted"))?;
    charge(budget, 0, 6)?;
    let mut query=store.db.prepare("SELECT s.revision,s.source_kind,s.schema_fingerprint,s.fields_json,s.evidence_json,h.schema_fingerprint FROM catalog_metadata_sources s JOIN catalog_metadata_schema_heads h ON h.item_id=s.item_id WHERE s.item_id=? AND s.namespace='motion.nfo' AND s.record_id='document'")?;
    let mut rows = query.query([&request.subject])?;
    let row = rows
        .next()?
        .ok_or_else(|| refused("actual normalized source absent"))?;
    let revision: i64 = row.get(0)?;
    let kind = text(row, 1, 16, budget)?;
    let fingerprint = text(row, 2, 64, budget)?;
    let fields = text(row, 3, MAX_METADATA_DOCUMENT_BYTES, budget)?;
    let evidence = text(row, 4, MAX_METADATA_EVIDENCE_BYTES, budget)?;
    let current = text(row, 5, 64, budget)?;
    if revision != expected_revision || kind != "provider" || fingerprint != current {
        return Err(refused(
            "actual source kind/revision/current schema differs",
        ));
    }
    let mut query=store.db.prepare("SELECT policies_json,system_fields_json FROM catalog_metadata_schemas WHERE item_id=? AND schema_fingerprint=?")?;
    let mut rows = query.query(params![request.subject, fingerprint])?;
    let row = rows
        .next()?
        .ok_or_else(|| refused("actual registered schema absent"))?;
    // Borrow/preflight before the established registry helper allocates its raw
    // strings and decoded policies; normalization stays with the core owner.
    for index in 0..2 {
        let ValueRef::Text(raw) = row.get_ref(index)? else {
            return Err(refused("schema text absent"));
        };
        if raw.len() > MAX_METADATA_SNAPSHOT_BYTES {
            return Err(refused("schema byte bound"));
        }
        let raw = std::str::from_utf8(raw).map_err(refused)?;
        budget.raw_json(raw, MAX_METADATA_JSON_DEPTH)?;
        budget.raw_json(raw, MAX_METADATA_JSON_DEPTH)?;
    }
    let schema =
        read_registered_schema(&store.db, &request.subject, &fingerprint).map_err(refused)?;
    validate_schema_extension(old_schema, &schema).map_err(refused)?;
    if schema.system_fields() != old_schema.system_fields() {
        return Err(refused("NFO PUT reclassified system fields"));
    }
    if schema.fingerprint() != old_schema.fingerprint() {
        charge(budget, 0, 1)?;
        let linked:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_metadata_schemas WHERE item_id=? AND schema_fingerprint=? AND prior_fingerprint=? AND command_catalog_id=? AND command_host_namespace=? AND command_operation_id=?)",params![request.subject,schema.fingerprint(),old_schema.fingerprint(),permit.outer.key().catalog_id(),permit.outer.key().host_namespace(),permit.outer.key().operation_id()],|r|r.get(0))?;
        if !linked {
            return Err(refused(
                "schema extension is not the exact outer owner step",
            ));
        }
    }
    charge(budget, 0, 1)?;
    let exact:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_metadata_history WHERE item_id=?1 AND namespace='motion.nfo' AND record_id='document' AND revision=?2 AND source_kind='provider' AND schema_fingerprint=?3 AND fields_json=?4 AND evidence_json=?5 AND raw_mutation_json=?6 AND command_catalog_id=?7 AND command_host_namespace=?8 AND command_operation_id=?9) AND ?2=(SELECT max(revision) FROM catalog_metadata_history WHERE item_id=?1 AND namespace='motion.nfo' AND record_id='document')",params![request.subject,revision,fingerprint,fields,evidence,permit.raw_request,permit.outer.key().catalog_id(),permit.outer.key().host_namespace(),permit.outer.key().operation_id()],|r|r.get(0))?;
    if !exact {
        return Err(refused(
            "actual source/raw history/outer command linkage differs",
        ));
    }
    budget.raw_json(&fields, MAX_METADATA_JSON_DEPTH)?;
    budget.raw_json(&evidence, MAX_METADATA_JSON_DEPTH)?;
    let source = ContributionDocument::from_json(
        &schema,
        ContributionKey::new(&request.subject, NAMESPACE, RECORD).map_err(refused)?,
        SourceKind::Provider,
        revision,
        &fields,
        &evidence,
    )
    .map_err(refused)?;
    budget.serialized(source.fields(), MAX_METADATA_JSON_DEPTH)?;
    budget.canonical_json(source.evidence())?;
    if serde_json::to_string(source.fields())? != fields
        || catabolic_core::encode(source.evidence()) != evidence
    {
        return Err(refused(
            "actual source is not normalized canonical owner encoding",
        ));
    }
    // Check the request with the ACTUAL registry after the owner runs. This does
    // not predict a schema or require raw fields to equal normalized values.
    budget.raw_json(&request.fields_json, MAX_METADATA_JSON_DEPTH)?;
    budget.raw_json(&request.evidence_json, MAX_METADATA_JSON_DEPTH)?;
    let normalized = ContributionReplacement::from_json(
        &schema,
        ContributionKey::new(&request.subject, NAMESPACE, RECORD).map_err(refused)?,
        SourceKind::Provider,
        request.expected_revision,
        &request.fields_json,
        &request.evidence_json,
    )
    .map_err(refused)?;
    charge(budget, 0, schema.fields().len())?;
    for (field, policy) in schema.fields() {
        if !old_schema.fields().contains_key(field)
            && (!normalized.fields().contains_key(field)
                || *policy
                    != (FieldPolicy {
                        value_type: ValueType::Json,
                        nullable: false,
                        normalization: Normalization::Exact,
                    }))
        {
            return Err(refused("schema extension exceeds exact request additions"));
        }
    }
    if normalized.fields() != source.fields() || normalized.evidence() != source.evidence() {
        return Err(refused(
            "actual source differs from exact normalized request",
        ));
    }
    verify_first_origin_effects(store, permit, &schema, budget)?;
    Ok((source, schema))
}

fn verify_first_origin_effects(
    store: &Store,
    permit: &NfoWritePermit,
    schema: &FieldSchema,
    budget: &mut ProofBudget,
) -> Result<()> {
    let Some(first) = permit
        .predecessor
        .first()
        .filter(|first| first.origin().is_some())
    else {
        return Ok(());
    };
    let raw = first
        .legacy_raw_json()
        .ok_or_else(|| refused("fresh origin baseline absent"))?;
    budget.raw_json(raw, MAX_METADATA_JSON_DEPTH)?;
    budget.raw_json(
        r#"{"origin":"unknown","import":"items.metadata"}"#,
        MAX_METADATA_JSON_DEPTH,
    )?;
    let legacy = ContributionDocument::from_json_with_system_fields(
        schema,
        ContributionKey::new(
            &permit.request.subject,
            crate::catalog_metadata_store::LEGACY_NAMESPACE,
            crate::catalog_metadata_store::LEGACY_RECORD_ID,
        )
        .map_err(refused)?,
        SourceKind::LegacyImport,
        1,
        raw,
        r#"{"origin":"unknown","import":"items.metadata"}"#,
    )
    .map_err(refused)?;
    budget.serialized(&legacy, 64)?;
    let reconciled = catabolic_core::catalog_metadata::plan_legacy_reconciliation(
        schema,
        &legacy,
        1,
        &["title".into()],
    )
    .map_err(refused)?;
    let title = legacy
        .fields()
        .get("title")
        .ok_or_else(|| refused("fresh origin title absent"))?;
    #[derive(Serialize)]
    struct ScanFields<'a> {
        title: &'a serde_json::Value,
    }
    #[derive(Serialize)]
    struct ScanEvidence<'a> {
        owner: &'static str,
        creation_intent_digest: &'a str,
    }
    charge(budget, 64, 1)?;
    let digest = catabolic_core::sha256(first.creation()?.canonical_payload());
    let fields = ScanFields { title };
    let evidence = ScanEvidence {
        owner: "motion.receipt-origin.v1",
        creation_intent_digest: &digest,
    };
    budget.serialized(&fields, MAX_METADATA_JSON_DEPTH)?;
    budget.serialized(&evidence, MAX_METADATA_JSON_DEPTH)?;
    let fields = serde_json::to_string(&fields)?;
    let evidence = serde_json::to_string(&evidence)?;
    budget.raw_json(&fields, MAX_METADATA_JSON_DEPTH)?;
    budget.raw_json(&evidence, MAX_METADATA_JSON_DEPTH)?;
    let scan = ContributionDocument::from_json(
        schema,
        ContributionKey::new(&permit.request.subject, "motion.scan", "origin").map_err(refused)?,
        SourceKind::Scan,
        1,
        &fields,
        &evidence,
    )
    .map_err(refused)?;
    for (expected, kind) in [(&reconciled, "legacy_import"), (&scan, "scan")] {
        budget.serialized(expected.fields(), MAX_METADATA_JSON_DEPTH)?;
        budget.canonical_json(expected.evidence())?;
        let fields = serde_json::to_string(expected.fields())?;
        let evidence = catabolic_core::encode(expected.evidence());
        charge(budget, 0, 2)?;
        let exact: bool = store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_metadata_sources s JOIN catalog_metadata_history h ON h.item_id=s.item_id AND h.namespace=s.namespace AND h.record_id=s.record_id AND h.revision=s.revision WHERE s.item_id=?1 AND s.namespace=?2 AND s.record_id=?3 AND s.revision=?4 AND s.source_kind=?5 AND s.schema_fingerprint=?6 AND s.fields_json=?7 AND s.evidence_json=?8 AND h.source_kind=s.source_kind AND h.schema_fingerprint=s.schema_fingerprint AND h.fields_json=s.fields_json AND h.evidence_json=s.evidence_json AND h.raw_mutation_json=?9 AND h.command_catalog_id=?10 AND h.command_host_namespace=?11 AND h.command_operation_id=?12) AND ?4=(SELECT max(revision) FROM catalog_metadata_history WHERE item_id=?1 AND namespace=?2 AND record_id=?3)",params![permit.request.subject,expected.key().namespace(),expected.key().record_id(),expected.revision(),kind,schema.fingerprint(),fields,evidence,permit.raw_request,permit.outer.key().catalog_id(),permit.outer.key().host_namespace(),permit.outer.key().operation_id()],|r|r.get(0))?;
        if !exact {
            return Err(refused(
                "fresh create scan/legacy reconciliation effects differ",
            ));
        }
    }
    Ok(())
}

fn verify_applied_tags(
    store: &Store,
    permit: &NfoWritePermit,
    revision: i64,
    budget: &mut ProofBudget,
) -> Result<BTreeSet<String>> {
    let subject = &permit.request.subject;
    let mut expected_ids = BTreeSet::new();
    for name in &permit.tags {
        let mut query = store
            .db
            .prepare("SELECT tag_id FROM tag_names WHERE name=?")?;
        let mut rows = query.query([name])?;
        let row = rows
            .next()?
            .ok_or_else(|| refused("requested tag mapping absent"))?;
        let id = text(row, 0, 256, budget)?;
        expected_ids.insert(id);
    }
    let mut actual_ids = BTreeSet::new();
    let mut actual_names = BTreeSet::new();
    let mut query=store.db.prepare("SELECT a.source,a.active,a.tag_id,t.name,a.id FROM item_tags a LEFT JOIN tags t ON t.id=a.tag_id WHERE a.item_id=?")?;
    let mut rows = query.query([subject])?;
    while let Some(row) = rows.next()? {
        charge(budget, 0, 5)?;
        let namespace = text(row, 0, 256, budget)?;
        let active: i64 = row.get(1)?;
        if namespace != NAMESPACE || active == 0 {
            continue;
        }
        if active != 1 || actual_ids.len() >= 100 {
            return Err(refused("actual tag activity/count bound"));
        }
        let id = text(row, 2, 256, budget)?;
        let name = text(row, 3, 100, budget)?;
        let assertion = text(row, 4, 256, budget)?;
        let tracked:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_entity_revisions WHERE entity_kind='item_tags' AND entity_key=json_array(?1) AND revision>0 AND deleted=0) AND EXISTS(SELECT 1 FROM catalog_entity_revisions WHERE entity_kind='tags' AND entity_key=json_array(?2) AND revision>0 AND deleted=0) AND EXISTS(SELECT 1 FROM tag_names WHERE tag_id=?2 AND name=?3) AND NOT EXISTS(SELECT 1 FROM catalog_tag_redirects WHERE source_tag_id=?2)",params![assertion,id,name],|r|r.get(0))?;
        if !tracked || !actual_ids.insert(id) || !actual_names.insert(name) {
            return Err(refused("untracked or duplicate actual tag ownership"));
        }
    }
    if actual_ids != expected_ids {
        return Err(refused(
            "actual tags differ from exact requested resolved vocabulary",
        ));
    }
    #[derive(Serialize)]
    struct After<'a> {
        excluded_tags: &'a [String],
        tags: &'a BTreeSet<String>,
    }
    let after = After {
        excluded_tags: &[],
        tags: &permit.tags,
    };
    budget.serialized(&after, MAX_METADATA_JSON_DEPTH)?;
    let expected_after = serde_json::to_string(&after)?;
    charge(budget, 0, 1)?;
    let exact:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_metadata_tag_history WHERE item_id=?1 AND namespace='motion.nfo' AND record_id='document' AND revision=?2 AND after_json=?3 AND command_catalog_id=?4 AND command_host_namespace=?5 AND command_operation_id=?6) AND ?2=(SELECT max(revision) FROM catalog_metadata_tag_history WHERE item_id=?1 AND namespace='motion.nfo' AND record_id='document')",params![subject,revision,expected_after,permit.outer.key().catalog_id(),permit.outer.key().host_namespace(),permit.outer.key().operation_id()],|r|r.get(0))?;
    if !exact {
        return Err(refused("new tag history/revision/outer key differs"));
    }
    Ok(actual_names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation_budget::{BudgetLimits, OperationBudget};
    use catabolic_core::catalog_command::CommandKey;
    use serde_json::{Value, json};
    use std::time::Duration;

    fn budget() -> ProofBudget {
        ProofBudget::new(
            OperationBudget::new(
                Duration::from_secs(30),
                BudgetLimits {
                    items: usize::MAX,
                    values: usize::MAX,
                    bytes: usize::MAX,
                    work: usize::MAX,
                },
            )
            .unwrap(),
        )
    }
    fn request() -> ContributionReplace {
        ContributionReplace {
            contract_version: 2,
            subject: "item".into(),
            namespace: NAMESPACE.into(),
            record_id: RECORD.into(),
            expected_schema_fingerprint: Some("a".repeat(64)),
            expected_revision: 7,
            fields_json: r#"{"title":"  retained raw spelling  "}"#.into(),
            evidence_json: "{}".into(),
            tags: vec!["genre:drama".into()],
            excluded_tags: vec![],
            origin_creation_intent: None,
        }
    }
    fn payload(request: &ContributionReplace) -> Value {
        json!({"operation":"nfo_public_put","contract_version":1,"expected_public_revision":3,"request":request})
    }
    fn command(payload: Value) -> CommandEnvelope {
        CommandEnvelope::new(
            COMMAND_CONTRACT_VERSION,
            CommandKey::new("catalog", "motion", "put").unwrap(),
            "incarnation",
            50,
            &payload.to_string(),
        )
        .unwrap()
    }
    #[test]
    fn exact_closed_outer_keeps_raw_fields_and_native_public_cas_distinct() {
        let request = request();
        let command = command(payload(&request));
        assert!(matches_outer(&command, &request, 3, &mut budget()).is_ok());
        assert!(matches_outer(&command, &request, 7, &mut budget()).is_err());
        assert_eq!(
            request_json(&request, &mut budget()).unwrap(),
            serde_json::to_string(&request).unwrap()
        );
    }
    #[test]
    fn operation_name_or_extra_identity_flag_is_not_a_permit() {
        let request = request();
        assert!(
            matches_outer(
                &command(json!({"operation":"nfo_public_put"})),
                &request,
                3,
                &mut budget()
            )
            .is_err()
        );
        let mut altered = payload(&request);
        altered["automatic_identity"] = json!("not a grant");
        assert!(matches_outer(&command(altered), &request, 3, &mut budget()).is_err());
    }
    #[test]
    fn changed_inner_bytes_or_unknown_inner_fields_refuse() {
        let request = request();
        let outer = command(payload(&request));
        let mut changed = request.clone();
        changed.fields_json = r#"{"title":"retained raw spelling"}"#.into();
        assert!(matches_outer(&outer, &changed, 3, &mut budget()).is_err());
        let mut altered = payload(&request);
        altered["request"]["allow_nfo"] = json!(true);
        assert!(matches_outer(&command(altered), &request, 3, &mut budget()).is_err());
    }
    #[test]
    fn raw_history_escape_expansion_is_bounded_before_append() {
        let mut request = request();
        request.fields_json = "\u{0001}".repeat(MAX_METADATA_DOCUMENT_BYTES);
        assert!(request_json(&request, &mut budget()).is_err());
    }
}
