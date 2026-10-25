//! Replay recorded source assertion writes from authenticated import facts.
//! Current rows are compared only at the terminal boundary, never used as seeds.
use super::*;
use crate::catalog_metadata_store::ContributionReplace;
use crate::tagging::source_assertion_plan as transition;
use serde::Deserialize;

#[derive(Debug, Serialize)]
struct AssertionRow {
    id: String,
    tag_id: String,
    active: bool,
    note: String,
    confidence: Option<f64>,
    created_at: String,
    updated_at: String,
    revision: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Before {
    assertions: Vec<(String, String, i64, String, Option<f64>)>,
    suppressions: Vec<(String, i64, bool, Option<i64>)>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct After {
    tags: Vec<String>,
    excluded_tags: Vec<String>,
}
struct History {
    before: String,
    after: String,
    command: CommandKey,
    at: i64,
}
fn timestamp(at: i64) -> Result<String> {
    chrono::DateTime::from_timestamp_micros(at)
        .filter(|_| at >= 0)
        .map(|t| t.to_rfc3339())
        .ok_or_else(|| refused("invalid recorded assertion time"))
}
fn charge(budget: &mut ProofBudget, bytes: usize, values: usize) -> Result<()> {
    budget.charge(BudgetCharge {
        bytes,
        values,
        work: bytes / 64 + values + 1,
        ..Default::default()
    })
}
fn source_time(
    store: &Store,
    subject: &str,
    revision: i64,
    command: &CommandEnvelope,
    budget: &mut ProofBudget,
) -> Result<i64> {
    charge(budget, 0, 2)?;
    store.db.query_row(
        "SELECT admitted_at_us FROM catalog_metadata_history WHERE item_id=? AND namespace='motion.nfo' AND record_id='document' AND revision=? AND command_catalog_id=? AND command_host_namespace=? AND command_operation_id=?",
        params![subject,revision,command.key().catalog_id(),command.key().host_namespace(),command.key().operation_id()],|r|r.get(0),
    ).map_err(Into::into)
}
fn read_history(
    store: &Store,
    subject: &str,
    budget: &mut ProofBudget,
) -> Result<Option<BTreeMap<i64, History>>> {
    let mut query=store.db.prepare("SELECT record_id,revision,before_json,after_json,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us FROM catalog_metadata_tag_history WHERE item_id=? AND namespace='motion.nfo'")?;
    let mut rows = query.query([subject])?;
    let mut history = BTreeMap::new();
    while let Some(row) = rows.next()? {
        charge(budget, 0, 8)?;
        if text(row, 0, 256, budget)? != "document" {
            return Ok(None);
        }
        let revision: i64 = row.get(1)?;
        let before = text(row, 2, 65536, budget)?;
        let after = text(row, 3, 65536, budget)?;
        budget.raw_json(&before, 32)?;
        budget.raw_json(&after, 32)?;
        let command = CommandKey::new(
            text(row, 4, 256, budget)?,
            text(row, 5, 256, budget)?,
            text(row, 6, 256, budget)?,
        )
        .map_err(|_| refused("invalid tag-history command key"))?;
        let at: i64 = row.get(7)?;
        if revision < 2
            || at < 0
            || history
                .insert(
                    revision,
                    History {
                        before,
                        after,
                        command,
                        at,
                    },
                )
                .is_some()
        {
            return Ok(None);
        }
    }
    Ok(Some(history))
}
fn requested_names(
    request: &ContributionReplace,
    budget: &mut ProofBudget,
) -> Result<Option<BTreeSet<String>>> {
    if request.tags.len() > 100 || !request.excluded_tags.is_empty() {
        return Ok(None);
    }
    let mut names = BTreeSet::new();
    for name in &request.tags {
        if name.is_empty() || name.len() > 100 {
            return Ok(None);
        }
        budget.serialized(&name, 32)?;
        super::super::imported_tags::reserve_normalization(name, budget)?;
        if crate::tagging::tag_name(&json!(name))? != *name || !names.insert(name.clone()) {
            return Ok(None);
        }
    }
    Ok(Some(names))
}
/// Since every committed replacement resolves or creates each requested alias,
/// a currently live permanent alias revision1 proves that mapping never changed
/// across the request. A current name with a later counter is NOT historical ID
/// proof, even when its spelling also appeared in the original import.
fn stable_requested_id(
    store: &Store,
    name: &str,
    budget: &mut ProofBudget,
) -> Result<Option<String>> {
    charge(budget, 0, 4)?;
    let mut query=store.db.prepare("SELECT n.tag_id,e.revision,e.deleted FROM tag_names n LEFT JOIN catalog_entity_revisions e ON e.entity_kind='tag_names' AND e.entity_key=json_array(n.name) WHERE n.name=?")?;
    let mut rows = query.query([name])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let revision: Option<i64> = row.get(1)?;
    let deleted: Option<bool> = row.get(2)?;
    if revision != Some(1) || deleted != Some(false) {
        return Ok(None);
    }
    Ok(Some(text(row, 0, 256, budget)?))
}

pub(super) fn accounted_in(
    store: &Store,
    names: &ImportedRequestedNames,
    mapping: Option<&ImportedTagMapping>,
    activation: &CommandEnvelope,
    commands: &[CommandEnvelope],
    generation: i64,
) -> Result<ImportedTagAdmission> {
    let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
    if let Some(mapping) = mapping {
        if mapping.subject != names.subject()
            || mapping.source_revision != names.source_revision()
            || mapping.activation_digest != activation.digest().as_str()
        {
            return Err(refused("historical tag mapping token scope mismatch"));
        }
    } else if !names.tags().is_empty() {
        return Ok(ImportedTagAdmission::MissingMapping);
    }
    let Some(mut histories) = read_history(store, names.subject(), &mut budget)? else {
        return Ok(ImportedTagAdmission::UnprovedHistory);
    };
    // Same typed-import owner invocation passes this exact time to both metadata
    // and tag creation. It is not the command receipt's later commit clock.
    let at = source_time(store, names.subject(), 1, activation, &mut budget)?;
    charge(
        &mut budget,
        256 + activation.key().operation_id().len() * 8,
        8,
    )?;
    let created = timestamp(at)?;
    let note=json!({"import_operation_id":activation.key().operation_id(),"source_revision":names.source_revision()}).to_string();
    let mut predicted = BTreeMap::<String, AssertionRow>::new();
    if let Some(mapping) = mapping {
        for row in mapping.assertions.values() {
            budget.serialized(
                &(
                    &row.assertion_id,
                    &row.assertion_id,
                    &row.tag_id,
                    &note,
                    &created,
                    &created,
                ),
                64,
            )?;
            predicted.insert(
                row.assertion_id.clone(),
                AssertionRow {
                    id: row.assertion_id.clone(),
                    tag_id: row.tag_id.clone(),
                    active: true,
                    note: note.clone(),
                    confidence: None,
                    created_at: created.clone(),
                    updated_at: created.clone(),
                    revision: 1,
                },
            );
        }
    }
    let mut proof = budget.serialized(
        &(
            "catabolic.nfo.imported-tags-accounted.v1",
            &store.database_id,
            &store.catalog_incarnation,
            generation,
            names.digest(),
            mapping.map(|m| m.digest.as_str()),
            activation.digest().as_str(),
            at,
            &predicted,
        ),
        64,
    )?;
    let uuid =
        uuid::Uuid::parse_str(&store.database_id).map_err(|_| refused("invalid catalog UUID"))?;
    for (index, command) in commands.iter().enumerate() {
        charge(&mut budget, 0, 1)?;
        let revision = i64::try_from(index)
            .ok()
            .and_then(|n| n.checked_add(2))
            .ok_or_else(|| refused("source revision overflow"))?;
        // The parent has already verified EVERY exact command and reconstructed
        // its metadata revision. This child adds only assertion-history closure.
        if command.payload()["operation"] == "metadata" {
            if histories.contains_key(&revision) {
                return Ok(ImportedTagAdmission::UnprovedHistory);
            }
            proof = budget.serialized(
                &(
                    proof,
                    revision,
                    command.digest().as_str(),
                    "v1_patch_preserves_assertions",
                ),
                64,
            )?;
            continue;
        }
        if command.payload()["operation"] != "metadata_contribution_replace" {
            return Ok(ImportedTagAdmission::UnprovedHistory);
        }
        budget.serialized(&command.payload()["request"], 64)?;
        let request: ContributionReplace =
            serde_json::from_value(command.payload()["request"].clone())?;
        let Some(wanted) = requested_names(&request, &mut budget)? else {
            return Ok(ImportedTagAdmission::UnprovedHistory);
        };
        let Some(history) = histories.remove(&revision) else {
            return Ok(ImportedTagAdmission::UnprovedHistory);
        };
        if history.command != *command.key()
            || history.at != source_time(store, names.subject(), revision, command, &mut budget)?
        {
            return Ok(ImportedTagAdmission::UnprovedHistory);
        }
        let before: Before = serde_json::from_value(
            catabolic_core::json::parse(&history.before, true)
                .map_err(|_| refused("invalid duplicate-safe before history"))?,
        )?;
        let after: After = serde_json::from_value(
            catabolic_core::json::parse(&history.after, true)
                .map_err(|_| refused("invalid duplicate-safe after history"))?,
        )?;
        if before.assertions.len() > 100
            || before.suppressions.len() > 100
            || !after.excluded_tags.is_empty()
            || !after.tags.iter().eq(wanted.iter())
        {
            return Ok(ImportedTagAdmission::UnprovedHistory);
        }
        charge(
            &mut budget,
            0,
            predicted.len() * 2 + before.suppressions.len(),
        )?;
        let active: Vec<_> = predicted.values().filter(|r| r.active).collect();
        if before.assertions.len() != active.len()
            || before.assertions.iter().zip(&active).any(|(a, p)| {
                a.0 != p.id || a.1 != p.tag_id || a.2 != 1 || a.3 != p.note || a.4 != p.confidence
            })
        {
            return Ok(ImportedTagAdmission::ChangedAssertions);
        }
        // Suppressions are independently owned decisions. Their exact raw
        // snapshot is bound below, not replayed as NFO-owned state.
        let mut resolved = BTreeMap::new();
        let mut insert_ids = BTreeMap::new();
        let mut existing = BTreeMap::new();
        for name in &wanted {
            let Some(tag) = stable_requested_id(store, name, &mut budget)? else {
                return Ok(ImportedTagAdmission::UnprovedVocabulary);
            };
            if !insert_ids.contains_key(&tag) && !existing.contains_key(&tag) {
                charge(&mut budget, 0, predicted.len() + 1)?;
                if let Some(row) = predicted.values().find(|r| r.tag_id == tag) {
                    budget.serialized(&(&tag, &row.id), 64)?;
                    existing.insert(tag.clone(), row.id.clone());
                } else {
                    charge(
                        &mut budget,
                        (names.subject().len() + tag.len() + NAMESPACE.len()) * 16 + 256,
                        12,
                    )?;
                    let insert = transition::assertion_id(&uuid, names.subject(), &tag, NAMESPACE);
                    budget.serialized(&(&tag, &insert), 64)?;
                    insert_ids.insert(tag.clone(), insert);
                }
            }
            budget.serialized(name, 64)?;
            resolved.insert(name.clone(), tag);
        }
        charge(&mut budget, 0, active.len() * 3 + wanted.len() * 8)?;
        let active_inputs: Vec<_> = active
            .iter()
            .map(|r| transition::Active {
                id: &r.id,
                tag_id: &r.tag_id,
            })
            .collect();
        let requested: Vec<_> = wanted
            .iter()
            .map(|name| {
                let tag = &resolved[name];
                transition::Requested {
                    name,
                    tag_id: tag,
                    existing: existing.get(tag).map(String::as_str),
                    insert_id: existing.get(tag).unwrap_or_else(|| &insert_ids[tag]),
                }
            })
            .collect();
        let plan = transition::plan(&active_inputs, &requested)?;
        // Own the bounded instruction IDs before mutating their borrowed rows.
        budget.serialized(&plan.deactivate, 64)?;
        for id in plan.counters.keys() {
            budget.serialized(id, 64)?;
        }
        let deactivate: Vec<String> = plan.deactivate.iter().map(|id| (*id).into()).collect();
        let mut writes = Vec::new();
        for write in &plan.writes {
            let (insert, id, tag) = match write {
                transition::Write::Insert { id, tag_id } => (true, *id, *tag_id),
                transition::Write::Reactivate { id, tag_id } => (false, *id, *tag_id),
            };
            budget.serialized(&(id, tag), 64)?;
            writes.push((insert, id.to_owned(), tag.to_owned()));
        }
        let counters: Vec<_> = plan
            .counters
            .iter()
            .map(|(id, delta)| ((*id).to_owned(), *delta))
            .collect();
        drop(plan);
        drop(active_inputs);
        drop(active);
        charge(&mut budget, 256 + command.key().operation_id().len() * 8, 8)?;
        let timestamp = timestamp(history.at)?;
        let note=json!({"metadata_source_revision":revision,"command_operation_id":command.key().operation_id()}).to_string();
        for id in deactivate {
            budget.serialized(&timestamp, 64)?;
            let row = predicted
                .get_mut(&id)
                .ok_or_else(|| refused("deactivation missing predicted row"))?;
            row.active = false;
            row.updated_at = timestamp.clone();
        }
        for (insert, id, tag) in writes {
            budget.serialized(&(&id, &id, &note, &timestamp, &timestamp), 64)?;
            if insert {
                predicted.insert(
                    id.clone(),
                    AssertionRow {
                        id,
                        tag_id: tag,
                        active: true,
                        note: note.clone(),
                        confidence: None,
                        created_at: timestamp.clone(),
                        updated_at: timestamp.clone(),
                        revision: 1,
                    },
                );
            } else {
                let row = predicted
                    .get_mut(&id)
                    .ok_or_else(|| refused("reactivation missing predicted row"))?;
                row.active = true;
                row.note = note.clone();
                row.updated_at = timestamp.clone();
            }
        }
        for (id, delta) in counters {
            let row = predicted
                .get_mut(&id)
                .ok_or_else(|| refused("counter missing predicted row"))?;
            if let transition::Counter::Advance(delta) = delta {
                row.revision = row
                    .revision
                    .checked_add(
                        i64::try_from(delta).map_err(|_| refused("assertion counter overflow"))?,
                    )
                    .ok_or_else(|| refused("assertion counter overflow"))?;
            }
        }
        proof = budget.serialized(
            &(
                proof,
                revision,
                command.digest().as_str(),
                history.at,
                history.before,
                history.after,
                &predicted,
            ),
            64,
        )?;
    }
    if !histories.is_empty() {
        return Ok(ImportedTagAdmission::UnprovedHistory);
    }
    terminal(store, names.subject(), &predicted, proof, &mut budget)
}

fn terminal(
    store: &Store,
    subject: &str,
    predicted: &BTreeMap<String, AssertionRow>,
    mut proof: String,
    budget: &mut ProofBudget,
) -> Result<ImportedTagAdmission> {
    let mut query=store.db.prepare("SELECT id,tag_id,source,confidence,note,active,created_at,updated_at FROM item_tags WHERE item_id=?")?;
    let mut rows = query.query([subject])?;
    let mut seen = BTreeSet::new();
    let mut vocabulary_proof = BTreeMap::new();
    while let Some(row) = rows.next()? {
        charge(budget, 0, 8)?;
        let id = text(row, 0, 256, budget)?;
        let tag = text(row, 1, 256, budget)?;
        let source = text(row, 2, 256, budget)?;
        if source != NAMESPACE {
            continue;
        }
        let confidence: Option<f64> = row.get(3)?;
        let note = text(row, 4, 4096, budget)?;
        let active: i64 = row.get(5)?;
        let created = text(row, 6, 64, budget)?;
        let updated = text(row, 7, 64, budget)?;
        charge(budget, 0, 3)?;
        let stamp:Option<(i64,bool)>=store.db.query_row("SELECT revision,deleted FROM catalog_entity_revisions WHERE entity_kind='item_tags' AND entity_key=json_array(?)",[&id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some(expected) = predicted.get(&id) else {
            return Ok(ImportedTagAdmission::ChangedAssertions);
        };
        if tag != expected.tag_id
            || confidence != expected.confidence
            || note != expected.note
            || active != i64::from(expected.active)
            || created != expected.created_at
            || updated != expected.updated_at
            || stamp != Some((expected.revision, false))
        {
            return Ok(ImportedTagAdmission::ChangedAssertions);
        }
        budget.serialized(&id, 64)?;
        seen.insert(id);
        charge(budget, 0, 7)?;
        let mut vocabulary=store.db.prepare("SELECT t.name,e.revision,e.deleted,n.tag_id,a.revision,a.deleted,EXISTS(SELECT 1 FROM catalog_tag_redirects r WHERE r.source_tag_id=t.id) FROM tags t LEFT JOIN catalog_entity_revisions e ON e.entity_kind='tags' AND e.entity_key=json_array(t.id) LEFT JOIN tag_names n ON n.name=t.name LEFT JOIN catalog_entity_revisions a ON a.entity_kind='tag_names' AND a.entity_key=json_array(n.name) WHERE t.id=?")?;
        let mut vocabulary = vocabulary.query([&tag])?;
        let Some(row) = vocabulary.next()? else {
            return Ok(ImportedTagAdmission::ChangedAssertions);
        };
        let canonical = row
            .get_ref(0)?
            .as_str()
            .map_err(|_| refused("canonical name not text"))?;
        if canonical.is_empty() || canonical.len() > 100 {
            return Ok(ImportedTagAdmission::ChangedAssertions);
        }
        let canonical = text(row, 0, 100, budget)?;
        let tag_revision: Option<i64> = row.get(1)?;
        let deleted: Option<bool> = row.get(2)?;
        let alias = row
            .get_ref(3)?
            .as_str()
            .map_err(|_| refused("canonical alias absent"))?;
        let alias_revision: Option<i64> = row.get(4)?;
        let alias_deleted: Option<bool> = row.get(5)?;
        let redirect: bool = row.get(6)?;
        if tag_revision.is_none_or(|n| n < 1)
            || deleted != Some(false)
            || alias != tag
            || alias_revision.is_none_or(|n| n < 1)
            || alias_deleted != Some(false)
            || redirect
        {
            return Ok(ImportedTagAdmission::ChangedAssertions);
        }
        budget.serialized(&(&tag, &canonical, tag_revision, alias_revision), 64)?;
        vocabulary_proof.insert(tag, (canonical, tag_revision, alias_revision));
    }
    if seen.len() != predicted.len() {
        return Ok(ImportedTagAdmission::ChangedAssertions);
    }
    proof = budget.serialized(&(proof, vocabulary_proof), 64)?;
    Ok(ImportedTagAdmission::Accounted { digest: proof })
}
