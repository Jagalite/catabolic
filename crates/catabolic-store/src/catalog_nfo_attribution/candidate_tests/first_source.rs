//! Real native40 create/init owners followed by the explicitly unregistered41
//! test installer, empty-inventory bootstrap and actual outer receipt engine.
//! The controlled host owns this entire archive and independently audits its
//! empty automatic-identity/sidecar inventory. This is not a production audit
//! builder, and a missing source row never substitutes for that host grant.
use super::*;
use crate::catalog_metadata_store::{MetadataChange, MetadataMutation};
use crate::catalog_nfo_attribution::first_source::{
    self as first, FirstNfoSourceAudit, VerifiedFirstNfoAbsence,
};

struct FirstFixture {
    _root: tempfile::TempDir,
    store: Store,
    creation: CommandEnvelope,
    managed: bool,
}
fn first_initialization() -> MetadataAuthority {
    grant(MetadataAccess::InitializeV2 {
        system_fields: ["motion_catalog".into()].into(),
    })
}
impl FirstFixture {
    fn new(managed: bool, expire_creation: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("first.db");
        migration::initialize_native_at(&path, 40).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let creation = envelope(
            &store,
            "fresh-motion-create",
            json!({"operation":"item_create_batch","items":[{"id":SUBJECT,"kind":"video","metadata":{"title":"Original","motion_catalog":{"contract_version":1,"item_id":SUBJECT,"kind":"video","external_ids":[]}}}]}),
        );
        let mut creation_policy = policy();
        if expire_creation {
            creation_policy.retention = Duration::ZERO;
        }
        committed(commands::execute(&store, &creation, authority(true), creation_policy).unwrap());
        if managed {
            let initialize = MetadataInitializeV2 {
                contract_version: 2,
                subject: SUBJECT.into(),
                policies: vec![
                    PolicyField {
                        field: "motion_catalog".into(),
                        value_type: PolicyValueType::Json,
                        nullable: false,
                        normalization: PolicyNormalization::Exact,
                    },
                    PolicyField {
                        field: "title".into(),
                        value_type: PolicyValueType::String,
                        nullable: false,
                        normalization: PolicyNormalization::TrimAsciiLowercase,
                    },
                ],
                system_fields: ["motion_catalog".into()].into(),
            };
            let command = envelope(
                &store,
                "managed-init",
                json!({"operation":"metadata_initialize_v2","request":initialize}),
            );
            let init = first_initialization();
            committed(
                commands::execute_with_authority(
                    &store,
                    &command,
                    CatalogAuthority::new(authority(true))
                        .with_metadata(&init)
                        .with_metadata_initialization(&init),
                    policy(),
                )
                .unwrap(),
            );
            let reconcile = MetadataMutation {
                contract_version: 1,
                change: MetadataChange::ReconcileLegacy {
                    subject: SUBJECT.into(),
                    schema_fingerprint: metadata::current_schema_fingerprint(&store.db, SUBJECT)
                        .unwrap()
                        .unwrap(),
                    expected_revision: 1,
                    fields: vec!["title".into()],
                },
            };
            let command = envelope(
                &store,
                "managed-reconcile",
                json!({"operation":"metadata","mutation":reconcile}),
            );
            let access = grant(MetadataAccess::ReconcileLegacy);
            committed(
                commands::execute_with_authority(
                    &store,
                    &command,
                    CatalogAuthority::new(authority(true)).with_metadata(&access),
                    policy(),
                )
                .unwrap(),
            );
            let mut scan = put_request(&store, 0, json!({"title":"Original"}), &[]);
            scan.namespace = "motion.scan".into();
            replace_40(&store, "managed-scan", &scan, SourceKind::Scan);
            let mut local = put_request(&store, 0, json!({}), &[]);
            local.namespace = "motion.local".into();
            local.excluded_tags = vec!["hidden".into()];
            replace_40(&store, "managed-local", &local, SourceKind::Local);
            committed(
                pin(
                    &store,
                    "independent-manual-pin",
                    SUBJECT,
                    0,
                    PinDecision::Set {
                        value: "manual:choice".into(),
                    },
                )
                .unwrap(),
            );
        }
        if expire_creation {
            assert!(
                commands::compact_receipts(
                    &store,
                    commands::TrustedReceiptMaintenance {
                        host_namespace: HOST,
                        holder_namespace: "first-fixture-maintenance"
                    },
                    100
                )
                .unwrap()
                    > 0
            );
        }
        let before = migration::snapshot(&store.db).unwrap();
        drop(store);
        candidate::install_candidate41_for_test(&path).unwrap();
        assert!(!migration::is_supported_native_schema(41));
        let store = candidate::open_candidate41_for_test(&path, true).unwrap();
        migration::preservation(&store.db, &before).unwrap();
        let plan = store
            .transaction_detailed(|_| bootstrap::prepare_in(&store, &[]))
            .unwrap();
        let command = envelope(&store, "bootstrap-empty", plan.payload());
        let result = committed(
            commands::execute_candidate_for_test(
                &store,
                &command,
                authority(true),
                policy(),
                move |store, command, _| bootstrap::apply_in(store, command, plan),
            )
            .unwrap(),
        );
        assert_eq!(result["source_count"], 0);
        Self {
            _root: root,
            store,
            creation,
            managed,
        }
    }
    fn request(&self, fields: Value, names: &[&str]) -> ContributionReplace {
        let mut request = put_request(&self.store, 0, fields, names);
        if !self.managed {
            request.origin_creation_intent = Some(self.creation.canonical_payload().into());
        }
        request
    }
    fn audit(&self, command: &CommandEnvelope) -> FirstNfoSourceAudit {
        FirstNfoSourceAudit {
            catalog_id:self.store.database_id.clone(),incarnation:self.store.catalog_incarnation.clone().unwrap(),generation:command.expected_generation(),host_namespace:HOST.into(),subject:SUBJECT.into(),outer_digest:command.digest().as_str().into(),creation_digest:self.creation.digest().as_str().into(),
            complete_absence_audit_digest:catabolic_core::sha256(json!({"domain":"candidate.complete-first-archive.v1","creation":self.creation.digest().as_str(),"subject":SUBJECT,"automatic_identities":[],"sidecar_origins":[]}).to_string()),
        }
    }
    fn prepare(&self, store: &Store, command: &CommandEnvelope) -> Result<VerifiedFirstNfoAbsence> {
        let current = state::read_in(store, SUBJECT, command.expected_generation(), AGGREGATE)?;
        first::prepare_in(
            store,
            command,
            current,
            &self.creation,
            authority(false),
            &self.audit(command),
        )
    }
    fn execute(
        &self,
        command: &CommandEnvelope,
        request: &ContributionReplace,
    ) -> commands::Result<CommandOutcome> {
        commands::execute_candidate_for_test(
            &self.store,
            command,
            authority(true),
            policy(),
            |store, command, at| {
                let first = self.prepare(store, command)?;
                let published = public_put::apply_first_put_in(
                    store,
                    command,
                    request,
                    0,
                    first,
                    &source_grant(),
                    &first_initialization(),
                    &grant(MetadataAccess::ExtendSchema),
                    &tags(),
                    at,
                )?;
                Ok(
                    json!({"source_revision":published.source_revision(),"attribution_revision":published.attribution_revision(),"presence":published.presence(),"source_proof_digest":published.source_proof_digest()}),
                )
            },
        )
    }
    fn current(&self) -> state::CurrentNfoState {
        self.store
            .transaction_detailed(|_| {
                state::read_in(&self.store, SUBJECT, generation(&self.store), AGGREGATE)
            })
            .unwrap()
    }
}

#[test]
fn first_fresh_put_initializes_reconciles_origin_publishes_once_and_replays_after_restart() {
    let mut f = FirstFixture::new(false, false);
    assert!(f.current().native_source().is_none());
    let request = f.request(json!({"title":"New NFO","extra":7}), &["new"]);
    let command = put_command(&f.store, "first-put", 0, &request);
    let result = committed(f.execute(&command, &request).unwrap());
    assert_eq!(result["source_revision"], 1);
    assert_eq!(result["attribution_revision"], 1);
    assert_eq!(result["presence"]["incarnation"], 1);
    assert_eq!(result["presence"]["public_revision"], 1);
    let source_json = |namespace: &str, record: &str| -> Value {
        let raw:String=f.store.db.query_row("SELECT fields_json FROM catalog_metadata_sources WHERE item_id=? AND namespace=? AND record_id=?",[SUBJECT,namespace,record],|r|r.get(0)).unwrap();
        serde_json::from_str(&raw).unwrap()
    };
    assert!(
        source_json(metadata::LEGACY_NAMESPACE, metadata::LEGACY_RECORD_ID)
            .get("title")
            .is_none()
    );
    assert_eq!(
        source_json("motion.scan", "origin"),
        json!({"title":"Original"})
    );
    let scan_evidence:String=f.store.db.query_row("SELECT evidence_json FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.scan' AND record_id='origin'",[SUBJECT],|r|r.get(0)).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&scan_evidence).unwrap(),
        json!({"owner":"motion.receipt-origin.v1","creation_intent_digest":catabolic_core::sha256(f.creation.canonical_payload())})
    );
    let initial:String=f.store.db.query_row("SELECT fields_json FROM catalog_metadata_history WHERE item_id=? AND namespace=? AND record_id=? AND revision=1",[SUBJECT,metadata::LEGACY_NAMESPACE,metadata::LEGACY_RECORD_ID],|r|r.get(0)).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&initial).unwrap()["title"],
        "Original"
    );
    let effective: String = f
        .store
        .db
        .query_row("SELECT metadata FROM items WHERE id=?", [SUBJECT], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&effective).unwrap()["title"],
        "New NFO"
    );
    let current = f.current();
    let attribution = current.presence().head.attribution.as_ref().unwrap();
    assert!(attribution.origin.is_none() && attribution.active_identity.is_none());
    assert!(current.origin_physical().is_none() && current.origin_proof_digest().is_none());
    assert_eq!(
        current.attribution_audit_digest(),
        Some(f.audit(&command).complete_absence_audit_digest.as_str())
    );
    let after = migration::snapshot(&f.store.db).unwrap();
    let path = f.store.path.clone();
    drop(f.store);
    f.store = candidate::open_candidate41_for_test(&path, true).unwrap();
    assert_eq!(f.current().native_source().unwrap().revision(), 1);
    assert!(matches!(
        commands::execute_candidate_for_test(
            &f.store,
            &command,
            authority(false),
            policy(),
            |_, _, _| panic!("replay must precede first-source preparation")
        )
        .unwrap(),
        CommandOutcome::Replay { .. }
    ));
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), after);
}

#[test]
fn first_managed_put_uses_actual_normalization_and_retains_pin_and_local_suppression() {
    let f = FirstFixture::new(true, false);
    let schema = f.current().schema().unwrap().fingerprint().to_owned();
    let request = f.request(json!({"title":"  NEW NFO  "}), &["hidden"]);
    let command = put_command(&f.store, "managed-first-put", 0, &request);
    committed(f.execute(&command, &request).unwrap());
    let current = f.current();
    assert_eq!(current.schema().unwrap().fingerprint(), schema);
    assert_eq!(
        current.native_source().unwrap().fields()["title"],
        "new nfo"
    );
    assert!(
        matches!(&current.presence().head.pin,catabolic_core::catalog_metadata::nfo_refresh::PinHead::Decision {revision:1,value:Some(value)} if value=="manual:choice")
    );
    assert!(
        current
            .presence()
            .head
            .attribution
            .as_ref()
            .unwrap()
            .active_identity
            .is_none()
    );
    assert_eq!(f.store.db.query_row("SELECT count(*) FROM catalog_metadata_tag_suppressions WHERE item_id=? AND namespace='motion.local'",[SUBJECT],|r|r.get::<_,i64>(0)).unwrap(),1);
    assert!(
        matches!(&current.presence().head.source.as_ref().unwrap().document,DocumentState::Present {contribution,external_identity:None} if contribution.tags.contains("hidden"))
    );
}

#[test]
fn first_initialization_error_swallowed_before_source_write_still_fails_real_commit() {
    let f = FirstFixture::new(false, false);
    let before = migration::snapshot(&f.store.db).unwrap();
    let mut request = f.request(json!({"title":"New"}), &[]);
    // Bounded, duplicate-safe JSON passes permit admission, but the metadata
    // evidence owner rejects its non-object shape AFTER nested initialization.
    request.evidence_json = "[]".into();
    let command = put_command(&f.store, "first-invalid-evidence", 0, &request);
    let attempted = std::cell::Cell::new(false);
    let result=f.store.transaction_detailed::<_,Error>(|db| {
        let first=f.prepare(&f.store,&command)?;
        let permit=permit::prepare_first_put_in(&f.store,first,&command,&request,0,&first_initialization())?;
        let effect=metadata::replace_nfo_contribution(&f.store,&request,&command,&source_grant(),&first_initialization(),&grant(MetadataAccess::ExtendSchema),&tags(),123,&permit);
        assert!(effect.is_err());
        assert_eq!(db.query_row("SELECT count(*) FROM catalog_metadata_subjects WHERE item_id=?",[SUBJECT],|r|r.get::<_,i64>(0))?,1);
        assert_eq!(db.query_row("SELECT count(*) FROM catalog_metadata_history WHERE item_id=? AND namespace=? AND revision=1",[SUBJECT,metadata::LEGACY_NAMESPACE],|r|r.get::<_,i64>(0))?,1);
        assert_eq!(db.query_row("SELECT count(*) FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.nfo'",[SUBJECT],|r|r.get::<_,i64>(0))?,0);
        let barrier:(String,i64)=db.query_row("SELECT incarnation,barrier_id FROM catalog_metadata_permits WHERE item_id=?",[SUBJECT],|r|Ok((r.get(0)?,r.get(1)?)))?;
        assert_eq!(barrier,(command.admission_incarnation().into(),1));
        drop(permit); attempted.set(true); Ok(())
    });
    assert!(attempted.get());
    assert!(
        result
            .unwrap_err()
            .0
            .to_ascii_lowercase()
            .contains("foreign key")
    );
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
}

#[test]
fn first_source_dropped_permit_or_verified_token_cannot_commit_metadata_half() {
    for drop_verified in [false, true] {
        let f = FirstFixture::new(false, false);
        let before = migration::snapshot(&f.store.db).unwrap();
        let request = f.request(json!({"title":"New"}), &["new"]);
        let command = put_command(&f.store, "first-drop", 0, &request);
        let attempted = std::cell::Cell::new(false);
        let result=f.store.transaction_detailed::<_,Error>(|db| {
            let first=f.prepare(&f.store,&command)?;
            let permit=permit::prepare_first_put_in(&f.store,first,&command,&request,0,&first_initialization())?;
            metadata::replace_nfo_contribution(&f.store,&request,&command,&source_grant(),&first_initialization(),&grant(MetadataAccess::ExtendSchema),&tags(),123,&permit).map_err(|e|Error(e.to_string()))?;
            if drop_verified { drop(permit.verify_applied_in(&f.store,&command)?); } else { drop(permit); }
            assert_eq!(db.query_row("SELECT revision FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.nfo'",[SUBJECT],|r|r.get::<_,i64>(0))?,1);
            assert_eq!(db.query_row("SELECT count(*) FROM catalog_nfo_states WHERE item_id=?",[SUBJECT],|r|r.get::<_,i64>(0))?,0);
            attempted.set(true); Ok(())
        });
        assert!(attempted.get());
        assert!(
            result
                .unwrap_err()
                .0
                .to_ascii_lowercase()
                .contains("foreign key")
        );
        assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    }
}

#[test]
fn first_source_requires_exact_independent_audit_and_ordinary_source_grant() {
    let f = FirstFixture::new(false, false);
    let request = f.request(json!({"title":"New"}), &[]);
    let command = put_command(&f.store, "first-refused", 0, &request);
    let before = migration::snapshot(&f.store.db).unwrap();
    let result = f.store.transaction_detailed::<_, Error>(|_| {
        let current = state::read_in(&f.store, SUBJECT, generation(&f.store), AGGREGATE)?;
        assert!(permit::prepare_put_in(&f.store, current, &command, &request, 0).is_err());
        for changed in 0..8 {
            let mut audit = f.audit(&command);
            match changed {
                0 => audit.catalog_id = "other-catalog".into(),
                1 => audit.incarnation = "other-incarnation".into(),
                2 => audit.generation += 1,
                3 => audit.host_namespace = "other-host".into(),
                4 => audit.subject = "other-subject".into(),
                5 => audit.outer_digest = "0".repeat(64),
                6 => audit.creation_digest = "0".repeat(64),
                7 => audit.complete_absence_audit_digest.clear(),
                _ => unreachable!(),
            }
            let current = state::read_in(&f.store, SUBJECT, generation(&f.store), AGGREGATE)?;
            assert!(
                first::prepare_in(
                    &f.store,
                    &command,
                    current,
                    &f.creation,
                    authority(false),
                    &audit
                )
                .is_err(),
                "audit field {changed}"
            );
        }
        for intent in [None, Some("{}".to_owned())] {
            let mut wrong = request.clone();
            wrong.origin_creation_intent = intent;
            let command = put_command(&f.store, "wrong-origin-intent", 0, &wrong);
            let first = f.prepare(&f.store, &command)?;
            assert!(
                permit::prepare_first_put_in(
                    &f.store,
                    first,
                    &command,
                    &wrong,
                    0,
                    &first_initialization()
                )
                .is_err()
            );
        }
        Ok(())
    });
    result.unwrap();
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    let wrong = grant(MetadataAccess::SourceV2 {
        namespace: NFO.into(),
        record_id: RECORD.into(),
        kind: SourceKind::Provider,
        system_fields: BTreeSet::new(),
    });
    assert!(
        commands::execute_candidate_for_test(
            &f.store,
            &command,
            authority(true),
            policy(),
            |store, command, at| {
                let first = f.prepare(store, command)?;
                public_put::apply_first_put_in(
                    store,
                    command,
                    &request,
                    0,
                    first,
                    &wrong,
                    &first_initialization(),
                    &grant(MetadataAccess::ExtendSchema),
                    &tags(),
                    at,
                )?;
                Ok(json!({}))
            }
        )
        .is_err()
    );
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
}

#[test]
fn first_unmanaged_source_refuses_compacted_creation_body_without_effects() {
    let f = FirstFixture::new(false, true);
    let request = f.request(json!({"title":"New"}), &[]);
    let command = put_command(&f.store, "first-expired-origin", 0, &request);
    let before = migration::snapshot(&f.store.db).unwrap();
    let error = f
        .store
        .transaction_detailed::<_, Error>(|_| {
            f.prepare(&f.store, &command)?;
            Ok(())
        })
        .unwrap_err();
    assert!(error.0.contains("expired creation result"), "{error}");
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
}
