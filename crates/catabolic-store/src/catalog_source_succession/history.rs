//! Permanent succession verification. Result expiry never supplies missing
//! effect evidence, and a retained effect never rewrites command provenance.
#[path = "history/streaming.rs"]
mod streaming;
use super::*;
use crate::migration::source_history_rows::{FixedRow, Reader, RowView, Table};
use catabolic_core::{catalog_command::CommandResult, encode, sha256};
use rusqlite::types::Value as SqlValue;
use serde_json::{Value, json};
pub(super) use streaming::snapshot_revision;
pub(super) use streaming::{BorrowedContentFile, content_digest, digest as admitted_digest};

const MAX_HISTORY_ROWS: usize = 16_384;

/// Retain already-admitted file rows by move, indexed by authenticated revision.
/// The closed files table orders profile/source/revision/file_id first; all of
/// the first three are fixed per query. Verify strict binary file_id order before
/// using binary search. No encoded cohort, Value clone or copied key is needed.
struct RetainedFiles<R = Value> {
    cohorts: Vec<Vec<R>>,
    capacity: usize,
}
impl<R: RowView> RetainedFiles<R> {
    fn new(count: usize, budget: &mut impl StructuralReadBudget) -> Result<Self> {
        if count > MAX_HISTORY_ROWS {
            return Err(refuse("retained file history bound"));
        }
        let bytes = count
            .checked_mul(std::mem::size_of::<Vec<R>>())
            .ok_or_else(|| refuse("retained file history allocation overflow"))?;
        budget.charge(BudgetCharge {
            bytes,
            values: count,
            work: count,
            ..Default::default()
        })?;
        Ok(Self {
            cohorts: Vec::with_capacity(count),
            capacity: count,
        })
    }
    fn retain(
        &mut self,
        revision: i64,
        files: Vec<R>,
        budget: &mut impl StructuralReadBudget,
    ) -> Result<()> {
        if self.cohorts.len() >= self.capacity || revision != self.cohorts.len() as i64 + 1 {
            return Err(refuse("retained file history sequence differs"));
        }
        let mut previous: Option<&str> = None;
        for file in &files {
            let key = text(file, "file_id")?;
            let compared = previous.map_or(0, |old| old.len().min(key.len()));
            budget.charge(BudgetCharge {
                work: 1 + compared.div_ceil(64),
                ..Default::default()
            })?;
            if previous.is_some_and(|old| old >= key) {
                return Err(refuse("retained file history ordering differs"));
            }
            previous = Some(key);
        }
        // Capacity was admitted before allocation; push cannot grow it.
        self.cohorts.push(files);
        Ok(())
    }
    fn find<'a>(
        &'a self,
        revision: i64,
        file: &str,
        budget: &mut impl StructuralReadBudget,
    ) -> Result<Option<&'a R>> {
        let Some(index) = revision
            .checked_sub(1)
            .and_then(|n| usize::try_from(n).ok())
        else {
            return Ok(None);
        };
        let Some(files) = self.cohorts.get(index) else {
            return Ok(None);
        };
        let (mut low, mut high) = (0, files.len());
        while low < high {
            let mid = low + (high - low) / 2;
            let key = text(&files[mid], "file_id")?;
            budget.charge(BudgetCharge {
                work: 1 + key.len().min(file.len()).div_ceil(64),
                ..Default::default()
            })?;
            match key.cmp(file) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => return Ok(Some(&files[mid])),
            }
        }
        Ok(None)
    }
}

// Column names are obtained only from these closed-schema owner tables. Charge
// all cells before SELECT * materializes any retained JSON or identity strings.
fn header_rows(
    db: &rusqlite::Connection,
    profile: &str,
    source: &str,
    revision: Option<i64>,
    budget: &mut impl StructuralReadBudget,
) -> Result<Vec<Value>> {
    let table = "catalog_source_successions";
    let columns: Vec<String> = db
        .prepare("SELECT name FROM pragma_table_info(?) ORDER BY cid")?
        .query_map([table], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    if columns.is_empty()
        || columns
            .iter()
            .any(|c| !c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
    {
        return Err(refuse("permanent owner table shape absent"));
    }
    let mut args = vec![
        SqlValue::Text(profile.into()),
        SqlValue::Text(source.into()),
    ];
    let filter = if let Some(revision) = revision {
        args.push(SqlValue::Integer(revision));
        "profile=? AND source_id=? AND succession_revision=?"
    } else {
        "profile=? AND source_id=?"
    };
    let sizes = columns
        .iter()
        .map(|c| format!("coalesce(length(CAST(\"{c}\" AS BLOB)),0)"))
        .collect::<Vec<_>>()
        .join("+");
    let (count, bytes): (i64, i64) = db.query_row(
        &format!("SELECT count(*),coalesce(sum(256+{sizes}),0) FROM {table} WHERE {filter}"),
        rusqlite::params_from_iter(&args),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    charge_rows(budget, count, bytes, MAX_HISTORY_ROWS)?;
    let ordering = columns
        .iter()
        .map(|c| format!("\"{c}\" COLLATE BINARY"))
        .collect::<Vec<_>>()
        .join(",");
    crate::migration::rows(
        db,
        &format!("SELECT * FROM {table} WHERE {filter} ORDER BY {ordering}"),
        &args,
    )
}
fn text<'a>(row: &'a (impl RowView + ?Sized), key: &str) -> Result<&'a str> {
    row.get_field(key)
        .and_then(Value::as_str)
        .ok_or_else(|| refuse("permanent effect text differs"))
}
fn number(row: &(impl RowView + ?Sized), key: &str) -> Result<i64> {
    row.get_field(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| refuse("permanent effect integer differs"))
}
fn decoded(row: &(impl RowView + ?Sized), key: &str) -> Result<Value> {
    let raw = text(row, key)?;
    let value = catabolic_core::decode_json(raw)?;
    if encode(&value) != raw {
        return Err(refuse("permanent effect is not canonical"));
    }
    Ok(value)
}
// This one field is the real command receipt body, whose codec is compact.
// Other permanent JSON fields retain the source owner's existing encoding.
fn decoded_command_result(row: &(impl RowView + ?Sized)) -> Result<CommandResult> {
    let raw = text(row, "result_json")?;
    let result =
        CommandResult::new(raw).map_err(|_| refuse("permanent command result is invalid"))?;
    if result.canonical_json() != raw {
        return Err(refuse("permanent command result is not canonical"));
    }
    Ok(result)
}
#[cfg(test)]
pub(super) fn digest(
    row: &Value,
    files: &[Value],
    contexts: &[Value],
    evidence: &[Value],
    heads: &[Value],
    context_heads: &[Value],
    scopes: &[Value],
) -> Result<String> {
    let mut header = row
        .as_object()
        .cloned()
        .ok_or_else(|| refuse("permanent header absent"))?;
    header.remove("record_digest");
    Ok(sha256(encode(
        &json!({"header":header,"files":files,"contexts":contexts,"evidence":evidence,"heads":heads,"context_heads":context_heads,"scopes":scopes}),
    )))
}

/// Validate the full retained source chain and return its exact latest binding.
/// This is descriptive history, not a command or a new mapping grant. It is
/// consumed by source preparation before any future relocation can be planned.
#[cfg(test)]
pub(super) fn trace_history(phase: &str) {
    if let Some(owner) = crate::operation_budget::OperationBudget::current() {
        eprintln!(
            "source_history_phase={phase}; shared_used={:?}; shared_failure={:?}",
            owner.usage(),
            owner.failure()
        );
    }
}
pub(super) fn current_binding_in(
    store: &Store,
    profile: &str,
    source: &str,
    budget: &mut impl StructuralReadBudget,
) -> Result<Option<Value>> {
    verify_history_in(store, profile, source, budget, None)
}

// Fixed image owner may retain the already-validated original record values.
// No callback, new command, copied aggregate or alternative proof is accepted.
pub(super) fn collect_records_in(
    store: &Store,
    profile: &str,
    source: &str,
    budget: &mut impl StructuralReadBudget,
    records: &mut Vec<Value>,
) -> Result<Option<Value>> {
    verify_history_in(store, profile, source, budget, Some(records))
}
fn retain_record(records: &mut Vec<Value>, record: Value) -> Result<()> {
    if records.len() == records.capacity() {
        return Err(refuse(
            "precharged exported source record capacity exhausted",
        ));
    }
    records.push(record);
    Ok(())
}
fn verify_history_in(
    store: &Store,
    profile: &str,
    source: &str,
    budget: &mut impl StructuralReadBudget,
    retained: Option<&mut Vec<Value>>,
) -> Result<Option<Value>> {
    if store.schema_version != 41 {
        return Ok(None);
    }
    verify_history_from(
        &store.db,
        &store.database_id,
        store.catalog_incarnation.as_deref(),
        None,
        profile,
        source,
        budget,
        retained,
        None,
    )
}

pub(super) fn charge_restore_check(
    proof: &crate::catalog_restore_lineage::AdmittedRestoreHistory<'_>,
    budget: &mut impl StructuralReadBudget,
) -> Result<()> {
    // Reserve borrowed identity scalars and their repeated comparisons. The
    // token retains its original authority; no new identity is constructed.
    let bytes = proof
        .catalog_id()
        .len()
        .checked_add(proof.incarnation().len())
        .and_then(|n| n.checked_mul(4))
        .and_then(|n| n.checked_add(4 * std::mem::size_of::<String>() + 16))
        .ok_or_else(|| refuse("restore identity accounting overflow"))?;
    budget.charge(BudgetCharge {
        bytes,
        values: 6,
        work: 2 + bytes.div_ceil(64),
        ..Default::default()
    })
}

pub(super) fn collect_restored_records_in(
    db: &rusqlite::Connection,
    proof: &crate::catalog_restore_lineage::AdmittedRestoreHistory<'_>,
    profile: &str,
    source: &str,
    budget: &mut impl StructuralReadBudget,
    records: &mut Vec<Value>,
) -> Result<Option<Value>> {
    verify_restored_history_in(db, proof, profile, source, budget, Some(records))
}

pub(super) fn current_restored_binding_in(
    db: &rusqlite::Connection,
    proof: &crate::catalog_restore_lineage::AdmittedRestoreHistory<'_>,
    profile: &str,
    source: &str,
    budget: &mut impl StructuralReadBudget,
) -> Result<Option<Value>> {
    verify_restored_history_in(db, proof, profile, source, budget, None)
}

fn verify_restored_history_in(
    db: &rusqlite::Connection,
    proof: &crate::catalog_restore_lineage::AdmittedRestoreHistory<'_>,
    profile: &str,
    source: &str,
    budget: &mut impl StructuralReadBudget,
    records: Option<&mut Vec<Value>>,
) -> Result<Option<Value>> {
    charge_restore_check(proof, budget)?;
    proof.check(db)?;
    let result = verify_history_from(
        db,
        proof.catalog_id(),
        Some(proof.incarnation()),
        Some(proof),
        profile,
        source,
        budget,
        records,
        None,
    )?;
    charge_restore_check(proof, budget)?;
    proof.check(db)?;
    Ok(result)
}

// One semantic verifier for both owners. Only the fixed isolated-image wrapper
// supplies ancestry; ordinary Store reads keep exact current-incarnation proof.
fn verify_history_from(
    db: &rusqlite::Connection,
    catalog: &str,
    incarnation: Option<&str>,
    restored: Option<&crate::catalog_restore_lineage::AdmittedRestoreHistory<'_>>,
    profile: &str,
    source: &str,
    budget: &mut impl StructuralReadBudget,
    mut retained: Option<&mut Vec<Value>>,
    mut endpoint: Option<&mut super::file_endpoint::Collector>,
) -> Result<Option<Value>> {
    #[cfg(test)]
    trace_history("entry");
    // Match the source-correlated SQL chain cohort before materializing any
    // hop. Unrelated sources do not consume this retained-chain allowance.
    let (count, bytes):(i64,i64)=db.query_row(
        "SELECT count(*),coalesce(sum(256+length(CAST(evidence_key AS BLOB))+length(CAST(original_json AS BLOB))+length(CAST(renewed_json AS BLOB))),0) FROM catalog_source_succession_evidence WHERE profile=? AND source_id=?",
        [profile,source], |r|Ok((r.get(0)?,r.get(1)?)))?;
    if count < 0 || count > MAX_HISTORY_ROWS as i64 || bytes < 0 || bytes > MAX_BYTES as i64 {
        return Err(refuse("complete source evidence chain exceeds bound"));
    }
    let mut reader = Reader::new();
    let mut records = header_rows(db, profile, source, None, budget)?;
    records.sort_by_key(|r| r["revision"].as_i64());
    let mut previous: Option<Value> = None;
    let mut prior_files = RetainedFiles::<FixedRow>::new(records.len(), budget)?;
    let mut prior_evidence: BTreeMap<(String, String, String), (i64, FixedRow)> = BTreeMap::new();
    let mut previous_origin_rank = None;
    for (index, record) in records.into_iter().enumerate() {
        let revision = number(&record, "revision")?;
        if revision != i64::try_from(index).map_err(|_| refuse("history count overflow"))? + 1
            || text(&record, "catalog_id")? != catalog
            || restored.is_none() && Some(text(&record, "incarnation")?) != incarnation
        {
            return Err(refuse("permanent succession domain or sequence differs"));
        }
        if let Some(proof) = restored {
            let origin = text(&record, "incarnation")?;
            // Admission precedes the token's identity query/comparisons and both
            // bounded origin searches. This is not a per-hop generation claim.
            charge_restore_check(proof, budget)?;
            let comparison_work = proof.ordered_origins().try_fold(0usize, |total, value| {
                total
                    .checked_add(2 + 2 * value.len().min(origin.len()).div_ceil(64))
                    .ok_or_else(|| refuse("restore origin accounting overflow"))
            })?;
            budget.charge(BudgetCharge {
                work: comparison_work,
                ..Default::default()
            })?;
            proof.authorize_historical(origin, number(&record, "committed_generation")?)?;
            let rank = proof.origin_rank(origin)?;
            if previous_origin_rank.is_some_and(|previous| previous > rank) {
                return Err(refuse(
                    "source history moves backwards through restore ancestry",
                ));
            }
            previous_origin_rank = Some(rank);
        }
        let files = reader.rows(db, Table::Files, profile, source, Some(revision), budget)?;
        let contexts = reader.rows(db, Table::Contexts, profile, source, Some(revision), budget)?;
        let evidence = reader.rows(db, Table::Evidence, profile, source, Some(revision), budget)?;
        let heads = reader.rows(db, Table::Heads, profile, source, Some(revision), budget)?;
        let context_heads = reader.rows(
            db,
            Table::ContextHeads,
            profile,
            source,
            Some(revision),
            budget,
        )?;
        let scopes = reader.rows(db, Table::Scopes, profile, source, Some(revision), budget)?;
        let required_scopes = reader.rows(
            db,
            Table::RequiredScopes,
            profile,
            source,
            Some(revision),
            budget,
        )?;
        if required_scopes.len() != scopes.len()
            || required_scopes
                .iter()
                .zip(&scopes)
                .any(|(required, saved)| {
                    required["scope_kind"] != saved["scope_kind"]
                        || required["scope_key"] != saved["scope_key"]
                        || saved["revision"].as_i64().is_none_or(|v| v < 0)
                })
        {
            return Err(refuse("complete permanent scope membership differs"));
        }
        for context in &contexts {
            let count = context_heads
                .iter()
                .filter(|h| h["context_id"] == context["context_id"])
                .count();
            if number(context, "head_count")? != count as i64 {
                return Err(refuse("complete permanent context membership differs"));
            }
        }
        #[cfg(test)]
        trace_history("complete_cohort_before_digest");
        if number(&record, "file_count")? != files.len() as i64
            || number(&record, "context_count")? != contexts.len() as i64
            || number(&record, "head_count")? != heads.len() as i64
            || number(&record, "scope_count")? != scopes.len() as i64
            || number(&record, "evidence_count")? != evidence.len() as i64
            || text(&record, "evidence_digest")? != sha256(encode(&json!(evidence)))
            || text(&record, "record_digest")?
                != streaming::fixed_digest(
                    &record,
                    &files,
                    &contexts,
                    &evidence,
                    &heads,
                    &context_heads,
                    &scopes,
                    budget,
                )?
        {
            return Err(refuse("complete permanent effect differs"));
        }
        #[cfg(test)]
        trace_history("complete_digest_verified");
        if previous.is_none() && text(&record, "predecessor_digest")? != "0".repeat(64) {
            return Err(refuse("first permanent predecessor differs"));
        }
        if let Some(old) = &previous {
            if record["predecessor_digest"] != old["record_digest"]
                || record["old_binding_json"] != old["new_binding_json"]
                || record["old_mapping_json"] != old["new_mapping_json"]
                || number(&record, "expected_generation")? < number(old, "committed_generation")?
            {
                return Err(refuse("permanent predecessor differs"));
            }
        }
        let mut selected_old = None;
        for file in &files {
            let old = decoded(file, "old_observation_json")?;
            let occurrence = decoded(file, "old_occurrence_json")?;
            let keys = [
                "profile",
                "file_id",
                "size",
                "mtime_ns",
                "device",
                "inode",
                "status",
                "scan_id",
                "observation_origin",
                "succession_source_id",
                "succession_revision",
            ];
            if old
                .as_object()
                .is_none_or(|o| o.len() != keys.len() || keys.iter().any(|k| !o.contains_key(*k)))
                || old["profile"] != profile
                || old["file_id"] != file["file_id"]
                || old["status"] != "present"
                || ["size", "mtime_ns", "device", "inode"]
                    .iter()
                    .any(|key| old[key] != occurrence[key])
            {
                return Err(refuse("historical observation tuple differs"));
            }
            match old["observation_origin"].as_str() {
                Some("scan") => {
                    let scan = decoded(file, "old_scan_provenance_json")?;
                    // Initial effect minting must have matched the real scan row.
                    // Receipt-first historical recovery uses the permanent digest,
                    // not continued existence of ordinarily descriptive scan data.
                    if !old["succession_source_id"].is_null()
                        || !old["succession_revision"].is_null()
                        || scan
                            != json!({"id":text(&old,"scan_id")?,"profile":profile,"location":source})
                    {
                        return Err(refuse("historical scan provenance differs"));
                    }
                }
                Some("succession") => {
                    let predecessor = number(&old, "succession_revision")?;
                    let prior = prior_files
                        .find(predecessor, text(file, "file_id")?, budget)?
                        .ok_or_else(|| refuse("historical observation predecessor missing"))?;
                    if !decoded(file, "old_scan_provenance_json")?.is_null()
                        || !old["scan_id"].is_null()
                        || old["succession_source_id"] != source
                        || predecessor >= revision
                        || decoded(prior, "new_occurrence_json")? != occurrence
                        || prior["new_ctime_ns"] != file["old_ctime_ns"]
                    {
                        return Err(refuse("historical observation succession differs"));
                    }
                }
                _ => return Err(refuse("historical observation origin unsupported")),
            }
            if let Some(collector) = endpoint.as_deref() {
                let key = text(file, "file_id")?;
                budget.charge(BudgetCharge {
                    work: 1 + key.len().min(collector.file_id().len()).div_ceil(64),
                    ..Default::default()
                })?;
                if key == collector.file_id() {
                    if selected_old.is_some() {
                        return Err(refuse("duplicate selected endpoint file"));
                    }
                    super::file_endpoint::SelectedOld::admit(&old, &occurrence, budget)?;
                    selected_old = Some(super::file_endpoint::SelectedOld::new(old, occurrence));
                }
            }
        }
        // The outer record binds the prior command and its complete effect;
        // each nonzero evidence link additionally names the exact same-key
        // predecessor. A current effective view alone cannot supply this proof.
        for item in &evidence {
            let original = decoded(item, "original_json")?;
            let renewed = decoded(item, "renewed_json")?;
            if text(item, "evidence_digest")? != sha256(encode(&json!([original, renewed]))) {
                return Err(refuse("permanent evidence digest differs"));
            }
            let key = (
                text(item, "file_id")?.to_owned(),
                text(item, "evidence_kind")?.to_owned(),
                text(item, "evidence_key")?.to_owned(),
            );
            let predecessor = text(item, "predecessor_digest")?;
            if predecessor == "0".repeat(64)
                && matches!(
                    text(item, "evidence_kind")?,
                    "processing_fact" | "media_header"
                )
                && files
                    .iter()
                    .find(|f| f["file_id"] == item["file_id"])
                    .map(|f| decoded(f, "old_observation_json"))
                    .transpose()?
                    .is_some_and(|o| o["observation_origin"] == "succession")
            {
                return Err(refuse(
                    "renewed observation cannot restart base evidence chain",
                ));
            }
            if predecessor != "0".repeat(64) {
                let (prior_revision, prior) = prior_evidence
                    .get(&key)
                    .ok_or_else(|| refuse("permanent evidence predecessor missing"))?;
                if *prior_revision != revision - 1
                    || prior["evidence_digest"] != predecessor
                    || prior["original_json"] != item["original_json"]
                {
                    return Err(refuse("permanent evidence predecessor differs"));
                }
            }
        }
        let command_result = decoded_command_result(&record)?;
        let result = command_result.value();
        if result["admission_digest"] != record["admission_digest"]
            || text(&record, "result_digest")? != sha256(encode(result))
        {
            return Err(refuse("permanent result digest differs"));
        }
        let receipt_args = [
            SqlValue::Text(text(&record, "catalog_id")?.into()),
            SqlValue::Text(text(&record, "command_host_namespace")?.into()),
            SqlValue::Text(text(&record, "command_operation_id")?.into()),
        ];
        let (count, bytes): (i64,i64) = db.query_row(
            "SELECT count(*),coalesce(sum(256+length(CAST(payload_digest AS BLOB))+length(CAST(admission_incarnation AS BLOB))+length(CAST(result_state AS BLOB))+coalesce(length(CAST(result_json AS BLOB)),0)),0) FROM catalog_command_receipts WHERE catalog_id=? AND host_namespace=? AND operation_id=?",
            rusqlite::params_from_iter(&receipt_args), |r| Ok((r.get(0)?,r.get(1)?)))?;
        charge_rows(budget, count, bytes, 1)?;
        let receipt = crate::migration::rows(
            db,
            "SELECT payload_digest,admission_incarnation,expected_generation,committed_generation,result_state,result_json FROM catalog_command_receipts WHERE catalog_id=? AND host_namespace=? AND operation_id=?",
            &receipt_args,
        )?;
        let Some(receipt) = receipt.first() else {
            return Err(refuse("permanent command provenance missing"));
        };
        if receipt["payload_digest"] != record["payload_digest"]
            || receipt["admission_incarnation"] != record["incarnation"]
            || receipt["expected_generation"] != record["expected_generation"]
            || receipt["committed_generation"] != record["committed_generation"]
            || !(receipt["result_state"] == "expired" && receipt["result_json"].is_null()
                || receipt["result_state"] == "full"
                    && receipt["result_json"] == record["result_json"])
        {
            return Err(refuse("permanent command provenance differs"));
        }
        // Filesystem currentness is deliberately separate. Later legitimate
        // observations may advance a file without changing this binding chain.
        #[cfg(test)]
        trace_history("before_file_move_retention");
        prior_files.retain(revision, files, budget)?;
        if let Some(collector) = endpoint.as_deref_mut() {
            let selected = prior_files.find(revision, collector.file_id(), budget)?;
            collector.observe(revision, selected, selected_old, budget)?;
        }
        #[cfg(test)]
        trace_history("after_file_move_retention");
        // Evidence-key indexing is independent of the moved file cohorts; its
        // existing representation and complete validation remain unchanged.
        for item in evidence {
            prior_evidence.insert(
                (
                    text(&item, "file_id")?.to_owned(),
                    text(&item, "evidence_kind")?.to_owned(),
                    text(&item, "evidence_key")?.to_owned(),
                ),
                (revision, item),
            );
        }
        if let Some(old) = previous.replace(record)
            && let Some(records) = retained.as_deref_mut()
        {
            retain_record(records, old)?;
        }
    }
    #[cfg(test)]
    trace_history("exit");
    let binding = previous
        .as_ref()
        .map(|r| decoded(r, "new_binding_json"))
        .transpose()?;
    if let Some(last) = previous
        && let Some(records) = retained
    {
        retain_record(records, last)?;
    }
    Ok(binding)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct IndexBudget {
        bytes: usize,
        work: usize,
        byte_limit: usize,
        work_limit: usize,
    }
    impl IndexBudget {
        fn unlimited() -> Self {
            Self {
                bytes: 0,
                work: 0,
                byte_limit: usize::MAX,
                work_limit: usize::MAX,
            }
        }
    }
    impl StructuralReadBudget for IndexBudget {
        fn charge(&mut self, c: BudgetCharge) -> Result<()> {
            self.bytes = self
                .bytes
                .checked_add(c.bytes)
                .ok_or_else(|| refuse("test byte overflow"))?;
            self.work = self
                .work
                .checked_add(c.work)
                .ok_or_else(|| refuse("test work overflow"))?;
            if self.bytes > self.byte_limit || self.work > self.work_limit {
                return Err(refuse("test index budget"));
            }
            Ok(())
        }
        fn raw_json(&mut self, raw: &str) -> Result<()> {
            self.charge(BudgetCharge {
                bytes: raw.len(),
                ..Default::default()
            })
        }
    }
    #[test]
    fn moved_file_history_matches_map_oracle_without_cloning_cohorts() {
        let mut budget = IndexBudget::unlimited();
        let mut history = RetainedFiles::<Value>::new(2, &mut budget).unwrap();
        let mut oracle = BTreeMap::new();
        for revision in 1..=2 {
            let files = vec![
                json!({"file_id":"a","payload":{"escaped":"a\\b\n\"é"}}),
                json!({"file_id":"z","payload":[null,true,1]}),
            ];
            let allocation = files.as_ptr();
            let nested = files[0]["payload"]["escaped"].as_str().unwrap().as_ptr();
            for file in &files {
                oracle.insert(
                    (revision, text(file, "file_id").unwrap().to_owned()),
                    file.clone(),
                );
            }
            history.retain(revision, files, &mut budget).unwrap();
            assert_eq!(
                history.cohorts[(revision - 1) as usize].as_ptr(),
                allocation
            );
            assert_eq!(
                history.find(revision, "a", &mut budget).unwrap().unwrap()["payload"]["escaped"]
                    .as_str()
                    .unwrap()
                    .as_ptr(),
                nested
            );
        }
        for revision in [-1, 0, 1, 2, 3] {
            for file in ["a", "b", "z"] {
                assert_eq!(
                    history.find(revision, file, &mut budget).unwrap(),
                    oracle.get(&(revision, file.to_owned()))
                );
            }
        }
        assert_eq!(budget.bytes, 2 * std::mem::size_of::<Vec<Value>>());
    }
    #[test]
    fn moved_file_history_refuses_sequence_duplicates_and_wrong_order() {
        for files in [
            vec![json!({"file_id":"z"}), json!({"file_id":"a"})],
            vec![json!({"file_id":"a"}), json!({"file_id":"a"})],
            vec![json!({"file_id":null})],
        ] {
            let mut budget = IndexBudget::unlimited();
            let mut history = RetainedFiles::<Value>::new(1, &mut budget).unwrap();
            assert!(history.retain(1, files, &mut budget).is_err());
            assert!(history.cohorts.is_empty());
        }
        let mut budget = IndexBudget::unlimited();
        let mut history = RetainedFiles::<Value>::new(1, &mut budget).unwrap();
        assert!(history.retain(2, vec![], &mut budget).is_err());
        history.retain(1, vec![], &mut budget).unwrap();
        assert!(history.retain(2, vec![], &mut budget).is_err());
    }
    #[test]
    fn moved_file_history_admits_outer_storage_and_charges_lookup_before_use() {
        let bytes = std::mem::size_of::<Vec<Value>>();
        let mut budget = IndexBudget {
            byte_limit: bytes - 1,
            ..IndexBudget::unlimited()
        };
        assert!(RetainedFiles::<Value>::new(1, &mut budget).is_err());
        let mut budget = IndexBudget {
            byte_limit: bytes,
            ..IndexBudget::unlimited()
        };
        let mut history = RetainedFiles::<Value>::new(1, &mut budget).unwrap();
        history
            .retain(1, vec![json!({"file_id":"a"})], &mut budget)
            .unwrap();
        budget.work_limit = budget.work;
        assert!(history.find(1, "a", &mut budget).is_err());
        assert_eq!(budget.bytes, bytes);
    }

    #[test]
    fn permanent_commitment_binds_historical_scan_without_current_scan_lookup() {
        // Format evidence only. A real effect must mint this from the actual
        // scan under the command writer, never from request JSON.
        let file = json!({"old_scan_provenance_json":"{\"id\":\"scan\",\"profile\":\"default\",\"location\":\"media\"}"});
        let original = digest(&json!({}), &[file.clone()], &[], &[], &[], &[], &[]).unwrap();
        let mut changed = file;
        changed["old_scan_provenance_json"] =
            json!("{\"id\":\"different\",\"profile\":\"default\",\"location\":\"media\"}");
        assert_ne!(
            original,
            digest(&json!({}), &[changed], &[], &[], &[], &[], &[]).unwrap()
        );
    }

    #[test]
    fn permanent_commitment_covers_each_child_domain_without_self_reference() {
        // Commitment-format test only, not native admission or a fake receipt.
        let header = json!({"record_digest":"not_part_of_its_own_digest","revision":1});
        let files = vec![json!({"file_id":"file","new_occurrence_json":"{}"})];
        let contexts = vec![json!({"context_id":"context","renewal_json":"{}"})];
        let evidence = vec![json!({"evidence_kind":"component_assertion","original_json":"{}"})];
        let heads = vec![json!({"entity_kind":"bindings","revision":2})];
        let context_heads = vec![json!({"context_id":"context","entity_kind":"items"})];
        let scopes =
            vec![json!({"scope_kind":"source_files","scope_key":"[\"media\"]","revision":0})];
        let original = digest(
            &header,
            &files,
            &contexts,
            &evidence,
            &heads,
            &context_heads,
            &scopes,
        )
        .unwrap();
        let mut same = header.clone();
        same["record_digest"] = json!("changed_self_digest");
        assert_eq!(
            original,
            digest(
                &same,
                &files,
                &contexts,
                &evidence,
                &heads,
                &context_heads,
                &scopes
            )
            .unwrap()
        );
        for changed in [
            digest(
                &header,
                &files,
                &contexts,
                &evidence,
                &heads,
                &context_heads,
                &[],
            )
            .unwrap(),
            digest(&header, &files, &contexts, &evidence, &heads, &[], &scopes).unwrap(),
            digest(
                &header,
                &files,
                &contexts,
                &evidence,
                &[],
                &context_heads,
                &scopes,
            )
            .unwrap(),
            digest(
                &header,
                &[],
                &contexts,
                &evidence,
                &heads,
                &context_heads,
                &scopes,
            )
            .unwrap(),
            digest(
                &header,
                &files,
                &[],
                &evidence,
                &heads,
                &context_heads,
                &scopes,
            )
            .unwrap(),
            digest(
                &header,
                &files,
                &contexts,
                &[],
                &heads,
                &context_heads,
                &scopes,
            )
            .unwrap(),
        ] {
            assert_ne!(original, changed);
        }
        same["revision"] = json!(2);
        assert_ne!(
            original,
            digest(
                &same,
                &files,
                &contexts,
                &evidence,
                &heads,
                &context_heads,
                &scopes
            )
            .unwrap()
        );
    }
    #[test]
    fn retained_result_requires_canonical_complete_json() {
        assert!(decoded(&json!({"result":"{ \"a\":1}"}), "result").is_err());
        assert!(decoded(&json!({"result":"{\"a\":1"}), "result").is_err());
        assert_eq!(
            decoded(&json!({"result":encode(&json!({"a":1}))}), "result").unwrap(),
            json!({"a":1})
        );
    }
    #[test]
    fn retained_command_result_requires_exact_receipt_codec() {
        let value = json!({"admission_digest":"admitted","files":2,"binding":{"root":"/media"}});
        let spaced = encode(&value);
        let receipt = CommandResult::new(&spaced).unwrap();
        assert_ne!(receipt.canonical_json(), spaced);
        let decoded = decoded_command_result(&json!({
            "result_json":receipt.canonical_json()
        }))
        .unwrap();
        assert_eq!(decoded.value(), &value);
        // The content digest keeps its original (spaced) canonical domain.
        assert_eq!(sha256(encode(decoded.value())), sha256(&spaced));
        for raw in [
            spaced,
            format!(" {}", receipt.canonical_json()),
            r#"{"files":2,"binding":{"root":"/media"},"admission_digest":"admitted"}"#.into(),
            r#"{"files":1,"files":2}"#.into(),
            r#"{"files":1"#.into(),
        ] {
            assert!(decoded_command_result(&json!({"result_json":raw})).is_err());
        }
    }
    #[test]
    fn borrowed_history_digest_uses_inherited_budget_without_changing_domain() {
        use crate::operation_budget::{BudgetLimits, OperationBudget};
        use std::time::Duration;

        let shared = OperationBudget::new(
            Duration::from_secs(5),
            BudgetLimits {
                items: 4096,
                values: 16_384,
                bytes: MAX_BYTES,
                work: 16_384,
            },
        )
        .unwrap();
        shared
            .charge(BudgetCharge {
                work: 7,
                ..Default::default()
            })
            .unwrap();
        let mut budget = crate::catalog_nfo_attribution::ProofBudget::new(shared.clone());
        let header = json!({"record_digest":"excluded","revision":1,"result_json":"{}"});
        let child = [json!({"ordinal":1,"value":"quoted \\\" and Unicode é"})];
        let observed = admitted_digest(
            &header,
            &child,
            &child,
            &child,
            &child,
            &child,
            &child,
            &mut budget,
        )
        .unwrap();
        assert_eq!(
            observed,
            digest(&header, &child, &child, &child, &child, &child, &child).unwrap()
        );
        assert!(budget.uses_operation(&shared));
        assert!(shared.usage().work > 7);
        assert!(shared.usage().bytes > 0);
        assert!(shared.usage().values > 0);

        let exhausted = OperationBudget::new(
            Duration::from_secs(5),
            BudgetLimits {
                items: 4096,
                values: 16_384,
                bytes: MAX_BYTES,
                work: 1,
            },
        )
        .unwrap();
        for _ in 0..2 {
            let mut budget = crate::catalog_nfo_attribution::ProofBudget::new(exhausted.clone());
            assert!(admitted_digest(&header, &[], &[], &[], &[], &[], &[], &mut budget).is_err());
            assert!(exhausted.failure().is_some());
        }
    }
}

// Concrete descriptive collector only. It cannot escape a partial history or
// substitute a caller callback for the complete ordinary-incarnation verifier.
pub(super) fn collect_file_endpoint_in(
    store: &Store,
    profile: &str,
    source: &str,
    budget: &mut impl StructuralReadBudget,
    collector: &mut super::file_endpoint::Collector,
) -> Result<Option<Value>> {
    if store.schema_version != 41 {
        return Err(refuse("file endpoint requires completed schema41"));
    }
    verify_history_from(
        &store.db,
        &store.database_id,
        store.catalog_incarnation.as_deref(),
        None,
        profile,
        source,
        budget,
        None,
        Some(collector),
    )
}
