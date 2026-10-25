//! Actual managed-candidate41 non-identity system owner. These reference values
//! describe independently verified fixture assets, never permission to read bytes.
use super::*;
use catabolic_core::artwork::native::{
    ASSET_OWNER, AssetReference, ReferenceState, plan_associate_upload,
};
use metadata::MetadataSystemReplace;

const ART: &str = "motion_artwork";
fn artwork() -> Value {
    let empty = ReferenceState::empty(SUBJECT).unwrap();
    let asset = AssetReference {
        asset_id: "fixture-asset".into(),
        mime: "image/png".into(),
        width: 4,
        height: 4,
        size_bytes: 64,
        content_sha256: "ab".repeat(32),
        owner: ASSET_OWNER.into(),
    };
    serde_json::from_str(
        &plan_associate_upload(&empty, &asset)
            .unwrap()
            .state
            .to_json()
            .unwrap(),
    )
    .unwrap()
}
fn source(subject: &str, namespace: &str, fields: &[&str]) -> MetadataAuthority {
    MetadataAuthority {
        host_namespace: HOST.into(),
        subject: subject.into(),
        access: MetadataAccess::SourceV2 {
            namespace: namespace.into(),
            record_id: "source_projection".into(),
            kind: SourceKind::Scan,
            system_fields: fields.iter().map(|s| s.to_string()).collect(),
        },
    }
}
fn initialize(subject: &str, fields: &[&str]) -> MetadataAuthority {
    MetadataAuthority {
        host_namespace: HOST.into(),
        subject: subject.into(),
        access: MetadataAccess::InitializeV2 {
            system_fields: fields.iter().map(|s| s.to_string()).collect(),
        },
    }
}
fn system_request(
    store: &Store,
    subject: &str,
    namespace: &str,
    revision: i64,
    fields: Value,
) -> MetadataSystemReplace {
    MetadataSystemReplace {
        contract_version: 1,
        subject: subject.into(),
        namespace: namespace.into(),
        record_id: "source_projection".into(),
        expected_schema_fingerprint: metadata::current_schema_fingerprint(&store.db, subject)
            .unwrap(),
        expected_revision: revision,
        fields_json: fields.to_string(),
        evidence_json: json!({"owner":"controlled.artwork.fixture"}).to_string(),
        origin_creation_intent: None,
    }
}
fn command(store: &Store, id: &str, request: &MetadataSystemReplace) -> CommandEnvelope {
    fixture::envelope(
        store,
        id,
        json!({"operation":"metadata_system_replace","request":request}),
    )
}
fn execute(
    store: &Store,
    command: &CommandEnvelope,
    grant: &MetadataAuthority,
    initial: &MetadataAuthority,
) -> std::result::Result<CommandOutcome, commands::CommandStoreError> {
    let extension = MetadataAuthority {
        host_namespace: HOST.into(),
        subject: grant.subject.clone(),
        access: MetadataAccess::ExtendSchema,
    };
    let grants = CatalogAuthority::new(fixture::authority(true))
        .with_metadata(grant)
        .with_metadata_initialization(initial)
        .with_metadata_schema(&extension);
    if store.schema_version == 41 {
        commands::execute_candidate_dispatch_for_test(store, command, grants, fixture::policy())
    } else {
        commands::execute_with_authority(store, command, grants, fixture::policy())
    }
}
fn retained(store: &Store) -> Vec<Vec<Value>> {
    [
        "SELECT * FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.nfo' ORDER BY record_id",
        "SELECT * FROM catalog_metadata_history WHERE item_id=? AND namespace='motion.nfo' ORDER BY record_id,revision",
        "SELECT * FROM catalog_nfo_states WHERE item_id=?",
        "SELECT * FROM catalog_nfo_state_revisions WHERE item_id=? ORDER BY revision",
        "SELECT * FROM catalog_nfo_identity_claims WHERE item_id=? ORDER BY value",
        "SELECT * FROM catalog_nfo_file_attributions WHERE item_id=?",
        "SELECT * FROM catalog_identity_pins WHERE item_id=? ORDER BY namespace",
        "SELECT * FROM item_tags WHERE item_id=? ORDER BY id",
        "SELECT * FROM catalog_item_tag_suppressions WHERE item_id=? ORDER BY tag_id",
    ].into_iter().map(|sql|migration::rows(&store.db,sql,&[SUBJECT.to_owned().into()]).unwrap()).collect()
}
#[test]
fn actual_managed_artwork_system_replacement_preserves_nfo_origin_claim_proof_and_schema_ancestry()
{
    let mut f = fixture::Fixture::imported(true).install();
    f.bootstrap();
    let before = retained(&f.store);
    let effective_before: Value = serde_json::from_str(
        &f.store
            .db
            .query_row("SELECT metadata FROM items WHERE id=?", [SUBJECT], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
    )
    .unwrap();
    // Imported legacy metadata retains precedence over the provider document.
    // The artwork owner must preserve BOTH that effective value and the NFO
    // source, rather than silently reconcile legacy metadata on its behalf.
    assert_eq!(effective_before["title"], "Original");
    let provider: (String, String) = f.store.db.query_row(
        "SELECT source_kind,fields_json FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.nfo'",
        [SUBJECT], |r| Ok((r.get(0)?,r.get(1)?)),
    ).unwrap();
    assert_eq!(provider.0, "provider");
    let provider_fields: Value = serde_json::from_str(&provider.1).unwrap();
    assert_eq!(provider_fields["title"], "Imported NFO");
    assert_eq!(before[4].len(), 1, "positive automatic claim fixture");
    assert_eq!(
        before[5].len(),
        1,
        "positive physical-origin attribution fixture"
    );
    let old = metadata::current_schema_fingerprint(&f.store.db, SUBJECT)
        .unwrap()
        .unwrap();
    let old_registry = migration::rows(
        &f.store.db,
        "SELECT * FROM catalog_metadata_schemas WHERE item_id=? AND schema_fingerprint=?",
        &[SUBJECT.to_owned().into(), old.clone().into()],
    )
    .unwrap();
    let request = system_request(
        &f.store,
        SUBJECT,
        "motion.artwork",
        0,
        json!({"motion_artwork":artwork()}),
    );
    let first = command(&f.store, "managed-artwork", &request);
    committed(
        execute(
            &f.store,
            &first,
            &source(SUBJECT, "motion.artwork", &[ART]),
            &initialize(SUBJECT, &[]),
        )
        .unwrap(),
    );
    assert_eq!(retained(&f.store), before);
    let mut effective_after: Value = serde_json::from_str(
        &f.store
            .db
            .query_row("SELECT metadata FROM items WHERE id=?", [SUBJECT], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        effective_after.as_object_mut().unwrap().remove(ART),
        Some(artwork())
    );
    assert_eq!(
        effective_after, effective_before,
        "artwork cannot mask any NFO public value"
    );
    let current = metadata::current_schema_fingerprint(&f.store.db, SUBJECT)
        .unwrap()
        .unwrap();
    assert_ne!(current, old);
    assert_eq!(
        migration::rows(
            &f.store.db,
            "SELECT * FROM catalog_metadata_schemas WHERE item_id=? AND schema_fingerprint=?",
            &[SUBJECT.to_owned().into(), old.clone().into()]
        )
        .unwrap(),
        old_registry
    );
    let parent:String=f.store.db.query_row("SELECT prior_fingerprint FROM catalog_metadata_schemas WHERE item_id=? AND schema_fingerprint=?",rusqlite::params![SUBJECT,current],|r|r.get(0)).unwrap();
    assert_eq!(parent, old);
    f.store
        .transaction_detailed::<_, Error>(|_| {
            let state = state::read_in(&f.store, SUBJECT, generation(&f.store), 7)?;
            assert_eq!(state.native_source().unwrap().schema_fingerprint(), old);
            assert!(state.schema().unwrap().system_fields().contains(ART));
            assert!(state.origin_physical().is_some());
            Ok(())
        })
        .unwrap();
    let snapshot = migration::snapshot(&f.store.db).unwrap();
    assert!(matches!(
        commands::execute_candidate_dispatch_for_test(
            &f.store,
            &first,
            CatalogAuthority::new(fixture::authority(false)),
            fixture::policy()
        )
        .unwrap(),
        CommandOutcome::Replay { .. }
    ));
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), snapshot);
    let path = f.store.path.clone();
    drop(f.store);
    f.store = candidate::open_candidate41_for_test(&path, true).unwrap();
    assert!(matches!(
        commands::execute_candidate_dispatch_for_test(
            &f.store,
            &first,
            CatalogAuthority::new(fixture::authority(false)),
            fixture::policy()
        )
        .unwrap(),
        CommandOutcome::Replay { .. }
    ));
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), snapshot);
    let remove = system_request(&f.store, SUBJECT, "motion.artwork", 1, json!({}));
    let removal = command(&f.store, "remove-artwork-reference", &remove);
    committed(
        execute(
            &f.store,
            &removal,
            &source(SUBJECT, "motion.artwork", &[ART]),
            &initialize(SUBJECT, &[ART]),
        )
        .unwrap(),
    );
    assert_eq!(retained(&f.store), before);
    assert_eq!(
        metadata::current_schema_fingerprint(&f.store.db, SUBJECT)
            .unwrap()
            .unwrap(),
        current
    );
    assert_eq!(f.store.db.query_row("SELECT fields_json FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.artwork'",[SUBJECT],|r|r.get::<_,String>(0)).unwrap(),"{}");
    let effective_removed: Value = serde_json::from_str(
        &f.store
            .db
            .query_row("SELECT metadata FROM items WHERE id=?", [SUBJECT], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
    )
    .unwrap();
    assert_eq!(effective_removed, effective_before);
}

#[test]
fn managed_system_replacement_refuses_identity_origin_unmanaged_and_unrelated_classification() {
    let f = fixture::Fixture::ready();
    for case in 0..8 {
        let mut request = system_request(
            &f.store,
            SUBJECT,
            "motion.artwork",
            0,
            json!({"motion_artwork":artwork()}),
        );
        let mut grant = source(SUBJECT, "motion.artwork", &[ART]);
        let mut initial = initialize(SUBJECT, &[]);
        match case {
            0 => {
                request.namespace = "motion.nfo".into();
                grant = source(SUBJECT, "motion.nfo", &[ART]);
            }
            1 => {
                request.fields_json = json!({"motion_catalog":{"external_ids":[]}}).to_string();
                grant = source(SUBJECT, "motion.artwork", &["motion_catalog"]);
            }
            2 => {
                initial = initialize(SUBJECT, &["title"]);
            }
            3 => {
                initial = initialize(SUBJECT, &["unknown_system"]);
            }
            4 => {
                grant = source(SUBJECT, "motion.artwork", &[ART, "unused_classification"]);
            }
            5 => {
                request.origin_creation_intent = Some("retained-but-not-a-grant".into());
            }
            6 => {
                request.subject = OTHER.into();
                request.expected_schema_fingerprint = None;
                grant = source(OTHER, "motion.artwork", &[ART]);
                initial = initialize(OTHER, &[]);
            }
            7 => {
                request.fields_json = json!({"title":"masked provider value"}).to_string();
                grant = source(SUBJECT, "motion.artwork", &["title"]);
            }
            _ => unreachable!(),
        }
        let command = command(&f.store, &format!("denied-system-{case}"), &request);
        let before = migration::snapshot(&f.store.db).unwrap();
        assert!(
            execute(&f.store, &command, &grant, &initial).is_err(),
            "case {case}"
        );
        assert_eq!(
            migration::snapshot(&f.store.db).unwrap(),
            before,
            "case {case}"
        );
    }
    // A genuine40 source that also owns the identity marker cannot enter this
    // corridor merely by omitting that marker from its replacement/grant.
    let f = fixture::Fixture::created();
    let old = system_request(
        &f.store,
        SUBJECT,
        "motion.mixed",
        0,
        json!({"motion_artwork":artwork(),"motion_catalog":{"contract_version":1,"item_id":SUBJECT,"external_ids":[]}}),
    );
    let create = command(&f.store, "old-mixed-system", &old);
    committed(
        execute(
            &f.store,
            &create,
            &source(SUBJECT, "motion.mixed", &[ART, "motion_catalog"]),
            &initialize(SUBJECT, &[]),
        )
        .unwrap(),
    );
    let f = f.install();
    f.bootstrap();
    let request = system_request(
        &f.store,
        SUBJECT,
        "motion.mixed",
        1,
        json!({"motion_artwork":artwork()}),
    );
    let mixed_command = command(&f.store, "mixed-system-drop-marker", &request);
    let before = migration::snapshot(&f.store.db).unwrap();
    assert!(
        execute(
            &f.store,
            &mixed_command,
            &source(SUBJECT, "motion.mixed", &[ART]),
            &initialize(SUBJECT, &[])
        )
        .is_err()
    );
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);

    // Actual contract1 activation retains an external motion_catalog policy.
    // Refuse this system corridor rather than strand that marker in a new v2
    // schema without the independent protected-classification composition.
    let f = ImportFixture::empty();
    let batch = import_batch(1);
    let grants = typed_grants(&batch);
    Staging::stage(&f.store, "original-v1-system-root", batch, grants).activate(&f.store);
    let request = system_request(
        &f.store,
        SUBJECT,
        "motion.artwork",
        0,
        json!({"motion_artwork":artwork()}),
    );
    let command = command(&f.store, "unclassified-marker-system-denied", &request);
    let before = migration::snapshot(&f.store.db).unwrap();
    let error = execute(
        &f.store,
        &command,
        &source(SUBJECT, "motion.artwork", &[ART]),
        &initialize(SUBJECT, &[]),
    )
    .unwrap_err();
    assert!(error.to_string().contains("protected-classification"));
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
}

#[test]
fn late_managed_system_error_keeps_real_commit_barrier_and_exact_retry() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    let f = fixture::Fixture::ready();
    let request = system_request(
        &f.store,
        SUBJECT,
        "motion.artwork",
        0,
        json!({"motion_artwork":artwork()}),
    );
    let command = command(&f.store, "system-late-failure", &request);
    let before = migration::snapshot(&f.store.db).unwrap();
    let nfo = retained(&f.store);
    let attempted = std::cell::Cell::new(false);
    let outcome=f.store.transaction_detailed::<_,Error>(|db| {
        let reset=crate::catalog_source_succession::authorizer::Guard::for_store(&f.store)?;
        reset.replace(|context:AuthContext<'_>| {
            if context.database_name==Some("main") && matches!(context.action,AuthAction::Delete{table_name:"catalog_metadata_permits"}) { Authorization::Deny } else { Authorization::Allow }
        })?;
        let result=metadata::replace_system(&f.store,&request,&command,&source(SUBJECT,"motion.artwork",&[ART]),&initialize(SUBJECT,&[]),&extension(),None,1);
        reset.restore()?;
        assert!(result.is_err());
        assert_eq!(db.query_row("SELECT revision FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.artwork'",[SUBJECT],|r|r.get::<_,i64>(0))?,1);
        assert_ne!(metadata::current_schema_fingerprint(db,SUBJECT).unwrap(),request.expected_schema_fingerprint);
        assert_eq!(retained(&f.store),nfo);
        assert!(db.prepare("PRAGMA foreign_key_check(catalog_metadata_permits)")?.exists([])?);
        attempted.set(true);
        Ok(())
    });
    assert!(attempted.get());
    assert!(outcome.is_err());
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    committed(
        execute(
            &f.store,
            &command,
            &source(SUBJECT, "motion.artwork", &[ART]),
            &initialize(SUBJECT, &[]),
        )
        .unwrap(),
    );
    assert_eq!(retained(&f.store), nfo);
}
