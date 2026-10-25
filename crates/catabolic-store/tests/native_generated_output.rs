//! Generated-output admission owner tests with real filesystem identity and
//! fixture-provided measurement/transform grants, not encoder qualification.
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility};
use catabolic_store::{
    Store,
    catalog_commands::{
        self as commands, CatalogAuthority, CommandOutcome, ReceiptPolicy, TrustedCommandAuthority,
    },
    catalog_discovery::{FreshMediaDiscoveryAuthority, MeasuredOriginalGrant},
    catalog_generated_output::{GeneratedOutputAuthority, GeneratedOutputRequest},
    catalog_identity_mutation::IdentityAuthority,
    catalog_import_typed::*,
    migration,
};
use serde_json::{Value, json};
use std::{fs, fs::File, os::unix::fs::MetadataExt, sync::Arc, time::Duration};
fn base() -> TrustedCommandAuthority<'static> {
    TrustedCommandAuthority {
        host_namespace: "generated-test",
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: true,
        identity_authority: None,
    }
}
fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::ZERO,
        max_receipts: 1000,
        max_hold_records: 1000,
        max_logical_bytes: 16 * 1024 * 1024,
    }
}
struct Fixture {
    root: tempfile::TempDir,
    store: Store,
    grant: GeneratedOutputAuthority,
}
impl Fixture {
    fn generation(&self) -> i64 {
        self.store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap()
    }
    fn envelope(&self, key: &str, request: &GeneratedOutputRequest) -> CommandEnvelope {
        CommandEnvelope::new(
            1,
            CommandKey::new(&self.store.database_id, "generated-test", key).unwrap(),
            self.store.catalog_incarnation.as_deref().unwrap(),
            self.generation(),
            &json!({"operation":"generated_output_admit","request":request}).to_string(),
        )
        .unwrap()
    }
    fn count(&self, table: &str) -> i64 {
        self.store
            .db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    fn new() -> Self {
        Self::with_motion_types(false)
    }
    fn with_motion_types(motion: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("catalog.db");
        migration::initialize_native_at(&db, 39).unwrap();
        let store = Store::open(&db, true, false).unwrap();
        for location in ["input", "output"] {
            let path = root.path().join(location);
            fs::create_dir(&path).unwrap();
            catabolic_store::observation::execute(
                &store,
                "default",
                "bind",
                &json!({"source":location,"root":path}),
            )
            .unwrap();
            fs::write(path.join("media.mkv"), location.as_bytes()).unwrap();
            let st = path.join("media.mkv").metadata().unwrap();
            store.transaction(|db|{db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES(?,'default',?,1,1,'[]')",[location,location])?;db.execute("INSERT INTO files VALUES(?,?, 'media.mkv')",[location,location])?;db.execute("INSERT INTO observations VALUES('default',?,?,?,?,?,'present',?)",rusqlite::params![location,st.len() as i64,st.mtime()*1_000_000_000+st.mtime_nsec(),st.dev() as i64,st.ino() as i64,location])?;db.execute("INSERT INTO native_source_ctimes VALUES('default',?,?,?,?,?,?)",rusqlite::params![location,st.len() as i64,st.mtime()*1_000_000_000+st.mtime_nsec(),st.dev() as i64,st.ino() as i64,st.ctime()*1_000_000_000+st.ctime_nsec()])?;Ok(())}).unwrap();
        }
        let mut typed = TypedAuthority {
            physical_files: [("default".into(), ["input".into(), "output".into()].into())].into(),
            ..Default::default()
        };
        let input =
            capture_existing_occurrence(&store, "input-occurrence", "default", "input", &typed)
                .unwrap();
        let output =
            capture_existing_occurrence(&store, "output-occurrence", "default", "output", &typed)
                .unwrap();
        let ctime = |name: &str| {
            let st = root.path().join(name).join("media.mkv").metadata().unwrap();
            st.ctime() * 1_000_000_000 + st.ctime_nsec()
        };
        let interval = json!({"start_us":0,"end_us":1_000_000});
        let original = json!({"contract_version":1,"profile":"default","context":{"id":"context","authority_namespace":"generated-test","context_key":"work","expected_revision":null},"timelines":[{"timeline_id":"timeline","edition_item_id":"edition","expected_revision":null,"identity_state":"provisional","coverage":interval}],"versions":[{"expected_revision":null,"expected_timeline_revision":1,"version":{"contract_version":1,"version_id":"original","association_item_id":"original-item","edition_item_id":"edition","timeline_id":"timeline","state":"ready","declared_coverage":interval,"bindings":[{"binding_id":"original-binding","association_id":"input-association","file":input.exact,"part":1,"source":{"kind":"whole_file","duration_us":1_000_000},"timeline_interval":interval}]},"equivalence":null,"expected_binding_revisions":{"original-binding":null}}],"evidence":[]});
        let native = NativeIdentityBatch {
            occurrences: vec![input.clone()],
            associations: vec![ImportedAssociation {
                source_association_id: "input-association".into(),
                source_occurrence_id: "input-occurrence".into(),
                item_id: "original-item".into(),
                part: 1,
            }],
            mutations: vec![original],
        };
        let identity = IdentityAuthority {
            authority_namespace: "generated-test".into(),
            context_keys: ["work".into()].into(),
            allow_declared_evidence: false,
            allow_verified_evidence: false,
        };
        let discovery = FreshMediaDiscoveryAuthority {
            host_namespace: "generated-test".into(),
            new_item_ids: ["edition".into(), "original-item".into()].into(),
            physical_files: typed.physical_files.clone(),
            identity: identity.clone(),
            relationship_edges: Default::default(),
            measured_originals: [(
                "original".into(),
                MeasuredOriginalGrant {
                    file: input.exact.clone(),
                    duration_us: 1_000_000,
                    observed_ctime_ns: ctime("input"),
                },
            )]
            .into(),
        };
        let generation = store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap();
        let command=CommandEnvelope::new(1,CommandKey::new(&store.database_id,"generated-test","original").unwrap(),store.catalog_incarnation.as_deref().unwrap(),generation,&json!({"operation":"fresh_media_discovery","items":[{"id":"edition","kind":if motion {"custom:motion_edition"} else {"video"},"metadata":{}},{"id":"original-item","kind":if motion {"custom:motion_version"} else {"video"},"metadata":{}}],"native":native,"relationships":[]}).to_string()).unwrap();
        commands::execute_with_authority(
            &store,
            &command,
            CatalogAuthority::new(base()).with_discovery(&discovery),
            policy(),
        )
        .unwrap();
        if motion {
            catabolic_store::curation::item_put(
                &store,
                &json!({"id":"work","kind":"video","metadata":{}}),
            )
            .unwrap();
            for (source, target, kind) in [
                ("work", "edition", "custom:motion_edition"),
                ("edition", "original-item", "custom:motion_version"),
            ] {
                catabolic_store::curation::relate(
                    &store,
                    &json!({"source":source,"target":target,"kind":kind}),
                )
                .unwrap();
            }
        }
        typed.identity = Some(IdentityAuthority {
            allow_declared_evidence: true,
            ..identity
        });
        typed.existing_items.insert(
            "edition".into(),
            capture_existing_item(
                &store,
                "generated-test",
                "edition",
                &["edition".into()].into(),
            )
            .unwrap(),
        );
        typed.existing_contexts.insert(
            "context".into(),
            capture_existing_context(
                &store,
                "generated-test",
                "context",
                &["context".into()].into(),
            )
            .unwrap(),
        );
        typed.existing_timelines.insert(
            "timeline".into(),
            capture_existing_timeline(
                &store,
                "generated-test",
                "timeline",
                &["timeline".into()].into(),
            )
            .unwrap(),
        );
        typed.relationships = Some(RelationshipImportAuthority {
            host_namespace: "generated-test".into(),
            edges: [(
                "edition".into(),
                "generated-item".into(),
                "custom:motion_version".into(),
            )]
            .into(),
        });
        let mut mutation = json!({"contract_version":1,"profile":"default","context":{"id":"context","authority_namespace":"generated-test","context_key":"work","expected_revision":1},"timelines":[],"versions":[{"expected_revision":null,"expected_timeline_revision":1,"version":{"contract_version":1,"version_id":"generated-version","association_item_id":"generated-item","edition_item_id":"edition","timeline_id":"timeline","state":"ready","declared_coverage":interval,"bindings":[{"binding_id":"generated-binding","association_id":"output-association","file":output.exact,"part":1,"source":{"kind":"whole_file","duration_us":1_000_000},"timeline_interval":interval}]},"equivalence":{"basis":"declared","evidence_id":"render-evidence"},"expected_binding_revisions":{"generated-binding":null}}],"evidence":[{"id":"render-evidence","basis":"declared","claims":[{"edition_item_id":"edition","timeline_id":"timeline","timeline_revision":1,"version_id":"generated-version","version_revision":1,"exact_files":[output.exact]}],"evidence":{"fixture":"trusted exact transform declaration, not duration inference"}}]});
        mutation["timelines"] = json!([{"timeline_id":"timeline","edition_item_id":"edition","expected_revision":1,"identity_state":"established","coverage":interval}]);
        mutation["versions"][0]["expected_timeline_revision"] = json!(2);
        mutation["evidence"][0]["claims"][0]["timeline_revision"] = json!(2);
        mutation["evidence"][0]["claims"].as_array_mut().unwrap().push(json!({"edition_item_id":"edition","timeline_id":"timeline","timeline_revision":2,"version_id":"original","version_revision":2,"exact_files":[input.exact]}));
        let request = GeneratedOutputRequest {
            profile: "default".into(),
            job_id: "motion-job".into(),
            attempt: 1,
            preparation_digest: "a".repeat(64),
            provenance: json!({"recipe":"fixture","completion":"validated"}),
            input_version_id: "original".into(),
            input_version_revision: 1,
            input_timeline_revision: 1,
            establish_provisional: true,
            input: input.exact,
            input_ctime_ns: ctime("input"),
            output: output.clone(),
            output_ctime_ns: ctime("output"),
            output_size: 6,
            output_sha256: catabolic_core::sha256(b"output"),
            output_duration_us: 1_000_000,
            output_binding: catabolic_store::observation::binding(
                &store, "default", "source", "output",
            )
            .unwrap(),
            item: json!({"id":"generated-item","kind":if motion {"custom:motion_version"} else {"video"},"metadata":{}}),
            native: NativeIdentityBatch {
                occurrences: vec![output],
                associations: vec![ImportedAssociation {
                    source_association_id: "output-association".into(),
                    source_occurrence_id: "output-occurrence".into(),
                    item_id: "generated-item".into(),
                    part: 1,
                }],
                mutations: vec![mutation],
            },
            relationships: vec![ImportedRelationship {
                source_relationship_id: "edition-generated".into(),
                source_revision: 1,
                source_item_id: "edition".into(),
                target_item_id: "generated-item".into(),
                kind: "custom:motion_version".into(),
                position: None,
                metadata: json!({}),
            }],
        };
        let grant = GeneratedOutputAuthority {
            host_namespace: "generated-test".into(),
            request,
            output_file: Arc::new(File::open(root.path().join("output/media.mkv")).unwrap()),
            typed,
        };
        Self { root, store, grant }
    }
}
#[test]
fn generated_admission_is_atomic_receipt_owned_and_replays_without_execution() {
    let f = Fixture::new();
    let command = f.envelope("publish", &f.grant.request);
    assert!(
        commands::execute_with_authority(
            &f.store,
            &command,
            CatalogAuthority::new(base()),
            policy()
        )
        .is_err()
    );
    f.store.db.execute_batch("CREATE TEMP TRIGGER fixture_fail_generated_receipt BEFORE INSERT ON catalog_command_receipts BEGIN SELECT RAISE(ABORT,'injected receipt failure'); END;").unwrap();
    assert!(
        commands::execute_with_authority(
            &f.store,
            &command,
            CatalogAuthority::new(base()).with_generated_output(&f.grant),
            policy()
        )
        .is_err()
    );
    assert_eq!(f.count("items"), 2);
    assert_eq!(f.count("catalog_versions"), 1);
    f.store
        .db
        .execute_batch("DROP TRIGGER fixture_fail_generated_receipt")
        .unwrap();
    let first = commands::execute_with_authority(
        &f.store,
        &command,
        CatalogAuthority::new(base()).with_generated_output(&f.grant),
        policy(),
    )
    .unwrap();
    assert!(matches!(first, CommandOutcome::Committed { .. }));
    assert_eq!(f.count("items"), 3);
    assert_eq!(f.count("catalog_versions"), 2);
    assert_eq!(f.count("native_execution_lifetimes"), 0);
    assert_eq!(
        f.store
            .db
            .query_row(
                "SELECT current_revision FROM catalog_timelines WHERE id='timeline'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    assert_eq!(
        f.store
            .db
            .query_row(
                "SELECT current_revision FROM catalog_versions WHERE id='original'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    let replay = commands::execute_with_authority(
        &f.store,
        &command,
        CatalogAuthority::new(base()).with_generated_output(&f.grant),
        policy(),
    )
    .unwrap();
    assert!(matches!(replay, CommandOutcome::Replay { .. }));
    assert_eq!(f.count("catalog_versions"), 2);
}
#[test]
fn generated_admission_rejects_changed_proof_heads_and_descriptor() {
    for kind in [
        "ctime",
        "timeline",
        "version",
        "binding",
        "fd",
        "duration",
        "implicit-establishment",
    ] {
        let f = Fixture::new();
        let mut grant = f.grant.clone();
        match kind {
            "ctime" => grant.request.output_ctime_ns += 1,
            "implicit-establishment" => grant.request.establish_provisional = false,
            "duration" => {
                grant.request.output_duration_us = 2_000_000;
                grant.request.native.mutations[0]["versions"][0]["version"]["bindings"][0]["source"]
                    ["duration_us"] = json!(2_000_000);
            }
            "timeline" => grant.request.input_timeline_revision += 1,
            "version" => grant.request.input_version_revision += 1,
            "binding" => grant.request.output_binding["root"] = json!("/wrong"),
            "fd" => {
                grant.output_file =
                    Arc::new(File::open(f.root.path().join("input/media.mkv")).unwrap())
            }
            _ => unreachable!(),
        }
        let command = f.envelope("deny", &grant.request);
        assert!(
            commands::execute_with_authority(
                &f.store,
                &command,
                CatalogAuthority::new(base()).with_generated_output(&grant),
                policy()
            )
            .is_err(),
            "{kind}"
        );
        assert_eq!(f.count("catalog_versions"), 1);
        assert_eq!(f.count("items"), 2);
    }
}

#[test]
fn establishment_refuses_another_current_version_on_the_timeline() {
    let f = Fixture::new();
    f.store.transaction(|db|{db.execute("INSERT INTO items VALUES('other-item','video','{}')",[])?;db.execute("INSERT INTO item_files(id,file_id,item_id,role,origin,part) VALUES('other-association','output','other-item','primary','explicit',1)",[])?;Ok(())}).unwrap();
    let interval = json!({"start_us":0,"end_us":1_000_000});
    let mutation = json!({"contract_version":1,"profile":"default","context":{"id":"context","authority_namespace":"generated-test","context_key":"work","expected_revision":1},"timelines":[],"versions":[{"expected_revision":null,"expected_timeline_revision":1,"version":{"contract_version":1,"version_id":"other-version","association_item_id":"other-item","edition_item_id":"edition","timeline_id":"timeline","state":"ready","declared_coverage":interval,"bindings":[{"binding_id":"other-binding","association_id":"other-association","file":f.grant.request.output.exact,"part":1,"source":{"kind":"whole_file","duration_us":1_000_000},"timeline_interval":interval}]},"equivalence":null,"expected_binding_revisions":{"other-binding":null}}],"evidence":[]});
    let command = CommandEnvelope::new(
        1,
        CommandKey::new(&f.store.database_id, "generated-test", "other-version").unwrap(),
        f.store.catalog_incarnation.as_deref().unwrap(),
        f.generation(),
        &json!({"operation":"identity","mutation":mutation}).to_string(),
    )
    .unwrap();
    let command_authority = TrustedCommandAuthority {
        host_namespace: "generated-test",
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: true,
        identity_authority: f.grant.typed.identity.as_ref(),
    };
    commands::execute_with_authority(
        &f.store,
        &command,
        CatalogAuthority::new(command_authority),
        policy(),
    )
    .unwrap();
    let generated = f.envelope("refuse-establishment", &f.grant.request);
    let error = commands::execute_with_authority(
        &f.store,
        &generated,
        CatalogAuthority::new(base()).with_generated_output(&f.grant),
        policy(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("sole exact measured original"),
        "{error}"
    );
    assert_eq!(f.count("catalog_versions"), 2);
    assert_eq!(
        f.store
            .db
            .query_row(
                "SELECT current_revision FROM catalog_timelines WHERE id='timeline'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn generated_input_capture_is_exact_fenced_and_existing_only() {
    use catabolic_store::{
        catalog_generated_output::capture_input, catalog_identity_mutation::TimelineState,
        catalog_projection::TrustedProjectionAuthority,
    };
    let f = Fixture::new();
    let authority = TrustedProjectionAuthority {
        catalog_id: f.store.database_id.clone(),
        incarnation: f.store.catalog_incarnation.clone().unwrap(),
        expected_generation: f.generation(),
    };
    let (before, grants) = capture_input(
        &f.store,
        &authority,
        "generated-test",
        "default",
        "original",
    )
    .unwrap();
    assert_eq!(before.generation, authority.expected_generation);
    assert_eq!(before.version_revision, 1);
    assert_eq!(before.timeline.expected_revision, Some(1));
    assert_eq!(before.timeline.identity_state, TimelineState::Provisional);
    assert_eq!(before.original.version.version_id, "original");
    assert_eq!(before.binding_revisions.len(), 1);
    assert_eq!(grants.existing_items.len(), 1);
    assert_eq!(grants.existing_contexts.len(), 1);
    assert_eq!(grants.existing_timelines.len(), 1);
    assert!(
        grants.identity.is_none()
            && grants.relationships.is_none()
            && grants.physical_files.is_empty()
            && grants.metadata.is_empty()
            && grants.pins.is_none()
            && grants.tag_sources.is_empty()
            && grants.logical_aliases.is_none()
    );
    for field in ["catalog", "incarnation", "generation"] {
        let mut wrong = authority.clone();
        match field {
            "catalog" => wrong.catalog_id.push('x'),
            "incarnation" => wrong.incarnation.push('x'),
            _ => wrong.expected_generation += 1,
        }
        assert!(capture_input(&f.store, &wrong, "generated-test", "default", "original").is_err());
    }
    assert!(capture_input(&f.store, &authority, "generated-test", "default", "missing").is_err());
    assert!(
        capture_input(
            &f.store,
            &authority,
            "generated-test",
            "other-profile",
            "original"
        )
        .is_err()
    );
    let command = f.envelope("capture-transition", &f.grant.request);
    commands::execute_with_authority(
        &f.store,
        &command,
        CatalogAuthority::new(base()).with_generated_output(&f.grant),
        policy(),
    )
    .unwrap();
    assert!(
        capture_input(
            &f.store,
            &authority,
            "generated-test",
            "default",
            "original"
        )
        .is_err()
    );
    let current = TrustedProjectionAuthority {
        expected_generation: f.generation(),
        ..authority
    };
    let (after, grants) =
        capture_input(&f.store, &current, "generated-test", "default", "original").unwrap();
    let historical = catabolic_store::catalog_generated_output::historical_version(
        &f.store,
        &current,
        "default",
        "original",
        &before.original.version.association_item_id,
        1,
    )
    .unwrap();
    assert_eq!(historical["version"]["current_revision"], 2);
    assert_eq!(historical["version_snapshot"]["revision"], 1);
    assert_eq!(historical["timeline_snapshot"]["revision"], 1);
    assert_eq!(
        historical["timeline_snapshot"]["identity_state"],
        "provisional"
    );
    assert_eq!(historical["retired"], false);
    assert_eq!(historical["playable"], false);
    assert_eq!(after.version_revision, 2);
    assert_eq!(after.timeline.expected_revision, Some(2));
    assert_eq!(after.timeline.identity_state, TimelineState::Established);
    assert_eq!(
        after.context.expected_revision.unwrap(),
        before.context.expected_revision.unwrap() + 1
    );
    assert!(after.original.equivalence.is_some());
    assert_eq!(
        grants.existing_timelines[&after.timeline.timeline_id].revision,
        2
    );
    assert!(grants.identity.is_none());
}

fn selected_fixture() -> Fixture {
    let f = Fixture::with_motion_types(true);
    let command = f.envelope("selected-output-publish", &f.grant.request);
    assert_eq!(
        commands::execute_with_authority(
            &f.store,
            &command,
            CatalogAuthority::new(base()).with_generated_output(&f.grant),
            policy()
        )
        .unwrap()
        .code(),
        "committed"
    );
    f
}
fn selected_read(f: &Fixture, original: bool) -> catabolic_store::Result<Value> {
    use catabolic_store::catalog_projection::{
        self, TrustedProjectionAuthority, VersionLineageRequest,
    };
    catalog_projection::selected_current_versions(
        &f.store,
        &TrustedProjectionAuthority {
            catalog_id: f.store.database_id.clone(),
            incarnation: f.store.catalog_incarnation.clone().unwrap(),
            expected_generation: f.generation(),
        },
        "default",
        &[VersionLineageRequest {
            work_item_id: "work".into(),
            edition_item_id: "edition".into(),
            version_item_id: if original {
                "original-item"
            } else {
                "generated-item"
            }
            .into(),
            version_id: if original {
                "original"
            } else {
                "generated-version"
            }
            .into(),
        }],
    )
}
#[test]
fn selected_current_generated_output_survives_offline_unselected_original_without_relaxing_full_manifest()
 {
    let f = selected_fixture();
    let present = selected_read(&f, false).unwrap();
    assert_eq!(
        present["selection"]["selected_versions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        present["selection"]["complete_context"]["structural_versions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        present["selection"]["all_context_occurrences_current"],
        false
    );
    fs::remove_file(f.root.path().join("input/media.mkv")).unwrap();
    catabolic_store::observation::execute(&f.store, "default", "scan", &json!({"source":"input"}))
        .unwrap();
    assert!(
        catabolic_store::catalog_identity_mutation::load_manifest(
            &f.store,
            "default",
            &["generated-version".into()]
        )
        .is_err(),
        "strict complete manifest must remain strict"
    );
    assert!(
        selected_read(&f, true).is_err(),
        "missing requested original is not blessed"
    );
    let offline = selected_read(&f, false).unwrap();
    assert_eq!(
        offline["selection"]["selected_versions"],
        present["selection"]["selected_versions"]
    );
    assert_eq!(
        offline["selection"]["current_occurrences"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        offline["selection"]["current_occurrences"][0]["file"]["file_id"],
        "output"
    );
    fs::write(f.root.path().join("output/media.mkv"), b"changed-output").unwrap();
    assert!(
        selected_read(&f, false).is_err(),
        "changed requested output cannot reuse saved exact output proof"
    );
}
#[test]
fn selected_current_output_rechecks_ctime_root_and_complete_structural_lineage() {
    use std::os::unix::fs::PermissionsExt;
    let f = selected_fixture();
    selected_read(&f, false).unwrap();
    let path = f.root.path().join("output/media.mkv");
    let before = path.metadata().unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(before.mode() ^ 0o040)).unwrap();
    assert!(
        selected_read(&f, false).is_err(),
        "ctime-only output change must fail even without a content revision update"
    );
    let f = selected_fixture();
    let output = f.root.path().join("output");
    fs::rename(&output, f.root.path().join("old-output")).unwrap();
    fs::create_dir(&output).unwrap();
    fs::copy(
        f.root.path().join("old-output/media.mkv"),
        output.join("media.mkv"),
    )
    .unwrap();
    assert!(
        selected_read(&f, false).is_err(),
        "rebound root is not current output authority"
    );
    let f = selected_fixture();
    catabolic_store::curation::item_put(
        &f.store,
        &json!({"id":"other-work","kind":"video","metadata":{}}),
    )
    .unwrap();
    catabolic_store::curation::relate(
        &f.store,
        &json!({"source":"other-work","target":"edition","kind":"custom:motion_edition"}),
    )
    .unwrap();
    assert!(
        selected_read(&f, false).is_err(),
        "competing active ownership cannot be reduced to a requested subset"
    );
}

#[test]
fn selected_output_retains_context_evidence_rules_and_rejects_stale_snapshot() {
    use catabolic_store::{
        catalog_identity_mutation::{ContextChange, IdentityMutation, VersionChange},
        catalog_projection::{self, TrustedProjectionAuthority, VersionLineageRequest},
    };
    use std::collections::BTreeMap;
    let f = selected_fixture();
    let before = selected_read(&f, false).unwrap();
    let original = before["selection"]["complete_context"]["structural_versions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["version"]["version_id"] == "original")
        .unwrap();
    let version: catabolic_core::catalog_identity::VersionContract =
        serde_json::from_value(original["version"].clone()).unwrap();
    let expected_binding_revisions:BTreeMap<String,Option<i64>>=f.store.db.prepare("SELECT binding_id,binding_revision FROM catalog_version_bindings WHERE version_id='original' AND version_revision=2").unwrap().query_map([],|r|Ok((r.get(0)?,Some(r.get(1)?)))).unwrap().collect::<Result<_,_>>().unwrap();
    let context_head: i64 = f
        .store
        .db
        .query_row(
            "SELECT current_revision FROM catalog_segmentation_contexts WHERE id='context'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    // A real nonselected-member mutation attempting to drop required evidence
    // must fail atomically. Selection cannot make its context rule disappear.
    let mutation = IdentityMutation {
        contract_version: 1,
        profile: "default".into(),
        context: ContextChange {
            id: "context".into(),
            authority_namespace: "generated-test".into(),
            context_key: "work".into(),
            expected_revision: Some(context_head),
        },
        timelines: vec![],
        versions: vec![VersionChange {
            version,
            expected_revision: Some(2),
            expected_timeline_revision: 2,
            equivalence: None,
            expected_binding_revisions,
        }],
        evidence: vec![],
    };
    let command = CommandEnvelope::new(
        1,
        CommandKey::new(
            &f.store.database_id,
            "generated-test",
            "drop-unselected-evidence",
        )
        .unwrap(),
        f.store.catalog_incarnation.as_deref().unwrap(),
        f.generation(),
        &json!({"operation":"identity","mutation":mutation}).to_string(),
    )
    .unwrap();
    let identity = IdentityAuthority {
        authority_namespace: "generated-test".into(),
        context_keys: ["work".into()].into(),
        allow_declared_evidence: true,
        allow_verified_evidence: false,
    };
    assert!(
        commands::execute(
            &f.store,
            &command,
            TrustedCommandAuthority {
                identity_authority: Some(&identity),
                ..base()
            },
            policy()
        )
        .is_err()
    );
    assert_eq!(
        selected_read(&f, false).unwrap(),
        before,
        "rejected context mutation changes no selected proof"
    );
    let old = TrustedProjectionAuthority {
        catalog_id: f.store.database_id.clone(),
        incarnation: f.store.catalog_incarnation.clone().unwrap(),
        expected_generation: f.generation(),
    };
    let mut changes = Vec::new();
    let mut claims = Vec::new();
    for entry in before["selection"]["complete_context"]["structural_versions"]
        .as_array()
        .unwrap()
    {
        let version = &entry["version"];
        let id = version["version_id"].as_str().unwrap();
        let head = before["selection"]["complete_context"]["version_heads"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["version_id"] == id)
            .unwrap()["version_revision"]
            .as_i64()
            .unwrap();
        let bindings:BTreeMap<String,Option<i64>>=f.store.db.prepare("SELECT binding_id,binding_revision FROM catalog_version_bindings WHERE version_id=? AND version_revision=?").unwrap().query_map(rusqlite::params![id,head],|r|Ok((r.get(0)?,Some(r.get(1)?)))).unwrap().collect::<Result<_,_>>().unwrap();
        changes.push(json!({"version":version,"expected_revision":head,"expected_timeline_revision":3,"expected_binding_revisions":bindings,"equivalence":{"basis":"declared","evidence_id":"renewed-context-evidence"}}));
        claims.push(json!({"edition_item_id":"edition","timeline_id":"timeline","timeline_revision":3,"version_id":id,"version_revision":head+1,"exact_files":version["bindings"].as_array().unwrap().iter().map(|b|b["file"].clone()).collect::<Vec<_>>()}));
    }
    let renewed = json!({"contract_version":1,"profile":"default","context":{"id":"context","authority_namespace":"generated-test","context_key":"work","expected_revision":context_head},"timelines":[{"timeline_id":"timeline","edition_item_id":"edition","expected_revision":2,"identity_state":"established","coverage":{"start_us":0,"end_us":1_000_000}}],"versions":changes,"evidence":[{"id":"renewed-context-evidence","basis":"declared","claims":claims,"evidence":{"fixture":"explicit complete-context reassertion"}}]});
    let command = CommandEnvelope::new(
        1,
        CommandKey::new(&f.store.database_id, "generated-test", "renew-context-head").unwrap(),
        f.store.catalog_incarnation.as_deref().unwrap(),
        f.generation(),
        &json!({"operation":"identity","mutation":renewed}).to_string(),
    )
    .unwrap();
    assert_eq!(
        commands::execute(
            &f.store,
            &command,
            TrustedCommandAuthority {
                identity_authority: Some(&identity),
                ..base()
            },
            policy()
        )
        .unwrap()
        .code(),
        "committed"
    );
    let renewed = selected_read(&f, false).unwrap();
    assert!(
        renewed["selection"]["complete_context"]["version_heads"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["timeline_revision"] == 3
                && h["equivalence_evidence_id"] == "renewed-context-evidence")
    );
    assert_ne!(
        renewed["selection"]["selected_versions"], before["selection"]["selected_versions"],
        "current evidence must replace the old selected snapshot"
    );
    assert!(
        catalog_projection::selected_current_versions(
            &f.store,
            &old,
            "default",
            &[VersionLineageRequest {
                work_item_id: "work".into(),
                edition_item_id: "edition".into(),
                version_item_id: "generated-item".into(),
                version_id: "generated-version".into()
            }]
        )
        .is_err()
    );
}
