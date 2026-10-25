//! Fixed component-assertion transfer preparation. No public row injection or
//! early current-view visibility; the sole source owner publishes the receipt.
use super::*;
use crate::{
    catalog_identity_mutation::StructuralReadBudget, catalog_source_succession::VerifiedContent,
    operation_budget::BudgetCharge,
};
use catabolic_core::catalog_command::CommandEnvelope;

pub(crate) struct PreparedAssertionTransfer {
    envelope: CommandEnvelope,
    profile: String,
    source: String,
    succession_revision: i64,
    old: Value,
    assertion_heads: Vec<(String, i64, i64)>,
    row: Value,
    association: Value,
    item: Value,
    renewed: Value,
    occurrence: Value,
}
fn one(
    store: &Store,
    table: &'static str,
    id: &str,
    budget: &mut impl StructuralReadBudget,
) -> Result<Value> {
    // Only fixed internal owner tables reach this helper; charge serialized
    // payloads before materialization, including all variable text columns.
    let columns: Vec<String> = store
        .db
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get(1))?
        .collect::<std::result::Result<_, _>>()?;
    let lengths = columns
        .iter()
        .map(|c| format!("coalesce(length(CAST(\"{c}\" AS BLOB)),0)"))
        .collect::<Vec<_>>()
        .join("+");
    let bytes: i64 = store.db.query_row(
        &format!("SELECT 256+{lengths} FROM {table} WHERE id=?"),
        [id],
        |r| r.get(0),
    )?;
    if !(0..=8 * 1024 * 1024).contains(&bytes) {
        return Err(Error("component transfer row exceeds bound".into()));
    }
    budget.charge(BudgetCharge {
        bytes: bytes as usize,
        items: 1,
        values: columns.len(),
        work: 1,
    })?;
    rows(
        &store.db,
        &format!("SELECT * FROM {table} WHERE id=?"),
        &[id.to_owned().into()],
    )?
    .pop()
    .ok_or_else(|| Error("component transfer row absent".into()))
}
fn assertion_heads(
    store: &Store,
    profile: &str,
    file: &str,
    budget: &mut impl StructuralReadBudget,
) -> Result<Vec<(String, i64, i64)>> {
    let from = "component_assertions a JOIN component_occurrences o ON o.id=a.occurrence_id JOIN catalog_entity_revisions h ON h.entity_kind='component_assertions' AND h.entity_key=json_array(a.id) WHERE o.profile=? AND o.file_id=?";
    let (count, bytes): (i64, i64) = store.db.query_row(
        &format!("SELECT count(*),coalesce(sum(128+length(CAST(a.id AS BLOB))),0) FROM {from}"),
        params![profile, file],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if !(0..=16384).contains(&count) || !(0..=8 * 1024 * 1024).contains(&bytes) {
        return Err(Error("component assertion set exceeds bound".into()));
    }
    budget.charge(BudgetCharge {
        bytes: bytes as usize,
        items: count as usize,
        values: count as usize * 3,
        work: count as usize,
    })?;
    Ok(store
        .db
        .prepare(&format!(
            "SELECT a.id,h.revision,h.deleted FROM {from} ORDER BY a.id"
        ))?
        .query_map(params![profile, file], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<std::result::Result<_, _>>()?)
}
fn retained(store: &Store, envelope: &CommandEnvelope) -> Result<()> {
    if store.schema_version != 41
        || store.db.is_autocommit()
        || store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
        || envelope.key().catalog_id() != store.database_id
        || Some(envelope.admission_incarnation()) != store.catalog_incarnation.as_deref()
    {
        return Err(Error("retained component succession owner differs".into()));
    }
    Ok(())
}
/// Called only before physical renewal. Expected revisions derive exclusively
/// from the independently held, hashed complete source candidate capability.
pub(crate) fn prepare_assertion_transfer(
    store: &Store,
    envelope: &CommandEnvelope,
    profile: &str,
    assertion: &str,
    content: &VerifiedContent,
    budget: &mut impl StructuralReadBudget,
) -> Result<PreparedAssertionTransfer> {
    retained(store, envelope)?;
    let generation: i64 =
        store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })?;
    if generation != envelope.expected_generation() {
        return Err(Error("component preparation generation differs".into()));
    }
    let old = one(store, "component_assertions", assertion, budget)?;
    if old["active"] != 1 {
        return Err(Error("inactive assertion cannot transfer".into()));
    }
    let original = one(
        store,
        "component_occurrences",
        text(&old, "occurrence_id")?,
        budget,
    )?;
    let file = text(&original, "file_id")?;
    let (old_snapshot, next_snapshot) = content
        .replacement(file)
        .ok_or_else(|| Error("assertion outside complete candidate".into()))?;
    if decode_json(text(&original, "snapshot")?)? != *old_snapshot {
        // Native component snapshots intentionally omit ctime; compare revisions
        // and all existing snapshot fields rather than minting a new hash claim.
        if components::revision(&decode_json(text(&original, "snapshot")?)?)
            != components::revision(old_snapshot)
        {
            return Err(Error("assertion old physical revision differs".into()));
        }
    }
    let source = text(old_snapshot, "location")?.to_owned();
    let succession_revision: i64 = store.db.query_row("SELECT coalesce(max(revision),0)+1 FROM catalog_source_successions WHERE profile=? AND source_id=?",params![profile,source],|r|r.get(0))?;
    let assertion_heads = assertion_heads(store, profile, file, budget)?;
    let row = get(store, profile, text(&old, "occurrence_id")?, true)?;
    let raw = decode_json(text(&old, "payload")?)?;
    let (mut claim, _, _) =
        validate_claim(store, profile, file, text(&original, "item_id")?, &raw)?;
    let association = one(
        store,
        "item_files",
        text(&original, "association_id")?,
        budget,
    )?;
    let item = one(store, "items", text(&original, "item_id")?, budget)?;
    let id = uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        encode(&json!([
            "source_succession_occurrence",
            envelope.key().catalog_id(),
            envelope.key().host_namespace(),
            envelope.key().operation_id(),
            envelope.digest().as_str(),
            original["id"]
        ]))
        .as_bytes(),
    )
    .to_string();
    let mut occurrence = original;
    occurrence["id"] = json!(id);
    occurrence["revision"] = json!(components::revision(next_snapshot));
    let mut snapshot = next_snapshot.clone();
    snapshot
        .as_object_mut()
        .ok_or_else(|| Error("candidate snapshot shape".into()))?
        .remove("ctime_ns");
    occurrence["snapshot"] = json!(encode(&snapshot));
    claim["occurrence_id"] = json!(id);
    for dependency in claim["dependencies"]
        .as_array_mut()
        .ok_or_else(|| Error("assertion dependency shape".into()))?
    {
        if let Some((old, next)) = content.replacement(text(dependency, "file_id")?) {
            if dependency["revision"] != components::revision(old) {
                return Err(Error("assertion dependency changed".into()));
            }
            dependency["revision"] = json!(components::revision(next));
        }
    }
    if let Some(compatibility) = claim.get_mut("compatibility") {
        if let Some((old, next)) = content.replacement(text(compatibility, "video_file_id")?) {
            if compatibility["video_revision"] != components::revision(old) {
                return Err(Error("assertion compatibility changed".into()));
            }
            compatibility["video_revision"] = json!(components::revision(next));
        }
    }
    let mut renewed = old.clone();
    renewed["occurrence_id"] = json!(id);
    renewed["payload"] = json!(encode(&claim));
    Ok(PreparedAssertionTransfer {
        envelope: envelope.clone(),
        profile: profile.into(),
        source,
        succession_revision,
        old,
        assertion_heads,
        row,
        association,
        item,
        renewed,
        occurrence,
    })
}
impl PreparedAssertionTransfer {
    /// Only the fixed source owner may insert this exact planned row. This is
    /// not a current query grant and preserves old job IDs as historical origin.
    pub(crate) fn occurrence(&self) -> &Value {
        &self.occurrence
    }
    pub(crate) fn finish(
        self,
        store: &Store,
        envelope: &CommandEnvelope,
        budget: &mut impl StructuralReadBudget,
    ) -> Result<(Value, Value)> {
        retained(store, envelope)?;
        if assertion_heads(
            store,
            &self.profile,
            text(&self.occurrence, "file_id")?,
            budget,
        )? != self.assertion_heads
        {
            return Err(Error("complete component assertion set changed".into()));
        }
        if *envelope != self.envelope
            || one(
                store,
                "component_assertions",
                text(&self.old, "id")?,
                budget,
            )? != self.old
            || one(
                store,
                "component_occurrences",
                text(&self.occurrence, "id")?,
                budget,
            )? != self.occurrence
            || one(store, "item_files", text(&self.association, "id")?, budget)? != self.association
            || one(store, "items", text(&self.item, "id")?, budget)? != self.item
        {
            return Err(Error("component succession post-state differs".into()));
        }
        if !store.db.prepare("SELECT 1 FROM catalog_source_succession_permits p JOIN observations o ON o.profile=p.profile AND o.succession_source_id=p.source_id AND o.succession_revision=p.succession_revision WHERE p.profile=? AND p.source_id=? AND p.succession_revision=? AND p.incarnation=? AND o.file_id=? AND o.observation_origin='succession' AND o.scan_id IS NULL")?.exists(params![self.profile,self.source,self.succession_revision,envelope.admission_incarnation(),text(&self.occurrence,"file_id")?])?
            || components::occurrence(&store.db,&self.profile,text(&self.occurrence,"file_id")?)? != decode_json(text(&self.occurrence,"snapshot")?)? {
            return Err(Error("component renewal physical owner differs".into()));
        }
        let claim = decode_json(text(&self.renewed, "payload")?)?;
        // Supersedes remains historical assertion lineage on the old occurrence.
        // All association/kind/compatibility/dependency validators still execute.
        validate_claim_row(
            store,
            &self.profile,
            text(&self.occurrence, "file_id")?,
            text(&self.occurrence, "item_id")?,
            claim,
            self.row,
        )?;
        Ok((self.old, self.renewed))
    }
}
