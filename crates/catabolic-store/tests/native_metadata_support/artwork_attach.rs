//! Actual native40 dispatcher/owner composition. No asset bytes or byte grants
//! are fabricated: fixture descriptors exercise only reference-state decisions.
use super::*;
use catabolic_core::artwork::native::{ASSET_OWNER, AssetReference, ReferenceState, SEMANTICS};

fn asset(id: &str) -> AssetReference {
    AssetReference {
        asset_id: id.into(),
        mime: "image/png".into(),
        width: 1,
        height: 1,
        size_bytes: 3,
        content_sha256: "a".repeat(64),
        owner: ASSET_OWNER.into(),
    }
}
fn source_access() -> MetadataAccess {
    MetadataAccess::SourceV2 {
        namespace: "motion.artwork".into(),
        record_id: "source_projection".into(),
        kind: SourceKind::Scan,
        system_fields: ["motion_artwork".into()].into(),
    }
}
fn command(store: &Store, id: &str, asset: &AssetReference) -> CommandEnvelope {
    envelope(
        store,
        id,
        json!({"operation":"metadata_artwork_attach","request":metadata::MetadataArtworkAttach {
            contract_version:1,subject:ITEM.into(),asset:asset.clone(),origin_creation_intent:None,
        }}),
    )
}
fn execute_with_grants(
    store: &Store,
    command: &CommandEnvelope,
    descriptor: &metadata::ArtworkAttachAuthority,
    source: MetadataAccess,
) -> Result<CommandOutcome, commands::CommandStoreError> {
    let source = grant(source);
    let initialize = grant(MetadataAccess::InitializeV2 {
        system_fields: Default::default(),
    });
    let extension = grant(MetadataAccess::ExtendSchema);
    commands::execute_with_authority(
        store,
        command,
        CatalogAuthority::new(base())
            .with_artwork_attach(descriptor)
            .with_metadata(&source)
            .with_metadata_initialization(&initialize)
            .with_metadata_schema(&extension),
        policy(),
    )
}
fn descriptor_grant(asset: &AssetReference) -> metadata::ArtworkAttachAuthority {
    metadata::ArtworkAttachAuthority {
        host_namespace: HOST.into(),
        subject: ITEM.into(),
        asset: asset.clone(),
    }
}
fn execute(
    store: &Store,
    command: &CommandEnvelope,
    asset: &AssetReference,
) -> Result<CommandOutcome, commands::CommandStoreError> {
    execute_with_grants(store, command, &descriptor_grant(asset), source_access())
}
fn current(store: &Store) -> (ReferenceState, i64) {
    let snapshot = metadata::read_contributions(store, &read_authority(store)).unwrap();
    let source = snapshot
        .sources
        .iter()
        .find(|source| {
            source.namespace == "motion.artwork" && source.record_id == "source_projection"
        })
        .unwrap();
    (
        ReferenceState::parse_json(&source.fields["motion_artwork"].to_string(), ITEM).unwrap(),
        source.revision,
    )
}

#[test]
fn native40_artwork_attach_merges_current_state_after_stale_concurrent_admission() {
    let f = Fixture::at_version(40);
    let a = asset("upload-a");
    let b = asset("upload-b");
    let first = command(&f.store, "attach-a", &a);
    let stale_second = command(&f.store, "attach-b", &b);
    assert!(matches!(
        execute(&f.store, &first, &a).unwrap(),
        CommandOutcome::Committed { .. }
    ));
    let stable = all_state(&f.store);
    assert!(matches!(
        execute(&f.store, &stale_second, &b).unwrap(),
        CommandOutcome::StaleGeneration
    ));
    assert_eq!(all_state(&f.store), stable);
    // A fresh admission has no source/aggregate CAS and no stale client state.
    let second = command(&f.store, "attach-b", &b);
    execute(&f.store, &second, &b).unwrap();
    let (state, revision) = current(&f.store);
    assert_eq!(
        state.associated_asset_ids(),
        &["upload-a".to_owned(), "upload-b".to_owned()]
    );
    assert_eq!(revision, 2);
    assert!(state.contributions().is_empty());
    assert!(state.selections().is_empty());
    assert_eq!(state.assets().len(), 2);
    let receipts: i64 = f.store.db.query_row(
        "SELECT count(*) FROM catalog_command_receipts WHERE operation_id IN ('attach-a','attach-b')",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(receipts, 2);
    let inner_receipts: i64 = f.store.db.query_row(
        "SELECT count(*) FROM catalog_command_receipts WHERE json_extract(result_json,'$.operation')='metadata_system_replace'",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(inner_receipts, 0);
    let stable = all_state(&f.store);
    assert!(matches!(
        commands::execute(&f.store, &first, base(), policy()).unwrap(),
        CommandOutcome::Replay { .. }
    ));
    assert_eq!(all_state(&f.store), stable);
    assert_no_permits(&f.store);
}

#[test]
fn native40_artwork_duplicate_association_has_receipt_but_no_source_or_generation_change() {
    let f = Fixture::at_version(40);
    let asset = asset("upload");
    execute(&f.store, &command(&f.store, "attach-once", &asset), &asset).unwrap();
    let before = all_state(&f.store);
    let generation_before = generation(&f.store);
    let next = command(&f.store, "attach-already-associated", &asset);
    let outcome = execute(&f.store, &next, &asset).unwrap();
    assert_eq!(body(&outcome)["change"], "unchanged");
    assert_eq!(body(&outcome)["source_revision"], 1);
    let after = all_state(&f.store);
    for table in [
        "items",
        "catalog_state",
        "catalog_entity_revisions",
        "catalog_metadata_subjects",
        "catalog_metadata_sources",
        "catalog_metadata_history",
        "catalog_metadata_schemas",
        "catalog_metadata_schema_heads",
        "catalog_metadata_projection",
    ] {
        assert_eq!(
            before[table], after[table],
            "unexpected no-op write to {table}"
        );
    }
    assert_eq!(generation(&f.store), generation_before);
    assert_eq!(
        current(&f.store).0.associated_asset_ids(),
        &["upload".to_owned()]
    );
    assert!(matches!(
        execute(&f.store, &next, &asset).unwrap(),
        CommandOutcome::Replay { .. }
    ));
    assert_eq!(all_state(&f.store), after);
    assert_no_permits(&f.store);
}

#[test]
fn native40_artwork_attach_retains_existing_contributions_and_selection_revision() {
    let f = Fixture::at_version(40);
    let selected = asset("selected");
    let wire = json!({
        "contract_version":1,"item_id":ITEM,
        "contributions":[{"role":"poster","source":"provider","asset_id":"selected","revision":4}],
        "selections":[{"role":"poster","asset_id":"selected","revision":9}],
        "associated_asset_ids":[],"assets":[selected],"semantics":SEMANTICS,
    });
    let seed = system_command(
        &f.store,
        "seed-selected-artwork",
        0,
        json!({"motion_artwork":wire}),
    );
    system_execute(&f.store, &seed, true).unwrap();
    let before = current(&f.store).0;
    let uploaded = asset("new-upload");
    execute(
        &f.store,
        &command(&f.store, "attach-to-selected", &uploaded),
        &uploaded,
    )
    .unwrap();
    let (after, revision) = current(&f.store);
    assert_eq!(after.contributions(), before.contributions());
    assert_eq!(after.selections(), before.selections());
    assert_eq!(after.selections()[0].revision, 9);
    assert_eq!(
        after.asset_reference("selected"),
        before.asset_reference("selected")
    );
    assert_eq!(after.associated_asset_ids(), &["new-upload".to_owned()]);
    assert_eq!(revision, 2);
}

#[test]
fn native40_artwork_attach_requires_exact_descriptor_and_singleton_source_grants() {
    let f = Fixture::at_version(40);
    let asset = asset("upload");
    let command = command(&f.store, "grant-bound-attach", &asset);
    let before = all_state(&f.store);
    assert_eq!(
        commands::execute(&f.store, &command, base(), policy())
            .unwrap_err()
            .code,
        CommandStoreErrorCode::AuthorityDenied
    );
    for access in [
        MetadataAccess::Source {
            namespace: "motion.artwork".into(),
            record_id: "source_projection".into(),
            kind: SourceKind::Scan,
        },
        MetadataAccess::SourceV2 {
            namespace: "other".into(),
            record_id: "source_projection".into(),
            kind: SourceKind::Scan,
            system_fields: ["motion_artwork".into()].into(),
        },
        MetadataAccess::SourceV2 {
            namespace: "motion.artwork".into(),
            record_id: "other".into(),
            kind: SourceKind::Scan,
            system_fields: ["motion_artwork".into()].into(),
        },
        MetadataAccess::SourceV2 {
            namespace: "motion.artwork".into(),
            record_id: "source_projection".into(),
            kind: SourceKind::Local,
            system_fields: ["motion_artwork".into()].into(),
        },
        MetadataAccess::SourceV2 {
            namespace: "motion.artwork".into(),
            record_id: "source_projection".into(),
            kind: SourceKind::Scan,
            system_fields: ["motion_catalog".into()].into(),
        },
        MetadataAccess::SourceV2 {
            namespace: "motion.artwork".into(),
            record_id: "source_projection".into(),
            kind: SourceKind::Scan,
            system_fields: ["motion_artwork".into(), "motion_catalog".into()].into(),
        },
    ] {
        assert_eq!(
            execute_with_grants(&f.store, &command, &descriptor_grant(&asset), access)
                .unwrap_err()
                .code,
            CommandStoreErrorCode::AuthorityDenied
        );
        assert_eq!(all_state(&f.store), before);
    }
    for changed in 0..3 {
        let mut descriptor = descriptor_grant(&asset);
        match changed {
            0 => descriptor.subject = "other".into(),
            1 => descriptor.host_namespace = "other".into(),
            _ => descriptor.asset.content_sha256 = "b".repeat(64),
        }
        assert_eq!(
            execute_with_grants(&f.store, &command, &descriptor, source_access())
                .unwrap_err()
                .code,
            CommandStoreErrorCode::AuthorityDenied
        );
        assert_eq!(all_state(&f.store), before);
    }
    assert_no_permits(&f.store);
}

#[test]
fn native40_artwork_attach_conflicting_or_invalid_descriptor_rolls_back() {
    let f = Fixture::at_version(40);
    let original = asset("upload");
    execute(
        &f.store,
        &command(&f.store, "attach-original", &original),
        &original,
    )
    .unwrap();
    let before = all_state(&f.store);
    for changed in 0..4 {
        let mut asset = original.clone();
        match changed {
            0 => asset.content_sha256 = "b".repeat(64),
            1 => asset.owner = "untrusted-byte-owner".into(),
            2 => asset.width = 8193,
            _ => asset.content_sha256 = "bad-digest".into(),
        }
        let command = command(&f.store, &format!("invalid-attachment-{changed}"), &asset);
        assert!(execute(&f.store, &command, &asset).is_err());
        assert_eq!(all_state(&f.store), before);
    }
    assert_no_permits(&f.store);
}

#[test]
fn native40_artwork_attach_refuses_ambiguous_current_source_even_when_values_agree() {
    let f = Fixture::at_version(40);
    let original = asset("upload");
    execute(
        &f.store,
        &command(&f.store, "attach-original", &original),
        &original,
    )
    .unwrap();
    let wire: Value = serde_json::from_str(&current(&f.store).0.to_json().unwrap()).unwrap();
    let other = envelope(
        &f.store,
        "other-current-artwork-source",
        payload(MetadataMutation {
            contract_version: 1,
            change: MetadataChange::Patch {
                subject: ITEM.into(),
                namespace: "other-owner".into(),
                record_id: "record".into(),
                schema_fingerprint: current_schema(&f.store),
                expected_revision: 0,
                edits_json: json!({"motion_artwork":{"op":"set","value":wire}}).to_string(),
                evidence_json: "{}".into(),
            },
        }),
    );
    run(
        &f.store,
        &other,
        &grant(MetadataAccess::SourceV2 {
            namespace: "other-owner".into(),
            record_id: "record".into(),
            kind: SourceKind::Scan,
            system_fields: ["motion_artwork".into()].into(),
        }),
    );
    let before = all_state(&f.store);
    let next = asset("next");
    assert!(
        execute(
            &f.store,
            &command(&f.store, "ambiguous-attach", &next),
            &next
        )
        .is_err()
    );
    assert_eq!(all_state(&f.store), before);
    assert_no_permits(&f.store);
}

#[test]
fn native40_artwork_attach_refuses_extra_fields_in_the_exact_source() {
    let f = Fixture::at_version(40);
    let wire: Value =
        serde_json::from_str(&ReferenceState::empty(ITEM).unwrap().to_json().unwrap()).unwrap();
    let seed = system_command(
        &f.store,
        "artwork-extra-field-seed",
        0,
        json!({"motion_artwork":wire,"another_system_field":{"retained":true}}),
    );
    let broad_source = grant(MetadataAccess::SourceV2 {
        namespace: "motion.artwork".into(),
        record_id: "source_projection".into(),
        kind: SourceKind::Scan,
        system_fields: ["motion_artwork".into(), "another_system_field".into()].into(),
    });
    let initialization = grant(MetadataAccess::InitializeV2 {
        system_fields: Default::default(),
    });
    let extension = grant(MetadataAccess::ExtendSchema);
    commands::execute_with_authority(
        &f.store,
        &seed,
        CatalogAuthority::new(base())
            .with_metadata(&broad_source)
            .with_metadata_initialization(&initialization)
            .with_metadata_schema(&extension),
        policy(),
    )
    .unwrap();
    let before = all_state(&f.store);
    let asset = asset("new-upload");
    assert_eq!(
        execute(
            &f.store,
            &command(&f.store, "extra-fields-attach-denied", &asset),
            &asset
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::AuthorityDenied,
    );
    assert_eq!(all_state(&f.store), before);
    assert_no_permits(&f.store);
}

#[test]
fn native40_artwork_attach_composes_receipt_origin_under_one_outer_receipt() {
    let (f, creation) = created_origin_fixture();
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
    let asset = asset("origin-upload");
    let command = envelope(
        &f.store,
        "origin-artwork-attach",
        json!({
            "operation":"metadata_artwork_attach", "request":metadata::MetadataArtworkAttach {
                contract_version:1,subject:ITEM.into(),asset:asset.clone(),
                origin_creation_intent:Some(creation.canonical_payload().into()),
            },
        }),
    );
    let descriptor = descriptor_grant(&asset);
    let source = grant(source_access());
    let initialization = grant(MetadataAccess::InitializeV2 {
        system_fields: ["motion_catalog".into()].into(),
    });
    let extension = grant(MetadataAccess::ExtendSchema);
    commands::execute_with_authority(
        &f.store,
        &command,
        CatalogAuthority::new(base())
            .with_artwork_attach(&descriptor)
            .with_metadata(&source)
            .with_metadata_initialization(&initialization)
            .with_metadata_schema(&extension)
            .with_metadata_origin(&origin),
        policy(),
    )
    .unwrap();
    let snapshot = metadata::read_contributions(&f.store, &read_authority(&f.store)).unwrap();
    assert_eq!(
        snapshot
            .sources
            .iter()
            .find(|s| s.namespace == "motion.scan")
            .unwrap()
            .fields["title"],
        "Original"
    );
    assert_eq!(current(&f.store).1, 1);
    let receipt_count: i64 = f.store.db.query_row(
        "SELECT count(*) FROM catalog_command_receipts WHERE operation_id='origin-artwork-attach'",
        [], |row|row.get(0),
    ).unwrap();
    assert_eq!(receipt_count, 1);
    let stable = all_state(&f.store);
    assert!(matches!(
        commands::execute(&f.store, &command, base(), policy()).unwrap(),
        CommandOutcome::Replay { .. }
    ));
    assert_eq!(all_state(&f.store), stable);
    assert_no_permits(&f.store);
}
