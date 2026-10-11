// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Explicit reviewed remount repair. Root renumbering is never inferred from a
//! path-only match; source replacement requires the caller's named trust scope.
use crate::filesystem_mutation::{
    open_at, open_root, owner, owner_state, parent, relative, state, version,
};
use crate::{Error, Result, Store, migration::rows};
use catabolic_core::{encode, sha256};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    os::unix::fs::MetadataExt,
    path::Path,
};

fn boolean(args: &Value, key: &str) -> Result<bool> {
    args.get(key)
        .map(|v| {
            v.as_bool()
                .ok_or_else(|| Error(format!("{key} must be boolean")))
        })
        .transpose()
        .map(|v| v.unwrap_or(false))
}
fn texts(args: &Value, key: &str) -> Result<BTreeSet<String>> {
    args.get(key)
        .map(|v| {
            v.as_array()
                .ok_or_else(|| Error(format!("{key} must be a list")))
                .and_then(|v| {
                    v.iter()
                        .map(|v| {
                            v.as_str()
                                .map(str::to_owned)
                                .ok_or_else(|| Error(format!("{key} must contain text")))
                        })
                        .collect()
                })
        })
        .transpose()
        .map(|v| v.unwrap_or_default())
}
/// Build or apply a fresh digest-pinned repair using the exact frozen schema.
pub fn execute(store: &Store, profile: &str, args: &Value) -> Result<Value> {
    repair_with(store, profile, args, &crate::source::volume_uuid)
}
/// Reverify device-number changes without adopting unknown roots or volumes.
/// This is suitable for bounded maintenance and retains normal repair auditing.
pub fn recover_device_numbers(store: &Store, profile: &str, args: &Value) -> Result<Value> {
    let required = args
        .get("required_bindings")
        .map(|value| -> Result<BTreeSet<(String, String)>> {
            let array = value
                .as_array()
                .ok_or_else(|| Error("required_bindings must be a list".into()))?;
            array
                .iter()
                .map(|entry| {
                    let pair =
                        entry
                            .as_array()
                            .filter(|pair| pair.len() == 2)
                            .ok_or_else(|| {
                                Error("required binding must be a kind/owner pair".into())
                            })?;
                    let kind = pair[0]
                        .as_str()
                        .filter(|kind| matches!(*kind, "source" | "output"))
                        .ok_or_else(|| Error("invalid required binding kind".into()))?;
                    let owner = pair[1]
                        .as_str()
                        .ok_or_else(|| Error("invalid required binding owner".into()))?;
                    Ok((kind.to_string(), owner.to_string()))
                })
                .collect()
        })
        .transpose()?;
    let mut changed = false;
    for binding in rows(
        &store.db,
        "SELECT * FROM bindings WHERE profile=? ORDER BY kind,owner",
        &[profile.to_string().into()],
    )? {
        let kind = binding["kind"].as_str().unwrap();
        let owner = binding["owner"].as_str().unwrap();
        if required
            .as_ref()
            .is_some_and(|required| !required.contains(&(kind.to_string(), owner.to_string())))
        {
            continue;
        }
        if kind == "source"
            && crate::observation::trust_policy(store, profile, owner)?["settings"]["device"]
                == "skip"
        {
            continue;
        }
        if open_root(binding["root"].as_str().unwrap())?
            .metadata()?
            .dev()
            != binding["device"].as_u64().unwrap()
        {
            changed = true;
        }
    }
    if !changed {
        return Ok(Value::Null);
    }
    let preview = execute(store, profile, &json!({"inventory_recovery":true}))?;
    let mut result = execute(
        store,
        profile,
        &json!({"inventory_recovery":true,"apply":true,"expected_plan":preview["plan_id"]}),
    )?;
    result["automatic"] = json!(true);
    Ok(result)
}

fn repair_with(
    store: &Store,
    profile: &str,
    args: &Value,
    volume: &impl Fn(&File) -> Option<String>,
) -> Result<Value> {
    let apply = boolean(args, "apply")?;
    let adopt = boolean(args, "adopt_existing")?;
    let inventory = boolean(args, "inventory_recovery")?;
    let trusted = texts(args, "trust_sources")?;
    if inventory && (adopt || !trusted.is_empty()) {
        return Err(Error(
            "inventory recovery cannot adopt replacement storage".into(),
        ));
    }
    if store
        .db
        .prepare("SELECT 1 FROM journal LIMIT 1")?
        .exists([])?
    {
        return Err(Error(
            "recover pending operations before changing desired catalog state".into(),
        ));
    }
    if store
        .db
        .prepare("SELECT 1 FROM consumer_deliveries WHERE profile=? AND lease_until>?")?
        .exists(params![profile, crate::filesystem_mutation::now()])?
    {
        return Err(Error(
            "consumer delivery is in flight; retry after its lease ends".into(),
        ));
    }
    if store.db.prepare("SELECT 1 FROM owned_hardlinks WHERE profile=? UNION ALL SELECT 1 FROM retained_hardlinks WHERE profile=? LIMIT 1")?.exists([profile,profile])?{return Err(Error("remount repair of hardlink ownership is not yet supported".into()));}
    let bindings = rows(
        &store.db,
        "SELECT * FROM bindings WHERE profile=? ORDER BY kind,owner",
        &[profile.to_string().into()],
    )?;
    if bindings.is_empty() {
        return Err(Error("no bindings to repair".into()));
    }
    let sources: BTreeSet<_> = bindings
        .iter()
        .filter(|r| r["kind"] == "source")
        .map(|r| r["owner"].as_str().unwrap().to_string())
        .collect();
    if !trusted.is_subset(&sources) {
        return Err(Error("unknown source in --trust-source".into()));
    }
    for source in &trusted {
        if store
            .db
            .prepare("SELECT 1 FROM generated_locations WHERE profile=? AND location=?")?
            .exists([profile, source])?
        {
            return Err(Error(
                "generated artifact locations cannot be adopted with --trust-source".into(),
            ));
        }
    }
    let mut changes = Vec::new();
    let mut candidates = BTreeMap::new();
    let mut handles = BTreeMap::new();
    for old in &bindings {
        let kind = old["kind"].as_str().unwrap();
        let name = old["owner"].as_str().unwrap();
        let fd = open_root(old["root"].as_str().unwrap())?;
        let metadata = fd.metadata()?;
        let stable = volume(&fd);
        let trust = kind == "source" && trusted.contains(name);
        if stable.is_none() && !trust {
            return Err(Error(
                "stable volume UUID unavailable; repair refused".into(),
            ));
        }
        if old["inode"].as_u64() != Some(metadata.ino()) && !trust {
            return Err(Error(
                "root inode changed; remount repair cannot adopt a replacement directory".into(),
            ));
        }
        let known: Option<String> = store
            .db
            .query_row(
                "SELECT volume_uuid FROM binding_volumes WHERE profile=? AND kind=? AND owner=?",
                [profile, kind, name],
                |r| r.get(0),
            )
            .optional()?;
        if known.is_some() && known != stable && !trust {
            return Err(Error(
                "volume UUID changed; remount repair cannot adopt a replacement volume".into(),
            ));
        }
        if known.is_none() && !adopt && !trust {
            return Err(Error(
                "legacy binding has no volume UUID; preview with --adopt-existing".into(),
            ));
        }
        if kind == "output" {
            let mode: Option<String> = store
                .db
                .query_row(
                    "SELECT mode FROM catalog_link_modes WHERE catalog=?",
                    [name],
                    |r| r.get(0),
                )
                .optional()?;
            if mode.as_deref() == Some("hardlink") {
                return Err(Error(
                    "remount repair currently supports symlink outputs only".into(),
                ));
            }
            if owner_state(&fd, &owner(store, profile, name))? != "owned" {
                return Err(Error("output ownership is missing".into()));
            }
        }
        let mut candidate = old.clone();
        candidate["device"] = json!(metadata.dev());
        candidate["inode"] = json!(metadata.ino());
        changes.push(json!({"before":old,"after":candidate,"volume_uuid":stable,"previous_volume_uuid":known,"adopted":known.is_none(),"trusted_replacement":trust}));
        candidates.insert((kind.to_string(), name.to_string()), candidate);
        handles.insert((kind.to_string(), name.to_string()), fd);
    }
    let mut observed = serde_json::Map::new();
    let mut pending = Vec::new();
    let mut output_links = serde_json::Map::new();
    let mut selections = Vec::new();
    let mut count = 0;
    for change in &changes {
        let old = &change["before"];
        if old["kind"] != "output" {
            continue;
        }
        let catalog = old["owner"].as_str().unwrap();
        let owned: BTreeMap<String, String> = store
            .db
            .prepare("SELECT path,target FROM owned_links WHERE profile=? AND catalog=?")?
            .query_map([profile, catalog], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        let mappings = crate::filesystem_mutation::mappings(store, profile, catalog)?;
        let desired: BTreeSet<_> = mappings
            .iter()
            .map(|m| m["path"].as_str().unwrap().to_string())
            .collect();
        if owned.keys().cloned().collect::<BTreeSet<_>>() != desired {
            return Err(Error(
                "owned links differ from selected mappings; repair refused".into(),
            ));
        }
        for mapping in mappings {
            let path = mapping["path"].as_str().unwrap();
            let file = mapping["file_id"].as_str().unwrap();
            let source = mapping["location"].as_str().unwrap();
            let source_path = mapping["source_path"].as_str().unwrap();
            selections.push(json!({"catalog":catalog,"path":path,"file_id":file,"location":source,"source_path":source_path}));
            let source_key = ("source".to_string(), source.to_string());
            let candidate = candidates
                .get(&source_key)
                .ok_or_else(|| Error("published source is unbound".into()))?;
            let destination = Path::new(old["root"].as_str().unwrap()).join(path);
            let expected = relative(
                &Path::new(candidate["root"].as_str().unwrap()).join(source_path),
                destination.parent().unwrap(),
            )?;
            if owned.get(path) != Some(&expected)
                || state(&handles[&("output".into(), catalog.into())], path)?
                    != ("link".into(), Some(expected.clone()))
            {
                return Err(Error(
                    "published symlink or target changed; repair refused".into(),
                ));
            }
            output_links
                .entry(catalog.to_string())
                .or_insert_with(|| json!({}))[path] = json!(expected);
            count += 1;
            let current = (|| -> Result<Value> {
                let (parent, leaf) = parent(&handles[&source_key], source_path, false)?;
                let file_handle = open_at(&parent, &leaf, false)?;
                let metadata = file_handle.metadata()?;
                if !metadata.is_file() {
                    return Err(Error(format!(
                        "source is no longer a regular file: {source_path}"
                    )));
                }
                let current = version(&metadata);
                let trust = trusted.contains(source);
                if mapping["status"].is_null() {
                    return Err(Error(
                        "published source has no observation; scan first".into(),
                    ));
                }
                if !trust
                    && (mapping["status"] != "present"
                        || ["size", "mtime_ns", "inode"]
                            .iter()
                            .any(|k| current[k] != mapping[k])
                        || current["device"] != candidate["device"])
                {
                    return Err(Error(
                        "published source metadata changed; repair refused".into(),
                    ));
                }
                let header = crate::observation::header_check(&file_handle, source_path, &current)?;
                if header["status"] == "invalid"
                    || crate::filesystem_mutation::recorded_reason(store, profile, file, &current)?
                        .is_some()
                {
                    return Err(Error(
                        "published source is unhealthy; repair refused".into(),
                    ));
                }
                if version(&open_at(&parent, &leaf, false)?.metadata()?) != current {
                    return Err(Error("published source changed during repair".into()));
                }
                Ok(
                    json!({"device":current["device"],"inode":current["inode"],"size":current["size"],"mtime_ns":current["mtime_ns"],"old_device":mapping["device"],"trusted_replacement":trust}),
                )
            })();
            match current {
                Ok(value) => {
                    observed.insert(file.into(), value);
                }
                Err(e) if inventory => pending
                    .push(json!({"file_id":file,"source":source,"path":source_path,"reason":e.0})),
                Err(e) => return Err(e),
            }
        }
    }
    let plan = json!({"bindings":changes,"verified_links":count,"observations":observed,"inventory_recovery":inventory,"pending_sources":pending});
    let preconditions = if inventory {
        json!({"bindings":changes,"output_links":output_links,"selections":selections,"inventory_recovery":true})
    } else {
        plan.clone()
    };
    let digest = sha256(encode(&preconditions));
    let mut result = json!({"plan_id":digest,"bindings":changes,"verified_links":count,"verified_sources":observed.len(),"sources_needing_observation":pending,"inventory_recovery":inventory,"applied":false,"complete":true});
    if !apply {
        return Ok(result);
    }
    if args["expected_plan"] != digest {
        return Err(Error(
            "repair preview changed; preview again and supply --expected-plan".into(),
        ));
    }
    let verification=store.transaction(|db|{
        db.execute("INSERT INTO remount_guard VALUES(?)",[profile])?;
        for change in &changes {
            let old=&change["before"];let new=&change["after"];let kind=old["kind"].as_str().unwrap();let name=old["owner"].as_str().unwrap();
            let fd=open_root(old["root"].as_str().unwrap())?;if json!([fd.metadata()?.dev(),fd.metadata()?.ino(),volume(&fd)])!=json!([new["device"],new["inode"],change["volume_uuid"]]){return Err(Error("root changed during repair".into()));}
            db.execute("UPDATE bindings SET device=?,inode=? WHERE profile=? AND kind=? AND owner=?",params![new["device"].as_i64(),new["inode"].as_i64(),profile,kind,name])?;
            db.execute("DELETE FROM binding_volumes WHERE profile=? AND kind=? AND owner=?",[profile,kind,name])?;if let Some(stable)=change["volume_uuid"].as_str(){db.execute("INSERT INTO binding_volumes VALUES(?,?,?,?)",[profile,kind,name,stable])?;}
            if kind=="output"&&old!=new {
                for consumer in rows(db,"SELECT id,local_binding FROM consumer_bindings WHERE profile=? AND catalog=?",&[profile.to_string().into(),name.to_string().into()])?{if catabolic_core::decode_json(consumer["local_binding"].as_str().unwrap())?!=*old{return Err(Error("consumer pinned to different output; repair refused".into()));}}
                db.execute("UPDATE consumer_bindings SET local_binding=?,revision=revision+1,indexing=NULL WHERE profile=? AND catalog=?",params![encode(new),profile,name])?;
                for publication in rows(db,"SELECT local_binding FROM publication_generations WHERE profile=? AND catalog=?",&[profile.to_string().into(),name.to_string().into()])?{if catabolic_core::decode_json(publication["local_binding"].as_str().unwrap())?!=*old{return Err(Error("publication pinned to different output; repair refused".into()));}}
                db.execute("UPDATE publication_generations SET local_binding=? WHERE profile=? AND catalog=?",params![encode(new),profile,name])?;
            }
        }
        for(file,observation)in &observed{if observation["trusted_replacement"]!=true{db.execute("UPDATE observations SET device=? WHERE profile=? AND file_id=?",params![observation["device"].as_i64(),profile,file])?;}}
        db.execute("DELETE FROM remount_guard WHERE profile=?",[profile])?;
        for(file,observation)in &observed{if observation["trusted_replacement"]==true{db.execute("UPDATE observations SET device=?,inode=?,size=?,mtime_ns=?,status='present' WHERE profile=? AND file_id=?",params![observation["device"].as_i64(),observation["inode"].as_i64(),observation["size"].as_i64(),observation["mtime_ns"].as_i64(),profile,file])?;}}
        let mut reports=Vec::new();for change in &changes{if change["before"]["kind"]=="output"{reports.extend(crate::filesystem_mutation::execute(store,profile,"verify",&json!({"catalog":change["before"]["owner"]}))?["catalogs"].as_array().unwrap().iter().cloned());}}
        let verification=json!({"healthy":reports.iter().all(|r|r["healthy"]==true),"catalogs":reports});
        if inventory {for change in &changes{let old=&change["before"];let candidate=&change["after"];let fd=open_root(old["root"].as_str().unwrap())?;
            if json!([fd.metadata()?.dev(),fd.metadata()?.ino(),volume(&fd)])!=json!([candidate["device"],candidate["inode"],change["volume_uuid"]]){return Err(Error("root changed during repair".into()));}
            if old["kind"]=="output"{let catalog=old["owner"].as_str().unwrap();if owner_state(&fd,&owner(store,profile,catalog))?!="owned"{return Err(Error("output ownership changed during repair".into()));}
            if let Some(links)=output_links.get(catalog).and_then(Value::as_object){for(path,target)in links{if state(&fd,path)?!=("link".into(),target.as_str().map(str::to_owned)){return Err(Error("published symlink changed during repair".into()));}}}}
        }}
        if verification["healthy"]!=true&&!inventory{let mut reasons=Vec::new();for report in &reports {for issue in report["issues"].as_array().unwrap(){let mut item=issue.clone();item["catalog"]=report["catalog"].clone();reasons.push(item);}}return Err(Error(format!("final publication verification failed; repair rolled back: {}",encode(&json!(reasons.iter().take(10).collect::<Vec<_>>())))));}
        db.execute("DELETE FROM remount_guard WHERE profile=?",[profile])?;
        db.execute("INSERT INTO remount_repairs(id,profile,plan) VALUES(?,?,?)",params![uuid::Uuid::new_v4().to_string(),profile,encode(&plan)])?;
        Ok(verification)
    })?;
    result["applied"] = json!(true);
    result["verification"] = verification;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trust_requires_exact_reviewed_plan_and_cannot_adopt_unnamed_replacements() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("catalog.sqlite3");
        crate::migration::initialize(&database).unwrap();
        let store = Store::open(&database, true, false).unwrap();
        let source = root.path().join("source");
        std::fs::create_dir(&source).unwrap();
        crate::observation::execute(
            &store,
            "default",
            "bind",
            &json!({"owner":"media","root":source}),
        )
        .unwrap();
        std::fs::rename(&source, root.path().join("old")).unwrap();
        std::fs::create_dir(&source).unwrap();
        let unavailable = |_: &File| None;
        assert!(
            repair_with(&store, "default", &json!({}), &unavailable)
                .unwrap_err()
                .0
                .contains("UUID unavailable")
        );
        let preview = repair_with(
            &store,
            "default",
            &json!({"trust_sources":["media"]}),
            &unavailable,
        )
        .unwrap();
        assert_eq!(preview["applied"], false);
        let before = crate::observation::binding(&store, "default", "source", "media").unwrap();
        assert!(
            repair_with(
                &store,
                "default",
                &json!({"trust_sources":["media"],"apply":true,"expected_plan":"stale"}),
                &unavailable
            )
            .is_err()
        );
        assert_eq!(
            before,
            crate::observation::binding(&store, "default", "source", "media").unwrap()
        );
        let result = repair_with(
            &store,
            "default",
            &json!({"trust_sources":["media"],"apply":true,"expected_plan":preview["plan_id"]}),
            &unavailable,
        )
        .unwrap();
        assert_eq!(result["applied"], true);
        assert_eq!(
            crate::observation::binding(&store, "default", "source", "media").unwrap()["inode"],
            source.metadata().unwrap().ino()
        );
    }
    #[test]
    fn stable_uuid_device_renumbering_is_atomic_and_source_bytes_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("catalog.sqlite3");
        crate::migration::initialize(&database).unwrap();
        let store = Store::open(&database, true, false).unwrap();
        let source = root.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("one.txt"), "original").unwrap();
        crate::observation::execute(
            &store,
            "default",
            "bind",
            &json!({"owner":"media","root":source}),
        )
        .unwrap();
        let original = crate::observation::binding(&store, "default", "source", "media").unwrap();
        store.transaction(|db|{db.execute("UPDATE bindings SET device=42",[])?;db.execute("INSERT OR REPLACE INTO binding_volumes VALUES('default','source','media','stable')",[])?;Ok(())}).unwrap();
        let stable = |_: &File| Some("stable".to_string());
        let preview = repair_with(&store, "default", &json!({}), &stable).unwrap();
        assert_eq!(preview["bindings"][0]["before"]["device"], 42);
        repair_with(
            &store,
            "default",
            &json!({"apply":true,"expected_plan":preview["plan_id"]}),
            &stable,
        )
        .unwrap();
        assert_eq!(
            crate::observation::binding(&store, "default", "source", "media").unwrap(),
            original
        );
        assert_eq!(
            std::fs::read_to_string(source.join("one.txt")).unwrap(),
            "original"
        );
        assert_eq!(
            store
                .db
                .query_row("SELECT count(*) FROM remount_guard", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
