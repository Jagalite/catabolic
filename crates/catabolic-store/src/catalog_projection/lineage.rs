//! Current named structural lineage. This is a bounded observation, never an
//! application ACL, occurrence grant, or playable-version proof.
use super::*;
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct VersionLineageRequest {
    pub work_item_id: String,
    pub edition_item_id: String,
    pub version_item_id: String,
    pub version_id: String,
}

pub fn version_lineage(
    store: &Store,
    authority: &TrustedProjectionAuthority,
    requests: &[VersionLineageRequest],
) -> Result<Value> {
    if !matches!(store.schema_version, 39 | 40)
        || !store.db.is_autocommit()
        || requests.len() > 1000
        || requests.iter().collect::<BTreeSet<_>>().len() != requests.len()
        || requests
            .iter()
            .flat_map(|r| {
                [
                    &r.work_item_id,
                    &r.edition_item_id,
                    &r.version_item_id,
                    &r.version_id,
                ]
            })
            .any(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
    {
        return Err(Error("invalid bounded version lineage request".into()));
    }
    store.transaction_available()?;
    store.transaction(|_| capture_in(store, authority, requests))
}
/// Candidate-only retained-writer structural read. This preserves the public
/// wrapper's complete request bounds without opening a nested transaction or
/// widening its supported 39/40 schemas. The result is still descriptive only.
pub(crate) fn version_lineage_in(
    store: &Store,
    authority: &TrustedProjectionAuthority,
    requests: &[VersionLineageRequest],
) -> Result<Value> {
    if store.schema_version != 41
        || !store.writable
        || store.db.is_autocommit()
        || requests.len() > 1000
        || requests.iter().collect::<BTreeSet<_>>().len() != requests.len()
        || requests
            .iter()
            .flat_map(|r| {
                [
                    &r.work_item_id,
                    &r.edition_item_id,
                    &r.version_item_id,
                    &r.version_id,
                ]
            })
            .any(|id| id.is_empty() || id.len() > 256 || id.chars().any(char::is_control))
    {
        return Err(Error("invalid retained version lineage request".into()));
    }
    store.transaction_available()?;
    capture_in(store, authority, requests)
}

pub(super) fn capture_in(
    store: &Store,
    authority: &TrustedProjectionAuthority,
    requests: &[VersionLineageRequest],
) -> Result<Value> {
    if store.db.is_autocommit() {
        return Err(Error("lineage capture requires retained snapshot".into()));
    }
    let db = &store.db;

    let budget = store.catalog_operation_budget()?;
    let (catalog, incarnation, generation): (String, String, i64) = db.query_row(
        "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if catalog != authority.catalog_id
        || incarnation != authority.incarnation
        || generation != authority.expected_generation
    {
        return Err(Error(
            "version lineage identity or generation changed".into(),
        ));
    }
    let mut rows = Vec::new();
    let mut total_rows = 0usize;
    let mut total_bytes = 0usize;
    for request in requests {
        budget
            .check()
            .map_err(|e| Error(format!("lineage operation budget: {e:?}")))?;
        let ids = json!([
            request.work_item_id,
            request.edition_item_id,
            request.version_item_id
        ])
        .to_string();
        let owners = crate::migration::rows(
            db,
            "SELECT i.id,i.kind,e.revision AS item_revision,EXISTS(SELECT 1 FROM catalog_logical_retirements x WHERE x.item_id=i.id) AS retired FROM items i JOIN catalog_entity_revisions e ON e.entity_kind='items' AND e.entity_key=json_array(i.id) AND e.deleted=0 WHERE i.id IN(SELECT value FROM json_each(?)) ORDER BY i.id",
            &[ids.into()],
        )?;
        if owners.len() != 3 {
            return Err(Error(
                "named lineage owner missing or aliased identities".into(),
            ));
        }
        for (id, kinds) in [
            (&request.work_item_id, &["video", "custom:motion_audio"][..]),
            (&request.edition_item_id, &["custom:motion_edition"][..]),
            (&request.version_item_id, &["custom:motion_version"][..]),
        ] {
            if !owners.iter().any(|o| {
                o["id"] == *id
                    && o["kind"].as_str().is_some_and(|k| kinds.contains(&k))
                    && o["item_revision"].as_i64().is_some_and(|v| v > 0)
            }) {
                return Err(Error("named lineage owner kind/head mismatch".into()));
            }
        }
        let versions = crate::migration::rows(
            db,
            "SELECT v.id,v.association_item_id,v.current_revision,r.timeline_id,r.timeline_revision,t.edition_item_id,t.current_revision AS current_timeline_revision,EXISTS(SELECT 1 FROM catalog_version_retirements x WHERE x.version_id=v.id) AS retired FROM catalog_versions v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision JOIN catalog_timelines t ON t.id=r.timeline_id JOIN catalog_timeline_revisions tr ON tr.timeline_id=t.id AND tr.revision=t.current_revision WHERE v.id=?",
            &[request.version_id.clone().into()],
        )?;
        if versions.len() != 1 {
            return Err(Error("named lineage typed current head missing".into()));
        }
        let typed = &versions[0];
        let version_head = typed["current_revision"]
            .as_i64()
            .filter(|v| *v > 0)
            .ok_or_else(|| Error("lineage version revision invalid".into()))?;
        let timeline = typed["timeline_id"]
            .as_str()
            .ok_or_else(|| Error("lineage timeline identity invalid".into()))?;
        let timeline_head = typed["current_timeline_revision"]
            .as_i64()
            .filter(|v| *v > 0)
            .ok_or_else(|| Error("lineage timeline revision invalid".into()))?;
        let invalidation_heads:i64=db.query_row("SELECT count(*) FROM catalog_entity_revisions WHERE deleted=0 AND revision>0 AND ((entity_kind='catalog_versions' AND entity_key=json_array(?1)) OR (entity_kind='catalog_version_revisions' AND entity_key=json_array(?1,?2)) OR (entity_kind='catalog_timelines' AND entity_key=json_array(?3)) OR (entity_kind='catalog_timeline_revisions' AND entity_key=json_array(?3,?4)))",params![request.version_id,version_head,timeline,timeline_head],|r|r.get(0))?;
        if invalidation_heads != 4 {
            return Err(Error("lineage typed invalidation head missing".into()));
        }

        let associations: i64 = db.query_row(
            "SELECT count(*) FROM catalog_versions WHERE association_item_id=?",
            [&request.version_item_id],
            |r| r.get(0),
        )?;
        let edges = crate::migration::rows(
            db,
            "SELECT e.id,e.source_id,e.target_id,e.kind,e.active,r.revision AS edge_revision FROM item_relationships e LEFT JOIN catalog_entity_revisions r ON r.entity_kind='item_relationships' AND r.entity_key=json_array(e.id) AND r.deleted=0 WHERE e.active=1 AND ((e.target_id=?1 AND e.kind='custom:motion_edition') OR (e.target_id=?2 AND e.kind='custom:motion_version')) ORDER BY e.id LIMIT 1001",
            &[
                request.edition_item_id.clone().into(),
                request.version_item_id.clone().into(),
            ],
        )?;
        if edges.len() > 1000
            || edges
                .iter()
                .any(|e| e["edge_revision"].as_i64().is_none_or(|v| v < 1))
        {
            return Err(Error(
                "lineage edge inventory exceeds bound or lacks current head".into(),
            ));
        }
        let edition_parents: Vec<_> = edges
            .iter()
            .filter(|e| e["kind"] == "custom:motion_edition")
            .collect();
        let version_parents: Vec<_> = edges
            .iter()
            .filter(|e| e["kind"] == "custom:motion_version")
            .collect();
        let retired = typed["retired"] == 1 || owners.iter().any(|o| o["retired"] == 1);
        let ambiguous = edition_parents.len() > 1 || version_parents.len() > 1 || associations > 1;
        let active = typed["association_item_id"] == request.version_item_id
            && typed["edition_item_id"] == request.edition_item_id
            && typed["timeline_revision"] == typed["current_timeline_revision"]
            && edition_parents.len() == 1
            && edition_parents[0]["source_id"] == request.work_item_id
            && version_parents.len() == 1
            && version_parents[0]["source_id"] == request.edition_item_id;
        let status = if retired {
            "retired"
        } else if ambiguous {
            "ambiguous"
        } else if active {
            "active"
        } else {
            "detached"
        };
        let row = json!({"work_item_id":request.work_item_id,"edition_item_id":request.edition_item_id,"version_item_id":request.version_item_id,"version_id":request.version_id,"status":status,"owners":owners,"typed_version":typed,"active_parent_edges":edges});
        total_rows = total_rows
            .checked_add(4 + edges.len())
            .ok_or_else(|| Error("lineage row overflow".into()))?;
        total_bytes = total_bytes
            .checked_add(serde_json::to_vec(&row)?.len())
            .ok_or_else(|| Error("lineage byte overflow".into()))?;
        if total_rows > 10000 || total_bytes > 4 * 1024 * 1024 {
            return Err(Error("complete lineage capture exceeds bound".into()));
        }
        rows.push(row);
    }
    budget
        .check()
        .map_err(|e| Error(format!("lineage operation budget: {e:?}")))?;
    Ok(
        json!({"contract_version":1,"catalog_id":catalog,"incarnation":incarnation,"generation":generation,"rows":rows,"descriptive_only":true,"playable":false}),
    )
}
