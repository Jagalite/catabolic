// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Aggregate capture owner tests, not HTTP serving/issuance qualification.
use catabolic_store::{
    Store, catalog_projection::TrustedProjectionAuthority, catalog_work_capture, curation,
    migration,
};
use serde_json::json;

fn grant(store: &Store) -> TrustedProjectionAuthority {
    TrustedProjectionAuthority {
        catalog_id: store.database_id.clone(),
        incarnation: store.catalog_incarnation.clone().unwrap(),
        expected_generation: store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap(),
    }
}

#[test]
fn complete_plain_capture_is_principal_independent_and_unrelated_edits_do_not_change_dependencies()
{
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("native.db");
    migration::initialize_native_at(&path, 39).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    curation::item_put(&store,&json!({"id":"work","kind":"video","metadata":{"title":"Original","motion_catalog":{"contract_version":1,"item_id":"work","kind":"video"}}})).unwrap();
    let first = catalog_work_capture::capture(&store, &grant(&store), "default", "work").unwrap();
    assert_eq!(first.work_id(), "work");
    assert_eq!(first.catalog_id(), store.database_id);
    assert!(first.library_definitions().is_empty());
    curation::item_put(
        &store,
        &json!({"id":"unrelated","kind":"video","metadata":{"title":"Other"}}),
    )
    .unwrap();
    let next = catalog_work_capture::capture(&store, &grant(&store), "default", "work").unwrap();
    assert!(next.generation() > first.generation());
    assert_eq!(next.dependencies(), first.dependencies());
    curation::item_put(&store,&json!({"id":"work","kind":"video","metadata":{"title":"Changed","motion_catalog":{"contract_version":1,"item_id":"work","kind":"video"}}})).unwrap();
    let changed = catalog_work_capture::capture(&store, &grant(&store), "default", "work").unwrap();
    assert_ne!(changed.dependencies(), first.dependencies());
    assert!(
        catalog_work_capture::capture(
            &store,
            &TrustedProjectionAuthority {
                expected_generation: first.generation(),
                ..grant(&store)
            },
            "default",
            "work"
        )
        .is_err()
    );
    assert!(
        catalog_work_capture::capture(
            &store,
            &TrustedProjectionAuthority {
                incarnation: "wrong".into(),
                ..grant(&store)
            },
            "default",
            "work"
        )
        .is_err()
    );
}

#[test]
fn unknown_graph_never_becomes_an_empty_complete_capture() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("native.db");
    migration::initialize_native_at(&path, 39).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    for id in ["work", "other"] {
        curation::item_put(
            &store,
            &json!({"id":id,"kind":"video","metadata":{"title":id}}),
        )
        .unwrap();
    }
    // Explicit fixture relationship: this tests capture refusal, not mutation
    // authorization. The capture owner must account for every incident edge.
    store.db.execute("INSERT INTO item_relationships(id,source_id,target_id,kind,position,metadata,active) VALUES('edge','work','other','custom:unsupported',NULL,'{}',1)",[]).unwrap();
    assert!(catalog_work_capture::capture(&store, &grant(&store), "default", "work").is_err());
}

#[test]
fn supported_work_edges_capture_both_endpoints_and_inactive_facts() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("native.db");
    migration::initialize_native_at(&path, 39).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    for id in ["work", "other"] {
        curation::item_put(&store, &json!({"id":id,"kind":"video","metadata":{}})).unwrap();
    }
    let before = catalog_work_capture::capture(&store, &grant(&store), "default", "work").unwrap();
    // Explicit shape fixture only. Durable relationship command tests separately
    // prove mutation custody; this test proves complete dependency accounting.
    let metadata=json!({"motion_relationship":{"contract_version":1,"id":"edge","kind":"part_of","position":0}}).to_string();
    store.db.execute("INSERT INTO item_relationships(id,source_id,target_id,kind,position,metadata,active) VALUES('edge','work','other','custom:motion_part_of',NULL,?,1)",[&metadata]).unwrap();
    let active = catalog_work_capture::capture(&store, &grant(&store), "default", "work").unwrap();
    assert_ne!(before.dependencies(), active.dependencies());
    store
        .db
        .execute("UPDATE item_relationships SET active=0 WHERE id='edge'", [])
        .unwrap();
    let inactive =
        catalog_work_capture::capture(&store, &grant(&store), "default", "work").unwrap();
    assert_ne!(active.dependencies(), inactive.dependencies());
    assert_ne!(before.dependencies(), inactive.dependencies());
    store
        .db
        .execute(
            "UPDATE item_relationships SET metadata='{}' WHERE id='edge'",
            [],
        )
        .unwrap();
    assert!(catalog_work_capture::capture(&store, &grant(&store), "default", "work").is_err());
}

#[test]
fn capture_preserves_callers_sqlite_progress_owner_through_commit() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("native.db");
    migration::initialize_native_at(&path, 39).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    curation::item_put(&store, &json!({"id":"work","kind":"video","metadata":{}})).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    store
        .db
        .progress_handler(
            1,
            Some(move || {
                observed.fetch_add(1, Ordering::SeqCst);
                false
            }),
        )
        .unwrap();
    catalog_work_capture::capture(&store, &grant(&store), "default", "work").unwrap();
    assert!(calls.load(Ordering::SeqCst) > 0);
    calls.store(0, Ordering::SeqCst);
    let _:i64=store.db.query_row("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100) SELECT sum(x) FROM n",[],|r|r.get(0)).unwrap();
    assert!(
        calls.load(Ordering::SeqCst) > 100,
        "capture must not reset an existing VM owner"
    );
    store.db.progress_handler(0, None::<fn() -> bool>).unwrap();
}

#[test]
fn item_tag_vocabulary_and_identity_facts_are_complete_and_disconnected_tags_are_irrelevant() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("native.db");
    migration::initialize_native_at(&path, 39).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    curation::item_put(
        &store,
        &json!({"id":"work","kind":"video","metadata":{"title":"Tagged"}}),
    )
    .unwrap();
    // Explicit database fixture supplies observations to the read owner. This
    // does not claim mutation authorization or contribution-write coverage.
    store.db.execute_batch("INSERT INTO tags(id,name,description) VALUES('tag','genre:drama',''),('parent','genre',''); INSERT INTO tag_names(name,tag_id) VALUES('Drama','tag'); INSERT INTO tag_parents(child_id,parent_id) VALUES('tag','parent'); INSERT INTO item_tags(id,item_id,tag_id,source,confidence,note,active,created_at,updated_at) VALUES('assertion','work','tag','motion.local',1.0,'',1,'fixture','fixture'); INSERT INTO identities(namespace,value,item_id) VALUES('test.provider','identity','work');").unwrap();
    let first = catalog_work_capture::capture(&store, &grant(&store), "default", "work").unwrap();
    assert_eq!(
        first.dependencies()["item_tags"].as_array().unwrap().len(),
        1
    );
    assert_eq!(first.dependencies()["tags"].as_array().unwrap().len(), 2);
    assert_eq!(
        first.dependencies()["identities"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        first.dependencies()["catalog_legacy_identity_evidence"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    store
        .db
        .execute(
            "INSERT INTO tags(id,name,description) VALUES('unrelated','unrelated','')",
            [],
        )
        .unwrap();
    let unrelated =
        catalog_work_capture::capture(&store, &grant(&store), "default", "work").unwrap();
    assert_eq!(first.dependencies(), unrelated.dependencies());
    store
        .db
        .execute(
            "UPDATE tag_names SET name='Dramatic' WHERE tag_id='tag'",
            [],
        )
        .unwrap();
    let renamed = catalog_work_capture::capture(&store, &grant(&store), "default", "work").unwrap();
    assert_ne!(first.dependencies(), renamed.dependencies());
    store
        .db
        .execute("UPDATE item_tags SET active=0 WHERE id='assertion'", [])
        .unwrap();
    let inactive =
        catalog_work_capture::capture(&store, &grant(&store), "default", "work").unwrap();
    assert_eq!(inactive.dependencies()["item_tags"][0]["active"], 0);
    assert_ne!(renamed.dependencies(), inactive.dependencies());
}

#[test]
fn missing_tag_entity_head_refuses_the_whole_capture() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("native.db");
    migration::initialize_native_at(&path, 39).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    curation::item_put(
        &store,
        &json!({"id":"work","kind":"video","metadata":{"title":"Tagged"}}),
    )
    .unwrap();
    store.db.execute_batch("INSERT INTO tags(id,name,description) VALUES('tag','tag',''); INSERT INTO item_tags(id,item_id,tag_id,source,confidence,note,active,created_at,updated_at) VALUES('assertion','work','tag','motion.local',NULL,'',1,'fixture','fixture'); DELETE FROM catalog_entity_revisions WHERE entity_kind='tags' AND entity_key=json_array('tag');").unwrap();
    assert!(catalog_work_capture::capture(&store, &grant(&store), "default", "work").is_err());
}
