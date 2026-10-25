//! Selected current versions are not a whole-context playable manifest.
use super::*;
use std::collections::BTreeSet;
pub fn selected_current_versions(
    store: &Store,
    authority: &TrustedProjectionAuthority,
    profile: &str,
    requests: &[VersionLineageRequest],
) -> Result<Value> {
    if !matches!(store.schema_version, 39 | 40)
        || !store.db.is_autocommit()
        || profile.is_empty()
        || profile.len() > 256
        || requests.is_empty()
        || requests.len() > 64
        || requests
            .iter()
            .map(|r| &r.version_id)
            .collect::<BTreeSet<_>>()
            .len()
            != requests.len()
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
        return Err(Error("invalid selected-current projection request".into()));
    }
    store.transaction_available()?;
    store.transaction(|_| {
        let lineage=super::lineage::capture_in(store,authority,requests)?;
        if lineage["rows"].as_array().is_none_or(|rows|rows.len()!=requests.len()||rows.iter().any(|row|row["status"]!="active")) {
            return Err(Error("selected version structural lineage is not active".into()));
        }
        let ids:Vec<_>=requests.iter().map(|r|r.version_id.clone()).collect();
        let selected=crate::catalog_identity_mutation::load_selected_in(store,profile,&ids)?;
        let value=json!({"contract_version":1,"catalog_id":authority.catalog_id,"incarnation":authority.incarnation,"generation":authority.expected_generation,"profile":profile,"selection":selected,"lineage":lineage,"application_authority":false});
        if serde_json::to_vec(&value)?.len()>8*1024*1024 {return Err(Error("selected-current complete result exceeds byte bound".into()));}
        Ok(value)
    })
}
