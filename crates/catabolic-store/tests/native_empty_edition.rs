//! Native command-owner qualification only; no Motion HTTP/serving claim.
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility};
use catabolic_store::{
    Store,
    catalog_commands::{
        self, CatalogAuthority, CommandOutcome, CommandStoreErrorCode, ReceiptPolicy,
        TrustedCommandAuthority,
    },
    catalog_edition_creation::{EmptyEditionAuthority, EmptyEditionRequest},
    curation, migration,
};
use serde_json::{Value, json};
use std::time::Duration;
fn setup() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("native.db");
    migration::initialize_native_at(&path, 40).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    curation::item_put(&store,&json!({"id":"work","kind":"video","metadata":{"title":"Work","motion_catalog":{"contract_version":1,"item_id":"work","kind":"video","media_type":"movie"}}})).unwrap();
    (root, store)
}
fn request() -> EmptyEditionRequest {
    EmptyEditionRequest {
        contract_version: 1,
        work_id: "work".into(),
        edition_id: "edition".into(),
        timeline_id: "edition".into(),
        relationship_id: "edge".into(),
        context_id: "context".into(),
        label: "Original cut".into(),
        request_digest: "a".repeat(64),
    }
}
fn trusted_grant(store: &Store) -> EmptyEditionAuthority {
    EmptyEditionAuthority{host_namespace:"host".into(),profile:"default".into(),work_id:"work".into(),expected_work_revision:store.db.query_row("SELECT revision FROM catalog_entity_revisions WHERE entity_kind='items' AND entity_key='[\"work\"]'",[],|r|r.get(0)).unwrap(),edition_id:"edition".into(),timeline_id:"edition".into(),relationship_id:"edge".into(),context_id:"context".into(),context_key:"motion.edition:edition".into()}
}
fn command(store: &Store, key: &str, request: &EmptyEditionRequest) -> CommandEnvelope {
    CommandEnvelope::new(
        1,
        CommandKey::new(&store.database_id, "host", key).unwrap(),
        store.catalog_incarnation.as_deref().unwrap(),
        store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap(),
        &json!({"operation":"empty_edition_create","request":request}).to_string(),
    )
    .unwrap()
}
fn run(
    store: &Store,
    command: &CommandEnvelope,
    grant: Option<&EmptyEditionAuthority>,
) -> Result<CommandOutcome, catabolic_store::catalog_commands::CommandStoreError> {
    let base = CatalogAuthority::new(TrustedCommandAuthority {
        host_namespace: "host",
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: true,
        identity_authority: None,
    });
    let authority = match grant {
        Some(grant) => base.with_empty_edition(grant),
        None => base,
    };
    catalog_commands::execute_with_authority(
        store,
        command,
        authority,
        ReceiptPolicy {
            retention: Duration::from_secs(3600),
            max_receipts: 100,
            max_logical_bytes: 1024 * 1024,
            max_hold_records: 100,
        },
    )
}
fn result(outcome: CommandOutcome) -> Value {
    match outcome {
        CommandOutcome::Committed { result, .. } | CommandOutcome::Replay { result, .. } => result,
        other => panic!("unexpected {}", other.code()),
    }
}
fn no_partial(store: &Store) {
    for table in [
        "catalog_timelines",
        "catalog_segmentation_contexts",
        "item_relationships",
        "catalog_command_receipts",
    ] {
        let count: i64 = store
            .db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
    let items: i64 = store
        .db
        .query_row("SELECT count(*) FROM items", [], |r| r.get(0))
        .unwrap();
    assert_eq!(items, 1);
}
#[test]
fn one_receipt_creates_exact_empty_edition_and_timeline_then_replays_after_restart() {
    let (root, store) = setup();
    let grant = trusted_grant(&store);
    let command = command(&store, "create", &request());
    let committed = result(run(&store, &command, Some(&grant)).unwrap());
    assert_eq!(
        committed["item_revision_baselines"]["motion.edition:edition"],
        1
    );
    assert_eq!(committed["timeline_revision"], 1);
    let row:(String,i64,String,Option<i64>,Option<i64>)=store.db.query_row("SELECT t.edition_item_id,t.current_revision,r.identity_state,r.coverage_start_us,r.coverage_end_us FROM catalog_timelines t JOIN catalog_timeline_revisions r ON r.timeline_id=t.id AND r.revision=t.current_revision",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap();
    assert_eq!(
        row,
        (
            "motion.edition:edition".into(),
            1,
            "provisional".into(),
            None,
            None
        )
    );
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM catalog_versions", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let generation: i64 = store
        .db
        .query_row("SELECT generation FROM catalog_state", [], |r| r.get(0))
        .unwrap();
    let capture = catabolic_store::catalog_work_capture::capture(
        &store,
        &catabolic_store::catalog_projection::TrustedProjectionAuthority {
            catalog_id: store.database_id.clone(),
            incarnation: store.catalog_incarnation.clone().unwrap(),
            expected_generation: generation,
        },
        "default",
        "work",
    )
    .unwrap();
    assert_eq!(
        capture.dependencies()["timelines"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        capture.dependencies()["versions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let authority = catabolic_store::catalog_projection::TrustedProjectionAuthority {
        catalog_id: store.database_id.clone(),
        incarnation: store.catalog_incarnation.clone().unwrap(),
        expected_generation: generation,
    };
    let timeline = catabolic_store::catalog_projection::named_timeline(
        &store,
        &authority,
        "edition",
        "motion.edition:edition",
    )
    .unwrap();
    assert_eq!(timeline["active"], true);
    assert_eq!(timeline["playable"], false);
    assert_eq!(timeline["timeline"]["current_revision"], 1);
    assert!(timeline["versions"].as_array().unwrap().is_empty());
    assert_eq!(timeline["revision_contexts"][0]["id"], "context");
    assert!(
        catabolic_store::catalog_projection::named_timeline(
            &store,
            &authority,
            "edition",
            "wrong-edition"
        )
        .is_err()
    );
    assert!(
        catabolic_store::catalog_projection::named_timeline(
            &store,
            &catabolic_store::catalog_projection::TrustedProjectionAuthority {
                expected_generation: generation - 1,
                ..authority
            },
            "edition",
            "motion.edition:edition"
        )
        .is_err()
    );
    drop(store);
    let store = Store::open(&root.path().join("native.db"), true, false).unwrap();
    // Full receipt visibility is independent from mutation authority. Replay
    // does not mint a new grant or require a current creation precondition.
    assert_eq!(result(run(&store, &command, None).unwrap()), committed);
    assert_eq!(
        store
            .db
            .query_row("SELECT generation FROM catalog_state", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        generation
    );
}
#[test]
fn missing_grant_wrong_work_head_and_late_identity_failure_roll_back_every_effect() {
    for case in 0..4 {
        let (_root, store) = setup();
        let mut grant = trusted_grant(&store);
        let command = command(&store, "denied", &request());
        match case {
            1 => grant.work_id = "different".into(),
            2 => grant.expected_work_revision += 1,
            3 => grant.profile = "missing-profile".into(),
            _ => {}
        }
        let error = run(
            &store,
            &command,
            if case == 0 { None } else { Some(&grant) },
        )
        .unwrap_err();
        assert!(matches!(
            error.code,
            CommandStoreErrorCode::AuthorityDenied
                | CommandStoreErrorCode::OperationConflict
                | CommandStoreErrorCode::InvalidOperation
        ));
        no_partial(&store);
    }
}
#[test]
fn exact_allocation_container_and_existing_ids_are_not_overwritten() {
    let (_root, store) = setup();
    let grant = trusted_grant(&store);
    let mut changed = request();
    changed.edition_id = "other".into();
    changed.timeline_id = "other".into();
    assert!(
        run(
            &store,
            &command(&store, "wrong-allocation", &changed),
            Some(&grant)
        )
        .is_err()
    );
    no_partial(&store);
    let first = command(&store, "first", &request());
    run(&store, &first, Some(&grant)).unwrap();
    let second = command(&store, "second", &request());
    let error = run(&store, &second, Some(&grant)).unwrap_err();
    assert_eq!(error.code, CommandStoreErrorCode::OperationConflict);
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM catalog_command_receipts", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        1
    );
    let (_root, store) = setup();
    curation::item_put(&store,&json!({"id":"work","kind":"video","metadata":{"motion_catalog":{"contract_version":1,"item_id":"work","media_type":"series"}}})).unwrap();
    assert!(
        run(
            &store,
            &command(&store, "container", &request()),
            Some(&trusted_grant(&store))
        )
        .is_err()
    );
    no_partial(&store);
}

#[test]
fn named_empty_timeline_reports_later_real_native_identity_head_not_origin_metadata() {
    let (_root, store) = setup();
    let grant = trusted_grant(&store);
    run(
        &store,
        &command(&store, "create-before-edit", &request()),
        Some(&grant),
    )
    .unwrap();
    let identity = catabolic_store::catalog_identity_mutation::IdentityAuthority {
        authority_namespace: "host".into(),
        context_keys: std::collections::BTreeSet::from([grant.context_key.clone()]),
        allow_declared_evidence: false,
        allow_verified_evidence: false,
    };
    let generation = store
        .db
        .query_row("SELECT generation FROM catalog_state", [], |r| r.get(0))
        .unwrap();
    let change=CommandEnvelope::new(1,CommandKey::new(&store.database_id,"host","edit-timeline").unwrap(),store.catalog_incarnation.as_deref().unwrap(),generation,&json!({"operation":"identity","mutation":{"contract_version":1,"profile":"default","context":{"id":"context","authority_namespace":"host","context_key":grant.context_key,"expected_revision":1},"timelines":[{"timeline_id":"edition","edition_item_id":"motion.edition:edition","expected_revision":1,"identity_state":"established","coverage":{"start_us":0,"end_us":2000000}}],"versions":[],"evidence":[]}}).to_string()).unwrap();
    catalog_commands::execute_with_authority(
        &store,
        &change,
        CatalogAuthority::new(TrustedCommandAuthority {
            host_namespace: "host",
            receipt_visibility: ReceiptVisibility::FullResult,
            allow_new_commands: true,
            identity_authority: Some(&identity),
        }),
        ReceiptPolicy {
            retention: Duration::from_secs(3600),
            max_receipts: 100,
            max_logical_bytes: 1024 * 1024,
            max_hold_records: 100,
        },
    )
    .unwrap();
    let generation = store
        .db
        .query_row("SELECT generation FROM catalog_state", [], |r| r.get(0))
        .unwrap();
    let named = catabolic_store::catalog_projection::named_timeline(
        &store,
        &catabolic_store::catalog_projection::TrustedProjectionAuthority {
            catalog_id: store.database_id.clone(),
            incarnation: store.catalog_incarnation.clone().unwrap(),
            expected_generation: generation,
        },
        "edition",
        "motion.edition:edition",
    )
    .unwrap();
    assert_eq!(named["timeline"]["current_revision"], 2);
    assert_eq!(named["timeline"]["identity_state"], "established");
    assert_eq!(named["timeline"]["coverage_end_us"], 2000000);
    assert_eq!(named["revision_contexts"][0]["current_revision"], 2);
    assert!(named["versions"].as_array().unwrap().is_empty());
    assert_eq!(named["playable"], false);
    assert_eq!(
        named["edition_metadata"]["motion_edition"]["source_timelines"][0]["revision"], 1,
        "immutable baseline is not current timeline authority"
    );
}
