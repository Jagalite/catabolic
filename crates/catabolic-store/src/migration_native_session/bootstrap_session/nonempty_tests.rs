//! Real40 typed owners followed by the actual offline migration/bootstrap owner.
//! This constructed host archive has no automatic identity or physical origin.
//! No receipt, immutable history, or authority is synthesized by SQL.
use super::*;
use crate::{
    catalog_commands::CatalogAuthority,
    catalog_metadata_store::{
        self as metadata, ContributionReplace, MetadataAccess, MetadataAuthority,
        MetadataInitializeV2, PolicyField, PolicyNormalization, PolicyValueType,
    },
    catalog_nfo_attribution::{NfoCreatedSourceAuthority, bootstrap_entry::BaselineEvidence},
    catalog_tag_mutation::{TagAuthority, TagSubject},
};
use catabolic_core::{catalog_command::CommandKey, catalog_metadata::SourceKind};
use std::{collections::BTreeSet, time::Duration};
const HOST: &str = "offline-nonempty-archive";
const SUBJECT: &str = "created-nfo";
fn authority() -> TrustedCommandAuthority<'static> {
    TrustedCommandAuthority {
        host_namespace: HOST,
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: true,
        identity_authority: None,
    }
}
fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::from_secs(3600),
        max_receipts: 100,
        max_hold_records: 100,
        max_logical_bytes: 16 * 1024 * 1024,
    }
}
fn envelope(store: &Store, id: &str, payload: Value) -> CommandEnvelope {
    let generation = store
        .db
        .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
    CommandEnvelope::new(
        1,
        CommandKey::new(&store.database_id, HOST, id).unwrap(),
        store.catalog_incarnation.as_deref().unwrap(),
        generation,
        &payload.to_string(),
    )
    .unwrap()
}
fn grant(access: MetadataAccess) -> MetadataAuthority {
    MetadataAuthority {
        host_namespace: HOST.into(),
        subject: SUBJECT.into(),
        access,
    }
}
struct Fixture {
    _root: tempfile::TempDir,
    session: OfflineBootstrapSession,
    first: CommandEnvelope,
    audit: NfoCreatedSourceAuthority,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("nonempty.db");
        initialize_native_at(&path, 40).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let create = envelope(
            &store,
            "create",
            json!({"operation":"item_put","item":{"id":SUBJECT,"kind":"video","metadata":{}}}),
        );
        assert!(matches!(
            catalog_commands::execute(&store, &create, authority(), policy()).unwrap(),
            CommandOutcome::Committed { .. }
        ));
        let initialize = grant(MetadataAccess::InitializeV2 {
            system_fields: BTreeSet::new(),
        });
        let request = MetadataInitializeV2 {
            contract_version: 2,
            subject: SUBJECT.into(),
            policies: vec![PolicyField {
                field: "title".into(),
                value_type: PolicyValueType::String,
                nullable: false,
                normalization: PolicyNormalization::TrimAsciiLowercase,
            }],
            system_fields: BTreeSet::new(),
        };
        let init = envelope(
            &store,
            "initialize",
            json!({"operation":"metadata_initialize_v2","request":request}),
        );
        assert!(matches!(
            catalog_commands::execute_with_authority(
                &store,
                &init,
                CatalogAuthority::new(authority())
                    .with_metadata(&initialize)
                    .with_metadata_initialization(&initialize),
                policy()
            )
            .unwrap(),
            CommandOutcome::Committed { .. }
        ));
        let request = ContributionReplace { contract_version: 2, subject: SUBJECT.into(), namespace: "motion.nfo".into(), record_id: "document".into(),
            expected_schema_fingerprint: metadata::current_schema_fingerprint(&store.db, SUBJECT).unwrap(), expected_revision: 0,
            fields_json: json!({"title":" Genuine Native Source "}).to_string(),
            evidence_json: json!({"owner":"motion.metadata.replace","request_digest":"a".repeat(64),"expected_aggregate_revision":7}).to_string(),
            tags: vec![], excluded_tags: vec![], origin_creation_intent: None };
        let first = envelope(
            &store,
            "first-source",
            json!({"operation":"metadata_contribution_replace","request":request}),
        );
        let source = grant(MetadataAccess::Source {
            namespace: "motion.nfo".into(),
            record_id: "document".into(),
            kind: SourceKind::Provider,
        });
        let extension = grant(MetadataAccess::ExtendSchema);
        let tags = TagAuthority {
            host_namespace: HOST.into(),
            allowed_subjects: [TagSubject::Item(SUBJECT.into())].into(),
            allow_global_merge: false,
        };
        assert!(matches!(
            catalog_commands::execute_with_authority(
                &store,
                &first,
                CatalogAuthority::new(authority())
                    .with_metadata(&source)
                    .with_metadata_initialization(&initialize)
                    .with_metadata_schema(&extension)
                    .with_tags(&tags),
                policy()
            )
            .unwrap(),
            CommandOutcome::Committed { .. }
        ));
        let audit = NfoCreatedSourceAuthority { host_namespace: HOST.into(), subject: SUBJECT.into(), request_digest: "a".repeat(64),
            complete_absence_audit_digest: sha256(json!({"domain":"offline.fixture.complete-created-archive.v1","subject":SUBJECT,"creation":create.digest().as_str(),"first":first.digest().as_str(),"automatic_identities":[],"sidecar_origins":[]}).to_string()) };
        drop(store);
        let owner = RetainedNativeUpgrade::acquire(
            &path,
            NativeUpgradeQuiescence {
                admission_stopped: true,
                readers_and_workers_stopped: true,
                exclusive_operational_ownership: true,
            },
        )
        .unwrap();
        let (owner, plan) = owner.plan_closed41().unwrap();
        let (owner, _) = owner
            .apply_closed41(&plan, None, &|_, _, _| Ok(()))
            .unwrap();
        Self {
            _root: root,
            session: owner.into_bootstrap().unwrap(),
            first,
            audit,
        }
    }
    fn bootstrap(&self) -> CommandEnvelope {
        let evidence = [SourceEvidence {
            baseline: BaselineEvidence::NativeCreated {
                first: &self.first,
                audit: &self.audit,
            },
            later_commands: &[],
        }];
        let payload = self.session.prepare(HOST, &evidence).unwrap();
        let command = envelope(&self.session.store, "bootstrap", payload);
        assert!(self.session.reconcile(&command, HOST).unwrap().is_none());
        assert!(matches!(
            self.session
                .execute(&command, HOST, &evidence, policy())
                .unwrap(),
            CommandOutcome::Committed { .. }
        ));
        command
    }
}
#[test]
fn genuine_nonempty_source_bootstraps_completes_and_receipt_replays() {
    let f = Fixture::new();
    let command = f.bootstrap();
    let complete = f.session.complete(&command).unwrap();
    assert_eq!(complete.source_count(), 1);
    assert_eq!(
        complete.committed_generation(),
        command.expected_generation() + 3
    );
    assert_eq!(
        complete.inventory_digest(),
        command.payload()["inventory_digest"].as_str().unwrap()
    );
    let digest = complete.state_digest().to_owned();
    drop(complete);
    assert!(matches!(
        f.session.execute(&command, HOST, &[], policy()).unwrap(),
        CommandOutcome::Replay { .. }
    ));
    assert_eq!(f.session.complete(&command).unwrap().state_digest(), digest);
    let source: String = f.session.store.db.query_row("SELECT fields_json FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.nfo'", [SUBJECT], |r| r.get(0)).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&source).unwrap()["title"],
        "genuine native source"
    );
}
#[test]
fn actual_nonempty_completion_refuses_changed_prior_and_bootstrap_entity_heads() {
    for domain in ["items", "catalog_nfo_states"] {
        let f = Fixture::new();
        let command = f.bootstrap();
        f.session.complete(&command).unwrap();
        let changed = f.session.store.db.execute("UPDATE catalog_entity_revisions SET revision=revision+1 WHERE entity_kind=? AND entity_key=json_array(?)", rusqlite::params![domain,SUBJECT]).unwrap();
        assert_eq!(changed, 1);
        assert!(
            f.session.complete(&command).is_err(),
            "changed {domain} head must not complete"
        );
    }
}
#[test]
fn actual_nonempty_receipt_and_source_tampering_are_denied_without_disabling_guards() {
    let f = Fixture::new();
    let command = f.bootstrap();
    let before = snapshot(&f.session.store.db).unwrap();
    assert!(f.session.store.db.execute("UPDATE catalog_command_receipts SET payload_digest=? WHERE catalog_id=? AND host_namespace=? AND operation_id=?",
        rusqlite::params!["b".repeat(64),f.first.key().catalog_id(),HOST,f.first.key().operation_id()]).is_err());
    assert!(f.session.store.db.execute("UPDATE catalog_metadata_sources SET fields_json=? WHERE item_id=? AND namespace='motion.nfo'",
        rusqlite::params![r#"{"title":"forged"}"#,SUBJECT]).is_err());
    assert_eq!(snapshot(&f.session.store.db).unwrap(), before);
    assert_eq!(f.session.complete(&command).unwrap().source_count(), 1);
}
