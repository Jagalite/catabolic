// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Independent regressions for R1's manifest and media-effect boundaries.

use catabolic_core::{catalog_identity::*, component, encode};
use catabolic_store::{
    Store,
    catalog_identity::{self, CatalogManifest, ManifestVersion},
    component_mutation, curation, fallback_resolution, media_mutation, migration,
    public_components,
};
use rusqlite::params;
use serde_json::{Value, json};
use std::{fs, os::unix::fs::MetadataExt};

fn interval(start: i64, end: i64) -> IntervalUs {
    IntervalUs::new(start, end).unwrap()
}

struct Fixture {
    _root: tempfile::TempDir,
    store: Store,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        let source_meta = source.metadata().unwrap();
        let media = source.join("episodes.mkv");
        fs::write(&media, b"review fixture; recorded probe evidence").unwrap();
        let observed = media.metadata().unwrap();
        let database = root.path().join("catalog.sqlite3");
        migration::initialize(&database).unwrap();
        let store = Store::open(&database, true, false).unwrap();
        store
            .db
            .execute_batch(
                "INSERT INTO locations VALUES ('media');
            INSERT INTO scans(id,profile,location,complete,observed,errors)
            VALUES ('scan','default','media',1,1,'[]');
            INSERT INTO files VALUES ('file','media','episodes.mkv');",
            )
            .unwrap();
        store
            .db
            .execute(
                "INSERT INTO bindings VALUES ('default','source','media',?,?,?)",
                params![
                    source.to_str(),
                    source_meta.dev() as i64,
                    source_meta.ino() as i64
                ],
            )
            .unwrap();
        store
            .db
            .execute(
                "INSERT INTO observations VALUES ('default','file',?,?,?,?,'present','scan')",
                params![
                    observed.len() as i64,
                    observed.mtime() * 1_000_000_000 + observed.mtime_nsec(),
                    observed.dev() as i64,
                    observed.ino() as i64
                ],
            )
            .unwrap();
        for id in ["edition-1", "edition-2", "episode-1", "episode-2"] {
            curation::item_put(&store, &json!({"id":id,"kind":"movie","metadata":{}})).unwrap();
        }
        for item in ["episode-1", "episode-2"] {
            curation::dispatch(
                &store,
                "default",
                "associate",
                &json!({
                    "file_id":"file","item_id":item,"role":"primary","part":1,
                    "metadata":{"representation_group":item}
                }),
            )
            .unwrap();
        }
        let snapshot = media_mutation::snapshot(&store, "default", "file").unwrap();
        let data = json!({"format":{"duration":"120.0","format_name":"matroska"},
            "streams":[{"index":0,"codec_type":"video","codec_name":"h264","duration":"120.0"}]});
        store.transaction(|db| {
            db.execute("INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state,result) VALUES ('probe','default','file','probe','media',?,'{}','probe','complete',?)",
                params![encode(&snapshot), encode(&data)])?;
            db.execute("INSERT INTO file_facts VALUES ('default','file','probe','probe',?,?,'complete')",
                params![encode(&snapshot), encode(&data)])?;
            public_components::call(&store, "default", "index_file", &json!({"file_id":"file"}))?;
            Ok(())
        }).unwrap();
        Self { _root: root, store }
    }

    fn version(&self, number: u32, start_us: i64) -> VersionContract {
        let item = format!("episode-{number}");
        let association_id: String = self
            .store
            .db
            .query_row(
                "SELECT id FROM item_files WHERE item_id=? AND role='primary' AND active=1",
                [&item],
                |r| r.get(0),
            )
            .unwrap();
        let snapshot = media_mutation::snapshot(&self.store, "default", "file").unwrap();
        VersionContract {
            contract_version: 1,
            version_id: format!("version-{number}"),
            association_item_id: item,
            edition_item_id: format!("edition-{number}"),
            timeline_id: format!("timeline-{number}"),
            state: VersionState::Ready,
            declared_coverage: Some(interval(0, 60_000_000)),
            bindings: vec![VersionBinding {
                binding_id: format!("binding-{number}"),
                association_id,
                file: ExactFileRevision {
                    file_id: "file".into(),
                    revision: public_components::revision(&snapshot).unwrap(),
                },
                part: 1,
                source: SourceExtent::Slice {
                    interval: interval(start_us, start_us + 60_000_000),
                },
                timeline_interval: Some(interval(0, 60_000_000)),
            }],
        }
    }

    fn video(&self, item: &str) -> Value {
        let occurrence: String = self.store.db.query_row(&format!(
            "SELECT occurrence_id FROM ({}) WHERE profile='default' AND item_id=? AND kind='video' AND current=1",
            catabolic_store::query::view_sql("catalog_component_occurrences")), [item], |r|r.get(0)).unwrap();
        component_mutation::get(&self.store, "default", &occurrence, true).unwrap()
    }
}

#[test]
fn same_revision_segments_cannot_evade_nonoverlap_with_distinct_set_ids() {
    let fixture = Fixture::new();
    for second_start in [59_000_000, 60_000_000] {
        let versions = [fixture.version(1, 0), fixture.version(2, second_start)];
        let segmentation_sets = versions
            .iter()
            .enumerate()
            .map(|(i, version)| {
                let binding = &version.bindings[0];
                SegmentationSet {
                    contract_version: 1,
                    segmentation_set_id: format!("set-{i}"),
                    file: binding.file.clone(),
                    segments: vec![ActiveSegment {
                        binding_id: binding.binding_id.clone(),
                        file: binding.file.clone(),
                        source_interval: binding.source.interval().unwrap().unwrap(),
                    }],
                }
            })
            .collect();
        let manifest = CatalogManifest {
            contract_version: 1,
            database_id: fixture.store.database_id.clone(),
            expected_epoch: fallback_resolution::epoch(&fixture.store).unwrap(),
            versions: versions
                .into_iter()
                .map(|version| ManifestVersion {
                    version,
                    equivalence: None,
                })
                .collect(),
            segmentation_sets,
        };
        assert_eq!(
            catalog_identity::validate_manifest(&fixture.store, "default", &manifest)
                .unwrap_err()
                .0,
            "catalog_identity_duplicate_segmentation_revision"
        );
    }
}

#[test]
fn manifest_segment_budget_is_aggregate_and_precedes_member_validation() {
    let fixture = Fixture::new();
    let version = fixture.version(1, 0);
    let binding = &version.bindings[0];
    let segment = ActiveSegment {
        binding_id: binding.binding_id.clone(),
        file: binding.file.clone(),
        source_interval: binding.source.interval().unwrap().unwrap(),
    };
    let manifest = CatalogManifest {
        contract_version: 1,
        database_id: fixture.store.database_id.clone(),
        expected_epoch: fallback_resolution::epoch(&fixture.store).unwrap(),
        segmentation_sets: (0..2)
            .map(|index| SegmentationSet {
                contract_version: 1,
                segmentation_set_id: format!("set-{index}"),
                file: binding.file.clone(),
                segments: vec![segment.clone(); 512 + index],
            })
            .collect(),
        versions: vec![ManifestVersion {
            version,
            equivalence: None,
        }],
    };
    assert_eq!(
        catalog_identity::validate_manifest(&fixture.store, "default", &manifest)
            .unwrap_err()
            .0,
        "catalog_identity_manifest_budget"
    );
}

#[test]
fn processing_revalidation_refuses_native_binding_even_when_mux_is_preapproved() {
    let fixture = Fixture::new();
    let version = fixture.version(1, 0);
    let binding = &version.bindings[0];
    let video = fixture.video("episode-1");
    let primary = json!({"id":binding.association_id,"item_id":version.association_item_id,
        "file_id":binding.file.file_id,"part":binding.part,"metadata":{}});
    let compatibility =
        component::compatible_binding(&version, binding, &video, &video, &primary).unwrap();
    let package = json!({"version":1,"components":[{"occurrence":video,"compatibility":compatibility}],
        "dependencies":[],"packaging":{"action":"mux","ready":false,"operation_id":"approved"}});
    let approved = json!({"id":"approved","preset":"component-mux"});
    assert_eq!(
        component_mutation::validate_package(
            &fixture.store,
            "default",
            &package,
            Some(&approved),
            false
        )
        .unwrap_err()
        .0,
        "native_binding_effect_unsupported"
    );
    let mut legacy = package.clone();
    legacy["components"][0]["compatibility"] = json!({"offset_seconds":0.0});
    component_mutation::validate_package(
        &fixture.store,
        "default",
        &legacy,
        Some(&approved),
        false,
    )
    .unwrap();
    for marker in [
        json!({"requires_interval_enforcement":true}),
        json!({"binding_id":"binding-1"}),
        json!({"source_interval":{"start_us":0,"end_us":60_000_000}}),
        json!({"timeline_interval":{"start_us":0,"end_us":60_000_000}}),
        json!({"coordinate_offset_us":0}),
    ] {
        legacy["components"][0]["compatibility"] = marker;
        assert_eq!(
            component_mutation::validate_package(
                &fixture.store,
                "default",
                &legacy,
                Some(&approved),
                false
            )
            .unwrap_err()
            .0,
            "native_binding_effect_unsupported"
        );
    }
}
