//! Pure effect tests only. Actual native refresh transaction/receipt tests live
//! with the candidate refresh owner; these constructors do not prove DB custody.
use super::*;
use catabolic_core::catalog_command::CommandKey;

fn schema() -> FieldSchema {
    let exact = FieldPolicy {
        value_type: ValueType::Json,
        nullable: false,
        normalization: Normalization::Exact,
    };
    FieldSchema::new_v2(
        vec![("title".into(), exact), ("unknown".into(), exact)],
        vec![("motion_catalog".into(), exact)],
    )
    .unwrap()
}
fn command() -> CommandEnvelope {
    CommandEnvelope::new(
        1,
        CommandKey::new("catalog", "host", "refresh").unwrap(),
        "incarnation",
        1,
        r#"{"operation":"nfo_refresh"}"#,
    )
    .unwrap()
}
fn marker(value: &str) -> Value {
    json!({"contract_version":1,"item_id":"work","media_type":"movie","unrelated":{"keep":[3,2,1]},
        "external_ids":[{"provider":"other-z","namespace_version":"v9","value":"opaque"},
            {"provider":"nfo","namespace_version":"","value":value},
            {"provider":"other-a","value":"also opaque","extra":true}]})
}
fn document(namespace: &str, record: &str, kind: SourceKind) -> ContributionDocument {
    ContributionDocument::from_json_with_system_fields(&schema(),ContributionKey::new("work",namespace,record).unwrap(),kind,7,
        &json!({"motion_catalog":marker("old"),"title":"Original title","unknown":{"nested":"retained"}}).to_string(),
        r#"{"exact":"evidence","array":[2,1]}"#).unwrap()
}
fn grant(doc: &ContributionDocument) -> MetadataAuthority {
    MetadataAuthority {
        host_namespace: "host".into(),
        subject: "work".into(),
        access: MetadataAccess::SourceV2 {
            namespace: doc.key().namespace().into(),
            record_id: doc.key().record_id().into(),
            kind: doc.kind(),
            system_fields: ["motion_catalog".into()].into(),
        },
    }
}
#[test]
fn imported_scan_and_mixed_local_documents_preserve_unrelated_values_evidence_and_order() {
    let scan = document("motion.catalog", "work", SourceKind::Scan);
    let local = document("motion.local", "document", SourceKind::Local);
    let docs = vec![scan, local];
    let grants = docs.iter().map(grant).collect::<Vec<_>>();
    let changed = plan(
        &schema(),
        &docs,
        &command(),
        "work",
        Some("old"),
        None,
        Some("new"),
        &ProjectionGrants {
            sources: &grants,
            legacy_reconciliation: None,
        },
    )
    .unwrap();
    assert_eq!(changed.len(), 2);
    for old in &docs {
        let next = changed.iter().find(|next| next.key() == old.key()).unwrap();
        let mut expected = old.fields().clone();
        expected.get_mut("motion_catalog").unwrap()["external_ids"][1]["value"] = json!("new");
        assert_eq!(next.fields(), &expected);
        assert_eq!(next.evidence(), old.evidence());
        assert_eq!(next.revision(), 8);
        assert_eq!(next.kind(), old.kind());
    }
}
#[test]
fn shadowed_source_requires_its_own_exact_field_grant_and_noop_does_not_advance() {
    let docs = vec![
        document("motion.catalog", "work", SourceKind::Scan),
        document("motion.local", "document", SourceKind::Local),
    ];
    let local = grant(&docs[1]);
    assert!(
        plan(
            &schema(),
            &docs,
            &command(),
            "work",
            Some("old"),
            None,
            Some("new"),
            &ProjectionGrants {
                sources: &[local],
                legacy_reconciliation: None
            }
        )
        .is_err()
    );
    let changed = plan(
        &schema(),
        &docs,
        &command(),
        "work",
        Some("old"),
        None,
        Some("old"),
        &ProjectionGrants {
            sources: &[],
            legacy_reconciliation: None,
        },
    )
    .unwrap();
    assert!(changed.is_empty());
}
#[test]
fn retained_legacy_requires_reconciliation_and_distinct_exact_structural_source() {
    let legacy = document(LEGACY_NAMESPACE, LEGACY_RECORD_ID, SourceKind::LegacyImport);
    let reconcile = MetadataAuthority {
        host_namespace: "host".into(),
        subject: "work".into(),
        access: MetadataAccess::ReconcileLegacy,
    };
    let structural = MetadataAuthority {
        host_namespace: "host".into(),
        subject: "work".into(),
        access: MetadataAccess::SourceV2 {
            namespace: "motion.structure".into(),
            record_id: "source_projection".into(),
            kind: SourceKind::Scan,
            system_fields: ["motion_catalog".into()].into(),
        },
    };
    assert!(
        plan(
            &schema(),
            &[legacy.clone()],
            &command(),
            "work",
            Some("old"),
            None,
            Some("new"),
            &ProjectionGrants {
                sources: &[structural.clone()],
                legacy_reconciliation: None
            }
        )
        .is_err()
    );
    let changed = plan(
        &schema(),
        &[legacy.clone()],
        &command(),
        "work",
        Some("old"),
        None,
        Some("new"),
        &ProjectionGrants {
            sources: &[structural],
            legacy_reconciliation: Some(&reconcile),
        },
    )
    .unwrap();
    let retained = changed
        .iter()
        .find(|d| d.kind() == SourceKind::LegacyImport)
        .unwrap();
    let mut expected = legacy.fields().clone();
    expected.remove("motion_catalog");
    assert_eq!(retained.fields(), &expected);
    assert_eq!(retained.evidence(), legacy.evidence());
    let replacement = changed
        .iter()
        .find(|d| d.kind() == SourceKind::Scan)
        .unwrap();
    assert_eq!(replacement.fields().len(), 1);
    assert_eq!(
        replacement.fields()["motion_catalog"]["external_ids"][1]["value"],
        "new"
    );
}
#[test]
fn pin_and_unknown_identity_provenance_refuse_without_mutating_inputs() {
    let doc = document("motion.catalog", "work", SourceKind::Scan);
    let original = doc.clone();
    let grants = [grant(&doc)];
    for (prior, pin, next) in [
        (Some("old"), Some("old"), Some("new")),
        (Some("unproved"), None, Some("new")),
    ] {
        assert!(
            plan(
                &schema(),
                &[doc.clone()],
                &command(),
                "work",
                prior,
                pin,
                next,
                &ProjectionGrants {
                    sources: &grants,
                    legacy_reconciliation: None
                }
            )
            .is_err()
        );
    }
    assert_eq!(doc, original);
}

#[test]
fn classification_requires_shadowed_source_authority_even_for_unchanged_identity() {
    let docs = vec![
        document("motion.structure", "source_projection", SourceKind::Scan),
        document("motion.local", "document", SourceKind::Local),
    ];
    let complete = docs.iter().map(grant).collect::<Vec<_>>();
    let only_local = [grant(&docs[1])];
    let partial = ProjectionGrants {
        sources: &only_local,
        legacy_reconciliation: None,
    };
    // Existing already-classified projection has no field effect here. Policy
    // classification is a separate change and still requires every contributor.
    assert!(
        plan(
            &schema(),
            &docs,
            &command(),
            "work",
            Some("old"),
            None,
            Some("old"),
            &partial,
        )
        .unwrap()
        .is_empty()
    );
    assert!(require_classification(&docs, &command(), "work", &partial).is_err());
    assert!(
        require_classification(
            &docs,
            &command(),
            "work",
            &ProjectionGrants {
                sources: &complete,
                legacy_reconciliation: None,
            }
        )
        .is_ok()
    );
    let mut foreign = complete;
    foreign[0].subject = "other-work".into();
    assert!(
        require_classification(
            &docs,
            &command(),
            "work",
            &ProjectionGrants {
                sources: &foreign,
                legacy_reconciliation: None,
            }
        )
        .is_err()
    );
}
