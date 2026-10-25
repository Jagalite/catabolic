//! Retained-writer command replay and immutable scope preparation.
use catabolic_core::{
    catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility},
    catalog_host_lease::HostAuthorization,
};
use catabolic_runtime::host::{CONTRACT_VERSION, HostSession};
use catabolic_store::{
    catalog_commands::{CommandOutcome, ReceiptPolicy, TrustedCommandAuthority},
    migration,
};
use serde_json::json;
use std::time::Duration;

#[test]
fn retained_owner_commands_replay_and_reject_wrong_gate_namespace() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.sqlite3");
    migration::initialize_native(&path).unwrap();
    let gate = HostAuthorization::new("motion-test".into(), "revision-1".into()).unwrap();
    let mut host =
        HostSession::open_authorized(&path, "default", true, CONTRACT_VERSION, gate.clone())
            .unwrap();
    let identity = host.identity().unwrap();
    let (exists, observed_generation) = host.observe_item_for_create("work-one").unwrap();
    assert!(!exists);
    assert_eq!(observed_generation, host.catalog_generation().unwrap());
    let command = CommandEnvelope::new(1,
        CommandKey::new(identity["catalog_id"].as_str().unwrap(), "motion-test", "create-one").unwrap(),
        identity["catalog_incarnation"].as_str().unwrap(), host.catalog_generation().unwrap(),
        &json!({"operation":"item_put_batch","items":[{"id":"work-one","kind":"video","metadata":{"title":"One"}}]}).to_string()).unwrap();
    let policy = ReceiptPolicy {
        retention: Duration::from_secs(3600),
        max_receipts: 100,
        max_logical_bytes: 1024 * 1024,
        max_hold_records: 100,
    };
    let authority = TrustedCommandAuthority {
        host_namespace: "motion-test",
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: true,
        identity_authority: None,
    };
    assert!(matches!(
        host.execute_command(&command, authority, policy).unwrap(),
        CommandOutcome::Committed { .. }
    ));
    assert!(matches!(
        host.execute_command(&command, authority, policy).unwrap(),
        CommandOutcome::Replay { .. }
    ));
    let (exists, next_generation) = host.observe_item_for_create("work-one").unwrap();
    assert!(exists);
    assert!(next_generation > observed_generation);
    let shape = host
        .item_projection_shapes(&["work-one".into()], &["title".into()])
        .unwrap();
    assert_eq!(shape["items"][0]["requires_rich_projection"], false);
    let shape = host
        .item_projection_shapes(&["work-one".into()], &[])
        .unwrap();
    assert_eq!(shape["items"][0]["requires_rich_projection"], true);
    assert!(
        host.execute_command(
            &command,
            TrustedCommandAuthority {
                host_namespace: "other-host",
                ..authority
            },
            policy
        )
        .is_err()
    );
    let definition = json!({"version":1,"mode":"selection","selection":{"language":"sql","query":"SELECT item_id FROM catalog_items ORDER BY item_id"}});
    let first = host.prepare_selection("motion-scope", &definition).unwrap();
    let second = host.prepare_selection("motion-scope", &definition).unwrap();
    assert_eq!(first["id"], second["id"]);
    assert_eq!(first["digest"], second["digest"]);
    gate.begin_revocation().unwrap();
    assert!(host.execute_command(&command, authority, policy).is_err());
    assert!(host.prepare_selection("motion-scope", &definition).is_err());
    assert!(host.catalog_generation().is_err());
}

#[test]
fn retained_logical_preview_preserves_namespace_generation_and_revocation_fences() {
    use catabolic_store::catalog_logical_edits::{
        LogicalChange, LogicalEditAuthority, LogicalEditRequest,
    };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("logical.sqlite3");
    migration::initialize_native(&path).unwrap();
    let gate = HostAuthorization::new("motion-test".into(), "revision-1".into()).unwrap();
    let mut host =
        HostSession::open_authorized(&path, "default", true, CONTRACT_VERSION, gate.clone())
            .unwrap();
    let identity = host.identity().unwrap();
    let command = CommandEnvelope::new(1,
        CommandKey::new(identity["catalog_id"].as_str().unwrap(), "motion-test", "seed-logical").unwrap(),
        identity["catalog_incarnation"].as_str().unwrap(), host.catalog_generation().unwrap(),
        &json!({"operation":"item_put_batch","items":[{"id":"source","kind":"video","metadata":{}},{"id":"survivor","kind":"video","metadata":{}}]}).to_string()).unwrap();
    host.execute_command(
        &command,
        TrustedCommandAuthority {
            host_namespace: "motion-test",
            receipt_visibility: ReceiptVisibility::FullResult,
            allow_new_commands: true,
            identity_authority: None,
        },
        ReceiptPolicy {
            retention: Duration::from_secs(3600),
            max_receipts: 100,
            max_logical_bytes: 1024 * 1024,
            max_hold_records: 100,
        },
    )
    .unwrap();
    let request = LogicalEditRequest {
        contract_version: 1,
        change: LogicalChange::Merge {
            sources: vec!["source".into()],
            survivor: "survivor".into(),
        },
        new_items: vec![],
        association_clones: vec![],
        contexts: vec![],
    };
    let grant = LogicalEditAuthority {
        host_namespace: "motion-test".into(),
        item_ids: ["source".into(), "survivor".into()].into_iter().collect(),
        new_item_ids: Default::default(),
    };
    let generation = host.catalog_generation().unwrap();
    let named = host
        .projection_named_items(
            generation,
            &["survivor".into(), "source".into(), "missing".into()],
        )
        .unwrap();
    assert_eq!(named["items"].as_array().unwrap().len(), 2);
    assert_eq!(named["items"][0]["item_id"], "source");
    assert_eq!(named["items"][0]["retirement"], serde_json::Value::Null);
    assert_eq!(named["missing_ids"], json!(["missing"]));
    assert_eq!(host.catalog_generation().unwrap(), generation);
    assert!(
        host.projection_named_items(generation - 1, &["source".into()])
            .is_err()
    );
    assert!(
        host.projection_named_items(generation, &["source".into(), "source".into()])
            .is_err()
    );
    assert!(
        host.projection_named_items(
            generation,
            &(0..201).map(|id| format!("item-{id}")).collect::<Vec<_>>()
        )
        .is_err()
    );
    let preview = host
        .preview_logical_edit(generation, request.clone(), &grant, None)
        .unwrap();
    assert_eq!(preview.reviewed.expected_generation, generation);
    assert_eq!(host.catalog_generation().unwrap(), generation);
    assert!(
        host.preview_logical_edit(generation - 1, request.clone(), &grant, None)
            .is_err()
    );
    let mut wrong = grant.clone();
    wrong.host_namespace = "other-host".into();
    assert!(
        host.preview_logical_edit(generation, request.clone(), &wrong, None)
            .is_err()
    );
    wrong = grant.clone();
    wrong.item_ids.remove("source");
    assert!(
        host.preview_logical_edit(generation, request.clone(), &wrong, None)
            .is_err()
    );
    gate.begin_revocation().unwrap();
    assert!(
        host.projection_named_items(generation, &["source".into()])
            .is_err()
    );
    assert!(
        host.preview_logical_edit(generation, request, &grant, None)
            .is_err()
    );
}
