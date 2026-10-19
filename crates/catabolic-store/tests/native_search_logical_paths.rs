// Explicit schema35 populated logical-owner integration, separate from historical34.
//! Real schema34 catalogs and the existing durable dispatcher. No substitute
//! receipt engine; all replacement commits/replays use catalog_commands::execute.
use catabolic_core::{
    catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility},
    catalog_identity::{
        ExactFileRevision, IntervalUs, SourceExtent, VersionBinding, VersionContract, VersionState,
    },
};
use catabolic_store::{
    Store,
    catalog_commands::{
        self, CatalogAuthority, CommandOutcome, ReceiptPolicy, TrustedCommandAuthority,
    },
    catalog_identity_mutation::{
        self, ContextChange, IdentityAuthority, IdentityMutation, TimelineChange, TimelineState,
        VersionChange,
    },
    catalog_logical_edits::{
        self, AssociationClone, ContextReplacement, LogicalChange, LogicalEditAuthority,
        LogicalEditRequest, LogicalResolution, NewItem, VersionReplacement,
    },
    curation, media_mutation, migration, public_components,
};
use rusqlite::params;
use serde_json::{Value, json};
use std::{fs, os::unix::fs::MetadataExt, path::PathBuf, time::Duration};
const END: i64 = 60_000_000;
fn interval(a: i64, b: i64) -> IntervalUs {
    IntervalUs::new(a, b).unwrap()
}
fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::from_secs(3600),
        max_receipts: 1000,
        max_logical_bytes: 16 * 1024 * 1024,
        max_hold_records: 1000,
    }
}
fn result(outcome: CommandOutcome) -> Value {
    match outcome {
        CommandOutcome::Committed { result, .. } | CommandOutcome::Replay { result, .. } => result,
        other => panic!("{other:?}"),
    }
}
struct Fixture {
    _root: tempfile::TempDir,
    path: PathBuf,
    store: Store,
    identity: IdentityAuthority,
    logical: LogicalEditAuthority,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("logical.db");
        migration::initialize_native_at(&path, 35).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let source = root.path().join("media");
        fs::create_dir(&source).unwrap();
        let d = source.metadata().unwrap();
        let file = source.join("file.mkv");
        fs::write(&file, b"logical fixture").unwrap();
        let f = file.metadata().unwrap();
        store.db.execute_batch("INSERT INTO locations VALUES('media'); INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES('scan','default','media',1,1,'[]');INSERT INTO files VALUES('file','media','file.mkv');").unwrap();
        store
            .db
            .execute(
                "INSERT INTO bindings VALUES('default','source','media',?,?,?)",
                params![source.to_str(), d.dev() as i64, d.ino() as i64],
            )
            .unwrap();
        store
            .db
            .execute(
                "INSERT INTO observations VALUES('default','file',?,?,?,?,'present','scan')",
                params![
                    f.len() as i64,
                    f.mtime() * 1_000_000_000 + f.mtime_nsec(),
                    f.dev() as i64,
                    f.ino() as i64
                ],
            )
            .unwrap();
        for id in [
            "edition",
            "survivor",
            "other-edition",
            "old-owner",
            "unchanged-owner",
        ] {
            curation::item_put(
                &store,
                &json!({"id":id,"kind":"movie","metadata":{"title":id}}),
            )
            .unwrap();
        }
        curation::item_put(&store,&json!({"id":"edition","kind":"movie","metadata":{"title":"Source title","provider_fact":"must remain"},"identities":{"imdb":"old-provider-id"}})).unwrap();
        for id in ["old-owner", "unchanged-owner"] {
            curation::dispatch(&store,"default","associate",&json!({"file_id":"file","item_id":id,"role":"primary","part":1,"metadata":{"evidence":"original association"}})).unwrap();
        }
        let identity = IdentityAuthority {
            authority_namespace: "logical-test".into(),
            context_keys: ["main".into()].into(),
            allow_declared_evidence: true,
            allow_verified_evidence: false,
        };
        let logical = LogicalEditAuthority {
            host_namespace: "logical-test".into(),
            item_ids: [
                "edition",
                "survivor",
                "other-edition",
                "old-owner",
                "unchanged-owner",
                "left",
                "right",
                "new-owner",
                "left-owner",
                "right-owner",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            new_item_ids: ["left", "right", "new-owner", "left-owner", "right-owner"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        };
        Self {
            _root: root,
            path,
            store,
            identity,
            logical,
        }
    }
    fn generation(&self) -> i64 {
        self.store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap()
    }
    fn count(&self, table: &str) -> i64 {
        self.store
            .db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    fn envelope(&self, key: &str, payload: Value) -> CommandEnvelope {
        CommandEnvelope::new(
            1,
            CommandKey::new(&self.store.database_id, "logical-test", key).unwrap(),
            self.store.catalog_incarnation.clone().unwrap(),
            self.generation(),
            &payload.to_string(),
        )
        .unwrap()
    }
    fn authority(&self) -> CatalogAuthority<'_> {
        CatalogAuthority::new(TrustedCommandAuthority {
            host_namespace: "logical-test",
            receipt_visibility: ReceiptVisibility::FullResult,
            allow_new_commands: true,
            identity_authority: Some(&self.identity),
        })
        .with_logical_edits(&self.logical)
    }
    fn run(&self, c: &CommandEnvelope) -> catalog_commands::Result<CommandOutcome> {
        let result =
            catalog_commands::execute_with_authority(&self.store, c, self.authority(), policy());
        assert_eq!(self.count("catalog_search_dirty"), 0);
        assert!(
            !catabolic_store::catalog_search_index::status(&self.store)
                .unwrap()
                .ready
        );
        result
    }
    fn old_version(&self, owner: &str, edition: &str, start: i64) -> VersionContract {
        let association: String = self
            .store
            .db
            .query_row("SELECT id FROM item_files WHERE item_id=?", [owner], |r| {
                r.get(0)
            })
            .unwrap();
        let file = media_mutation::snapshot(&self.store, "default", "file").unwrap();
        VersionContract {
            contract_version: 1,
            version_id: format!("v-{owner}"),
            association_item_id: owner.into(),
            edition_item_id: edition.into(),
            timeline_id: format!("t-{owner}"),
            state: VersionState::Ready,
            declared_coverage: Some(interval(0, END)),
            bindings: vec![VersionBinding {
                binding_id: format!("b-{owner}"),
                association_id: association,
                file: ExactFileRevision {
                    file_id: "file".into(),
                    revision: public_components::revision(&file).unwrap(),
                },
                part: 1,
                source: SourceExtent::Slice {
                    interval: interval(start, start + END),
                },
                timeline_interval: Some(interval(0, END)),
            }],
        }
    }
    fn mutation(&self, versions: Vec<VersionContract>, revision: Option<i64>) -> IdentityMutation {
        IdentityMutation {
            contract_version: 1,
            profile: "default".into(),
            context: ContextChange {
                id: "context".into(),
                authority_namespace: "logical-test".into(),
                context_key: "main".into(),
                expected_revision: revision,
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
                .map(|v| VersionChange {
                    expected_revision: None,
                    expected_timeline_revision: 1,
                    expected_binding_revisions: v
                        .bindings
                        .iter()
                        .map(|b| (b.binding_id.clone(), None))
                        .collect(),
                    version: v,
                    equivalence: None,
                })
                .collect(),
            evidence: vec![],
        }
    }
    fn seed(&self) {
        let m = self.mutation(
            vec![
                self.old_version("old-owner", "edition", 0),
                self.old_version("unchanged-owner", "other-edition", END),
            ],
            None,
        );
        result(
            self.run(&self.envelope("seed", json!({"operation":"identity","mutation":m})))
                .unwrap(),
        );
    }
    fn request(&self, split: bool) -> LogicalEditRequest {
        let original = self.old_version("old-owner", "edition", 0);
        let mut versions = vec![];
        let mut new_items = vec![];
        let mut clones = vec![];
        let children = if split {
            vec![
                ("left", "left-owner", 0, END / 2),
                ("right", "right-owner", END / 2, END),
            ]
        } else {
            vec![("survivor", "new-owner", 0, END)]
        };
        for (edition, owner, a, b) in children {
            if split {
                new_items.push(NewItem {
                    id: edition.into(),
                    kind: "movie".into(),
                });
            }
            new_items.push(NewItem {
                id: owner.into(),
                kind: "movie".into(),
            });
            let association = format!("a-{owner}");
            clones.push(AssociationClone {
                source_association_id: original.bindings[0].association_id.clone(),
                new_association_id: association.clone(),
                new_owner_item_id: owner.into(),
            });
            let mut v = original.clone();
            v.version_id = format!("v-{owner}");
            v.association_item_id = owner.into();
            v.edition_item_id = edition.into();
            v.timeline_id = format!("t-{owner}");
            v.declared_coverage = Some(interval(0, b - a));
            v.bindings[0].binding_id = format!("b-{owner}");
            v.bindings[0].association_id = association;
            v.bindings[0].source = SourceExtent::Slice {
                interval: interval(a, b),
            };
            v.bindings[0].timeline_interval = Some(interval(0, b - a));
            versions.push(v);
        }
        let ids = versions.iter().map(|v| v.version_id.clone()).collect();
        LogicalEditRequest {
            contract_version: 1,
            change: if split {
                LogicalChange::Split {
                    source: "edition".into(),
                    children: vec!["left".into(), "right".into()],
                }
            } else {
                LogicalChange::Merge {
                    sources: vec!["edition".into()],
                    survivor: "survivor".into(),
                }
            },
            new_items,
            association_clones: clones,
            contexts: vec![ContextReplacement {
                retired_versions: vec![VersionReplacement {
                    version_id: original.version_id,
                    expected_revision: 1,
                    replacement_version_ids: ids,
                }],
                mutation: self.mutation(versions, Some(1)),
            }],
        }
    }
    fn preview(&self, split: bool) -> catalog_logical_edits::LogicalEditPreview {
        catalog_logical_edits::preview_logical_edit(
            &self.store,
            self.request(split),
            &self.logical,
            Some(&self.identity),
        )
        .unwrap()
    }
    fn command(&self, key: &str, split: bool) -> CommandEnvelope {
        self.envelope(
            key,
            json!({"operation":"logical_edit","reviewed":self.preview(split).reviewed}),
        )
    }
}

#[test]
fn populated_merge_preserves_history_and_replays_original_result() {
    let f = Fixture::new();
    f.seed();
    let before = f.generation();
    let preview = f.preview(false);
    assert_eq!(f.generation(), before);
    assert_eq!(f.count("catalog_logical_retirements"), 0);
    assert!(!preview.reviewed.reference_map.implies_timeline_equivalence);
    let command = f.command("merge", false);
    let first = result(f.run(&command).unwrap());
    assert_eq!(f.count("catalog_version_retirements"), 1);
    assert_eq!(f.count("catalog_logical_retirements"), 1);
    assert_eq!(f.count("catalog_live_versions"), 2);
    assert_eq!(f.count("catalog_versions"), 3);
    assert_eq!(f.count("catalog_logical_edit_permits"), 0);
    assert_eq!(
        f.store
            .db
            .query_row(
                "SELECT edition_item_id FROM catalog_timelines WHERE id='t-old-owner'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "edition"
    );
    assert_eq!(f.store.db.query_row("SELECT state FROM catalog_version_revisions WHERE version_id='v-old-owner' AND revision=1",[],|r|r.get::<_,String>(0)).unwrap(),"ready");
    assert_eq!(
        f.store
            .db
            .query_row(
                "SELECT item_id FROM identities WHERE value='old-provider-id'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "edition"
    );
    assert!(
        f.store
            .db
            .query_row("SELECT metadata FROM items WHERE id='edition'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap()
            .contains("must remain")
    );
    assert_eq!(
        catalog_logical_edits::resolve_logical_item(&f.store, "edition", &f.logical).unwrap(),
        LogicalResolution::Merged {
            historical_item_id: "edition".into(),
            item_id: "survivor".into()
        }
    );
    assert!(
        catalog_logical_edits::resolve_logical_item(&f.store, "old-owner", &f.logical).is_err()
    );
    let manifest =
        catalog_identity_mutation::load_manifest(&f.store, "default", &["v-new-owner".into()])
            .unwrap();
    assert_eq!(manifest.versions.len(), 2);
    assert!(
        manifest
            .versions
            .iter()
            .any(|v| v.version.version_id == "v-unchanged-owner")
    );
    assert!(
        catalog_identity_mutation::load_manifest(&f.store, "default", &["v-old-owner".into()])
            .is_err()
    );
    let generation = f.generation();
    assert_eq!(result(f.run(&command).unwrap()), first);
    assert_eq!(f.generation(), generation);
    let Fixture {
        _root,
        path,
        store,
        identity,
        logical,
    } = f;
    drop(store);
    let store = Store::open(&path, true, false).unwrap();
    let replay = catalog_commands::execute_with_authority(
        &store,
        &command,
        CatalogAuthority::new(TrustedCommandAuthority {
            host_namespace: "logical-test",
            receipt_visibility: ReceiptVisibility::FullResult,
            allow_new_commands: true,
            identity_authority: Some(&identity),
        })
        .with_logical_edits(&logical),
        policy(),
    )
    .unwrap();
    assert_eq!(replay.code(), "replay");
    assert_eq!(result(replay), first);
}
#[test]
fn populated_split_keeps_two_explicit_children_and_no_redirect() {
    let f = Fixture::new();
    f.seed();
    let body = result(f.run(&f.command("split", true)).unwrap());
    assert_eq!(f.count("catalog_logical_aliases"), 0);
    assert_eq!(f.count("catalog_live_versions"), 3);
    assert_eq!(
        catalog_logical_edits::resolve_logical_item(&f.store, "edition", &f.logical).unwrap(),
        LogicalResolution::Split {
            historical_item_id: "edition".into(),
            children: vec!["left".into(), "right".into()]
        }
    );
    let entries = body["reference_map"]["entries"].as_array().unwrap();
    let source = entries
        .iter()
        .find(|e| e["source"]["kind"] == "version" && e["source"]["id"] == "v-old-owner")
        .unwrap();
    assert_eq!(source["replacements"].as_array().unwrap().len(), 2);
    assert_eq!(body["reference_map"]["implies_timeline_equivalence"], false);
    let manifest =
        catalog_identity_mutation::load_manifest(&f.store, "default", &["v-left-owner".into()])
            .unwrap();
    assert_eq!(manifest.segmentation_sets[0].segments.len(), 3);
}
#[test]
fn late_failure_rolls_back_every_owner_and_receipt() {
    let f = Fixture::new();
    f.seed();
    let command = f.command("late-failure", true);
    let generation = f.generation();
    let tables = [
        "items",
        "item_files",
        "catalog_versions",
        "catalog_version_revisions",
        "catalog_version_retirements",
        "catalog_logical_retirements",
        "catalog_segment_members",
        "catalog_command_receipts",
    ];
    let before: Vec<_> = tables.iter().map(|t| f.count(t)).collect();
    f.store.db.execute_batch("CREATE TRIGGER injected_logical_failure BEFORE INSERT ON catalog_logical_retirements BEGIN SELECT RAISE(ABORT,'late logical failure');END;").unwrap();
    assert!(
        f.run(&command)
            .unwrap_err()
            .error
            .0
            .contains("late logical failure")
    );
    assert_eq!(f.generation(), generation);
    assert_eq!(
        tables.iter().map(|t| f.count(t)).collect::<Vec<_>>(),
        before
    );
    assert_eq!(f.count("catalog_logical_edit_permits"), 0);
    assert_eq!(f.count("catalog_mutation_permits"), 0);
}
#[test]
fn stale_phantom_and_mutated_review_are_refused() {
    let f = Fixture::new();
    f.seed();
    let command = f.command("stale", false);
    curation::item_put(
        &f.store,
        &json!({"id":"unrelated","kind":"movie","metadata":{}}),
    )
    .unwrap();
    assert_eq!(f.run(&command).unwrap().code(), "stale_generation");
    let mut reviewed = f.preview(false).reviewed;
    reviewed.affected_digest = "0".repeat(64);
    let error = f
        .run(&f.envelope(
            "tamper",
            json!({"operation":"logical_edit","reviewed":reviewed}),
        ))
        .unwrap_err();
    assert!(error.error.0.contains("stale_review"));
    assert_eq!(f.count("catalog_version_retirements"), 0);
}
#[test]
fn source_membership_coverage_authority_and_context_spoof_are_refused() {
    let f = Fixture::new();
    f.seed();
    let mut request = f.request(true);
    request.contexts[0].mutation.versions[1].version.bindings[0].source = SourceExtent::Slice {
        interval: interval(END / 2 + 1, END + 1),
    };
    assert!(
        catalog_logical_edits::preview_logical_edit(
            &f.store,
            request,
            &f.logical,
            Some(&f.identity)
        )
        .unwrap_err()
        .error
        .0
        .contains("nonpartitioning")
    );
    let mut request = f.request(false);
    request.contexts[0].retired_versions.clear();
    assert!(
        catalog_logical_edits::preview_logical_edit(
            &f.store,
            request,
            &f.logical,
            Some(&f.identity)
        )
        .is_err()
    );
    let mut authority = f.logical.clone();
    authority.item_ids.remove("unchanged-owner");
    assert_eq!(
        catalog_logical_edits::preview_logical_edit(
            &f.store,
            f.request(false),
            &authority,
            Some(&f.identity)
        )
        .unwrap_err()
        .code,
        catalog_logical_edits::LogicalEditErrorCode::AuthorityDenied
    );
    let mut identity = f.identity.clone();
    identity.context_keys.insert("different".into());
    let mut request = f.request(false);
    request.contexts[0].mutation.context.context_key = "different".into();
    assert_eq!(
        catalog_logical_edits::preview_logical_edit(&f.store, request, &f.logical, Some(&identity))
            .unwrap_err()
            .code,
        catalog_logical_edits::LogicalEditErrorCode::AuthorityDenied
    );
}
#[test]
fn direct_legacy_and_native_resurrection_fail_closed() {
    let f = Fixture::new();
    f.seed();
    result(f.run(&f.command("merge", false)).unwrap());
    assert!(
        curation::item_put(
            &f.store,
            &json!({"id":"edition","kind":"movie","metadata":{}})
        )
        .is_err()
    );
    assert!(
        curation::item_put(
            &f.store,
            &json!({"kind":"movie","metadata":{},"identities":{"imdb":"old-provider-id"}})
        )
        .is_err()
    );
    assert!(
        curation::dispatch(
            &f.store,
            "default",
            "associate",
            &json!({"file_id":"file","item_id":"edition","role":"cover"})
        )
        .is_err()
    );
    assert!(
        f.store
            .db
            .execute("DELETE FROM catalog_version_retirements", [])
            .is_err()
    );
    let m = f.mutation(vec![f.old_version("old-owner", "edition", 0)], Some(2));
    assert!(
        f.run(&f.envelope("resurrect", json!({"operation":"identity","mutation":m})))
            .is_err()
    );
}
#[test]
fn empty_graph_split_and_native_schema33_availability() {
    let f = Fixture::new();
    let request = LogicalEditRequest {
        contract_version: 1,
        change: LogicalChange::Split {
            source: "edition".into(),
            children: vec!["left".into(), "right".into()],
        },
        new_items: vec![
            NewItem {
                id: "left".into(),
                kind: "movie".into(),
            },
            NewItem {
                id: "right".into(),
                kind: "movie".into(),
            },
        ],
        association_clones: vec![],
        contexts: vec![],
    };
    let reviewed =
        catalog_logical_edits::preview_logical_edit(&f.store, request.clone(), &f.logical, None)
            .unwrap()
            .reviewed;
    result(
        f.run(&f.envelope(
            "empty-split",
            json!({"operation":"logical_edit","reviewed":reviewed}),
        ))
        .unwrap(),
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("schema33.db");
    migration::initialize_native_at(&path, 33).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    assert_eq!(
        catalog_logical_edits::preview_logical_edit(&store, request, &f.logical, None)
            .unwrap_err()
            .code,
        catalog_logical_edits::LogicalEditErrorCode::Unsupported
    );
    let generation = store
        .db
        .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
    let command = CommandEnvelope::new(
        1,
        CommandKey::new(&store.database_id, "logical-test", "legacy33").unwrap(),
        store.catalog_incarnation.clone().unwrap(),
        generation,
        &json!({"operation":"item_put","item":{"id":"schema33-item","kind":"movie","metadata":{}}})
            .to_string(),
    )
    .unwrap();
    assert_eq!(
        catalog_commands::execute(
            &store,
            &command,
            TrustedCommandAuthority {
                host_namespace: "logical-test",
                receipt_visibility: ReceiptVisibility::FullResult,
                allow_new_commands: true,
                identity_authority: None
            },
            policy()
        )
        .unwrap()
        .code(),
        "committed"
    );
}

#[test]
fn retired_mapping_and_expected_set_writes_fail_without_relabeling_history() {
    let f = Fixture::new();
    f.store.db.execute("INSERT INTO mappings(id,catalog,file_id,item_id,path,active) VALUES('historical','global','file','edition','historical.mkv',0)",[]).unwrap();
    f.seed();
    result(f.run(&f.command("merge-guards", false)).unwrap());
    for sql in [
        "INSERT INTO mappings(id,catalog,file_id,item_id,path,active) VALUES('new-history','global','file','edition','new-history.mkv',1)",
        "UPDATE mappings SET active=1 WHERE id='historical'",
        "DELETE FROM mappings WHERE id='historical'",
        "INSERT INTO expected_sets(id,collection_id,source,edition,ordering,complete,members) VALUES('new-set','edition','reviewed','default','sequential',1,'[]')",
    ] {
        let error = f.store.db.execute(sql, []).unwrap_err();
        assert!(
            error.to_string().contains("catalog_logical_item_retired"),
            "{sql}: {error}"
        );
    }
    assert_eq!(
        f.store
            .db
            .query_row(
                "SELECT active FROM mappings WHERE id='historical'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    f.store.db.execute("INSERT INTO mappings(id,catalog,file_id,item_id,path,active) VALUES('new-active','global','file','survivor','active.mkv',1)",[]).unwrap();
    f.store
        .db
        .execute("DELETE FROM mappings WHERE id='new-active'", [])
        .unwrap();
    f.store.db.execute("INSERT INTO expected_sets(id,collection_id,source,edition,ordering,complete,members) VALUES('active-set','survivor','reviewed','default','sequential',1,'[]')",[]).unwrap();
    f.store
        .db
        .execute(
            "UPDATE expected_sets SET complete=0 WHERE id='active-set'",
            [],
        )
        .unwrap();
    f.store
        .db
        .execute("DELETE FROM expected_sets WHERE id='active-set'", [])
        .unwrap();
}

#[test]
fn newly_related_native_version_invalidates_review_and_requires_explicit_mapping() {
    let f = Fixture::new();
    f.seed();
    let command = f.command("before-phantom", false);
    curation::item_put(
        &f.store,
        &json!({"id":"phantom-owner","kind":"movie","metadata":{}}),
    )
    .unwrap();
    curation::dispatch(
        &f.store,
        "default",
        "associate",
        &json!({"file_id":"file","item_id":"phantom-owner","role":"primary","part":1}),
    )
    .unwrap();
    let mutation = f.mutation(
        vec![f.old_version("phantom-owner", "edition", END * 2)],
        Some(1),
    );
    result(
        f.run(&f.envelope(
            "new-related-version",
            json!({"operation":"identity","mutation":mutation}),
        ))
        .unwrap(),
    );
    assert_eq!(f.run(&command).unwrap().code(), "stale_generation");
    let mut request = f.request(false);
    request.contexts[0].mutation.context.expected_revision = Some(2);
    let mut logical = f.logical.clone();
    logical.item_ids.insert("phantom-owner".into());
    let error =
        catalog_logical_edits::preview_logical_edit(&f.store, request, &logical, Some(&f.identity))
            .unwrap_err();
    assert!(error.error.0.contains("incomplete_version_membership"));
    assert_eq!(f.count("catalog_version_retirements"), 0);
}

#[test]
fn public_reads_refuse_ambient_transactions_but_dispatcher_apply_remains_atomic() {
    let f = Fixture::new();
    f.seed();
    let command = f.command("ambient-after", false);
    f.store.db.execute_batch("BEGIN").unwrap();
    assert!(
        catalog_logical_edits::preview_logical_edit(
            &f.store,
            f.request(false),
            &f.logical,
            Some(&f.identity)
        )
        .unwrap_err()
        .error
        .0
        .contains("ambient_transaction")
    );
    assert!(
        catalog_logical_edits::resolve_logical_item(&f.store, "edition", &f.logical)
            .unwrap_err()
            .error
            .0
            .contains("ambient_transaction")
    );
    f.store.db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(f.run(&command).unwrap().code(), "committed");
}

#[test]
fn public_preview_and_resolution_detect_path_replacement_during_reads() {
    for resolve in [false, true] {
        let f = Fixture::new();
        f.seed();
        let replacement = f._root.path().join("replacement.db");
        migration::initialize_native(&replacement).unwrap();
        let original = f.path.clone();
        let request = f.request(false);
        let mut replaced = false;
        f.store
            .db
            .progress_handler(
                1,
                Some(move || {
                    if !replaced {
                        fs::rename(&replacement, &original).unwrap();
                        replaced = true;
                    }
                    false
                }),
            )
            .unwrap();
        let failed = if resolve {
            catalog_logical_edits::resolve_logical_item(&f.store, "edition", &f.logical).is_err()
        } else {
            catalog_logical_edits::preview_logical_edit(
                &f.store,
                request,
                &f.logical,
                Some(&f.identity),
            )
            .is_err()
        };
        f.store
            .db
            .progress_handler(0, None::<fn() -> bool>)
            .unwrap();
        assert!(
            failed,
            "public read must detect replaced pathname after SQL begins"
        );
    }
}

#[test]
fn survivor_versions_in_another_context_are_visible_and_require_exact_scope() {
    let mut f = Fixture::new();
    f.seed();
    curation::item_put(
        &f.store,
        &json!({"id":"survivor-owner","kind":"movie","metadata":{}}),
    )
    .unwrap();
    curation::dispatch(
        &f.store,
        "default",
        "associate",
        &json!({"file_id":"file","item_id":"survivor-owner","role":"primary","part":1}),
    )
    .unwrap();
    f.identity.context_keys.insert("survivor-scope".into());
    f.logical.item_ids.insert("survivor-owner".into());
    let mut mutation = f.mutation(
        vec![f.old_version("survivor-owner", "survivor", END * 2)],
        None,
    );
    mutation.context.id = "survivor-context".into();
    mutation.context.context_key = "survivor-scope".into();
    result(
        f.run(&f.envelope(
            "seed-survivor",
            json!({"operation":"identity","mutation":mutation}),
        ))
        .unwrap(),
    );
    let preview = f.preview(false);
    assert!(
        preview.affected_graph["catalog_versions"]
            .iter()
            .any(|v| v["id"] == "v-survivor-owner")
    );
    let mut authority = f.identity.clone();
    authority.context_keys.remove("survivor-scope");
    assert_eq!(
        catalog_logical_edits::preview_logical_edit(
            &f.store,
            f.request(false),
            &f.logical,
            Some(&authority)
        )
        .unwrap_err()
        .code,
        catalog_logical_edits::LogicalEditErrorCode::AuthorityDenied
    );
    result(f.run(&f.command("merge-survivor", false)).unwrap());
    assert_eq!(f.count("catalog_live_versions"), 3);
    assert_eq!(f.store.db.query_row("SELECT current_revision FROM catalog_segmentation_contexts WHERE id='survivor-context'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
}
