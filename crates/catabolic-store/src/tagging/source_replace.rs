//! Attributed source-tag replacement composed by the metadata v2 owner. The
//! outer dispatcher owns receipt, transaction and rollback; no owner opens one.
pub(crate) mod plan;
use super::*;
use crate::catalog_tag_mutation::{
    SuppressionChange, TagAuthority, TagCommandContext, TagMutation, TagSubject,
};
use catabolic_core::catalog_command::CommandEnvelope;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn replace_source(
    store: &Store,
    command: &CommandEnvelope,
    authority: &TagAuthority,
    subject: &str,
    namespace: &str,
    record_id: &str,
    revision: i64,
    tags: &[String],
    excluded: &[String],
    time: i64,
) -> Result<Value> {
    replace_source_authorized(
        store, command, authority, subject, namespace, record_id, revision, tags, excluded, time,
        None,
    )
}

pub(crate) fn replace_source_authorized(
    store: &Store,
    command: &CommandEnvelope,
    authority: &TagAuthority,
    subject: &str,
    namespace: &str,
    record_id: &str,
    revision: i64,
    tags: &[String],
    excluded: &[String],
    time: i64,
    nfo: Option<&crate::catalog_nfo_attribution::source_effect::NfoSourceEffect>,
) -> Result<Value> {
    if store.schema_version == 41 && namespace == "motion.nfo" {
        nfo.ok_or_else(|| Error("NFO source-tag capability required".into()))?
            .authorize_tags_in(store, command, subject, namespace, record_id, revision)?;
    }
    let db = &store.db;
    let tag_subject = TagSubject::Item(subject.into());
    if db.is_autocommit()
        || authority.host_namespace != command.key().host_namespace()
        || !authority.allowed_subjects.contains(&tag_subject)
        || tags.len() > 100
        || excluded.len() > 100
        || (!excluded.is_empty() && namespace != "motion.local")
    {
        return Err(Error(
            "source tag replacement authority/bounds mismatch".into(),
        ));
    }
    let normalize = |names: &[String]| -> Result<BTreeSet<String>> {
        let mut result = BTreeSet::new();
        for name in names {
            let canonical = tag_name(&json!(name))?;
            if name.is_empty()
                || name.len() > 100
                || canonical != *name
                || !result.insert(canonical)
            {
                return Err(Error("invalid or duplicate source tag".into()));
            }
        }
        Ok(result)
    };
    let tags = normalize(tags)?;
    let excluded = normalize(excluded)?;
    let mut statement = db.prepare("SELECT id,tag_id,active,note,confidence FROM item_tags WHERE item_id=? AND source=? AND active=1 ORDER BY id LIMIT 101")?;
    let old: Vec<(String, String, i64, String, Option<f64>)> = statement
        .query_map(params![subject, namespace], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<std::result::Result<_, _>>()?;
    if old.len() > 100 {
        return Err(Error(
            "retained source tags exceed bounded replacement".into(),
        ));
    }
    let mut decision_query = db.prepare("SELECT h.tag_id,h.revision,h.suppressed,o.revision FROM catalog_item_tag_suppressions s JOIN catalog_tag_suppression_revisions h ON h.subject_type='item' AND h.subject_id=s.item_id AND h.tag_id=s.tag_id AND h.revision=s.revision LEFT JOIN catalog_metadata_tag_suppressions o ON o.item_id=s.item_id AND o.tag_id=s.tag_id AND o.namespace='motion.local' WHERE s.item_id=? AND h.suppressed=1 ORDER BY h.tag_id LIMIT 101")?;
    let mut decisions: Vec<(String, i64, bool, Option<i64>)> = decision_query
        .query_map([subject], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<std::result::Result<_, _>>()?;
    if decisions.len() > 100 {
        return Err(Error("source suppression snapshot exceeds bound".into()));
    }
    let uuid = uuid::Uuid::parse_str(&store.database_id).map_err(|e| Error(e.to_string()))?;
    let stable = |value: Value| {
        uuid::Uuid::new_v5(
            &uuid,
            catabolic_core::encode_with(&value, false, true).as_bytes(),
        )
        .to_string()
    };
    let mut mapped = BTreeMap::new();
    for name in tags.union(&excluded) {
        let existing: Option<String> = db
            .query_row("SELECT tag_id FROM tag_names WHERE name=?", [name], |r| {
                r.get(0)
            })
            .optional()?;
        let id = match existing {
            Some(id) => id,
            None => {
                let id = stable(json!(["metadata-tag", name]));
                db.execute(
                    "INSERT INTO tags(id,name,description) VALUES (?,?,'')",
                    params![id, name],
                )?;
                db.execute(
                    "INSERT INTO tag_names(name,tag_id) VALUES (?,?)",
                    params![name, id],
                )?;
                id
            }
        };
        if db
            .prepare("SELECT 1 FROM catalog_tag_redirects WHERE source_tag_id=?")?
            .exists([&id])?
        {
            return Err(Error("source tag resolves to retired vocabulary".into()));
        }
        mapped.insert(name.clone(), id);
    }
    // Inactive historical heads do not impose a lifetime cardinality cap.
    // Revisit only requested exclusions, preserving their exact CAS/ownership.
    for name in &excluded {
        let id = &mapped[name];
        if decisions.iter().any(|row| &row.0 == id) {
            continue;
        }
        let prior: Option<(String,i64,bool,Option<i64>)> = db.query_row("SELECT h.tag_id,h.revision,h.suppressed,o.revision FROM catalog_item_tag_suppressions s JOIN catalog_tag_suppression_revisions h ON h.subject_type='item' AND h.subject_id=s.item_id AND h.tag_id=s.tag_id AND h.revision=s.revision LEFT JOIN catalog_metadata_tag_suppressions o ON o.item_id=s.item_id AND o.tag_id=s.tag_id AND o.namespace='motion.local' WHERE s.item_id=? AND s.tag_id=?",params![subject,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        if let Some(prior) = prior {
            decisions.push(prior);
        }
    }
    store
        .catalog_operation_budget()?
        .charge(crate::operation_budget::BudgetCharge {
            bytes: old
                .iter()
                .map(|row| row.0.len() + row.1.len() + row.3.len())
                .sum::<usize>()
                + tags
                    .iter()
                    .chain(excluded.iter())
                    .map(String::len)
                    .sum::<usize>(),
            values: old.len() + decisions.len() + tags.len() + excluded.len(),
            work: (old.len() + decisions.len() + tags.len() + excluded.len()) * 8 + 1,
            ..Default::default()
        })
        .map_err(|_| Error("source tag replacement operation budget exhausted".into()))?;
    let before = json!({"assertions":old,"suppressions":decisions});
    if before.to_string().len() > 65536 {
        return Err(Error("source tag history exceeds byte budget".into()));
    }
    let timestamp = chrono::DateTime::from_timestamp_micros(time)
        .ok_or_else(|| Error("invalid source tag time".into()))?
        .to_rfc3339();
    let note = json!({"metadata_source_revision":revision,"command_operation_id":command.key().operation_id()}).to_string();
    // Resolve the same requested existing rows before mutation. This adds no
    // lifetime inactive-row scan: only the at-most100 requested vocabulary IDs.
    store
        .catalog_operation_budget()?
        .charge(crate::operation_budget::BudgetCharge {
            values: old.len() * 3 + tags.len() * 8,
            work: old.len() * 4 + tags.len() * 10 + 1,
            ..Default::default()
        })
        .map_err(|_| Error("source assertion plan operation budget exhausted".into()))?;
    let mut existing = BTreeMap::new();
    let mut insert_ids = BTreeMap::new();
    for name in &tags {
        let id = &mapped[name];
        if existing.contains_key(id) {
            continue;
        }
        let mut lookup =
            db.prepare("SELECT id FROM item_tags WHERE item_id=? AND source=? AND tag_id=?")?;
        let mut lookup = lookup.query(params![subject, namespace, id])?;
        let assertion = if let Some(row) = lookup.next()? {
            let raw = row
                .get_ref(0)?
                .as_str()
                .map_err(|_| Error("source assertion identity is not text".into()))?;
            store
                .catalog_operation_budget()?
                .charge(crate::operation_budget::BudgetCharge {
                    bytes: raw.len(),
                    values: 1,
                    work: raw.len() / 64 + 1,
                    ..Default::default()
                })
                .map_err(|_| Error("source assertion lookup budget exhausted".into()))?;
            Some(raw.to_owned())
        } else {
            None
        };
        if assertion.is_some() {
            store
                .catalog_operation_budget()?
                .charge(crate::operation_budget::BudgetCharge {
                    bytes: id.len(),
                    values: 1,
                    work: 2,
                    ..Default::default()
                })
                .map_err(|_| Error("source assertion identity budget exhausted".into()))?;
        } else {
            // Reserve the unchanged UUID helper's JSON/canonical representations
            // only for genuinely new rows, as the original owner did.
            store
                .catalog_operation_budget()?
                .charge(crate::operation_budget::BudgetCharge {
                    bytes: (subject.len() + id.len() + namespace.len()) * 16 + id.len() * 2 + 256,
                    values: 12,
                    work: (subject.len() + id.len() + namespace.len()) / 4 + 16,
                    ..Default::default()
                })
                .map_err(|_| Error("source assertion identity budget exhausted".into()))?;
            insert_ids.insert(
                id.clone(),
                plan::assertion_id(&uuid, subject, id, namespace),
            );
        }
        existing.insert(id.clone(), assertion);
    }
    let active: Vec<_> = old
        .iter()
        .map(|row| plan::Active {
            id: &row.0,
            tag_id: &row.1,
        })
        .collect();
    let requested: Vec<_> = tags
        .iter()
        .map(|name| {
            let id = &mapped[name];
            plan::Requested {
                name,
                tag_id: id,
                existing: existing[id].as_deref(),
                insert_id: existing[id]
                    .as_deref()
                    .unwrap_or_else(|| insert_ids[id].as_str()),
            }
        })
        .collect();
    let assertion_plan = plan::plan(&active, &requested)?;
    // Preserve the exact bulk-deactivation and per-request write sequence. The
    // plan retains alias-repeat no-ops instead of changing qualified40 effects.
    db.execute(
        "UPDATE item_tags SET active=0,updated_at=? WHERE item_id=? AND source=? AND active=1",
        params![timestamp, subject, namespace],
    )?;
    for write in assertion_plan.writes {
        match write {
            plan::Write::Reactivate { id: assertion, .. } => {
                db.execute(
                    "UPDATE item_tags SET active=1,note=?,updated_at=? WHERE id=?",
                    params![note, timestamp, assertion],
                )?;
            }
            plan::Write::Insert {
                id: assertion,
                tag_id: id,
            } => {
                db.execute("INSERT INTO item_tags(id,item_id,tag_id,source,confidence,note,active,created_at,updated_at) VALUES (?,?,?,?,NULL,?,1,?,?)",params![assertion,subject,id,namespace,note,timestamp,timestamp])?;
            }
        }
    }
    if namespace == "motion.local" {
        let wanted: BTreeSet<_> = excluded.iter().map(|name| mapped[name].clone()).collect();
        let mut changes = Vec::new();
        for (id, current, suppressed, owned) in &decisions {
            let desired = wanted.contains(id);
            if desired == *suppressed {
                if desired && *owned != Some(*current) {
                    return Err(Error(
                        "local source cannot claim another suppression owner".into(),
                    ));
                }
                continue;
            }
            if *owned != Some(*current) {
                return Err(Error(
                    "local source cannot overwrite another suppression owner".into(),
                ));
            }
            changes.push(SuppressionChange {
                subject: tag_subject.clone(),
                canonical_tag_id: id.clone(),
                expected_revision: Some(*current),
                suppressed: desired,
                reason: note.clone(),
            });
        }
        for id in wanted {
            if decisions.iter().any(|row| row.0 == id) {
                continue;
            }
            changes.push(SuppressionChange {
                subject: tag_subject.clone(),
                canonical_tag_id: id,
                expected_revision: None,
                suppressed: true,
                reason: note.clone(),
            });
        }
        // The established suppression owner enforces exact revision CAS.
        for chunk in changes.chunks(crate::catalog_tag_mutation::MAX_SUPPRESSIONS) {
            let generation =
                db.query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                    r.get(0)
                })?;
            crate::catalog_tag_mutation::apply(
                db,
                &TagCommandContext {
                    catalog_id: command.key().catalog_id(),
                    host_namespace: command.key().host_namespace(),
                    operation_id: command.key().operation_id(),
                    expected_generation: generation,
                    expected_incarnation: command.admission_incarnation(),
                    recorded_at_us: time,
                },
                authority,
                &TagMutation::SetSuppression {
                    contract_version: 1,
                    changes: chunk.to_vec(),
                },
            )
            .map_err(|e| e.error)?;
            for change in chunk {
                let current: i64=db.query_row("SELECT revision FROM catalog_item_tag_suppressions WHERE item_id=? AND tag_id=?",params![subject,change.canonical_tag_id],|r|r.get(0))?;
                db.execute("INSERT INTO catalog_metadata_tag_suppressions(item_id,tag_id,namespace,revision) VALUES (?,?,'motion.local',?) ON CONFLICT(item_id,tag_id) DO UPDATE SET revision=excluded.revision",params![subject,change.canonical_tag_id,current])?;
            }
        }
    }
    let after = json!({"tags":tags,"excluded_tags":excluded});
    db.execute("INSERT INTO catalog_metadata_tag_history(item_id,namespace,record_id,revision,before_json,after_json,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us) VALUES (?,?,?,?,?,?,?,?,?,?)",params![subject,namespace,record_id,revision,before.to_string(),after.to_string(),command.key().catalog_id(),command.key().host_namespace(),command.key().operation_id(),time])?;
    Ok(after)
}
