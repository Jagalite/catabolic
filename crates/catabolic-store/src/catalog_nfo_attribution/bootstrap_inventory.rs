//! Private, complete bootstrap discovery under a retained writer. These are
//! descriptions of retained facts, not recovered envelopes, import output,
//! provenance grants, or permission to replay a command. Compacted receipts keep
//! their payload commitment; callers still need independently verified envelopes.
mod tags;
use super::proof_budget::ProofBudget;
use crate::{Error, Result, Store, migration::BootstrapWorkspace, operation_budget::BudgetCharge};
use catabolic_core::catalog_command::{CommandKey, PayloadDigest};
use rusqlite::{Row, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
pub use tags::{Assertion, FileInventory, MergeHistory, TagHistory};

fn refused(message: impl std::fmt::Display) -> Error {
    Error(format!("catalog_nfo_inventory_refused: {message}"))
}
fn raw_digest(raw: &str) -> String {
    format!("{:x}", Sha256::digest(raw.as_bytes()))
}
fn text<'a>(row: &'a Row<'_>, index: usize) -> Result<&'a str> {
    row.get_ref(index)?
        .as_str()
        .map_err(|_| refused("non-text retained fact"))
}
/// Charge borrowed SQLite values before any String, digest or descriptor copy.
/// Reserve only the new digest bytes, in addition to borrowed inputs/scalars.
fn charge_row(
    row: &Row<'_>,
    columns: &[usize],
    digests: usize,
    budget: &mut ProofBudget,
) -> Result<()> {
    let mut bytes = digests
        .checked_mul(64)
        .ok_or_else(|| refused("digest bytes"))?;
    for index in columns {
        bytes = bytes
            .checked_add(text(row, *index)?.len())
            .ok_or_else(|| refused("row bytes"))?;
    }
    budget.charge(BudgetCharge {
        items: 1,
        bytes,
        values: columns.len() + 16,
        work: bytes / 64 + columns.len() + 16,
    })
}
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct HeadStamp {
    revision: i64,
    deleted: bool,
}
fn head(
    store: &Store,
    kind: &str,
    key: &str,
    budget: &mut ProofBudget,
) -> Result<Option<HeadStamp>> {
    budget.charge(BudgetCharge {
        values: 2,
        work: 2,
        ..Default::default()
    })?;
    let mut query = store.db.prepare("SELECT revision,deleted FROM catalog_entity_revisions WHERE entity_kind=? AND entity_key=?")?;
    let mut rows = query.query(params![kind, key])?;
    rows.next()?
        .map(|row| {
            Ok(HeadStamp {
                revision: row.get(0)?,
                deleted: row.get(1)?,
            })
        })
        .transpose()
}
fn active(head: &Option<HeadStamp>) -> bool {
    head.as_ref().is_some_and(|h| h.revision > 0 && !h.deleted)
}
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct SourceHead {
    revision: i64,
    kind: String,
    fingerprint: String,
    fields_digest: String,
    evidence_digest: String,
    entity: Option<HeadStamp>,
}
impl SourceHead {
    pub fn revision(&self) -> i64 {
        self.revision
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub(super) fn matches(
        &self,
        revision: i64,
        fingerprint: &str,
        fields: &str,
        evidence: &str,
    ) -> bool {
        self.revision == revision
            && self.kind == "provider"
            && self.fingerprint == fingerprint
            && self.fields_digest == raw_digest(fields)
            && self.evidence_digest == raw_digest(evidence)
    }
}
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct SchemaHead {
    fingerprint: String,
    contract: Option<i64>,
    entity: Option<HeadStamp>,
    registry_entity: Option<HeadStamp>,
}
impl SchemaHead {
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Retirement {
    kind: String,
    command: CommandKey,
}
#[derive(Debug, PartialEq, Eq, Serialize)]
pub enum ReceiptCommitment {
    Missing,
    Retained {
        payload_digest: PayloadDigest,
        admission_incarnation: String,
        contract_version: i64,
        expected_generation: i64,
        committed_generation: i64,
    },
}
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct HistoryStep {
    revision: i64,
    kind: String,
    fingerprint: String,
    fields_digest: String,
    evidence_digest: String,
    raw_effect_digest: String,
    admitted_at_us: i64,
    command: CommandKey,
    receipt: ReceiptCommitment,
    schema_registered: bool,
}
/// A subject is retained even when these obligations prevent bootstrap. None is
/// an absent *native row*, never positive automatic-identity/sidecar absence.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub enum Obligation {
    MissingItem,
    MissingItemHead,
    MissingSource,
    MissingSourceHead,
    UnsupportedRecord,
    UnsupportedSourceKind,
    MissingHistory,
    HistoryGap {
        expected: i64,
        actual: i64,
    },
    SourceHistoryMismatch,
    MissingSchema,
    MissingSchemaHead,
    MissingRegisteredSchema,
    MissingReceipt,
    ForeignCommandCatalog,
    MissingRetirementHead,
    InconsistentRetirement,
    UnsupportedTagMerge,
    MissingAssertionHead,
    UnlinkedTagHistory,
    /// The permanent assertion key has no recoverable namespace/subject. Only
    /// an exact authenticated collision plus a nonreset revision2 head is
    /// accounted initially; higher counters need independent prior history.
    UnresolvedAssertionTombstone {
        entity_kind: String,
        entity_key: String,
        revision: i64,
        reason: String,
    },
}
#[derive(Debug, PartialEq, Eq, Serialize, Default)]
pub struct SubjectInventory {
    item_exists: bool,
    item_head: Option<HeadStamp>,
    retirement: Option<Retirement>,
    retirement_head: Option<HeadStamp>,
    schema: Option<SchemaHead>,
    sources: BTreeMap<String, SourceHead>,
    #[serde(serialize_with = "serialize_history")]
    history: BTreeMap<(String, i64), HistoryStep>,
    assertions: BTreeMap<String, tags::Assertion>,
    #[serde(serialize_with = "tags::serialize_history")]
    tag_history: BTreeMap<(String, i64), tags::TagHistory>,
    tag_merges: std::collections::BTreeSet<String>,
    obligations: Vec<Obligation>,
}
fn serialize_history<S: serde::Serializer>(
    history: &BTreeMap<(String, i64), HistoryStep>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.collect_seq(history.iter().map(|((record, _), step)| (record, step)))
}
impl HistoryStep {
    pub fn revision(&self) -> i64 {
        self.revision
    }
    pub fn command(&self) -> &CommandKey {
        &self.command
    }
    pub fn receipt(&self) -> &ReceiptCommitment {
        &self.receipt
    }
    pub fn raw_effect_digest(&self) -> &str {
        &self.raw_effect_digest
    }
}
impl SubjectInventory {
    pub fn history(&self) -> impl Iterator<Item = (&str, &HistoryStep)> {
        self.history
            .iter()
            .map(|((record, _), step)| (record.as_str(), step))
    }

    pub fn source(&self) -> Option<&SourceHead> {
        self.sources.get("document")
    }
    pub fn schema(&self) -> Option<&SchemaHead> {
        self.schema.as_ref()
    }
    pub fn retired(&self) -> bool {
        self.retirement.is_some()
    }
    pub fn first_revision(&self) -> Option<&HistoryStep> {
        self.history
            .iter()
            .find(|((record, _), _)| record == "document")
            .map(|(_, step)| step)
    }
    pub fn obligations(&self) -> &[Obligation] {
        &self.obligations
    }
    fn classify(&mut self, catalog: &str) {
        let mut issues = Vec::new();
        tags::classify(self, &mut issues, catalog);
        let issues = &mut issues;
        if !self.history.keys().any(|(record, _)| record == "document") {
            issues.push(Obligation::MissingHistory);
        }
        if !self.item_exists {
            issues.push(Obligation::MissingItem);
        }
        if !active(&self.item_head) {
            issues.push(Obligation::MissingItemHead);
        }
        if self.retirement.is_some() && !active(&self.retirement_head) {
            issues.push(Obligation::MissingRetirementHead);
        }
        if self.retirement.is_none() && self.retirement_head.is_some() {
            issues.push(Obligation::InconsistentRetirement);
        }
        match &self.schema {
            None => issues.push(Obligation::MissingSchema),
            Some(schema) => {
                if !active(&schema.entity) {
                    issues.push(Obligation::MissingSchemaHead);
                }
                if !active(&schema.registry_entity) || !matches!(schema.contract, Some(1 | 2)) {
                    issues.push(Obligation::MissingRegisteredSchema);
                }
            }
        }
        if !self.sources.contains_key("document") {
            issues.push(Obligation::MissingSource);
        }
        if self.sources.keys().any(|r| r != "document")
            || self.history.keys().any(|(r, _)| r != "document")
        {
            issues.push(Obligation::UnsupportedRecord);
        }
        if self.sources.values().any(|s| s.kind != "provider")
            || self.history.values().any(|s| s.kind != "provider")
        {
            issues.push(Obligation::UnsupportedSourceKind);
        }
        for (record, source) in &self.sources {
            if !active(&source.entity) {
                issues.push(Obligation::MissingSourceHead);
            }
            let last = self
                .history
                .iter()
                .filter(|((r, _), _)| r == record)
                .next_back()
                .map(|(_, s)| s);
            match last {
                None => issues.push(Obligation::MissingHistory),
                Some(last)
                    if source.revision != last.revision
                        || source.kind != last.kind
                        || source.fingerprint != last.fingerprint
                        || source.fields_digest != last.fields_digest
                        || source.evidence_digest != last.evidence_digest =>
                {
                    issues.push(Obligation::SourceHistoryMismatch)
                }
                Some(_) => (),
            }
        }
        let mut prior_record = None;
        let mut expected: Option<i64> = Some(1);
        for ((record, revision), step) in &self.history {
            if prior_record != Some(record) {
                expected = Some(1);
                prior_record = Some(record);
            }
            if expected != Some(*revision) {
                issues.push(Obligation::HistoryGap {
                    expected: expected.unwrap_or(i64::MAX),
                    actual: *revision,
                });
            }
            expected = revision.checked_add(1);
            if matches!(step.receipt, ReceiptCommitment::Missing) {
                issues.push(Obligation::MissingReceipt);
            }
            if step.command.catalog_id() != catalog {
                issues.push(Obligation::ForeignCommandCatalog);
            }
            if !step.schema_registered {
                issues.push(Obligation::MissingRegisteredSchema);
            }
        }
        self.obligations = std::mem::take(issues);
    }
}
#[derive(Debug, Serialize)]
pub struct BootstrapInventory {
    catalog_id: String,
    incarnation: String,
    generation: i64,
    subjects: BTreeMap<String, SubjectInventory>,
    files: BTreeMap<String, tags::FileInventory>,
    tag_merges: BTreeMap<String, tags::MergeHistory>,
    // Keep the existing digest/wire shape unchanged when no global gap exists.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    global_obligations: Vec<Obligation>,
    #[serde(skip)]
    digest: String,
}
impl BootstrapInventory {
    pub fn catalog_id(&self) -> &str {
        &self.catalog_id
    }
    pub fn incarnation(&self) -> &str {
        &self.incarnation
    }
    pub fn generation(&self) -> i64 {
        self.generation
    }

    pub fn subjects(&self) -> &BTreeMap<String, SubjectInventory> {
        &self.subjects
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn global_obligations(&self) -> &[Obligation] {
        &self.global_obligations
    }
    pub(super) fn require_complete(&self) -> Result<()> {
        if let Some(obligation) = self.global_obligations.first() {
            return Err(refused(format!("global: {obligation:?}")));
        }
        if let Some((file, _)) = self.files.first_key_value() {
            return Err(refused(format!("UnsupportedFileTags: {file}")));
        }
        if let Some((subject, description)) = self
            .subjects
            .iter()
            .find(|(_, s)| !s.obligations.is_empty())
        {
            return Err(refused(format!(
                "subject {subject}: {:?}",
                description.obligations
            )));
        }
        Ok(())
    }
}
fn receipt(store: &Store, key: &CommandKey, budget: &mut ProofBudget) -> Result<ReceiptCommitment> {
    budget.charge(BudgetCharge {
        work: 1,
        ..Default::default()
    })?;
    let mut query=store.db.prepare("SELECT canonicalization_version,payload_digest,admission_incarnation,contract_version,expected_generation,committed_generation FROM catalog_command_receipts WHERE catalog_id=? AND host_namespace=? AND operation_id=?")?;
    let mut rows = query.query(params![
        key.catalog_id(),
        key.host_namespace(),
        key.operation_id()
    ])?;
    let Some(row) = rows.next()? else {
        return Ok(ReceiptCommitment::Missing);
    };
    charge_row(row, &[1, 2], 0, budget)?;
    Ok(ReceiptCommitment::Retained {
        payload_digest: PayloadDigest::new(row.get(0)?, text(row, 1)?).map_err(refused)?,
        admission_incarnation: text(row, 2)?.into(),
        contract_version: row.get(3)?,
        expected_generation: row.get(4)?,
        committed_generation: row.get(5)?,
    })
}
fn scope(
    store: &Store,
    budget: &mut ProofBudget,
    control: Option<&BootstrapWorkspace<'_>>,
) -> Result<(String, String, i64)> {
    if !matches!(store.schema_version, 40 | 41) || !store.writable || store.db.is_autocommit() {
        return Err(refused("native40/41 retained writer required"));
    }
    store.transaction_available_with_control_workspace(control)?;
    let mut query = store
        .db
        .prepare("SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1")?;
    let mut rows = query.query([])?;
    let row = rows
        .next()?
        .ok_or_else(|| refused("catalog state absent"))?;
    charge_row(row, &[0, 1], 0, budget)?;
    if text(row, 0)? != store.database_id
        || Some(text(row, 1)?) != store.catalog_incarnation.as_deref()
    {
        return Err(refused("catalog/incarnation changed"));
    }
    Ok((text(row, 0)?.into(), text(row, 1)?.into(), row.get(2)?))
}
/// No filtering, UNION, DISTINCT, aggregate or sorting can hide an unbounded
/// ledger scan before admission. Every visited row is charged, including other
/// namespaces. Only NFO effect columns are projected; those raw borrowed buffers
/// are charged before hashing, and are never retained as reconstructed envelopes.
pub(super) fn capture_in(store: &Store) -> Result<BootstrapInventory> {
    capture_with_control_workspace(store, None)
}

pub(super) fn capture_with_control_workspace(
    store: &Store,
    control: Option<&BootstrapWorkspace<'_>>,
) -> Result<BootstrapInventory> {
    let shared = store.catalog_operation_budget()?;
    let mut budget = ProofBudget::new(shared.clone());
    let (catalog_id, incarnation, generation) = scope(store, &mut budget, control)?;
    let mut subjects: BTreeMap<String, SubjectInventory> = BTreeMap::new();
    for history in [false, true] {
        let sql = if history {
            "SELECT item_id,namespace,record_id,revision,CASE WHEN namespace='motion.nfo' THEN source_kind END,CASE WHEN namespace='motion.nfo' THEN schema_fingerprint END,CASE WHEN namespace='motion.nfo' THEN fields_json END,CASE WHEN namespace='motion.nfo' THEN evidence_json END,CASE WHEN namespace='motion.nfo' THEN raw_mutation_json END,CASE WHEN namespace='motion.nfo' THEN command_catalog_id END,CASE WHEN namespace='motion.nfo' THEN command_host_namespace END,CASE WHEN namespace='motion.nfo' THEN command_operation_id END,admitted_at_us FROM catalog_metadata_history"
        } else {
            "SELECT item_id,namespace,record_id,revision,CASE WHEN namespace='motion.nfo' THEN source_kind END,CASE WHEN namespace='motion.nfo' THEN schema_fingerprint END,CASE WHEN namespace='motion.nfo' THEN fields_json END,CASE WHEN namespace='motion.nfo' THEN evidence_json END FROM catalog_metadata_sources"
        };
        let mut query = store.db.prepare(sql)?;
        let mut rows = query.query([])?;
        loop {
            // Scanning unrelated catalog rows consumes the inherited operation
            // only. The local NFO closure cap must not become a catalog row cap.
            shared
                .charge(BudgetCharge {
                    work: 1,
                    ..Default::default()
                })
                .map_err(|error| refused(format!("{error:?}")))?;
            let Some(row) = rows.next()? else { break };
            let is_nfo = text(row, 1)? == "motion.nfo";
            if !is_nfo {
                let mut bytes = 0usize;
                for index in [0, 1, 2] {
                    bytes = bytes
                        .checked_add(text(row, index)?.len())
                        .ok_or_else(|| refused("scan key bytes"))?;
                }
                // Charge actual scan keys to the inherited operation, but not
                // as locally retained/hashing bytes: no descriptor is allocated.
                shared
                    .charge(BudgetCharge {
                        items: 1,
                        bytes,
                        values: 3,
                        work: bytes / 64 + 4,
                    })
                    .map_err(|error| refused(format!("{error:?}")))?;
                continue;
            }
            charge_row(
                row,
                if history {
                    &[0, 1, 2, 4, 5, 6, 7, 8, 9, 10, 11]
                } else {
                    &[0, 1, 2, 4, 5, 6, 7]
                },
                if history { 3 } else { 2 },
                &mut budget,
            )?;
            let subject = text(row, 0)?;
            let record = text(row, 2)?;
            let description = subjects.entry(subject.into()).or_default();
            if history {
                let command = CommandKey::new(text(row, 9)?, text(row, 10)?, text(row, 11)?)
                    .map_err(refused)?;
                let receipt = receipt(store, &command, &mut budget)?;
                let registered:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_metadata_schemas WHERE item_id=? AND schema_fingerprint=?)",params![subject,text(row,5)?],|r|r.get(0))?;
                let step = HistoryStep {
                    revision: row.get(3)?,
                    kind: text(row, 4)?.into(),
                    fingerprint: text(row, 5)?.into(),
                    fields_digest: raw_digest(text(row, 6)?),
                    evidence_digest: raw_digest(text(row, 7)?),
                    raw_effect_digest: raw_digest(text(row, 8)?),
                    admitted_at_us: row.get(12)?,
                    command,
                    receipt,
                    schema_registered: registered,
                };
                description
                    .history
                    .insert((record.into(), step.revision), step);
            } else {
                budget.serialized(&(subject, "motion.nfo", record), 4)?;
                let key = serde_json::to_string(&(subject, "motion.nfo", record))?;
                let entity = head(store, "catalog_metadata_sources", &key, &mut budget)?;
                description.sources.insert(
                    record.into(),
                    SourceHead {
                        revision: row.get(3)?,
                        kind: text(row, 4)?.into(),
                        fingerprint: text(row, 5)?.into(),
                        fields_digest: raw_digest(text(row, 6)?),
                        evidence_digest: raw_digest(text(row, 7)?),
                        entity,
                    },
                );
            }
        }
    }
    let (files, tag_merges, global_obligations) =
        tags::capture_in(store, &mut subjects, &mut budget, &shared)?;
    for (subject, description) in &mut subjects {
        budget.serialized(&[subject.as_str()], 4)?;
        let key = serde_json::to_string(&[subject.as_str()])?;
        description.item_head = head(store, "items", &key, &mut budget)?;
        description.item_exists = store.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM items WHERE id=?)",
            [subject.as_str()],
            |r| r.get(0),
        )?;
        description.retirement_head =
            head(store, "catalog_logical_retirements", &key, &mut budget)?;
        let mut query=store.db.prepare("SELECT kind,command_catalog_id,command_host_namespace,command_operation_id FROM catalog_logical_retirements WHERE item_id=?")?;
        let mut rows = query.query([subject.as_str()])?;
        if let Some(row) = rows.next()? {
            charge_row(row, &[0, 1, 2, 3], 0, &mut budget)?;
            description.retirement = Some(Retirement {
                kind: text(row, 0)?.into(),
                command: CommandKey::new(text(row, 1)?, text(row, 2)?, text(row, 3)?)
                    .map_err(refused)?,
            });
        }
        let mut query=store.db.prepare("SELECT h.schema_fingerprint,s.contract_version FROM catalog_metadata_schema_heads h LEFT JOIN catalog_metadata_schemas s ON s.item_id=h.item_id AND s.schema_fingerprint=h.schema_fingerprint WHERE h.item_id=?")?;
        let mut rows = query.query([subject.as_str()])?;
        if let Some(row) = rows.next()? {
            charge_row(row, &[0], 0, &mut budget)?;
            budget.serialized(&(subject.as_str(), text(row, 0)?), 4)?;
            let registry_key = serde_json::to_string(&(subject.as_str(), text(row, 0)?))?;
            description.schema = Some(SchemaHead {
                fingerprint: text(row, 0)?.into(),
                contract: row.get(1)?,
                entity: head(store, "catalog_metadata_schema_heads", &key, &mut budget)?,
                registry_entity: head(
                    store,
                    "catalog_metadata_schemas",
                    &registry_key,
                    &mut budget,
                )?,
            });
        }
        // Admission before classifier work and any obligation-vector allocation.
        budget.charge(BudgetCharge {
            bytes: description
                .history
                .len()
                .saturating_add(description.sources.len())
                .saturating_mul(64)
                .saturating_add(512),
            values: description
                .history
                .len()
                .saturating_add(description.sources.len())
                .saturating_mul(4)
                .saturating_add(16),
            work: description
                .history
                .len()
                .saturating_mul(description.sources.len().saturating_add(4))
                + 16,
            ..Default::default()
        })?;
        budget.charge(BudgetCharge {
            bytes: description
                .tag_history
                .len()
                .saturating_add(description.assertions.len())
                .saturating_mul(256),
            values: description
                .tag_history
                .len()
                .saturating_add(description.assertions.len())
                .saturating_mul(8),
            work: description
                .tag_history
                .len()
                .saturating_add(description.assertions.len())
                .saturating_add(description.tag_merges.len())
                .saturating_mul(8),
            ..Default::default()
        })?;
        description.classify(&catalog_id);
    }
    let mut result = BootstrapInventory {
        catalog_id,
        incarnation,
        generation,
        subjects,
        files,
        tag_merges,
        global_obligations,
        digest: String::new(),
    };
    // Borrow the complete ordered descriptor set. No reconstructed envelope or
    // second owned representation is needed for its commitment.
    result.digest = budget.serialized(&("catabolic.nfo.bootstrap-inventory.v1", &result), 64)?;
    let end = scope(store, &mut budget, control)?;
    if end.0 != result.catalog_id || end.1 != result.incarnation || end.2 != result.generation {
        return Err(refused("scope changed during inventory"));
    }
    Ok(result)
}

#[cfg(test)]
mod tests;

// Owned descriptions expose exact captured facts, never a provenance capability.
impl HeadStamp {
    pub fn revision(&self) -> i64 {
        self.revision
    }
    pub fn deleted(&self) -> bool {
        self.deleted
    }
}
impl SourceHead {
    pub fn kind(&self) -> &str {
        &self.kind
    }
    pub fn fields_digest(&self) -> &str {
        &self.fields_digest
    }
    pub fn evidence_digest(&self) -> &str {
        &self.evidence_digest
    }
    pub fn entity(&self) -> Option<&HeadStamp> {
        self.entity.as_ref()
    }
}
impl SchemaHead {
    pub fn contract(&self) -> Option<i64> {
        self.contract
    }
    pub fn entity(&self) -> Option<&HeadStamp> {
        self.entity.as_ref()
    }
    pub fn registry_entity(&self) -> Option<&HeadStamp> {
        self.registry_entity.as_ref()
    }
}
impl Retirement {
    pub fn kind(&self) -> &str {
        &self.kind
    }
    pub fn command(&self) -> &CommandKey {
        &self.command
    }
}
impl HistoryStep {
    pub fn kind(&self) -> &str {
        &self.kind
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    pub fn fields_digest(&self) -> &str {
        &self.fields_digest
    }
    pub fn evidence_digest(&self) -> &str {
        &self.evidence_digest
    }
    pub fn admitted_at_us(&self) -> i64 {
        self.admitted_at_us
    }
    pub fn schema_registered(&self) -> bool {
        self.schema_registered
    }
}
impl SubjectInventory {
    pub fn item_exists(&self) -> bool {
        self.item_exists
    }
    pub fn item_head(&self) -> Option<&HeadStamp> {
        self.item_head.as_ref()
    }
    pub fn retirement(&self) -> Option<&Retirement> {
        self.retirement.as_ref()
    }
    pub fn retirement_head(&self) -> Option<&HeadStamp> {
        self.retirement_head.as_ref()
    }
    pub fn sources(&self) -> &BTreeMap<String, SourceHead> {
        &self.sources
    }
    pub fn assertions(&self) -> &BTreeMap<String, Assertion> {
        &self.assertions
    }
    pub fn tag_history(&self) -> impl Iterator<Item = (&str, &TagHistory)> {
        self.tag_history
            .iter()
            .map(|((namespace, _), step)| (namespace.as_str(), step))
    }
    pub fn tag_merges(&self) -> &std::collections::BTreeSet<String> {
        &self.tag_merges
    }
}
impl BootstrapInventory {
    /// Nonempty file inventory is an explicit unsupported-file obligation.
    pub fn files(&self) -> &BTreeMap<String, FileInventory> {
        &self.files
    }
    /// Retained merges are unresolved lineage, not recovered current ownership.
    pub fn tag_merges(&self) -> &BTreeMap<String, MergeHistory> {
        &self.tag_merges
    }
}
