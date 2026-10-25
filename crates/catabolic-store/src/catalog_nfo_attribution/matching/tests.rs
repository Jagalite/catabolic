use super::super::candidate_tests as fixture;
use super::*;
use crate::{
    catalog_commands::{self as commands, CatalogAuthority, CommandOutcome},
    catalog_metadata_store::{MetadataAccess, MetadataSystemReplace},
    migration,
};
use catabolic_core::catalog_metadata::SourceKind;
const HOST: &str = "nfo-candidate-tests";
const SUBJECT: &str = "nfo-subject";
fn projection() -> MetadataAuthority {
    MetadataAuthority {
        host_namespace: HOST.into(),
        subject: SUBJECT.into(),
        access: MetadataAccess::SourceV2 {
            namespace: "motion.structure".into(),
            record_id: "source_projection".into(),
            kind: SourceKind::Scan,
            system_fields: ["motion_catalog".into()].into(),
        },
    }
}
fn pins() -> PinAuthority {
    PinAuthority {
        host_namespace: HOST.into(),
        readable: Default::default(),
        mutable: [("nfo".into(), [SUBJECT.into()].into())].into(),
    }
}
fn ready(extra: bool) -> fixture::Fixture {
    let f = fixture::Fixture::imported(false);
    let mut ids = vec![json!({"provider":"nfo","value":"tmdb:42","namespace_version":""})];
    if extra {
        ids.push(json!({"provider":"other","value":"retained-other","namespace_version":""}));
    }
    let request=MetadataSystemReplace{contract_version:1,subject:SUBJECT.into(),namespace:"motion.structure".into(),record_id:"source_projection".into(),expected_schema_fingerprint:metadata::current_schema_fingerprint(&f.store.db,SUBJECT).unwrap(),expected_revision:0,fields_json:json!({"motion_catalog":{"contract_version":1,"item_id":SUBJECT,"match_state":"manual","external_ids":ids,"unrelated":"preserved"}}).to_string(),evidence_json:json!({"owner":"actual controlled matching fixture"}).to_string(),origin_creation_intent:None};
    let command = fixture::envelope(
        &f.store,
        "matching-structure",
        json!({"operation":"metadata_system_replace","request":request}),
    );
    let grant = projection();
    let init = MetadataAuthority {
        host_namespace: HOST.into(),
        subject: SUBJECT.into(),
        access: MetadataAccess::InitializeV2 {
            system_fields: Default::default(),
        },
    };
    let extension = MetadataAuthority {
        host_namespace: HOST.into(),
        subject: SUBJECT.into(),
        access: MetadataAccess::ExtendSchema,
    };
    commands::execute_with_authority(
        &f.store,
        &command,
        CatalogAuthority::new(fixture::authority(true))
            .with_metadata(&grant)
            .with_metadata_initialization(&init)
            .with_metadata_schema(&extension),
        fixture::policy(),
    )
    .unwrap();
    fixture::pin(
        &f.store,
        "manual-pin",
        SUBJECT,
        0,
        PinDecision::Set {
            value: "tmdb:42".into(),
        },
    )
    .unwrap();
    let f = f.install();
    f.bootstrap();
    f
}
fn request(store: &Store) -> UnmatchRequest {
    UnmatchRequest{contract_version:1,subject:SUBJECT.into(),expected_item_revision:store.db.query_row("SELECT revision FROM catalog_entity_revisions WHERE entity_kind='items' AND entity_key=json_array(?) AND deleted=0",[SUBJECT],|r|r.get(0)).unwrap(),expected_pin_revision:1,expected_schema_fingerprint:metadata::current_schema_fingerprint(&store.db,SUBJECT).unwrap().unwrap()}
}
fn execute(
    store: &Store,
    outer: &CommandEnvelope,
    request: &UnmatchRequest,
    authorized: bool,
) -> commands::Result<CommandOutcome> {
    let subjects = if authorized {
        [SUBJECT.into()].into()
    } else {
        BTreeSet::new()
    };
    let pins = pins();
    let projection = [projection()];
    let auth = UnmatchAuthority {
        host_namespace: HOST,
        mutable_subjects: &subjects,
        pins: &pins,
        projection: &projection,
    };
    commands::execute_candidate_for_test(
        store,
        outer,
        fixture::authority(true),
        fixture::policy(),
        |store, outer, at| apply_unmatch_in(store, outer, request, &auth, at),
    )
}
fn retained(store: &Store) -> Vec<Vec<Value>> {
    ["SELECT * FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.nfo' ORDER BY record_id",
     "SELECT * FROM catalog_metadata_history WHERE item_id=? AND namespace='motion.nfo' ORDER BY record_id,revision",
     "SELECT * FROM catalog_nfo_states WHERE item_id=?",
     "SELECT * FROM catalog_nfo_state_revisions WHERE item_id=? ORDER BY revision",
     "SELECT * FROM catalog_nfo_identity_claims WHERE item_id=? ORDER BY value",
     "SELECT * FROM catalog_nfo_file_attributions WHERE item_id=?",
     "SELECT * FROM item_tags WHERE item_id=? ORDER BY id"].iter().map(|sql|migration::rows(&store.db,sql,&[SUBJECT.to_owned().into()]).unwrap()).collect()
}
#[test]
fn actual_unmatch_preserves_identity_collision_and_sources_replays_and_reopens() {
    let mut f = ready(false);
    let before = retained(&f.store);
    let request = request(&f.store);
    let outer = fixture::envelope(
        &f.store,
        "actual-unmatch",
        json!({"operation":"nfo_unmatch","request":request}),
    );
    assert!(matches!(
        execute(&f.store, &outer, &request, true).unwrap(),
        CommandOutcome::Committed { .. }
    ));
    assert_eq!(retained(&f.store), before);
    let (revision,value):(i64,Option<String>)=f.store.db.query_row("SELECT current_revision,value FROM catalog_identity_pins WHERE item_id=? AND namespace='nfo'",[SUBJECT],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!((revision, value), (2, None));
    let marker:String=f.store.db.query_row("SELECT json_extract(effective_json,'$.motion_catalog') FROM catalog_metadata_projection WHERE item_id=?",[SUBJECT],|r|r.get(0)).unwrap();
    let marker: Value = serde_json::from_str(&marker).unwrap();
    assert_eq!(marker["match_state"], "unmatched");
    assert_eq!(marker["external_ids"][0]["value"], "tmdb:42");
    assert_eq!(marker["unrelated"], "preserved");
    assert!(
        fixture::pin(
            &f.store,
            "collision-after-unmatch",
            "other-subject",
            0,
            PinDecision::Set {
                value: "tmdb:42".into()
            }
        )
        .is_err()
    );
    let after = migration::snapshot(&f.store.db).unwrap();
    commands::execute_candidate_for_test(
        &f.store,
        &outer,
        fixture::authority(false),
        fixture::policy(),
        |_, _, _| -> Result<Value> { panic!("replay must not prepare current authority") },
    )
    .unwrap();
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), after);
    let path = f.store.path.clone();
    drop(f.store);
    f.store = crate::candidate41_test_support::open_candidate41_for_test(&path, true).unwrap();
    commands::execute_candidate_for_test(
        &f.store,
        &outer,
        fixture::authority(false),
        fixture::policy(),
        |_, _, _| -> Result<Value> { panic!("reopen replay must not apply") },
    )
    .unwrap();
    assert_eq!(retained(&f.store), before);
}
#[test]
fn actual_unmatch_refuses_uncovered_identity_and_missing_current_curation_without_effects() {
    for extra in [false, true] {
        let f = ready(extra);
        let before = migration::snapshot(&f.store.db).unwrap();
        let request = request(&f.store);
        let outer = fixture::envelope(
            &f.store,
            "denied-unmatch",
            json!({"operation":"nfo_unmatch","request":request}),
        );
        assert!(execute(&f.store, &outer, &request, extra).is_err());
        assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    }
}
#[test]
fn actual_unmatch_swallowed_post_projection_error_cannot_commit_partial_unpin() {
    let f = ready(false);
    let before = migration::snapshot(&f.store.db).unwrap();
    let request = request(&f.store);
    let outer = fixture::envelope(
        &f.store,
        "caught-unmatch",
        json!({"operation":"nfo_unmatch","request":request}),
    );
    let subjects = [SUBJECT.into()].into();
    let pins = pins();
    let projection = [projection()];
    let auth = UnmatchAuthority {
        host_namespace: HOST,
        mutable_subjects: &subjects,
        pins: &pins,
        projection: &projection,
    };
    let result: Result<()> = f.store.transaction_detailed(|_| {
        let mut probe = |store: &Store| {
            let pin: Option<String> = store.db.query_row(
                "SELECT value FROM catalog_identity_pins WHERE item_id=? AND namespace='nfo'",
                [SUBJECT],
                |r| r.get(0),
            )?;
            assert!(pin.is_none());
            Err(refused("actual late failure"))
        };
        assert!(
            apply_unmatch_with_probe_in(&f.store, &outer, &request, &auth, 1, &mut probe).is_err()
        );
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    // The real receipt corridor must also reject a caller that catches the
    // publication error and returns a nominal result instead of propagating it.
    let caught = commands::execute_candidate_for_test(
        &f.store,
        &outer,
        fixture::authority(true),
        fixture::policy(),
        |store, command, at| {
            let mut probe = |_: &Store| Err(refused("caught terminal publication error"));
            assert!(
                apply_unmatch_with_probe_in(store, command, &request, &auth, at, &mut probe)
                    .is_err()
            );
            Ok(json!({"incorrectly_swallowed":true}))
        },
    );
    assert!(caught.is_err());
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
}

#[test]
fn actual_unmatch_requires_each_pin_and_marker_grant_and_exact_current_heads() {
    let f = ready(false);
    let before = migration::snapshot(&f.store.db).unwrap();
    for case in 0..4 {
        let mut request = request(&f.store);
        if case == 2 {
            request.expected_pin_revision += 1;
        }
        if case == 3 {
            request.expected_item_revision += 1;
        }
        let outer = fixture::envelope(
            &f.store,
            &format!("unmatch-grant-{case}"),
            json!({"operation":"nfo_unmatch","request":request}),
        );
        let subjects = [SUBJECT.into()].into();
        let mut pins = pins();
        if case == 0 {
            pins.mutable.clear();
        }
        let projection = if case == 1 {
            vec![]
        } else {
            vec![projection()]
        };
        let auth = UnmatchAuthority {
            host_namespace: HOST,
            mutable_subjects: &subjects,
            pins: &pins,
            projection: &projection,
        };
        assert!(
            commands::execute_candidate_for_test(
                &f.store,
                &outer,
                fixture::authority(true),
                fixture::policy(),
                |store, outer, at| apply_unmatch_in(store, outer, &request, &auth, at)
            )
            .is_err()
        );
        assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    }
}

#[test]
fn actual_already_unmatched_keeps_pin_tombstone_source_heads_and_generation() {
    let f = ready(false);
    let first = request(&f.store);
    let command = fixture::envelope(
        &f.store,
        "first-unmatch",
        json!({"operation":"nfo_unmatch","request":first}),
    );
    execute(&f.store, &command, &first, true).unwrap();
    let before = retained(&f.store);
    let at = generation(&f.store).unwrap();
    let structural=migration::rows(&f.store.db,"SELECT * FROM catalog_metadata_history WHERE item_id=? AND namespace='motion.structure' ORDER BY revision",&[SUBJECT.to_owned().into()]).unwrap();
    let mut request = request(&f.store);
    request.expected_pin_revision = 2;
    let outer = fixture::envelope(
        &f.store,
        "already-unmatched",
        json!({"operation":"nfo_unmatch","request":request}),
    );
    execute(&f.store, &outer, &request, true).unwrap();
    assert_eq!(generation(&f.store).unwrap(), at);
    assert_eq!(retained(&f.store), before);
    assert_eq!(migration::rows(&f.store.db,"SELECT * FROM catalog_metadata_history WHERE item_id=? AND namespace='motion.structure' ORDER BY revision",&[SUBJECT.to_owned().into()]).unwrap(),structural);
}
