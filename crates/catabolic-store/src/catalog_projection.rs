// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Trusted, rebuildable application projection snapshots. Not an ACL endpoint.
//! The host holds its retained owner and application authority through committing
//! the derived projection. A page never authorizes writes back into this catalog.
mod lineage;
pub(crate) use lineage::version_lineage_in;
pub use lineage::{VersionLineageRequest, version_lineage};
mod named;
mod selected;
pub use selected::selected_current_versions;
mod timeline;
pub use named::named_items;
pub use timeline::named_timeline;

use crate::{Error, Result, Store};
use rusqlite::params;
use serde_json::{Value, json};

pub const MAX_PROJECTION_PAGE_ITEMS: usize = 1000;
pub const MAX_PROJECTION_PAGE_BYTES: usize = 8 * 1024 * 1024;
/// Independently established whole-catalog projection authority. Never decode
/// this grant from a user request or infer it from a page cursor.
#[derive(Clone, Debug)]
pub struct TrustedProjectionAuthority {
    pub catalog_id: String,
    pub incarnation: String,
    pub expected_generation: i64,
}
/// Deterministic keyset paging of preserved native item IDs. Missing IDs are not
/// deletion evidence: callers must not blindly delete application references.
/// Logical retirement is explicit and historical source rows remain present.
pub fn item_page(
    store: &Store,
    authority: &TrustedProjectionAuthority,
    after_id: Option<&str>,
    limit: usize,
) -> Result<Value> {
    if store.schema_version < 34
        || !(1..=MAX_PROJECTION_PAGE_ITEMS).contains(&limit)
        || after_id
            .is_some_and(|id| id.is_empty() || id.len() > 256 || id.chars().any(char::is_control))
        || !store.db.is_autocommit()
    {
        return Err(Error(
            "invalid projection page or ambient transaction".into(),
        ));
    }
    store.transaction_available()?;
    store.transaction(|db| {
        let (catalog,incarnation,generation):(String,String,i64)=db.query_row("SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        if catalog!=authority.catalog_id || incarnation!=authority.incarnation || generation!=authority.expected_generation {
            return Err(Error("projection catalog/incarnation/generation changed".into()));
        }
        let mut statement=db.prepare("SELECT i.id,i.kind,i.metadata,r.revision,l.kind,l.survivor_item_id,l.children_json FROM items i LEFT JOIN catalog_entity_revisions r ON r.entity_kind='items' AND r.entity_key=json_array(i.id) AND r.deleted=0 LEFT JOIN catalog_logical_retirements l ON l.item_id=i.id WHERE (?1 IS NULL OR i.id>?1) ORDER BY i.id LIMIT ?2")?;
        let rows=statement.query_map(params![after_id,(limit+1) as i64],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<i64>>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,Option<String>>(6)?)))?;
        let mut items=Vec::new();let mut bytes=0;let mut has_more=false;
        for row in rows {
            let (id,kind,metadata,revision,retirement_kind,survivor,children)=row?;
            if items.len()==limit {has_more=true;break;}
            let revision=revision.ok_or_else(||Error("projection item revision is unavailable".into()))?;
            let metadata:Value=serde_json::from_str(&metadata)?;
            if !metadata.is_object() {return Err(Error("projection item metadata is not an object".into()));}
            let retirement=match retirement_kind {
                None=>Value::Null,
                Some(kind)=>json!({"kind":kind,"survivor_item_id":survivor,"children":serde_json::from_str::<Value>(&children.ok_or_else(||Error("missing retirement children".into()))?)?}),
            };
            let item=json!({"item_id":id,"kind":kind,"metadata":metadata,"item_revision":revision,"retirement":retirement});
            let size=serde_json::to_vec(&item)?.len();
            if size>MAX_PROJECTION_PAGE_BYTES {return Err(Error("one projection item exceeds page byte budget".into()));}
            if bytes+size>MAX_PROJECTION_PAGE_BYTES {has_more=true;break;}
            bytes+=size;items.push(item);
        }
        let next=if has_more {items.last().map(|item|item["item_id"].clone())}else{None};
        Ok(json!({"catalog_id":catalog,"incarnation":incarnation,"generation":generation,"items":items,"next_after_id":next}))
    })
}

/// Current authoritative identity context and its exact timeline facts. All
/// members of the native context are returned, not only requested candidates.
/// Historical Motion scalars are not used or implied by this read.
pub fn version_manifest(
    store: &Store,
    authority: &TrustedProjectionAuthority,
    profile: &str,
    version_ids: &[String],
) -> Result<Value> {
    if store.schema_version < 33 || !store.db.is_autocommit() {
        return Err(Error(
            "invalid identity projection schema or ambient transaction".into(),
        ));
    }
    store.transaction_available()?;
    store.transaction(|db| {
        let (catalog, incarnation, generation): (String, String, i64) = db.query_row(
            "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1", [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if catalog != authority.catalog_id || incarnation != authority.incarnation || generation != authority.expected_generation {
            return Err(Error("projection catalog/incarnation/generation changed".into()));
        }
        // The existing identity owner validates complete context membership,
        // current heads, coverage, bindings and retained equivalence evidence.
        let manifest = crate::catalog_identity_mutation::load_manifest(store, profile, version_ids)?;
        let mut revisions = Vec::new();
        for entry in &manifest.versions {
            let id = &entry.version.version_id;
            let (version_revision, timeline_id, timeline_revision, current_timeline_revision, state, start, end): (i64,String,i64,i64,String,Option<i64>,Option<i64>) = db.query_row(
                "SELECT v.current_revision,r.timeline_id,r.timeline_revision,t.current_revision,tr.identity_state,tr.coverage_start_us,tr.coverage_end_us FROM catalog_versions v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision JOIN catalog_timelines t ON t.id=r.timeline_id JOIN catalog_timeline_revisions tr ON tr.timeline_id=t.id AND tr.revision=t.current_revision WHERE v.id=?",
                [id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)),
            )?;
            if timeline_revision != current_timeline_revision || timeline_id != entry.version.timeline_id {
                return Err(Error("projection version references stale timeline head".into()));
            }
            let coverage = match (start,end) {
                (None,None) => None,
                (Some(start),Some(end)) => Some(catabolic_core::catalog_identity::IntervalUs::new(start,end).map_err(|e|Error(e.to_string()))?),
                _ => return Err(Error("projection timeline coverage is incomplete".into())),
            };
            revisions.push(json!({"version_id":id,"version_revision":version_revision,"timeline_id":timeline_id,"timeline_revision":timeline_revision,"current_timeline_revision":current_timeline_revision,"timeline_identity_state":state,"timeline_coverage":coverage}));
        }
        Ok(json!({"catalog_id":catalog,"incarnation":incarnation,"generation":generation,"manifest":manifest,"version_revisions":revisions}))
    })
}

/// Current unavailable binding facts, not a playable manifest or equivalence
/// grant. Unknown observations and changed source identity are never absence.
pub fn unavailable_version_facts(
    store: &Store,
    authority: &TrustedProjectionAuthority,
    profile: &str,
    version_ids: &[String],
) -> Result<Value> {
    use std::{
        collections::BTreeSet,
        time::{Duration, Instant},
    };
    if !store.db.is_autocommit()
        || version_ids.is_empty()
        || version_ids.len() > 64
        || version_ids.iter().collect::<BTreeSet<_>>().len() != version_ids.len()
    {
        return Err(Error("invalid unavailable projection bounds".into()));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    store.transaction(|db| {
        let (catalog,incarnation,generation):(String,String,i64)=db.query_row(
            "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        if catalog!=authority.catalog_id || incarnation!=authority.incarnation || generation!=authority.expected_generation {
            return Err(Error("unavailable projection identity changed".into()));
        }
        let mut versions=Vec::new(); let mut total=0usize; let mut bytes=0usize; let mut unavailable=false;
        for id in version_ids {
            let (owner,head,timeline,bound,current,edition):(String,i64,String,i64,i64,String)=db.query_row(
                "SELECT v.association_item_id,v.current_revision,r.timeline_id,r.timeline_revision,t.current_revision,t.edition_item_id FROM catalog_versions v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision JOIN catalog_timelines t ON t.id=r.timeline_id WHERE v.id=? AND NOT EXISTS(SELECT 1 FROM catalog_version_retirements x WHERE x.version_id=v.id) AND NOT EXISTS(SELECT 1 FROM catalog_logical_retirements x WHERE x.item_id IN(v.association_item_id,t.edition_item_id))",
                [id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
            if bound!=current {return Err(Error("unavailable timeline head changed".into()));}
            let mut statement=db.prepare("SELECT binding_id,binding_revision,association_id,profile,file_id,file_revision,part,source_kind,source_start_us,source_end_us,timeline_start_us,timeline_end_us FROM catalog_active_binding_revisions WHERE version_id=? ORDER BY part,binding_id LIMIT 1025")?;
            let rows=statement.query_map([id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,i64>(6)?,r.get::<_,String>(7)?,r.get::<_,Option<i64>>(8)?,r.get::<_,Option<i64>>(9)?,r.get::<_,Option<i64>>(10)?,r.get::<_,Option<i64>>(11)?)))?;
            let mut bindings=Vec::new();
            for row in rows {
                total+=1;
                if total>1024 || Instant::now()>=deadline {return Err(Error("unavailable projection budget exceeded".into()));}
                let (binding,revision,association,p,file,file_revision,part,kind,start,end,ts,te)=row?;
                if ts.is_some()!=te.is_some() || (ts.is_none() && kind!="whole_file") {
                    return Err(Error("unavailable binding extent is incomplete".into()));
                }
                if p!=profile {return Err(Error("unavailable binding profile changed".into()));}
                let raw:String=db.query_row("SELECT snapshot_json FROM catalog_exact_file_revisions WHERE profile=? AND file_id=? AND revision=?",params![p,file,file_revision],|r|r.get(0))?;
                if raw.len()>64*1024 {return Err(Error("unavailable snapshot exceeds budget".into()));}
                let retained:Value=serde_json::from_str(&raw)?;
                let observed=crate::components::occurrence(db,profile,&file)?;
                if crate::components::revision(&retained)!=file_revision || retained["status"]!="present"
                    || ["id","location","path","root","root_device","root_inode","source_policy","volume_uuid"].iter().any(|k|retained[*k]!=observed[*k]) {
                    return Err(Error("unavailable occurrence identity changed".into()));
                }
                let current_revision=crate::components::revision(&observed);
                let status=match observed["status"].as_str() {
                    Some("missing")=>"missing",
                    Some("present") if current_revision==file_revision=>"present",
                    Some("present") if ["size","mtime_ns","device","inode"].iter().all(|k|observed[*k].as_i64().is_some())=>"replacement",
                    _=>return Err(Error("unavailable observation is unknown".into())),
                };
                unavailable|=status!="present";
                let fact=json!({"binding_id":binding,"binding_revision":revision,"association_id":association,"profile":p,"file":{"file_id":file,"revision":file_revision},"part":part,"source_kind":kind,"source_start_us":start,"source_end_us":end,"timeline_start_us":ts,"timeline_end_us":te,"observation":{"state":status,"current_revision":current_revision,"snapshot":observed}});
                bytes=bytes.saturating_add(serde_json::to_vec(&fact)?.len());
                if bytes>4*1024*1024 {return Err(Error("unavailable projection byte budget exceeded".into()));}
                bindings.push(fact);
            }
            let count:i64=db.query_row("SELECT COUNT(*) FROM catalog_version_bindings WHERE version_id=? AND version_revision=?",params![id,head],|r|r.get(0))?;
            if bindings.is_empty() || count!=bindings.len() as i64 {return Err(Error("unavailable binding set is incomplete".into()));}
            versions.push(json!({"version_id":id,"association_item_id":owner,"edition_item_id":edition,"version_revision":head,"timeline_id":timeline,"timeline_revision":bound,"current_timeline_revision":current,"bindings":bindings}));
        }
        if Instant::now()>=deadline {return Err(Error("unavailable projection deadline exceeded".into()));}
        if !unavailable {return Err(Error("no explicit unavailable occurrence".into()));}
        let result=json!({"contract_version":1,"catalog_id":catalog,"incarnation":incarnation,"generation":generation,"versions":versions});
        if serde_json::to_vec(&result)?.len()>4*1024*1024 {return Err(Error("unavailable projection byte budget exceeded".into()));}
        Ok(result)
    })
}

#[cfg(test)]
mod unavailable_tests {
    use super::*;
    #[test]
    fn unavailable_facts_require_explicit_observation_and_unchanged_source_identity() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog");
        crate::migration::initialize_native_at(&path, 39).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        // Adapter classification fixture, not a native mutation qualification.
        store.db.execute_batch(r#"
          CREATE TEMP TABLE catalog_versions(id TEXT,association_item_id TEXT,current_revision INTEGER);
          CREATE TEMP TABLE catalog_version_revisions(version_id TEXT,revision INTEGER,timeline_id TEXT,timeline_revision INTEGER);
          CREATE TEMP TABLE catalog_timelines(id TEXT,edition_item_id TEXT,current_revision INTEGER);
          CREATE TEMP TABLE catalog_version_retirements(version_id TEXT);
          CREATE TEMP TABLE catalog_logical_retirements(item_id TEXT);
          CREATE TEMP TABLE catalog_version_bindings(version_id TEXT,version_revision INTEGER);
          CREATE TEMP TABLE catalog_active_binding_revisions(version_id TEXT,binding_id TEXT,binding_revision INTEGER,association_id TEXT,profile TEXT,file_id TEXT,file_revision TEXT,part INTEGER,source_kind TEXT,source_start_us INTEGER,source_end_us INTEGER,timeline_start_us INTEGER,timeline_end_us INTEGER);
          CREATE TEMP TABLE catalog_exact_file_revisions(profile TEXT,file_id TEXT,revision TEXT,snapshot_json TEXT);
          CREATE TEMP TABLE files(id TEXT,location TEXT,path TEXT);
          CREATE TEMP TABLE observations(profile TEXT,file_id TEXT,size INTEGER,mtime_ns INTEGER,device INTEGER,inode INTEGER,status TEXT);
          CREATE TEMP TABLE bindings(profile TEXT,kind TEXT,owner TEXT,root TEXT,device INTEGER,inode INTEGER);
          CREATE TEMP TABLE source_identity_policies(profile TEXT,location TEXT,identity_policy TEXT,settings TEXT,revision INTEGER);
          CREATE TEMP TABLE binding_volumes(profile TEXT,kind TEXT,owner TEXT,volume_uuid TEXT);
          INSERT INTO catalog_versions VALUES('v','motion.version:v',1);
          INSERT INTO catalog_version_revisions VALUES('v',1,'t',1);
          INSERT INTO catalog_timelines VALUES('t','motion.edition:e',1);
          INSERT INTO catalog_version_bindings VALUES('v',1);
          INSERT INTO files VALUES('f','source','movie.mp4');
          INSERT INTO observations VALUES('default','f',10,20,30,40,'present');
          INSERT INTO bindings VALUES('default','source','source','/root',30,50);
        "#).unwrap();
        let snapshot = crate::components::occurrence(&store.db, "default", "f").unwrap();
        let revision = crate::components::revision(&snapshot);
        store
            .db
            .execute(
                "INSERT INTO catalog_exact_file_revisions VALUES('default','f',?,?)",
                params![revision, snapshot.to_string()],
            )
            .unwrap();
        store.db.execute("INSERT INTO catalog_active_binding_revisions VALUES('v','b',1,'a','default','f',?,1,'whole_file',NULL,NULL,0,1000)",[&revision]).unwrap();
        let grant = TrustedProjectionAuthority {
            catalog_id: store.database_id.clone(),
            incarnation: store.catalog_incarnation.clone().unwrap(),
            expected_generation: store
                .db
                .query_row("SELECT generation FROM catalog_state", [], |r| r.get(0))
                .unwrap(),
        };
        let read = || unavailable_version_facts(&store, &grant, "default", &["v".into()]);
        assert!(read().is_err(), "present is never unavailable evidence");
        store
            .db
            .execute("UPDATE observations SET status='missing'", [])
            .unwrap();
        assert_eq!(
            read().unwrap()["versions"][0]["bindings"][0]["observation"]["state"],
            "missing"
        );
        store.db.execute("UPDATE catalog_active_binding_revisions SET timeline_start_us=NULL,timeline_end_us=NULL", []).unwrap();
        let unknown = read().unwrap();
        assert!(unknown["versions"][0]["bindings"][0]["timeline_start_us"].is_null());
        assert!(unknown["versions"][0]["bindings"][0]["timeline_end_us"].is_null());
        store
            .db
            .execute(
                "UPDATE catalog_active_binding_revisions SET timeline_start_us=0",
                [],
            )
            .unwrap();
        assert!(
            read().is_err(),
            "mixed unknown extent cannot become coverage"
        );
        store
            .db
            .execute(
                "UPDATE catalog_active_binding_revisions SET timeline_end_us=1000",
                [],
            )
            .unwrap();
        let mut stale = grant.clone();
        stale.expected_generation += 1;
        assert!(unavailable_version_facts(&store, &stale, "default", &["v".into()]).is_err());
        store
            .db
            .execute("UPDATE catalog_timelines SET current_revision=2", [])
            .unwrap();
        assert!(
            read().is_err(),
            "stale timeline must not be unavailable evidence"
        );
        store
            .db
            .execute("UPDATE catalog_timelines SET current_revision=1", [])
            .unwrap();
        store
            .db
            .execute("UPDATE observations SET status='present',inode=41", [])
            .unwrap();
        assert_eq!(
            read().unwrap()["versions"][0]["bindings"][0]["observation"]["state"],
            "replacement"
        );
        store
            .db
            .execute("UPDATE bindings SET inode=51", [])
            .unwrap();
        assert!(read().is_err(), "changed root is not missing-file evidence");
        store
            .db
            .execute("UPDATE bindings SET inode=50", [])
            .unwrap();
        store
            .db
            .execute("UPDATE observations SET status='unknown'", [])
            .unwrap();
        assert!(read().is_err(), "unknown is not missing-file evidence");
        store.db.execute("DELETE FROM observations", []).unwrap();
        assert!(
            read().is_err(),
            "absent observation is not missing-file evidence"
        );
    }
}
