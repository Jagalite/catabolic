use super::*;
use crate::{
    Error,
    catalog_commands::{
        self as commands, CatalogAuthority, CommandOutcome, ReceiptPolicy,
        TrustedReceiptMaintenance,
    },
    catalog_metadata_store::{
        self as metadata, MetadataAccess, MetadataAuthority, MetadataChange, MetadataMutation,
    },
    catalog_nfo_attribution::verify_source_history_in,
    migration,
    operation_budget::{BudgetLimits, OperationBudget},
};
use catabolic_core::catalog_command::{CommandKey, ReceiptVisibility};
use serde_json::{Value, json};
use std::{collections::BTreeSet, time::Duration};

const HOST: &str = "nfo-v1-compound";
const SUBJECT: &str = "subject";
fn authority(write: bool) -> TrustedCommandAuthority<'static> {
    TrustedCommandAuthority {
        host_namespace: HOST,
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: write,
        identity_authority: None,
    }
}
fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::ZERO,
        max_receipts: 100,
        max_hold_records: 100,
        max_logical_bytes: 16 * 1024 * 1024,
    }
}
fn generation(store: &Store) -> i64 {
    store
        .db
        .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap()
}
fn envelope(store: &Store, id: &str, payload: Value) -> CommandEnvelope {
    CommandEnvelope::new(
        1,
        CommandKey::new(&store.database_id, HOST, id).unwrap(),
        store.catalog_incarnation.as_deref().unwrap(),
        generation(store),
        &payload.to_string(),
    )
    .unwrap()
}
fn evidence() -> Value {
    json!({"owner":"motion.metadata.replace","request_digest":"a".repeat(64),"expected_aggregate_revision":11})
}
fn legacy() -> Value {
    json!({"title":" Original Legacy ","retained":[true,{"n":1}],"motion_catalog":{"owned":"catalog"}})
}
fn edits() -> Value {
    json!({"title":{"op":"set","value":" NFO TITLE "},"plot":{"op":"set","value":["a",2]}})
}
fn policies(legacy: &Value, edits: &Value) -> Vec<PolicyField> {
    legacy
        .as_object()
        .unwrap()
        .keys()
        .chain(edits.as_object().unwrap().keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|field| PolicyField {
            field,
            value_type: PolicyValueType::Json,
            nullable: false,
            normalization: PolicyNormalization::Exact,
        })
        .collect()
}
fn source_grant(kind: SourceKind) -> MetadataAuthority {
    MetadataAuthority {
        host_namespace: HOST.into(),
        subject: SUBJECT.into(),
        access: MetadataAccess::Source {
            namespace: "motion.nfo".into(),
            record_id: "document".into(),
            kind,
        },
    }
}
fn later_patch(store: &Store) -> CommandEnvelope {
    let mutation = MetadataMutation {
        contract_version: 1,
        change: MetadataChange::Patch {
            subject: SUBJECT.into(),
            namespace: "motion.nfo".into(),
            record_id: "document".into(),
            schema_fingerprint: metadata::current_schema_fingerprint(&store.db, SUBJECT)
                .unwrap()
                .unwrap(),
            expected_revision: 1,
            edits_json: json!({"title":{"op":"set","value":"Later"},"plot":{"op":"remove"}})
                .to_string(),
            evidence_json: evidence().to_string(),
        },
    };
    let command = envelope(
        store,
        "later",
        json!({"operation":"metadata","mutation":mutation}),
    );
    assert!(matches!(
        commands::execute_with_authority(
            store,
            &command,
            CatalogAuthority::new(authority(true))
                .with_metadata(&source_grant(SourceKind::Provider)),
            policy()
        )
        .unwrap(),
        CommandOutcome::Committed { .. }
    ));
    command
}

struct Fixture {
    _root: tempfile::TempDir,
    store: Store,
    first: CommandEnvelope,
    later: Vec<CommandEnvelope>,
    legacy_raw: String,
}
impl Fixture {
    fn new(later: bool) -> Self {
        Self::custom(
            legacy(),
            edits(),
            evidence(),
            None,
            SourceKind::Provider,
            later,
        )
    }
    fn custom(
        legacy: Value,
        edits: Value,
        supplied_evidence: Value,
        supplied_policies: Option<Vec<PolicyField>>,
        kind: SourceKind,
        later: bool,
    ) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("native39.db");
        migration::initialize_native_at(&path, 39).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let policies = supplied_policies.unwrap_or_else(|| policies(&legacy, &edits));
        let seed = envelope(
            &store,
            "seed",
            json!({"operation":"item_put","item":{"id":SUBJECT,"kind":"video","metadata":legacy}}),
        );
        commands::execute(&store, &seed, authority(true), policy()).unwrap();
        let legacy_raw = store
            .db
            .query_row("SELECT metadata FROM items WHERE id=?", [SUBJECT], |r| {
                r.get::<_, String>(0)
            })
            .unwrap();
        // This invokes the actual compound dispatcher with two separate trusted
        // owner grants. No history/receipt inserts or trigger bypass are used.
        let first = envelope(
            &store,
            "first",
            json!({"operation":"metadata_initialize_and_patch","subject":SUBJECT,
                "namespace":"motion.nfo","record_id":"document","policies":policies,
                "edits_json":serde_json::to_string_pretty(&edits).unwrap(),
                "evidence_json":serde_json::to_string_pretty(&supplied_evidence).unwrap()}),
        );
        let initialization = MetadataAuthority {
            host_namespace: HOST.into(),
            subject: SUBJECT.into(),
            access: MetadataAccess::Initialize,
        };
        assert!(matches!(
            commands::execute_with_authority(
                &store,
                &first,
                CatalogAuthority::new(authority(true))
                    .with_metadata_initialization(&initialization)
                    .with_metadata(&source_grant(kind)),
                policy()
            )
            .unwrap(),
            CommandOutcome::Committed { .. }
        ));
        let later = if later {
            vec![later_patch(&store)]
        } else {
            vec![]
        };
        let before = history(&store);
        let old_generation = generation(&store);
        let old_incarnation = store.catalog_incarnation.clone();
        drop(store);
        // Upgrade through the official native migration owner, preserving the
        // genuine native39 records and their outer command receipt.
        migration::upgrade_native(
            &path,
            &migration::plan_native(&path).unwrap(),
            migration::NativeUpgradeQuiescence {
                admission_stopped: true,
                readers_and_workers_stopped: true,
                exclusive_operational_ownership: true,
            },
            None,
            &|_, _, _| Ok(()),
        )
        .unwrap();
        let store = Store::open(&path, true, false).unwrap();
        assert_eq!(store.schema_version, 40);
        assert_eq!(store.catalog_incarnation, old_incarnation);
        assert_eq!(generation(&store), old_generation);
        assert_eq!(history(&store), before);
        Self {
            _root: root,
            store,
            first,
            later,
            legacy_raw,
        }
    }
    fn grant(&self) -> NfoCreatedSourceAuthority {
        NfoCreatedSourceAuthority {
            host_namespace: HOST.into(),
            subject: SUBJECT.into(),
            request_digest: "a".repeat(64),
            complete_absence_audit_digest: "b".repeat(64),
        }
    }
    fn verify(
        &self,
        command: &CommandEnvelope,
        grant: &NfoCreatedSourceAuthority,
    ) -> Result<VerifiedNfoBaseline> {
        self.store.transaction_detailed(|_| {
            verify_initialize_and_patch_in(
                &self.store,
                command,
                grant,
                authority(false),
                generation(&self.store),
            )
        })
    }
    fn baseline(&self) -> VerifiedNfoBaseline {
        self.verify(&self.first, &self.grant()).unwrap()
    }
    fn assert_history(&self, baseline: &VerifiedNfoBaseline) {
        self.store
            .transaction_detailed(|_| {
                let source = verify_source_history_in(
                    &self.store,
                    baseline,
                    &self.later,
                    authority(false),
                    generation(&self.store),
                )?;
                assert_eq!(
                    source.current_source().revision(),
                    1 + self.later.len() as i64
                );
                assert_eq!(source.automatic_identity(), None);
                assert_eq!(source.origin(), None);
                assert_eq!(
                    source.attribution_audit_digest(),
                    self.grant().complete_absence_audit_digest
                );
                Ok::<_, Error>(())
            })
            .unwrap();
    }
    fn synthetic_mutations(&self) -> (MetadataMutation, MetadataMutation) {
        let request: Request = serde_json::from_value(self.first.payload().clone()).unwrap();
        let fingerprint: String = self
            .store
            .db
            .query_row(
                "SELECT schema_fingerprint FROM catalog_metadata_subjects WHERE item_id=?",
                [SUBJECT],
                |r| r.get(0),
            )
            .unwrap();
        (
            MetadataMutation {
                contract_version: 1,
                change: MetadataChange::Initialize {
                    subject: request.subject.clone(),
                    policies: request.policies,
                },
            },
            MetadataMutation {
                contract_version: 1,
                change: MetadataChange::Patch {
                    subject: request.subject,
                    namespace: request.namespace,
                    record_id: request.record_id,
                    schema_fingerprint: fingerprint,
                    expected_revision: 0,
                    edits_json: request.edits_json,
                    evidence_json: request.evidence_json,
                },
            },
        )
    }
}
type HistoryRow = (String, String, i64, String, String, String, String, String);
fn history(store: &Store) -> Vec<HistoryRow> {
    store.db.prepare("SELECT namespace,record_id,revision,source_kind,schema_fingerprint,fields_json,evidence_json,raw_mutation_json FROM catalog_metadata_history WHERE item_id=? ORDER BY namespace,record_id,revision").unwrap()
        .query_map([SUBJECT],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).unwrap()
        .collect::<std::result::Result<Vec<_>,_>>().unwrap()
}

#[test]
fn compound_owner_retains_exact_initialize_and_patch_raw_under_one_outer_receipt() {
    let f = Fixture::new(false);
    let before = history(&f.store);
    let baseline = f.baseline();
    let (initialize, patch) = f.synthetic_mutations();
    let legacy_raw: String = f
        .store
        .db
        .query_row(
            "SELECT legacy_raw_json FROM catalog_metadata_subjects WHERE item_id=?",
            [SUBJECT],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(legacy_raw, f.legacy_raw);
    let current: String = f
        .store
        .db
        .query_row("SELECT metadata FROM items WHERE id=?", [SUBJECT], |r| {
            r.get(0)
        })
        .unwrap();
    assert_ne!(serde_json::from_str::<Value>(&current).unwrap(), legacy());
    let rows = history(&f.store);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].7, serde_json::to_string(&initialize).unwrap());
    assert_eq!(
        baseline.raw_mutation_json,
        serde_json::to_string(&patch).unwrap()
    );
    assert_ne!(baseline.raw_mutation_json, f.first.payload().to_string());
    assert_eq!(baseline.command, f.first);
    assert_eq!(baseline.revision, 1);
    assert_eq!(
        serde_json::from_str::<Value>(&baseline.fields_json).unwrap(),
        json!({"title":" NFO TITLE ","plot":["a",2]})
    );
    assert_eq!(
        serde_json::from_str::<Value>(&baseline.evidence_json).unwrap(),
        evidence()
    );
    let receipts: i64 = f.store.db.query_row("SELECT count(*) FROM catalog_command_receipts WHERE catalog_id=? AND host_namespace=? AND operation_id=?",params![f.first.key().catalog_id(),HOST,"first"],|r|r.get(0)).unwrap();
    assert_eq!(receipts, 1);
    f.assert_history(&baseline);
    assert_eq!(history(&f.store), before);
}

#[test]
fn explicit_policy_order_and_normalization_are_reconstructed_without_inference() {
    let mut supplied = policies(&legacy(), &edits());
    let title = supplied.iter_mut().find(|p| p.field == "title").unwrap();
    title.value_type = PolicyValueType::String;
    title.normalization = PolicyNormalization::TrimAsciiLowercase;
    supplied.reverse();
    let f = Fixture::custom(
        legacy(),
        edits(),
        evidence(),
        Some(supplied.clone()),
        SourceKind::Provider,
        false,
    );
    let baseline = f.baseline();
    let policies_raw: String = f
        .store
        .db
        .query_row(
            "SELECT policies_json FROM catalog_metadata_subjects WHERE item_id=?",
            [SUBJECT],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(policies_raw, serde_json::to_string(&supplied).unwrap());
    assert_eq!(
        serde_json::from_str::<Value>(&baseline.fields_json).unwrap()["title"],
        "nfo title"
    );
    let legacy_fields: String = f.store.db.query_row("SELECT fields_json FROM catalog_metadata_history WHERE item_id=? AND namespace=? AND revision=1",params![SUBJECT,LEGACY_NAMESPACE],|r|r.get(0)).unwrap();
    // LegacyImport deliberately retains original values; provider policy
    // normalization must not rewrite unreconciled legacy provenance.
    assert_eq!(
        serde_json::from_str::<Value>(&legacy_fields).unwrap()["title"],
        " Original Legacy "
    );
    assert_eq!(
        serde_json::from_str::<Value>(&f.legacy_raw).unwrap()["title"],
        " Original Legacy "
    );
    f.assert_history(&baseline);
}

#[test]
fn empty_first_source_is_present_revision_one_with_nonempty_legacy_schema() {
    let f = Fixture::custom(
        legacy(),
        json!({}),
        evidence(),
        None,
        SourceKind::Provider,
        false,
    );
    let baseline = f.baseline();
    assert_eq!(baseline.fields_json, "{}");
    assert_eq!(baseline.revision, 1);
    f.assert_history(&baseline);
}

#[test]
fn later_patch_and_compacted_outer_receipt_preserve_baseline_not_replay_authority() {
    let f = Fixture::new(true);
    let baseline = f.baseline();
    f.assert_history(&baseline);
    assert!(
        commands::compact_receipts(
            &f.store,
            TrustedReceiptMaintenance {
                host_namespace: HOST,
                holder_namespace: "nfo-compound-maintenance"
            },
            100
        )
        .unwrap()
            > 0
    );
    let recovered = f.baseline();
    assert_eq!(recovered.raw_mutation_json, baseline.raw_mutation_json);
    f.assert_history(&recovered);
    for command in std::iter::once(&f.first).chain(&f.later) {
        assert!(matches!(
            commands::reconcile_receipt(&f.store, command, authority(false)).unwrap(),
            Some(CommandOutcome::ReceiptExpired)
        ));
    }
    assert!(f.verify(&f.later[0], &f.grant()).is_err());
}

#[test]
fn native_remove_reserved_or_nonprovider_effects_do_not_prove_public_nfo_creation() {
    for (edits, kind) in [
        (json!({"title":{"op":"remove"}}), SourceKind::Provider),
        (
            json!({"motion_private":{"op":"set","value":"private"}}),
            SourceKind::Provider,
        ),
        (edits(), SourceKind::Local),
    ] {
        let f = Fixture::custom(legacy(), edits, evidence(), None, kind, false);
        assert!(f.verify(&f.first, &f.grant()).is_err());
    }
}

#[test]
fn independently_audited_closed_evidence_is_required_even_with_real_compound_receipt() {
    for supplied in [
        json!({}),
        json!({"owner":"motion.metadata.replace","request_digest":"a".repeat(64),"expected_aggregate_revision":11,"external_id":"not-proof"}),
    ] {
        let f = Fixture::custom(
            legacy(),
            edits(),
            supplied,
            None,
            SourceKind::Provider,
            false,
        );
        assert!(f.verify(&f.first, &f.grant()).is_err());
    }
    let f = Fixture::new(false);
    for field in 0..4 {
        let mut grant = f.grant();
        match field {
            0 => grant.host_namespace = "other".into(),
            1 => grant.subject = "other".into(),
            2 => grant.request_digest = "c".repeat(64),
            _ => grant.complete_absence_audit_digest.clear(),
        }
        assert!(f.verify(&f.first, &grant).is_err());
    }
    f.baseline();
}

#[test]
fn synthetic_children_same_key_tampering_and_unrelated_receipts_are_not_outer_proof() {
    let f = Fixture::new(false);
    let (initialize, patch) = f.synthetic_mutations();
    for mutation in [initialize, patch] {
        let command = CommandEnvelope::new(
            1,
            f.first.key().clone(),
            f.first.admission_incarnation(),
            f.first.expected_generation(),
            &json!({"operation":"metadata","mutation":mutation}).to_string(),
        )
        .unwrap();
        assert!(f.verify(&command, &f.grant()).is_err());
    }
    for field in ["policies", "edits_json", "evidence_json", "subject"] {
        let mut changed = f.first.payload().clone();
        match field {
            "policies" => changed[field].as_array_mut().unwrap().reverse(),
            "subject" => changed[field] = json!("other"),
            _ => changed[field] = json!("{}"),
        }
        let command = CommandEnvelope::new(
            1,
            f.first.key().clone(),
            f.first.admission_incarnation(),
            f.first.expected_generation(),
            &changed.to_string(),
        )
        .unwrap();
        assert!(f.verify(&command, &f.grant()).is_err());
    }
    let unrelated = envelope(
        &f.store,
        "other-command",
        json!({"operation":"item_put","item":{"id":"other","kind":"video","metadata":{}}}),
    );
    commands::execute(&f.store, &unrelated, authority(true), policy()).unwrap();
    assert!(f.verify(&unrelated, &f.grant()).is_err());
    f.assert_history(&f.baseline());
}

#[test]
fn retained_writer_exact_generation_and_full_receipt_scope_are_required() {
    let f = Fixture::new(false);
    assert!(
        verify_initialize_and_patch_in(
            &f.store,
            &f.first,
            &f.grant(),
            authority(false),
            generation(&f.store)
        )
        .is_err()
    );
    for denied in [
        authority(true),
        TrustedCommandAuthority {
            receipt_visibility: ReceiptVisibility::CommittedStatusOnly,
            ..authority(false)
        },
        TrustedCommandAuthority {
            host_namespace: "other",
            ..authority(false)
        },
    ] {
        assert!(
            f.store
                .transaction_detailed::<_, Error>(|_| verify_initialize_and_patch_in(
                    &f.store,
                    &f.first,
                    &f.grant(),
                    denied,
                    generation(&f.store)
                ))
                .is_err()
        );
    }
    assert!(
        f.store
            .transaction_detailed::<_, Error>(|_| verify_initialize_and_patch_in(
                &f.store,
                &f.first,
                &f.grant(),
                authority(false),
                generation(&f.store) + 1
            ))
            .is_err()
    );
}

#[test]
fn v1_field_boundary_and_inherited_budget_refusal_leave_history_unchanged() {
    let fields = (0..MAX_METADATA_FIELDS)
        .map(|i| (format!("field_{i:02}"), json!({"op":"set","value":i})))
        .collect::<serde_json::Map<_, _>>();
    let f = Fixture::custom(
        json!({}),
        Value::Object(fields),
        evidence(),
        None,
        SourceKind::Provider,
        false,
    );
    let baseline = f.baseline();
    assert_eq!(
        serde_json::from_str::<Value>(&baseline.fields_json)
            .unwrap()
            .as_object()
            .unwrap()
            .len(),
        MAX_METADATA_FIELDS
    );
    f.assert_history(&baseline);
    let before = history(&f.store);
    {
        let budget = OperationBudget::new(
            Duration::from_secs(30),
            BudgetLimits {
                items: 100,
                values: 100,
                bytes: 64,
                work: 100,
            },
        )
        .unwrap();
        let _guard = budget.enter().unwrap();
        assert!(f.verify(&f.first, &f.grant()).is_err());
        assert!(budget.failure().is_some());
    }
    assert_eq!(history(&f.store), before);
    f.baseline();
}
