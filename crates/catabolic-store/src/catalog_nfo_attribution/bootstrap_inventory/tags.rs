//! Inventory only. Assertion spelling/counters and merge snapshots do not prove
//! imported IDs or source ownership. Suppressions have no source namespace and
//! remain independent manual/local facts; their reason JSON is never authority.
use super::*;
use crate::operation_budget::OperationBudget;
use serde_json::Value;
use std::collections::BTreeSet;
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Assertion {
    id: String,
    tag_id: String,
    active: bool,
    row_digest: String,
    entity: Option<HeadStamp>,
}
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct TagHistory {
    revision: i64,
    before_digest: String,
    after_digest: String,
    command: CommandKey,
    receipt: ReceiptCommitment,
    admitted_at_us: i64,
}
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct MergeHistory {
    source_tag_id: String,
    target_tag_id: String,
    snapshot_digest: String,
    command: CommandKey,
    receipt: ReceiptCommitment,
    subjects: BTreeSet<(String, String)>,
}
#[derive(Debug, PartialEq, Eq, Serialize, Default)]
pub struct FileInventory {
    assertions: BTreeMap<String, Assertion>,
    merges: BTreeSet<String>,
}
// A tombstone does not preserve its old namespace or subject. Never guess
// either from its opaque assertion ID. The map key is only an internal lookup.
struct Tombstone {
    entity_key: String,
    revision: i64,
    reason: &'static str,
    accounted: bool,
}
type Tombstones = BTreeMap<(String, String), Tombstone>;
fn tombstones_in(
    store: &Store,
    budget: &mut ProofBudget,
    shared: &OperationBudget,
) -> Result<(Tombstones, Vec<Obligation>)> {
    let mut found = BTreeMap::new();
    let mut malformed = Vec::new();
    // Full bounded traversal, not a hidden filtered scan. Unrelated permanent
    // heads consume inherited scan work only, not the retained NFO closure cap.
    let mut query = store.db.prepare("SELECT entity_kind,CASE WHEN entity_kind IN ('item_tags','file_tags') AND deleted=1 THEN entity_key END,revision,deleted FROM catalog_entity_revisions")?;
    let mut rows = query.query([])?;
    loop {
        step(shared)?;
        let Some(row) = rows.next()? else { break };
        let kind = text(row, 0)?;
        if !matches!(kind, "item_tags" | "file_tags") || !row.get::<_, bool>(3)? {
            unrelated(row, &[0], shared)?;
            continue;
        }
        charge_row(row, &[0, 1], 0, budget)?;
        let key = text(row, 1)?;
        let revision = row.get(2)?;
        budget.raw_json(key, 4)?;
        let parsed: Value = serde_json::from_str(key)?;
        let identifier = parsed
            .as_array()
            .filter(|a| a.len() == 1)
            .and_then(|a| a[0].as_str());
        let Some(identifier) = identifier else {
            budget.charge(BudgetCharge {
                bytes: 64,
                values: 1,
                work: 1,
                ..Default::default()
            })?;
            malformed.push(Obligation::UnresolvedAssertionTombstone {
                entity_kind: kind.into(),
                entity_key: key.into(),
                revision,
                reason: "malformed permanent assertion key".into(),
            });
            continue;
        };
        budget.serialized(&(kind, identifier), 4)?;
        let live: bool = store.db.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {kind} WHERE id=?)"),
            [identifier],
            |r| r.get(0),
        )?;
        let reason = if live {
            "deleted assertion head has a live row"
        } else if revision != 2 {
            "prior assertion counter is not independently reconstructed"
        } else {
            "no authenticated exact merge collision accounts for deletion"
        };
        let lookup = (kind.into(), identifier.into());
        if found.contains_key(&lookup) {
            budget.charge(BudgetCharge {
                bytes: 64,
                values: 1,
                work: 1,
                ..Default::default()
            })?;
            malformed.push(Obligation::UnresolvedAssertionTombstone {
                entity_kind: lookup.0,
                entity_key: key.into(),
                revision,
                reason: "multiple permanent keys for one assertion identity".into(),
            });
        } else {
            found.insert(
                lookup,
                Tombstone {
                    entity_key: key.into(),
                    revision,
                    reason,
                    accounted: false,
                },
            );
        }
    }
    Ok((found, malformed))
}
fn finish_tombstones(
    found: Tombstones,
    mut obligations: Vec<Obligation>,
    budget: &mut ProofBudget,
) -> Result<Vec<Obligation>> {
    for ((kind, _), tombstone) in found {
        if tombstone.accounted {
            continue;
        }
        budget.charge(BudgetCharge {
            bytes: tombstone.reason.len(),
            values: 1,
            work: 1,
            ..Default::default()
        })?;
        obligations.push(Obligation::UnresolvedAssertionTombstone {
            entity_kind: kind,
            entity_key: tombstone.entity_key,
            revision: tombstone.revision,
            reason: tombstone.reason.into(),
        });
    }
    // Malformed and well-formed keys share one deterministic catalog-level order.
    let comparisons = obligations.len().saturating_mul(
        usize::BITS as usize - obligations.len().max(1).leading_zeros() as usize + 1,
    );
    budget.charge(BudgetCharge {
        work: comparisons,
        ..Default::default()
    })?;
    obligations.sort_by(|a, b| match (a, b) {
        (
            Obligation::UnresolvedAssertionTombstone {
                entity_kind: ak,
                entity_key: ae,
                ..
            },
            Obligation::UnresolvedAssertionTombstone {
                entity_kind: bk,
                entity_key: be,
                ..
            },
        ) => (ak, ae).cmp(&(bk, be)),
        _ => std::cmp::Ordering::Equal,
    });
    Ok(obligations)
}
#[derive(Serialize)]
struct MergePayload<'a> {
    operation: &'static str,
    mutation: MergeMutation<'a>,
}
#[derive(Serialize)]
struct MergeMutation<'a> {
    operation: &'static str,
    contract_version: u32,
    merge: &'a Value,
}
#[derive(Serialize)]
struct MergeEnvelope<'a> {
    contract_version: u32,
    canonicalization_version: u32,
    key: &'a CommandKey,
    admission_incarnation: &'a str,
    expected_generation: i64,
    payload: MergePayload<'a>,
}
/// Descriptive authentication only: no reconstructed envelope is executed or
/// exposed as a grant. Both encoders below are the original owning algorithms.
pub(super) fn authenticate_merge(
    store: &Store,
    row: &Row<'_>,
    snapshot: &Value,
    budget: &mut ProofBudget,
) -> Result<bool> {
    use catabolic_core::catalog_command::{
        COMMAND_CANONICALIZATION_VERSION, COMMAND_CONTRACT_VERSION,
    };
    let Some(body) = snapshot.pointer("/preview/body") else {
        return Ok(false);
    };
    let Some(request) = snapshot.get("request") else {
        return Ok(false);
    };
    let Some(preview_digest) = snapshot.pointer("/preview/digest").and_then(Value::as_str) else {
        return Ok(false);
    };
    if snapshot.get("contract_version").and_then(Value::as_u64) != Some(1)
        || body.get("contract_version").and_then(Value::as_u64) != Some(1)
        || body.get("canonicalization_version").and_then(Value::as_u64) != Some(1)
        || request.get("preview_digest").and_then(Value::as_str) != Some(preview_digest)
        || request.get("source_tag_id").and_then(Value::as_str) != Some(text(row, 0)?)
        || request.get("target_tag_id").and_then(Value::as_str) != Some(text(row, 1)?)
        || body
            .pointer("/affected/source_tag_id")
            .and_then(Value::as_str)
            != Some(text(row, 0)?)
        || body
            .pointer("/affected/target_tag_id")
            .and_then(Value::as_str)
            != Some(text(row, 1)?)
        || request.get("expected_source_revision") != body.pointer("/affected/source_revision")
        || request.get("expected_target_revision") != body.pointer("/affected/target_revision")
    {
        return Ok(false);
    }
    // Actual merge_history_json uses this exact compact serde wrapper. Equality
    // also rejects duplicate-key or non-writer raw encodings before accounting.
    budget.serialized(snapshot, 64)?;
    if serde_json::to_string(snapshot)? != text(row, 2)? {
        return Ok(false);
    }
    budget.serialized(body, 64)?;
    if crate::catalog_tag_mutation::canonical_preview_value_digest(body)
        .map_err(|e| refused(e.to_string()))?
        != preview_digest
    {
        return Ok(false);
    }
    charge_row(row, &[3, 4, 5], 0, budget)?;
    let key = CommandKey::new(text(row, 3)?, text(row, 4)?, text(row, 5)?).map_err(refused)?;
    let commitment = receipt(store, &key, budget)?;
    let ReceiptCommitment::Retained {
        payload_digest,
        admission_incarnation,
        contract_version,
        expected_generation,
        committed_generation,
    } = commitment
    else {
        return Ok(false);
    };
    let current_generation: i64 =
        store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })?;
    if key.catalog_id() != store.database_id
        || contract_version != i64::from(COMMAND_CONTRACT_VERSION)
        || body.pointer("/catalog/catalog_id").and_then(Value::as_str) != Some(key.catalog_id())
        || body.pointer("/catalog/incarnation").and_then(Value::as_str)
            != Some(admission_incarnation.as_str())
        || body.pointer("/catalog/generation").and_then(Value::as_i64) != Some(expected_generation)
        || body.get("host_namespace").and_then(Value::as_str) != Some(key.host_namespace())
        || committed_generation < expected_generation
        || committed_generation > current_generation
    {
        return Ok(false);
    }
    let view = MergeEnvelope {
        contract_version: COMMAND_CONTRACT_VERSION,
        canonicalization_version: COMMAND_CANONICALIZATION_VERSION,
        key: &key,
        admission_incarnation: &admission_incarnation,
        expected_generation,
        payload: MergePayload {
            operation: "tags",
            mutation: MergeMutation {
                operation: "merge",
                contract_version: 1,
                merge: request,
            },
        },
    };
    budget.serialized(&view, 64)?;
    let envelope = serde_json::to_value(&view)?;
    budget.canonical_serialized(&envelope, 64)?;
    let canonical = catabolic_core::encode_with(&envelope, false, true);
    Ok(catabolic_core::sha256(canonical.as_bytes()) == payload_digest.as_str())
}
fn account_merge_tombstones(
    store: &Store,
    row: &Row<'_>,
    snapshot: &Value,
    tombstones: &mut Tombstones,
    budget: &mut ProofBudget,
) -> Result<()> {
    if tombstones.is_empty() {
        return Ok(());
    }
    let source = snapshot
        .pointer("/preview/body/affected/source_assertions")
        .and_then(Value::as_array)
        .ok_or_else(|| refused("missing source assertions"))?;
    let target = snapshot
        .pointer("/preview/body/affected/target_assertions")
        .and_then(Value::as_array)
        .ok_or_else(|| refused("missing target assertions"))?;
    let mut authenticated = None;
    for assertion in source {
        // Charge lookup/traversal work and temporary keys before allocation.
        budget.charge(BudgetCharge {
            work: 1,
            ..Default::default()
        })?;
        let Some(kind) = assertion.pointer("/subject/kind").and_then(Value::as_str) else {
            continue;
        };
        let Some(id) = assertion.get("id").and_then(Value::as_str) else {
            continue;
        };
        if !matches!(kind, "item" | "file") {
            continue;
        }
        let entity_kind = if kind == "item" {
            "item_tags"
        } else {
            "file_tags"
        };
        budget.serialized(&(entity_kind, id), 4)?;
        let key = (entity_kind.to_owned(), id.to_owned());
        let Some(tombstone) = tombstones.get_mut(&key) else {
            continue;
        };
        if tombstone.revision != 2 || tombstone.reason == "deleted assertion head has a live row" {
            continue;
        }
        // Under continuously preserved admitted settings / the trusted closed
        // owner contract, DELETE/REPLACE of a permanent head is forbidden and
        // UPDATE must advance by one. This is the same retained-head assumption
        // used by existing owners, not tamper resistance against arbitrary raw
        // SQLite that disables safety settings or replaces tracking triggers.
        // Thus revision2 is the narrow insert1/delete2 cohort. Higher counters
        // do not contain enough historical information and stay obligations.
        budget.charge(BudgetCharge {
            work: target.len() + 1,
            ..Default::default()
        })?;
        let mut collisions = target.iter().filter(|other| {
            other.get("subject") == assertion.get("subject")
                && other.get("source") == assertion.get("source")
        });
        let Some(other) = collisions.next() else {
            continue;
        };
        if collisions.next().is_some() {
            continue;
        }
        if assertion.get("tag_id").and_then(Value::as_str) != Some(text(row, 0)?)
            || other.get("tag_id").and_then(Value::as_str) != Some(text(row, 1)?)
            || other
                .get("id")
                .and_then(Value::as_str)
                .is_none_or(|other_id| other_id == id)
            || assertion
                .pointer("/subject/id")
                .and_then(Value::as_str)
                .is_none()
            || assertion.get("source").and_then(Value::as_str).is_none()
        {
            continue;
        }
        let valid = match authenticated {
            Some(valid) => valid,
            None => {
                let valid = authenticate_merge(store, row, snapshot, budget)?;
                authenticated = Some(valid);
                valid
            }
        };
        if valid {
            tombstone.accounted = true;
        } else {
            tombstone.reason = "merge snapshot/request/receipt commitment mismatch";
        }
    }
    Ok(())
}
fn step(shared: &OperationBudget) -> Result<()> {
    shared
        .charge(BudgetCharge {
            work: 1,
            ..Default::default()
        })
        .map_err(|e| refused(format!("{e:?}")))
}
fn unrelated(row: &Row<'_>, columns: &[usize], shared: &OperationBudget) -> Result<()> {
    let mut bytes = 0usize;
    for index in columns {
        bytes = bytes
            .checked_add(text(row, *index)?.len())
            .ok_or_else(|| refused("tag scan bytes"))?;
    }
    shared
        .charge(BudgetCharge {
            items: 1,
            bytes,
            values: columns.len(),
            work: bytes / 64 + columns.len() + 1,
        })
        .map_err(|e| refused(format!("{e:?}")))
}
pub(super) fn capture_in(
    store: &Store,
    subjects: &mut BTreeMap<String, SubjectInventory>,
    budget: &mut ProofBudget,
    shared: &OperationBudget,
) -> Result<(
    BTreeMap<String, FileInventory>,
    BTreeMap<String, MergeHistory>,
    Vec<Obligation>,
)> {
    let (mut tombstones, malformed_tombstones) = tombstones_in(store, budget, shared)?;
    let mut files = BTreeMap::<String, FileInventory>::new();
    for kind in ["item", "file"] {
        let mut query=store.db.prepare(&format!("SELECT {kind}_id,source,CASE WHEN source='motion.nfo' THEN id END,CASE WHEN source='motion.nfo' THEN tag_id END,active,confidence,CASE WHEN source='motion.nfo' THEN note END,CASE WHEN source='motion.nfo' THEN created_at END,CASE WHEN source='motion.nfo' THEN updated_at END FROM {kind}_tags"))?;
        let mut rows = query.query([])?;
        loop {
            step(shared)?;
            let Some(row) = rows.next()? else { break };
            if text(row, 1)? != "motion.nfo" {
                unrelated(row, &[0, 1], shared)?;
                continue;
            }
            charge_row(row, &[0, 1, 2, 3, 6, 7, 8], 1, budget)?;
            let subject = text(row, 0)?;
            let id = text(row, 2)?;
            let active: bool = row.get(4)?;
            let confidence: Option<f64> = row.get(5)?;
            let digest = budget.serialized(
                &(
                    kind,
                    subject,
                    id,
                    text(row, 3)?,
                    "motion.nfo",
                    active,
                    confidence,
                    text(row, 6)?,
                    text(row, 7)?,
                    text(row, 8)?,
                ),
                4,
            )?;
            budget.serialized(&[id], 4)?;
            let key = serde_json::to_string(&[id])?;
            let assertion = Assertion {
                id: id.into(),
                tag_id: text(row, 3)?.into(),
                active,
                row_digest: digest,
                entity: head(store, &format!("{kind}_tags"), &key, budget)?,
            };
            // Reserve the second ID copy used as the ordered map key.
            budget.charge(BudgetCharge {
                bytes: id.len(),
                values: 1,
                work: 1,
                ..Default::default()
            })?;
            if kind == "item" {
                subjects
                    .entry(subject.into())
                    .or_default()
                    .assertions
                    .insert(id.into(), assertion);
            } else {
                files
                    .entry(subject.into())
                    .or_default()
                    .assertions
                    .insert(id.into(), assertion);
            }
        }
    }
    let mut query=store.db.prepare("SELECT item_id,namespace,record_id,revision,CASE WHEN namespace='motion.nfo' THEN before_json END,CASE WHEN namespace='motion.nfo' THEN after_json END,CASE WHEN namespace='motion.nfo' THEN command_catalog_id END,CASE WHEN namespace='motion.nfo' THEN command_host_namespace END,CASE WHEN namespace='motion.nfo' THEN command_operation_id END,admitted_at_us FROM catalog_metadata_tag_history")?;
    let mut rows = query.query([])?;
    loop {
        step(shared)?;
        let Some(row) = rows.next()? else { break };
        if text(row, 1)? != "motion.nfo" {
            unrelated(row, &[0, 1, 2], shared)?;
            continue;
        }
        charge_row(row, &[0, 1, 2, 4, 5, 6, 7, 8], 2, budget)?;
        let command =
            CommandKey::new(text(row, 6)?, text(row, 7)?, text(row, 8)?).map_err(refused)?;
        let history = TagHistory {
            revision: row.get(3)?,
            before_digest: raw_digest(text(row, 4)?),
            after_digest: raw_digest(text(row, 5)?),
            receipt: receipt(store, &command, budget)?,
            command,
            admitted_at_us: row.get(9)?,
        };
        subjects
            .entry(text(row, 0)?.into())
            .or_default()
            .tag_history
            .insert((text(row, 2)?.into(), history.revision), history);
    }
    let mut merges = BTreeMap::new();
    let mut query=store.db.prepare("SELECT source_tag_id,original_target_tag_id,snapshot_json,command_catalog_id,command_host_namespace,command_operation_id FROM catalog_tag_merges")?;
    let mut rows = query.query([])?;
    loop {
        step(shared)?;
        let Some(row) = rows.next()? else { break };
        unrelated(row, &[0, 1, 3, 4, 5], shared)?;
        let raw = text(row, 2)?;
        // The writer stores {contract_version,request,preview:{body:{affected}}}.
        // Bound and charge the entire borrowed input before Value allocation.
        // Scratch has the existing byte/node/depth ceiling and shares cumulative
        // operation admission, but unrelated merges consume no retained-NFO quota.
        let mut scratch = ProofBudget::new(shared.clone());
        scratch.raw_json(raw, 64)?;
        let snapshot: Value = serde_json::from_str(raw)?;
        if snapshot.get("contract_version").and_then(Value::as_u64) != Some(1) {
            return Err(refused("unsupported tag merge history contract"));
        }
        let affected = snapshot
            .pointer("/preview/body/affected")
            .and_then(Value::as_object)
            .ok_or_else(|| refused("unsupported tag merge history shape"))?;
        let mut relevant = BTreeSet::new();
        for side in ["source_assertions", "target_assertions"] {
            let assertions = affected
                .get(side)
                .and_then(Value::as_array)
                .ok_or_else(|| refused("incomplete tag merge assertion inventory"))?;
            for assertion in assertions {
                step(shared)?;
                let source = assertion
                    .get("source")
                    .and_then(Value::as_str)
                    .ok_or_else(|| refused("unknown tag merge assertion source"))?;
                if source != "motion.nfo" {
                    continue;
                }
                let kind = assertion
                    .pointer("/subject/kind")
                    .and_then(Value::as_str)
                    .ok_or_else(|| refused("unknown tag merge subject kind"))?;
                let subject = assertion
                    .pointer("/subject/id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| refused("unknown tag merge subject identity"))?;
                if !matches!(kind, "item" | "file") {
                    return Err(refused("unsupported tag merge subject kind"));
                }
                budget.serialized(&(kind, subject, text(row, 0)?), 4)?;
                budget.charge(BudgetCharge {
                    bytes: subject.len() + kind.len() + text(row, 0)?.len(),
                    values: 3,
                    work: 3,
                    ..Default::default()
                })?;
                relevant.insert((kind.to_owned(), subject.to_owned()));
                if kind == "item" {
                    subjects
                        .entry(subject.into())
                        .or_default()
                        .tag_merges
                        .insert(text(row, 0)?.into());
                } else {
                    files
                        .entry(subject.into())
                        .or_default()
                        .merges
                        .insert(text(row, 0)?.into());
                }
            }
        }
        account_merge_tombstones(store, row, &snapshot, &mut tombstones, &mut scratch)?;
        if relevant.is_empty() {
            continue;
        }
        charge_row(row, &[0, 1, 2, 3, 4, 5], 1, budget)?;
        let command =
            CommandKey::new(text(row, 3)?, text(row, 4)?, text(row, 5)?).map_err(refused)?;
        budget.charge(BudgetCharge {
            bytes: text(row, 0)?.len(),
            values: 1,
            work: 1,
            ..Default::default()
        })?;
        merges.insert(
            text(row, 0)?.into(),
            MergeHistory {
                source_tag_id: text(row, 0)?.into(),
                target_tag_id: text(row, 1)?.into(),
                snapshot_digest: raw_digest(raw),
                receipt: receipt(store, &command, budget)?,
                command,
                subjects: relevant,
            },
        );
    }
    Ok((
        files,
        merges,
        finish_tombstones(tombstones, malformed_tombstones, budget)?,
    ))
}
pub(super) fn serialize_history<S: serde::Serializer>(
    history: &BTreeMap<(String, i64), TagHistory>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.collect_seq(history.iter().map(|((record, _), step)| (record, step)))
}
pub(super) fn classify(subject: &SubjectInventory, issues: &mut Vec<Obligation>, catalog: &str) {
    if !subject.tag_merges.is_empty() {
        issues.push(Obligation::UnsupportedTagMerge);
    }
    if subject.assertions.values().any(|a| !active(&a.entity)) {
        issues.push(Obligation::MissingAssertionHead);
    }
    for ((record, revision), history) in &subject.tag_history {
        if record != "document" {
            issues.push(Obligation::UnsupportedRecord);
        }
        if matches!(history.receipt, ReceiptCommitment::Missing) {
            issues.push(Obligation::MissingReceipt);
        }
        if history.command.catalog_id() != catalog {
            issues.push(Obligation::ForeignCommandCatalog);
        }
        if !subject
            .history
            .get(&(record.clone(), *revision))
            .is_some_and(|step| {
                step.command == history.command && step.admitted_at_us == history.admitted_at_us
            })
        {
            issues.push(Obligation::UnlinkedTagHistory);
        }
    }
}

impl Assertion {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn tag_id(&self) -> &str {
        &self.tag_id
    }
    pub fn active(&self) -> bool {
        self.active
    }
    pub fn row_digest(&self) -> &str {
        &self.row_digest
    }
    pub fn entity(&self) -> Option<&HeadStamp> {
        self.entity.as_ref()
    }
}
impl TagHistory {
    pub fn revision(&self) -> i64 {
        self.revision
    }
    pub fn before_digest(&self) -> &str {
        &self.before_digest
    }
    pub fn after_digest(&self) -> &str {
        &self.after_digest
    }
    pub fn command(&self) -> &CommandKey {
        &self.command
    }
    pub fn receipt(&self) -> &ReceiptCommitment {
        &self.receipt
    }
    pub fn admitted_at_us(&self) -> i64 {
        self.admitted_at_us
    }
}
impl MergeHistory {
    pub fn source_tag_id(&self) -> &str {
        &self.source_tag_id
    }
    pub fn target_tag_id(&self) -> &str {
        &self.target_tag_id
    }
    pub fn snapshot_digest(&self) -> &str {
        &self.snapshot_digest
    }
    pub fn command(&self) -> &CommandKey {
        &self.command
    }
    pub fn receipt(&self) -> &ReceiptCommitment {
        &self.receipt
    }
    pub fn subjects(&self) -> &BTreeSet<(String, String)> {
        &self.subjects
    }
}
impl FileInventory {
    pub fn assertions(&self) -> &BTreeMap<String, Assertion> {
        &self.assertions
    }
    pub fn merges(&self) -> &BTreeSet<String> {
        &self.merges
    }
}
