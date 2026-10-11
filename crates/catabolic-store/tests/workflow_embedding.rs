// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Selected public-adapter owner regressions on disposable catalogs.
use catabolic_store::{Store, curation, migration};
use serde_json::{Value, json};

#[test]
fn proposal_list_preserves_sqlite_scalar_cursor_binding() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    migration::initialize(&path).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    store
        .db
        .execute("INSERT OR IGNORE INTO profiles VALUES ('default')", [])
        .unwrap();
    store
        .db
        .execute("INSERT OR IGNORE INTO locations VALUES ('media')", [])
        .unwrap();
    store
        .db
        .execute(
            "INSERT INTO files(id,location,path) VALUES ('file','media','fixture')",
            [],
        )
        .unwrap();
    for id in ["0-first", "2-second", "a-third"] {
        store.db.execute(
            "INSERT INTO proposals(id,profile,file_id,kind,payload,evidence,source,snapshot) VALUES (?,'default','file','associate','{}','{}','cursor-test','{}')",
            [id],
        ).unwrap();
    }
    let ids = |cursor: Value| {
        curation::dispatch(&store, "default", "proposal-list", &json!({"after":cursor}))
            .unwrap()["proposals"].as_array().unwrap().iter()
            .map(|row| row["id"].as_str().unwrap().to_owned()).collect::<Vec<_>>()
    };
    for empty in [
        Value::Null,
        json!(false),
        json!(0),
        json!(0.0),
        json!(""),
        json!([]),
        json!({}),
    ] {
        assert_eq!(ids(empty), ["0-first", "2-second", "a-third"]);
    }
    for scalar in [json!(true), json!(1), json!(1.5), json!("1")] {
        assert_eq!(ids(scalar), ["2-second", "a-third"]);
    }
    assert_eq!(ids(json!(9)), ["a-third"]);
    for invalid in [
        json!([1]),
        json!({"after":1}),
        json!(18446744073709551615_u64),
    ] {
        assert!(
            curation::dispatch(
                &store,
                "default",
                "proposal-list",
                &json!({"after":invalid})
            )
            .is_err()
        );
    }
}

#[test]
fn sidecars_preserve_sqlite_scalar_cursor_binding() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    migration::initialize(&path).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    store.db.execute_batch(
        "INSERT OR IGNORE INTO profiles VALUES ('default');
         INSERT OR IGNORE INTO locations VALUES ('media');
         INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','media',1,2,'[]');
         INSERT INTO files(id,location,path) VALUES ('primary','media','movie.mkv'),('0-subtitle','media','movie.en.srt');
         INSERT INTO observations(profile,file_id,size,mtime_ns,device,inode,status,scan_id) VALUES ('default','primary',1,1,1,1,'present','scan'),('default','0-subtitle',1,1,1,1,'present','scan');
         INSERT INTO items(id,kind,metadata) VALUES ('item','movie','{}');
         INSERT INTO item_files(id,file_id,item_id,role,origin) VALUES ('association','primary','item','primary','explicit');"
    ).unwrap();
    for cursor in [
        Value::Null,
        json!(false),
        json!(0),
        json!(0.0),
        json!(""),
        json!([]),
        json!({}),
    ] {
        let result = curation::sidecars(&store, "default", &json!({"after":cursor})).unwrap();
        assert_eq!(result["candidates"].as_array().unwrap().len(), 1);
    }
    for cursor in [json!(true), json!(1), json!(1.5), json!("1")] {
        let result = curation::sidecars(&store, "default", &json!({"after":cursor})).unwrap();
        assert_eq!(result["candidates"], json!([]));
    }
    for invalid in [json!([1]), json!({"after":1})] {
        assert!(curation::sidecars(&store, "default", &json!({"after":invalid})).is_err());
    }
}
