//! Actual closed-observation, curation and typed-identity owners. The controlled
//! host knows its complete two-part archive and supplies positive original-file
//! and sidecar custody audits; this does not qualify a production host adapter.
use super::*;
mod shared_initial;
use crate::{
    candidate41_test_support as candidate,
    catalog_commands::{
        self as commands, CatalogAuthority, CommandOutcome, ReceiptPolicy, TrustedCommandAuthority,
    },
    catalog_identity_mutation::{
        ContextChange, IdentityAuthority, IdentityMutation, TimelineChange, TimelineState,
        VersionChange,
    },
    catalog_logical_edits::{
        self as logical, LogicalChange, LogicalEditAuthority, LogicalEditRequest,
    },
    curation, migration,
};
use catabolic_core::{
    catalog_command::{CommandKey, ReceiptVisibility},
    catalog_host_lease::HostAuthorization,
    catalog_identity::{
        ExactFileRevision, IntervalUs, SourceExtent, VersionBinding, VersionContract, VersionState,
    },
    catalog_metadata::nfo_refresh::{
        CONTRACT_VERSION_V2, PinHead, SidecarObservation, SubjectState,
    },
};
use std::{
    cell::Cell,
    path::{Path, PathBuf},
    time::Duration,
};

pub(in crate::catalog_nfo_attribution::refresh) const HOST: &str = "nfo-observation-tests";
pub(in crate::catalog_nfo_attribution::refresh) const WORK: &str = "work";
const EDITION: &str = "edition";
const VERSION_ITEM: &str = "version-item";
const VERSION: &str = "native-version";
const CONTEXT: &str = "context";

pub(in crate::catalog_nfo_attribution::refresh) fn authority() -> TrustedCommandAuthority<'static> {
    TrustedCommandAuthority {
        host_namespace: HOST,
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: true,
        identity_authority: None,
    }
}
pub(in crate::catalog_nfo_attribution::refresh) fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::from_secs(3600),
        max_receipts: 100,
        max_hold_records: 100,
        max_logical_bytes: 16 * 1024 * 1024,
    }
}
pub(in crate::catalog_nfo_attribution::refresh) fn generation(store: &Store) -> i64 {
    store
        .db
        .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap()
}
pub(in crate::catalog_nfo_attribution::refresh) fn envelope(
    store: &Store,
    id: &str,
    payload: Value,
) -> CommandEnvelope {
    CommandEnvelope::new(
        1,
        CommandKey::new(&store.database_id, HOST, id).unwrap(),
        store.catalog_incarnation.as_deref().unwrap(),
        generation(store),
        &payload.to_string(),
    )
    .unwrap()
}
fn committed(outcome: CommandOutcome) {
    assert!(
        matches!(outcome, CommandOutcome::Committed { .. }),
        "{outcome:?}"
    );
}

pub(in crate::catalog_nfo_attribution::refresh) struct Fixture {
    _root: tempfile::TempDir,
    path: PathBuf,
    media: PathBuf,
    pub(in crate::catalog_nfo_attribution::refresh) store: Store,
    pub(in crate::catalog_nfo_attribution::refresh) creation: Option<CommandEnvelope>,
}
impl Fixture {
    pub(in crate::catalog_nfo_attribution::refresh) fn new(detached: bool) -> Self {
        Self::new_with_creation(detached, false)
    }
    pub(in crate::catalog_nfo_attribution::refresh) fn new_first() -> Self {
        Self::new_with_creation(false, true)
    }
    fn new_with_creation(detached: bool, fresh: bool) -> Self {
        Self::new_with_work(detached, fresh, None::<fn(&Store)>)
    }
    /// The callback must create WORK through the actual typed import owner.
    /// All physical and structural owners below remain the same real fixture.
    pub(in crate::catalog_nfo_attribution::refresh) fn new_imported(
        import_work: impl FnOnce(&Store),
    ) -> Self {
        Self::new_with_work(false, false, Some(import_work))
    }
    fn new_with_work(
        detached: bool,
        fresh: bool,
        import_work: Option<impl FnOnce(&Store)>,
    ) -> Self {
        Self::new_with_source_policy(detached, fresh, import_work, false)
    }
    fn new_relocation() -> Self {
        Self::new_with_source_policy(false, false, None::<fn(&Store)>, true)
    }
    fn new_with_source_policy(
        detached: bool,
        fresh: bool,
        mut import_work: Option<impl FnOnce(&Store)>,
        relocation: bool,
    ) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("observation.db");
        let media = root.path().join("media");
        std::fs::create_dir(&media).unwrap();
        for part in 1..=2 {
            let bytes = if relocation {
                // Real PCM WAVE media, independently scanned by the official
                // helper. Existing fixture defaults remain byte-for-byte unchanged.
                let samples = vec![part as u8; 16000];
                let mut wav = Vec::new();
                wav.extend_from_slice(b"RIFF");
                wav.extend_from_slice(&(36u32 + samples.len() as u32).to_le_bytes());
                wav.extend_from_slice(b"WAVEfmt ");
                wav.extend_from_slice(&16u32.to_le_bytes());
                wav.extend_from_slice(&1u16.to_le_bytes());
                wav.extend_from_slice(&1u16.to_le_bytes());
                wav.extend_from_slice(&8000u32.to_le_bytes());
                wav.extend_from_slice(&16000u32.to_le_bytes());
                wav.extend_from_slice(&2u16.to_le_bytes());
                wav.extend_from_slice(&16u16.to_le_bytes());
                wav.extend_from_slice(b"data");
                wav.extend_from_slice(&(samples.len() as u32).to_le_bytes());
                wav.extend_from_slice(&samples);
                wav
            } else {
                format!("original part {part}").into_bytes()
            };
            std::fs::write(media.join(format!("part-{part}.mkv")), bytes).unwrap();
        }
        migration::initialize_native_at(&path, 40).unwrap();
        closed_scan(&path, &media);
        let store = Store::open(&path, true, false).unwrap();
        if relocation {
            crate::observation::execute(
                &store,
                "default",
                "trust",
                &json!({"source":"media","overrides":{"uuid":"skip"},"apply":true}),
            )
            .unwrap();
        }
        let mut creation = None;
        for (id, kind) in [
            (WORK, "video"),
            ("survivor", "video"),
            (EDITION, "custom:motion_edition"),
            (VERSION_ITEM, "custom:motion_version"),
        ] {
            if id == WORK {
                if let Some(import_work) = import_work.take() {
                    import_work(&store);
                    continue;
                }
            }
            let payload = if fresh && id == WORK {
                json!({"operation":"item_create_batch","items":[{"id":id,"kind":kind,"metadata":{"title":"Original","motion_catalog":{"contract_version":1,"item_id":id,"kind":kind,"external_ids":[]}}}]})
            } else {
                json!({"operation":"item_put","item":{"id":id,"kind":kind,"metadata":{}}})
            };
            let command = envelope(&store, &format!("item-{id}"), payload);
            if fresh && id == WORK {
                creation = Some(command.clone());
            }
            committed(commands::execute(&store, &command, authority(), policy()).unwrap());
        }
        if !detached {
            curation::relate(
                &store,
                &json!({"source":WORK,"target":EDITION,"kind":"custom:motion_edition"}),
            )
            .unwrap();
        }
        curation::relate(
            &store,
            &json!({"source":EDITION,"target":VERSION_ITEM,"kind":"custom:motion_version"}),
        )
        .unwrap();
        let mut bindings = vec![];
        for part in 1..=2u32 {
            let file: String = store
                .db
                .query_row(
                    "SELECT id FROM files WHERE location='media' AND path=?",
                    [format!("part-{part}.mkv")],
                    |r| r.get(0),
                )
                .unwrap();
            curation::dispatch(&store,"default","associate",&json!({"file_id":file,"item_id":VERSION_ITEM,"role":"primary","part":part,"metadata":{}})).unwrap();
            let association: String = store
                .db
                .query_row(
                    "SELECT id FROM item_files WHERE item_id=? AND file_id=? AND active=1",
                    params![VERSION_ITEM, file],
                    |r| r.get(0),
                )
                .unwrap();
            let occurrence = crate::components::occurrence(&store.db, "default", &file).unwrap();
            bindings.push(VersionBinding {
                binding_id: format!("binding-{part}"),
                association_id: association,
                file: ExactFileRevision {
                    file_id: file,
                    revision: crate::components::revision(&occurrence),
                },
                part,
                source: SourceExtent::WholeFile {
                    duration_us: Some(1_000_000),
                },
                timeline_interval: Some(
                    IntervalUs::new(i64::from(part - 1) * 1_000_000, i64::from(part) * 1_000_000)
                        .unwrap(),
                ),
            });
        }
        let coverage = Some(IntervalUs::new(0, 2_000_000).unwrap());
        let version = VersionContract {
            contract_version: 1,
            version_id: VERSION.into(),
            association_item_id: VERSION_ITEM.into(),
            edition_item_id: EDITION.into(),
            timeline_id: "timeline".into(),
            state: VersionState::Ready,
            declared_coverage: coverage,
            bindings,
        };
        let mutation = IdentityMutation {
            contract_version: 1,
            profile: "default".into(),
            context: ContextChange {
                id: CONTEXT.into(),
                authority_namespace: HOST.into(),
                context_key: CONTEXT.into(),
                expected_revision: None,
            },
            timelines: vec![TimelineChange {
                timeline_id: "timeline".into(),
                edition_item_id: EDITION.into(),
                expected_revision: None,
                identity_state: TimelineState::Provisional,
                coverage,
            }],
            versions: vec![VersionChange {
                expected_revision: None,
                expected_timeline_revision: 1,
                expected_binding_revisions: version
                    .bindings
                    .iter()
                    .map(|b| (b.binding_id.clone(), None))
                    .collect(),
                version,
                equivalence: None,
            }],
            evidence: vec![],
        };
        let grant = IdentityAuthority {
            authority_namespace: HOST.into(),
            context_keys: [CONTEXT.into()].into(),
            allow_declared_evidence: false,
            allow_verified_evidence: false,
        };
        let command = envelope(
            &store,
            "identity",
            json!({"operation":"identity","mutation":mutation}),
        );
        committed(
            commands::execute(
                &store,
                &command,
                TrustedCommandAuthority {
                    identity_authority: Some(&grant),
                    ..authority()
                },
                policy(),
            )
            .unwrap(),
        );
        Self {
            _root: root,
            path,
            media,
            store,
            creation,
        }
    }
    /// Opt-in real unrelated evidence cohort; all old fixture defaults stay
    /// unchanged. Physical registrations/scans are genuine closed-helper40.
    fn with_unrelated_scanned_sources(mut self, count: usize) -> Self {
        assert_eq!(self.store.schema_version, 40);
        drop(self.store);
        for index in 0..count {
            let name = format!("unrelated-source-{index}");
            let root = self._root.path().join(&name);
            std::fs::create_dir(&root).unwrap();
            for part in 1..=2 {
                std::fs::copy(
                    self.media.join(format!("part-{part}.mkv")),
                    root.join(format!("part-{part}.mkv")),
                )
                .unwrap();
            }
            closed_scan_named(&self.path, &root, &name);
        }
        self.store = Store::open(&self.path, true, false).unwrap();
        for index in 0..count {
            let name = format!("unrelated-source-{index}");
            let store = &self.store;
            crate::observation::execute(
                store,
                "default",
                "trust",
                &json!({"source":name,"overrides":{"uuid":"skip"},"apply":true}),
            )
            .unwrap();
            let work = format!("{name}-work");
            let edition = format!("{name}-edition");
            let version_item = format!("{name}-version-item");
            let version_id = format!("{name}-version");
            let context = format!("{name}-context");
            let timeline = format!("{name}-timeline");
            let mut creation = None;
            let fresh = false;
            let detached = false;
            let mut import_work: Option<fn(&Store)> = None;
            for (id, kind) in [
                (work.as_str(), "video"),
                (edition.as_str(), "custom:motion_edition"),
                (version_item.as_str(), "custom:motion_version"),
            ] {
                if id == work.as_str() {
                    if let Some(import_work) = import_work.take() {
                        import_work(&store);
                        continue;
                    }
                }
                let payload = if fresh && id == work.as_str() {
                    json!({"operation":"item_create_batch","items":[{"id":id,"kind":kind,"metadata":{"title":"Original","motion_catalog":{"contract_version":1,"item_id":id,"kind":kind,"external_ids":[]}}}]})
                } else {
                    json!({"operation":"item_put","item":{"id":id,"kind":kind,"metadata":{}}})
                };
                let command = envelope(&store, &format!("item-{id}"), payload);
                if fresh && id == work.as_str() {
                    creation = Some(command.clone());
                }
                committed(commands::execute(&store, &command, authority(), policy()).unwrap());
            }
            if !detached {
                curation::relate(
                &store,
                &json!({"source":work.as_str(),"target":edition.as_str(),"kind":"custom:motion_edition"}),
            )
            .unwrap();
            }
            curation::relate(
            &store,
            &json!({"source":edition.as_str(),"target":version_item.as_str(),"kind":"custom:motion_version"}),
        )
        .unwrap();
            let mut bindings = vec![];
            for part in 1..=2u32 {
                let file: String = store
                    .db
                    .query_row(
                        "SELECT id FROM files WHERE location=? AND path=?",
                        params![name, format!("part-{part}.mkv")],
                        |r| r.get(0),
                    )
                    .unwrap();
                curation::dispatch(&store,"default","associate",&json!({"file_id":file,"item_id":version_item.as_str(),"role":"primary","part":part,"metadata":{}})).unwrap();
                let association: String = store
                    .db
                    .query_row(
                        "SELECT id FROM item_files WHERE item_id=? AND file_id=? AND active=1",
                        params![version_item.as_str(), file],
                        |r| r.get(0),
                    )
                    .unwrap();
                let occurrence =
                    crate::components::occurrence(&store.db, "default", &file).unwrap();
                bindings.push(VersionBinding {
                    binding_id: format!("{name}-binding-{part}"),
                    association_id: association,
                    file: ExactFileRevision {
                        file_id: file,
                        revision: crate::components::revision(&occurrence),
                    },
                    part,
                    source: SourceExtent::WholeFile {
                        duration_us: Some(1_000_000),
                    },
                    timeline_interval: Some(
                        IntervalUs::new(
                            i64::from(part - 1) * 1_000_000,
                            i64::from(part) * 1_000_000,
                        )
                        .unwrap(),
                    ),
                });
            }
            let coverage = Some(IntervalUs::new(0, 2_000_000).unwrap());
            let version = VersionContract {
                contract_version: 1,
                version_id: version_id.as_str().into(),
                association_item_id: version_item.as_str().into(),
                edition_item_id: edition.as_str().into(),
                timeline_id: timeline.clone(),
                state: VersionState::Ready,
                declared_coverage: coverage,
                bindings,
            };
            let mutation = IdentityMutation {
                contract_version: 1,
                profile: "default".into(),
                context: ContextChange {
                    id: context.as_str().into(),
                    authority_namespace: HOST.into(),
                    context_key: context.as_str().into(),
                    expected_revision: None,
                },
                timelines: vec![TimelineChange {
                    timeline_id: timeline.clone(),
                    edition_item_id: edition.as_str().into(),
                    expected_revision: None,
                    identity_state: TimelineState::Provisional,
                    coverage,
                }],
                versions: vec![VersionChange {
                    expected_revision: None,
                    expected_timeline_revision: 1,
                    expected_binding_revisions: version
                        .bindings
                        .iter()
                        .map(|b| (b.binding_id.clone(), None))
                        .collect(),
                    version,
                    equivalence: None,
                }],
                evidence: vec![],
            };
            let grant = IdentityAuthority {
                authority_namespace: HOST.into(),
                context_keys: [context.as_str().into()].into(),
                allow_declared_evidence: false,
                allow_verified_evidence: false,
            };
            let command = envelope(
                &store,
                &format!("{name}-identity"),
                json!({"operation":"identity","mutation":mutation}),
            );
            committed(
                commands::execute(
                    &store,
                    &command,
                    TrustedCommandAuthority {
                        identity_authority: Some(&grant),
                        ..authority()
                    },
                    policy(),
                )
                .unwrap(),
            );
            drop(creation);
        }
        self
    }
    pub(in crate::catalog_nfo_attribution::refresh) fn selection(&self) -> CurrentFileSelection {
        self.selection_part(1)
    }
    pub(in crate::catalog_nfo_attribution::refresh) fn selection_part(
        &self,
        part: u32,
    ) -> CurrentFileSelection {
        let binding_id = format!("binding-{part}");
        let (association,file):(String,String)=self.store.db.query_row("SELECT association_id,file_id FROM catalog_active_binding_revisions WHERE binding_id=?",[&binding_id],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        CurrentFileSelection {
            file: FileKey {
                profile: "default".into(),
                file_id: file,
            },
            order_key: format!("{part:04}"),
            lineage: VersionLineageRequest {
                work_item_id: WORK.into(),
                edition_item_id: EDITION.into(),
                version_item_id: VERSION_ITEM.into(),
                version_id: VERSION.into(),
            },
            binding_id,
            association_id: association,
        }
    }
    pub(in crate::catalog_nfo_attribution::refresh) fn media_path(&self) -> &Path {
        &self.media
    }
    pub(in crate::catalog_nfo_attribution::refresh) fn reopen_candidate(mut self) -> Self {
        assert_eq!(self.store.schema_version, 41);
        drop(self.store);
        self.store = candidate::open_candidate41_for_test(&self.path, true).unwrap();
        self
    }
    pub(in crate::catalog_nfo_attribution::refresh) fn remove_and_rescan(
        mut self,
        part: u32,
    ) -> Self {
        assert_eq!(self.store.schema_version, 40);
        std::fs::remove_file(self.media.join(format!("part-{part}.mkv"))).unwrap();
        drop(self.store);
        closed_scan(&self.path, &self.media);
        self.store = Store::open(&self.path, true, false).unwrap();
        self
    }
    pub(in crate::catalog_nfo_attribution::refresh) fn install_candidate(mut self) -> Self {
        drop(self.store);
        candidate::install_candidate41_for_test(&self.path).unwrap();
        self.store = candidate::open_candidate41_for_test(&self.path, true).unwrap();
        self
    }
    pub(in crate::catalog_nfo_attribution::refresh) fn retire(self) -> Self {
        // The logical owner correctly refuses active structural relationships.
        // Detach the fixture's exact edge through its owner before retiring;
        // retain the positive logical retirement record, not merely absence.
        let edge: String = self.store.db.query_row(
            "SELECT id FROM item_relationships WHERE source_id=? AND target_id=? AND kind='custom:motion_edition' AND active=1",
            params![WORK, EDITION], |r| r.get(0),
        ).unwrap();
        curation::dispatch(
            &self.store,
            "default",
            "relationship-disable",
            &json!({"id":edge}),
        )
        .unwrap();
        let grant = LogicalEditAuthority {
            host_namespace: HOST.into(),
            item_ids: [
                WORK.into(),
                "survivor".into(),
                EDITION.into(),
                VERSION_ITEM.into(),
            ]
            .into(),
            new_item_ids: BTreeSet::new(),
        };
        let preview = logical::preview_logical_edit(
            &self.store,
            LogicalEditRequest {
                contract_version: 1,
                change: LogicalChange::Merge {
                    sources: vec![WORK.into()],
                    survivor: "survivor".into(),
                },
                new_items: vec![],
                association_clones: vec![],
                contexts: vec![],
            },
            &grant,
            None,
        )
        .unwrap();
        let command = envelope(
            &self.store,
            "retire",
            json!({"operation":"logical_edit","reviewed":preview.reviewed}),
        );
        let outcome = if self.store.schema_version == 41 {
            commands::execute_candidate_for_test(
                &self.store,
                &command,
                authority(),
                policy(),
                |store, outer, time| {
                    logical::apply_logical_edit(store, &preview.reviewed, outer, &grant, None, time)
                        .map_err(|e| e.error)
                },
            )
        } else {
            commands::execute_with_authority(
                &self.store,
                &command,
                CatalogAuthority::new(authority()).with_logical_edits(&grant),
                policy(),
            )
        };
        committed(outcome.unwrap());
        let retired: bool = self
            .store
            .db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id=?)",
                [WORK],
                |r| r.get(0),
            )
            .unwrap();
        assert!(retired, "fixture must contain actual owner retirement");
        self
    }
    fn scope(&self) -> Fence {
        Fence {
            catalog_id: self.store.database_id.clone(),
            incarnation: self.store.catalog_incarnation.clone().unwrap(),
            generation: generation(&self.store),
        }
    }
    fn read<T>(&self, f: impl FnOnce(&Store, &mut ProofBudget) -> Result<T>) -> Result<T> {
        self.store.transaction(|_| {
            let mut budget = ProofBudget::new(self.store.catalog_operation_budget()?);
            let _source_read = crate::catalog_source_succession::current::enter_read(&self.store)?;
            f(&self.store, &mut budget)
        })
    }
}

// Exact qualified helper lifecycle, not direct Store scan state or fabricated
// ctime. Parent must supply its rebuilt non-harness closed helper executable.
fn closed_scan(path: &Path, media: &Path) {
    closed_scan_named(path, media, "media");
}
fn closed_scan_named(path: &Path, media: &Path, source: &str) {
    use catabolic_runtime::{
        helper,
        host::{CONTRACT_VERSION, HostSession},
    };
    let binary = PathBuf::from(
        std::env::var_os("CATABOLIC_HOST_TEST_HELPER")
            .expect("CATABOLIC_HOST_TEST_HELPER must be the qualified rebuilt non-harness helper"),
    );
    let artifact = helper::TrustedClosedObservationArtifact::after_packaging_verification(
        &binary,
        &catabolic_core::sha256(std::fs::read(&binary).unwrap()),
        helper::ClosedObservationProtocol::NoChildWithExactJoinV2,
    )
    .unwrap();
    let gate = HostAuthorization::new(
        HOST.into(),
        format!("observation-scan-{}", uuid::Uuid::new_v4()),
    )
    .unwrap();
    let mut host =
        HostSession::open_authorized(path, "default", true, CONTRACT_VERSION, gate.clone())
            .unwrap();
    host.register_source(source, media, &[]).unwrap();
    host.register_closed_observation_helper(&artifact).unwrap();
    let handle = host.start_scan(source, ()).unwrap();
    let revoked = gate.begin_revocation().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(report) = host.poll_scan(&handle, Duration::from_millis(50)).unwrap() {
            assert_eq!(report["complete"], true);
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "closed scan did not finish"
        );
    }
    revoked.wait_drained(Duration::ZERO).unwrap();
    host.close().unwrap();
    drop(host);
    assert!(
        crate::native_worker_inventory::inspect(&Store::open(path, true, false).unwrap())
            .unwrap()
            .admits_new_work()
    );
}

#[test]
fn available_selected_file_survives_unavailable_sibling() {
    let fixture = Fixture::new(false).remove_and_rescan(2).install_candidate();
    fixture
        .read(|store, budget| {
            // The old whole-context manifest deliberately refuses the stale sibling.
            assert!(
                crate::catalog_identity_mutation::load_manifest(
                    store,
                    "default",
                    &[VERSION.into()]
                )
                .is_err()
            );
            let captured =
                capture_current_in(store, &fixture.scope(), &fixture.selection(), budget)?;
            assert_eq!(captured.availability, FileAvailability::Available);
            validate_live(store, &captured, budget)
        })
        .unwrap();
}

#[test]
fn selected_stale_bytes_or_unavailable_file_never_authorize_publication() {
    let fixture = Fixture::new(false).install_candidate();
    fixture
        .read(|store, budget| {
            let captured =
                capture_current_in(store, &fixture.scope(), &fixture.selection(), budget)?;
            std::fs::write(
                fixture.media.join("part-1.mkv"),
                b"changed selected original bytes",
            )?;
            assert!(validate_live(store, &captured, budget).is_err());
            Ok(())
        })
        .unwrap();
    let missing = Fixture::new(false).remove_and_rescan(1).install_candidate();
    missing
        .read(|store, budget| {
            let captured =
                capture_current_in(store, &missing.scope(), &missing.selection(), budget)?;
            assert_eq!(captured.availability, FileAvailability::Unavailable);
            assert!(validate_live(store, &captured, budget).is_err());
            Ok(())
        })
        .unwrap();
}

#[test]
fn retired_or_detached_current_lineage_refuses() {
    for fixture in [Fixture::new(false).retire(), Fixture::new(true)] {
        let fixture = fixture.install_candidate();
        assert!(
            fixture
                .read(|store, budget| capture_current_in(
                    store,
                    &fixture.scope(),
                    &fixture.selection(),
                    budget
                ))
                .is_err()
        );
    }
}

#[test]
fn exact_audit_callback_and_initial_work_head_remain_required() {
    // At original collection only part one is available. The complete selected
    // available-original corpus therefore contains exactly this observation.
    let fixture = Fixture::new(false).remove_and_rescan(2).install_candidate();
    let captured = fixture
        .read(|store, budget| {
            capture_current_in(store, &fixture.scope(), &fixture.selection(), budget)
        })
        .unwrap();
    fixture
        .read(|store, budget| {
            let selection = fixture.selection();
            let head = SubjectHead {
                subject_state: SubjectState::Active {
                    item_revision: captured.work_item_revision,
                },
                schema_fingerprint: None,
                system_fields: BTreeSet::new(),
                source: None,
                attribution: None,
                pin: PinHead::NeverDecided,
            };
            let subjects: BTreeMap<String, _> = [(WORK.into(), head)].into();
            let request = RefreshRequestV2 {
                contract_version: CONTRACT_VERSION_V2,
                fence: fixture.scope(),
                subject: WORK.into(),
                expected_heads: subjects
                    .iter()
                    .map(|(id, head)| (id.clone(), head.stamp()))
                    .collect(),
                observations: vec![Observation {
                    expected_file: captured.current.clone(),
                    sidecar: SidecarObservation::Absent,
                }],
            };
            let outer = envelope(
                store,
                "audit",
                json!({"operation":"nfo_refresh","request":request}),
            );
            let allow = Cell::new(true);
            let calls = Cell::new(0);
            let custody = || {
                calls.set(calls.get() + 1);
                if allow.get() && !fixture.media.join("part-1.nfo").try_exists()? {
                    Ok(())
                } else {
                    Err(Error("retained sidecar or source lifecycle changed".into()))
                }
            };
            let audit = RefreshObservationAudit {
                catalog_id: store.database_id.clone(),
                incarnation: store.catalog_incarnation.clone().unwrap(),
                generation: generation(store),
                host_namespace: HOST.into(),
                subject: WORK.into(),
                outer_digest: outer.digest().as_str().into(),
                request_digest: budget.serialized(&request, 64)?,
                complete_original_selection_digest: catabolic_core::sha256(json!({"domain":"controlled.complete-available-original-selection.v1","selection":request.observations,"unavailable_original":fixture.selection_part(2).file,"source_root":fixture.media}).to_string()),
                observations: vec![ObservationAudit {
                    observed_file: captured.current.clone(),
                    original_lineage: selection.lineage.clone(),
                    original_file_provenance_digest: catabolic_core::sha256(json!({"domain":"controlled.original-creation.v1","native_file":captured.current,"fixture_bytes":"original part 1","generated":false}).to_string()),
                    observation_digest: budget.serialized(&request.observations[0], 64)?,
                    terminal: TerminalObservationAudit::Present {
                        selection,
                        current: captured.current.clone(),
                        availability: captured.availability.clone(),
                        source_state: SourceState::Enabled,
                        native_binding: captured.native_binding.clone(),
                        root_sidecar_audit_digest: catabolic_core::sha256(json!({"domain":"controlled.root-sidecar.v1","binding":captured.native_binding,"sidecar":"part-1.nfo","observed":"absent","source_enabled":true}).to_string()),
                    },
                }],
                custody: &custody,
            };
            let verified = verify_in(
                store,
                &outer,
                &request,
                &audit,
                &subjects,
                &BTreeMap::new(),
                budget,
            )?;
            assert_eq!(verified.revalidated_files().len(), 1);
            assert!(calls.get() >= 2);
            verified.revalidate_custody_in(store, &BTreeMap::new(), budget)?;
            allow.set(false);
            assert!(
                verified
                    .revalidate_custody_in(store, &BTreeMap::new(), budget)
                    .unwrap_err()
                    .0
                    .contains("retained sidecar or source lifecycle changed")
            );
            allow.set(true);
            let incorrect = [(WORK.into(), captured.work_item_revision + 1)].into();
            assert!(verified.revalidate_custody_in(store,&incorrect,budget).unwrap_err().0.contains("terminal native occurrence/structural/source facts changed"));
            Ok(())
        })
        .unwrap();
}

#[test]
fn source_succession_captures_complete_context_and_refuses_retired_closure() {
    let fixture = Fixture::new(false).install_candidate();
    // This genuine scan/identity fixture has no NFO origins. The complete
    // bootstrap owner must prove that empty cohort before logical retirement;
    // an absent state row alone is not a bootstrap or retirement grant.
    let bootstrap = fixture
        .store
        .transaction_detailed(|_| {
            crate::catalog_nfo_attribution::bootstrap::prepare_in(&fixture.store, &[])
        })
        .unwrap();
    let bootstrap_command = envelope(
        &fixture.store,
        "source-retirement-bootstrap",
        bootstrap.payload(),
    );
    committed(
        commands::execute_candidate_for_test(
            &fixture.store,
            &bootstrap_command,
            authority(),
            policy(),
            move |store, command, _| {
                crate::catalog_nfo_attribution::bootstrap::apply_in(store, command, bootstrap)
            },
        )
        .unwrap(),
    );
    let captured = fixture
        .read(|store, budget| {
            let captured = crate::catalog_source_succession::capture_source_succession_in(
                store, "default", "media", budget,
            )?;
            assert_eq!(captured.closure_counts_for_test(), (2, 1, 1, 2));
            captured.revalidate_in(store, budget)?;
            // Multipart whole-file structure is supported by the private
            // composite now; structural selection is still not byte authority.
            assert_eq!(captured.required_current_files()?.len(), 2);
            Ok(captured)
        })
        .unwrap();
    // Use the existing real receipt-backed logical owner, not SQL head edits.
    let fixture = fixture.retire();
    assert!(
        fixture
            .read(|store, budget| captured.revalidate_in(store, budget))
            .is_err()
    );
}

#[test]
fn source_succession_keeps_unavailable_sibling_in_descriptive_closure() {
    let fixture = Fixture::new(false).remove_and_rescan(2).install_candidate();
    fixture
        .read(|store, budget| {
            let captured = crate::catalog_source_succession::capture_source_succession_in(
                store, "default", "media", budget,
            )?;
            assert_eq!(captured.closure_counts_for_test(), (2, 1, 1, 2));
            captured.revalidate_in(store, budget)?;
            // Structural revisions include the missing sibling. Availability
            // remains the actual manifest/descriptor owner's separate refusal.
            assert_eq!(captured.required_current_files()?.len(), 2);
            assert!(
                crate::catalog_identity_mutation::load_manifest(
                    store,
                    "default",
                    &[VERSION.into()],
                )
                .is_err()
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn normal40_host_source_preparation_preserves_unavailable_sibling_and_revalidates() {
    use catabolic_runtime::host::{CONTRACT_VERSION, HostSession};
    // No candidate41 installation: the existing closed helper creates actual
    // native40 occurrences and the ordinary typed owners create the context.
    let fixture = Fixture::new(false).remove_and_rescan(2);
    assert_eq!(fixture.store.schema_version, 40);
    let expected = generation(&fixture.store);
    let path = fixture.path.clone();
    drop(fixture.store);
    let authorization = HostAuthorization::new(HOST.into(), "source-normal40-read".into()).unwrap();
    let mut host = HostSession::open_authorized(
        &path,
        "default",
        true,
        CONTRACT_VERSION,
        authorization.clone(),
    )
    .unwrap();
    let prepared = host.prepare_source_succession(expected, "media").unwrap();
    assert_eq!(prepared.generation(), expected);
    assert_eq!(prepared.source(), "media");
    assert_eq!(prepared.registry_files().count(), 2);
    assert_eq!(prepared.context_count(), 1);
    host.revalidate_source_preparation(&prepared).unwrap();
    assert!(
        host.prepare_source_succession(expected - 1, "media")
            .is_err()
    );
    // Descriptive closure must not accidentally become playable authority.
    assert!(
        host.projection_version_manifest(expected, &[VERSION.into()])
            .is_err()
    );
    let revoked = authorization.begin_revocation().unwrap();
    assert!(host.revalidate_source_preparation(&prepared).is_err());
    revoked.wait_drained(Duration::ZERO).unwrap();
    host.close().unwrap();
}

#[test]
fn normal40_source_preparation_fences_changes_and_describes_detached_registry() {
    let fixture = Fixture::new(false);
    assert_eq!(fixture.store.schema_version, 40);
    let expected = crate::catalog_projection::TrustedProjectionAuthority {
        catalog_id: fixture.store.database_id.clone(),
        incarnation: fixture.store.catalog_incarnation.clone().unwrap(),
        expected_generation: generation(&fixture.store),
    };
    let captured =
        crate::catalog_source_succession::prepare(&fixture.store, &expected, "default", "media")
            .unwrap();
    crate::catalog_source_succession::revalidate(&fixture.store, &captured).unwrap();
    // Previously attached is different from never-attached descriptive rows:
    // a genuine edge mutation must invalidate the captured admission first.
    let edge:String = fixture.store.db.query_row(
        "SELECT id FROM item_relationships WHERE source_id=? AND target_id=? AND kind='custom:motion_edition' AND active=1",
        params![WORK,EDITION], |r|r.get(0)).unwrap();
    curation::dispatch(
        &fixture.store,
        "default",
        "relationship-disable",
        &json!({"id":edge}),
    )
    .unwrap();
    assert!(crate::catalog_source_succession::revalidate(&fixture.store, &captured).is_err());
    fixture
        .read(|store, budget| {
            let detached = crate::catalog_source_succession::capture_source_succession_in(
                store, "default", "media", budget,
            )?;
            // Typed context/associations remain live without WORK->EDITION.
            // A native manifest is not Motion requester/library publication authority.
            assert_eq!(detached.required_current_files()?.len(), 2);
            assert!(
                crate::catalog_identity_mutation::load_manifest(
                    store,
                    "default",
                    &[VERSION.into()]
                )
                .is_ok()
            );
            Ok(())
        })
        .unwrap();
    // Restore through the same real owner before the independent retirement
    // test. Restoring equal membership does not revive the old captured heads.
    curation::relate(
        &fixture.store,
        &json!({"source":WORK,"target":EDITION,"kind":"custom:motion_edition"}),
    )
    .unwrap();
    assert!(crate::catalog_source_succession::revalidate(&fixture.store, &captured).is_err());
    let fixture = fixture.retire();
    assert!(crate::catalog_source_succession::revalidate(&fixture.store, &captured).is_err());
    assert!(
        crate::catalog_source_succession::prepare(&fixture.store, &expected, "default", "media")
            .is_err()
    );
    drop(fixture);
    let detached = Fixture::new(true);
    let expected = crate::catalog_projection::TrustedProjectionAuthority {
        catalog_id: detached.store.database_id.clone(),
        incarnation: detached.store.catalog_incarnation.clone().unwrap(),
        expected_generation: generation(&detached.store),
    };
    // Missing WORK->EDITION does not retire the typed version or context.
    // Descriptive closure remains complete without granting playable authority.
    let descriptive =
        crate::catalog_source_succession::prepare(&detached.store, &expected, "default", "media")
            .unwrap();
    assert_eq!(descriptive.registry_files().count(), 2);
    assert_eq!(descriptive.context_count(), 1);
    let live_versions: i64 = detached
        .store
        .db
        .query_row(
            "SELECT count(*) FROM catalog_live_versions WHERE id=?",
            [VERSION],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(live_versions, 1);
    let parents: i64 = detached.store.db.query_row(
        "SELECT count(*) FROM item_relationships WHERE source_id=? AND target_id=? AND kind='custom:motion_edition' AND active=1",
        params![WORK, EDITION], |r| r.get(0)).unwrap();
    assert_eq!(parents, 0);
    crate::catalog_source_succession::revalidate(&detached.store, &descriptive).unwrap();
    detached
        .read(|store, budget| {
            let captured = crate::catalog_source_succession::capture_source_succession_in(
                store, "default", "media", budget,
            )?;
            // Typed context/associations remain live without WORK->EDITION.
            // A native manifest is not Motion requester/library publication authority.
            assert_eq!(captured.required_current_files()?.len(), 2);
            assert!(
                crate::catalog_identity_mutation::load_manifest(
                    store,
                    "default",
                    &[VERSION.into()]
                )
                .is_ok()
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn candidate_source_evidence_baseline_matches_real_scanned_query_and_headers() {
    // Real helper observations and typed context owners, no invented transfer
    // permit, effect, component, processing job or receipt rows.
    let fixture = Fixture::new(false).install_candidate();
    assert_eq!(fixture.store.schema_version, 41);
    for (base, effective) in [
        ("file_facts", "catalog_source_effective_file_facts"),
        ("media_header_checks", "catalog_source_effective_headers"),
        (
            "component_assertions",
            "catalog_source_effective_assertions",
        ),
    ] {
        let raw = crate::migration::rows(
            &fixture.store.db,
            &format!("SELECT * FROM {base} ORDER BY 1,2"),
            &[],
        )
        .unwrap();
        let current = crate::migration::rows(
            &fixture.store.db,
            &format!("SELECT * FROM {effective} ORDER BY 1,2"),
            &[],
        )
        .unwrap();
        if base == "media_header_checks" {
            assert_eq!(raw.len(), 2);
        }
        assert_eq!(current, raw, "baseline differs for {base}");
    }
    let raw = crate::query::view_sql("catalog_component_occurrences");
    let current = crate::query::view_sql_for(&fixture.store, "catalog_component_occurrences");
    assert_eq!(
        crate::migration::rows(
            &fixture.store.db,
            &format!("SELECT * FROM ({raw}) ORDER BY occurrence_id"),
            &[]
        )
        .unwrap(),
        crate::migration::rows(
            &fixture.store.db,
            &format!("SELECT * FROM ({current}) ORDER BY occurrence_id"),
            &[]
        )
        .unwrap(),
    );
    // Exercise the actual SQL authorizer on a separately admitted read
    // connection, including the candidate-only constant refusal function.
    let read = candidate::open_candidate41_for_test(&fixture.path, false).unwrap();
    let options = crate::sql::Options {
        profile: "default",
        stable: true,
        ..Default::default()
    };
    let selected = crate::sql::execute_store(
        &read,
        Some("SELECT count(*) AS n FROM catalog_component_occurrences"),
        &options,
    )
    .unwrap();
    assert_eq!(selected["complete"], true);
    let headers = crate::sql::execute_store(
        &read,
        Some("SELECT count(*) AS n FROM main.catalog_source_effective_headers"),
        &options,
    )
    .unwrap();
    assert_eq!(headers["rows"], json!([[2]]));
    let http_options = crate::sql::Options {
        profile: "default",
        stable: true,
        http: true,
        ..Default::default()
    };
    assert!(
        crate::sql::execute_store(
            &read,
            Some("SELECT count(*) FROM main.execution_claims"),
            &http_options
        )
        .is_err()
    );
    assert!(
        crate::sql::execute_store(
            &read,
            Some("SELECT count(*) FROM main.observation_jobs"),
            &http_options
        )
        .is_err()
    );

    let refused = crate::sql::execute_store(
        &read,
        Some("SELECT catalog_source_evidence_refused()"),
        &options,
    )
    .unwrap_err();
    assert!(
        refused
            .0
            .contains("source succession evidence exceeds retained read budget")
    );
    drop(read);
    let effects: i64 = fixture
        .store
        .db
        .query_row(
            "SELECT count(*) FROM catalog_source_live_transfers",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(effects, 0);
    let refusal = fixture
        .store
        .db
        .query_row("SELECT catalog_source_evidence_refused()", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap_err();
    assert!(
        refusal
            .to_string()
            .contains("source succession evidence exceeds retained read budget")
    );

    // Real owner away-and-back transition: byte-equal relationship contents
    // must not restore a saved structural preparation. This is not a positive
    // transfer-effect test, which requires the independently verified owner.
    let captured = fixture
        .read(|store, budget| {
            crate::catalog_source_succession::capture_source_succession_in(
                store, "default", "media", budget,
            )
        })
        .unwrap();
    let before = crate::migration::rows(&fixture.store.db, "SELECT * FROM item_relationships WHERE source_id=? AND target_id=? AND kind='custom:motion_edition'", &[WORK.to_owned().into(), EDITION.to_owned().into()]).unwrap();
    let edge = before[0]["id"].as_str().unwrap();
    let head: i64 = fixture.store.db.query_row("SELECT revision FROM catalog_entity_revisions WHERE entity_kind='item_relationships' AND entity_key=json_array(?) AND deleted=0", [edge], |r| r.get(0)).unwrap();
    curation::dispatch(
        &fixture.store,
        "default",
        "relationship-disable",
        &json!({"id":edge}),
    )
    .unwrap();
    curation::relate(
        &fixture.store,
        &json!({"source":WORK,"target":EDITION,"kind":"custom:motion_edition"}),
    )
    .unwrap();
    let after = crate::migration::rows(
        &fixture.store.db,
        "SELECT * FROM item_relationships WHERE id=?",
        &[edge.to_owned().into()],
    )
    .unwrap();
    assert_eq!(before, after);
    let current: i64 = fixture.store.db.query_row("SELECT revision FROM catalog_entity_revisions WHERE entity_kind='item_relationships' AND entity_key=json_array(?) AND deleted=0", [edge], |r| r.get(0)).unwrap();
    assert!(current > head);
    assert!(
        fixture
            .read(|store, budget| captured.revalidate_in(store, budget))
            .is_err()
    );
}

#[test]
fn retained_source_candidate_hashes_actual_roots_and_refuses_changed_copy() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let fixture = Fixture::new(false).install_candidate();
    // Actual source policy owner explicitly disables only UUID probing. Device,
    // inode, ctime and complete native capture checks remain independent.
    crate::observation::execute(
        &fixture.store,
        "default",
        "trust",
        &json!({"source":"media","overrides":{"uuid":"skip"},"apply":true}),
    )
    .unwrap();
    let copied = tempfile::tempdir().unwrap();
    for part in 1..=2 {
        std::fs::copy(
            fixture.media.join(format!("part-{part}.mkv")),
            copied.path().join(format!("part-{part}.mkv")),
        )
        .unwrap();
    }
    let stop = AtomicBool::new(false);
    let before = migration::snapshot(&fixture.store.db).unwrap();
    let commitment = fixture
        .read(|store, budget| {
            crate::catalog_source_succession::verify_candidate_content_for_test(
                store,
                "default",
                "media",
                copied.path(),
                &stop,
                budget,
                || {},
            )
        })
        .unwrap();
    assert_eq!(commitment.len(), 64);
    assert_eq!(migration::snapshot(&fixture.store.db).unwrap(), before);
    // Mutation after successful hashing must fail retained descriptor/locator
    // validation, rather than reuse the equal-content commitment.
    let changed = fixture.read(|store, budget| {
        crate::catalog_source_succession::verify_candidate_content_for_test(
            store,
            "default",
            "media",
            copied.path(),
            &stop,
            budget,
            || {
                std::fs::write(copied.path().join("part-1.mkv"), b"different copied bytes")
                    .unwrap();
            },
        )
    });
    assert!(changed.is_err());
    assert_eq!(migration::snapshot(&fixture.store.db).unwrap(), before);
    std::fs::copy(
        fixture.media.join("part-1.mkv"),
        copied.path().join("part-1.mkv"),
    )
    .unwrap();
    let cancelled = fixture.read(|store, budget| {
        crate::catalog_source_succession::verify_candidate_content_for_test(
            store,
            "default",
            "media",
            copied.path(),
            &stop,
            budget,
            || stop.store(true, Ordering::Release),
        )
    });
    assert!(cancelled.is_err());
    assert_eq!(migration::snapshot(&fixture.store.db).unwrap(), before);
    // The distinct candidate fixed-owner helper shares production preparation
    // mechanics, not Completed admission. It must independently retain named
    // root/held-descriptor checks and cancellation after successful hashing.
    stop.store(false, Ordering::Release);
    // The earlier mutation/restore changed candidate ctime. Compare the fixed
    // owner against a freshly verified generic commitment, not the old tuple.
    let expected = fixture
        .read(|store, budget| {
            crate::catalog_source_succession::verify_candidate_content_for_test(
                store,
                "default",
                "media",
                copied.path(),
                &stop,
                budget,
                || {},
            )
        })
        .unwrap();
    let fixed = fixture
        .read(|store, budget| {
            crate::catalog_source_succession::verify_candidate_preparation_for_test(
                store,
                "default",
                "media",
                copied.path(),
                &stop,
                budget,
                || {},
            )
        })
        .unwrap();
    assert_eq!(fixed, expected);
    let displaced = tempfile::tempdir().unwrap();
    let saved = displaced.path().join("held-root");
    let replaced = fixture.read(|store, budget| {
        crate::catalog_source_succession::verify_candidate_preparation_for_test(
            store,
            "default",
            "media",
            copied.path(),
            &stop,
            budget,
            || {
                std::fs::rename(copied.path(), &saved).unwrap();
                std::fs::create_dir(copied.path()).unwrap();
                for part in 1..=2 {
                    std::fs::copy(
                        fixture.media.join(format!("part-{part}.mkv")),
                        copied.path().join(format!("part-{part}.mkv")),
                    )
                    .unwrap();
                }
            },
        )
    });
    assert!(replaced.is_err());
    std::fs::remove_dir_all(copied.path()).unwrap();
    std::fs::rename(&saved, copied.path()).unwrap();
    let fixed_cancelled = fixture.read(|store, budget| {
        crate::catalog_source_succession::verify_candidate_preparation_for_test(
            store,
            "default",
            "media",
            copied.path(),
            &stop,
            budget,
            || stop.store(true, Ordering::Release),
        )
    });
    assert!(fixed_cancelled.is_err());
    assert_eq!(migration::snapshot(&fixture.store.db).unwrap(), before);
}

#[test]
fn scanned40_candidate_observation_transition_preserves_rows_and_denies_fake_renewal() {
    let fixture = Fixture::new(false);
    let before = migration::snapshot(&fixture.store.db).unwrap();
    let old_ledger = migration::rows(
        &fixture.store.db,
        "SELECT * FROM schema_migrations WHERE version<=40 ORDER BY version",
        &[],
    )
    .unwrap();
    let observations = migration::rows(&fixture.store.db,
        "SELECT profile,file_id,size,mtime_ns,device,inode,status,scan_id FROM observations ORDER BY profile,file_id", &[]).unwrap();
    assert!(!observations.is_empty());
    let fixture = fixture.install_candidate();
    // Genuine composed candidate installation, not yet the offline operator
    // transition or a source-renewal command.
    migration::preservation(&fixture.store.db, &before).unwrap();
    assert_eq!(
        old_ledger,
        migration::rows(
            &fixture.store.db,
            "SELECT * FROM schema_migrations WHERE version<=40 ORDER BY version",
            &[]
        )
        .unwrap()
    );
    assert_eq!(observations, migration::rows(&fixture.store.db,
        "SELECT profile,file_id,size,mtime_ns,device,inode,status,scan_id FROM observations ORDER BY profile,file_id", &[]).unwrap());
    let bad: i64 = fixture.store.db.query_row("SELECT count(*) FROM observations WHERE observation_origin!='scan' OR scan_id IS NULL OR succession_source_id IS NOT NULL OR succession_revision IS NOT NULL", [], |r| r.get(0)).unwrap();
    assert_eq!(bad, 0);
    for sql in [
        "UPDATE observations SET scan_id=NULL",
        "UPDATE observations SET observation_origin='succession',scan_id=NULL,succession_source_id='media',succession_revision=1",
        "UPDATE observations SET observation_origin='scan',succession_source_id='media',succession_revision=1",
    ] {
        assert!(
            fixture
                .store
                .transaction(|db| {
                    db.execute(sql, [])?;
                    Ok(())
                })
                .is_err()
        );
        migration::preservation(&fixture.store.db, &before).unwrap();
    }
}

#[test]
fn actual_closed_scan_witness_survives_offline41_pending_inventory() {
    let fixture = Fixture::new(false);
    // The fixture already owns the real writer. Inspect that owner, then drop
    // it before requesting exclusive offline migration custody.
    let inventory = crate::native_worker_inventory::inspect(&fixture.store).unwrap();
    assert!(inventory.admits_new_work());
    assert!(
        inventory.witnessed_observation > 0,
        "actual closed helper witness required"
    );
    drop(fixture.store);
    let pending = migration::assert_live_pending41_for_test(&fixture.path).unwrap();
    assert!(
        pending
            .as_object()
            .unwrap()
            .values()
            .all(|v| v.as_i64() == Some(0))
    );
    assert!(Store::open(&fixture.path, true, false).is_err());
}

#[test]
fn actual_multipart_source_succession_renews_once_and_replays_without_roots() {
    use crate::catalog_source_succession as succession;
    use std::sync::atomic::AtomicBool;
    let fixture = Fixture::new_relocation()
        .with_unrelated_scanned_sources(4)
        .install_candidate();
    let copied = tempfile::tempdir().unwrap();
    for part in 1..=2 {
        std::fs::copy(
            fixture.media.join(format!("part-{part}.mkv")),
            copied.path().join(format!("part-{part}.mkv")),
        )
        .unwrap();
    }
    let stop = AtomicBool::new(false);
    let commitment = fixture
        .read(|store, budget| {
            succession::verify_candidate_content_for_test(
                store,
                "default",
                "media",
                copied.path(),
                &stop,
                budget,
                || {},
            )
        })
        .unwrap();
    let command = envelope(
        &fixture.store,
        "actual-source-succession",
        json!({
            "operation":"source_succession", "profile":"default", "source":"media",
            "candidate_root":copied.path().to_str().unwrap(), "content_commitment":commitment,"admission_digest":catabolic_core::sha256("native-only fixture intent, not Motion authority")
        }),
    );
    // The native fixture commits an explicit test intent, never a fabricated
    // Motion actor grant. Same content with a different intent is a conflict.
    let mut changed_intent = command.payload().clone();
    changed_intent["admission_digest"] = json!(catabolic_core::sha256("different fixture intent"));
    let changed_intent_command =
        envelope(&fixture.store, "actual-source-succession", changed_intent);
    let mut malformed_intent = command.payload().clone();
    malformed_intent["admission_digest"] = json!("not-a-digest");
    let malformed_intent_command =
        envelope(&fixture.store, "malformed-source-intent", malformed_intent);
    let old_watch = crate::source_events::snapshot(&fixture.store, "default").unwrap();
    let old_query = fixture
        .read(|store, _| {
            crate::query::CatalogQuery::new(store, "default")?
                .execute("files", &json!({"location":"media"}))
        })
        .unwrap();
    let old_files =
        migration::rows(&fixture.store.db, "SELECT * FROM files ORDER BY id", &[]).unwrap();
    let old_scans =
        migration::rows(&fixture.store.db, "SELECT * FROM scans ORDER BY id", &[]).unwrap();
    let execute = || {
        let mut budget = ProofBudget::new(fixture.store.catalog_operation_budget().unwrap());
        succession::execute_for_test(
            &fixture.store,
            &command,
            authority(),
            ReceiptPolicy {
                retention: Duration::ZERO,
                ..policy()
            },
            &stop,
            &mut budget,
        )
    };
    let before = migration::snapshot(&fixture.store.db).unwrap();
    let mut malformed_budget = ProofBudget::new(fixture.store.catalog_operation_budget().unwrap());
    assert!(
        succession::execute_for_test(
            &fixture.store,
            &malformed_intent_command,
            authority(),
            policy(),
            &stop,
            &mut malformed_budget
        )
        .is_err()
    );
    assert_eq!(migration::snapshot(&fixture.store.db).unwrap(), before);
    stop.store(true, std::sync::atomic::Ordering::Release);
    assert!(
        execute().is_err(),
        "cancelled preparation admitted a renewal"
    );
    stop.store(false, std::sync::atomic::Ordering::Release);
    assert_eq!(migration::snapshot(&fixture.store.db).unwrap(), before);
    let mut fault_budget = ProofBudget::new(fixture.store.catalog_operation_budget().unwrap());
    let fault = succession::swallowed_renewal_failure_for_test(
        &fixture.store,
        &command,
        &stop,
        &mut fault_budget,
    );
    assert!(fault.is_err(), "swallowed physical renewal error committed");
    assert_eq!(migration::snapshot(&fixture.store.db).unwrap(), before);
    let failed_budget = succession::source_budget_failure_for_test(
        &fixture.store,
        &command,
        authority(),
        policy(),
        &stop,
    );
    assert!(
        failed_budget.is_err(),
        "swallowed source budget failure committed"
    );
    assert_eq!(migration::snapshot(&fixture.store.db).unwrap(), before);
    let outcome = execute().unwrap();
    let result = match outcome {
        CommandOutcome::Committed { result, .. } => result,
        other => panic!("actual source mutation did not commit: {other:?}"),
    };
    assert_eq!(
        result["admission_digest"],
        command.payload()["admission_digest"]
    );
    assert_ne!(result["admission_digest"], result["content_commitment"]);
    let mut conflict_budget = ProofBudget::new(fixture.store.catalog_operation_budget().unwrap());
    assert!(matches!(
        succession::execute_for_test(
            &fixture.store,
            &changed_intent_command,
            authority(),
            policy(),
            &stop,
            &mut conflict_budget
        )
        .unwrap(),
        CommandOutcome::Conflict
    ));
    assert_eq!(result["files"], 2);
    assert_eq!(result["contexts"], 1);
    assert_eq!(
        migration::rows(&fixture.store.db, "SELECT * FROM files ORDER BY id", &[]).unwrap(),
        old_files
    );
    assert_eq!(
        migration::rows(&fixture.store.db, "SELECT * FROM scans ORDER BY id", &[]).unwrap(),
        old_scans
    );
    let renewed: i64 = fixture.store.db.query_row(
        "SELECT count(*) FROM observations WHERE observation_origin='succession' AND scan_id IS NULL AND succession_revision=1", [], |r|r.get(0)).unwrap();
    assert_eq!(renewed, 2);
    let current_query = fixture
        .read(|store, _| {
            crate::query::CatalogQuery::new(store, "default")?
                .execute("files", &json!({"location":"media"}))
        })
        .unwrap();
    let rows = current_query["files"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert!(row["scan_id"].is_null());
        assert!(
            row["observed_at"].is_null(),
            "old scan cannot measure renewed inode"
        );
        let old = old_query["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|old| old["id"] == row["id"])
            .unwrap();
        assert_eq!(row["media_header_status"], old["media_header_status"]);
    }
    let current_watch = crate::source_events::snapshot(&fixture.store, "default").unwrap();
    assert_ne!(old_watch.fence, current_watch.fence);
    let watched = current_watch
        .sources
        .iter()
        .find(|s| s.name == "media")
        .unwrap();
    assert_eq!(watched.root, copied.path());
    fixture
        .read(|store, budget| {
            succession::capture_source_succession_in(store, "default", "media", budget)
        })
        .unwrap();
    let after = migration::snapshot(&fixture.store.db).unwrap();
    // Saved receipt recovery must precede all descriptor preparation. Removing
    // both named roots does not manufacture any new physical authority.
    std::fs::rename(
        &fixture.media,
        fixture._root.path().join("old-root-unavailable"),
    )
    .unwrap();
    let new_root_unavailable = copied.path().with_extension("temporarily-unavailable");
    std::fs::rename(copied.path(), &new_root_unavailable).unwrap();
    let replay = execute();
    assert!(
        matches!(replay.unwrap(), CommandOutcome::Replay { result: replayed, .. } if replayed == result)
    );
    assert_eq!(migration::snapshot(&fixture.store.db).unwrap(), after);
    // Actual receipt maintenance expires only the disposable result. Permanent
    // typed succession evidence and the exact command identity survive.
    assert_eq!(
        crate::catalog_commands::compact_candidate_receipts_for_test(
            &fixture.store,
            crate::catalog_commands::TrustedReceiptMaintenance {
                host_namespace: HOST,
                holder_namespace: "source-succession-test",
            },
            1,
        )
        .unwrap(),
        1
    );
    assert!(matches!(execute().unwrap(), CommandOutcome::ReceiptExpired));
    std::fs::rename(&new_root_unavailable, copied.path()).unwrap();
    // This reopen precedes the deliberately unwitnessed synchronous rescan.
    // It uses exact candidate admission, not normal unregistered41 admission.
    let fixture = fixture.reopen_candidate();
    fixture
        .read(|store, budget| {
            succession::capture_source_succession_in(store, "default", "media", budget)
        })
        .unwrap();
    let mut replay_budget = ProofBudget::new(fixture.store.catalog_operation_budget().unwrap());
    assert!(matches!(
        succession::execute_for_test(
            &fixture.store,
            &command,
            authority(),
            policy(),
            &stop,
            &mut replay_budget,
        )
        .unwrap(),
        CommandOutcome::ReceiptExpired
    ));
    // A second genuine copy consumes the authenticated prior effect, including
    // its expired receipt, instead of treating raw base facts as current bytes.
    let copied_again = tempfile::tempdir().unwrap();
    for part in 1..=2 {
        std::fs::copy(
            copied.path().join(format!("part-{part}.mkv")),
            copied_again.path().join(format!("part-{part}.mkv")),
        )
        .unwrap();
    }
    // Generic verification deliberately keeps all complete recaptures. This
    // evidence-bearing second-hop cohort exceeds that corridor's existing local
    // proof budget, rather than silently gaining the fixed-owner reuse grant.
    let generic_second = fixture.read(|store, budget| {
        succession::verify_candidate_content_for_test(
            store,
            "default",
            "media",
            copied_again.path(),
            &stop,
            budget,
            || {},
        )
    });
    assert!(
        generic_second
            .unwrap_err()
            .to_string()
            .contains("catalog_nfo_proof_budget_refused")
    );
    // Explicit candidate-only fixed-owner mechanics, not generic verification
    // or a fabricated Completed admission. The genuine completed fixture below
    // independently exercises both multipart hops through real preparation.
    let second_commitment = fixture
        .read(|store, budget| {
            succession::verify_candidate_preparation_for_test(
                store,
                "default",
                "media",
                copied_again.path(),
                &stop,
                budget,
                || {},
            )
        })
        .unwrap();
    let second_command = envelope(
        &fixture.store,
        "actual-source-succession-again",
        json!({
            "operation":"source_succession", "profile":"default", "source":"media",
            "candidate_root":copied_again.path().to_str().unwrap(), "content_commitment":second_commitment,"admission_digest":catabolic_core::sha256("second native-only fixture intent, not Motion authority")
        }),
    );
    let mut second_budget = ProofBudget::new(fixture.store.catalog_operation_budget().unwrap());
    assert!(matches!(
        succession::execute_for_test(
            &fixture.store,
            &second_command,
            authority(),
            policy(),
            &stop,
            &mut second_budget
        )
        .unwrap(),
        CommandOutcome::Committed { .. }
    ));
    fixture
        .read(|store, budget| {
            succession::capture_source_succession_in(store, "default", "media", budget)
        })
        .unwrap();
    let links:i64 = fixture.store.db.query_row(
        "SELECT count(*) FROM catalog_source_succession_evidence WHERE succession_revision=2 AND evidence_kind='media_header' AND predecessor_digest<>?",
        ["0".repeat(64)], |r|r.get(0)).unwrap();
    assert_eq!(links, 2);
    let fixture = fixture.reopen_candidate();
    let twice = fixture
        .read(|store, _| {
            crate::query::CatalogQuery::new(store, "default")?
                .execute("files", &json!({"location":"media"}))
        })
        .unwrap();
    assert_eq!(twice["files"].as_array().unwrap().len(), 2);
    for row in twice["files"].as_array().unwrap() {
        assert!(row["scan_id"].is_null());
        let old = old_query["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|old| old["id"] == row["id"])
            .unwrap();
        assert_eq!(row["media_header_status"], old["media_header_status"]);
    }
    let source_index_columns = fixture.store.db.prepare(
        "SELECT name FROM pragma_index_info('sqlite_autoindex_catalog_source_successions_2') ORDER BY seqno"
    ).unwrap().query_map([], |row| row.get::<_, String>(0)).unwrap()
        .collect::<std::result::Result<Vec<_>, _>>().unwrap();
    assert_eq!(
        source_index_columns,
        ["profile", "source_id", "revision"],
        "seeded reader must use the exact closed source-prefix primary key"
    );
    let filtered_pages = || {
        fixture
            .read(|store, _| {
                let budget = store.catalog_operation_budget()?;
                let before = budget.usage().work;
                let query = crate::query::CatalogQuery::new(store, "default")?;
                let first = query.execute("files", &json!({"location":"media","limit":1}))?;
                assert_eq!(first["files"].as_array().unwrap().len(), 1);
                assert!(first["next_cursor"].is_string());
                let second = query.execute(
                    "files",
                    &json!({"location":"media","limit":1,"cursor":first["next_cursor"]}),
                )?;
                assert_eq!(second["files"].as_array().unwrap().len(), 1);
                assert!(second["next_cursor"].is_null());
                let mut ids = vec![
                    first["files"][0]["id"].clone(),
                    second["files"][0]["id"].clone(),
                ];
                ids.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
                assert_ne!(ids[0], ids[1]);
                for row in [&first["files"][0], &second["files"][0]] {
                    assert_eq!(row["location"], "media");
                    assert!(row["scan_id"].is_null());
                }
                Ok((ids, budget.usage().work - before))
            })
            .unwrap()
    };
    let before_unrelated = filtered_pages();
    for index in 0..4 {
        let name = format!("unrelated-source-{index}");
        let old_root = fixture._root.path().join(&name);
        let new_root = fixture._root.path().join(format!("{name}-renewed"));
        std::fs::create_dir(&new_root).unwrap();
        for part in 1..=2 {
            std::fs::copy(
                old_root.join(format!("part-{part}.mkv")),
                new_root.join(format!("part-{part}.mkv")),
            )
            .unwrap();
        }
        let commitment = fixture
            .read(|store, budget| {
                succession::verify_candidate_content_for_test(
                    store,
                    "default",
                    &name,
                    &new_root,
                    &stop,
                    budget,
                    || {},
                )
            })
            .unwrap();
        let command = envelope(
            &fixture.store,
            &format!("{name}-renewal"),
            json!({"operation":"source_succession","profile":"default","source":name,"candidate_root":new_root.to_str().unwrap(),"content_commitment":commitment,"admission_digest":catabolic_core::sha256("native-only fixture intent, not Motion authority")}),
        );
        let mut budget = ProofBudget::new(fixture.store.catalog_operation_budget().unwrap());
        committed(
            succession::execute_for_test(
                &fixture.store,
                &command,
                authority(),
                policy(),
                &stop,
                &mut budget,
            )
            .unwrap(),
        );
    }
    let unrelated_headers:i64=fixture.store.db.query_row("SELECT count(*) FROM catalog_source_succession_evidence WHERE source_id<>'media' AND evidence_kind='media_header'",[],|r|r.get(0)).unwrap();
    assert_eq!(
        unrelated_headers, 8,
        "fixture must contain real transferred evidence"
    );
    let after_unrelated = filtered_pages();
    assert_eq!(
        before_unrelated.0, after_unrelated.0,
        "filtered pagination omitted or mixed sources"
    );
    assert_eq!(
        before_unrelated.1, after_unrelated.1,
        "unrelated transfer histories consumed filtered source work"
    );
    let current_headers = || -> i64 {
        fixture.read(|store, _| Ok(store.db.query_row(
        "SELECT count(*) FROM catalog_source_live_transfers WHERE profile='default' AND source_id='media' AND evidence_kind='media_header'", [], |r|r.get(0))?)).unwrap()
    };
    assert_eq!(current_headers(), 2);
    // Genuine unrelated producer changes must not globally invalidate source
    // transfer evidence or consume another source's chain budget.
    for id in ["unrelated-parent", "unrelated-child"] {
        curation::item_put(
            &fixture.store,
            &json!({"id":id,"kind":"video","metadata":{}}),
        )
        .unwrap();
    }
    curation::relate(&fixture.store, &json!({"source":"unrelated-parent","target":"unrelated-child","kind":"custom:motion_edition"})).unwrap();
    assert_eq!(current_headers(), 2);
    fixture
        .read(|store, budget| {
            succession::capture_source_succession_in(store, "default", "media", budget)
        })
        .unwrap();
    // Explicit rolled-back corruption harness, never an owner mutation: normal
    // writes first refuse. Restore the exact trigger before either reader runs.
    assert!(fixture.store.db.execute("UPDATE catalog_source_succession_evidence SET predecessor_digest=? WHERE succession_revision=2",["0".repeat(64)]).is_err());
    for altered in ["0".repeat(64), "f".repeat(64)] {
        let before_corruption = migration::snapshot(&fixture.store.db).unwrap();
        let rejected: crate::Result<()> = fixture.store.transaction_detailed(|db| {
            let store = &fixture.store;
            assert!(std::ptr::eq(db, &store.db), "corruption fixture lost retained connection");
            let _source_read = succession::current::enter_read(store)?;
            let reset=succession::authorizer::Guard::for_store(store)?;
            store.db.authorizer(Some(|_:rusqlite::hooks::AuthContext<'_>|rusqlite::hooks::Authorization::Allow))?;
            let trigger:String=store.db.query_row("SELECT sql FROM sqlite_master WHERE name='catalog_source_succession_evidence_no_update'",[],|r|r.get(0))?;
            store.db.execute_batch("DROP TRIGGER catalog_source_succession_evidence_no_update")?;
            store.db.execute("UPDATE catalog_source_succession_evidence SET predecessor_digest=? WHERE succession_revision=2 AND evidence_kind='media_header'",[&altered])?;
            store.db.execute_batch(&trigger)?;
            reset.restore()?;
            let live:i64=store.db.query_row("SELECT count(*) FROM catalog_source_live_transfers WHERE profile='default' AND source_id='media' AND evidence_kind='media_header'",[],|r|r.get(0))?;
            assert_eq!(live,0,"SQL accepted altered predecessor");
            let mut budget=ProofBudget::new(store.catalog_operation_budget()?);
            assert!(succession::capture_source_succession_in(store,"default","media",&mut budget).is_err(),"Rust accepted altered predecessor");
            Err(crate::Error("rollback intentional corruption".into()))
        });
        assert_eq!(rejected.unwrap_err().0, "rollback intentional corruption");
        assert_eq!(
            migration::snapshot(&fixture.store.db).unwrap(),
            before_corruption
        );
        assert_eq!(current_headers(), 2);
    }
    for missing_receipt in [false, true] {
        let before_corruption = migration::snapshot(&fixture.store.db).unwrap();
        let rejected:crate::Result<()> = fixture.store.transaction_detailed(|db| {
            let store = &fixture.store;
            assert!(std::ptr::eq(db, &store.db), "corruption fixture lost retained connection");
            let reset=succession::authorizer::Guard::for_store(store)?;
            store.db.authorizer(Some(|_:rusqlite::hooks::AuthContext<'_>|rusqlite::hooks::Authorization::Allow))?;
            let table=if missing_receipt {"catalog_command_receipts"} else {"catalog_source_successions"};
            let triggers:Vec<(String,String)>=store.db.prepare("SELECT name,sql FROM sqlite_master WHERE type='trigger' AND tbl_name=? ORDER BY name")?
                .query_map([table],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<std::result::Result<_,_>>()?;
            for (name,_) in &triggers {
                store.db.execute_batch(&format!("DROP TRIGGER \"{}\"", name.replace('"', "\"\"")))?;
            }
            if missing_receipt {
                // The LEFT JOIN must treat every missing receipt field as NULL,
                // never let SQL's three-valued result comparison grant evidence.
                assert_eq!(store.db.execute("DELETE FROM catalog_command_receipts WHERE catalog_id=? AND host_namespace=? AND operation_id=?",params![store.database_id,HOST,"actual-source-succession"] )?,1);
            } else {
                assert_eq!(store.db.execute("UPDATE catalog_source_successions SET predecessor_digest=? WHERE profile='default' AND source_id='media' AND revision=1",["f".repeat(64)])?,1);
            }
            for (_,sql) in &triggers {store.db.execute_batch(sql)?;}
            reset.restore()?;
            let live:i64=store.db.query_row("SELECT count(*) FROM catalog_source_live_transfers WHERE profile='default' AND source_id='media' AND evidence_kind='media_header'",[],|r|r.get(0))?;
            assert_eq!(live,0);
            let mut budget=ProofBudget::new(store.catalog_operation_budget()?);
            assert!(succession::capture_source_succession_in(store,"default","media",&mut budget).is_err());
            Err(crate::Error("rollback intentional missing provenance".into()))
        });
        assert_eq!(
            rejected.unwrap_err().0,
            "rollback intentional missing provenance"
        );
        assert_eq!(
            migration::snapshot(&fixture.store.db).unwrap(),
            before_corruption
        );
        assert_eq!(current_headers(), 2);
    }
    // A real relevant relationship away-and-back advances permanent scoped
    // history even though the final visible parent membership is equal.
    let edge:String=fixture.store.db.query_row("SELECT id FROM item_relationships WHERE source_id=? AND target_id=? AND kind='custom:motion_edition' AND active=1",params![WORK,EDITION],|r|r.get(0)).unwrap();
    curation::dispatch(
        &fixture.store,
        "default",
        "relationship-disable",
        &json!({"id":edge}),
    )
    .unwrap();
    curation::relate(
        &fixture.store,
        &json!({"source":WORK,"target":EDITION,"kind":"custom:motion_edition"}),
    )
    .unwrap();
    assert_eq!(
        current_headers(),
        0,
        "away-and-back resurrected transfer evidence"
    );
    // Genuine synchronous scan owner replacement, not a closed-helper cleanup
    // proof: this fixture deliberately makes no restart/admission claim for its
    // final scan job. The official helper still refuses unregistered41.
    let rescanned = crate::observation::execute(
        &fixture.store,
        "default",
        "scan",
        &json!({"source":"media"}),
    )
    .unwrap();
    assert_eq!(rescanned["complete"], true);
    let scan_produced:i64=fixture.store.db.query_row(
        "SELECT count(*) FROM observations WHERE observation_origin='scan' AND scan_id IS NOT NULL AND succession_source_id IS NULL AND succession_revision IS NULL",[],|r|r.get(0)).unwrap();
    assert_eq!(scan_produced, 2);
    let unresolved = fixture
        .read(|store, _| crate::native_worker_inventory::inspect_source_candidate_for_test(store))
        .unwrap();
    assert!(
        !unresolved.admits_new_work(),
        "direct scan must not invent cleanup custody"
    );
    fixture
        .read(|store, budget| {
            succession::capture_source_succession_in(store, "default", "media", budget)
        })
        .unwrap();
}

#[test]
fn genuine_completed_catalog_runs_two_source_renewals_and_recovers_permanent_receipts() {
    use crate::migration::offline_bootstrap::{
        CurrentCatalogImage, CurrentRestoreHostAttestation, OfflineBootstrap,
        PreparedOfflineBootstrap, Recovery,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    let Fixture {
        _root,
        path,
        media,
        store,
        ..
    } = Fixture::new_relocation();
    let original_headers = migration::rows(
        &store.db,
        "SELECT * FROM main.media_header_checks ORDER BY file_id",
        &[],
    )
    .unwrap();
    assert_eq!(original_headers.len(), 2);
    let inspect_rotated_copy = |image: CurrentCatalogImage| {
        // Explicit native-fixture host commitments, not verified Motion pair
        // authority. Each image is independently rotated once; discarding it
        // returns the same original, unrotated completed owner for the next hop.
        let host = CurrentRestoreHostAttestation {
            pair_sha256: "a".repeat(64),
            motion_binding_sha256: "b".repeat(64),
            catalog_id: image.catalog_id().into(),
            incarnation: image.incarnation().into(),
            generation: image.generation(),
            current_inventory_sha256: image.current_inventory_sha256().into(),
            transition_digest: image.transition_digest().into(),
        };
        let expected_effects = image.source_effects().to_vec();
        let rotated = image
            .rotate_current(&host, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(rotated.generation(), host.generation);
        assert_eq!(rotated.source_effects(), expected_effects);
        let evidence = rotated
            .current_source_evidence("default", "media", &AtomicBool::new(false))
            .unwrap();
        assert_eq!(evidence.headers().len(), original_headers.len());
        for (renewed, original) in evidence.headers().iter().zip(&original_headers) {
            for key in [
                "profile",
                "file_id",
                "size",
                "status",
                "reason",
                "bytes_read",
            ] {
                assert_eq!(renewed[key], original[key], "restored header {key}");
            }
            assert!(
                renewed["scan_id"].is_null(),
                "restore must not invent a measurement scan"
            );
            for key in ["mtime_ns", "device", "inode"] {
                assert!(renewed[key].is_i64());
            }
        }
        // This real scan-only cohort has no probe-produced processing facts.
        assert!(evidence.facts().is_empty());
        assert!(
            rotated
                .current_source_evidence("default", "media", &AtomicBool::new(true))
                .is_err()
        );
        rotated.discard_copy()
    };
    drop(store);
    let quiescence = || migration::NativeUpgradeQuiescence {
        admission_stopped: true,
        readers_and_workers_stopped: true,
        exclusive_operational_ownership: true,
    };
    let planned = PreparedOfflineBootstrap::prepare(&path, quiescence()).unwrap();
    let plan = planned.approved_plan().clone();
    let offline = planned.apply(None).unwrap();
    let bootstrap_payload = offline.prepare(HOST, &[]).unwrap();
    let bootstrap = CommandEnvelope::new(
        1,
        CommandKey::new(offline.catalog_id(), HOST, "completed-source-bootstrap").unwrap(),
        offline.incarnation(),
        offline.generation().unwrap(),
        &bootstrap_payload.to_string(),
    )
    .unwrap();
    assert!(matches!(
        offline.execute(&bootstrap, HOST, &[], policy()).unwrap(),
        CommandOutcome::Committed { .. }
    ));
    let mut completed = offline.into_completed_catalog(&bootstrap).unwrap();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    for part in 1..=2 {
        std::fs::copy(
            media.join(format!("part-{part}.mkv")),
            first.path().join(format!("part-{part}.mkv")),
        )
        .unwrap();
        std::fs::copy(
            media.join(format!("part-{part}.mkv")),
            second.path().join(format!("part-{part}.mkv")),
        )
        .unwrap();
    }
    let stop = AtomicBool::new(false);
    let prepared = completed
        .prepare_source_succession("default", "media", first.path(), &stop)
        .unwrap();
    let make_command = |prepared: &crate::catalog_source_succession::SourceRenewalPreparation,
                        id: &str| {
        CommandEnvelope::new(1, CommandKey::new(&prepared.catalog_id, HOST, id).unwrap(),
            &prepared.incarnation, prepared.expected_generation, &json!({
                "operation":"source_succession", "profile":prepared.profile, "source":prepared.source,
                "candidate_root":prepared.candidate_root, "content_commitment":prepared.content_commitment,
                "admission_digest":catabolic_core::sha256(format!("explicit native fixture intent: {id}"))
            }).to_string()).unwrap()
    };
    let command = make_command(&prepared, "completed-source-first");
    let mut denied = authority();
    denied.allow_new_commands = false;
    assert!(
        completed
            .execute_source_succession(&command, denied, policy(), &stop)
            .is_err()
    );
    stop.store(true, Ordering::Release);
    assert!(
        completed
            .execute_source_succession(&command, authority(), policy(), &stop)
            .is_err()
    );
    stop.store(false, Ordering::Release);
    let retry = completed
        .prepare_source_succession("default", "media", first.path(), &stop)
        .unwrap();
    assert_eq!(retry.expected_generation, prepared.expected_generation);
    assert_eq!(retry.content_commitment, prepared.content_commitment);
    let mut wrong = command.payload().clone();
    wrong["content_commitment"] = json!("f".repeat(64));
    let wrong = CommandEnvelope::new(
        1,
        CommandKey::new(&prepared.catalog_id, HOST, "completed-source-wrong-content").unwrap(),
        &prepared.incarnation,
        prepared.expected_generation,
        &wrong.to_string(),
    )
    .unwrap();
    assert!(
        completed
            .execute_source_succession(&wrong, authority(), policy(), &stop)
            .is_err()
    );
    assert!(
        completed
            .source_succession_receipt(&wrong, authority())
            .unwrap()
            .is_none()
    );
    let committed_generation = match completed
        .execute_source_succession(
            &command,
            authority(),
            ReceiptPolicy {
                retention: Duration::ZERO,
                ..policy()
            },
            &stop,
        )
        .unwrap()
    {
        CommandOutcome::Committed {
            committed_generation,
            result,
            ..
        } => {
            assert_eq!(
                result["admission_digest"],
                command.payload()["admission_digest"]
            );
            assert_eq!(result["files"], 2);
            assert_eq!(result["contexts"], 1);
            committed_generation
        }
        other => panic!("genuine completed source did not commit: {other:?}"),
    };
    // Lost ACK and expired ordinary result require no old or new root access.
    let old_away = media.with_extension("completed-old-unavailable");
    let first_away = first.path().with_extension("completed-new-unavailable");
    std::fs::rename(&media, &old_away).unwrap();
    std::fs::rename(first.path(), &first_away).unwrap();
    assert!(matches!(
        completed
            .execute_source_succession(&command, denied, policy(), &stop)
            .unwrap(),
        CommandOutcome::Replay { .. }
    ));
    let receipt = completed
        .source_succession_receipt(&command, denied)
        .unwrap()
        .unwrap();
    assert_eq!(receipt.command(), &command);
    assert_eq!(receipt.effect_description()["file_count"], 2);
    assert_eq!(receipt.effect_description()["context_count"], 1);
    assert_eq!(
        receipt.effect_description()["admission_digest"],
        command.payload()["admission_digest"]
    );
    assert_eq!(
        completed.compact_source_receipt_for_test(&command).unwrap(),
        1
    );
    assert!(matches!(
        completed
            .execute_source_succession(&command, denied, policy(), &stop)
            .unwrap(),
        CommandOutcome::ReceiptExpired
    ));
    let expired = completed
        .source_succession_receipt(&command, denied)
        .unwrap()
        .unwrap();
    assert_eq!(expired.effect_description(), receipt.effect_description());
    // Same retained completed owner: export current state, not pristine40 delta.
    // Expired result and unavailable media roots do not erase permanent facts.
    let image = completed.into_current_image(&stop).unwrap();
    assert_eq!(image.generation(), committed_generation);
    assert_eq!(image.incarnation(), prepared.incarnation);
    assert_eq!(
        image.source_effects(),
        [receipt.effect_description().clone()]
    );
    completed = inspect_rotated_copy(image);

    let mut changed = command.payload().clone();
    changed["admission_digest"] = json!(catabolic_core::sha256("changed native fixture intent"));
    let changed = CommandEnvelope::new(
        1,
        CommandKey::new(&prepared.catalog_id, HOST, "completed-source-first").unwrap(),
        &prepared.incarnation,
        prepared.expected_generation,
        &changed.to_string(),
    )
    .unwrap();
    assert!(
        completed
            .source_succession_receipt(&changed, denied)
            .is_err()
    );
    assert!(matches!(
        completed
            .execute_source_succession(&changed, authority(), policy(), &stop)
            .unwrap(),
        CommandOutcome::Conflict
    ));
    std::fs::rename(&first_away, first.path()).unwrap();
    let next = completed
        .prepare_source_succession("default", "media", second.path(), &stop)
        .unwrap();
    assert_eq!(next.expected_generation, committed_generation);
    let command2 = make_command(&next, "completed-source-second");
    assert!(matches!(
        completed
            .execute_source_succession(&command2, authority(), policy(), &stop)
            .unwrap(),
        CommandOutcome::Committed { .. }
    ));
    let latest = completed
        .source_succession_receipt(&command2, authority())
        .unwrap()
        .unwrap();
    assert_eq!(latest.effect_description()["revision"], 2);
    // The delayed first ACK remains historical, not permission to regress head.
    assert_eq!(latest.effect_description()["file_count"], 2);
    assert_eq!(latest.effect_description()["context_count"], 1);
    let delayed = completed
        .source_succession_receipt(&command, denied)
        .unwrap()
        .unwrap();
    assert_eq!(delayed.effect_description(), receipt.effect_description());
    let image = completed.into_current_image(&stop).unwrap();
    assert_eq!(image.incarnation(), prepared.incarnation);
    assert_eq!(
        image.source_effects(),
        [
            receipt.effect_description().clone(),
            latest.effect_description().clone()
        ]
    );
    assert_eq!(image.current_inventory_sha256().len(), 64);
    let completed = inspect_rotated_copy(image);
    drop(completed);
    // A later-mutated catalog cannot masquerade as pristine offline Complete.
    let pristine = OfflineBootstrap::recover(
        &path,
        quiescence(),
        &plan,
        Recovery::CompletedReceiptOnly {
            command: &bootstrap,
        },
    );
    assert!(match pristine {
        Err(_) => true,
        Ok(owner) => owner.complete(&bootstrap).is_err(),
    });
    assert!(
        Store::open(&path, true, false).is_err(),
        "normal41 remains unregistered"
    );
}

#[test]
fn source_pre_effect_pass_enforces_continuity_and_restores_baseline() {
    let fixture = Fixture::new_relocation().install_candidate();
    let other_root = tempfile::tempdir().unwrap();
    let other_path = other_root.path().join("other.sqlite3");
    migration::initialize_native_at(&other_path, 40).unwrap();
    let other = Store::open(&other_path, true, false).unwrap();
    // This envelope selects a descriptive pre-effect pass only. No receipt,
    // source permit, byte equivalence or Motion actor grant is fabricated.
    let command = envelope(
        &fixture.store,
        "source-pass-fences",
        json!({"operation":"source_succession","profile":"default","source":"media"}),
    );
    for case in [
        "unchanged",
        "write",
        "write_rollback",
        "schema",
        "temp_schema",
        "commit",
        "rollback",
        "savepoint",
        "attach",
        "detach",
        "owner",
        "cancel",
        "unwind",
    ] {
        fixture
            .read(|store, budget| {
                crate::catalog_source_succession::assert_fence_for_test(
                    store, &command, &other, case, budget,
                )?;
                crate::catalog_source_succession::assert_preparation_fence_for_test(
                    store,
                    &fixture.media,
                    &other,
                    case,
                    budget,
                )
            })
            .unwrap_or_else(|error| panic!("{case}: {error}"));
    }
}

#[test]
fn genuine_media_library_scope_keeps_nonempty_selection_and_completed_owner_facts() {
    use crate::catalog_scope::SavedQueryRef;
    use crate::catalog_source_succession::{LibraryScopeSource, capture_host_library_scope};
    use crate::migration::offline_bootstrap::PreparedOfflineBootstrap;
    let Fixture {
        _root,
        path,
        media,
        store,
        ..
    } = Fixture::new_relocation();
    // Actual ordinary40 producers and two observed PCM files. Match the app's
    // Work -> Edition -> Version metadata/edge semantics, not an empty scope.
    curation::item_put(&store,&json!({"id":WORK,"kind":"video","metadata":{"title":"Scope media","motion_catalog":{"contract_version":1,"item_id":WORK,"kind":"video","external_ids":[]}}})).unwrap();
    const LIBRARY: &str = "scope-test-library";
    curation::item_put(&store,&json!({"id":format!("motion.library:{LIBRARY}"),"kind":"custom:motion_library","metadata":{"title":"Scope library","motion_library":{"contract_version":1,"id":LIBRARY,"catalog_revision":1,"name":"Scope library","kind":"mixed","language":"en","source_ids":["motion-source"]}}})).unwrap();
    let query = "SELECT DISTINCT w.item_id FROM catalog_items w JOIN catalog_relationships e ON e.source_id=w.item_id AND e.kind='custom:motion_edition' AND e.active=1 JOIN catalog_relationships v ON v.source_id=e.target_id AND v.kind='custom:motion_version' AND v.active=1 JOIN catalog_item_files a ON a.item_id=v.target_id AND a.active=1 WHERE json_type(w.metadata,'$.motion_catalog')='object' AND w.item_id=json_extract(w.metadata,'$.motion_catalog.item_id') AND a.profile='default' AND a.location IN ('media') ORDER BY w.item_id";
    let saved = crate::definitions::query_put(
        &store,
        "default",
        "library-scope-fixture",
        &json!({"version":1,"mode":"selection","selection":{"language":"sql","query":query}}),
    )
    .unwrap();
    let reference = SavedQueryRef::new(
        saved["id"].as_str().unwrap().into(),
        saved["revision"].as_i64().unwrap(),
        saved["digest"].as_str().unwrap().into(),
    )
    .unwrap();
    let selectors = vec![LibraryScopeSource::new("motion-source".into(), "media".into()).unwrap()];
    let before =
        capture_host_library_scope(&store, "default", LIBRARY, &reference, &selectors).unwrap();
    assert_eq!(before.definition()["catalog_revision"], 1);
    assert_eq!(before.sources().len(), 1);
    let read = Store::open(&path, false, false).unwrap();
    let selected = crate::sql::execute_store(
        &read,
        Some(query),
        &crate::sql::Options {
            profile: "default",
            stable: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(selected["complete"], true);
    assert_eq!(selected["rows"], json!([[WORK]]));
    drop(read);
    let wrong =
        vec![LibraryScopeSource::new("wrong-motion-source".into(), "media".into()).unwrap()];
    assert!(capture_host_library_scope(&store, "default", LIBRARY, &reference, &wrong).is_err());
    let broad=crate::definitions::query_put(&store,"default","library-broad-fixture",&json!({"version":1,"mode":"selection","selection":{"language":"sql","query":"SELECT item_id FROM catalog_items"}})).unwrap();
    let broad = SavedQueryRef::new(
        broad["id"].as_str().unwrap().into(),
        broad["revision"].as_i64().unwrap(),
        broad["digest"].as_str().unwrap().into(),
    )
    .unwrap();
    assert!(capture_host_library_scope(&store, "default", LIBRARY, &broad, &selectors).is_err());
    drop(store);
    let offline = PreparedOfflineBootstrap::prepare(
        &path,
        migration::NativeUpgradeQuiescence {
            admission_stopped: true,
            readers_and_workers_stopped: true,
            exclusive_operational_ownership: true,
        },
    )
    .unwrap()
    .apply(None)
    .unwrap();
    let payload = offline.prepare(HOST, &[]).unwrap();
    let command = CommandEnvelope::new(
        1,
        CommandKey::new(offline.catalog_id(), HOST, "completed-library-scope").unwrap(),
        offline.incarnation(),
        offline.generation().unwrap(),
        &payload.to_string(),
    )
    .unwrap();
    assert!(matches!(
        offline.execute(&command, HOST, &[], policy()).unwrap(),
        CommandOutcome::Committed { .. }
    ));
    let completed = offline.into_completed_catalog(&command).unwrap();
    let proof = completed
        .library_scope("default", LIBRARY, &reference, &selectors)
        .unwrap();
    assert_eq!(proof.facts().item_revision(), before.item_revision());
    assert_eq!(proof.facts().definition(), before.definition());
    assert_eq!(proof.facts().sources(), before.sources());
    assert!(proof.facts().generation() > before.generation());
    proof.revalidate().unwrap();
    assert!(
        completed
            .library_scope("default", LIBRARY, &reference, &wrong)
            .is_err()
    );
    assert!(
        completed
            .library_scope("default", LIBRARY, &broad, &selectors)
            .is_err()
    );
    // No filesystem availability predicate belongs in a descriptive scope proof.
    std::fs::rename(&media, media.with_extension("offline")).unwrap();
    proof.revalidate().unwrap();
    drop(proof);
    drop(completed);
}

#[test]
fn genuine_completed_library_file_endpoints_follow_two_hops_without_read_grant() {
    use crate::catalog_scope::SavedQueryRef;
    use crate::catalog_source_succession::{
        FileEndpointRequest, LibraryScopeSource, capture_host_library_scope,
    };
    use crate::migration::offline_bootstrap::PreparedOfflineBootstrap;
    use catabolic_core::catalog_identity::ExactFileRevision;
    use std::sync::atomic::AtomicBool;
    let Fixture {
        _root,
        path,
        media,
        store,
        ..
    } = Fixture::new_relocation();
    // Actual ordinary40 producers and two observed PCM files. Match the app's
    // Work -> Edition -> Version metadata/edge semantics, not an empty scope.
    curation::item_put(&store,&json!({"id":WORK,"kind":"video","metadata":{"title":"Scope media","motion_catalog":{"contract_version":1,"item_id":WORK,"kind":"video","external_ids":[]}}})).unwrap();
    const LIBRARY: &str = "scope-test-library";
    curation::item_put(&store,&json!({"id":format!("motion.library:{LIBRARY}"),"kind":"custom:motion_library","metadata":{"title":"Scope library","motion_library":{"contract_version":1,"id":LIBRARY,"catalog_revision":1,"name":"Scope library","kind":"mixed","language":"en","source_ids":["motion-source"]}}})).unwrap();
    let query = "SELECT DISTINCT w.item_id FROM catalog_items w JOIN catalog_relationships e ON e.source_id=w.item_id AND e.kind='custom:motion_edition' AND e.active=1 JOIN catalog_relationships v ON v.source_id=e.target_id AND v.kind='custom:motion_version' AND v.active=1 JOIN catalog_item_files a ON a.item_id=v.target_id AND a.active=1 WHERE json_type(w.metadata,'$.motion_catalog')='object' AND w.item_id=json_extract(w.metadata,'$.motion_catalog.item_id') AND a.profile='default' AND a.location IN ('media') ORDER BY w.item_id";
    let saved = crate::definitions::query_put(
        &store,
        "default",
        "library-scope-fixture",
        &json!({"version":1,"mode":"selection","selection":{"language":"sql","query":query}}),
    )
    .unwrap();
    let reference = SavedQueryRef::new(
        saved["id"].as_str().unwrap().into(),
        saved["revision"].as_i64().unwrap(),
        saved["digest"].as_str().unwrap().into(),
    )
    .unwrap();
    let selectors = vec![LibraryScopeSource::new("motion-source".into(), "media".into()).unwrap()];
    let before =
        capture_host_library_scope(&store, "default", LIBRARY, &reference, &selectors).unwrap();
    assert_eq!(before.definition()["catalog_revision"], 1);
    assert_eq!(before.sources().len(), 1);
    let read = Store::open(&path, false, false).unwrap();
    let selected = crate::sql::execute_store(
        &read,
        Some(query),
        &crate::sql::Options {
            profile: "default",
            stable: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(selected["complete"], true);
    assert_eq!(selected["rows"], json!([[WORK]]));
    drop(read);

    crate::catalog_source_succession::assert_occurrence_capture_parity_for_test(
        &store, "default", "media",
    );
    let original =
        crate::catalog_import_typed::capture_source_occurrences(&store, "default", "media")
            .unwrap();
    let files = original["occurrences"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    let requests: Vec<_> = files
        .iter()
        .map(|file| {
            FileEndpointRequest::new(
                ExactFileRevision {
                    file_id: file["id"].as_str().unwrap().into(),
                    revision: file["exact_revision"].as_str().unwrap().into(),
                },
                file["ctime_ns"].as_i64().unwrap(),
            )
            .unwrap()
        })
        .collect();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    for part in 1..=2 {
        for target in [first.path(), second.path()] {
            std::fs::copy(
                media.join(format!("part-{part}.mkv")),
                target.join(format!("part-{part}.mkv")),
            )
            .unwrap();
        }
    }
    drop(store);
    let quiescence = || migration::NativeUpgradeQuiescence {
        admission_stopped: true,
        readers_and_workers_stopped: true,
        exclusive_operational_ownership: true,
    };
    let offline = PreparedOfflineBootstrap::prepare(&path, quiescence())
        .unwrap()
        .apply(None)
        .unwrap();
    let payload = offline.prepare(HOST, &[]).unwrap();
    let bootstrap = CommandEnvelope::new(
        1,
        CommandKey::new(offline.catalog_id(), HOST, "endpoint-bootstrap").unwrap(),
        offline.incarnation(),
        offline.generation().unwrap(),
        &payload.to_string(),
    )
    .unwrap();
    assert!(matches!(
        offline.execute(&bootstrap, HOST, &[], policy()).unwrap(),
        CommandOutcome::Committed { .. }
    ));
    let mut completed = offline.into_completed_catalog(&bootstrap).unwrap();
    {
        let scope = completed
            .library_scope("default", LIBRARY, &reference, &selectors)
            .unwrap();
        assert!(
            scope.file_endpoint("media", &requests[0]).is_err(),
            "zero-hop is explicitly unsupported"
        );
        // A separate genuine empty Complete is not allowed to reuse this scope.
        let foreign_root = tempfile::tempdir().unwrap();
        let foreign_path = foreign_root.path().join("foreign.db");
        migration::initialize_native_at(&foreign_path, 40).unwrap();
        let foreign = PreparedOfflineBootstrap::prepare(&foreign_path, quiescence())
            .unwrap()
            .apply(None)
            .unwrap();
        let payload = foreign.prepare(HOST, &[]).unwrap();
        let command = CommandEnvelope::new(
            1,
            CommandKey::new(foreign.catalog_id(), HOST, "foreign-endpoint-bootstrap").unwrap(),
            foreign.incarnation(),
            foreign.generation().unwrap(),
            &payload.to_string(),
        )
        .unwrap();
        assert!(matches!(
            foreign.execute(&command, HOST, &[], policy()).unwrap(),
            CommandOutcome::Committed { .. }
        ));
        let foreign = foreign.into_completed_catalog(&command).unwrap();
        scope.assert_foreign_completed_owner_refused_for_test(&foreign);
        // Separate genuine operations: this oracle/negative coverage does not
        // add comparison work to either endpoint acceptance call below.
        scope.assert_fresh_endpoint_facts_for_test(&foreign, "media", &requests[0]);
    }
    let stop = AtomicBool::new(false);
    for (index, candidate) in [first.path(), second.path()].into_iter().enumerate() {
        let prepared = completed
            .prepare_source_succession("default", "media", candidate, &stop)
            .unwrap();
        let operation = format!("endpoint-hop-{}", index + 1);
        let command=CommandEnvelope::new(1,CommandKey::new(&prepared.catalog_id,HOST,&operation).unwrap(),
            &prepared.incarnation,prepared.expected_generation,&json!({"operation":"source_succession",
                "profile":prepared.profile,"source":prepared.source,"candidate_root":prepared.candidate_root,
                "content_commitment":prepared.content_commitment,
                "admission_digest":catabolic_core::sha256(format!("endpoint fixture intent {operation}"))}).to_string()).unwrap();
        assert!(matches!(
            completed
                .execute_source_succession(&command, authority(), policy(), &stop)
                .unwrap(),
            CommandOutcome::Committed { .. }
        ));
        let receipt = completed
            .source_succession_receipt(&command, authority())
            .unwrap()
            .unwrap();
        assert_eq!(receipt.effect_description()["file_count"], 2);
        assert_eq!(receipt.effect_description()["context_count"], 1);
        let scope = completed
            .library_scope("default", LIBRARY, &reference, &selectors)
            .unwrap();
        assert_eq!(scope.facts().item_revision(), before.item_revision());
        assert_eq!(scope.facts().definition(), before.definition());
        assert_eq!(scope.facts().query(), &reference);
        for (request, original) in requests.iter().zip(files) {
            let endpoint = scope.file_endpoint("media", request).unwrap();
            let facts = endpoint.facts();
            assert_eq!(facts.request(), request);
            assert_eq!(facts.first_succession_revision(), 1);
            assert_eq!(facts.last_succession_revision(), (index + 1) as i64);
            assert_eq!(facts.current_snapshot()["path"], original["path"]);
            assert_eq!(facts.current_snapshot()["root"], prepared.candidate_root);
            assert_eq!(facts.current_snapshot()["location"], "media");
            assert_eq!(facts.bytes(), original["size"].as_i64().unwrap());
            let bytes = std::fs::read(media.join(original["path"].as_str().unwrap())).unwrap();
            assert_eq!(facts.content_sha256(), catabolic_core::sha256(bytes));
            assert_ne!(facts.current_revision(), request.exact().revision);
            assert!(
                !facts
                    .current_snapshot()
                    .as_object()
                    .unwrap()
                    .contains_key("ctime_ns")
            );
            endpoint.revalidate().unwrap();
        }
        let wrong_revision = FileEndpointRequest::new(
            ExactFileRevision {
                file_id: requests[0].exact().file_id.clone(),
                revision: "f".repeat(64),
            },
            requests[0].ctime_ns(),
        )
        .unwrap();
        assert!(scope.file_endpoint("media", &wrong_revision).is_err());
        let wrong_ctime =
            FileEndpointRequest::new(requests[0].exact().clone(), requests[0].ctime_ns() + 1)
                .unwrap();
        assert!(scope.file_endpoint("media", &wrong_ctime).is_err());
        assert!(scope.file_endpoint("foreign-source", &requests[0]).is_err());
        // Controlled current-row faults, not an admitted public closed scan.
        scope.assert_terminal_corruption_refused_for_test("media", &requests[0]);
        scope.assert_history_corruption_refused_for_test("media", &requests[0], (index + 1) as i64);
        let endpoint = scope.file_endpoint("media", &requests[0]).unwrap();
        let unavailable = candidate.with_extension("temporarily-offline");
        std::fs::rename(candidate, &unavailable).unwrap();
        endpoint.revalidate().unwrap();
        std::fs::rename(&unavailable, candidate).unwrap();
    }
    // Baseline selection above was genuinely nonempty. These later facts are
    // descriptive transformations, not a new schema41 query/read/open grant.
}
