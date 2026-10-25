use catabolic_store::{
    Store,
    catalog_projection::{self, TrustedProjectionAuthority},
    curation, migration,
};
use serde_json::json;
#[test]
fn projection_pages_preserve_exact_native_rows_and_reject_stale_identity() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog");
    migration::initialize_native_at(&path, 39).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    for id in ["a", "b", "c"] {
        curation::item_put(&store,&json!({"id":id,"kind":"video","metadata":{"title":id,"motion_catalog":{"item_id":id,"catalog_revision":1}}})).unwrap();
    }
    let grant = TrustedProjectionAuthority {
        catalog_id: store.database_id.clone(),
        incarnation: store.catalog_incarnation.clone().unwrap(),
        expected_generation: store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap(),
    };
    let first = catalog_projection::item_page(&store, &grant, None, 2).unwrap();
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    assert_eq!(first["next_after_id"], "b");
    assert_eq!(
        first["items"][0]["metadata"]["motion_catalog"]["item_id"],
        "a"
    );
    assert!(first["items"][0]["item_revision"].as_i64().unwrap() > 0);
    assert!(first["items"][0]["retirement"].is_null());
    let last = catalog_projection::item_page(&store, &grant, Some("b"), 2).unwrap();
    assert_eq!(last["items"][0]["item_id"], "c");
    assert!(last["next_after_id"].is_null());
    let mut wrong = grant.clone();
    wrong.incarnation = "wrong".into();
    assert!(catalog_projection::item_page(&store, &wrong, None, 2).is_err());
    assert!(catalog_projection::item_page(&store, &grant, None, 0).is_err());
    curation::item_put(&store, &json!({"id":"d","kind":"video","metadata":{}})).unwrap();
    assert!(catalog_projection::item_page(&store, &grant, Some("b"), 2).is_err());
}
