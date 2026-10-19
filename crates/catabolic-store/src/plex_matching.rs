// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Reviewed Plex matches retain evidence through the existing curation owner.
//! This module reads published links, but never changes output or media bytes.
use crate::{
    Result, Store, consumers, curation, filesystem_mutation as filesystem, migration::rows,
};
use catabolic_core::{decode_json, encode, sha256};
use serde_json::{Value, json};
use std::path::Path;

pub fn error(code: &str) -> crate::Error {
    consumers::Failure::new(code).error()
}
pub fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    consumers::text(&value[key], 255).map_err(|e| error(&e.0))
}
pub fn options(request: &Value) -> Result<(&str, usize)> {
    let source = request
        .get("metadata_source")
        .and_then(Value::as_str)
        .unwrap_or("association");
    let maximum = request
        .get("max_items")
        .map(Value::as_u64)
        .unwrap_or(Some(10000));
    if !matches!(source, "association" | "item")
        || request
            .get("metadata_source")
            .is_some_and(|v| !v.is_string())
        || maximum.is_none_or(|n| !(1..=10000).contains(&n))
    {
        return Err(error("metadata_invalid_match_options"));
    }
    if request["apply"] == true && request["expected_plan"].as_str().is_none_or(str::is_empty) {
        return Err(error("metadata_match_requires_expected_plan"));
    }
    Ok((source, maximum.unwrap() as usize))
}
fn subset(value: &Value, keys: &[&str]) -> Value {
    Value::Object(
        keys.iter()
            .map(|k| ((*k).to_owned(), value[*k].clone()))
            .collect(),
    )
}
/// The exact published mapping, association, source revision and output owner.
/// Uses existing descriptor-fenced helpers rather than pathname-only checks.
pub fn published_snapshot(
    store: &Store,
    profile: &str,
    binding_id: &str,
    mapping_id: &str,
) -> Result<Value> {
    crate::definitions::require_recovered(store)?;
    let binding = consumers::binding(store, profile, binding_id)?;
    if binding["enabled"] == 0 || binding["enabled"] == false {
        return Err(error("metadata_binding_disabled"));
    }
    let mappings = rows(
        &store.db,
        "SELECT * FROM mappings WHERE id=? AND catalog=? AND active=1",
        &[
            mapping_id.to_owned().into(),
            field(&binding, "catalog")?.to_owned().into(),
        ],
    )?;
    let mapping = mappings
        .first()
        .filter(|_| mappings.len() == 1)
        .ok_or_else(|| error("metadata_mapping_not_active_in_binding"))?;
    let associations = rows(
        &store.db,
        "SELECT * FROM item_files WHERE file_id=? AND item_id=? AND active=1 AND role='primary'",
        &[
            field(mapping, "file_id")?.to_owned().into(),
            field(mapping, "item_id")?.to_owned().into(),
        ],
    )?;
    let association = associations
        .first()
        .filter(|_| associations.len() == 1)
        .ok_or_else(|| error("metadata_requires_unique_primary_association"))?;
    let payload = json!({"item_id":mapping["item_id"],"role":"primary","part":association["part"]});
    let snapshot =
        curation::proposal_snapshot(store, profile, field(mapping, "file_id")?, &payload)?;
    let output = crate::observation::binding(store, profile, "output", field(mapping, "catalog")?)?;
    let source = crate::observation::binding(
        store,
        profile,
        "source",
        field(&snapshot["file"], "location")?,
    )?;
    let target = Path::new(source["root"].as_str().unwrap())
        .join(snapshot["file"]["path"].as_str().unwrap());
    let destination =
        Path::new(output["root"].as_str().unwrap()).join(mapping["path"].as_str().unwrap());
    let expected = filesystem::relative(
        &target,
        destination.parent().ok_or_else(|| error("invalid_path"))?,
    )?;
    crate::source::with_validated_source(&snapshot["file"], |_| {
        let (root, _) = crate::observation::checked_root(store, profile, &output)?;
        if filesystem::owner_state(
            &root,
            &filesystem::owner(store, profile, field(mapping, "catalog")?),
        )? != "owned"
        {
            return Err(error("metadata_output_not_owned"));
        }
        let owned = rows(
            &store.db,
            "SELECT target FROM owned_links WHERE profile=? AND catalog=? AND path=?",
            &[
                profile.to_owned().into(),
                field(mapping, "catalog")?.to_owned().into(),
                mapping["path"].as_str().unwrap().to_owned().into(),
            ],
        )?;
        if owned.len() != 1
            || owned[0]["target"] != expected
            || filesystem::state(&root, mapping["path"].as_str().unwrap())?
                != ("link".into(), Some(expected.clone()))
        {
            return Err(error("metadata_published_link_changed"));
        }
        Ok(())
    })?;
    let relative = consumers::path(mapping["path"].as_str().unwrap(), true)?;
    let subtree = binding["subtree"].as_str().unwrap();
    if !consumers::within(&relative, subtree) {
        return Err(error("outside_binding_scope"));
    }
    let suffix = relative
        .strip_prefix(subtree)
        .unwrap()
        .trim_start_matches('/');
    let remote = binding["remote_root"].as_str().unwrap();
    let remote_path = if suffix.is_empty() {
        remote.to_owned()
    } else {
        format!("{}/{suffix}", remote.trim_end_matches('/'))
    };
    Ok(
        json!({"binding":subset(&binding,&["id","connection_id","catalog","subtree","remote_root","library","revision","enabled"]),"mapping":mapping,"association":association,"item":snapshot["item"][0],"output":output,"source":source,"remote_path":remote_path,"source_snapshot":snapshot["file"]}),
    )
}
pub fn evidence(
    connection: &Value,
    library: &Value,
    local: &Value,
    remote: &Value,
    metadata_source: &str,
) -> Result<Value> {
    if remote["reported_type"] != remote["kind"]
        || !matches!(
            remote["kind"].as_str(),
            Some("movie" | "episode" | "track" | "photo")
        )
        || remote["guid"].as_str().is_none_or(str::is_empty)
    {
        return Err(error("metadata_match_unsupported_remote_item"));
    }
    if metadata_source == "item" && local["item"]["kind"] != remote["kind"] {
        return Err(error("metadata_item_source_kind_mismatch_use_association"));
    }
    let mut paths: Vec<_> = remote["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["path"].as_str().unwrap())
        .collect();
    paths.sort_unstable();
    Ok(
        json!({"version":1,"connection_id":connection["id"],"server_id":connection["server_id"],"library":subset(library,&["id","uuid","type","roots","created_at"]),"rating_key":remote["rating_key"],"kind":remote["kind"],"guid":remote["guid"],"remote_paths":paths,"remote_path":local["remote_path"],"binding_id":local["binding"]["id"],"mapping_id":local["mapping"]["id"],"association_id":local["association"]["id"],"association_identity":subset(&local["association"],&["file_id","item_id","role","part"]),"source_snapshot":local["source_snapshot"],"output":local["output"],"source":local["source"],"metadata_source":metadata_source,"binding":local["binding"],"mapping":local["mapping"]}),
    )
}
/// Revalidate after the detached read, then durably put and accept evidence.
/// Both mutation steps belong to curation, including savepoints and idempotence.
pub fn finish(
    store: &Store,
    profile: &str,
    request: &Value,
    connection: &Value,
    local: &Value,
    evidence: &Value,
    checkpoint: &mut dyn FnMut() -> Result<()>,
) -> Result<Value> {
    checkpoint()?;
    options(request)?;
    let current = published_snapshot(
        store,
        profile,
        field(request, "binding_id")?,
        field(request, "mapping_id")?,
    )?;
    if consumers::connection(store, profile, field(connection, "id")?)? != *connection
        || current != *local
    {
        return Err(error("metadata_match_local_changed"));
    }
    let reviewed = json!([store.database_id, profile, connection, local, evidence]);
    let encoded = encode(&reviewed);
    if encoded.len() > 4 * 1024 * 1024 {
        return Err(error("metadata_match_plan_too_large"));
    }
    let digest = sha256(encoded);
    let apply = request["apply"] == true;
    if apply && request["expected_plan"] != digest {
        return Err(error("metadata_match_plan_changed_preview_again"));
    }
    let mut match_id = Value::Null;
    if apply {
        checkpoint()?;
        let proposal = curation::proposal_put(
            store,
            profile,
            &json!({"file_id":local["mapping"]["file_id"],"payload":{"item_id":local["mapping"]["item_id"],"role":"primary","part":local["association"]["part"]},"source":"plex-match","evidence":evidence}),
        )?;
        checkpoint()?;
        // Revalidate publication immediately before admitting accepted evidence.
        // A failed decision leaves the independently durable pending proposal.
        if published_snapshot(
            store,
            profile,
            field(request, "binding_id")?,
            field(request, "mapping_id")?,
        )? != *local
        {
            return Err(error("metadata_match_local_changed"));
        }
        // Publication validation can itself observe a signal. Recheck at the
        // final admission boundary before curation can record acceptance.
        checkpoint()?;
        curation::accept_existing_association_evidence(
            store,
            profile,
            &json!({"id":proposal["id"],"actor":"plex-match"}),
        )?;
        match_id = proposal["id"].clone();
    }
    Ok(
        json!({"plan_id":digest,"applied":apply,"match_id":match_id,"match":evidence,"item_kind":local["item"]["kind"],"complete":true}),
    )
}
/// Saved-match publication fence for the later metadata owner. Metadata edits
/// remain permitted; association structure and source/publication revisions do not.
pub fn matched_publication(
    store: &Store,
    profile: &str,
    connection: &Value,
    library: &Value,
    match_id: &str,
) -> Result<(Value, Value)> {
    if consumers::connection(store, profile, field(connection, "id")?)? != *connection {
        return Err(error("metadata_connection_changed"));
    }
    let proposal = curation::proposal_get(store, profile, match_id)?;
    if proposal["source"] != "plex-match" || proposal["state"] != "accepted" {
        return Err(error("metadata_requires_accepted_match"));
    }
    let e = &proposal["evidence"];
    if e["connection_id"] != connection["id"]
        || e["server_id"] != connection["server_id"]
        || e["library"] != subset(library, &["id", "uuid", "type", "roots", "created_at"])
    {
        return Err(error("metadata_match_connection_or_library_changed"));
    }
    let local = published_snapshot(
        store,
        profile,
        field(e, "binding_id")?,
        field(e, "mapping_id")?,
    )?;
    if [
        "binding",
        "mapping",
        "remote_path",
        "source_snapshot",
        "output",
        "source",
    ]
    .iter()
    .any(|k| local[*k] != e[*k])
        || local["association"]["id"] != e["association_id"]
        || subset(
            &local["association"],
            &["file_id", "item_id", "role", "part"],
        ) != e["association_identity"]
    {
        return Err(error("metadata_match_publication_changed"));
    }
    if e["metadata_source"] == "item" && local["item"]["kind"] != e["kind"] {
        return Err(error("metadata_item_source_kind_mismatch_use_association"));
    }
    Ok((local, e.clone()))
}
pub fn listing(store: &Store, profile: &str, request: &Value) -> Result<Value> {
    let limit = consumers::limit(request, 100, 1000)?;
    let after = request.get("after").and_then(Value::as_str).unwrap_or("");
    let binding = request
        .get("binding_id")
        .filter(|v| !v.is_null())
        .map(|v| consumers::text(v, 255))
        .transpose()?;
    let mut result = rows(
        &store.db,
        "SELECT id,file_id,json_extract(evidence,'$.binding_id') binding_id,json_extract(evidence,'$.mapping_id') mapping_id,json_extract(evidence,'$.rating_key') rating_key,json_extract(evidence,'$.metadata_source') metadata_source FROM proposals WHERE profile=? AND source='plex-match' AND state='accepted' AND id>? AND (? IS NULL OR json_extract(evidence,'$.binding_id')=?) ORDER BY id LIMIT ?",
        &[
            profile.to_owned().into(),
            after.to_owned().into(),
            binding.map(str::to_owned).into(),
            binding.map(str::to_owned).into(),
            ((limit + 1) as i64).into(),
        ],
    )?;
    let next = if result.len() > limit {
        result[limit - 1]["id"].clone()
    } else {
        Value::Null
    };
    result.truncate(limit);
    Ok(json!({"matches":result,"next_after":next}))
}
pub fn library(local: &Value) -> Result<Value> {
    Ok(decode_json(
        local["binding"]["library"]
            .as_str()
            .ok_or_else(|| error("invalid_configuration"))?,
    )?)
}
