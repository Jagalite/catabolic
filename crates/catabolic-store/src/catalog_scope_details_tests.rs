use super::*;
use crate::catalog_metadata_store::{
    MetadataAccess, MetadataAuthority, MetadataChange, MetadataMutation, PolicyField,
    PolicyNormalization, PolicyValueType,
};
use catabolic_core::catalog_metadata::SourceKind;

fn admitted(
    f: &Fixture,
    h: &HostAuthorization,
    selections: Vec<TrustedSelection>,
) -> (ScopedReadSession, ScopeHandle) {
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = s
        .admit_host_scope(
            TrustedHostReadGrant::new(h.clone(), "default".into(), selections, vec![]).unwrap(),
            &lease(h),
            budget(),
        )
        .unwrap();
    (s, cap)
}
fn selected(f: &Fixture, name: &str, ids: &str) -> TrustedSelection {
    TrustedSelection::new(
        f.saved(
            name,
            "selection",
            "sql",
            &format!("SELECT item_id FROM catalog_items WHERE item_id IN ({ids})"),
        ),
        vec!["title".into()],
    )
    .unwrap()
}
fn execute_metadata(f: &Fixture, item: &str, change: MetadataChange, access: MetadataAccess) {
    use crate::catalog_commands::{
        self as commands, CatalogAuthority, ReceiptPolicy, TrustedCommandAuthority,
    };
    use catabolic_core::catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility};
    let store = &f.writer;
    let generation = store
        .db
        .query_row("SELECT generation FROM catalog_state", [], |r| r.get(0))
        .unwrap();
    let mutation = MetadataMutation {
        contract_version: 1,
        change,
    };
    let command = CommandEnvelope::new(
        1,
        CommandKey::new(
            &store.database_id,
            "detail-fixture",
            uuid::Uuid::new_v4().to_string(),
        )
        .unwrap(),
        store.catalog_incarnation.as_deref().unwrap(),
        generation,
        &encode(&json!({"operation":"metadata","mutation":mutation})),
    )
    .unwrap();
    let authority = MetadataAuthority {
        host_namespace: "detail-fixture".into(),
        subject: item.into(),
        access,
    };
    commands::execute_with_authority(
        store,
        &command,
        CatalogAuthority::new(TrustedCommandAuthority {
            host_namespace: "detail-fixture",
            receipt_visibility: ReceiptVisibility::FullResult,
            allow_new_commands: true,
            identity_authority: None,
        })
        .with_metadata(&authority),
        ReceiptPolicy {
            retention: Duration::from_secs(3600),
            max_receipts: 1000,
            max_logical_bytes: 16 * 1024 * 1024,
            max_hold_records: 1000,
        },
    )
    .unwrap();
}
fn initialize(f: &Fixture, item: &str) -> String {
    let policies = [
        ("title", PolicyValueType::String),
        ("secret", PolicyValueType::String),
        ("year", PolicyValueType::Integer),
    ]
    .into_iter()
    .map(|(field, value_type)| PolicyField {
        field: field.into(),
        value_type,
        nullable: false,
        normalization: PolicyNormalization::Exact,
    })
    .collect();
    execute_metadata(
        f,
        item,
        MetadataChange::Initialize {
            subject: item.into(),
            policies,
        },
        MetadataAccess::Initialize,
    );
    f.writer
        .db
        .query_row(
            "SELECT schema_fingerprint FROM catalog_metadata_subjects WHERE item_id=?",
            [item],
            |r| r.get(0),
        )
        .unwrap()
}
fn contribution(
    f: &Fixture,
    item: &str,
    fingerprint: &str,
    namespace: &str,
    kind: SourceKind,
    field: &str,
    value: &str,
) {
    execute_metadata(
        f,
        item,
        MetadataChange::Patch {
            subject: item.into(),
            namespace: namespace.into(),
            record_id: "one".into(),
            schema_fingerprint: fingerprint.into(),
            expected_revision: 0,
            edits_json: encode(&json!({field:{"op":"set","value":value}})),
            evidence_json:
                r#"{"private_path":"/private/DO_NOT_DISCLOSE","other_subject":"hidden"}"#.into(),
        },
        MetadataAccess::Source {
            namespace: namespace.into(),
            record_id: "one".into(),
            kind,
        },
    );
}
fn conflict(f: &Fixture) {
    let fingerprint = initialize(f, "a");
    execute_metadata(
        f,
        "a",
        MetadataChange::ReconcileLegacy {
            subject: "a".into(),
            schema_fingerprint: fingerprint.clone(),
            expected_revision: 1,
            fields: vec!["title".into()],
        },
        MetadataAccess::ReconcileLegacy,
    );
    contribution(
        f,
        "a",
        &fingerprint,
        "provider-a",
        SourceKind::Provider,
        "title",
        "One",
    );
    contribution(
        f,
        "a",
        &fingerprint,
        "provider-b",
        SourceKind::Provider,
        "title",
        "Two",
    );
    contribution(
        f,
        "a",
        &fingerprint,
        "scan",
        SourceKind::Scan,
        "title",
        "Shadowed",
    );
    contribution(
        f,
        "a",
        &fingerprint,
        "private-source",
        SourceKind::Provider,
        "secret",
        "SECRET_FIELD_VALUE",
    );
}
fn provenance(f: &Fixture) -> TrustedSelection {
    selected(f, "provenance", "'a','b'")
        .with_provenance(TrustedProvenanceRead::new(vec!["title".into()]).unwrap())
}

#[test]
fn memberships_include_only_matching_explicitly_admitted_selection_labels() {
    let f = Fixture::at(36);
    let both = selected(&f, "Visible-library", "'a','b'").with_membership_read();
    let a = selected(&f, "Only-Alpha", "'a'").with_membership_read();
    let private = selected(&f, "DO_NOT_DISCLOSE", "'a','hidden'");
    let private_id = private.query.id.clone();
    let h = host();
    let (mut s, cap) = admitted(&f, &h, vec![both.clone(), a.clone()]);
    let out = read(
        &mut s,
        &cap,
        &h,
        ScopeReadRequest::GetMemberships("a".into()),
    )
    .unwrap();
    assert_eq!(out["scoped_total"], 2);
    let expected: BTreeSet<String> = [both.query.id.clone(), a.query.id].into();
    let actual: BTreeSet<String> = out["memberships"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["selection_id"].as_str().unwrap().into())
        .collect();
    assert_eq!(actual, expected);
    assert!(!encode(&out).contains(&private_id));
    assert!(!encode(&out).contains("DO_NOT_DISCLOSE"));
    assert!(!encode(&out).contains("SELECT"));
    assert!(!encode(&out).contains("digest"));
    let out = read(
        &mut s,
        &cap,
        &h,
        ScopeReadRequest::GetMemberships("b".into()),
    )
    .unwrap();
    assert_eq!(
        out["memberships"],
        json!([{"selection_id":both.query.id,"name":"Visible-library"}])
    );
    for id in ["hidden", "missing"] {
        assert_eq!(
            code(read(
                &mut s,
                &cap,
                &h,
                ScopeReadRequest::GetMemberships(id.into())
            )),
            ScopeErrorCode::NotFoundOrNotVisible
        );
    }
}
#[test]
fn membership_and_provenance_purposes_are_independent_of_metadata_and_search() {
    let f = Fixture::at(36);
    initialize(&f, "a");
    let plain = selected(&f, "plain", "'a','b'")
        .with_search_fields(TrustedSearchFields::new(vec!["provider".into()], true, true).unwrap());
    let allowed = selected(&f, "a-membership", "'a'").with_membership_read();
    let h = host();
    let (mut s, cap) = admitted(&f, &h, vec![plain, allowed]);
    assert!(read(&mut s, &cap, &h, ScopeReadRequest::GetItem("a".into())).is_ok());
    assert_eq!(
        code(read(
            &mut s,
            &cap,
            &h,
            ScopeReadRequest::GetMemberships("b".into())
        )),
        ScopeErrorCode::NotFoundOrNotVisible
    );
    assert_eq!(
        code(read(
            &mut s,
            &cap,
            &h,
            ScopeReadRequest::GetMetadataProvenance("a".into())
        )),
        ScopeErrorCode::NotFoundOrNotVisible
    );
    assert!(TrustedProvenanceRead::new(vec![]).is_err());
    assert!(TrustedProvenanceRead::new(vec!["x".into(); 65]).is_err());
}
#[test]
fn provenance_retains_conflicts_and_shadowed_contributions_without_hidden_fields_or_evidence() {
    let f = Fixture::at(34);
    conflict(&f);
    initialize(&f, "hidden");
    let h = host();
    let (mut s, cap) = admitted(&f, &h, vec![provenance(&f)]);
    let out = read(
        &mut s,
        &cap,
        &h,
        ScopeReadRequest::GetMetadataProvenance("a".into()),
    )
    .unwrap();
    assert_eq!(out["fields"]["title"]["state"], "conflict");
    assert_eq!(
        out["fields"]["title"]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(out["scoped_total"], 3);
    let sources: BTreeSet<String> = out["contributions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["source"]["namespace"].as_str().unwrap().into())
        .collect();
    assert_eq!(
        sources,
        ["provider-a".into(), "provider-b".into(), "scan".into()].into()
    );
    for hidden in [
        "secret",
        "SECRET_FIELD_VALUE",
        "private-source",
        "private_path",
        "DO_NOT_DISCLOSE",
        "schema_fingerprint",
        "inputs",
        "hidden",
    ] {
        assert!(!encode(&out).contains(hidden), "{hidden}");
    }
    for id in ["hidden", "missing"] {
        assert_eq!(
            code(read(
                &mut s,
                &cap,
                &h,
                ScopeReadRequest::GetMetadataProvenance(id.into())
            )),
            ScopeErrorCode::NotFoundOrNotVisible
        );
    }
    assert_eq!(
        code(read(
            &mut s,
            &cap,
            &h,
            ScopeReadRequest::GetMetadataProvenance("b".into())
        )),
        ScopeErrorCode::MetadataNotInitialized
    );
}
#[test]
fn provenance_masks_are_per_item_and_do_not_expand_resolved_metadata_visibility() {
    let f = Fixture::at(36);
    initialize(&f, "a");
    initialize(&f, "b");
    let a = selected(&f, "a-title", "'a'")
        .with_provenance(TrustedProvenanceRead::new(vec!["title".into()]).unwrap());
    let b = TrustedSelection::new(
        f.saved(
            "b-secret",
            "selection",
            "sql",
            "SELECT item_id FROM catalog_items WHERE item_id='b'",
        ),
        vec![],
    )
    .unwrap()
    .with_provenance(TrustedProvenanceRead::new(vec!["secret".into()]).unwrap());
    let h = host();
    let (mut s, cap) = admitted(&f, &h, vec![a, b]);
    let out = read(
        &mut s,
        &cap,
        &h,
        ScopeReadRequest::GetMetadataProvenance("a".into()),
    )
    .unwrap();
    assert_eq!(out["fields"]["title"]["value"], "Alpha");
    assert!(out["fields"]["secret"].is_null());
    let out = read(
        &mut s,
        &cap,
        &h,
        ScopeReadRequest::GetMetadataProvenance("b".into()),
    )
    .unwrap();
    assert_eq!(out["fields"]["secret"]["value"], "visible-secret-b");
    assert!(out["fields"]["title"].is_null());
    let detail = read(&mut s, &cap, &h, ScopeReadRequest::GetItem("b".into())).unwrap();
    assert_eq!(detail["result"]["rows"][0][2], "{}");
}
#[test]
fn real36_to37_upgrade_preserves_membership_and_provenance_with_stale_fencing() {
    let f = Fixture::at(36);
    conflict(&f);
    let selection = provenance(&f).with_membership_read();
    let f = f.upgrade37();
    let h = host();
    let (mut s, cap) = admitted(&f, &h, vec![selection.clone()]);
    assert_eq!(
        read(
            &mut s,
            &cap,
            &h,
            ScopeReadRequest::GetMemberships("a".into())
        )
        .unwrap()["scoped_total"],
        1
    );
    assert_eq!(
        read(
            &mut s,
            &cap,
            &h,
            ScopeReadRequest::GetMetadataProvenance("a".into())
        )
        .unwrap()["fields"]["title"]["state"],
        "conflict"
    );
    f.mutate("changed hidden item");
    for request in [
        ScopeReadRequest::GetMemberships("a".into()),
        ScopeReadRequest::GetMetadataProvenance("a".into()),
    ] {
        assert_eq!(
            code(read(&mut s, &cap, &h, request)),
            ScopeErrorCode::StaleScope
        );
    }
    let fresh = s
        .admit_host_scope(
            TrustedHostReadGrant::new(h.clone(), "default".into(), vec![selection], vec![])
                .unwrap(),
            &lease(&h),
            budget(),
        )
        .unwrap();
    assert!(
        read(
            &mut s,
            &fresh,
            &h,
            ScopeReadRequest::GetMetadataProvenance("a".into())
        )
        .is_ok()
    );
}
#[test]
fn provenance_source_budget_exhaustion_is_typed_and_cleanup_allows_retry() {
    let f = Fixture::at(36);
    conflict(&f);
    let h = host();
    let (mut s, cap) = admitted(
        &f,
        &h,
        vec![
            selected(&f, "one", "'a'")
                .with_provenance(TrustedProvenanceRead::new(vec!["title".into()]).unwrap()),
        ],
    );
    let result = s.read_host_scope(
        &cap,
        lease(&h),
        ScopeReadRequest::GetMetadataProvenance("a".into()),
        ReadBudget::new(Duration::from_secs(5), 1).unwrap(),
    );
    assert_eq!(code(result), ScopeErrorCode::IdBudget);
    assert!(s.store.as_ref().unwrap().db.is_autocommit());
    assert_eq!(
        read(
            &mut s,
            &cap,
            &h,
            ScopeReadRequest::GetMetadataProvenance("a".into())
        )
        .unwrap()["scoped_total"],
        3
    );
}
#[test]
fn detail_vm_budget_and_cancellation_never_return_partial_memberships_or_provenance() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let f = Fixture::at(36);
    conflict(&f);
    let h = host();
    let (mut s, cap) = admitted(&f, &h, vec![provenance(&f).with_membership_read()]);
    let work = OperationBudget::new(
        Duration::from_secs(5),
        BudgetLimits {
            items: 1000,
            values: 10000,
            bytes: MAX_BYTES,
            work: 1,
        },
    )
    .unwrap();
    let active = work.enter().unwrap();
    let poisoned = Cell::new(false);
    let result = s.fresh(|slot, _| {
        details::read(
            slot.as_ref().unwrap(),
            &cap.0,
            &cap.0.members,
            details::Request::Provenance("a".into()),
            Instant::now(),
            budget(),
            &poisoned,
        )
    });
    assert_eq!(code(result), ScopeErrorCode::WorkBudget);
    drop(active);
    assert!(!poisoned.get());
    let work = OperationBudget::new(
        Duration::from_secs(5),
        BudgetLimits {
            items: 1000,
            values: 10000,
            bytes: 1,
            work: 100000,
        },
    )
    .unwrap();
    let active = work.enter().unwrap();
    let result = s.fresh(|slot, _| {
        details::read(
            slot.as_ref().unwrap(),
            &cap.0,
            &cap.0.members,
            details::Request::Provenance("a".into()),
            Instant::now(),
            budget(),
            &poisoned,
        )
    });
    assert_eq!(code(result), ScopeErrorCode::ByteBudget);
    drop(active);
    let calls = AtomicUsize::new(0);
    let cancelled = crate::embedding::with_interrupt_check(
        move || calls.fetch_add(1, Ordering::Relaxed) > 50,
        || {
            Ok(read(
                &mut s,
                &cap,
                &h,
                ScopeReadRequest::GetMetadataProvenance("a".into()),
            ))
        },
    )
    .unwrap();
    assert_eq!(code(cancelled), ScopeErrorCode::Cancelled);
    assert!(s.store.as_ref().unwrap().db.is_autocommit());
    assert!(
        read(
            &mut s,
            &cap,
            &h,
            ScopeReadRequest::GetMemberships("a".into())
        )
        .is_ok()
    );
    assert!(
        read(
            &mut s,
            &cap,
            &h,
            ScopeReadRequest::GetMetadataProvenance("a".into())
        )
        .is_ok()
    );
}
#[test]
fn detail_delivery_holds_the_exact_gate_and_rejects_equal_label_rebinding() {
    let f = Fixture::at(36);
    let h = host();
    let (mut s, cap) = admitted(
        &f,
        &h,
        vec![selected(&f, "library", "'a'").with_membership_read()],
    );
    let other = host();
    assert_eq!(
        code(read(
            &mut s,
            &cap,
            &other,
            ScopeReadRequest::GetMemberships("a".into())
        )),
        ScopeErrorCode::WrongAuthority
    );
    let out = s
        .read_host_scope(
            &cap,
            lease(&h),
            ScopeReadRequest::GetMemberships("a".into()),
            budget(),
        )
        .unwrap();
    let revoked = h.begin_revocation().unwrap();
    assert!(revoked.wait_drained(Duration::ZERO).is_err());
    out.deliver(|value| {
        assert_eq!(value["scoped_total"], 1);
        assert!(revoked.wait_drained(Duration::ZERO).is_err());
    });
    assert!(revoked.wait_drained(Duration::ZERO).is_ok());
    assert!(h.acquire("rev-1").is_err());
}

#[test]
fn snapshot_provenance_seam_does_not_weaken_the_public_current_read_boundary() {
    use crate::catalog_metadata_store::{
        self as metadata, MetadataReadAuthority, MetadataStoreErrorCode,
    };
    let f = Fixture::at(36);
    initialize(&f, "a");
    let authority = MetadataReadAuthority {
        catalog_id: f.writer.database_id.clone(),
        incarnation: f.writer.catalog_incarnation.clone().unwrap(),
        subject: "a".into(),
        expected_generation: f
            .writer
            .db
            .query_row("SELECT generation FROM catalog_state", [], |r| r.get(0))
            .unwrap(),
    };
    let work = operation(budget()).unwrap();
    let fields = ["title".into()].into();
    assert_eq!(
        metadata::read_metadata_provenance_in_snapshot(&f.writer.db, &authority, &fields, &work)
            .unwrap_err()
            .code,
        MetadataStoreErrorCode::TransactionRequired
    );
    f.writer.db.execute_batch("BEGIN").unwrap();
    assert_eq!(
        metadata::read_metadata(&f.writer, &authority)
            .unwrap_err()
            .code,
        MetadataStoreErrorCode::AmbientReadTransaction
    );
    let mut stale = authority;
    stale.expected_generation += 1;
    assert_eq!(
        metadata::read_metadata_provenance_in_snapshot(&f.writer.db, &stale, &fields, &work)
            .unwrap_err()
            .code,
        MetadataStoreErrorCode::StaleGeneration
    );
    f.writer.db.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn serialized_provenance_output_obeys_cancellation_and_cumulative_byte_budget() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let f = Fixture::at(36);
    conflict(&f);
    let h = host();
    let (mut s, cap) = admitted(&f, &h, vec![provenance(&f)]);
    let raw_bytes:i64=f.writer.db.query_row("SELECT length(CAST(policies_json AS BLOB))+(SELECT coalesce(sum(length(CAST(fields_json AS BLOB))+length(CAST(evidence_json AS BLOB))+length(CAST(namespace AS BLOB))+length(CAST(record_id AS BLOB))),0) FROM catalog_metadata_sources WHERE item_id='a') FROM catalog_metadata_subjects WHERE item_id='a'",[],|r|r.get(0)).unwrap();
    let work = operation(budget()).unwrap();
    let active = work.enter().unwrap();
    let poisoned = Cell::new(false);
    let observer = work.clone();
    let reached = Arc::new(AtomicBool::new(false));
    let witness = reached.clone();
    let result = crate::embedding::with_interrupt_check(
        move || {
            if observer.usage().bytes > raw_bytes as usize {
                witness.store(true, Ordering::Release);
                true
            } else {
                false
            }
        },
        || {
            Ok(s.fresh(|slot, _| {
                details::read(
                    slot.as_ref().unwrap(),
                    &cap.0,
                    &cap.0.members,
                    details::Request::Provenance("a".into()),
                    Instant::now(),
                    budget(),
                    &poisoned,
                )
            }))
        },
    )
    .unwrap();
    assert!(
        reached.load(Ordering::Acquire),
        "cancellation observed only after serialized output was charged"
    );
    assert_eq!(code(result), ScopeErrorCode::Cancelled);
    drop(active);
    let work = OperationBudget::new(
        Duration::from_secs(5),
        BudgetLimits {
            items: 1000,
            values: 10000,
            bytes: raw_bytes as usize,
            work: 100000,
        },
    )
    .unwrap();
    let active = work.enter().unwrap();
    let result = s.fresh(|slot, _| {
        details::read(
            slot.as_ref().unwrap(),
            &cap.0,
            &cap.0.members,
            details::Request::Provenance("a".into()),
            Instant::now(),
            budget(),
            &poisoned,
        )
    });
    assert_eq!(code(result), ScopeErrorCode::ByteBudget);
    assert_eq!(
        work.usage().bytes,
        raw_bytes as usize,
        "source decode fit; output could not fit"
    );
    drop(active);
    assert!(s.store.as_ref().unwrap().db.is_autocommit());
    assert!(
        read(
            &mut s,
            &cap,
            &h,
            ScopeReadRequest::GetMetadataProvenance("a".into())
        )
        .is_ok()
    );
}
#[test]
fn mutation_after_detail_preparation_before_delivery_discards_both_adapter_outputs() {
    for provenance_request in [false, true] {
        let f = Fixture::at(36);
        conflict(&f);
        f.writer
            .db
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        let h = host();
        let (mut s, cap) = admitted(&f, &h, vec![provenance(&f).with_membership_read()]);
        let work = operation(budget()).unwrap();
        let active = work.enter().unwrap();
        let poisoned = Cell::new(false);
        let result = s.fresh(|slot, _| {
            let request = if provenance_request {
                details::Request::Provenance("a".into())
            } else {
                details::Request::Memberships("a".into())
            };
            let prepared = details::read(
                slot.as_ref().unwrap(),
                &cap.0,
                &cap.0.members,
                request,
                Instant::now(),
                budget(),
                &poisoned,
            )?;
            f.mutate("committed after output preparation");
            Ok(prepared)
        });
        assert_eq!(code(result), ScopeErrorCode::StaleScope);
        drop(active);
        assert!(s.store.as_ref().unwrap().db.is_autocommit());
    }
}
#[test]
fn hidden_only_contributions_do_not_change_visible_provenance_payload_or_counts() {
    let f = Fixture::at(36);
    conflict(&f);
    let h = host();
    let selection = provenance(&f);
    let (mut s, cap) = admitted(&f, &h, vec![selection.clone()]);
    let before = read(
        &mut s,
        &cap,
        &h,
        ScopeReadRequest::GetMetadataProvenance("a".into()),
    )
    .unwrap();
    let fingerprint: String = f
        .writer
        .db
        .query_row(
            "SELECT schema_fingerprint FROM catalog_metadata_subjects WHERE item_id='a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    contribution(
        &f,
        "a",
        &fingerprint,
        "another-private-source",
        SourceKind::Provider,
        "secret",
        "SECOND_HIDDEN_VALUE",
    );
    assert_eq!(
        code(read(
            &mut s,
            &cap,
            &h,
            ScopeReadRequest::GetMetadataProvenance("a".into())
        )),
        ScopeErrorCode::StaleScope
    );
    let fresh = s
        .admit_host_scope(
            TrustedHostReadGrant::new(h.clone(), "default".into(), vec![selection], vec![])
                .unwrap(),
            &lease(&h),
            budget(),
        )
        .unwrap();
    let after = read(
        &mut s,
        &fresh,
        &h,
        ScopeReadRequest::GetMetadataProvenance("a".into()),
    )
    .unwrap();
    // scope is the fresh opaque authorization capability, never a content hash.
    for key in [
        "fields",
        "contributions",
        "scoped_total",
        "item_id",
        "contract_version",
    ] {
        assert_eq!(before[key], after[key], "{key}");
    }
    assert_eq!(after.as_object().unwrap().len(), 6);
    assert!(!encode(&after).contains("fingerprint"));
    assert!(!encode(&after).contains("another-private-source"));
}
