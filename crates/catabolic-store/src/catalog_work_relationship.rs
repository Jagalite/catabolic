// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Pure application-wire shape checks. These do not authorize an edge or mint
//! a capture; callers must read actual native rows and authorize both endpoints.
use crate::{Error, Result, Store, catalog_projection::TrustedProjectionAuthority};
use rusqlite::params;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub const MAX_RELATIONSHIP_READ_ROWS: usize = 1000;
pub const MAX_RELATIONSHIP_READ_BYTES: usize = 4 * 1024 * 1024;

/// Trusted, snapshot-pinned native projection, not an endpoint permission grant.
/// Every incident reserved-domain row is returned, including inactive rows.
/// The server must independently authorize BOTH endpoints before exposing rows.
/// There is no truncation, cursor, recursive traversal, or caller-supplied SQL.
pub fn read_incident(
    store: &Store,
    authority: &TrustedProjectionAuthority,
    work_id: &str,
    limit: usize,
) -> Result<Value> {
    read_projection(store, authority, Some(work_id), None, limit)
}

/// Read one exact native ID. Missing and other-domain IDs produce an empty
/// relationships array, never a synthetic relationship or permission grant.
pub fn read_exact(
    store: &Store,
    authority: &TrustedProjectionAuthority,
    relationship_id: &str,
) -> Result<Value> {
    read_projection(store, authority, None, Some(relationship_id), 1)
}

fn read_projection(
    store: &Store,
    authority: &TrustedProjectionAuthority,
    work_id: Option<&str>,
    relationship_id: Option<&str>,
    limit: usize,
) -> Result<Value> {
    let invalid = || Error("invalid Motion relationship projection request".into());
    if store.schema_version < 34
        || !store.db.is_autocommit()
        || !(1..=MAX_RELATIONSHIP_READ_ROWS).contains(&limit)
        || authority.expected_generation < 0
        || work_id
            .into_iter()
            .chain(relationship_id)
            .any(|id| !catabolic_core::catalog_relationship::valid_identifier(id))
    {
        return Err(invalid());
    }
    store.transaction_available()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    let check = || {
        if Instant::now() >= deadline || crate::embedding::interrupt_requested() {
            return Err(Error(
                "Motion relationship projection deadline/cancellation".into(),
            ));
        }
        Ok(())
    };
    store.transaction(|db| {
        check()?;
        let (catalog, incarnation, generation): (String, String, i64) = db.query_row(
            "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
            [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        )?;
        if catalog != authority.catalog_id || incarnation != authority.incarnation || generation != authority.expected_generation {
            return Err(Error("Motion relationship projection identity changed".into()));
        }
        let mut statement = db.prepare(
            "SELECT e.id,e.source_id,e.target_id,e.kind,e.position,e.metadata,e.active,r.revision \
             FROM item_relationships e LEFT JOIN catalog_entity_revisions r \
             ON r.entity_kind='item_relationships' AND r.entity_key=json_array(e.id) AND r.deleted=0 \
             WHERE ((?1 IS NOT NULL AND (e.source_id=?1 OR e.target_id=?1)) OR (?2 IS NOT NULL AND e.id=?2)) \
             AND e.kind IN ('custom:motion_part_of','custom:motion_edition_of','custom:motion_performed_by','custom:motion_created_by','custom:motion_extra_of','custom:motion_derived_from') \
             ORDER BY e.id COLLATE BINARY LIMIT ?3",
        )?;
        let mut rows = statement.query(params![work_id,relationship_id,(limit+1) as i64])?;
        let mut relationships = Vec::new();
        let mut bytes = 0usize;
        while let Some(row) = rows.next()? {
            check()?;
            if relationships.len() >= limit {
                return Err(Error("Motion relationship projection row budget exceeded".into()));
            }
            // Charge SQLite-owned text before allocating any row JSON.
            for i in [0,1,2,3,5] {
                let text = row.get_ref(i)?.as_str().map_err(|_| invalid())?;
                bytes = bytes.saturating_add(text.len()).saturating_add(32);
                if bytes > MAX_RELATIONSHIP_READ_BYTES {
                    return Err(Error("Motion relationship projection byte budget exceeded".into()));
                }
            }
            let revision = row.get::<_,Option<i64>>(7)?.filter(|value| *value > 0)
                .ok_or_else(|| Error("Motion relationship projection native revision unavailable".into()))?;
            let native = json!({
                "id":row.get::<_,String>(0)?,"source_id":row.get::<_,String>(1)?,
                "target_id":row.get::<_,String>(2)?,"kind":row.get::<_,String>(3)?,
                "position":row.get::<_,Option<i64>>(4)?,"metadata":row.get::<_,String>(5)?,
                "active":row.get::<_,i64>(6)?,
            });
            let mut decoded = decode_record(&native)?.ok_or_else(invalid)?;
            decoded["native_kind"] = native["kind"].clone();
            decoded["revision"] = json!(revision);
            bytes = bytes.saturating_add(serde_json::to_vec(&decoded)?.len());
            if bytes > MAX_RELATIONSHIP_READ_BYTES {
                return Err(Error("Motion relationship projection byte budget exceeded".into()));
            }
            relationships.push(decoded);
        }
        // EOF within limit is completeness, never a silently partial page.
        check()?;
        Ok(json!({"catalog_id":catalog,"incarnation":incarnation,"generation":generation,"relationships":relationships}))
    })
}

pub fn public_kind(native: &str) -> Option<&'static str> {
    match native {
        "custom:motion_part_of" => Some("part_of"),
        "custom:motion_edition_of" => Some("edition_of"),
        "custom:motion_performed_by" => Some("performed_by"),
        "custom:motion_created_by" => Some("created_by"),
        "custom:motion_extra_of" => Some("extra_of"),
        "custom:motion_derived_from" => Some("derived_from"),
        _ => None,
    }
}
/// None means a different relationship domain, not an empty authorized edge.
/// Invalid records in our reserved domain are errors, including inactive ones.
pub fn decode_record(row: &Value) -> Result<Option<Value>> {
    let Some(kind) = row["kind"].as_str().and_then(public_kind) else {
        return Ok(None);
    };
    let invalid = || Error("malformed Motion work relationship projection".into());
    let id = row["id"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(invalid)?;
    let source = row["source_id"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(invalid)?;
    let target = row["target_id"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(invalid)?;
    if source == target
        || !row["position"].is_null()
        || !matches!(row["active"].as_i64(), Some(0 | 1))
    {
        return Err(invalid());
    }
    let metadata: Value = serde_json::from_str(row["metadata"].as_str().ok_or_else(invalid)?)?;
    let outer = metadata.as_object().ok_or_else(invalid)?;
    let body = metadata["motion_relationship"]
        .as_object()
        .ok_or_else(invalid)?;
    if outer.len() != 1
        || body.len() != 4
        || body.get("contract_version") != Some(&json!(1))
        || body.get("id") != Some(&json!(id))
        || body.get("kind") != Some(&json!(kind))
    {
        return Err(invalid());
    }
    let position = body.get("position").ok_or_else(invalid)?;
    if !position.is_null() && !position.as_u64().is_some_and(|n| n <= u64::from(u32::MAX)) {
        return Err(invalid());
    }
    Ok(Some(
        json!({"id":id,"source_item_id":source,"target_item_id":target,"kind":kind,"position":position,"active":row["active"]}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn zero_and_maximum_wire_positions_are_not_native_ordinals() {
        for position in [Value::Null, json!(0), json!(u32::MAX)] {
            let mut row = json!({"id":"edge","source_id":"a","target_id":"b","kind":"custom:motion_extra_of","position":null,"active":1,
                "metadata":json!({"motion_relationship":{"contract_version":1,"id":"edge","kind":"extra_of","position":position}}).to_string()});
            assert_eq!(decode_record(&row).unwrap().unwrap()["position"], position);
            row["position"] = json!(1);
            assert!(decode_record(&row).is_err());
        }
    }
    #[test]
    fn reserved_domain_never_swallows_malformed_or_foreign_edges() {
        let mut row = json!({"kind":"custom:unrelated"});
        assert!(decode_record(&row).unwrap().is_none());
        row["kind"] = json!("custom:motion_part_of");
        assert!(decode_record(&row).is_err());
    }
}
