//! Exact bounded named facts for privileged host policy validation, not an ACL.
use super::*;
use rusqlite::OptionalExtension;
use std::collections::BTreeSet;

pub fn named_items(
    store: &Store,
    authority: &TrustedProjectionAuthority,
    ids: &[String],
) -> Result<Value> {
    const MAX_IDS: usize = 200;
    const MAX_BYTES: usize = 4 * 1024 * 1024;
    if store.schema_version < 34
        || ids.len() > MAX_IDS
        || ids
            .iter()
            .any(|id| id.is_empty() || id.len() > 256 || id.chars().any(char::is_control))
        || ids.iter().collect::<BTreeSet<_>>().len() != ids.len()
        || !store.db.is_autocommit()
    {
        return Err(Error(
            "invalid named projection request or ambient transaction".into(),
        ));
    }
    store.transaction_available()?;
    store.transaction(|db| {
        let (catalog, incarnation, generation): (String,String,i64) = db.query_row(
            "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1", [],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        if catalog != authority.catalog_id || incarnation != authority.incarnation || generation != authority.expected_generation {
            return Err(Error("named projection identity or generation changed".into()));
        }
        let mut items=Vec::new(); let mut missing=Vec::new(); let mut bytes=0usize;
        for id in ids.iter().collect::<BTreeSet<_>>() {
            // Check SQLite byte lengths before allocating any caller-owned JSON.
            let lengths: Option<(i64,i64,i64)> = db.query_row(
                "SELECT length(CAST(i.metadata AS BLOB)),length(CAST(i.kind AS BLOB)),coalesce(length(CAST(l.children_json AS BLOB)),0) FROM items i LEFT JOIN catalog_logical_retirements l ON l.item_id=i.id WHERE i.id=?",
                [id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let Some((metadata_bytes,kind_bytes,children_bytes)) = lengths else { missing.push(id.clone()); continue };
            for size in [metadata_bytes,kind_bytes,children_bytes] {
                let size:usize = size.try_into().map_err(|_|Error("invalid named projection length".into()))?;
                bytes=bytes.checked_add(size).ok_or_else(||Error("named projection byte overflow".into()))?;
                if bytes>MAX_BYTES { return Err(Error("named projection byte budget exceeded".into())); }
            }
            let (kind,metadata,revision,retirement_kind,survivor,children):(String,String,Option<i64>,Option<String>,Option<String>,Option<String>) = db.query_row(
                "SELECT i.kind,i.metadata,r.revision,l.kind,l.survivor_item_id,l.children_json FROM items i LEFT JOIN catalog_entity_revisions r ON r.entity_kind='items' AND r.entity_key=json_array(i.id) AND r.deleted=0 LEFT JOIN catalog_logical_retirements l ON l.item_id=i.id WHERE i.id=?",
                [id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
            let metadata:Value=serde_json::from_str(&metadata)?;
            if !metadata.is_object() { return Err(Error("named projection metadata is not an object".into())); }
            let revision=revision.filter(|value|*value>0).ok_or_else(||Error("named projection item head missing".into()))?;
            let retirement=match retirement_kind {
                None=>Value::Null,
                Some(kind)=>json!({"kind":kind,"survivor_item_id":survivor,"children":serde_json::from_str::<Value>(&children.ok_or_else(||Error("named retirement children missing".into()))?)?}),
            };
            items.push(json!({"item_id":id,"kind":kind,"metadata":metadata,"item_revision":revision,"retirement":retirement}));
        }
        let result=json!({"catalog_id":catalog,"incarnation":incarnation,"generation":generation,"items":items,"missing_ids":missing});
        if serde_json::to_vec(&result)?.len()>MAX_BYTES { return Err(Error("named projection response budget exceeded".into())); }
        Ok(result)
    })
}
