// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! V2 association translation and full-map qualification, through existing owners.
//! Physical rows are disposable, quiescent fixture observations with real stat
//! values. Declared durations/equivalence are fixture input, not a media probe,
//! scan-cleanup witness, availability claim, or publication qualification.
use super::*;
use catabolic_store::catalog_import_typed::{
    ImportedAssociation, NativeIdentityBatch, TypedAuthority, TypedBatch,
    capture_existing_occurrence,
};
use std::collections::{BTreeMap, BTreeSet};

const PART: i64 = 10_000_000;
const FILES: [&str; 6] = [
    "physical",
    "duplicate",
    "alt-one",
    "alt-two",
    "cut",
    "unknown",
];

fn representations(f: &Fixture, split: bool) -> (Vec<ImportBatch>, TypedAuthority) {
    use std::os::unix::fs::MetadataExt;
    seed_physical_snapshot(f);
    for file in &FILES[2..] {
        let path = f._root.path().join("source").join(format!("{file}.mkv"));
        std::fs::write(&path, b"same-content-native-import-fixture").unwrap();
        let m = path.metadata().unwrap();
        f.store.transaction(|db| {
            db.execute("INSERT INTO files(id,location,path) VALUES(?1,'media',?2)", rusqlite::params![file,format!("{file}.mkv")])?;
            db.execute("INSERT INTO observations VALUES('default',?1,?2,?3,?4,?5,'present','fixture-scan')", rusqlite::params![file,m.len() as i64,m.mtime()*1_000_000_000+m.mtime_nsec(),m.dev() as i64,m.ino() as i64])?;
            Ok(())
        }).unwrap();
    }
    let grants = TypedAuthority {
        physical_files: [(
            "default".into(),
            FILES.into_iter().map(str::to_owned).collect(),
        )]
        .into(),
        identity: Some(
            catabolic_store::catalog_identity_mutation::IdentityAuthority {
                authority_namespace: "import-host".into(),
                context_keys: ["representations".into()].into(),
                allow_declared_evidence: true,
                allow_verified_evidence: false,
            },
        ),
        ..Default::default()
    };
    let occurrences: Vec<_> = FILES
        .into_iter()
        .map(|file| {
            capture_existing_occurrence(
                &f.store,
                &format!("source-{file}"),
                "default",
                file,
                &grants,
            )
            .unwrap()
        })
        .collect();
    let mut associations = vec![];
    let mut versions = vec![];
    for (name, timeline, parts, coverage) in [
        ("a", "theatrical", vec![0, 1], 2 * PART),
        ("b", "theatrical", vec![2, 3], 2 * PART),
        ("cut", "directors-cut", vec![4], PART + PART / 2),
        ("unknown", "unknown-encode", vec![5], 2 * PART),
    ] {
        let mut bindings = vec![];
        let mut revisions = BTreeMap::<String, Option<i64>>::new();
        for (index, physical) in parts.iter().enumerate() {
            let part = index as u32 + 1;
            let association = format!("source-association-{name}-{part}");
            let binding = format!("binding-{name}-{part}");
            let duration = if parts.len() == 1 { coverage } else { PART };
            associations.push(ImportedAssociation {
                source_association_id: association.clone(),
                source_occurrence_id: occurrences[*physical].source_occurrence_id.clone(),
                item_id: format!("version-item-{name}"),
                part,
            });
            bindings.push(json!({"binding_id":binding,"association_id":association,
                "file":occurrences[*physical].exact,"part":part,
                "source":{"kind":"whole_file","duration_us":duration},
                "timeline_interval":{"start_us":index as i64*PART,"end_us":index as i64*PART+duration}}));
            revisions.insert(binding, None);
        }
        let equivalence = if name == "a" || name == "b" {
            json!({"basis":"declared","evidence_id":"declared-alternatives"})
        } else {
            Value::Null
        };
        versions.push(
            json!({"expected_revision":null,"expected_timeline_revision":1,
            "version":{"contract_version":1,"version_id":format!("version-{name}"),
                "association_item_id":format!("version-item-{name}"),"edition_item_id":"edition",
                "timeline_id":format!("timeline-{timeline}"),"state":"ready",
                "declared_coverage":{"start_us":0,"end_us":coverage},"bindings":bindings},
            "equivalence":equivalence,"expected_binding_revisions":revisions}),
        );
    }
    let claims: Vec<_> = versions[..2].iter().map(|v| json!({
        "edition_item_id":"edition","timeline_id":"timeline-theatrical","timeline_revision":1,
        "version_id":v["version"]["version_id"],"version_revision":1,
        "exact_files":v["version"]["bindings"].as_array().unwrap().iter().map(|b|b["file"].clone()).collect::<Vec<_>>()
    })).collect();
    let timelines: Vec<_> = [("theatrical","established",2*PART),("directors-cut","provisional",PART+PART/2),("unknown-encode","provisional",2*PART)]
        .into_iter().map(|(name,state,duration)|json!({"timeline_id":format!("timeline-{name}"),"edition_item_id":"edition","expected_revision":null,
            "identity_state":state,"coverage":{"start_us":0,"end_us":duration}})).collect();
    let mutation = json!({"contract_version":1,"profile":"default",
        "context":{"id":"representations-context","authority_namespace":"import-host","context_key":"representations","expected_revision":null},
        "timelines":timelines,"versions":versions,
        "evidence":[{"id":"declared-alternatives","basis":"declared","claims":claims,
            "evidence":{"source":"explicit fixture declaration","scope":"theatrical alternatives only"}}]});
    let mut imported = batch("edition");
    imported
        .items
        .extend(["a", "b", "cut", "unknown"].into_iter().map(|name| {
            let mut item = item(&format!("version-item-{name}"));
            item.legacy_metadata = json!({"title":"Same apparent title"});
            item
        }));
    if split {
        // Physical maps and source association IDs are in an earlier batch;
        // the owner mutation consumes them only after the complete snapshot loads.
        imported.typed = Some(TypedBatch {
            native: Some(NativeIdentityBatch {
                occurrences,
                associations,
                mutations: vec![],
            }),
            ..Default::default()
        });
        (
            vec![
                imported,
                ImportBatch {
                    items: vec![],
                    unresolved_evidence: vec![],
                    typed: Some(TypedBatch {
                        native: Some(NativeIdentityBatch {
                            mutations: vec![mutation],
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                },
            ],
            grants,
        )
    } else {
        imported.typed = Some(TypedBatch {
            native: Some(NativeIdentityBatch {
                occurrences,
                associations,
                mutations: vec![mutation],
            }),
            ..Default::default()
        });
        (vec![imported], grants)
    }
}

fn owner_counts(store: &Store) -> Vec<i64> {
    [
        "items",
        "item_files",
        "catalog_segmentation_contexts",
        "catalog_segmentation_revisions",
        "catalog_timelines",
        "catalog_timeline_revisions",
        "catalog_versions",
        "catalog_version_revisions",
        "catalog_bindings",
        "catalog_binding_revisions",
        "catalog_version_bindings",
        "catalog_identity_evidence",
        "catalog_mutation_permits",
    ]
    .iter()
    .map(|table| {
        store
            .db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    })
    .collect()
}
fn assert_invisible(f: &Fixture, before: i64, receipts: i64) {
    assert_eq!(owner_counts(&f.store), vec![0; 13]);
    assert_eq!(generation(&f.store), before);
    assert_eq!(count(&f.store, "catalog_command_receipts"), receipts);
    assert_eq!(count(&f.store, "catalog_receipt_holds"), receipts);
    assert_eq!(count(&f.store, "files"), 6);
    if f.store.schema_version >= 35 {
        assert_eq!(count(&f.store, "catalog_search_documents"), 0);
    }
    assert!(f.store.db.is_autocommit());
}
fn full_digest(logical: &Value, typed: &Value) -> String {
    catabolic_core::sha256(
        catabolic_core::encode_with(
            &json!({"contract_version":2,"mappings":logical,"typed_mappings":typed}),
            false,
            true,
        )
        .as_bytes(),
    )
}
fn assert_full_map(validated: &Value) {
    let logical = &validated["proposed_mappings"];
    let typed = &validated["proposed_typed_mappings"];
    let mappings = typed.as_array().unwrap();
    assert_eq!(logical.as_array().unwrap().len(), 5);
    assert_eq!(mappings.len(), 13);
    assert_eq!(full_digest(logical, typed), validated["mapping_digest"]);
    let association: BTreeMap<_, _> = mappings
        .iter()
        .filter(|r| r["owner"] == "association")
        .map(|r| (r["source_association_id"].as_str().unwrap(), r))
        .collect();
    assert_eq!(association.len(), 6);
    let ids: BTreeSet<_> = association
        .values()
        .map(|a| a["association_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 6);
    for (source, mapped) in &association {
        assert_ne!(*source, mapped["association_id"].as_str().unwrap());
    }
    let identity = mappings.iter().find(|r| r["owner"] == "identity").unwrap();
    assert_eq!(identity["result"]["context_revision"], 1);
    let native_versions = identity["result"]["versions"].as_array().unwrap();
    assert_eq!(native_versions.len(), 4);
    for version in native_versions {
        assert_eq!(version["revision"], 1);
        assert_eq!(version["timeline_revision"], 1);
        for binding in version["bindings"].as_array().unwrap() {
            let name = version["version_id"]
                .as_str()
                .unwrap()
                .strip_prefix("version-")
                .unwrap();
            let source = format!("source-association-{name}-{}", binding["part"]);
            let mapped = association[source.as_str()];
            assert_eq!(binding["association_id"], mapped["association_id"]);
            assert_eq!(
                binding["original_association"]["id"],
                mapped["association_id"]
            );
            assert_eq!(binding["file"]["file_id"], mapped["file_id"]);
            assert_eq!(binding["file"]["revision"], mapped["file_revision"]);
            assert_eq!(binding["binding_revision"], 1);
        }
    }
    // Every owner family changes the full digest; a logical-only digest or an
    // edited association/evidence/placement result cannot acknowledge this map.
    for owner in ["occurrence", "association", "identity"] {
        let mut altered = typed.clone();
        let row = altered
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|r| r["owner"] == owner)
            .unwrap();
        match owner {
            "occurrence" => row["file_revision"] = json!("different-exact-revision"),
            "association" => row["association_id"] = json!("wrong-destination-association"),
            _ => {
                row["result"]["versions"][0]["equivalence"]["evidence_id"] =
                    json!("unreviewed-evidence")
            }
        }
        assert_ne!(full_digest(logical, &altered), validated["mapping_digest"]);
    }
}
fn assert_persisted(f: &Fixture) {
    assert_eq!(
        owner_counts(&f.store),
        vec![5, 6, 1, 1, 3, 3, 4, 4, 6, 6, 6, 1, 0]
    );
    let manifest = catabolic_store::catalog_identity_mutation::load_manifest(
        &f.store,
        "default",
        &["version-a".into()],
    )
    .unwrap();
    assert_eq!(manifest.versions.len(), 4);
    let versions: BTreeMap<_, _> = manifest
        .versions
        .iter()
        .map(|v| (v.version.version_id.as_str(), v))
        .collect();
    let a = versions["version-a"];
    let b = versions["version-b"];
    assert_eq!(a.version.timeline_id, b.version.timeline_id);
    assert_ne!(a.version.association_item_id, b.version.association_item_id);
    for alternative in [a, b] {
        assert_eq!(
            alternative
                .version
                .bindings
                .iter()
                .map(|b| b.part)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            alternative.equivalence.as_ref().unwrap().evidence_id,
            "declared-alternatives"
        );
    }
    for name in ["version-cut", "version-unknown"] {
        let version = versions[name];
        assert_ne!(version.version.timeline_id, a.version.timeline_id);
        assert!(version.equivalence.is_none());
    }
    assert_ne!(
        versions["version-cut"].version.timeline_id,
        versions["version-unknown"].version.timeline_id
    );
    assert_eq!(
        versions["version-unknown"].version.declared_coverage,
        a.version.declared_coverage
    );
    assert_ne!(
        versions["version-cut"].version.declared_coverage,
        a.version.declared_coverage
    );
    let evidence: String = f
        .store
        .db
        .query_row(
            "SELECT evidence_json FROM catalog_identity_evidence WHERE id='declared-alternatives'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let evidence: Value = serde_json::from_str(&evidence).unwrap();
    assert_eq!(evidence["claims"].as_array().unwrap().len(), 2);
    for claim in evidence["claims"].as_array().unwrap() {
        let v = versions[claim["version_id"].as_str().unwrap()];
        assert_eq!(
            claim["exact_files"],
            serde_json::to_value(
                v.version
                    .bindings
                    .iter()
                    .map(|b| &b.file)
                    .collect::<Vec<_>>()
            )
            .unwrap()
        );
    }
    // Exercise the existing whole-version decision with the imported manifests.
    // These explicit availability inputs do not assert filesystem availability.
    use catabolic_core::catalog_identity::{VersionCandidate, select_complete_version};
    let a_partial = vec![a.version.bindings[0].binding_id.clone()];
    let b_partial = vec![b.version.bindings[1].binding_id.clone()];
    let b_full = b
        .version
        .bindings
        .iter()
        .map(|b| b.binding_id.clone())
        .collect::<Vec<_>>();
    let choose = |available: &[String]| {
        select_complete_version(
            "edition",
            "timeline-theatrical",
            a.version.declared_coverage.unwrap(),
            &[
                VersionCandidate {
                    version: &a.version,
                    equivalence: a.equivalence.as_ref(),
                    available_binding_ids: &a_partial,
                },
                VersionCandidate {
                    version: &b.version,
                    equivalence: b.equivalence.as_ref(),
                    available_binding_ids: available,
                },
            ],
        )
        .unwrap()
    };
    assert_eq!(choose(&b_partial), None);
    assert_eq!(choose(&b_full), Some(1));
    let unknown = versions["version-unknown"];
    let unknown_available = unknown
        .version
        .bindings
        .iter()
        .map(|b| b.binding_id.clone())
        .collect::<Vec<_>>();
    assert!(
        catabolic_core::catalog_identity::select_explicit_version(
            "version-unknown",
            &unknown.version,
            &unknown_available
        )
        .unwrap()
    );
    assert_eq!(
        select_complete_version(
            "edition",
            "timeline-theatrical",
            a.version.declared_coverage.unwrap(),
            &[VersionCandidate {
                version: &unknown.version,
                equivalence: unknown.equivalence.as_ref(),
                available_binding_ids: &unknown_available
            },]
        ),
        Err(catabolic_core::catalog_identity::IdentityError::VersionScopeMismatch)
    );
}

#[test]
fn multipart_cuts_and_equivalence_translate_full_maps_rehearse_activate_and_replay() {
    for schema in [34, 35, 36, 37] {
        for split in [false, true] {
            let f = Fixture::at(schema);
            let (batches, grants) = representations(&f, split);
            let before = generation(&f.store);
            let digest = stage_typed_batches(&f, &batches);
            assert_invisible(&f, before, batches.len() as i64 + 1);
            let validated = run_typed(&f, &validate(&digest), &grants, None).unwrap();
            assert_full_map(result(&validated));
            assert_invisible(&f, before, batches.len() as i64 + 2);
            let map = result(&validated)["mapping_digest"].as_str().unwrap();
            let active = activation_grant(&f.store, &digest, map);
            let mutation = activate(&digest, map);
            let intent = f.envelope(&mutation);
            let committed = run_typed(&f, &mutation, &grants, Some(&active)).unwrap();
            assert_eq!(
                result(&committed)["typed_results"],
                result(&validated)["proposed_typed_mappings"]
            );
            assert_eq!(result(&committed)["mapping_digest"], map);
            assert_persisted(&f);
            let counts = owner_counts(&f.store);
            let after = generation(&f.store);
            let Fixture { _root, path, store } = f;
            drop(store);
            let store = Store::open(&path, true, false).unwrap();
            let replay = commands::execute(
                &store,
                &intent,
                TrustedCommandAuthority {
                    allow_new_commands: false,
                    ..base()
                },
                policy(),
            )
            .unwrap();
            assert_eq!(replay.code(), "replay");
            assert_eq!(result(&replay), result(&committed));
            assert_eq!(owner_counts(&store), counts);
            assert_eq!(generation(&store), after);
            assert_eq!(
                count(&store, "catalog_command_receipts"),
                batches.len() as i64 + 3
            );
            assert_eq!(
                count(&store, "catalog_receipt_holds"),
                batches.len() as i64 + 3
            );
        }
    }
}

#[test]
fn multipart_mapping_and_equivalence_refusals_leave_no_rehearsal_visibility() {
    for schema in [34, 35, 36, 37] {
        for failure in [
            "missing-evidence",
            "stale-evidence",
            "wrong-association",
            "undeclared-authority",
        ] {
            let f = Fixture::at(schema);
            let (mut batches, mut grants) = representations(&f, false);
            let native = batches[0].typed.as_mut().unwrap().native.as_mut().unwrap();
            match failure {
                "missing-evidence" => native.mutations[0]["evidence"] = json!([]),
                "stale-evidence" => {
                    native.mutations[0]["evidence"][0]["claims"][0]["version_revision"] = json!(2)
                }
                "wrong-association" => {
                    native.mutations[0]["versions"][0]["version"]["bindings"][0]["association_id"] =
                        json!("source-association-b-1")
                }
                _ => grants.identity.as_mut().unwrap().allow_declared_evidence = false,
            }
            let digest = stage_typed_batches(&f, &batches);
            let before = generation(&f.store);
            let error = run_typed(&f, &validate(&digest), &grants, None).unwrap_err();
            assert_eq!(
                error.code,
                CommandStoreErrorCode::Import(if failure == "undeclared-authority" {
                    ImportErrorCode::AuthorityDenied
                } else {
                    ImportErrorCode::SnapshotConflict
                }),
                "{failure}: {error}"
            );
            assert_invisible(&f, before, 2);
        }
    }
}

#[test]
fn multipart_full_mapping_acknowledgment_rejects_logical_only_or_edited_typed_maps() {
    for schema in [34, 35, 36, 37] {
        let f = Fixture::at(schema);
        let (batches, grants) = representations(&f, true);
        let digest = stage_typed_batches(&f, &batches);
        let validated = run_typed(&f, &validate(&digest), &grants, None).unwrap();
        let body = result(&validated);
        let before = generation(&f.store);
        let logical: Vec<import::LogicalMapping> =
            serde_json::from_value(body["proposed_mappings"].clone()).unwrap();
        let mut altered = body["proposed_typed_mappings"].clone();
        altered[6]["association_id"] = json!("wrong-owner-generated-id");
        for wrong in [
            import::mapping_digest(&logical).unwrap(),
            full_digest(&body["proposed_mappings"], &altered),
        ] {
            assert_ne!(wrong, body["mapping_digest"]);
            let active = activation_grant(&f.store, &digest, &wrong);
            assert_eq!(
                run_typed(&f, &activate(&digest, &wrong), &grants, Some(&active))
                    .unwrap_err()
                    .code,
                CommandStoreErrorCode::Import(ImportErrorCode::SnapshotConflict)
            );
            assert_invisible(&f, before, 4);
        }
        let map = body["mapping_digest"].as_str().unwrap();
        let active = activation_grant(&f.store, &digest, map);
        assert_eq!(
            run_typed(&f, &activate(&digest, map), &grants, Some(&active))
                .unwrap()
                .code(),
            "committed"
        );
        assert_persisted(&f);
    }
}

#[test]
fn multipart_replacement_after_validation_refuses_activation_without_partial_identity() {
    for schema in [34, 35, 36, 37] {
        let f = Fixture::at(schema);
        let (batches, grants) = representations(&f, false);
        let digest = stage_typed_batches(&f, &batches);
        let validated = run_typed(&f, &validate(&digest), &grants, None).unwrap();
        let map = result(&validated)["mapping_digest"].as_str().unwrap();
        f.store
            .transaction(|db| {
                db.execute(
                    "UPDATE observations SET size=size+1 WHERE file_id='alt-two'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let before = generation(&f.store);
        let active = activation_grant(&f.store, &digest, map);
        let error = run_typed(&f, &activate(&digest, map), &grants, Some(&active)).unwrap_err();
        assert!(
            matches!(
                error.code,
                CommandStoreErrorCode::Import(
                    ImportErrorCode::StaleValidation | ImportErrorCode::SnapshotConflict
                )
            ),
            "{error}"
        );
        assert_invisible(&f, before, 3);
    }
}

#[test]
fn multipart_late_receipt_failure_rolls_back_all_shapes_then_same_intent_retries() {
    for schema in [34, 35, 36, 37] {
        for split in [false, true] {
            let f = Fixture::at(schema);
            let (batches, grants) = representations(&f, split);
            let digest = stage_typed_batches(&f, &batches);
            let validated = run_typed(&f, &validate(&digest), &grants, None).unwrap();
            let map = result(&validated)["mapping_digest"].as_str().unwrap();
            let before = generation(&f.store);
            let active = activation_grant(&f.store, &digest, map);
            f.store.db.execute_batch("CREATE TEMP TRIGGER fail_representations_receipt BEFORE INSERT ON catalog_command_receipts WHEN NEW.operation_id LIKE '%/activate' BEGIN SELECT CASE WHEN (SELECT count(*) FROM catalog_bindings)=6 AND (SELECT count(*) FROM catalog_versions)=4 AND (SELECT count(*) FROM catalog_identity_evidence)=1 THEN RAISE(ABORT,'late-representations-qualified') ELSE RAISE(ABORT,'early-unqualified-failure') END; END;").unwrap();
            let error = run_typed(&f, &activate(&digest, map), &grants, Some(&active)).unwrap_err();
            assert!(
                error.to_string().contains("late-representations-qualified"),
                "{error}"
            );
            assert_invisible(&f, before, batches.len() as i64 + 2);
            f.store
                .db
                .execute_batch("DROP TRIGGER fail_representations_receipt")
                .unwrap();
            let committed = run_typed(&f, &activate(&digest, map), &grants, Some(&active)).unwrap();
            assert_eq!(
                result(&committed)["typed_results"],
                result(&validated)["proposed_typed_mappings"]
            );
            assert_persisted(&f);
        }
    }
}
