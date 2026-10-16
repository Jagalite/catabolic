// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! R1 pre-schema composition evidence against the existing SQLite, query,
//! component and fallback owners. All files and catalogs are disposable.
//!
//! These are deliberately legacy association items, not a test implementation
//! of typed versions. Metadata labels below are input evidence, not validated
//! timeline identities. In particular, the existing resolver cannot choose a
//! version for a work, establish equivalence, or require omitted declared parts.
use catabolic_core::catalog_identity::{
    ActiveSegment, EquivalenceBasis, EquivalenceEvidence, ExactFileRevision, IntervalUs,
    SegmentationSet, SourceExtent, VersionBinding, VersionContract, VersionState,
};
use catabolic_core::encode;
use catabolic_store::{
    Result, Store,
    catalog_identity::{
        self, CatalogManifest, ManifestVersion, VersionRequest, VersionResolution, VersionSelection,
    },
    component_mutation, curation, definitions, fallback_mutation,
    fallback_resolution::{self, Context, Resolver},
    graphql, media_mutation, migration, public_components,
    selection::Evaluation,
    sql,
};
use rusqlite::params;
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, os::unix::fs::MetadataExt, path::Path};

const VERSIONS: [&str; 4] = ["encode-a", "encode-b", "other-cut", "unknown-encode"];

struct Fixture {
    root: tempfile::TempDir,
    store: Option<Store>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        let path = root.path().join("catalog.db");
        migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let metadata = source.metadata().unwrap();
        store
            .db
            .execute_batch(
                "INSERT INTO locations VALUES ('media');
             INSERT INTO scans(id,profile,location,complete,observed,errors)
             VALUES ('scan','default','media',1,1,'[]');",
            )
            .unwrap();
        store
            .db
            .execute(
                "INSERT INTO bindings VALUES ('default','source','media',?,?,?)",
                params![
                    source.to_str(),
                    metadata.dev() as i64,
                    metadata.ino() as i64
                ],
            )
            .unwrap();
        for id in ["work", "theatrical", "directors-cut"]
            .into_iter()
            .chain(VERSIONS)
        {
            curation::item_put(
                &store,
                &json!({
                    "id":id,"kind":"movie","metadata":{"title":"Same title","language":"en"}
                }),
            )
            .unwrap();
        }
        for edition in ["theatrical", "directors-cut"] {
            curation::relate(
                &store,
                &json!({
                    "source":edition,"target":"work","kind":"edition_of"
                }),
            )
            .unwrap();
        }
        for version in VERSIONS {
            // This ordinary relationship is queryable composition only. The
            // typed owner, foreign keys and equivalence evidence do not exist yet.
            let edition = if version == "other-cut" {
                "directors-cut"
            } else {
                "theatrical"
            };
            curation::relate(
                &store,
                &json!({
                    "source":version,"target":edition,"kind":"custom:version_of"
                }),
            )
            .unwrap();
        }
        let fixture = Self {
            root,
            store: Some(store),
        };
        for version in VERSIONS {
            for part in [1, 2] {
                fixture.add_file(
                    &format!("{version}-{part}"),
                    version,
                    "primary",
                    part,
                    &Self::metadata(version),
                );
                fixture.probe(&format!("{version}-{part}"));
            }
        }
        fixture
    }

    fn store(&self) -> &Store {
        self.store.as_ref().unwrap()
    }

    fn metadata(version: &str) -> Value {
        let (edition, timeline) = match version {
            "other-cut" => ("directors-cut", "director-timeline"),
            "unknown-encode" => ("theatrical", "provisional-unknown"),
            _ => ("theatrical", "theatrical-timeline"),
        };
        json!({"representation_group":version,"edition_id":edition,"timeline_id":timeline})
    }

    fn source(&self, file: &str) -> std::path::PathBuf {
        self.root.path().join("source").join(format!("{file}.bin"))
    }

    fn add_file(&self, file: &str, item: &str, role: &str, part: i64, metadata: &Value) {
        fs::write(self.source(file), b"disposable composition evidence").unwrap();
        let observed = self.source(file).metadata().unwrap();
        self.store()
            .db
            .execute(
                "INSERT INTO files VALUES (?,'media',?)",
                params![file, format!("{file}.bin")],
            )
            .unwrap();
        self.store()
            .db
            .execute(
                "INSERT INTO observations VALUES ('default',?,?,?,?,?,'present','scan')",
                params![
                    file,
                    observed.len() as i64,
                    observed.mtime() * 1_000_000_000 + observed.mtime_nsec(),
                    observed.dev() as i64,
                    observed.ino() as i64
                ],
            )
            .unwrap();
        curation::dispatch(
            self.store(),
            "default",
            "associate",
            &json!({
                "file_id":file,"item_id":item,"role":role,"part":part,"metadata":metadata
            }),
        )
        .unwrap();
    }

    fn probe(&self, file: &str) {
        // Seed recorded tool evidence, as the existing component fixtures do;
        // indexing, compatibility, queries and live source checks are production.
        let snapshot = media_mutation::snapshot(self.store(), "default", file).unwrap();
        let job = uuid::Uuid::new_v4().to_string();
        let data = json!({"format":{"duration":"120.0","format_name":"matroska"},"streams":[
            {"index":0,"codec_type":"video","codec_name":"h264","duration":"120.0"},
            {"index":1,"codec_type":"audio","codec_name":"aac","tags":{"language":"eng"}}
        ]});
        self.store().transaction(|db| {
            db.execute("INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state,result) VALUES (?,'default',?,'probe','media',?,'{}',?,'complete',?)",
                params![job, file, encode(&snapshot), job, encode(&data)])?;
            db.execute("INSERT INTO file_facts VALUES ('default',?,'probe',?,?,?,'complete') ON CONFLICT(profile,file_id,operation) DO UPDATE SET job_id=excluded.job_id,snapshot=excluded.snapshot,data=excluded.data,status=excluded.status",
                params![file, job, encode(&snapshot), encode(&data)])?;
            public_components::call(self.store(), "default", "index_file", &json!({"file_id":file}))?;
            Ok(())
        }).unwrap();
    }

    fn query(&self, name: &str, source: &str) -> String {
        definitions::query_put(
            self.store(),
            "default",
            name,
            &json!({
                "selection":{"language":"sql","query":source}
            }),
        )
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn policy(&self, subtitles: bool) -> String {
        let primary = self.query(
            "all-primary",
            "SELECT id AS association_id FROM item_files WHERE role='primary' AND active=1",
        );
        let audio = self.query("all-audio", "SELECT occurrence_id FROM catalog_component_occurrences WHERE profile=:profile AND kind='audio' AND current=1");
        let mut requirements =
            vec![json!({"name":"audio","fallbacks":[{"name":"all","query_id":audio}]})];
        if subtitles {
            let subtitle = self.query("all-subtitles", "SELECT occurrence_id FROM catalog_component_occurrences WHERE profile=:profile AND kind='subtitle' AND current=1");
            requirements
                .push(json!({"name":"subtitles","fallbacks":[{"name":"all","query_id":subtitle}]}));
        }
        fallback_mutation::put(self.store(), "default", "composition", &json!({
            "fallbacks":[{"name":"all","query_id":primary}],
            "within_tier":{"tie_break":"file_id"},"failback":{"mode":"immediate"},
            // Candidate-evidence query timeouts are swallowed as unavailable
            // evidence, so parallel debug runs need the maximum fixture budget.
            "budgets":{"query_timeout_ms":30000},
            "package":{"publication":if subtitles {"sidecars"} else {"selected_only"},"requirements":requirements}
        })).unwrap()["id"].as_str().unwrap().to_owned()
    }

    fn entries(items: &[&str]) -> Vec<Value> {
        items
            .iter()
            .flat_map(|item| [1, 2].map(|part| json!({"item_id":item,"part":part})))
            .collect()
    }

    fn resolve(&mut self, items: &[&str], policy: &str) -> Value {
        let mut evaluation =
            Evaluation::new(self.store.take().unwrap(), "default", 60000, 100000, false).unwrap();
        let mut probe =
            |path: &Path, profile: &str, database: &str, snapshots: &Value, _| -> Result<Value> {
                fallback_resolution::probe_check(&json!({
                    "database":path,"database_id":database,"profile":profile,"snapshots":snapshots
                }))
            };
        let result = Resolver::new(Context::Local(&mut evaluation), &mut probe, None).resolve(
            &Self::entries(items),
            policy,
            "global",
            &BTreeMap::new(),
            false,
        );
        self.store = Some(evaluation.into_store());
        result.unwrap()
    }

    fn occurrence(&self, file: &str, kind: &str) -> Value {
        let id: String = self.store().db.query_row(
            &format!("SELECT occurrence_id FROM ({}) WHERE profile='default' AND file_id=? AND kind=? AND current=1",
                catabolic_store::query::view_sql("catalog_component_occurrences")),
            [file, kind], |r|r.get(0)
        ).unwrap();
        component_mutation::get(self.store(), "default", &id, true).unwrap()
    }

    fn claim(&self, file: &str, part: i64) -> Value {
        let video = self.occurrence(&format!("encode-a-{part}"), "video");
        let subtitle = self.occurrence(file, "subtitle");
        json!({"occurrence_id":subtitle["occurrence_id"],"compatibility":{
            "edition_id":"theatrical","video_file_id":video["file_id"],"video_revision":video["revision"],
            "part":part,"timeline_id":"theatrical-timeline","coverage":"full","offset_seconds":0.0,"synchronization":"declared"
        }})
    }

    fn accept(&self, file: &str, claim: &Value) -> String {
        let proposal = curation::proposal_put(
            self.store(),
            "default",
            &json!({
                "file_id":file,"payload":{"item_id":"encode-a","component":claim},
                "evidence":{"review":"disposable R1 composition fixture"}
            }),
        )
        .unwrap();
        let accepted = curation::proposal_decide(
            self.store(),
            "default",
            &json!({
                "id":proposal["id"],"accept":true,"actor":"fixture"
            }),
        )
        .unwrap();
        assert_eq!(accepted["state"], "accepted");
        proposal["id"].as_str().unwrap().to_owned()
    }

    fn manifest(&self) -> CatalogManifest {
        CatalogManifest {
            contract_version: 1,
            database_id: self.store().database_id.clone(),
            expected_epoch: fallback_resolution::epoch(self.store()).unwrap(),
            versions: VERSIONS
                .iter()
                .map(|name| {
                    let metadata = Self::metadata(name);
                    ManifestVersion {
                        version: VersionContract {
                            contract_version: 1,
                            version_id: (*name).into(),
                            association_item_id: (*name).into(),
                            edition_item_id: metadata["edition_id"].as_str().unwrap().into(),
                            timeline_id: metadata["timeline_id"].as_str().unwrap().into(),
                            state: VersionState::Ready,
                            declared_coverage: Some(interval(0, 240_000_000)),
                            bindings: [1, 2]
                                .into_iter()
                                .map(|part| {
                                    let file = format!("{name}-{part}");
                                    let video = self.occurrence(&file, "video");
                                    VersionBinding {
                                        binding_id: format!("binding-{file}"),
                                        association_id: video["association_id"]
                                            .as_str()
                                            .unwrap()
                                            .into(),
                                        file: ExactFileRevision {
                                            file_id: file,
                                            revision: video["revision"].as_str().unwrap().into(),
                                        },
                                        part,
                                        source: SourceExtent::WholeFile {
                                            duration_us: Some(120_000_000),
                                        },
                                        timeline_interval: Some(interval(
                                            i64::from(part - 1) * 120_000_000,
                                            i64::from(part) * 120_000_000,
                                        )),
                                    }
                                })
                                .collect(),
                        },
                        equivalence: matches!(*name, "encode-a" | "encode-b").then(|| {
                            EquivalenceEvidence {
                                basis: EquivalenceBasis::Declared,
                                evidence_id: format!("declared-{name}-theatrical"),
                            }
                        }),
                    }
                })
                .collect(),
            segmentation_sets: vec![],
        }
    }

    fn native(
        &mut self,
        manifest: &CatalogManifest,
        request: &VersionRequest,
    ) -> Result<VersionResolution> {
        let mut evaluation =
            Evaluation::new(self.store.take().unwrap(), "default", 60000, 100000, false).unwrap();
        let mut probe = |path: &Path, profile: &str, database: &str, snapshots: &Value, _| {
            fallback_resolution::probe_check(
                &json!({"database":path,"database_id":database,"profile":profile,"snapshots":snapshots}),
            )
        };
        let result =
            catalog_identity::resolve_manifest(&mut evaluation, manifest, request, &mut probe);
        self.store = Some(evaluation.into_store());
        result
    }
}

fn interval(start: i64, end: i64) -> IntervalUs {
    IntervalUs::new(start, end).unwrap()
}

fn request(policy: &str) -> VersionRequest {
    VersionRequest {
        edition_item_id: "theatrical".into(),
        timeline_id: "theatrical-timeline".into(),
        coverage: interval(0, 240_000_000),
        selection: VersionSelection::Equivalent {
            candidates: vec!["encode-a".into(), "encode-b".into()],
        },
        policy_id: policy.into(),
        catalog: "global".into(),
    }
}

fn decisions(result: &Value) -> &[Value] {
    result["decisions"].as_array().unwrap()
}

fn assert_blocked(decision: &Value) {
    assert_eq!(decision["state"], "blocked", "{decision}");
    assert_eq!(decision["reasons"], json!(["multipart_coverage_ambiguous"]));
    assert!(decision["file_id"].is_null());
    assert!(decision["revision"].is_null());
    assert!(decision["content_path"].is_null());
    assert_eq!(
        decision["evidence"]["component_package"]["packaging"]["ready"],
        false
    );
}

#[test]
fn separate_items_preserve_two_complete_two_part_encodes_in_actual_queries() {
    let fixture = Fixture::new();
    let sql = sql::execute_store(fixture.store(), Some(
        "SELECT item_id,part,file_id FROM item_files WHERE item_id IN ('encode-a','encode-b') AND active=1 ORDER BY item_id,part"
    ), &sql::Options::default()).unwrap();
    assert_eq!(
        sql["rows"],
        json!([
            ["encode-a", 1, "encode-a-1"],
            ["encode-a", 2, "encode-a-2"],
            ["encode-b", 1, "encode-b-1"],
            ["encode-b", 2, "encode-b-2"]
        ])
    );
    let error = curation::dispatch(
        fixture.store(),
        "default",
        "associate",
        &json!({
            "file_id":"encode-b-1","item_id":"encode-a","part":1
        }),
    )
    .unwrap_err();
    assert_eq!(
        error.0,
        "that item/role already has a file at this part number"
    );
    // SQLite independently retains the uniqueness constraint; no schema relaxation.
    assert!(fixture.store().db.execute(
        "INSERT INTO item_files(id,file_id,item_id,role,part,origin) VALUES ('duplicate','encode-b-1','encode-a','primary',1,'explicit')", []
    ).is_err());

    let gql = graphql::execute(
        &fixture.store().path,
        "{ componentOccurrences(kind:\"video\",current:true){nodes{id itemId fileId revision}} }",
        "default",
        json!({}),
        None,
        5000,
    )
    .unwrap();
    assert!(gql.get("errors").is_none(), "{gql}");
    let rows = gql["data"]["componentOccurrences"]["nodes"]
        .as_array()
        .unwrap();
    assert_eq!(rows.len(), 8);
    for version in VERSIONS {
        for part in [1, 2] {
            let rows = rows
                .iter()
                .filter(|r| r["itemId"] == version && r["fileId"] == format!("{version}-{part}"))
                .collect::<Vec<_>>();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0]["fileId"], format!("{version}-{part}"));
            assert_eq!(
                rows[0]["revision"],
                fixture.occurrence(&format!("{version}-{part}"), "video")["revision"]
            );
        }
    }
}

#[test]
fn existing_fallback_resolves_each_complete_item_without_cross_version_components() {
    let mut fixture = Fixture::new();
    let policy = fixture.policy(false);
    let result = fixture.resolve(&VERSIONS, &policy);
    assert!(
        result["checks"].as_u64().unwrap() >= 8,
        "real probes must run"
    );
    assert_eq!(decisions(&result).len(), 8);
    for decision in decisions(&result) {
        assert_eq!(decision["state"], "resolved_preferred", "{decision}");
        assert_eq!(
            decision["file_id"],
            format!(
                "{}-{}",
                decision["item_id"].as_str().unwrap(),
                decision["part"]
            )
        );
        let package = &decision["evidence"]["component_package"];
        assert_eq!(package["packaging"]["action"], "use_container");
        assert_eq!(package["packaging"]["exposes_unselected_components"], false);
        assert_eq!(package["video"]["revision"], decision["revision"]);
        for selected in package["components"].as_array().unwrap() {
            let occurrence = &selected["occurrence"];
            assert_eq!(occurrence["item_id"], decision["item_id"]);
            assert_eq!(occurrence["part"], decision["part"]);
            assert_eq!(occurrence["file_id"], decision["file_id"]);
            assert_eq!(occurrence["revision"], decision["revision"]);
            // Legacy internal tracks still use container identity. Metadata
            // labels have not silently replaced that production contract.
            assert_eq!(
                selected["compatibility"]["timeline_id"],
                format!("container:{}", decision["revision"].as_str().unwrap())
            );
        }
    }
}

#[test]
fn missing_part_blocks_the_whole_item_and_never_borrows_an_alternative_part() {
    let mut fixture = Fixture::new();
    let policy = fixture.policy(false);
    fs::remove_file(fixture.source("encode-a-2")).unwrap();
    let result = fixture.resolve(&["encode-a", "encode-b"], &policy);
    for decision in decisions(&result) {
        if decision["item_id"] == "encode-a" {
            assert_blocked(decision);
        } else {
            assert_eq!(decision["state"], "resolved_preferred", "{decision}");
        }
    }
    fs::remove_file(fixture.source("encode-b-1")).unwrap();
    let result = fixture.resolve(&["encode-a", "encode-b"], &policy);
    for decision in decisions(&result) {
        assert_blocked(decision);
    }
    // A-1 plus B-2 is not a playable representation. Nothing is published.
    assert_eq!(
        fixture
            .store()
            .db
            .query_row("SELECT count(*) FROM fallback_entries", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn mismatched_or_absent_legacy_representation_groups_are_refused() {
    for metadata in [json!({}), json!({"representation_group":"encode-b"})] {
        let mut fixture = Fixture::new();
        curation::dispatch(
            fixture.store(),
            "default",
            "associate",
            &json!({
                "file_id":"encode-a-2","item_id":"encode-a","part":2,"metadata":metadata
            }),
        )
        .unwrap();
        let result = fixture.resolve(&["encode-a"], &fixture.policy(false));
        for decision in decisions(&result) {
            assert_blocked(decision);
        }
    }
}

#[test]
fn related_cuts_and_unknown_encodes_do_not_supply_a_missing_part() {
    let mut fixture = Fixture::new();
    let policy = fixture.policy(false);
    fs::remove_file(fixture.source("encode-a-2")).unwrap();
    fs::remove_file(fixture.source("encode-b-2")).unwrap();
    let result = fixture.resolve(&VERSIONS, &policy);
    for decision in decisions(&result) {
        if matches!(decision["item_id"].as_str(), Some("encode-a" | "encode-b")) {
            assert_blocked(decision);
        } else {
            assert_eq!(decision["state"], "resolved_preferred");
        }
    }
    // Related work/edition IDs alone do not resolve a concrete representation.
    let result = fixture.resolve(&["work", "theatrical"], &policy);
    for decision in decisions(&result) {
        assert_blocked(decision);
    }
}

#[test]
fn same_path_replacement_stales_components_and_retains_old_revision_evidence() {
    let mut fixture = Fixture::new();
    let old = fixture.occurrence("encode-a-1", "video");
    let policy = fixture.policy(false);
    fs::write(
        fixture.source("encode-a-1"),
        b"a replacement with a distinctly different file size",
    )
    .unwrap();
    let result = fixture.resolve(&["encode-a"], &policy);
    for decision in decisions(&result) {
        assert_blocked(decision);
    }
    let observed = fixture.source("encode-a-1").metadata().unwrap();
    fixture.store().db.execute("UPDATE observations SET size=?,mtime_ns=?,device=?,inode=? WHERE file_id='encode-a-1' AND profile='default'",
        params![observed.len() as i64,observed.mtime()*1_000_000_000+observed.mtime_nsec(),observed.dev() as i64,observed.ino() as i64]).unwrap();
    assert_eq!(
        component_mutation::get(
            fixture.store(),
            "default",
            old["occurrence_id"].as_str().unwrap(),
            true
        )
        .unwrap_err()
        .0,
        "stale_or_conflicting_component_occurrence"
    );
    let history = component_mutation::get(
        fixture.store(),
        "default",
        old["occurrence_id"].as_str().unwrap(),
        false,
    )
    .unwrap();
    assert_eq!(history["current"], false);
    assert_eq!(history["revision"], old["revision"]);
    let result = fixture.resolve(&["encode-a"], &policy);
    for decision in decisions(&result) {
        assert_blocked(decision);
    }
    fixture.probe("encode-a-1");
    let current = fixture.occurrence("encode-a-1", "video");
    assert_ne!(current["revision"], old["revision"]);
    assert_ne!(current["occurrence_id"], old["occurrence_id"]);
    // Legacy associations follow current file evidence after a fresh probe.
    // This is NOT the future typed binding's required explicit rebind contract.
    let result = fixture.resolve(&["encode-a"], &policy);
    assert_eq!(decisions(&result)[0]["revision"], current["revision"]);
    assert_eq!(decisions(&result)[0]["state"], "resolved_preferred");
}

#[test]
fn external_claims_require_exact_item_part_edition_revision_and_full_coverage() {
    let fixture = Fixture::new();
    fixture.add_file(
        "subtitle-1",
        "encode-a",
        "subtitle",
        1,
        &json!({"language":"en"}),
    );
    let valid = fixture.claim("subtitle-1", 1);
    component_mutation::validate_claim(
        fixture.store(),
        "default",
        "subtitle-1",
        "encode-a",
        &valid,
    )
    .unwrap();
    for (key, value, message) in [
        (
            "coverage",
            json!("partial"),
            "invalid component assertion: invalid compatibility declaration",
        ),
        (
            "part",
            json!(2),
            "component compatibility edition or part mismatch",
        ),
        (
            "edition_id",
            json!("directors-cut"),
            "component compatibility edition mismatch",
        ),
        (
            "video_file_id",
            json!("encode-b-1"),
            "compatibility requires a primary of this exact item and part",
        ),
        (
            "video_revision",
            json!("old-revision"),
            "stale component compatibility or dependency revision",
        ),
        (
            "synchronization",
            json!("inferred"),
            "invalid component assertion: invalid compatibility declaration",
        ),
    ] {
        let mut claim = valid.clone();
        claim["compatibility"][key] = value;
        assert_eq!(
            component_mutation::validate_claim(
                fixture.store(),
                "default",
                "subtitle-1",
                "encode-a",
                &claim
            )
            .unwrap_err()
            .0,
            message,
            "{key}"
        );
    }
    assert_eq!(
        component_mutation::validate_claim(
            fixture.store(),
            "default",
            "subtitle-1",
            "encode-b",
            &valid
        )
        .unwrap_err()
        .0,
        "component assertion target is stale or mismatched"
    );
    assert_eq!(
        fixture
            .store()
            .db
            .query_row("SELECT count(*) FROM component_assertions", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn accepted_external_components_still_need_the_selected_timeline_and_revision() {
    let mut fixture = Fixture::new();
    for part in [1, 2] {
        let file = format!("subtitle-{part}");
        fixture.add_file(
            &file,
            "encode-a",
            "subtitle",
            part,
            &json!({"language":"en"}),
        );
        let mut claim = fixture.claim(&file, part);
        if part == 1 {
            claim["compatibility"]["timeline_id"] = json!("director-timeline");
        }
        fixture.accept(&file, &claim);
    }
    let policy = fixture.policy(true);
    let result = fixture.resolve(&["encode-a"], &policy);
    for decision in decisions(&result) {
        assert_blocked(decision);
    }
    let old: String = fixture.store().db.query_row(
        "SELECT a.id FROM component_assertions a JOIN component_occurrences o ON o.id=a.occurrence_id WHERE o.file_id='subtitle-1' AND a.active=1", [], |r|r.get(0)
    ).unwrap();
    let mut corrected = fixture.claim("subtitle-1", 1);
    corrected["supersedes"] = json!([old]);
    fixture.accept("subtitle-1", &corrected);
    let result = fixture.resolve(&["encode-a"], &policy);
    for decision in decisions(&result) {
        assert_eq!(decision["state"], "resolved_preferred", "{decision}");
        let package = &decision["evidence"]["component_package"];
        assert_eq!(package["packaging"]["action"], "publish_sidecars");
        let subtitle = package["components"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["requirement"] == "subtitles")
            .unwrap();
        assert_eq!(subtitle["occurrence"]["item_id"], "encode-a");
        assert_eq!(subtitle["occurrence"]["part"], decision["part"]);
        assert_eq!(
            subtitle["compatibility"]["timeline_id"],
            "theatrical-timeline"
        );
        assert_eq!(
            subtitle["compatibility"]["video_revision"],
            decision["revision"]
        );
    }
    // A valid claim is not portable to the other complete encode, even though
    // it has the same edition label, timeline label, title, duration and language.
    let result = fixture.resolve(&["encode-b"], &policy);
    for decision in decisions(&result) {
        assert_blocked(decision);
    }
}

#[test]
fn native_bridge_selects_one_entire_alternative_and_never_combines_partial_versions() {
    let mut fixture = Fixture::new();
    let policy = fixture.policy(false);
    let manifest = fixture.manifest();
    let request = request(&policy);
    let first = fixture.native(&manifest, &request).unwrap();
    assert_eq!(first.selected_version_id.as_deref(), Some("encode-a"));
    assert_eq!(first.parts.len(), 2);
    assert!(!first.publication_ready);
    fs::remove_file(fixture.source("encode-a-2")).unwrap();
    let alternate = fixture.native(&manifest, &request).unwrap();
    assert_eq!(alternate.selected_version_id.as_deref(), Some("encode-b"));
    assert_eq!(alternate.attempted_versions, vec!["encode-a", "encode-b"]);
    assert_eq!(
        alternate
            .parts
            .iter()
            .map(|p| p.binding.file.file_id.as_str())
            .collect::<Vec<_>>(),
        vec!["encode-b-1", "encode-b-2"]
    );
    for part in &alternate.parts {
        for component in &part.components {
            assert_eq!(
                component["compatibility"]["timeline_id"],
                "theatrical-timeline"
            );
            assert_eq!(component["compatibility"]["version_id"], "encode-b");
        }
    }
    fs::remove_file(fixture.source("encode-b-1")).unwrap();
    let unavailable = fixture.native(&manifest, &request).unwrap();
    assert!(unavailable.selected_version_id.is_none());
    assert!(unavailable.parts.is_empty());
    assert!(!unavailable.publication_ready);
}

#[test]
fn native_bridge_checks_all_candidates_and_does_not_infer_unknown_equivalence() {
    let mut fixture = Fixture::new();
    let policy = fixture.policy(false);
    let manifest = fixture.manifest();
    let mut request = request(&policy);
    for name in ["other-cut", "unknown-encode"] {
        request.selection = VersionSelection::Equivalent {
            candidates: vec!["encode-a".into(), name.into()],
        };
        assert_eq!(
            fixture.native(&manifest, &request).unwrap_err().0,
            "catalog_identity_version_scope_mismatch"
        );
    }
    let mut forged = manifest.clone();
    forged.versions[3].version.timeline_id = "theatrical-timeline".into();
    assert_eq!(
        fixture.native(&forged, &request).unwrap_err().0,
        "catalog_identity_missing_equivalence_evidence"
    );
    request.selection = VersionSelection::Explicit {
        version_id: "unknown-encode".into(),
    };
    request.timeline_id = "provisional-unknown".into();
    let direct = fixture.native(&manifest, &request).unwrap();
    assert_eq!(
        direct.selected_version_id.as_deref(),
        Some("unknown-encode")
    );
    assert_eq!(direct.parts.len(), 2);
}

#[test]
fn native_manifest_requires_the_complete_actual_binding_set_and_revision() {
    let mut fixture = Fixture::new();
    let policy = fixture.policy(false);
    let manifest = fixture.manifest();
    let request = request(&policy);
    let mut omitted = manifest.clone();
    omitted.versions[0].version.bindings.pop();
    omitted.versions[0].version.declared_coverage = Some(interval(0, 120_000_000));
    assert_eq!(
        fixture.native(&omitted, &request).unwrap_err().0,
        "catalog_identity_binding_set_mismatch"
    );
    let mut foreign = manifest.clone();
    foreign.versions[0].version.bindings[0].association_id = manifest.versions[1].version.bindings
        [0]
    .association_id
    .clone();
    assert_eq!(
        fixture.native(&foreign, &request).unwrap_err().0,
        "catalog_identity_binding_set_mismatch"
    );
    let mut stale = manifest.clone();
    stale.versions[0].version.bindings[0].file.revision = "previous-file-revision".into();
    assert_eq!(
        fixture.native(&stale, &request).unwrap_err().0,
        "catalog_identity_stale_file_revision"
    );
    fixture
        .store()
        .db
        .execute(
            "UPDATE observations SET size=size+1 WHERE file_id='encode-a-1'",
            [],
        )
        .unwrap();
    assert_eq!(
        fixture.native(&manifest, &request).unwrap_err().0,
        "stale_resolution_plan"
    );
    stale = manifest.clone();
    stale.expected_epoch = fallback_resolution::epoch(fixture.store()).unwrap();
    assert_eq!(
        fixture.native(&stale, &request).unwrap_err().0,
        "catalog_identity_stale_file_revision"
    );
}

#[test]
fn native_component_bridge_refuses_legacy_external_full_claim_without_coordinate_mapping() {
    let mut fixture = Fixture::new();
    for part in [1, 2] {
        let file = format!("subtitle-{part}");
        fixture.add_file(
            &file,
            "encode-a",
            "subtitle",
            part,
            &json!({"language":"en"}),
        );
        fixture.accept(&file, &fixture.claim(&file, part));
    }
    let policy = fixture.policy(true);
    let manifest = fixture.manifest();
    // The existing full-container claim is accepted and playable on the legacy
    // path, but cannot establish selected-binding coordinate coverage.
    let legacy = fixture.resolve(&["encode-a"], &policy);
    assert!(
        decisions(&legacy)
            .iter()
            .all(|d| d["state"] == "resolved_preferred")
    );
    assert_eq!(
        fixture.native(&manifest, &request(&policy)).unwrap_err().0,
        "catalog_identity_component_binding_unsupported"
    );
}

#[test]
fn native_manifest_is_fenced_across_the_real_detached_probe_boundary() {
    let mut fixture = Fixture::new();
    let policy = fixture.policy(false);
    let manifest = fixture.manifest();
    let mut evaluation = Evaluation::new(
        fixture.store.take().unwrap(),
        "default",
        60000,
        100000,
        false,
    )
    .unwrap();
    let mut observed_detachment = false;
    let mut probe = |path: &Path, profile: &str, database: &str, snapshots: &Value, _| {
        // This writer can only open after the production owner has detached.
        // Change a different candidate, proving the whole manifest is fenced.
        let writer = Store::open(path, true, false)?;
        writer.transaction(|db| {
            db.execute(
                "UPDATE observations SET size=size+1 WHERE file_id='encode-b-2'",
                [],
            )?;
            Ok(())
        })?;
        drop(writer);
        observed_detachment = true;
        fallback_resolution::probe_check(
            &json!({"database":path,"database_id":database,"profile":profile,"snapshots":snapshots}),
        )
    };
    let error = catalog_identity::resolve_manifest(
        &mut evaluation,
        &manifest,
        &request(&policy),
        &mut probe,
    )
    .unwrap_err();
    assert_eq!(error.0, "stale_resolution_plan");
    assert!(observed_detachment);
    assert!(evaluation.store().owns_writer_lock());
    fixture.store = Some(evaluation.into_store());
    assert_eq!(
        fixture
            .store()
            .db
            .query_row("SELECT count(*) FROM fallback_entries", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn native_candidates_share_one_existing_resolver_entry_and_probe_budget() {
    let mut fixture = Fixture::new();
    let policy = fixture.policy(false);
    let mut definition =
        fallback_mutation::get(fixture.store(), "default", &policy).unwrap()["definition"].clone();
    definition["budgets"]["max_entries"] = json!(2);
    let bounded = fallback_mutation::put(fixture.store(), "default", "two-entries", &definition)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let manifest = fixture.manifest();
    assert_eq!(
        fixture.native(&manifest, &request(&bounded)).unwrap_err().0,
        "resolution_entry_budget"
    );

    definition["budgets"]["max_entries"] = json!(4);
    definition["budgets"]["max_checks"] = json!(2);
    let bounded = fallback_mutation::put(fixture.store(), "default", "two-checks", &definition)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let manifest = fixture.manifest();
    fs::remove_file(fixture.source("encode-a-2")).unwrap();
    let mut evaluation = Evaluation::new(
        fixture.store.take().unwrap(),
        "default",
        60000,
        100000,
        false,
    )
    .unwrap();
    let mut checks = 0;
    let mut probe = |path: &Path, profile: &str, database: &str, snapshots: &Value, _| {
        checks += 1;
        fallback_resolution::probe_check(
            &json!({"database":path,"database_id":database,"profile":profile,"snapshots":snapshots}),
        )
    };
    let result = catalog_identity::resolve_manifest(
        &mut evaluation,
        &manifest,
        &request(&bounded),
        &mut probe,
    )
    .unwrap();
    assert_eq!(
        checks, 2,
        "the fallback alternative must not receive a new probe allowance"
    );
    assert!(result.selected_version_id.is_none());
    assert!(result.parts.is_empty());
    fixture.store = Some(evaluation.into_store());
}

#[test]
fn two_episodes_in_one_file_inherit_exact_slices_through_real_component_selection() {
    let mut fixture = Fixture::new();
    for episode in ["episode-1", "episode-2"] {
        curation::item_put(
            fixture.store(),
            &json!({"id":format!("edition-{episode}"),"kind":"movie","metadata":{}}),
        )
        .unwrap();
        curation::item_put(
            fixture.store(),
            &json!({"id":episode,"kind":"movie","metadata":{}}),
        )
        .unwrap();
        curation::dispatch(
            fixture.store(),
            "default",
            "associate",
            &json!({
                "file_id":"encode-a-1","item_id":episode,"part":1,
                "metadata":{"representation_group":episode}
            }),
        )
        .unwrap();
    }
    let policy = fixture.policy(false);
    let original = ExactFileRevision {
        file_id: "encode-a-1".into(),
        revision: public_components::revision(
            &media_mutation::snapshot(fixture.store(), "default", "encode-a-1").unwrap(),
        )
        .unwrap(),
    };
    let mut manifest = CatalogManifest {
        contract_version: 1,
        database_id: fixture.store().database_id.clone(),
        expected_epoch: fallback_resolution::epoch(fixture.store()).unwrap(),
        versions: vec![],
        segmentation_sets: vec![],
    };
    let mut segments = vec![];
    for (index, episode) in ["episode-1", "episode-2"].into_iter().enumerate() {
        let association: String = fixture
            .store()
            .db
            .query_row(
                "SELECT id FROM item_files WHERE item_id=? AND role='primary'",
                [episode],
                |r| r.get(0),
            )
            .unwrap();
        let source = interval(index as i64 * 60_000_000, (index as i64 + 1) * 60_000_000);
        let binding = VersionBinding {
            binding_id: format!("binding-{episode}"),
            association_id: association,
            file: original.clone(),
            part: 1,
            source: SourceExtent::Slice { interval: source },
            timeline_interval: Some(interval(0, 60_000_000)),
        };
        segments.push(ActiveSegment {
            binding_id: binding.binding_id.clone(),
            file: original.clone(),
            source_interval: source,
        });
        manifest.versions.push(ManifestVersion {
            version: VersionContract {
                contract_version: 1,
                version_id: episode.into(),
                association_item_id: episode.into(),
                edition_item_id: format!("edition-{episode}"),
                timeline_id: format!("timeline-{episode}"),
                state: VersionState::Ready,
                declared_coverage: Some(interval(0, 60_000_000)),
                bindings: vec![binding],
            },
            equivalence: None,
        });
    }
    manifest.segmentation_sets.push(SegmentationSet {
        contract_version: 1,
        segmentation_set_id: "two-episodes".into(),
        file: original,
        segments,
    });
    for (index, episode) in ["episode-1", "episode-2"].into_iter().enumerate() {
        let request = VersionRequest {
            edition_item_id: format!("edition-{episode}"),
            timeline_id: format!("timeline-{episode}"),
            coverage: interval(0, 60_000_000),
            selection: VersionSelection::Explicit {
                version_id: episode.into(),
            },
            policy_id: policy.clone(),
            catalog: "global".into(),
        };
        let result = fixture.native(&manifest, &request).unwrap();
        assert_eq!(result.selected_version_id.as_deref(), Some(episode));
        assert!(!result.publication_ready);
        assert_eq!(result.parts.len(), 1);
        let part = &result.parts[0];
        assert!(part.content_path.is_none());
        assert!(part.requires_interval_enforcement);
        assert_eq!(
            part.components.len(),
            2,
            "real embedded video and audio are selected"
        );
        for selected in &part.components {
            assert_eq!(selected["occurrence"]["item_id"], episode);
            assert_eq!(
                selected["compatibility"]["basis"],
                "native_internal_binding_v1"
            );
            assert_eq!(
                selected["compatibility"]["source_interval"],
                json!(interval(
                    index as i64 * 60_000_000,
                    (index as i64 + 1) * 60_000_000
                ))
            );
            assert_eq!(
                selected["compatibility"]["timeline_interval"],
                json!(interval(0, 60_000_000))
            );
        }
    }
    let mut incomplete = manifest.clone();
    incomplete.segmentation_sets[0].segments.pop();
    assert_eq!(
        catalog_identity::validate_manifest(fixture.store(), "default", &incomplete)
            .unwrap_err()
            .0,
        "catalog_identity_slice_requires_segmentation_set"
    );
    let mut overlap = manifest.clone();
    overlap.segmentation_sets[0].segments[1].source_interval = interval(59_000_000, 119_000_000);
    assert_eq!(
        catalog_identity::validate_manifest(fixture.store(), "default", &overlap)
            .unwrap_err()
            .0,
        "catalog_identity_overlapping_coverage"
    );
    for overlapping in [false, true] {
        let mut split = manifest.clone();
        let mut second = split.segmentation_sets[0].clone();
        second.segmentation_set_id = "second-set".into();
        second.segments = vec![second.segments.remove(1)];
        split.segmentation_sets[0].segments.pop();
        if overlapping {
            let source = interval(59_000_000, 119_000_000);
            second.segments[0].source_interval = source;
            split.versions[1].version.bindings[0].source = SourceExtent::Slice { interval: source };
        }
        split.segmentation_sets.push(second);
        assert_eq!(
            catalog_identity::validate_manifest(fixture.store(), "default", &split)
                .unwrap_err()
                .0,
            "catalog_identity_duplicate_segmentation_revision",
            "splitting one source into singleton sets cannot bypass this manifest's one editorial context"
        );
    }
}
