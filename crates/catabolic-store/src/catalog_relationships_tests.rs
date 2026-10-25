use super::*;
use crate::{
    catalog_commands::{
        self, CatalogAuthority, CommandOutcome, ReceiptPolicy, TrustedCommandAuthority,
    },
    curation, migration,
};
use catabolic_core::{
    catalog_command::{CommandKey, ReceiptVisibility},
    encode,
};

const HOST: &str = "relationship-owner";
const KIND: &str = "custom:contains";
struct Fixture {
    store: Store,
    _root: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        Self::at_schema(34)
    }
    fn at_schema(schema: u32) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("native.sqlite3");
        migration::initialize_native_at(&path, schema).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        for id in ["a", "b", "c", "hidden"] {
            curation::item_put(&store, &json!({"id":id,"kind":"video","metadata":{}})).unwrap();
        }
        Self { store, _root: root }
    }
    fn generation(&self) -> i64 {
        self.store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap()
    }
    fn command(&self, operation_id: &str, mutation: &RelationshipMutation) -> CommandEnvelope {
        self.payload(
            operation_id,
            json!({"operation":"relationships","mutation":mutation}),
        )
    }
    fn payload(&self, operation_id: &str, payload: Value) -> CommandEnvelope {
        CommandEnvelope::new(
            1,
            CommandKey::new(&self.store.database_id, HOST, operation_id).unwrap(),
            self.store.catalog_incarnation.as_deref().unwrap(),
            self.generation(),
            &encode(&payload),
        )
        .unwrap()
    }
    fn execute(
        &self,
        command: &CommandEnvelope,
        grant: &RelationshipAuthority,
    ) -> Result<CommandOutcome> {
        self.execute_policy(command, grant, policy())
    }
    fn execute_policy(
        &self,
        command: &CommandEnvelope,
        grant: &RelationshipAuthority,
        policy: ReceiptPolicy,
    ) -> Result<CommandOutcome> {
        catalog_commands::execute_with_authority(
            &self.store,
            command,
            CatalogAuthority::new(base()).with_relationships(grant),
            policy,
        )
    }
    fn count(&self, table: &str) -> i64 {
        self.store
            .db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    fn revision(&self, id: &str) -> (i64, i64) {
        self.store.db.query_row(
            "SELECT revision,deleted FROM catalog_entity_revisions WHERE entity_kind='item_relationships' AND entity_key=json_array(?)",
            [id], |r| Ok((r.get(0)?,r.get(1)?)),
        ).unwrap()
    }
}
fn base() -> TrustedCommandAuthority<'static> {
    TrustedCommandAuthority {
        host_namespace: HOST,
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: true,
        identity_authority: None,
    }
}
fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::from_secs(60),
        max_receipts: 1000,
        max_logical_bytes: 16 * 1024 * 1024,
        max_hold_records: 10,
    }
}
fn edge(source: &str, target: &str) -> RelationshipEdge {
    RelationshipEdge {
        source: source.into(),
        target: target.into(),
        kind: KIND.into(),
    }
}
fn grant(edges: Vec<RelationshipEdge>) -> RelationshipAuthority {
    RelationshipAuthority {
        host_namespace: HOST.into(),
        create_edges: edges.into_iter().collect(),
        kind_rules: [(KIND.into(), RelationshipKindRule::Acyclic)].into(),
        delete_edges: BTreeMap::new(),
    }
}
fn create(id: &str, source: &str, target: &str) -> RelationshipMutation {
    RelationshipMutation::Create {
        id: id.into(),
        source: source.into(),
        target: target.into(),
        kind: KIND.into(),
        metadata: json!({"application_ordinal":0,"nested":{"retained":true}}),
    }
}
fn remove(
    id: &str,
    source: &str,
    target: &str,
    revision: i64,
) -> (RelationshipMutation, RelationshipAuthority) {
    let expected_edge = edge(source, target);
    let mut grant = grant(vec![]);
    grant.delete_edges.insert(
        id.into(),
        AuthorizedRelationshipDelete {
            expected_revision: revision,
            edge: expected_edge.clone(),
        },
    );
    (
        RelationshipMutation::Delete {
            id: id.into(),
            expected_revision: revision,
            expected_edge,
        },
        grant,
    )
}
fn committed(outcome: CommandOutcome) -> Value {
    match outcome {
        CommandOutcome::Committed { result, .. } => result,
        other => panic!("expected commit: {other:?}"),
    }
}

#[test]
fn create_preserves_zero_duplicate_ordinals_and_replays_exact_full_receipt() {
    for schema in [34, migration::NATIVE_SCHEMA_VERSION] {
        let f = Fixture::at_schema(schema);
        let mutation = create("edge-a", "a", "b");
        let command = f.command("create-a", &mutation);
        let result = committed(f.execute(&command, &grant(vec![edge("a", "b")])).unwrap());
        assert_eq!(result["relationship"]["id"], "edge-a");
        assert_eq!(result["relationship"]["revision"], 1);
        assert_eq!(result["relationship"]["position"], Value::Null);
        assert_eq!(result["relationship"]["metadata"]["application_ordinal"], 0);
        assert_eq!(f.revision("edge-a"), (1, 0));
        let other = f.command("create-c", &create("edge-c", "c", "b"));
        committed(f.execute(&other, &grant(vec![edge("c", "b")])).unwrap());
        let count: i64 = f.store.db.query_row("SELECT count(*) FROM item_relationships WHERE position IS NULL AND json_extract(metadata,'$.application_ordinal')=0", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 2);
        let generation = f.generation();
        // Independent full-result read authority permits receipt replay even
        // after mutation grants were withdrawn and its generation is stale.
        let replay = catalog_commands::execute(&f.store, &command, base(), policy()).unwrap();
        assert!(
            matches!(replay, CommandOutcome::Replay { result: ref replayed, .. } if *replayed == result)
        );
        assert_eq!(f.generation(), generation);
        assert_eq!(f.count("catalog_command_receipts"), 2);
    }
}

#[test]
fn duplicate_edge_id_collision_and_command_collision_never_upsert() {
    let f = Fixture::new();
    let original = f.command("first", &create("edge-a", "a", "b"));
    let authority = grant(vec![edge("a", "b"), edge("a", "c")]);
    committed(f.execute(&original, &authority).unwrap());
    let generation = f.generation();
    for (operation, mutation, message) in [
        ("same-id", create("edge-a", "a", "c"), "id_already_used"),
        ("same-edge", create("edge-new", "a", "b"), "duplicate_edge"),
    ] {
        let error = f
            .execute(&f.command(operation, &mutation), &authority)
            .unwrap_err();
        assert_eq!(error.code, CommandStoreErrorCode::OperationConflict);
        assert!(error.to_string().contains(message));
        assert_eq!(f.generation(), generation);
        assert_eq!(f.count("item_relationships"), 1);
        assert_eq!(f.count("catalog_command_receipts"), 1);
    }
    let collided = f.command("first", &create("different", "a", "c"));
    assert!(matches!(
        f.execute(&collided, &authority).unwrap(),
        CommandOutcome::Conflict
    ));
    assert_eq!(f.generation(), generation);
}

#[test]
fn self_edges_missing_endpoints_and_wrong_independent_grants_refuse_atomically() {
    let f = Fixture::new();
    let generation = f.generation();
    let self_edge = f.command("self", &create("self", "a", "a"));
    assert_eq!(
        f.execute(&self_edge, &grant(vec![edge("a", "a")]))
            .unwrap_err()
            .code,
        CommandStoreErrorCode::InvalidOperation
    );
    let request = f.command("wrong-grant", &create("edge-a", "a", "b"));
    let mut no_rule = grant(vec![edge("a", "b")]);
    no_rule.kind_rules.clear();
    let mut wrong_host = grant(vec![edge("a", "b")]);
    wrong_host.host_namespace = "another-host".into();
    for unauthorized in [
        grant(vec![edge("b", "a")]),
        grant(vec![edge("a", "c")]),
        no_rule,
        wrong_host,
    ] {
        assert_eq!(
            f.execute(&request, &unauthorized).unwrap_err().code,
            CommandStoreErrorCode::AuthorityDenied
        );
    }
    let absent = f.command("absent", &create("absent", "a", "missing"));
    assert!(
        f.execute(&absent, &grant(vec![edge("a", "missing")]))
            .unwrap_err()
            .to_string()
            .contains("endpoint_not_live")
    );
    assert_eq!(f.generation(), generation);
    assert_eq!(f.count("item_relationships"), 0);
    assert_eq!(f.count("catalog_command_receipts"), 0);
}

#[test]
fn complete_per_kind_graph_includes_unauthorized_intermediate_nodes() {
    let f = Fixture::new();
    for (id, source, target) in [("ah", "a", "hidden"), ("hb", "hidden", "b")] {
        committed(
            f.execute(
                &f.command(id, &create(id, source, target)),
                &grant(vec![edge(source, target)]),
            )
            .unwrap(),
        );
    }
    let command = f.command("cycle", &create("cycle", "b", "a"));
    let generation = f.generation();
    let error = f
        .execute(&command, &grant(vec![edge("b", "a")]))
        .unwrap_err();
    assert!(error.to_string().contains("catalog_relationship_cycle"));
    assert_eq!(f.generation(), generation);
    let mut directed = grant(vec![edge("b", "a")]);
    directed
        .kind_rules
        .insert(KIND.into(), RelationshipKindRule::Directed);
    committed(f.execute(&command, &directed).unwrap());
    let mut different = create("different-kind", "b", "a");
    let RelationshipMutation::Create { kind, .. } = &mut different else {
        unreachable!()
    };
    *kind = "custom:other".into();
    let mut different_grant = grant(vec![]);
    different_grant.create_edges.insert(RelationshipEdge {
        source: "b".into(),
        target: "a".into(),
        kind: "custom:other".into(),
    });
    different_grant
        .kind_rules
        .insert("custom:other".into(), RelationshipKindRule::Acyclic);
    committed(
        f.execute(&f.command("different-kind", &different), &different_grant)
            .unwrap(),
    );
}

#[test]
fn delete_cas_exact_edge_grant_tombstone_and_replay() {
    let f = Fixture::new();
    committed(
        f.execute(
            &f.command("create", &create("edge-a", "a", "b")),
            &grant(vec![edge("a", "b")]),
        )
        .unwrap(),
    );
    f.store
        .db
        .execute(
            "UPDATE item_relationships SET metadata='{\"changed\":true}' WHERE id='edge-a'",
            [],
        )
        .unwrap();
    assert_eq!(f.revision("edge-a"), (2, 0));
    let (stale, stale_grant) = remove("edge-a", "a", "b", 1);
    assert!(
        f.execute(&f.command("stale", &stale), &stale_grant)
            .unwrap_err()
            .to_string()
            .contains("revision_conflict")
    );
    let (delete, delete_grant) = remove("edge-a", "a", "b", 2);
    let command = f.command("delete", &delete);
    let (_, wrong_grant) = remove("edge-a", "a", "c", 2);
    assert_eq!(
        f.execute(&command, &wrong_grant).unwrap_err().code,
        CommandStoreErrorCode::AuthorityDenied
    );
    let (wrong_intent, wrong_grant) = remove("edge-a", "a", "c", 2);
    assert!(
        f.execute(&f.command("wrong-edge", &wrong_intent), &wrong_grant)
            .unwrap_err()
            .to_string()
            .contains("edge_changed")
    );
    let result = committed(f.execute(&command, &delete_grant).unwrap());
    assert_eq!(result["relationship"]["metadata"]["changed"], true);
    assert_eq!(result["deleted_revision"], 3);
    assert_eq!(f.revision("edge-a"), (3, 1));
    assert_eq!(f.count("item_relationships"), 0);
    assert!(
        matches!(catalog_commands::execute(&f.store, &command, base(), policy()).unwrap(), CommandOutcome::Replay { result: ref replayed, .. } if *replayed == result)
    );
    let reused = f.command("reuse", &create("edge-a", "a", "c"));
    assert!(
        f.execute(&reused, &grant(vec![edge("a", "c")]))
            .unwrap_err()
            .to_string()
            .contains("id_already_used")
    );
    // A different explicit ID can recreate the same directed tuple, but the
    // old relationship identity and deletion receipt remain permanent.
    committed(
        f.execute(
            &f.command("recreate", &create("edge-new", "a", "b")),
            &grant(vec![edge("a", "b")]),
        )
        .unwrap(),
    );
    assert_eq!(f.revision("edge-a"), (3, 1));
    assert_eq!(f.revision("edge-new"), (1, 0));
}

#[test]
fn deletion_refuses_cascading_native_fk_dependencies() {
    let f = Fixture::new();
    committed(
        f.execute(
            &f.command("create", &create("edge-a", "a", "b")),
            &grant(vec![edge("a", "b")]),
        )
        .unwrap(),
    );
    f.store.db.execute_batch("CREATE TABLE relationship_consumer(id TEXT PRIMARY KEY, relationship_id TEXT REFERENCES item_relationships(id) ON DELETE CASCADE); INSERT INTO relationship_consumer VALUES('consumer','edge-a');").unwrap();
    let generation = f.generation();
    let (delete, authority) = remove("edge-a", "a", "b", 1);
    let error = f
        .execute(&f.command("delete", &delete), &authority)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("catalog_relationship_referenced"),
        "{error}"
    );
    assert_eq!(f.count("relationship_consumer"), 1);
    assert_eq!(f.count("item_relationships"), 1);
    assert_eq!(f.revision("edge-a"), (1, 0));
    assert_eq!(f.generation(), generation);
}

#[test]
fn receipt_write_failure_rolls_back_create_and_delete_including_revisions() {
    let f = Fixture::new();
    f.store.db.execute_batch("CREATE TRIGGER refuse_relationship_receipt BEFORE INSERT ON catalog_command_receipts BEGIN SELECT RAISE(ABORT,'test receipt refused'); END;").unwrap();
    let generation = f.generation();
    let command = f.command("create", &create("edge-a", "a", "b"));
    let authority = grant(vec![edge("a", "b")]);
    assert!(f.execute(&command, &authority).is_err());
    assert_eq!(f.count("item_relationships"), 0);
    assert_eq!(f.count("catalog_command_receipts"), 0);
    let tombstones: i64 = f
        .store
        .db
        .query_row(
            "SELECT count(*) FROM catalog_entity_revisions WHERE entity_kind='item_relationships'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tombstones, 0);
    assert_eq!(f.generation(), generation);
    f.store
        .db
        .execute_batch("DROP TRIGGER refuse_relationship_receipt")
        .unwrap();
    committed(f.execute(&command, &authority).unwrap());
    let generation = f.generation();
    f.store.db.execute_batch("CREATE TRIGGER refuse_relationship_receipt BEFORE INSERT ON catalog_command_receipts BEGIN SELECT RAISE(ABORT,'test receipt refused'); END;").unwrap();
    let (delete, authority) = remove("edge-a", "a", "b", 1);
    assert!(
        f.execute(&f.command("delete", &delete), &authority)
            .is_err()
    );
    assert_eq!(f.count("item_relationships"), 1);
    assert_eq!(f.revision("edge-a"), (1, 0));
    assert_eq!(f.count("catalog_command_receipts"), 1);
    assert_eq!(f.generation(), generation);
}

#[test]
fn storage_exhaustion_after_mutation_rolls_back_without_consuming_edge_id() {
    let f = Fixture::new();
    let command = f.command("create", &create("edge-a", "a", "b"));
    let authority = grant(vec![edge("a", "b")]);
    let generation = f.generation();
    let fixed = 256
        + command.key().catalog_id().len()
        + HOST.len()
        + command.key().operation_id().len()
        + command.digest().as_str().len()
        + command.admission_incarnation().len();
    let exhausted = ReceiptPolicy {
        max_logical_bytes: fixed as u64,
        ..policy()
    };
    assert!(matches!(
        f.execute_policy(&command, &authority, exhausted).unwrap(),
        CommandOutcome::ReceiptStorageExhausted
    ));
    assert_eq!(f.count("item_relationships"), 0);
    assert_eq!(f.count("catalog_command_receipts"), 0);
    assert_eq!(f.generation(), generation);
    committed(f.execute(&command, &authority).unwrap());
    assert_eq!(f.revision("edge-a"), (1, 0));
}

#[test]
fn stale_generation_and_strict_operation_shape_are_side_effect_free() {
    let f = Fixture::new();
    let command = f.command("stale", &create("edge-a", "a", "b"));
    curation::item_put(
        &f.store,
        &json!({"id":"a","kind":"video","metadata":{"changed":true}}),
    )
    .unwrap();
    let generation = f.generation();
    assert!(matches!(
        f.execute(&command, &grant(vec![edge("a", "b")])).unwrap(),
        CommandOutcome::StaleGeneration
    ));
    for mutation in [
        json!({"action":"create","id":"edge-a","source":"a","target":"b","kind":KIND,"metadata":{},"position":0}),
        json!({"action":"create","id":"edge-a","source":"a","target":"b","kind":KIND,"metadata":[]}),
        json!({"action":"delete","id":"edge-a","expected_revision":1}),
        json!({"action":"delete","id":"edge-a","expected_revision":0,"expected_edge":edge("a","b")}),
    ] {
        let command = f.payload(
            "invalid",
            json!({"operation":"relationships","mutation":mutation}),
        );
        assert_eq!(
            f.execute(&command, &grant(vec![edge("a", "b")]))
                .unwrap_err()
                .code,
            CommandStoreErrorCode::InvalidOperation
        );
    }
    assert_eq!(f.generation(), generation);
    assert_eq!(f.count("item_relationships"), 0);
    assert_eq!(f.count("catalog_command_receipts"), 0);
}

#[test]
fn graph_budget_and_deadline_refusals_are_closed() {
    let f = Fixture::new();
    let mut budget = Budget::new(&f.store).unwrap();
    budget.deadline = Instant::now() - Duration::from_secs(1);
    assert!(
        complete_graph(&f.store.db, KIND, &mut budget)
            .unwrap_err()
            .to_string()
            .contains("deadline")
    );
    budget.deadline = Instant::now() + Duration::from_secs(2);
    budget.rows = MAX_RELATIONSHIP_GRAPH_EDGES;
    assert!(
        budget
            .charge(1)
            .unwrap_err()
            .to_string()
            .contains("graph_budget")
    );
    let mut budget = Budget::new(&f.store).unwrap();
    assert!(
        budget
            .charge(MAX_RELATIONSHIP_GRAPH_BYTES + 1)
            .unwrap_err()
            .to_string()
            .contains("graph_budget")
    );
}

#[test]
fn durable_relationship_create_delete_remain_complete_capture_inputs() {
    use crate::{catalog_projection::TrustedProjectionAuthority, catalog_work_capture};
    let f = Fixture::at_schema(migration::NATIVE_SCHEMA_VERSION);
    let capture = |work: &str| {
        catalog_work_capture::capture(
            &f.store,
            &TrustedProjectionAuthority {
                catalog_id: f.store.database_id.clone(),
                incarnation: f.store.catalog_incarnation.clone().unwrap(),
                expected_generation: f.generation(),
            },
            "default",
            work,
        )
        .unwrap()
    };
    let before_source = capture("a");
    let before_target = capture("b");
    let id = "5412c522-5dc2-42ab-9708-dad308df8a91";
    let relationship = RelationshipEdge {
        source: "a".into(),
        target: "b".into(),
        kind: "custom:motion_part_of".into(),
    };
    let metadata = json!({"motion_relationship":{
        "contract_version":1,"id":id,"kind":"part_of","position":0,
    }});
    let mutation = RelationshipMutation::Create {
        id: id.into(),
        source: relationship.source.clone(),
        target: relationship.target.clone(),
        kind: relationship.kind.clone(),
        metadata: metadata.clone(),
    };
    let create_grant = RelationshipAuthority {
        host_namespace: HOST.into(),
        create_edges: [relationship.clone()].into(),
        kind_rules: [(relationship.kind.clone(), RelationshipKindRule::Acyclic)].into(),
        delete_edges: BTreeMap::new(),
    };
    let receipt = committed(
        f.execute(&f.command("capture-create", &mutation), &create_grant)
            .unwrap(),
    );
    assert_eq!(receipt["relationship"]["metadata"], metadata);
    assert!(receipt["relationship"]["position"].is_null());
    let active_source = capture("a");
    let active_target = capture("b");
    assert_ne!(active_source.dependencies(), before_source.dependencies());
    assert_ne!(active_target.dependencies(), before_target.dependencies());
    let revision = receipt["relationship"]["revision"].as_i64().unwrap();
    let delete = RelationshipMutation::Delete {
        id: id.into(),
        expected_revision: revision,
        expected_edge: relationship.clone(),
    };
    let delete_grant = RelationshipAuthority {
        host_namespace: HOST.into(),
        create_edges: BTreeSet::new(),
        kind_rules: BTreeMap::new(),
        delete_edges: [(
            id.into(),
            AuthorizedRelationshipDelete {
                expected_revision: revision,
                edge: relationship,
            },
        )]
        .into(),
    };
    committed(
        f.execute(&f.command("capture-delete", &delete), &delete_grant)
            .unwrap(),
    );
    let deleted_source = capture("a");
    let deleted_target = capture("b");
    assert_ne!(deleted_source.dependencies(), active_source.dependencies());
    assert_ne!(deleted_target.dependencies(), active_target.dependencies());
    assert_eq!(f.revision(id), (revision + 1, 1));
    assert_eq!(f.count("catalog_command_receipts"), 2);
}
