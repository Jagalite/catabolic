//! Reconstruct one complete NFO source history without treating its receipts as
//! mutation authority. Unknown operations and incomplete proof sets fail closed.
use super::{VerifiedNfoBaseline, verify_committed_command_in};
use crate::{
    Error, Result, Store,
    catalog_commands::TrustedCommandAuthority,
    catalog_metadata_store::{
        ContributionReplace, MAX_RAW_MUTATION_BYTES, current_schema_fingerprint,
        read_registered_schema,
    },
    operation_budget::{BudgetCharge, OperationBudget},
};
use catabolic_core::{
    catalog_command::{CommandEnvelope, CommandKey},
    catalog_metadata::{
        ContributionDocument, ContributionKey, ContributionReplacement, FieldPolicy, FieldSchema,
        MAX_METADATA_DOCUMENT_BYTES, MAX_METADATA_EVIDENCE_BYTES, MAX_METADATA_JSON_NODES,
        MAX_METADATA_SNAPSHOT_BYTES, Normalization, SourceKind, ValueType, nfo_refresh::FileOrigin,
        plan_replace, plan_schema_extension, replacement_fields, revalidate_document,
        validate_schema_extension,
    },
    encode,
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const NAMESPACE: &str = "motion.nfo";
const RECORD: &str = "document";

/// Descriptive verified state, never a source mutation or attribution grant.
/// Its observed generation is useful only while the owner retains/rechecks the
/// corresponding writer. Identity and origin are inherited, never inferred from
/// metadata fields, source spelling, or a caller's serialized evidence.
#[derive(Debug)]
pub(crate) struct VerifiedNfoSource {
    catalog_id: String,
    incarnation: String,
    subject: String,
    current_source: ContributionDocument,
    current_schema: FieldSchema,
    observed_generation: i64,
    automatic_identity: Option<String>,
    origin: Option<FileOrigin>,
    attribution_audit_digest: String,
    origin_proof_digest: Option<String>,
    origin_physical: Option<crate::catalog_import_typed::PhysicalMapping>,
    proof_digest: String,
    imported_tag_admission: Option<super::import_provenance::ImportedTagAdmission>,
}
impl VerifiedNfoSource {
    pub(super) fn catalog_id(&self) -> &str {
        &self.catalog_id
    }
    pub(super) fn incarnation(&self) -> &str {
        &self.incarnation
    }
    pub(super) fn subject(&self) -> &str {
        &self.subject
    }
    pub(super) fn current_source(&self) -> &ContributionDocument {
        &self.current_source
    }
    pub(super) fn current_schema(&self) -> &FieldSchema {
        &self.current_schema
    }
    pub(super) fn observed_generation(&self) -> i64 {
        self.observed_generation
    }
    pub(super) fn automatic_identity(&self) -> Option<&str> {
        self.automatic_identity.as_deref()
    }
    pub(super) fn origin(&self) -> Option<&FileOrigin> {
        self.origin.as_ref()
    }
    pub(super) fn attribution_audit_digest(&self) -> &str {
        &self.attribution_audit_digest
    }
    pub(super) fn origin_proof_digest(&self) -> Option<&str> {
        self.origin_proof_digest.as_deref()
    }
    pub(super) fn origin_physical(&self) -> Option<&crate::catalog_import_typed::PhysicalMapping> {
        self.origin_physical.as_ref()
    }
    pub(super) fn imported_tag_admission(
        &self,
    ) -> Option<&super::import_provenance::ImportedTagAdmission> {
        self.imported_tag_admission.as_ref()
    }
    pub(super) fn proof_digest(&self) -> &str {
        &self.proof_digest
    }
}

fn refused(reason: impl std::fmt::Display) -> Error {
    Error(format!("catalog_nfo_history_refused: {reason}"))
}
fn checked(value: i64) -> Result<usize> {
    usize::try_from(value).map_err(|_| refused("negative history resource count"))
}

/// Row/node/byte/work admission is cumulative across history, retained schemas,
/// supplied envelopes and decoded values. All charges precede text allocation
/// or semantic traversal; no prefix is returned when the closure is too large.
struct Budget {
    shared: OperationBudget,
    bytes: usize,
    values: usize,
    work: usize,
}
impl Budget {
    fn charge(&mut self, bytes: usize, values: usize, work: usize) -> Result<()> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or_else(|| refused("byte budget"))?;
        self.values = self
            .values
            .checked_add(values)
            .ok_or_else(|| refused("node budget"))?;
        self.work = self
            .work
            .checked_add(work)
            .ok_or_else(|| refused("work budget"))?;
        if self.bytes > MAX_METADATA_SNAPSHOT_BYTES
            || self.values > MAX_METADATA_JSON_NODES
            || self.work > MAX_METADATA_JSON_NODES
        {
            return Err(refused("history closure budget exceeded"));
        }
        self.shared
            .charge(BudgetCharge {
                bytes,
                values,
                work,
                ..Default::default()
            })
            .map_err(|_| refused("native operation budget exceeded"))
    }
    fn value(&mut self, value: &Value) -> Result<()> {
        let mut pending = vec![value];
        while let Some(value) = pending.pop() {
            self.charge(0, 1, 1)?;
            let children = match value {
                Value::Array(values) => values.len(),
                Value::Object(values) => values.len(),
                _ => 0,
            };
            if pending
                .len()
                .saturating_add(children)
                .saturating_add(self.values)
                > MAX_METADATA_JSON_NODES
            {
                return Err(refused("history node budget exceeded"));
            }
            match value {
                Value::Array(values) => pending.extend(values),
                Value::Object(values) => pending.extend(values.values()),
                _ => {}
            }
        }
        Ok(())
    }
    fn document(&mut self, document: &ContributionDocument) -> Result<()> {
        self.charge(0, 1, 1)?;
        for value in document.fields().values() {
            self.value(value)?;
        }
        self.value(document.evidence())
    }
}

#[derive(Debug)]
struct HistoryRow {
    revision: i64,
    kind: String,
    fingerprint: String,
    fields: String,
    evidence: String,
    raw: String,
    command: CommandKey,
}
impl HistoryRow {
    fn document(
        &self,
        subject: &str,
        kind: SourceKind,
        schema: &FieldSchema,
    ) -> Result<ContributionDocument> {
        if self.kind != kind_name(kind) || self.fingerprint != schema.fingerprint() {
            return Err(refused("history source kind/schema mismatch"));
        }
        let document = ContributionDocument::from_json_with_system_fields(
            schema,
            ContributionKey::new(subject, NAMESPACE, RECORD).map_err(refused)?,
            kind,
            self.revision,
            &self.fields,
            &self.evidence,
        )
        .map_err(refused)?;
        // History stores already-normalized values. Parsing must not silently
        // repair a contradictory field or noncanonical retained evidence row.
        if serde_json::to_string(document.fields())? != self.fields
            || encode(document.evidence()) != self.evidence
        {
            return Err(refused(
                "history fields/evidence differ from exact owner encoding",
            ));
        }
        Ok(document)
    }
}
fn kind_name(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Local => "local",
        SourceKind::Provider => "provider",
        SourceKind::Scan => "scan",
        SourceKind::LegacyImport => "legacy_import",
    }
}

// Length-prefix every component; stream retained buffers without cloning or
// escaping whole history rows into an additional JSON proof representation.
fn proof_hash(parts: &[&[u8]]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    format!("{:x}", hash.finalize())
}
fn validate_baseline_attribution(baseline: &VerifiedNfoBaseline) -> Result<()> {
    use catabolic_core::catalog_metadata::nfo_refresh::{MAX_IDENTITY_BYTES, OriginProvenance};
    let identifier = |value: &str, maximum: usize| {
        if value.is_empty()
            || value.len() > maximum
            || value.trim() != value
            || value.chars().any(char::is_control)
        {
            Err(refused("invalid baseline attribution identifier"))
        } else {
            Ok(())
        }
    };
    let digest = |value: &str| {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            Err(refused("invalid baseline attribution digest"))
        } else {
            Ok(())
        }
    };
    digest(&baseline.attribution_audit_digest)?;
    if let Some(identity) = &baseline.automatic_identity {
        // External IDs are retained opaque source values, not native identifiers.
        // Historical metadata PUT permits whitespace and controls within its bound.
        if identity.is_empty() || identity.len() > MAX_IDENTITY_BYTES {
            return Err(refused("invalid baseline external identity bounds"));
        }
    }
    if baseline.origin.is_none()
        && (baseline.origin_physical.is_some() || baseline.origin_proof_digest.is_some())
    {
        return Err(refused("origin proof exists without an origin"));
    }
    if let Some(origin) = &baseline.origin {
        identifier(&origin.file.profile, 256)?;
        identifier(&origin.file.file_id, 256)?;
        match &origin.provenance {
            OriginProvenance::Observed {
                exact_revision,
                binding_fingerprint,
                ownership_fingerprint,
            } => {
                if baseline.origin_physical.is_some() || baseline.origin_proof_digest.is_some() {
                    return Err(refused("observed origin contains imported part proof"));
                }
                digest(exact_revision)?;
                digest(binding_fingerprint)?;
                digest(ownership_fingerprint)?;
            }
            OriginProvenance::Imported {
                import_id,
                source_occurrence_id,
                mapping_digest,
            } => {
                identifier(import_id, 256)?;
                identifier(source_occurrence_id, 256)?;
                digest(mapping_digest)?;
                let physical = baseline
                    .origin_physical
                    .as_ref()
                    .ok_or_else(|| refused("imported origin lost exact physical revision"))?;
                if physical.profile != origin.file.profile
                    || physical.exact.file_id != origin.file.file_id
                    || &physical.source_occurrence_id != source_occurrence_id
                    || physical.exact.validate().is_err()
                {
                    return Err(refused(
                        "imported origin differs from exact physical revision",
                    ));
                }
                digest(
                    baseline.origin_proof_digest.as_deref().ok_or_else(|| {
                        refused("imported origin lacks exact physical-part proof")
                    })?,
                )?;
            }
        }
    }
    Ok(())
}

struct Registered {
    schema: FieldSchema,
    prior: Option<String>,
    command: CommandKey,
}

/// `commands` contains exactly revisions 2..=head in source-revision order.
/// The separately verified baseline retains its exact outer activation/create
/// envelope, which need not equal its synthetic child's raw mutation bytes.
pub(crate) fn verify_source_history_in(
    store: &Store,
    baseline: &VerifiedNfoBaseline,
    commands: &[CommandEnvelope],
    authority: TrustedCommandAuthority<'_>,
    expected_generation: i64,
) -> Result<VerifiedNfoSource> {
    // This also establishes native40 retained-writer, current host, catalog and
    // incarnation admission, without changing receipt replay/expiry behavior.
    let baseline_commit =
        verify_committed_command_in(store, &baseline.command, authority, expected_generation)?;
    if baseline.revision != 1 || baseline.source_kind == SourceKind::LegacyImport {
        return Err(refused("baseline is not NFO revision one"));
    }
    ContributionKey::new(&baseline.subject, NAMESPACE, RECORD).map_err(refused)?;
    let mut budget = Budget {
        shared: store.catalog_operation_budget()?,
        bytes: 0,
        values: 0,
        work: 0,
    };
    if commands.len() >= MAX_METADATA_JSON_NODES {
        return Err(refused("history command count exceeded"));
    }
    for command in std::iter::once(&baseline.command).chain(commands) {
        budget.charge(
            command.canonical_payload().len(),
            1,
            command.canonical_payload().len() / 64 + 1,
        )?;
    }
    let db = &store.db;
    let (count, first, last, bytes, fields_max, evidence_max, raw_max): (i64, i64, i64, i64, i64, i64, i64) = db.query_row(
        "SELECT count(*),coalesce(min(revision),0),coalesce(max(revision),0),coalesce(sum(length(CAST(source_kind AS BLOB))+length(CAST(schema_fingerprint AS BLOB))+length(CAST(fields_json AS BLOB))+length(CAST(evidence_json AS BLOB))+length(CAST(raw_mutation_json AS BLOB))+length(CAST(command_catalog_id AS BLOB))+length(CAST(command_host_namespace AS BLOB))+length(CAST(command_operation_id AS BLOB))),0),coalesce(max(length(CAST(fields_json AS BLOB))),0),coalesce(max(length(CAST(evidence_json AS BLOB))),0),coalesce(max(length(CAST(raw_mutation_json AS BLOB))),0) FROM (SELECT * FROM catalog_metadata_history WHERE item_id=?1 AND namespace=?2 AND record_id=?3 ORDER BY revision LIMIT ?4)",
        params![baseline.subject,NAMESPACE,RECORD,(MAX_METADATA_JSON_NODES + 1) as i64], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)),
    )?;
    if first != 1
        || count != last
        || checked(count)? != commands.len() + 1
        || fields_max > MAX_METADATA_DOCUMENT_BYTES as i64
        || evidence_max > MAX_METADATA_EVIDENCE_BYTES as i64
        || raw_max > MAX_RAW_MUTATION_BYTES as i64
    {
        return Err(refused(
            "missing, noncontiguous or oversized NFO history/proofs",
        ));
    }
    budget.charge(
        checked(bytes)?,
        checked(count)?.saturating_mul(9),
        checked(bytes)? / 64 + checked(count)?,
    )?;
    let head: Option<(i64, i64)> = db.query_row(
        "SELECT revision,length(CAST(source_kind AS BLOB))+length(CAST(schema_fingerprint AS BLOB))+length(CAST(fields_json AS BLOB))+length(CAST(evidence_json AS BLOB)) FROM catalog_metadata_sources WHERE item_id=? AND namespace=? AND record_id=?",
        params![baseline.subject,NAMESPACE,RECORD], |r|Ok((r.get(0)?,r.get(1)?)),
    ).optional()?;
    let Some((head_revision, head_bytes)) = head else {
        return Err(refused("current NFO source absent"));
    };
    if head_revision != last {
        return Err(refused("current source does not end complete history"));
    }
    budget.charge(checked(head_bytes)?, 6, checked(head_bytes)? / 64 + 1)?;
    let registry = read_registry(store, &baseline.subject, &mut budget)?;
    let current_fingerprint = current_schema_fingerprint(db, &baseline.subject)
        .map_err(refused)?
        .ok_or_else(|| refused("current metadata schema absent"))?;
    let lineage = lineage(&registry, &current_fingerprint, &mut budget)?;
    if lineage.len() != registry.len() {
        return Err(refused("registered schema branch outside current lineage"));
    }
    let current_schema = &registry[&current_fingerprint].schema;
    let mut query = db.prepare("SELECT revision,source_kind,schema_fingerprint,fields_json,evidence_json,raw_mutation_json,command_catalog_id,command_host_namespace,command_operation_id FROM catalog_metadata_history WHERE item_id=? AND namespace=? AND record_id=? ORDER BY revision")?;
    let mut rows = query.query(params![baseline.subject, NAMESPACE, RECORD])?;
    let mut previous: Option<ContributionDocument> = None;
    let mut prior_committed_generation = baseline_commit.committed_generation();
    validate_baseline_attribution(baseline)?;
    let origin = serde_json::to_string(&baseline.origin)?;
    let identity = serde_json::to_string(&baseline.automatic_identity)?;
    let origin_physical = serde_json::to_string(&baseline.origin_physical)?;
    budget.charge(
        origin.len() + identity.len() + origin_physical.len(),
        12,
        (origin.len() + identity.len() + origin_physical.len()) / 64 + 12,
    )?;
    let mut proof_digest = proof_hash(&[
        b"catabolic.nfo.source-history.v1",
        store.database_id.as_bytes(),
        store
            .catalog_incarnation
            .as_deref()
            .expect("verified native incarnation")
            .as_bytes(),
        &expected_generation.to_le_bytes(),
        baseline.subject.as_bytes(),
        baseline.attribution_audit_digest.as_bytes(),
        identity.as_bytes(),
        origin.as_bytes(),
        origin_physical.as_bytes(),
        baseline
            .origin_proof_digest
            .as_deref()
            .unwrap_or("")
            .as_bytes(),
        baseline.command.digest().as_str().as_bytes(),
        current_fingerprint.as_bytes(),
    ]);
    if let Some(names) = &baseline.imported_requested_names {
        // Bind the independently checked archived-name proof into every later
        // source proof and bootstrap effect. This adds no current assertion or
        // vocabulary-lineage claim. Native-created baselines retain None, not an
        // inferred empty set, and preserve their existing proof hash semantics.
        budget.charge(names.digest().len() + proof_digest.len(), 2, 3)?;
        proof_digest = proof_hash(&[
            b"catabolic.nfo.source-history.imported-requested-tags.v1",
            proof_digest.as_bytes(),
            names.digest().as_bytes(),
        ]);
    }
    let mut index = 0usize;
    while let Some(row) = rows.next()? {
        let row = HistoryRow {
            revision: row.get(0)?,
            kind: row.get(1)?,
            fingerprint: row.get(2)?,
            fields: row.get(3)?,
            evidence: row.get(4)?,
            raw: row.get(5)?,
            command: CommandKey::new(
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
            )
            .map_err(refused)?,
        };
        if row.revision != index as i64 + 1 {
            return Err(refused("history revision gap"));
        }
        let registered = registry
            .get(&row.fingerprint)
            .ok_or_else(|| refused("history schema absent"))?;
        let document = row.document(&baseline.subject, baseline.source_kind, &registered.schema)?;
        budget.document(&document)?;
        let envelope = if index == 0 {
            if row.revision != baseline.revision
                || row.command != *baseline.command.key()
                || row.fingerprint != baseline.schema_fingerprint
                || row.fields != baseline.fields_json
                || row.evidence != baseline.evidence_json
                || row.raw != baseline.raw_mutation_json
            {
                return Err(refused("revision-one row contradicts verified baseline"));
            }
            &baseline.command
        } else {
            let command = &commands[index - 1];
            if row.command != *command.key() {
                return Err(refused("source revision command key mismatch"));
            }
            let committed =
                verify_committed_command_in(store, command, authority, expected_generation)?;
            if command.expected_generation() < prior_committed_generation {
                return Err(refused("source history command generation order mismatch"));
            }
            verify_replacement(
                &row,
                &document,
                previous
                    .as_ref()
                    .expect("baseline precedes every replacement"),
                command,
                &registry,
                &lineage,
                &mut budget,
            )?;
            prior_committed_generation = committed.committed_generation();
            command
        };
        proof_digest = proof_hash(&[
            proof_digest.as_bytes(),
            &row.revision.to_le_bytes(),
            row.fingerprint.as_bytes(),
            row.fields.as_bytes(),
            row.evidence.as_bytes(),
            row.raw.as_bytes(),
            envelope.digest().as_str().as_bytes(),
        ]);
        previous = Some(document);
        index += 1;
    }
    if index != checked(count)? {
        return Err(refused("history count changed inside writer"));
    }
    let current_source = previous.ok_or_else(|| refused("empty NFO history"))?;
    let (kind, fingerprint, fields, evidence): (String,String,String,String) = db.query_row(
        "SELECT source_kind,schema_fingerprint,fields_json,evidence_json FROM catalog_metadata_sources WHERE item_id=? AND namespace=? AND record_id=?",
        params![baseline.subject,NAMESPACE,RECORD], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
    )?;
    if kind != kind_name(current_source.kind())
        || fingerprint != current_source.schema_fingerprint()
        || fields != serde_json::to_string(current_source.fields())?
        || evidence != encode(current_source.evidence())
    {
        return Err(refused("current NFO source contradicts terminal history"));
    }
    revalidate_document(
        current_schema,
        &registry[current_source.schema_fingerprint()].schema,
        &current_source,
    )
    .map_err(refused)?;
    let imported_tag_admission = baseline
        .imported_requested_names
        .as_ref()
        .map(|names| {
            super::import_provenance::verify_recorded_tags_in(
                store,
                names,
                baseline.imported_tag_mapping.as_ref(),
                &baseline.command,
                commands,
                expected_generation,
            )
        })
        .transpose()?;
    if let Some(digest) = imported_tag_admission
        .as_ref()
        .and_then(|admission| admission.digest())
    {
        budget.charge(digest.len() + proof_digest.len(), 2, 3)?;
        proof_digest = proof_hash(&[
            b"catabolic.nfo.source-history.imported-tag-closure.v1",
            proof_digest.as_bytes(),
            digest.as_bytes(),
        ]);
    }
    store.transaction_available()?;
    Ok(VerifiedNfoSource {
        catalog_id: store.database_id.clone(),
        incarnation: store
            .catalog_incarnation
            .clone()
            .expect("verified native incarnation"),
        subject: baseline.subject.clone(),
        current_source,
        current_schema: current_schema.clone(),
        observed_generation: expected_generation,
        automatic_identity: baseline.automatic_identity.clone(),
        origin: baseline.origin.clone(),
        attribution_audit_digest: baseline.attribution_audit_digest.clone(),
        origin_proof_digest: baseline.origin_proof_digest.clone(),
        origin_physical: baseline.origin_physical.clone(),
        proof_digest,
        imported_tag_admission,
    })
}

fn read_registry(
    store: &Store,
    subject: &str,
    budget: &mut Budget,
) -> Result<BTreeMap<String, Registered>> {
    let (count, bytes, fields): (i64,i64,i64) = store.db.query_row(
        "SELECT count(*),coalesce(sum(length(CAST(policies_json AS BLOB))+length(CAST(system_fields_json AS BLOB))+length(CAST(schema_fingerprint AS BLOB))+coalesce(length(CAST(prior_fingerprint AS BLOB)),0)+length(CAST(command_catalog_id AS BLOB))+length(CAST(command_host_namespace AS BLOB))+length(CAST(command_operation_id AS BLOB))),0),coalesce(sum(json_array_length(policies_json)),0) FROM (SELECT * FROM catalog_metadata_schemas WHERE item_id=?1 ORDER BY schema_fingerprint LIMIT ?2)",
        params![subject,(MAX_METADATA_JSON_NODES + 1) as i64], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    )?;
    if count < 1 || count > MAX_METADATA_JSON_NODES as i64 {
        return Err(refused("schema registry absent or oversized"));
    }
    budget.charge(
        checked(bytes)?,
        checked(fields)?
            .saturating_mul(5)
            .saturating_add(checked(count)?.saturating_mul(6)),
        checked(bytes)? / 64 + checked(fields)? + checked(count)?,
    )?;
    let mut query = store.db.prepare("SELECT schema_fingerprint,prior_fingerprint,command_catalog_id,command_host_namespace,command_operation_id FROM catalog_metadata_schemas WHERE item_id=? ORDER BY schema_fingerprint")?;
    let mut rows = query.query([subject])?;
    let mut registry = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let fingerprint: String = row.get(0)?;
        let schema = read_registered_schema(&store.db, subject, &fingerprint).map_err(refused)?;
        registry.insert(
            fingerprint,
            Registered {
                schema,
                prior: row.get(1)?,
                command: CommandKey::new(
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                )
                .map_err(refused)?,
            },
        );
    }
    Ok(registry)
}

/// Ordinal positions prove ancestry without rescanning the chain per revision.
fn lineage(
    registry: &BTreeMap<String, Registered>,
    head: &str,
    budget: &mut Budget,
) -> Result<BTreeMap<String, usize>> {
    let mut chain = Vec::new();
    let mut seen = BTreeSet::new();
    let mut cursor = Some(head);
    while let Some(fingerprint) = cursor {
        budget.charge(0, 0, 1)?;
        if !seen.insert(fingerprint.to_owned()) {
            return Err(refused("schema lineage cycle"));
        }
        let entry = registry
            .get(fingerprint)
            .ok_or_else(|| refused("missing schema predecessor"))?;
        if let Some(prior) = &entry.prior {
            let older = registry
                .get(prior)
                .ok_or_else(|| refused("missing schema predecessor"))?;
            budget.charge(0, 0, older.schema.fields().len())?;
            validate_schema_extension(&older.schema, &entry.schema).map_err(refused)?;
        }
        chain.push(fingerprint.to_owned());
        cursor = entry.prior.as_deref();
    }
    Ok(chain
        .into_iter()
        .rev()
        .enumerate()
        .map(|(ordinal, fingerprint)| (fingerprint, ordinal))
        .collect())
}

fn verify_replacement(
    row: &HistoryRow,
    actual: &ContributionDocument,
    prior: &ContributionDocument,
    command: &CommandEnvelope,
    registry: &BTreeMap<String, Registered>,
    lineage: &BTreeMap<String, usize>,
    budget: &mut Budget,
) -> Result<()> {
    if command.payload()["operation"] == "metadata" {
        return verify_v1_patch(row, actual, prior, command, registry, lineage, budget);
    }
    if command.payload()["operation"] != "metadata_contribution_replace" {
        return Err(refused("unrecognized source history operation"));
    }
    let request: ContributionReplace =
        serde_json::from_value(command.payload()["request"].clone())?;
    if command.payload() != &json!({"operation":"metadata_contribution_replace","request":request})
        || request.contract_version != 2
        || request.subject != actual.key().subject()
        || request.namespace != NAMESPACE
        || request.record_id != RECORD
        || request.expected_revision != prior.revision()
        || serde_json::to_string(&request)? != row.raw
    {
        return Err(refused("replacement envelope/raw mutation mismatch"));
    }
    let expected = request
        .expected_schema_fingerprint
        .as_deref()
        .ok_or_else(|| refused("later replacement lacks schema predecessor"))?;
    let before = registry
        .get(expected)
        .ok_or_else(|| refused("replacement expected schema absent"))?;
    let after = &registry[&row.fingerprint];
    if lineage[prior.schema_fingerprint()] > lineage[expected]
        || lineage[expected] > lineage[&row.fingerprint]
    {
        return Err(refused("replacement schema is not forward lineage"));
    }
    if expected != row.fingerprint
        && (after.prior.as_deref() != Some(expected) || after.command != *command.key())
    {
        return Err(refused(
            "replacement schema step not owned by exact command",
        ));
    }
    let values = replacement_fields(&request.fields_json).map_err(refused)?;
    if values.keys().any(|name| name.starts_with("motion_")) {
        return Err(refused("replacement uses reserved field prefix"));
    }
    budget.charge(
        0,
        0,
        before
            .schema
            .fields()
            .len()
            .saturating_add(after.schema.fields().len())
            .saturating_add(values.len()),
    )?;
    let additions = values
        .keys()
        .filter(|name| !before.schema.fields().contains_key(*name))
        .map(|name| {
            (
                name.clone(),
                FieldPolicy {
                    value_type: ValueType::Json,
                    nullable: false,
                    normalization: Normalization::Exact,
                },
            )
        })
        .collect();
    let systems = before
        .schema
        .fields()
        .iter()
        .filter(|(name, _)| after.schema.system_fields().contains(*name))
        .map(|(name, policy)| (name.clone(), *policy))
        .collect();
    if plan_schema_extension(&before.schema, additions, systems).map_err(refused)? != after.schema {
        return Err(refused("replacement did not produce registered schema"));
    }
    let prior_schema = &registry[prior.schema_fingerprint()].schema;
    let current = revalidate_document(&after.schema, prior_schema, prior).map_err(refused)?;
    let replacement = ContributionReplacement::from_json(
        &after.schema,
        actual.key().clone(),
        actual.kind(),
        request.expected_revision,
        &request.fields_json,
        &request.evidence_json,
    )
    .map_err(refused)?;
    let planned = plan_replace(&after.schema, Some(&current), &replacement).map_err(refused)?;
    if planned != *actual {
        return Err(refused(
            "replacement fields/evidence contradict exact committed request",
        ));
    }
    Ok(())
}

/// A historical Motion v1 source Patch cannot change automatic identity or file
/// attribution. It still needs exact raw mutation, public-only edits, closed
/// Motion evidence and complete schema/source lineage, not merely a committed key.
fn verify_v1_patch(
    row: &HistoryRow,
    actual: &ContributionDocument,
    prior: &ContributionDocument,
    command: &CommandEnvelope,
    registry: &BTreeMap<String, Registered>,
    lineage: &BTreeMap<String, usize>,
    budget: &mut Budget,
) -> Result<()> {
    use crate::catalog_metadata_store::{MetadataChange, MetadataMutation};
    use catabolic_core::catalog_metadata::{ContributionPatch, plan_update};
    let mutation: MetadataMutation = serde_json::from_value(command.payload()["mutation"].clone())?;
    if command
        .payload()
        .as_object()
        .is_none_or(|object| object.len() != 2)
        || mutation.contract_version != 1
        || serde_json::to_string(&mutation)? != row.raw
    {
        return Err(refused("v1 patch envelope/raw mutation mismatch"));
    }
    let MetadataChange::Patch {
        subject,
        namespace,
        record_id,
        schema_fingerprint,
        expected_revision,
        edits_json,
        evidence_json,
    } = &mutation.change
    else {
        return Err(refused("unrecognized source history operation"));
    };
    if subject != actual.key().subject()
        || namespace != NAMESPACE
        || record_id != RECORD
        || *expected_revision != prior.revision()
        || schema_fingerprint != &row.fingerprint
        || lineage[prior.schema_fingerprint()] > lineage[schema_fingerprint]
    {
        return Err(refused(
            "v1 patch exact source/schema precondition mismatch",
        ));
    }
    let schema = &registry[schema_fingerprint].schema;
    let patch = ContributionPatch::from_json(
        schema,
        actual.key().clone(),
        actual.kind(),
        *expected_revision,
        edits_json,
        evidence_json,
    )
    .map_err(refused)?;
    let evidence = patch.evidence();
    budget.value(evidence)?;
    if evidence.as_object().is_none_or(|object| object.len() != 3)
        || evidence["owner"] != "motion.metadata.replace"
        || evidence["expected_aggregate_revision"].as_u64().is_none()
        || evidence["request_digest"].as_str().is_none_or(|value| {
            value.len() != 64
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    {
        return Err(refused(
            "unrecognized source history operation: v1 patch evidence",
        ));
    }
    budget.charge(0, patch.edits().len(), patch.edits().len())?;
    for edit in patch.edits().values() {
        if let catabolic_core::catalog_metadata::FieldEdit::Set(value) = edit {
            budget.value(value)?;
        }
    }
    if patch.edits().keys().any(|key| key.starts_with("motion_")) {
        return Err(refused("v1 source patch includes protected fields"));
    }
    let current = revalidate_document(schema, &registry[prior.schema_fingerprint()].schema, prior)
        .map_err(refused)?;
    let planned = plan_update(schema, Some(&current), &patch).map_err(refused)?;
    if planned != *actual {
        return Err(refused("v1 patch contradicts retained source history"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
