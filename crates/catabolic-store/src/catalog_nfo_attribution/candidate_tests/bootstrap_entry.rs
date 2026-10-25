//! Consume borrowed evidence through the normal candidate receipt engine.
//! The host audits belong to this fixture's complete constructed archive, not
//! request JSON, and do not qualify a future production Motion audit builder.
use super::super::bootstrap_entry as entry;
use super::*;
use std::cell::Cell;

fn scope(store: &Store) -> entry::Scope<'_> {
    entry::Scope {
        catalog_id: &store.database_id,
        incarnation: store.catalog_incarnation.as_deref().unwrap(),
        host_namespace: HOST,
        expected_generation: generation(store),
    }
}
fn with_evidence<T>(
    f: &Fixture,
    use_evidence: impl FnOnce(&[entry::SourceEvidence<'_>]) -> T,
) -> T {
    match &f.baseline {
        Baseline::Created(first) => {
            let audit = NfoCreatedSourceAuthority {
                host_namespace: HOST.into(),
                subject: SUBJECT.into(),
                request_digest: "a".repeat(64),
                complete_absence_audit_digest: catabolic_core::sha256(json!({
                    "domain":"candidate.complete-created-archive.v1","subject":SUBJECT,
                    "first":first.digest().as_str(),"automatic_identities":[],"sidecar_origins":[],
                }).to_string()),
            };
            use_evidence(&[entry::SourceEvidence {
                baseline: entry::BaselineEvidence::NativeCreated {
                    first,
                    audit: &audit,
                },
                later_commands: &[],
            }])
        }
        Baseline::Imported(archive) => {
            let origin = match &archive.physical {
                None => entry::ImportOriginEvidence::NoOrigin,
                Some(physical) => entry::ImportOriginEvidence::Physical {
                    part: entry::ImportPart {
                        manifest: &physical.manifest,
                        batches: &physical.batches,
                        activation: &physical.activation,
                    },
                    audit: &physical.grant,
                },
            };
            use_evidence(&[entry::SourceEvidence {
                baseline: entry::BaselineEvidence::Imported {
                    subject: SUBJECT,
                    metadata: entry::ImportPart {
                        manifest: &archive.manifest,
                        batches: &archive.batches,
                        activation: &archive.activation,
                    },
                    audit: &archive.grant,
                    origin,
                },
                later_commands: &[],
            }])
        }
    }
}
fn preview(f: &Fixture, id: &str) -> CommandEnvelope {
    let payload = with_evidence(f, |evidence| {
        f.store.transaction_detailed::<_, Error>(|_| {
            entry::prepare_in(&f.store, scope(&f.store), evidence, authority(false))
                .map(|plan| plan.payload())
        })
    })
    .unwrap();
    envelope(&f.store, id, payload)
}
fn execute(
    f: &Fixture,
    outer: &CommandEnvelope,
    policy: ReceiptPolicy,
    calls: &Cell<usize>,
) -> commands::Result<CommandOutcome> {
    commands::execute_candidate_for_test(
        &f.store,
        outer,
        authority(true),
        policy,
        |store, outer, _| {
            calls.set(calls.get() + 1);
            // This builder is INSIDE the receipt-first callback. Neither root/audit
            // reconstruction nor verification is attempted on replay or expiry.
            with_evidence(f, |evidence| {
                entry::apply_in(store, outer, evidence, authority(false))
            })
        },
    )
}
fn retained_metadata(store: &Store) -> Vec<Value> {
    [
        "SELECT * FROM catalog_metadata_subjects ORDER BY item_id",
        "SELECT * FROM catalog_metadata_schemas ORDER BY item_id,schema_fingerprint",
        "SELECT * FROM catalog_metadata_schema_heads ORDER BY item_id",
        "SELECT * FROM catalog_metadata_sources ORDER BY item_id,namespace,record_id",
        "SELECT * FROM catalog_metadata_history ORDER BY item_id,namespace,record_id,revision",
        "SELECT * FROM item_tags ORDER BY id",
        "SELECT * FROM catalog_identity_pins ORDER BY namespace,item_id",
    ]
    .into_iter()
    .map(|sql| json!(migration::rows(&store.db, sql, &[]).unwrap()))
    .collect()
}

#[test]
fn borrowed_created_entry_reconstructs_inside_receipt_then_replays_expires_and_reopens() {
    let f = Fixture::created().install();
    let outer = preview(&f, "entry-created");
    let metadata = retained_metadata(&f.store);
    let calls = Cell::new(0);
    let mut retention = policy();
    retention.retention = Duration::ZERO;
    let result = committed(execute(&f, &outer, retention, &calls).unwrap());
    assert_eq!(calls.get(), 1);
    assert_eq!(result["source_count"], 1);
    assert_eq!(retained_metadata(&f.store), metadata);
    assert_eq!(f.state().presence().presence.public_revision, Some(1));
    let before = migration::snapshot(&f.store.db).unwrap();
    assert!(
        matches!(execute(&f, &outer, policy(), &calls).unwrap(), CommandOutcome::Replay{result: got,..} if got==result)
    );
    assert_eq!(calls.get(), 1);
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    let f = f.reopen();
    assert!(matches!(
        execute(&f, &outer, policy(), &calls).unwrap(),
        CommandOutcome::Replay { .. }
    ));
    assert_eq!(calls.get(), 1);
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    assert!(
        commands::compact_candidate_receipts_for_test(
            &f.store,
            commands::TrustedReceiptMaintenance {
                host_namespace: HOST,
                holder_namespace: "bootstrap-entry-expiry",
            },
            100
        )
        .unwrap()
            >= 1
    );
    let expired = migration::snapshot(&f.store.db).unwrap();
    assert!(matches!(
        execute(&f, &outer, policy(), &calls).unwrap(),
        CommandOutcome::ReceiptExpired
    ));
    assert_eq!(calls.get(), 1);
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), expired);
    assert_eq!(retained_metadata(&f.store), metadata);
}

#[test]
fn borrowed_import_entry_consumes_exact_no_origin_and_physical_parts() {
    for with_origin in [false, true] {
        let f = Fixture::imported(with_origin).install();
        let outer = preview(&f, "entry-imported");
        let metadata = retained_metadata(&f.store);
        let calls = Cell::new(0);
        let result = committed(execute(&f, &outer, policy(), &calls).unwrap());
        assert_eq!(result["source_count"], 1);
        let current = f.state();
        assert_eq!(
            current
                .head()
                .attribution
                .as_ref()
                .unwrap()
                .active_identity
                .as_deref(),
            Some("tmdb:42")
        );
        assert_eq!(current.origin_physical().is_some(), with_origin);
        assert_eq!(
            current
                .head()
                .attribution
                .as_ref()
                .unwrap()
                .origin
                .is_some(),
            with_origin
        );
        assert_eq!(retained_metadata(&f.store), metadata);
        let f = f.reopen();
        assert!(matches!(
            execute(&f, &outer, policy(), &calls).unwrap(),
            CommandOutcome::Replay { .. }
        ));
        assert_eq!(calls.get(), 1);
    }
}

#[test]
fn entry_refuses_duplicate_missing_foreign_stale_or_ungranted_proofs_without_effects() {
    let f = Fixture::created().install();
    let before = migration::snapshot(&f.store.db).unwrap();
    with_evidence(&f, |evidence| {
        for case in 0..8 {
            let result = f.store.transaction_detailed::<_, Error>(|_| {
                let mut expected = scope(&f.store);
                let mut read = authority(false);
                match case {
                    0 => return entry::prepare_in(&f.store, expected, &[], read).map(|_| ()),
                    1 => {
                        return entry::prepare_in(
                            &f.store,
                            expected,
                            &[evidence[0], evidence[0]],
                            read,
                        )
                        .map(|_| ());
                    }
                    2 => expected.expected_generation -= 1,
                    3 => expected.catalog_id = "foreign-catalog",
                    4 => expected.incarnation = "foreign-incarnation",
                    5 => expected.host_namespace = "foreign-host",
                    6 => read.allow_new_commands = true,
                    7 => read.receipt_visibility = ReceiptVisibility::CommittedStatusOnly,
                    _ => unreachable!(),
                }
                entry::prepare_in(&f.store, expected, evidence, read).map(|_| ())
            });
            assert!(result.is_err(), "case {case}");
            assert_eq!(
                migration::snapshot(&f.store.db).unwrap(),
                before,
                "case {case}"
            );
        }
        // The shared list admission refuses before allocating output proofs,
        // instead of resetting an operation budget for every duplicate subject.
        let many = vec![evidence[0]; 20_000];
        let result = f.store.transaction_detailed::<_, Error>(|_| {
            entry::prepare_in(&f.store, scope(&f.store), &many, authority(false)).map(|_| ())
        });
        let error = result.unwrap_err().0;
        assert!(
            error.contains("budget") || error.contains("admission"),
            "{error}"
        );
        assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    });
}

#[test]
fn entry_requires_exact_complete_later_command_history_and_outer_payload() {
    let f = Fixture::created();
    let request = put_request(&f.store, 1, json!({"title":"later"}), &["later-tag"]);
    let later = replace_40(
        &f.store,
        "entry-later-source",
        &request,
        SourceKind::Provider,
    );
    let f = f.install();
    let before = migration::snapshot(&f.store.db).unwrap();
    with_evidence(&f, |original| {
        let missing = f.store.transaction_detailed::<_, Error>(|_| {
            entry::prepare_in(&f.store, scope(&f.store), original, authority(false)).map(|_| ())
        });
        assert!(missing.unwrap_err().0.contains("history/proofs"));
        assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
        let commands = [later];
        let evidence = [entry::SourceEvidence {
            later_commands: &commands,
            ..original[0]
        }];
        let payload = f
            .store
            .transaction_detailed::<_, Error>(|_| {
                entry::prepare_in(&f.store, scope(&f.store), &evidence, authority(false))
                    .map(|plan| plan.payload())
            })
            .unwrap();
        let bad = envelope(
            &f.store,
            "entry-wrong-payload",
            json!({"operation":"nfo_bootstrap","contract_version":1,"inventory_digest":"0".repeat(64)}),
        );
        let denied = commands::execute_candidate_for_test(
            &f.store,
            &bad,
            authority(true),
            policy(),
            |store, outer, _| entry::apply_in(store, outer, &evidence, authority(false)),
        );
        assert!(denied.is_err());
        assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
        let outer = envelope(&f.store, "entry-later-complete", payload);
        committed(
            commands::execute_candidate_for_test(
                &f.store,
                &outer,
                authority(true),
                policy(),
                |store, outer, _| entry::apply_in(store, outer, &evidence, authority(false)),
            )
            .unwrap(),
        );
        assert_eq!(f.state().native_source().unwrap().revision(), 2);
    });
}

#[test]
fn borrowed_entry_does_not_admit_or_change_normal40() {
    let f = Fixture::created();
    let before = migration::snapshot(&f.store.db).unwrap();
    let refused = with_evidence(&f, |evidence| {
        f.store.transaction_detailed::<_, Error>(|_| {
            entry::prepare_in(&f.store, scope(&f.store), evidence, authority(false)).map(|_| ())
        })
    });
    assert!(refused.unwrap_err().0.contains("candidate41"));
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    assert_eq!(f.store.schema_version, 40);
}
