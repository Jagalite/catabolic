//! Native40 compound owner boundaries. These exercise the actual dispatcher.
use super::*;
use catabolic_store::catalog_tag_mutation::{TagAuthority, TagSubject};
use rusqlite::OptionalExtension;

#[path = "artwork_attach.rs"]
mod artwork_attach;

fn current_schema(store: &Store) -> String {
    store
        .db
        .query_row(
            "SELECT schema_fingerprint FROM catalog_metadata_schema_heads WHERE item_id=?",
            [ITEM],
            |r| r.get(0),
        )
        .unwrap()
}
fn replacement(
    store: &Store,
    id: &str,
    namespace: &str,
    revision: i64,
    fields: Value,
    tags: Vec<String>,
    excluded: Vec<String>,
) -> CommandEnvelope {
    envelope(
        store,
        id,
        json!({"operation":"metadata_contribution_replace","request":metadata::ContributionReplace {
            contract_version: 2, subject: ITEM.into(), namespace: namespace.into(), record_id: "record".into(),
            expected_schema_fingerprint: store.db.query_row("SELECT schema_fingerprint FROM catalog_metadata_schema_heads WHERE item_id=?",[ITEM],|r|r.get::<_,String>(0)).optional().unwrap(), expected_revision: revision,
            fields_json: fields.to_string(), evidence_json: json!({"test":id}).to_string(), tags, excluded_tags: excluded, origin_creation_intent:None,
        }}),
    )
}
fn execute(
    store: &Store,
    command: &CommandEnvelope,
    namespace: &str,
    allow_tags: bool,
) -> Result<CommandOutcome, commands::CommandStoreError> {
    let source = if namespace == "motion.local" {
        local(namespace)
    } else {
        provider(namespace)
    };
    let initialize = grant(MetadataAccess::InitializeV2 {
        system_fields: Default::default(),
    });
    let extension = grant(MetadataAccess::ExtendSchema);
    let tags = TagAuthority {
        host_namespace: HOST.into(),
        allowed_subjects: if allow_tags {
            [TagSubject::Item(ITEM.into())].into()
        } else {
            Default::default()
        },
        allow_global_merge: false,
    };
    commands::execute_with_authority(
        store,
        command,
        CatalogAuthority::new(base())
            .with_metadata(&source)
            .with_metadata_initialization(&initialize)
            .with_metadata_schema(&extension)
            .with_tags(&tags),
        policy(),
    )
}
fn fields(prefix: &str) -> Value {
    Value::Object(
        (0..100)
            .map(|i| {
                (
                    if i == 0 {
                        format!("{prefix}{}", "x".repeat(100 - prefix.len()))
                    } else {
                        format!("{prefix}{i}")
                    },
                    json!(i),
                )
            })
            .collect(),
    )
}
fn all_state(store: &Store) -> BTreeMap<&'static str, Vec<Vec<SqlValue>>> {
    let mut state = snapshot(store);
    for table in [
        "catalog_metadata_schemas",
        "catalog_metadata_schema_heads",
        "catalog_metadata_tag_history",
        "catalog_metadata_tag_suppressions",
        "tags",
        "tag_names",
        "item_tags",
        "catalog_item_tag_suppressions",
        "catalog_tag_suppression_revisions",
    ] {
        let mut stmt = store
            .db
            .prepare(&format!("SELECT * FROM {table} ORDER BY 1,2"))
            .unwrap();
        let n = stmt.column_count();
        let rows = stmt
            .query_map([], |r| {
                (0..n)
                    .map(|i| r.get(i))
                    .collect::<rusqlite::Result<Vec<SqlValue>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        state.insert(table, rows);
    }
    state
}
#[test]
fn native40_full_replacement_extends_union_preserves_history_tags_and_replay() {
    let f = Fixture::at_version(40);
    let initialize = f.initialize();
    let original = fingerprint(&f.store);
    let first = replacement(
        &f.store,
        "local-100",
        "motion.local",
        0,
        fields("a"),
        vec!["one".into()],
        vec!["hidden".into()],
    );
    assert!(matches!(
        execute(&f.store, &first, "motion.local", true).unwrap(),
        CommandOutcome::Committed { .. }
    ));
    let first_schema = current_schema(&f.store);
    assert_ne!(first_schema, original);
    let other = replacement(
        &f.store,
        "provider-100",
        "motion.provider",
        0,
        fields("b"),
        vec!["two".into()],
        vec![],
    );
    execute(&f.store, &other, "motion.provider", true).unwrap();
    assert_ne!(current_schema(&f.store), first_schema);
    let retained: (String, i64) = f.store.db.query_row("SELECT schema_fingerprint,revision FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.local'", [ITEM], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(retained, (first_schema, 1));
    let capture = catabolic_store::catalog_work_capture::capture(
        &f.store,
        &catabolic_store::catalog_projection::TrustedProjectionAuthority {
            catalog_id: f.store.database_id.clone(),
            incarnation: f.store.catalog_incarnation.clone().unwrap(),
            expected_generation: generation(&f.store),
        },
        "default",
        ITEM,
    )
    .unwrap();
    assert_eq!(capture.work_id(), ITEM);
    let third = replacement(
        &f.store,
        "local-replace-100",
        "motion.local",
        1,
        fields("c"),
        vec![],
        vec![],
    );
    execute(&f.store, &third, "motion.local", true).unwrap();
    let sources = metadata::read_contributions(&f.store, &read_authority(&f.store)).unwrap();
    let local = sources
        .sources
        .iter()
        .find(|s| s.namespace == "motion.local")
        .unwrap();
    assert_eq!(local.revision, 2);
    assert_eq!(local.fields.len(), 100);
    assert!(local.fields.keys().all(|k| k.starts_with('c')));
    assert!(local.tags.is_empty());
    assert!(sources.excluded_tags.is_empty());
    let stable = all_state(&f.store);
    assert!(matches!(
        execute(&f.store, &first, "motion.local", true).unwrap(),
        CommandOutcome::Replay { .. }
    ));
    assert!(matches!(
        run(&f.store, &initialize, &grant(MetadataAccess::Initialize)),
        CommandOutcome::Replay { .. }
    ));
    assert_eq!(all_state(&f.store), stable);
    assert_no_permits(&f.store);
}
#[test]
fn native40_source_cas_authority_and_external_bounds_roll_back_every_owner() {
    let f = Fixture::at_version(40);
    f.initialize();
    let first = replacement(
        &f.store,
        "first",
        "motion.local",
        0,
        json!({"new":1}),
        vec!["one".into()],
        vec![],
    );
    execute(&f.store, &first, "motion.local", true).unwrap();
    let stable = all_state(&f.store);
    for (id, revision, value, authority) in [
        ("stale", 0, json!({"another":2}), true),
        ("tag-denied", 1, json!({"another":2}), false),
        ("long-key", 1, json!({"x".repeat(101): 1}), true),
        (
            "too-many",
            1,
            Value::Object((0..101).map(|i| (format!("field{i}"), json!(i))).collect()),
            true,
        ),
        (
            "too-many-bytes",
            1,
            json!({"large":"x".repeat(65536)}),
            true,
        ),
    ] {
        let command = replacement(
            &f.store,
            id,
            "motion.local",
            revision,
            value,
            vec!["changed".into()],
            vec![],
        );
        assert!(
            execute(&f.store, &command, "motion.local", authority).is_err(),
            "{id}"
        );
        assert_eq!(all_state(&f.store), stable, "{id}");
    }
    assert_no_permits(&f.store);
}

#[test]
fn native39_to40_upgrade_preserves_source_bytes_receipts_and_capture_revisions() {
    let f = Fixture::at_version(39);
    f.initialize();
    let source = envelope(
        &f.store,
        "old-source",
        payload(patch(
            &f.store,
            "motion.local",
            0,
            json!({"title":{"op":"set","value":"Before"}}),
        )),
    );
    // Use the established v1 command owner before closing the old catalog.
    run(&f.store, &source, &local("motion.local"));
    let before = snapshot(&f.store);
    let old_fingerprint = fingerprint(&f.store);
    let Fixture { _root, path, store } = f;
    drop(store);
    let quiescence = || migration::NativeUpgradeQuiescence {
        admission_stopped: true,
        readers_and_workers_stopped: true,
        exclusive_operational_ownership: true,
    };
    let original_bytes = std::fs::read(&path).unwrap();
    let failed = migration::upgrade_native(
        &path,
        &migration::plan_native(&path).unwrap(),
        quiescence(),
        None,
        &|stage, _, version| {
            if stage == "upgrade:after_migration" && version == 40 {
                Err(catabolic_store::Error(
                    "injected after native40 table replacement".into(),
                ))
            } else {
                Ok(())
            }
        },
    );
    assert!(failed.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original_bytes);
    migration::upgrade_native(
        &path,
        &migration::plan_native(&path).unwrap(),
        migration::NativeUpgradeQuiescence {
            admission_stopped: true,
            readers_and_workers_stopped: true,
            exclusive_operational_ownership: true,
        },
        None,
        &|_, _, _| Ok(()),
    )
    .unwrap();
    let store = Store::open(&path, true, false).unwrap();
    assert_eq!(store.schema_version, 40);
    for table in [
        "catalog_metadata_subjects",
        "catalog_metadata_sources",
        "catalog_metadata_history",
        "catalog_metadata_projection",
        "items",
        "catalog_state",
        "catalog_command_receipts",
    ] {
        assert_eq!(snapshot(&store)[table], before[table]);
    }
    assert!(
        !store
            .db
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    let after_revisions = &snapshot(&store)["catalog_entity_revisions"];
    for row in &before["catalog_entity_revisions"] {
        assert!(after_revisions.contains(row));
    }
    assert_eq!(current_schema(&store), old_fingerprint);
    let capture = catabolic_store::catalog_work_capture::capture(
        &store,
        &catabolic_store::catalog_projection::TrustedProjectionAuthority {
            catalog_id: store.database_id.clone(),
            incarnation: store.catalog_incarnation.clone().unwrap(),
            expected_generation: generation(&store),
        },
        "default",
        ITEM,
    )
    .unwrap();
    assert_eq!(capture.work_id(), ITEM);
    assert!(matches!(
        run(&store, &source, &local("motion.local")),
        CommandOutcome::Replay { .. }
    ));
    let next = replacement(
        &store,
        "new-source",
        "motion.local",
        1,
        json!({"new_field":1}),
        vec!["tag".into()],
        vec![],
    );
    execute(&store, &next, "motion.local", true).unwrap();
    let old: String = store.db.query_row("SELECT schema_fingerprint FROM catalog_metadata_history WHERE item_id=? AND namespace='motion.local' AND revision=1", [ITEM], |r| r.get(0)).unwrap();
    assert_eq!(old, old_fingerprint);
    drop(_root);
}

#[test]
fn native40_empty_replacement_and_reserved_owner_fields_fail_closed() {
    let f = Fixture::at_version(40);
    f.initialize();
    let first = replacement(
        &f.store,
        "fill",
        "motion.local",
        0,
        json!({"field":1}),
        vec!["tag".into()],
        vec![],
    );
    execute(&f.store, &first, "motion.local", true).unwrap();
    let stable = all_state(&f.store);
    for name in [
        "motion_catalog",
        "motion_probe",
        "motion_discovery",
        "motion_future_owner",
    ] {
        let forbidden = replacement(
            &f.store,
            name,
            "motion.local",
            1,
            Value::Object(
                [(name.to_owned(), json!({"spoof":true}))]
                    .into_iter()
                    .collect(),
            ),
            vec![],
            vec![],
        );
        assert!(execute(&f.store, &forbidden, "motion.local", true).is_err());
        assert_eq!(all_state(&f.store), stable);
    }
    let empty = replacement(
        &f.store,
        "empty",
        "motion.local",
        1,
        json!({}),
        vec![],
        vec![],
    );
    execute(&f.store, &empty, "motion.local", true).unwrap();
    let snapshot = metadata::read_contributions(&f.store, &read_authority(&f.store)).unwrap();
    let local = snapshot
        .sources
        .iter()
        .find(|s| s.namespace == "motion.local")
        .unwrap();
    assert_eq!(local.revision, 2);
    assert!(local.fields.is_empty());
    assert!(local.tags.is_empty());
}

#[test]
fn native40_first_empty_subject_accepts_full100_field_replacement() {
    let f = Fixture::at_version(40);
    let first = replacement(
        &f.store,
        "first-empty-baseline",
        "motion.local",
        0,
        fields("z"),
        vec!["tag".into()],
        vec![],
    );
    execute(&f.store, &first, "motion.local", true).unwrap();
    let snap = metadata::read_contributions(&f.store, &read_authority(&f.store)).unwrap();
    assert_eq!(
        snap.sources
            .iter()
            .find(|s| s.namespace == "motion.local")
            .unwrap()
            .fields
            .len(),
        100
    );
    let version: i64 = f
        .store
        .db
        .query_row(
            "SELECT contract_version FROM catalog_metadata_subjects WHERE item_id=?",
            [ITEM],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(version, 2);
    assert_no_permits(&f.store);
}

#[test]
fn native40_explicit_empty_initialization_and_system_grants_are_independent() {
    let f = Fixture::at_version(40);
    let request = metadata::MetadataInitializeV2 {
        contract_version: 2,
        subject: ITEM.into(),
        policies: vec![],
        system_fields: Default::default(),
    };
    let command = envelope(
        &f.store,
        "initialize-empty-v2",
        json!({"operation":"metadata_initialize_v2","request":request}),
    );
    let old = grant(MetadataAccess::Initialize);
    assert!(
        commands::execute_with_authority(
            &f.store,
            &command,
            CatalogAuthority::new(base()).with_metadata_initialization(&old),
            policy()
        )
        .is_err()
    );
    let explicit = grant(MetadataAccess::InitializeV2 {
        system_fields: Default::default(),
    });
    commands::execute_with_authority(
        &f.store,
        &command,
        CatalogAuthority::new(base()).with_metadata_initialization(&explicit),
        policy(),
    )
    .unwrap();
    assert_eq!(raw_metadata(&f.store), "{}");
    let fingerprint = current_schema(&f.store);
    let first = replacement(
        &f.store,
        "first-field",
        "motion.local",
        0,
        json!({"new":1}),
        vec![],
        vec![],
    );
    execute(&f.store, &first, "motion.local", true).unwrap();
    assert_ne!(current_schema(&f.store), fingerprint);
    assert_no_permits(&f.store);
}

#[test]
fn native40_initializes100_field100_byte_baseline_without_rewriting_raw_evidence() {
    let f = Fixture::at_version(40);
    let baseline = fields("n");
    let seed = envelope(
        &f.store,
        "wide-baseline",
        json!({"operation":"item_put","item":{"id":ITEM,"kind":"video","metadata":baseline}}),
    );
    commands::execute(&f.store, &seed, base(), policy()).unwrap();
    let original = raw_metadata(&f.store);
    let request = metadata::MetadataInitializeV2 {
        contract_version: 2,
        subject: ITEM.into(),
        policies: baseline
            .as_object()
            .unwrap()
            .keys()
            .map(|field| PolicyField {
                field: field.clone(),
                value_type: PolicyValueType::Json,
                nullable: false,
                normalization: PolicyNormalization::Exact,
            })
            .collect(),
        system_fields: Default::default(),
    };
    let command = envelope(
        &f.store,
        "initialize-wide",
        json!({"operation":"metadata_initialize_v2","request":request}),
    );
    let explicit = grant(MetadataAccess::InitializeV2 {
        system_fields: Default::default(),
    });
    commands::execute_with_authority(
        &f.store,
        &command,
        CatalogAuthority::new(base()).with_metadata_initialization(&explicit),
        policy(),
    )
    .unwrap();
    let retained: String = f
        .store
        .db
        .query_row(
            "SELECT legacy_raw_json FROM catalog_metadata_subjects WHERE item_id=?",
            [ITEM],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(retained, original);
    let snapshot = metadata::read_contributions(&f.store, &read_authority(&f.store)).unwrap();
    assert_eq!(snapshot.sources[0].fields.len(), 100);
}

#[test]
fn native40_foreign_suppression_owner_rolls_back_schema_source_and_tag_writes() {
    use catabolic_store::catalog_tag_mutation::{SuppressionChange, TagMutation};
    let f = Fixture::at_version(40);
    f.initialize();
    let first = replacement(
        &f.store,
        "before-foreign",
        "motion.local",
        0,
        json!({"old":1}),
        vec!["one".into()],
        vec![],
    );
    execute(&f.store, &first, "motion.local", true).unwrap();
    let tag: String = f
        .store
        .db
        .query_row("SELECT tag_id FROM tag_names WHERE name='one'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let external = envelope(
        &f.store,
        "foreign-suppression",
        json!({"operation":"tags","mutation":TagMutation::SetSuppression {contract_version:1,changes:vec![SuppressionChange {subject:TagSubject::Item(ITEM.into()),canonical_tag_id:tag,expected_revision:None,suppressed:true,reason:"independent owner".into()}]}}),
    );
    let authority = TagAuthority {
        host_namespace: HOST.into(),
        allowed_subjects: [TagSubject::Item(ITEM.into())].into(),
        allow_global_merge: false,
    };
    commands::execute_with_authority(
        &f.store,
        &external,
        CatalogAuthority::new(base()).with_tags(&authority),
        policy(),
    )
    .unwrap();
    let before = all_state(&f.store);
    let replacement = replacement(
        &f.store,
        "cannot-claim-foreign",
        "motion.local",
        1,
        json!({"new_policy":2}),
        vec!["new-tag".into()],
        vec!["one".into()],
    );
    assert!(execute(&f.store, &replacement, "motion.local", true).is_err());
    assert_eq!(all_state(&f.store), before);
}

#[test]
fn native40_external_full_replacement_retains_independently_owned_system_values() {
    let f = Fixture::at_version(40);
    let system: std::collections::BTreeSet<String> = ["motion_catalog".into()].into();
    let request = metadata::MetadataInitializeV2 {
        contract_version: 2,
        subject: ITEM.into(),
        policies: ["motion_catalog", "field"]
            .into_iter()
            .map(|field| PolicyField {
                field: field.into(),
                value_type: PolicyValueType::Json,
                nullable: false,
                normalization: PolicyNormalization::Exact,
            })
            .collect(),
        system_fields: system.clone(),
    };
    let command = envelope(
        &f.store,
        "initialize-protected",
        json!({"operation":"metadata_initialize_v2","request":request}),
    );
    let initialization = grant(MetadataAccess::InitializeV2 {
        system_fields: system.clone(),
    });
    commands::execute_with_authority(
        &f.store,
        &command,
        CatalogAuthority::new(base()).with_metadata_initialization(&initialization),
        policy(),
    )
    .unwrap();
    let marker = json!({"contract_version":1,"item_id":ITEM,"kind":"movie"});
    let mutation = MetadataMutation {
        contract_version: 1,
        change: MetadataChange::Patch {
            subject: ITEM.into(),
            namespace: "motion.local".into(),
            record_id: "record".into(),
            schema_fingerprint: current_schema(&f.store),
            expected_revision: 0,
            edits_json:
                json!({"motion_catalog":{"op":"set","value":marker},"field":{"op":"set","value":1}})
                    .to_string(),
            evidence_json: "{}".into(),
        },
    };
    let denied = envelope(
        &f.store,
        "external-system-denied",
        payload(mutation.clone()),
    );
    assert!(
        commands::execute_with_authority(
            &f.store,
            &denied,
            CatalogAuthority::new(base()).with_metadata(&local("motion.local")),
            policy()
        )
        .is_err()
    );
    let trusted = grant(MetadataAccess::SourceV2 {
        namespace: "motion.local".into(),
        record_id: "record".into(),
        kind: SourceKind::Local,
        system_fields: system,
    });
    let allowed = envelope(&f.store, "system-owner", payload(mutation));
    run(&f.store, &allowed, &trusted);
    let replace = replacement(
        &f.store,
        "remove-only-public",
        "motion.local",
        1,
        json!({}),
        vec![],
        vec![],
    );
    execute(&f.store, &replace, "motion.local", true).unwrap();
    let snap = metadata::read_contributions(&f.store, &read_authority(&f.store)).unwrap();
    let local = snap
        .sources
        .iter()
        .find(|s| s.namespace == "motion.local")
        .unwrap();
    assert_eq!(local.fields.len(), 1);
    assert_eq!(local.fields["motion_catalog"], marker);
    assert_eq!(local.revision, 2);
}

fn created_origin_fixture() -> (Fixture, CommandEnvelope) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("origin.db");
    migration::initialize_native_at(&path, 40).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    let creation = envelope(
        &store,
        "motion-original-create",
        json!({"operation":"item_create_batch","items":[{"id":ITEM,"kind":"video","metadata":{"title":"Original","motion_catalog":{"contract_version":1,"item_id":ITEM,"kind":"video"}}}]}),
    );
    commands::execute(&store, &creation, base(), policy()).unwrap();
    (
        Fixture {
            _root: root,
            path,
            store,
        },
        creation,
    )
}
fn execute_origin(
    store: &Store,
    command: &CommandEnvelope,
    origin: &metadata::VerifiedCreatedOrigin,
) -> Result<CommandOutcome, commands::CommandStoreError> {
    let source = provider("motion.provider");
    let initialization = grant(MetadataAccess::InitializeV2 {
        system_fields: ["motion_catalog".into()].into(),
    });
    let extension = grant(MetadataAccess::ExtendSchema);
    let tags = TagAuthority {
        host_namespace: HOST.into(),
        allowed_subjects: [TagSubject::Item(ITEM.into())].into(),
        allow_global_merge: false,
    };
    commands::execute_with_authority(
        store,
        command,
        CatalogAuthority::new(base())
            .with_metadata(&source)
            .with_metadata_initialization(&initialization)
            .with_metadata_schema(&extension)
            .with_tags(&tags)
            .with_metadata_origin(origin),
        policy(),
    )
}
#[test]
fn native40_receipt_origin_and_provider_replace_are_atomic_replayable_and_preserve_history() {
    let (f, creation) = created_origin_fixture();
    let verified = metadata::verify_created_origin(
        &f.store,
        &creation,
        ITEM,
        TrustedCommandAuthority {
            allow_new_commands: false,
            ..base()
        },
    )
    .unwrap();
    let request = metadata::ContributionReplace {
        contract_version: 2,
        subject: ITEM.into(),
        namespace: "motion.provider".into(),
        record_id: "record".into(),
        expected_schema_fingerprint: None,
        expected_revision: 0,
        fields_json: json!({"title":"Provider"}).to_string(),
        evidence_json: "{}".into(),
        tags: vec![],
        excluded_tags: vec![],
        origin_creation_intent: Some(creation.canonical_payload().into()),
    };
    let command = envelope(
        &f.store,
        "origin-and-provider",
        json!({"operation":"metadata_contribution_replace","request":request}),
    );
    execute_origin(&f.store, &command, &verified).unwrap();
    let snapshot = metadata::read_contributions(&f.store, &read_authority(&f.store)).unwrap();
    assert_eq!(snapshot.metadata["title"], "Provider");
    let scan = snapshot
        .sources
        .iter()
        .find(|s| s.namespace == "motion.scan")
        .unwrap();
    assert_eq!(scan.fields["title"], "Original");
    assert_eq!(scan.revision, 1);
    let legacy = snapshot
        .sources
        .iter()
        .find(|s| s.namespace == metadata::LEGACY_NAMESPACE)
        .unwrap();
    assert_eq!(legacy.revision, 2);
    assert!(!legacy.fields.contains_key("title"));
    assert!(legacy.fields.contains_key("motion_catalog"));
    let original:String=f.store.db.query_row("SELECT fields_json FROM catalog_metadata_history WHERE item_id=? AND namespace=? AND revision=1",params![ITEM,metadata::LEGACY_NAMESPACE],|r|r.get(0)).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&original).unwrap()["title"],
        "Original"
    );
    let stable = all_state(&f.store);
    assert!(matches!(
        execute_origin(&f.store, &command, &verified).unwrap(),
        CommandOutcome::Replay { .. }
    ));
    assert_eq!(all_state(&f.store), stable);
    let Fixture { _root, path, store } = f;
    drop(store);
    let store = Store::open(&path, true, false).unwrap();
    assert_eq!(
        metadata::read_contributions(&store, &read_authority(&store))
            .unwrap()
            .metadata["title"],
        "Provider"
    );
    drop(_root);
}
#[test]
fn native40_verified_origin_cannot_cross_host_namespaces() {
    const OTHER_HOST: &str = "other-metadata-host";
    let (f, creation) = created_origin_fixture();
    let verified = metadata::verify_created_origin(
        &f.store,
        &creation,
        ITEM,
        TrustedCommandAuthority {
            allow_new_commands: false,
            ..base()
        },
    )
    .unwrap();
    let request = metadata::ContributionReplace {
        contract_version: 2,
        subject: ITEM.into(),
        namespace: "motion.provider".into(),
        record_id: "record".into(),
        expected_schema_fingerprint: None,
        expected_revision: 0,
        fields_json: json!({"title":"Provider"}).to_string(),
        evidence_json: "{}".into(),
        tags: vec![],
        excluded_tags: vec![],
        origin_creation_intent: Some(creation.canonical_payload().into()),
    };
    let command = CommandEnvelope::new(
        1,
        CommandKey::new(&f.store.database_id, OTHER_HOST, "cross-host-origin").unwrap(),
        f.store.catalog_incarnation.as_deref().unwrap(),
        generation(&f.store),
        &json!({"operation":"metadata_contribution_replace","request":request}).to_string(),
    )
    .unwrap();
    let source = MetadataAuthority {
        host_namespace: OTHER_HOST.into(),
        ..provider("motion.provider")
    };
    let initialization = MetadataAuthority {
        host_namespace: OTHER_HOST.into(),
        ..grant(MetadataAccess::InitializeV2 {
            system_fields: ["motion_catalog".into()].into(),
        })
    };
    let extension = MetadataAuthority {
        host_namespace: OTHER_HOST.into(),
        ..grant(MetadataAccess::ExtendSchema)
    };
    let tags = TagAuthority {
        host_namespace: OTHER_HOST.into(),
        allowed_subjects: [TagSubject::Item(ITEM.into())].into(),
        allow_global_merge: false,
    };
    let before = all_state(&f.store);
    let error = commands::execute_with_authority(
        &f.store,
        &command,
        CatalogAuthority::new(TrustedCommandAuthority {
            host_namespace: OTHER_HOST,
            ..base()
        })
        .with_metadata(&source)
        .with_metadata_initialization(&initialization)
        .with_metadata_schema(&extension)
        .with_tags(&tags)
        .with_metadata_origin(&verified),
        policy(),
    )
    .unwrap_err();
    assert_eq!(error.code, CommandStoreErrorCode::AuthorityDenied);
    assert_eq!(all_state(&f.store), before);
    assert_no_permits(&f.store);
}

#[test]
fn native40_origin_requires_exact_full_receipt_and_current_baseline() {
    let (f, creation) = created_origin_fixture();
    let mut wrong = creation.payload().clone();
    wrong["items"][0]["metadata"]["title"] = json!("Forged");
    let changed = CommandEnvelope::new(
        1,
        creation.key().clone(),
        creation.admission_incarnation(),
        creation.expected_generation(),
        &wrong.to_string(),
    )
    .unwrap();
    assert!(
        metadata::verify_created_origin(
            &f.store,
            &changed,
            ITEM,
            TrustedCommandAuthority {
                allow_new_commands: false,
                ..base()
            }
        )
        .is_err()
    );
    assert!(
        metadata::verify_created_origin(
            &f.store,
            &creation,
            "other",
            TrustedCommandAuthority {
                allow_new_commands: false,
                ..base()
            }
        )
        .is_err()
    );
    let origin = metadata::verify_created_origin(
        &f.store,
        &creation,
        ITEM,
        TrustedCommandAuthority {
            allow_new_commands: false,
            ..base()
        },
    )
    .unwrap();
    let mutation = envelope(
        &f.store,
        "diverged-origin",
        json!({"operation":"item_put","item":{"id":ITEM,"kind":"video","metadata":{"title":"Unknown native replacement","motion_catalog":{"contract_version":1,"item_id":ITEM,"kind":"video"}}}}),
    );
    commands::execute(&f.store, &mutation, base(), policy()).unwrap();
    assert!(
        metadata::verify_created_origin(
            &f.store,
            &creation,
            ITEM,
            TrustedCommandAuthority {
                allow_new_commands: false,
                ..base()
            },
        )
        .is_err()
    );
    let request = metadata::ContributionReplace {
        contract_version: 2,
        subject: ITEM.into(),
        namespace: "motion.provider".into(),
        record_id: "record".into(),
        expected_schema_fingerprint: None,
        expected_revision: 0,
        fields_json: json!({"title":"Provider"}).to_string(),
        evidence_json: "{}".into(),
        tags: vec![],
        excluded_tags: vec![],
        origin_creation_intent: Some(creation.canonical_payload().into()),
    };
    let command = envelope(
        &f.store,
        "cannot-relabel",
        json!({"operation":"metadata_contribution_replace","request":request}),
    );
    let before = all_state(&f.store);
    assert!(execute_origin(&f.store, &command, &origin).is_err());
    assert_eq!(all_state(&f.store), before);
    let restored = envelope(
        &f.store,
        "origin-title-restored-after-native-write",
        json!({"operation":"item_put","item":{"id":ITEM,"kind":"video","metadata":creation.payload()["items"][0]["metadata"]}}),
    );
    commands::execute(&f.store, &restored, base(), policy()).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&raw_metadata(&f.store)).unwrap(),
        creation.payload()["items"][0]["metadata"]
    );
    let before = all_state(&f.store);
    assert!(
        metadata::verify_created_origin(
            &f.store,
            &creation,
            ITEM,
            TrustedCommandAuthority {
                allow_new_commands: false,
                ..base()
            },
        )
        .is_err()
    );
    assert_eq!(all_state(&f.store), before);
    assert_no_permits(&f.store);
}

fn system_command(store: &Store, operation: &str, revision: i64, fields: Value) -> CommandEnvelope {
    envelope(
        store,
        operation,
        json!({"operation":"metadata_system_replace","request":metadata::MetadataSystemReplace {
            contract_version:1,subject:ITEM.into(),namespace:"motion.artwork".into(),record_id:"source_projection".into(),
            expected_schema_fingerprint:store.db.query_row("SELECT schema_fingerprint FROM catalog_metadata_schema_heads WHERE item_id=?",[ITEM],|r|r.get::<_,String>(0)).optional().unwrap(),
            expected_revision:revision,fields_json:fields.to_string(),evidence_json:"{}".into(),origin_creation_intent:None,
        }}),
    )
}
fn system_execute(
    store: &Store,
    command: &CommandEnvelope,
    system: bool,
) -> Result<CommandOutcome, commands::CommandStoreError> {
    system_execute_with_classification(store, command, system, Default::default())
}
fn system_execute_with_classification(
    store: &Store,
    command: &CommandEnvelope,
    system: bool,
    system_fields: std::collections::BTreeSet<String>,
) -> Result<CommandOutcome, commands::CommandStoreError> {
    let source = grant(if system {
        MetadataAccess::SourceV2 {
            namespace: "motion.artwork".into(),
            record_id: "source_projection".into(),
            kind: SourceKind::Scan,
            system_fields: ["motion_artwork".into()].into(),
        }
    } else {
        MetadataAccess::Source {
            namespace: "motion.artwork".into(),
            record_id: "source_projection".into(),
            kind: SourceKind::Scan,
        }
    });
    let initialize = grant(MetadataAccess::InitializeV2 { system_fields });
    let extension = grant(MetadataAccess::ExtendSchema);
    commands::execute_with_authority(
        store,
        command,
        CatalogAuthority::new(base())
            .with_metadata(&source)
            .with_metadata_initialization(&initialize)
            .with_metadata_schema(&extension),
        policy(),
    )
}
#[test]
fn native40_system_source_extension_requires_exact_grant_and_replays() {
    let f = Fixture::at_version(40);
    let command = system_command(
        &f.store,
        "system-artwork-first",
        0,
        json!({"motion_artwork":{"asset":"first"}}),
    );
    let unchanged = all_state(&f.store);
    assert_eq!(
        system_execute(&f.store, &command, false).unwrap_err().code,
        CommandStoreErrorCode::AuthorityDenied
    );
    assert_eq!(all_state(&f.store), unchanged);
    assert!(matches!(
        system_execute(&f.store, &command, true).unwrap(),
        CommandOutcome::Committed { .. }
    ));
    let snapshot = metadata::read_contributions(&f.store, &read_authority(&f.store)).unwrap();
    assert_eq!(snapshot.metadata["motion_artwork"]["asset"], "first");
    let stable = all_state(&f.store);
    assert!(matches!(
        system_execute(&f.store, &command, true).unwrap(),
        CommandOutcome::Replay { .. }
    ));
    assert_eq!(all_state(&f.store), stable);
    let denied = system_command(
        &f.store,
        "system-artwork-ungranted",
        1,
        json!({"motion_catalog":{"spoof":true}}),
    );
    assert!(system_execute(&f.store, &denied, true).is_err());
    assert_eq!(all_state(&f.store), stable);
    let stale = system_command(
        &f.store,
        "system-artwork-stale",
        0,
        json!({"motion_artwork":{}}),
    );
    assert!(system_execute(&f.store, &stale, true).is_err());
    assert_eq!(all_state(&f.store), stable);
    let changed = system_command(
        &f.store,
        "system-artwork-next",
        1,
        json!({"motion_artwork":{"asset":"next"}}),
    );
    system_execute(&f.store, &changed, true).unwrap();
    let snapshot = metadata::read_contributions(&f.store, &read_authority(&f.store)).unwrap();
    assert_eq!(snapshot.metadata["motion_artwork"]["asset"], "next");
    assert_eq!(
        snapshot
            .sources
            .iter()
            .find(|source| source.namespace == "motion.artwork")
            .unwrap()
            .revision,
        2
    );
}
#[test]
fn native40_system_source_cannot_reclassify_external_owner_fields() {
    let f = Fixture::at_version(40);
    // A v1 external field with a protected-looking name is not system authority.
    let seed = envelope(
        &f.store,
        "seed-foreign-artwork",
        json!({"operation":"item_put","item":{"id":ITEM,"kind":"video","metadata":{"motion_artwork":{"foreign":true}}}}),
    );
    commands::execute(&f.store, &seed, base(), policy()).unwrap();
    let command = system_command(
        &f.store,
        "system-artwork-collision",
        0,
        json!({"motion_artwork":{"asset":"mine"}}),
    );
    let before = all_state(&f.store);
    assert!(system_execute(&f.store, &command, true).is_err());
    assert_eq!(all_state(&f.store), before);
}

fn system_v1_structure_fixture() -> Fixture {
    let f = Fixture::at_version(40);
    let marker = json!({"contract_version":1,"item_id":ITEM,"kind":"video"});
    let seed = envelope(
        &f.store,
        "v1-structure-seed",
        json!({"operation":"item_put","item":{"id":ITEM,"kind":"video","metadata":{"title":"Original","motion_catalog":marker}}}),
    );
    commands::execute(&f.store, &seed, base(), policy()).unwrap();
    let initialize = envelope(
        &f.store,
        "v1-structure-initialize",
        payload(MetadataMutation {
            contract_version: 1,
            change: MetadataChange::Initialize {
                subject: ITEM.into(),
                policies: ["title", "motion_catalog", "provider_value"]
                    .into_iter()
                    .map(|field| PolicyField {
                        field: field.into(),
                        value_type: PolicyValueType::Json,
                        nullable: false,
                        normalization: PolicyNormalization::Exact,
                    })
                    .collect(),
            },
        }),
    );
    run(&f.store, &initialize, &grant(MetadataAccess::Initialize));
    let local = envelope(
        &f.store,
        "v1-structure-local",
        payload(patch(
            &f.store,
            "motion.local",
            0,
            json!({"title":{"op":"set","value":"Local"},"motion_catalog":{"op":"set","value":marker}}),
        )),
    );
    run(&f.store, &local, &self::local("motion.local"));
    let provider = envelope(
        &f.store,
        "v1-structure-provider",
        payload(patch(
            &f.store,
            "motion.provider",
            0,
            json!({"provider_value":{"op":"set","value":{"retained":true}}}),
        )),
    );
    run(&f.store, &provider, &self::provider("motion.provider"));
    f
}

fn system_retained_source_rows(store: &Store, table: &str) -> Vec<Vec<SqlValue>> {
    let mut query = store
        .db
        .prepare(&format!(
            "SELECT * FROM {table} WHERE item_id=? AND namespace<>'motion.artwork' ORDER BY namespace,record_id,revision"
        ))
        .unwrap();
    let columns = query.column_count();
    query
        .query_map([ITEM], |row| {
            (0..columns)
                .map(|column| row.get(column))
                .collect::<rusqlite::Result<Vec<SqlValue>>>()
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn system_current_classification(store: &Store) -> std::collections::BTreeSet<String> {
    let raw: String = store.db.query_row(
        "SELECT s.system_fields_json FROM catalog_metadata_schema_heads h JOIN catalog_metadata_schemas s ON s.item_id=h.item_id AND s.schema_fingerprint=h.schema_fingerprint WHERE h.item_id=?",
        [ITEM], |row| row.get(0),
    ).unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn system_followup_title(store: &Store, operation: &str) -> CommandEnvelope {
    let mut mutation = patch(
        store,
        "motion.local",
        1,
        json!({"title":{"op":"set","value":"After artwork"},"motion_catalog":{"op":"set","value":{"contract_version":1,"item_id":ITEM,"kind":"video"}}}),
    );
    let MetadataChange::Patch {
        schema_fingerprint, ..
    } = &mut mutation.change
    else {
        unreachable!()
    };
    *schema_fingerprint = current_schema(store);
    envelope(store, operation, payload(mutation))
}

fn system_title_grant() -> MetadataAuthority {
    grant(MetadataAccess::SourceV2 {
        namespace: "motion.local".into(),
        record_id: "record".into(),
        kind: SourceKind::Local,
        system_fields: ["motion_catalog".into()].into(),
    })
}

#[test]
fn native40_system_v1_explicit_classification_preserves_sources_and_future_title_writes() {
    let f = system_v1_structure_fixture();
    let original_fingerprint = fingerprint(&f.store);
    let source_rows = system_retained_source_rows(&f.store, "catalog_metadata_sources");
    let history_rows = system_retained_source_rows(&f.store, "catalog_metadata_history");
    let old_policies: String = f.store.db.query_row(
        "SELECT policies_json FROM catalog_metadata_schemas WHERE item_id=? AND schema_fingerprint=?",
        params![ITEM, original_fingerprint], |row| row.get(0),
    ).unwrap();
    let command = system_command(
        &f.store,
        "v1-structure-artwork",
        0,
        json!({"motion_artwork":{"asset":"first"}}),
    );
    assert!(matches!(
        system_execute_with_classification(
            &f.store,
            &command,
            true,
            ["motion_catalog".into()].into(),
        )
        .unwrap(),
        CommandOutcome::Committed { .. }
    ));
    assert_eq!(
        system_current_classification(&f.store),
        ["motion_artwork".into(), "motion_catalog".into()].into()
    );
    assert_eq!(fingerprint(&f.store), original_fingerprint);
    assert_eq!(
        system_retained_source_rows(&f.store, "catalog_metadata_sources"),
        source_rows
    );
    assert_eq!(
        system_retained_source_rows(&f.store, "catalog_metadata_history"),
        history_rows
    );
    let retained_policies: String = f.store.db.query_row(
        "SELECT policies_json FROM catalog_metadata_schemas WHERE item_id=? AND schema_fingerprint=? AND contract_version=1",
        params![ITEM, original_fingerprint], |row| row.get(0),
    ).unwrap();
    assert_eq!(retained_policies, old_policies);
    let stable = all_state(&f.store);
    assert!(matches!(
        system_execute_with_classification(
            &f.store,
            &command,
            true,
            ["motion_catalog".into()].into(),
        )
        .unwrap(),
        CommandOutcome::Replay { .. }
    ));
    assert_eq!(all_state(&f.store), stable);
    let title = system_followup_title(&f.store, "v1-structure-title-after-artwork");
    run(&f.store, &title, &system_title_grant());
    let snapshot = metadata::read_contributions(&f.store, &read_authority(&f.store)).unwrap();
    assert_eq!(snapshot.metadata["title"], "After artwork");
    assert_eq!(
        snapshot.metadata["provider_value"],
        json!({"retained":true})
    );
    assert_no_permits(&f.store);
}

#[test]
fn native40_system_missing_classification_never_infers_ownership_or_promotes_v2_external() {
    let f = system_v1_structure_fixture();
    let command = system_command(
        &f.store,
        "v1-no-classification",
        0,
        json!({"motion_artwork":{"asset":"first"}}),
    );
    system_execute(&f.store, &command, true).unwrap();
    assert_eq!(
        system_current_classification(&f.store),
        ["motion_artwork".into()].into()
    );
    let before = all_state(&f.store);
    let title = system_followup_title(&f.store, "unclassified-title-denied");
    assert!(
        commands::execute_with_authority(
            &f.store,
            &title,
            CatalogAuthority::new(base()).with_metadata(&system_title_grant()),
            policy(),
        )
        .is_err()
    );
    assert_eq!(all_state(&f.store), before);
    let promotion = system_command(
        &f.store,
        "v2-classification-promotion-denied",
        1,
        json!({"motion_artwork":{"asset":"next"}}),
    );
    assert!(
        system_execute_with_classification(
            &f.store,
            &promotion,
            true,
            ["motion_catalog".into()].into(),
        )
        .is_err()
    );
    assert_eq!(all_state(&f.store), before);
    assert_no_permits(&f.store);
}

#[test]
fn native40_system_classification_cannot_invent_an_absent_policy() {
    let f = system_v1_structure_fixture();
    let command = system_command(
        &f.store,
        "absent-classification",
        0,
        json!({"motion_artwork":{"asset":"first"}}),
    );
    let before = all_state(&f.store);
    assert!(
        system_execute_with_classification(
            &f.store,
            &command,
            true,
            ["absent_system_field".into()].into(),
        )
        .is_err()
    );
    assert_eq!(all_state(&f.store), before);
    assert_no_permits(&f.store);
}

#[test]
fn native40_system_raw_duplicate_keys_fail_before_normalization() {
    let f = Fixture::at_version(40);
    for (index, raw) in [
        r#"{"motion_artwork":{},"motion_artwork":{"asset":"last"}}"#,
        r#"{"motion_artwork":{"asset":"first","asset":"last"}}"#,
    ]
    .into_iter()
    .enumerate()
    {
        let command = system_command(&f.store, &format!("duplicate-system-{index}"), 0, json!({}));
        let mut payload = command.payload().clone();
        payload["request"]["fields_json"] = json!(raw);
        let command = envelope(&f.store, &format!("duplicate-system-{index}"), payload);
        let before = all_state(&f.store);
        assert!(system_execute(&f.store, &command, true).is_err());
        assert_eq!(all_state(&f.store), before);
        assert_no_permits(&f.store);
    }
}
