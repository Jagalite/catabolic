// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Persistent identity commands against real, disposable native catalogs.
//! Fixture associations are ordinary curation records until explicitly claimed.
use catabolic_core::{
    catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility},
    catalog_identity::{
        EquivalenceBasis, EquivalenceEvidence, ExactFileRevision, IntervalUs, SourceExtent,
        VersionBinding, VersionContract, VersionState,
    },
    encode,
};
use catabolic_store::{
    Store,
    catalog_commands::{
        self, CommandOutcome, CommandStoreErrorCode, ReceiptPolicy, TrustedCommandAuthority,
    },
    catalog_identity::{self, VersionRequest, VersionSelection},
    catalog_identity_mutation::{
        self, ContextChange, EquivalenceClaim, IdentityAuthority, IdentityEvidence,
        IdentityMutation, TimelineChange, TimelineState, VersionChange,
    },
    component_mutation, curation, definitions, fallback_mutation, fallback_projection,
    fallback_resolution, media_mutation, migration, public_components,
    selection::Evaluation,
};
use rusqlite::params;
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, os::unix::fs::MetadataExt, path::PathBuf, time::Duration};

const MINUTE: i64 = 60_000_000;

fn interval(start: i64, end: i64) -> IntervalUs {
    IntervalUs::new(start, end).unwrap()
}

struct Fixture {
    _root: tempfile::TempDir,
    path: PathBuf,
    store: Store,
    authority: IdentityAuthority,
}

impl Fixture {
    fn new() -> Self {
        Self::at_schema(33)
    }

    fn at_schema(schema: u32) -> Self {
        Self::with_motion_graph(schema, false)
    }

    fn with_motion_graph(schema: u32, motion: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        let source_meta = source.metadata().unwrap();
        let path = root.path().join("native.db");
        migration::initialize_native_at(&path, schema).unwrap();
        let store = Store::open(&path, true, false).unwrap();
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
                    source_meta.dev() as i64,
                    source_meta.ino() as i64
                ],
            )
            .unwrap();
        for file in ["shared", "alternative", "part-two"] {
            let media = source.join(format!("{file}.mkv"));
            fs::write(&media, b"persistent identity fixture").unwrap();
            let observed = media.metadata().unwrap();
            store
                .db
                .execute(
                    "INSERT INTO files VALUES (?,'media',?)",
                    params![file, format!("{file}.mkv")],
                )
                .unwrap();
            store
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
        }
        for item in [
            "edition",
            "other-edition",
            "one",
            "two",
            "alternative-owner",
            "multipart",
        ] {
            curation::item_put(
                &store,
                &json!({"id":item,"kind":if motion { if item.ends_with("edition") {"custom:motion_edition"} else {"custom:motion_version"} } else {"movie"},
                "metadata":{"title":"Same apparent title","edition_id":"guessed-edition",
                    "timeline_id":"guessed-timeline","representation_group":"same-label"}}),
            )
            .unwrap();
        }
        for (item, file, part) in [
            ("one", "shared", 1),
            ("two", "shared", 1),
            ("alternative-owner", "alternative", 1),
            ("multipart", "shared", 1),
            ("multipart", "part-two", 2),
        ] {
            curation::dispatch(
                &store,
                "default",
                "associate",
                &json!({
                "file_id":file,"item_id":item,"role":"primary","part":part,
                "metadata":if motion {json!({})} else {json!({"representation_group":"same-label"})}}),
            )
            .unwrap();
        }
        if motion {
            curation::item_put(
                &store,
                &json!({"id":"work", "kind":"video", "metadata":{"title":"Captured work"}}),
            )
            .unwrap();
            curation::relate(
                &store,
                &json!({"source":"work", "target":"edition", "kind":"custom:motion_edition"}),
            )
            .unwrap();
            curation::relate(
                &store,
                &json!({"source":"edition", "target":"one", "kind":"custom:motion_version"}),
            )
            .unwrap();
        }
        Self {
            _root: root,
            path,
            store,
            authority: IdentityAuthority {
                authority_namespace: "identity-test".into(),
                context_keys: ["editorial-main".into()].into(),
                allow_declared_evidence: true,
                allow_verified_evidence: false,
            },
        }
    }

    fn count(&self, table: &str) -> i64 {
        self.store
            .db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    fn generation(&self) -> i64 {
        self.store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap()
    }

    fn authority(&self) -> TrustedCommandAuthority<'_> {
        TrustedCommandAuthority {
            host_namespace: "identity-test",
            receipt_visibility: ReceiptVisibility::FullResult,
            allow_new_commands: true,
            identity_authority: Some(&self.authority),
        }
    }

    fn envelope(&self, key: &str, payload: Value) -> CommandEnvelope {
        CommandEnvelope::new(
            1,
            CommandKey::new(&self.store.database_id, "identity-test", key).unwrap(),
            self.store.catalog_incarnation.clone().unwrap(),
            self.generation(),
            &payload.to_string(),
        )
        .unwrap()
    }

    fn command(&self, key: &str, mutation: &IdentityMutation) -> CommandEnvelope {
        self.envelope(key, json!({"operation":"identity","mutation":mutation}))
    }

    fn execute(&self, command: &CommandEnvelope) -> catalog_commands::Result<CommandOutcome> {
        catalog_commands::execute(&self.store, command, self.authority(), policy())
    }

    fn commit(&self, key: &str, mutation: &IdentityMutation) -> Value {
        let result = self.execute(&self.command(key, mutation)).unwrap();
        assert_eq!(result.code(), "committed");
        body(result)
    }

    fn reject(&self, key: &str, mutation: &IdentityMutation, code: &str) {
        let generation = self.generation();
        let receipts = self.count("catalog_command_receipts");
        let error = self.execute(&self.command(key, mutation)).unwrap_err();
        assert!(error.error.0.contains(code), "expected {code}, got {error}");
        let category = match code {
            "catalog_identity_context_revision_conflict"
            | "catalog_identity_timeline_revision_conflict"
            | "catalog_identity_version_revision_conflict"
            | "catalog_identity_binding_revision_conflict"
            | "catalog_identity_context_scope_mismatch"
            | "catalog_identity_legacy_epoch_exhausted" => {
                Some(CommandStoreErrorCode::OperationConflict)
            }
            "catalog_identity_context_not_authorized"
            | "catalog_identity_evidence_not_authorized" => {
                Some(CommandStoreErrorCode::AuthorityDenied)
            }
            "catalog_identity_missing_equivalence_evidence"
            | "catalog_identity_binding_set_mismatch"
            | "catalog_identity_invalid_evidence" => Some(CommandStoreErrorCode::InvalidOperation),
            _ => None,
        };
        if let Some(category) = category {
            assert_eq!(error.code, category, "{code}");
        }
        assert_eq!(self.generation(), generation);
        assert_eq!(self.count("catalog_command_receipts"), receipts);
        assert_eq!(self.count("catalog_mutation_permits"), 0);
        assert!(self.store.db.is_autocommit());
    }

    fn version(&self, owner: &str, start: i64) -> VersionContract {
        let mut statement = self
            .store
            .db
            .prepare(
                "SELECT id,file_id,part FROM item_files
            WHERE item_id=? AND active=1 AND role='primary' ORDER BY part",
            )
            .unwrap();
        let records = statement
            .query_map([owner], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, u32>(2)?,
                ))
            })
            .unwrap();
        let bindings: Vec<_> = records
            .map(|record| {
                let (association_id, file_id, part) = record.unwrap();
                let snapshot = media_mutation::snapshot(&self.store, "default", &file_id).unwrap();
                VersionBinding {
                    binding_id: format!("binding-{owner}-{part}"),
                    association_id,
                    file: ExactFileRevision {
                        file_id,
                        revision: public_components::revision(&snapshot).unwrap(),
                    },
                    part,
                    source: SourceExtent::Slice {
                        interval: interval(start, start + MINUTE),
                    },
                    timeline_interval: Some(interval(
                        i64::from(part - 1) * MINUTE,
                        i64::from(part) * MINUTE,
                    )),
                }
            })
            .collect();
        VersionContract {
            contract_version: 1,
            version_id: format!("version-{owner}"),
            association_item_id: owner.into(),
            edition_item_id: "edition".into(),
            timeline_id: format!("timeline-{owner}"),
            state: VersionState::Ready,
            declared_coverage: Some(interval(0, bindings.len() as i64 * MINUTE)),
            bindings,
        }
    }

    fn mutation(&self, versions: Vec<VersionContract>) -> IdentityMutation {
        IdentityMutation {
            contract_version: 1,
            profile: "default".into(),
            context: ContextChange {
                id: "context-main".into(),
                authority_namespace: "identity-test".into(),
                context_key: "editorial-main".into(),
                expected_revision: None,
            },
            timelines: versions
                .iter()
                .map(|v| TimelineChange {
                    timeline_id: v.timeline_id.clone(),
                    edition_item_id: v.edition_item_id.clone(),
                    expected_revision: None,
                    identity_state: TimelineState::Provisional,
                    coverage: v.declared_coverage,
                })
                .collect(),
            versions: versions
                .into_iter()
                .map(|version| VersionChange {
                    expected_revision: None,
                    expected_timeline_revision: 1,
                    expected_binding_revisions: version
                        .bindings
                        .iter()
                        .map(|b| (b.binding_id.clone(), None))
                        .collect(),
                    version,
                    equivalence: None,
                })
                .collect(),
            evidence: vec![],
        }
    }

    fn associations(&self) -> Value {
        serde_json::to_value(
            migration::rows(&self.store.db, "SELECT * FROM item_files ORDER BY id", &[]).unwrap(),
        )
        .unwrap()
    }

    fn read_policy(&self) -> String {
        let primary = definitions::query_put(&self.store, "default", "identity-primary", &json!({
        "selection":{"language":"sql","query":"SELECT id AS association_id FROM item_files WHERE role='primary' AND active=1"}
    })).unwrap()["id"].as_str().unwrap().to_owned();
        let audio = definitions::query_put(&self.store, "default", "identity-audio", &json!({
        "selection":{"language":"sql","query":"SELECT occurrence_id FROM catalog_component_occurrences WHERE profile=:profile AND kind='audio' AND current=1"}
    })).unwrap()["id"].as_str().unwrap().to_owned();
        fallback_mutation::put(&self.store, "default", "identity-policy", &json!({
        "fallbacks":[{"name":"primary","query_id":primary}],
        "within_tier":{"tie_break":"file_id"},"failback":{"mode":"immediate"},
        "package":{"publication":"selected_only","requirements":[{"name":"audio","fallbacks":[{"name":"all","query_id":audio}]}]}
    })).unwrap()["id"].as_str().unwrap().to_owned()
    }

    fn probe(&self, file: &str) {
        let snapshot = media_mutation::snapshot(&self.store, "default", file).unwrap();
        let data = json!({"format":{"duration":"120.0","format_name":"matroska"},
            "streams":[{"index":0,"codec_type":"video","codec_name":"h264","duration":"120.0"},{"index":1,"codec_type":"audio","codec_name":"aac","tags":{"language":"eng"}}]});
        let job = format!("probe-{file}");
        self.store.transaction(|db| {
            db.execute("INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state,result)
                VALUES (?,'default',?,'probe','media',?,'{}',?,'complete',?)",
                params![job, file, encode(&snapshot), job, encode(&data)])?;
            db.execute("INSERT INTO file_facts VALUES ('default',?,'probe',?,?,?,'complete')",
                params![file, job, encode(&snapshot), encode(&data)])?;
            public_components::call(&self.store, "default", "index_file", &json!({"file_id":file}))?;
            Ok(())
        }).unwrap();
    }
}

fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::from_secs(3600),
        max_receipts: 1000,
        max_logical_bytes: 16 * 1024 * 1024,
        max_hold_records: 1000,
    }
}

fn body(outcome: CommandOutcome) -> Value {
    match outcome {
        CommandOutcome::Committed { result, .. } | CommandOutcome::Replay { result, .. } => result,
        other => panic!("expected durable result, got {other:?}"),
    }
}

fn evidence_claims(mutation: &IdentityMutation) -> Vec<EquivalenceClaim> {
    mutation
        .versions
        .iter()
        .map(|v| EquivalenceClaim {
            edition_item_id: v.version.edition_item_id.clone(),
            timeline_id: v.version.timeline_id.clone(),
            timeline_revision: v.expected_timeline_revision,
            version_id: v.version.version_id.clone(),
            version_revision: v.expected_revision.unwrap_or(0) + 1,
            exact_files: v.version.bindings.iter().map(|b| b.file.clone()).collect(),
        })
        .collect()
}

fn revise(mutation: &IdentityMutation) -> IdentityMutation {
    let mut result = mutation.clone();
    result.context.expected_revision = Some(1);
    for timeline in &mut result.timelines {
        timeline.expected_revision = Some(1);
    }
    for version in &mut result.versions {
        version.expected_revision = Some(1);
        version.expected_timeline_revision = 2;
        for revision in version.expected_binding_revisions.values_mut() {
            *revision = Some(1);
        }
    }
    result.evidence.clear();
    result
}

#[test]
fn durable_create_and_revise_retain_ids_history_exact_snapshots_and_legacy_owners() {
    let fixture = Fixture::new();
    let associations = fixture.associations();
    assert_eq!(
        fixture.count("catalog_versions"),
        0,
        "metadata labels must not claim identity"
    );
    let original = fixture.mutation(vec![fixture.version("one", 0)]);
    let command = fixture.command("create", &original);
    let created = body(fixture.execute(&command).unwrap());
    assert_eq!(created["context_revision"], 1);
    assert_eq!(created["versions"][0]["version_id"], "version-one");
    assert_eq!(
        created["versions"][0]["bindings"][0]["binding_id"],
        "binding-one-1"
    );
    let original_association = associations
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["item_id"] == "one")
        .unwrap();
    assert_eq!(
        &created["versions"][0]["bindings"][0]["original_association"],
        original_association
    );
    assert_eq!(
        created["versions"][0]["bindings"][0]["file"],
        serde_json::to_value(&original.versions[0].version.bindings[0].file).unwrap()
    );
    assert_eq!(
        created["versions"][0]["bindings"][0]["source"],
        serde_json::to_value(original.versions[0].version.bindings[0].source).unwrap()
    );
    let current = media_mutation::snapshot(&fixture.store, "default", "shared").unwrap();
    let (revision, retained): (String, String) = fixture
        .store
        .db
        .query_row(
            "SELECT revision,snapshot_json FROM catalog_exact_file_revisions",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(revision, public_components::revision(&current).unwrap());
    assert_eq!(serde_json::from_str::<Value>(&retained).unwrap(), current);
    let mut revised = revise(&original);
    revised.timelines[0].identity_state = TimelineState::Established;
    revised.versions[0].version.bindings[0].source = SourceExtent::Slice {
        interval: interval(MINUTE, MINUTE * 2),
    };
    let updated = fixture.commit("revise", &revised);
    assert_eq!(updated["context_revision"], 2);
    assert_eq!(updated["versions"][0]["revision"], 2);
    assert_eq!(updated["versions"][0]["bindings"][0]["binding_revision"], 2);
    for table in [
        "catalog_timeline_revisions",
        "catalog_version_revisions",
        "catalog_binding_revisions",
        "catalog_segmentation_revisions",
        "catalog_segment_members",
    ] {
        assert_eq!(fixture.count(table), 2, "{table}");
    }
    for table in [
        "catalog_timelines",
        "catalog_versions",
        "catalog_bindings",
        "catalog_exact_file_revisions",
        "catalog_segmentation_contexts",
        "catalog_segmentation_sets",
    ] {
        assert_eq!(fixture.count(table), 1, "{table}");
    }
    let bounds: Vec<(i64, i64)> = fixture
        .store
        .db
        .prepare(
            "SELECT revision,source_start_us
        FROM catalog_binding_revisions ORDER BY revision",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(bounds, [(1, 0), (2, MINUTE)]);
    assert_eq!(fixture.associations(), associations);
    assert_eq!(fixture.count("catalog_mutation_permits"), 0);
    let generation = fixture.generation();
    let Fixture {
        _root,
        path,
        store,
        authority,
    } = fixture;
    drop(store);
    let reopened = Store::open(&path, true, false).unwrap();
    let replay = catalog_commands::execute(
        &reopened,
        &command,
        TrustedCommandAuthority {
            host_namespace: "identity-test",
            receipt_visibility: ReceiptVisibility::FullResult,
            allow_new_commands: true,
            identity_authority: Some(&authority),
        },
        policy(),
    )
    .unwrap();
    assert_eq!(replay.code(), "replay");
    assert_eq!(body(replay), created);
    assert_eq!(
        reopened
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        generation
    );
    let loaded =
        catalog_identity_mutation::load_manifest(&reopened, "default", &["version-one".into()])
            .unwrap();
    assert_eq!(loaded.versions[0].version, revised.versions[0].version);
    drop(_root);
}

#[test]
fn every_expected_identity_head_is_compared_atomically() {
    let fixture = Fixture::new();
    let initial = fixture.mutation(vec![fixture.version("one", 0)]);
    fixture.commit("create", &initial);
    let valid = revise(&initial);
    for (index, expected) in ["context", "timeline", "version", "binding"]
        .into_iter()
        .enumerate()
    {
        let mut bad = valid.clone();
        match expected {
            "context" => bad.context.expected_revision = Some(9),
            "timeline" => bad.timelines[0].expected_revision = Some(9),
            "version" => bad.versions[0].expected_revision = Some(9),
            _ => {
                *bad.versions[0]
                    .expected_binding_revisions
                    .values_mut()
                    .next()
                    .unwrap() = Some(9)
            }
        }
        fixture.reject(
            &format!("bad-head-{index}"),
            &bad,
            &format!("catalog_identity_{expected}_revision_conflict"),
        );
    }
    fixture.commit("valid-revision", &valid);
    assert_eq!(fixture.count("catalog_version_revisions"), 2);
}

#[test]
fn coarse_generation_fences_identity_after_unrelated_legacy_write() {
    let fixture = Fixture::new();
    let mutation = fixture.mutation(vec![fixture.version("one", 0)]);
    let stale = fixture.command("stale", &mutation);
    curation::item_put(
        &fixture.store,
        &json!({"id":"unrelated","kind":"movie","metadata":{}}),
    )
    .unwrap();
    let generation = fixture.generation();
    assert_eq!(
        fixture.execute(&stale).unwrap(),
        CommandOutcome::StaleGeneration
    );
    assert_eq!(fixture.generation(), generation);
    assert_eq!(fixture.count("catalog_versions"), 0);
    assert_eq!(fixture.count("catalog_command_receipts"), 0);
    fixture.commit("fresh", &mutation);
}

#[test]
fn unchanged_members_are_included_in_overlap_checks_and_next_context_snapshot() {
    let fixture = Fixture::new();
    fixture.commit("first", &fixture.mutation(vec![fixture.version("one", 0)]));
    let mut second = fixture.mutation(vec![fixture.version("two", MINUTE - 1)]);
    second.context.expected_revision = Some(1);
    fixture.reject("overlap", &second, "catalog_identity_overlapping_coverage");
    second.versions[0].version.bindings[0].source = SourceExtent::Slice {
        interval: interval(MINUTE, 2 * MINUTE),
    };
    fixture.commit("adjacent", &second);
    let loaded = catalog_identity_mutation::load_manifest(
        &fixture.store,
        "default",
        &["version-two".into()],
    )
    .unwrap();
    assert_eq!(
        loaded.versions.len(),
        2,
        "loading one candidate retains its entire context"
    );
    assert_eq!(loaded.segmentation_sets.len(), 1);
    assert_eq!(loaded.segmentation_sets[0].segments.len(), 2);
    assert_eq!(
        fixture.count("catalog_segment_members"),
        3,
        "old snapshot plus both current members"
    );
    let members: i64 = fixture
        .store
        .db
        .query_row(
            "SELECT COUNT(*) FROM catalog_segment_members
        WHERE context_revision=2 AND binding_id='binding-one-1' AND binding_revision=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(members, 1);
}

#[test]
fn changing_context_or_supplying_another_set_name_cannot_escape_overlap_scope() {
    let fixture = Fixture::new();
    fixture.commit("first", &fixture.mutation(vec![fixture.version("one", 0)]));
    let mut second = fixture.mutation(vec![fixture.version("two", 0)]);
    second.context.id = "another-context".into();
    second.context.context_key = "unapproved-context".into();
    fixture.reject(
        "unapproved",
        &second,
        "catalog_identity_context_not_authorized",
    );
    second.context.context_key = "editorial-main".into();
    fixture.reject(
        "same-scope-new-id",
        &second,
        "catalog_identity_context_scope_mismatch",
    );
    let mut payload = json!({"operation":"identity","mutation":second});
    payload["mutation"]["segmentation_sets"] = json!([{"segmentation_set_id":"another-set"}]);
    let generation = fixture.generation();
    let error = fixture
        .execute(&fixture.envelope("invented-set", payload))
        .unwrap_err();
    assert_eq!(
        error.code,
        catalog_commands::CommandStoreErrorCode::InvalidOperation
    );
    assert_eq!(fixture.generation(), generation);
    assert_eq!(fixture.count("catalog_segmentation_contexts"), 1);
}

#[test]
fn equal_metadata_and_coverage_do_not_equate_independent_provisional_timelines() {
    let fixture = Fixture::new();
    let initial = fixture.mutation(vec![
        fixture.version("one", 0),
        fixture.version("alternative-owner", 0),
    ]);
    fixture.commit("independent", &initial);
    let loaded = catalog_identity_mutation::load_manifest(
        &fixture.store,
        "default",
        &["version-one".into()],
    )
    .unwrap();
    assert_eq!(loaded.versions.len(), 2);
    assert_ne!(
        loaded.versions[0].version.timeline_id,
        loaded.versions[1].version.timeline_id
    );
    assert!(loaded.versions.iter().all(|v| v.equivalence.is_none()));
    assert_eq!(fixture.count("catalog_identity_evidence"), 0);
    let mut rebind = revise(&initial);
    rebind.versions[1].version.timeline_id = rebind.versions[0].version.timeline_id.clone();
    rebind.timelines.clear();
    for version in &mut rebind.versions {
        version.expected_timeline_revision = 1;
    }
    fixture.reject(
        "implicit-rebind",
        &rebind,
        "catalog_identity_rebind_unsupported",
    );
}

#[test]
fn provisional_timeline_cannot_be_claimed_by_another_version() {
    let fixture = Fixture::new();
    let one = fixture.version("one", 0);
    let mut alternative = fixture.version("alternative-owner", 0);
    alternative.timeline_id = one.timeline_id.clone();
    let mut mutation = fixture.mutation(vec![one, alternative]);
    mutation.timelines.truncate(1);
    fixture.reject(
        "collision",
        &mutation,
        "catalog_identity_provisional_timeline_collision",
    );
    assert_eq!(fixture.count("catalog_timelines"), 0);
}

#[test]
fn alternatives_require_retained_authorized_evidence_and_complete_timeline_revisions() {
    let fixture = Fixture::new();
    let one = fixture.version("one", 0);
    let mut alternative = fixture.version("alternative-owner", 0);
    alternative.timeline_id = one.timeline_id.clone();
    let mut mutation = fixture.mutation(vec![one, alternative]);
    mutation.timelines.truncate(1);
    mutation.timelines[0].identity_state = TimelineState::Established;
    fixture.reject(
        "missing-evidence",
        &mutation,
        "catalog_identity_missing_equivalence_evidence",
    );
    mutation.evidence.push(IdentityEvidence { id: "editorial-declaration".into(), basis: EquivalenceBasis::Declared, claims: evidence_claims(&mutation),
        evidence: json!({"source":"fixture editorial comparison","scope":"same edition and coverage"}) });
    for version in &mut mutation.versions {
        version.equivalence = Some(EquivalenceEvidence {
            basis: EquivalenceBasis::Declared,
            evidence_id: "editorial-declaration".into(),
        });
    }
    fixture.commit("declared", &mutation);
    let loaded = catalog_identity_mutation::load_manifest(
        &fixture.store,
        "default",
        &["version-one".into()],
    )
    .unwrap();
    assert!(
        loaded
            .versions
            .iter()
            .all(|v| v.equivalence.as_ref().unwrap().evidence_id == "editorial-declaration")
    );
    assert_eq!(fixture.count("catalog_identity_evidence"), 1);
    let mut incomplete = revise(&mutation);
    incomplete.versions.truncate(1);
    fixture.reject(
        "partial-timeline",
        &incomplete,
        "catalog_identity_timeline_membership_incomplete",
    );
    let mut revised = revise(&mutation);
    fixture.reject(
        "stale-evidence",
        &revised,
        "catalog_identity_evidence_scope_mismatch",
    );
    revised.evidence.push(IdentityEvidence {
        id: "revised-declaration".into(),
        basis: EquivalenceBasis::Declared,
        claims: evidence_claims(&revised),
        evidence: json!({"source":"explicit revision comparison"}),
    });
    for version in &mut revised.versions {
        version.equivalence.as_mut().unwrap().evidence_id = "revised-declaration".into();
    }
    fixture.commit("all-timeline-members", &revised);
}

#[test]
fn untrusted_or_absent_evidence_authority_cannot_write_identity() {
    let fixture = Fixture::new();
    let mut mutation = fixture.mutation(vec![fixture.version("one", 0)]);
    mutation.timelines[0].identity_state = TimelineState::Established;
    mutation.evidence.push(IdentityEvidence {
        id: "verified".into(),
        basis: EquivalenceBasis::Verified,
        claims: evidence_claims(&mutation),
        evidence: json!({"test":"explicit evidence still requires trusted authority"}),
    });
    mutation.versions[0].equivalence = Some(EquivalenceEvidence {
        basis: EquivalenceBasis::Verified,
        evidence_id: "verified".into(),
    });
    fixture.reject(
        "verified-not-permitted",
        &mutation,
        "catalog_identity_evidence_not_authorized",
    );
    let mut authority = fixture.authority();
    authority.identity_authority = None;
    let error = catalog_commands::execute(
        &fixture.store,
        &fixture.command("no-authority", &mutation),
        authority,
        policy(),
    )
    .unwrap_err();
    assert_eq!(
        error.code,
        catalog_commands::CommandStoreErrorCode::AuthorityDenied
    );
    assert_eq!(fixture.count("catalog_identity_evidence"), 0);
    assert_eq!(fixture.count("catalog_versions"), 0);
}

#[test]
fn incomplete_owner_and_binding_revision_membership_are_rejected_without_partial_rows() {
    let fixture = Fixture::new();
    let full = fixture.mutation(vec![fixture.version("multipart", 0)]);
    let mut incomplete = full.clone();
    incomplete.versions[0].version.bindings.pop();
    incomplete.versions[0]
        .expected_binding_revisions
        .remove("binding-multipart-2");
    incomplete.versions[0].version.declared_coverage = Some(interval(0, MINUTE));
    incomplete.timelines[0].coverage = Some(interval(0, MINUTE));
    fixture.reject(
        "missing-part",
        &incomplete,
        "catalog_identity_binding_set_mismatch",
    );
    let mut missing_head = full.clone();
    missing_head.versions[0]
        .expected_binding_revisions
        .remove("binding-multipart-2");
    fixture.reject(
        "missing-head",
        &missing_head,
        "catalog_identity_binding_set_mismatch",
    );
    assert_eq!(fixture.count("catalog_binding_revisions"), 0);
    fixture.commit("complete", &full);
    assert_eq!(fixture.count("catalog_binding_revisions"), 2);
}

#[test]
fn malformed_interval_fields_and_evidence_leave_no_durable_state() {
    let fixture = Fixture::new();
    let mutation = fixture.mutation(vec![fixture.version("one", 0)]);
    let payload = json!({"operation":"identity","mutation":mutation});
    for (index, bad) in [
        {
            let mut p = payload.clone();
            p["mutation"]["versions"][0]["version"]["bindings"][0]["source"]["interval"]["end_us"] =
                json!(0);
            p
        },
        {
            let mut p = payload.clone();
            p["mutation"]["context"]["trusted"] = json!(true);
            p
        },
        {
            let mut p = payload.clone();
            p["mutation"]["versions"][0]["version"]["unexpected"] = json!(true);
            p
        },
    ]
    .into_iter()
    .enumerate()
    {
        let before = fixture.generation();
        let error = fixture
            .execute(&fixture.envelope(&format!("malformed-{index}"), bad))
            .unwrap_err();
        assert_eq!(
            error.code,
            catalog_commands::CommandStoreErrorCode::InvalidOperation
        );
        assert_eq!(fixture.generation(), before);
    }
    let mut bad = mutation;
    bad.evidence.push(IdentityEvidence {
        id: "empty".into(),
        basis: EquivalenceBasis::Declared,
        claims: evidence_claims(&bad),
        evidence: json!({}),
    });
    fixture.reject("empty-evidence", &bad, "catalog_identity_invalid_evidence");
    assert_eq!(fixture.count("catalog_command_receipts"), 0);
    assert_eq!(fixture.count("catalog_timelines"), 0);
}

#[test]
fn draft_with_unknown_duration_is_retained_without_invented_timing_or_readiness() {
    let fixture = Fixture::at_schema(39);
    let mut draft = fixture.version("one", 0);
    draft.state = VersionState::Draft;
    draft.declared_coverage = None;
    draft.bindings[0].source = SourceExtent::WholeFile { duration_us: None };
    draft.bindings[0].timeline_interval = None;
    fixture.commit("draft", &fixture.mutation(vec![draft.clone()]));
    let manifest = catalog_identity_mutation::load_manifest(
        &fixture.store,
        "default",
        &[draft.version_id.clone()],
    )
    .unwrap();
    assert_eq!(manifest.versions[0].version, draft);
    assert!(manifest.segmentation_sets.is_empty());
    let unmeasured: bool = fixture
        .store
        .db
        .query_row(
            "SELECT state='draft' AND coverage_start_us IS NULL
        AND coverage_end_us IS NULL FROM catalog_version_revisions",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(unmeasured);
    let no_bounds: bool = fixture
        .store
        .db
        .query_row(
            "SELECT source_start_us IS NULL AND source_end_us IS NULL
        AND timeline_start_us IS NULL AND timeline_end_us IS NULL FROM catalog_binding_revisions",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(no_bounds);
    // The real typed owner accepted unknown coverage above. An explicit later
    // disappearance can be described without inventing interval zeroes or
    // turning this draft into a playable version.
    fixture
        .store
        .db
        .execute(
            "UPDATE observations SET status='missing' WHERE profile='default' AND file_id='shared'",
            [],
        )
        .unwrap();
    let authority = catabolic_store::catalog_projection::TrustedProjectionAuthority {
        catalog_id: fixture.store.database_id.clone(),
        incarnation: fixture.store.catalog_incarnation.clone().unwrap(),
        expected_generation: fixture.generation(),
    };
    let unavailable = catabolic_store::catalog_projection::unavailable_version_facts(
        &fixture.store,
        &authority,
        "default",
        &[draft.version_id.clone()],
    )
    .unwrap();
    let binding = &unavailable["versions"][0]["bindings"][0];
    assert!(binding["timeline_start_us"].is_null());
    assert!(binding["timeline_end_us"].is_null());
    assert_eq!(binding["observation"]["state"], "missing");
    assert_eq!(
        fixture
            .store
            .db
            .query_row(
                "SELECT state FROM catalog_version_revisions WHERE version_id=? AND revision=1",
                [&draft.version_id],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "draft"
    );
}

#[test]
fn exact_component_revision_drift_refuses_new_claim_and_preserves_old_snapshot() {
    let fixture = Fixture::new();
    let mutation = fixture.mutation(vec![fixture.version("one", 0)]);
    fixture
        .store
        .db
        .execute(
            "UPDATE observations SET mtime_ns=mtime_ns+1 WHERE file_id='shared'",
            [],
        )
        .unwrap();
    fixture.reject(
        "stale-revision",
        &mutation,
        "catalog_identity_stale_file_revision",
    );
    let current = fixture.mutation(vec![fixture.version("one", 0)]);
    fixture.commit("fresh-revision", &current);
    let snapshot: String = fixture
        .store
        .db
        .query_row(
            "SELECT snapshot_json FROM catalog_exact_file_revisions",
            [],
            |r| r.get(0),
        )
        .unwrap();
    fixture
        .store
        .db
        .execute(
            "UPDATE observations SET size=size+1 WHERE file_id='shared'",
            [],
        )
        .unwrap();
    fixture.reject(
        "revise-stale-file",
        &revise(&current),
        "catalog_identity_stale_file_revision",
    );
    assert_eq!(
        fixture
            .store
            .db
            .query_row(
                "SELECT snapshot_json FROM catalog_exact_file_revisions",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        snapshot
    );
}

#[test]
fn claimed_associations_and_retained_identity_history_reject_legacy_sql_mutation() {
    let fixture = Fixture::new();
    fixture.commit("claim", &fixture.mutation(vec![fixture.version("one", 0)]));
    let associations = fixture.associations();
    for statement in [
        "UPDATE item_files SET item_id='two' WHERE item_id='one'",
        "UPDATE item_files SET active=0 WHERE item_id='one'",
        "DELETE FROM item_files WHERE item_id='one'",
        "UPDATE catalog_timelines SET edition_item_id='other-edition'",
        "DELETE FROM catalog_versions",
        "UPDATE catalog_binding_revisions SET source_start_us=1",
        "DELETE FROM catalog_version_revisions",
        "UPDATE catalog_exact_file_revisions SET snapshot_json='{}'",
        "DELETE FROM catalog_segment_members",
        "UPDATE catalog_segmentation_sets SET id='renamed-set'",
    ] {
        let generation = fixture.generation();
        assert!(
            fixture.store.db.execute(statement, []).is_err(),
            "must refuse: {statement}"
        );
        assert_eq!(
            fixture.generation(),
            generation,
            "failed statement changed generation: {statement}"
        );
    }
    assert_eq!(fixture.associations(), associations);
    assert_eq!(fixture.count("catalog_versions"), 1);
    assert_eq!(fixture.count("catalog_mutation_permits"), 0);
    // Ordinary, unclaimed curation is still owned by the old surface.
    fixture
        .store
        .db
        .execute(
            "UPDATE item_files SET metadata='{}' WHERE item_id='two'",
            [],
        )
        .unwrap();
}

#[test]
fn persisted_read_bridge_selects_exact_slice_without_claiming_publication_ready() {
    let fixture = Fixture::new();
    fixture.probe("shared");
    let policy_id = fixture.read_policy();
    let version = fixture.version("one", MINUTE);
    fixture.commit("slice", &fixture.mutation(vec![version.clone()]));
    let request = VersionRequest {
        edition_item_id: version.edition_item_id.clone(),
        timeline_id: version.timeline_id.clone(),
        coverage: version.declared_coverage.unwrap(),
        selection: VersionSelection::Explicit {
            version_id: version.version_id.clone(),
        },
        policy_id,
        catalog: "global".into(),
    };
    let retained = catalog_identity_mutation::load_manifest(
        &fixture.store,
        "default",
        std::slice::from_ref(&version.version_id),
    )
    .unwrap();
    assert_eq!(
        catalog_identity::validate_manifest(&fixture.store, "default", &retained)
            .unwrap_err()
            .0,
        "catalog_identity_authoritative_records_required"
    );
    let before = fixture.generation();
    let mut evaluation = Evaluation::new(fixture.store, "default", 60000, 100000, false).unwrap();
    let mut probe = |path: &std::path::Path,
                     profile: &str,
                     database: &str,
                     snapshots: &Value,
                     _| {
        fallback_resolution::probe_check(
            &json!({"database":path,"database_id":database,"profile":profile,"snapshots":snapshots}),
        )
    };
    let result =
        catalog_identity::resolve_persisted(&mut evaluation, &request, &mut probe).unwrap();
    assert_eq!(result.selected_version_id.as_deref(), Some("version-one"));
    assert_eq!(result.parts.len(), 1);
    assert_eq!(result.parts[0].binding, version.bindings[0]);
    assert!(result.parts[0].requires_interval_enforcement);
    assert!(!result.publication_ready);
    assert_eq!(
        evaluation
            .store()
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        before
    );
    let legacy = fallback_resolution::Resolver::new(
        fallback_resolution::Context::Local(&mut evaluation),
        &mut probe,
        None,
    )
    .resolve(
        &[json!({"item_id":"one","part":1})],
        &request.policy_id,
        "global",
        &BTreeMap::new(),
        false,
    );
    let legacy = legacy.unwrap();
    assert_eq!(legacy["decisions"][0]["state"], "blocked");
    assert!(legacy["decisions"][0]["file_id"].is_null());
    assert!(legacy["decisions"][0]["content_path"].is_null());
    assert_eq!(legacy["checks"], 0);
    assert_eq!(legacy["captures"], json!({}));
}

#[test]
fn retained_ctime_is_diagnostic_and_does_not_collide_with_component_revision_identity() {
    let fixture = Fixture::new();
    let mut snapshot = media_mutation::snapshot(&fixture.store, "default", "shared").unwrap();
    let revision = public_components::revision(&snapshot).unwrap();
    snapshot["ctime_ns"] = json!(123456789);
    assert_eq!(public_components::revision(&snapshot).unwrap(), revision);
    fixture
        .store
        .db
        .execute(
            "INSERT INTO catalog_exact_file_revisions VALUES ('default','shared',?,1,?,0)",
            params![revision, encode(&snapshot)],
        )
        .unwrap();
    fixture.commit(
        "same-revision-diagnostic-ctime",
        &fixture.mutation(vec![fixture.version("one", 0)]),
    );
    assert_eq!(fixture.count("catalog_exact_file_revisions"), 1);
    let retained: String = fixture
        .store
        .db
        .query_row(
            "SELECT snapshot_json FROM catalog_exact_file_revisions",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&retained).unwrap(), snapshot);
}

#[test]
fn single_ready_unshifted_whole_file_remains_readable_through_legacy_bridge() {
    let fixture = Fixture::new();
    fixture.probe("alternative");
    let policy_id = fixture.read_policy();
    let mut version = fixture.version("alternative-owner", 0);
    version.declared_coverage = Some(interval(0, 2 * MINUTE));
    version.bindings[0].source = SourceExtent::WholeFile {
        duration_us: Some(2 * MINUTE),
    };
    version.bindings[0].timeline_interval = version.declared_coverage;
    fixture.commit("whole-file", &fixture.mutation(vec![version.clone()]));
    let generation = fixture.generation();
    let mut evaluation = Evaluation::new(fixture.store, "default", 60000, 100000, false).unwrap();
    let mut probe = |path: &std::path::Path,
                     profile: &str,
                     database: &str,
                     snapshots: &Value,
                     _| {
        fallback_resolution::probe_check(
            &json!({"database":path,"database_id":database,"profile":profile,"snapshots":snapshots}),
        )
    };
    let legacy = fallback_resolution::Resolver::new(
        fallback_resolution::Context::Local(&mut evaluation),
        &mut probe,
        None,
    )
    .resolve(
        &[json!({"item_id":"alternative-owner","part":1})],
        &policy_id,
        "global",
        &BTreeMap::new(),
        false,
    )
    .unwrap();
    assert_eq!(
        legacy["decisions"][0]["state"], "resolved_preferred",
        "{legacy}"
    );
    assert_eq!(legacy["decisions"][0]["file_id"], "alternative");
    assert_eq!(
        legacy["decisions"][0]["revision"],
        version.bindings[0].file.revision
    );
    assert!(legacy["decisions"][0]["content_path"].is_string());
    assert_eq!(
        evaluation
            .store()
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        generation
    );
}

#[test]
fn retained_ids_cannot_rehome_a_timeline_version_or_binding_to_another_owner() {
    let fixture = Fixture::new();
    let original = fixture.mutation(vec![fixture.version("one", 0)]);
    fixture.commit("original-owner", &original);
    let associations = fixture.associations();
    let mut timeline = revise(&original);
    timeline.timelines[0].edition_item_id = "other-edition".into();
    timeline.versions[0].version.edition_item_id = "other-edition".into();
    fixture.reject(
        "move-timeline-owner",
        &timeline,
        "catalog_identity_timeline_owner_immutable",
    );
    let mut version = revise(&original);
    version.timelines.clear();
    version.versions[0].expected_timeline_revision = 1;
    version.versions[0].version.association_item_id = "two".into();
    fixture.reject(
        "move-version-owner",
        &version,
        "catalog_identity_rebind_unsupported",
    );
    let mut binding = fixture.mutation(vec![fixture.version("two", MINUTE)]);
    binding.context.expected_revision = Some(1);
    binding.versions[0].version.bindings[0].binding_id = "binding-one-1".into();
    binding.versions[0].expected_binding_revisions = [("binding-one-1".into(), Some(1))].into();
    fixture.reject(
        "move-binding-owner",
        &binding,
        "catalog_identity_binding_owner_immutable",
    );
    assert_eq!(fixture.associations(), associations);
    assert_eq!(fixture.count("catalog_timelines"), 1);
    assert_eq!(fixture.count("catalog_versions"), 1);
    assert_eq!(fixture.count("catalog_bindings"), 1);
}

#[test]
fn package_captured_before_native_slice_claim_cannot_be_used_for_later_effects() {
    let fixture = Fixture::new();
    fixture.probe("alternative");
    let occurrence_id: String = fixture.store.db.query_row(&format!(
        "SELECT occurrence_id FROM ({}) WHERE profile='default' AND item_id='alternative-owner' AND kind='video' AND current=1",
        catabolic_store::query::view_sql("catalog_component_occurrences")), [], |r| r.get(0)).unwrap();
    let occurrence =
        component_mutation::get(&fixture.store, "default", &occurrence_id, true).unwrap();
    let package = json!({"version":1,
        "components":[{"occurrence":occurrence,"compatibility":{"offset_seconds":0.0}}],
        "dependencies":[],"packaging":{"action":"mux","ready":false,"operation_id":"approved"}});
    let recipe = json!({"id":"approved","preset":"component-mux"});
    component_mutation::validate_package(&fixture.store, "default", &package, Some(&recipe), true)
        .unwrap();
    fixture.commit(
        "claim-after-package",
        &fixture.mutation(vec![fixture.version("alternative-owner", MINUTE)]),
    );
    assert_eq!(
        component_mutation::get(&fixture.store, "default", &occurrence_id, true).unwrap(),
        occurrence,
        "the native claim must be detected even when old component evidence is byte-for-byte unchanged"
    );
    assert_eq!(
        component_mutation::validate_package(
            &fixture.store,
            "default",
            &package,
            Some(&recipe),
            true
        )
        .unwrap_err()
        .0,
        "catalog_identity_legacy_binding_unsupported"
    );
}

#[test]
fn prepared_legacy_projection_is_stale_after_explicit_native_claim() {
    let mut fixture = Fixture::new();
    fixture
        .store
        .db
        .execute("INSERT INTO catalogs VALUES ('identity-projection')", [])
        .unwrap();
    fixture.probe("alternative");
    let policy_id = fixture.read_policy();
    let query = definitions::query_put(&fixture.store, "default", "identity-membership", &json!({
        "selection":{"language":"sql","query":"SELECT id AS item_id FROM items WHERE id='alternative-owner'"}
    })).unwrap()["id"].as_str().unwrap().to_owned();
    definitions::dispatch(&fixture.store, "default", "layout-put", &json!({
        "name":"identity-flat","value":{"version":1,"rules":[{"name":"all","path":"{item.id}/{file.name}"}]}
    })).unwrap();
    let mut evaluation = Evaluation::new(fixture.store, "default", 60000, 100000, false).unwrap();
    fallback_projection::bind(
        &mut evaluation,
        "identity-projection",
        &policy_id,
        &query,
        "identity-flat",
    )
    .unwrap();
    let mut probe = |path: &std::path::Path,
                     profile: &str,
                     database: &str,
                     snapshots: &Value,
                     _| {
        fallback_resolution::probe_check(
            &json!({"database":path,"database_id":database,"profile":profile,"snapshots":snapshots}),
        )
    };
    let prepared =
        fallback_projection::plan(&mut evaluation, "identity-projection", false, &mut probe)
            .unwrap();
    assert!(prepared.is_safe(), "{}", prepared.full_plan());
    assert_eq!(prepared.full_plan()["desired"].as_array().unwrap().len(), 1);
    let epoch = prepared.full_plan()["epoch"].as_i64().unwrap();
    fixture.store = evaluation.into_store();
    fallback_resolution::require_epoch(&fixture.store, epoch).unwrap();
    fixture.commit(
        "claim-after-projection",
        &fixture.mutation(vec![fixture.version("alternative-owner", MINUTE)]),
    );
    assert_eq!(
        fallback_resolution::epoch(&fixture.store).unwrap(),
        epoch + 1
    );
    assert_eq!(
        fallback_resolution::require_epoch(&fixture.store, epoch)
            .unwrap_err()
            .0,
        "stale_resolution_plan"
    );
    let error = fallback_projection::admit(&fixture.store, "default", prepared, 10, 10)
        .err()
        .expect("old prepared projection must be refused");
    assert_eq!(error.0, "stale_resolution_plan");
    assert_eq!(fixture.count("fallback_entries"), 0);
    assert_eq!(fixture.count("fallback_history"), 0);
}

#[test]
fn exhausted_legacy_epoch_rolls_back_identity_heads_history_permits_and_receipt() {
    let fixture = Fixture::new();
    fixture
        .store
        .db
        .execute(
            "UPDATE fallback_epoch SET generation=? WHERE id=1",
            [i64::MAX],
        )
        .unwrap();
    let associations = fixture.associations();
    fixture.reject(
        "exhausted-epoch",
        &fixture.mutation(vec![fixture.version("one", 0)]),
        "catalog_identity_legacy_epoch_exhausted",
    );
    assert_eq!(
        fallback_resolution::epoch(&fixture.store).unwrap(),
        i64::MAX
    );
    assert_eq!(fixture.associations(), associations);
    for table in [
        "catalog_timelines",
        "catalog_timeline_revisions",
        "catalog_versions",
        "catalog_version_revisions",
        "catalog_bindings",
        "catalog_binding_revisions",
        "catalog_exact_file_revisions",
        "catalog_segmentation_contexts",
        "catalog_segmentation_revisions",
        "catalog_segmentation_sets",
        "catalog_segment_members",
        "catalog_command_receipts",
        "catalog_mutation_permits",
    ] {
        assert_eq!(fixture.count(table), 0, "rolled back {table}");
    }
}

#[test]
fn projection_tracks_current_timeline_coverage_and_equivalence_under_exact_fence() {
    use catabolic_store::catalog_projection::{self, TrustedProjectionAuthority};
    let fixture = Fixture::new();
    let initial = fixture.mutation(vec![fixture.version("one", 0)]);
    fixture.commit("projection-initial", &initial);
    let grant = || TrustedProjectionAuthority {
        catalog_id: fixture.store.database_id.clone(),
        incarnation: fixture.store.catalog_incarnation.clone().unwrap(),
        expected_generation: fixture.generation(),
    };
    let old_grant = grant();
    let first = catalog_projection::version_manifest(
        &fixture.store,
        &old_grant,
        "default",
        &["version-one".into()],
    )
    .unwrap();
    assert_eq!(
        first["version_revisions"][0]["timeline_identity_state"],
        "provisional"
    );
    assert_eq!(
        first["version_revisions"][0]["timeline_coverage"]["end_us"],
        MINUTE
    );
    assert!(first["manifest"]["versions"][0]["equivalence"].is_null());
    let mut revised = revise(&initial);
    revised.timelines[0].identity_state = TimelineState::Established;
    revised.timelines[0].coverage = Some(interval(0, MINUTE / 2));
    revised.versions[0].version.declared_coverage = Some(interval(0, MINUTE / 2));
    revised.versions[0].version.bindings[0].source = SourceExtent::Slice {
        interval: interval(0, MINUTE / 2),
    };
    revised.versions[0].version.bindings[0].timeline_interval = Some(interval(0, MINUTE / 2));
    revised.evidence.push(IdentityEvidence {
        id: "projection-revised-evidence".into(),
        basis: EquivalenceBasis::Declared,
        claims: evidence_claims(&revised),
        evidence: json!({"source":"explicit fixture editorial revision"}),
    });
    revised.versions[0].equivalence = Some(EquivalenceEvidence {
        basis: EquivalenceBasis::Declared,
        evidence_id: "projection-revised-evidence".into(),
    });
    fixture.commit("projection-revised", &revised);
    assert!(
        catalog_projection::version_manifest(
            &fixture.store,
            &old_grant,
            "default",
            &["version-one".into()]
        )
        .is_err()
    );
    let current = grant();
    let result = catalog_projection::version_manifest(
        &fixture.store,
        &current,
        "default",
        &["version-one".into()],
    )
    .unwrap();
    assert_eq!(result["version_revisions"][0]["version_revision"], 2);
    assert_eq!(result["version_revisions"][0]["timeline_revision"], 2);
    assert_eq!(
        result["version_revisions"][0]["timeline_identity_state"],
        "established"
    );
    assert_eq!(
        result["version_revisions"][0]["timeline_coverage"]["end_us"],
        MINUTE / 2
    );
    assert_eq!(
        result["manifest"]["versions"][0]["equivalence"]["evidence_id"],
        "projection-revised-evidence"
    );
    let mut wrong = current.clone();
    wrong.incarnation = "wrong".into();
    assert!(
        catalog_projection::version_manifest(
            &fixture.store,
            &wrong,
            "default",
            &["version-one".into()]
        )
        .is_err()
    );
    // The durable owner trigger refuses a partial rollback to a historical
    // timeline head. Keep that protection active rather than disabling it just
    // to manufacture a corrupt state for the reader.
    let rejected = fixture
        .store
        .db
        .execute(
            "UPDATE catalog_timelines SET current_revision=1 WHERE id='timeline-one'",
            [],
        )
        .unwrap_err();
    assert!(
        rejected
            .to_string()
            .contains("catalog_revision_or_owner_conflict")
    );
    let still_current = catalog_projection::version_manifest(
        &fixture.store,
        &grant(),
        "default",
        &["version-one".into()],
    )
    .unwrap();
    assert_eq!(
        still_current["version_revisions"][0]["timeline_revision"],
        2
    );
    assert_eq!(
        still_current["version_revisions"][0]["timeline_coverage"]["end_us"],
        MINUTE / 2
    );
}

#[test]
fn complete_work_capture_retains_actual_typed_draft_and_missing_occurrence() {
    let fixture = Fixture::with_motion_graph(39, true);
    let mut draft = fixture.version("one", 0);
    draft.state = VersionState::Draft;
    draft.declared_coverage = None;
    draft.bindings[0].source = SourceExtent::WholeFile { duration_us: None };
    draft.bindings[0].timeline_interval = None;
    fixture.commit("capture-draft", &fixture.mutation(vec![draft]));
    let capture = || {
        catabolic_store::catalog_work_capture::capture(
            &fixture.store,
            &catabolic_store::catalog_projection::TrustedProjectionAuthority {
                catalog_id: fixture.store.database_id.clone(),
                incarnation: fixture.store.catalog_incarnation.clone().unwrap(),
                expected_generation: fixture.generation(),
            },
            "default",
            "work",
        )
        .unwrap()
    };
    let present = capture();
    assert_eq!(present.work_id(), "work");
    // The typed draft is real; explicit observation changes below are controlled
    // fixture facts, not a filesystem observation-owner qualification claim.
    fixture
        .store
        .db
        .execute(
            "UPDATE observations SET status='missing' WHERE profile='default' AND file_id='shared'",
            [],
        )
        .unwrap();
    let missing = capture();
    assert_ne!(present.dependencies(), missing.dependencies());
    assert!(
        serde_json::to_string(missing.dependencies())
            .unwrap()
            .contains("missing")
    );
    assert_eq!(fixture.store.db.query_row("SELECT state FROM catalog_version_revisions WHERE version_id='version-one' AND revision=1",[],|r|r.get::<_,String>(0)).unwrap(),"draft");
}

#[test]
fn named_lineage_tracks_real_relationship_delete_move_and_ambiguous_parents() {
    use catabolic_store::{
        catalog_commands::CatalogAuthority,
        catalog_projection::{self, TrustedProjectionAuthority, VersionLineageRequest},
        catalog_relationships::{
            AuthorizedRelationshipDelete, RelationshipAuthority, RelationshipEdge,
            RelationshipKindRule, RelationshipMutation,
        },
    };
    use std::collections::BTreeSet;
    let f = Fixture::with_motion_graph(40, true);
    f.commit("lineage-seed", &f.mutation(vec![f.version("one", 0)]));
    let request = VersionLineageRequest {
        work_item_id: "work".into(),
        edition_item_id: "edition".into(),
        version_item_id: "one".into(),
        version_id: "version-one".into(),
    };
    let grant = || TrustedProjectionAuthority {
        catalog_id: f.store.database_id.clone(),
        incarnation: f.store.catalog_incarnation.clone().unwrap(),
        expected_generation: f.generation(),
    };
    let read =
        || catalog_projection::version_lineage(&f.store, &grant(), &[request.clone()]).unwrap();
    let original = grant();
    assert_eq!(read()["rows"][0]["status"], "active");
    let (id,revision):(String,i64)=f.store.db.query_row("SELECT e.id,r.revision FROM item_relationships e JOIN catalog_entity_revisions r ON r.entity_kind='item_relationships' AND r.entity_key=json_array(e.id) WHERE e.source_id='work' AND e.target_id='edition' AND e.active=1",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    let edge = RelationshipEdge {
        source: "work".into(),
        target: "edition".into(),
        kind: "custom:motion_edition".into(),
    };
    let authority = RelationshipAuthority {
        host_namespace: "identity-test".into(),
        create_edges: BTreeSet::new(),
        kind_rules: BTreeMap::new(),
        delete_edges: BTreeMap::from([(
            id.clone(),
            AuthorizedRelationshipDelete {
                expected_revision: revision,
                edge: edge.clone(),
            },
        )]),
    };
    let command=f.envelope("lineage-delete",json!({"operation":"relationships","mutation":RelationshipMutation::Delete{id,expected_revision:revision,expected_edge:edge}}));
    let outcome = catalog_commands::execute_with_authority(
        &f.store,
        &command,
        CatalogAuthority::new(f.authority()).with_relationships(&authority),
        policy(),
    )
    .unwrap();
    assert_eq!(outcome.code(), "committed");
    assert_eq!(read()["rows"][0]["status"], "detached");
    assert!(catalog_projection::version_lineage(&f.store, &original, &[request.clone()]).is_err());
    curation::item_put(
        &f.store,
        &json!({"id":"other-work","kind":"video","metadata":{}}),
    )
    .unwrap();
    for (key, source, status) in [
        ("lineage-move", "other-work", "detached"),
        ("lineage-competing", "work", "ambiguous"),
    ] {
        let edge = RelationshipEdge {
            source: source.into(),
            target: "edition".into(),
            kind: "custom:motion_edition".into(),
        };
        let authority = RelationshipAuthority {
            host_namespace: "identity-test".into(),
            create_edges: BTreeSet::from([edge.clone()]),
            kind_rules: BTreeMap::from([(edge.kind.clone(), RelationshipKindRule::Acyclic)]),
            delete_edges: BTreeMap::new(),
        };
        let command=f.envelope(key,json!({"operation":"relationships","mutation":RelationshipMutation::Create{id:key.into(),source:edge.source,target:edge.target,kind:edge.kind,metadata:json!({})}}));
        assert_eq!(
            catalog_commands::execute_with_authority(
                &f.store,
                &command,
                CatalogAuthority::new(f.authority()).with_relationships(&authority),
                policy()
            )
            .unwrap()
            .code(),
            "committed"
        );
        assert_eq!(read()["rows"][0]["status"], status);
    }
    let mut wrong = grant();
    wrong.incarnation = "wrong".into();
    assert!(catalog_projection::version_lineage(&f.store, &wrong, &[request.clone()]).is_err());
    assert!(
        catalog_projection::version_lineage(
            &f.store,
            &grant(),
            &[request.clone(), request.clone()]
        )
        .is_err()
    );
    let mut missing = request;
    missing.version_id = "missing".into();
    assert!(catalog_projection::version_lineage(&f.store, &grant(), &[missing]).is_err());
}
