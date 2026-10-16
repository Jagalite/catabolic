use super::*;
fn fixture(schema: u32) -> Fixture {
    let f = Fixture::at(schema);
    f.writer.transaction(|db|{
        db.execute_batch("INSERT INTO locations VALUES('media'); INSERT INTO bindings VALUES('default','source','media','/recorded/private/DO_NOT_DISCLOSE',1,1); INSERT INTO source_identity_policies VALUES('default','media','strict','{}',1); INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES('scan','default','media',1,1,'[]');")?;
        for file in ["shared","other","private-file"] {
            db.execute("INSERT INTO files VALUES(?,'media',?)",params![file,format!("{file}/DO_NOT_DISCLOSE.mkv")])?;
            db.execute("INSERT INTO observations VALUES('default',?,1000,10,1,1,'present','scan')",[file])?;
        } Ok(())
    }).unwrap();
    for (item, file, role, part) in [
        ("a", "shared", "primary", 1),
        ("a", "other", "subtitle", 1),
        ("b", "other", "primary", 1),
        ("hidden", "private-file", "primary", 1),
        ("hidden", "shared", "primary", 2),
    ] {
        curation::dispatch(&f.writer,"default","associate",&json!({"item_id":item,"file_id":file,"role":role,"part":part,"metadata":{"title":"PRIVATE_ASSOCIATION_METADATA"}})).unwrap();
    }
    fact(
        &f,
        "shared",
        "probe",
        json!({"format":{"format_name":"matroska","duration":"120.0","size":"1000","filename":"/private/DO_NOT_DISCLOSE"},"streams":[{"tags":{"title":"HIDDEN_STREAM"}}]}),
    );
    fact(
        &f,
        "shared",
        "hash",
        json!({"digest":"a".repeat(64),"size":1000,"private":"DO_NOT_DISCLOSE"}),
    );
    f
}
fn fact(f: &Fixture, file: &str, operation: &str, data: Value) {
    let snapshot = crate::media_mutation::snapshot(&f.writer, "default", file).unwrap();
    let job = format!("{operation}-{file}");
    f.writer.transaction(|db|{
        db.execute("INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state,result) VALUES(?,'default',? ,?,'media',?,'{}',?,'complete',?)",params![job,file,operation,encode(&snapshot),job,encode(&data)])?;
        db.execute("INSERT INTO file_facts VALUES('default',?,?,?,?,?,'complete')",params![file,operation,job,encode(&snapshot),encode(&data)])?;Ok(())
    }).unwrap();
}
fn selected(
    f: &Fixture,
    name: &str,
    items: &str,
    roles: &[&str],
    fields: Vec<FileFactField>,
) -> TrustedSelection {
    TrustedSelection::new(
        f.saved(
            name,
            "selection",
            "sql",
            &format!("SELECT item_id FROM catalog_items WHERE item_id IN ({items})"),
        ),
        vec!["title".into()],
    )
    .unwrap()
    .with_file_facts(
        TrustedFileFactsRead::whole_file(roles.iter().map(|r| r.to_string()).collect(), fields)
            .unwrap(),
    )
}
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
fn files(
    s: &mut ScopedReadSession,
    cap: &ScopeHandle,
    h: &HostAuthorization,
    item: &str,
) -> Result<Value> {
    read(s, cap, h, ScopeReadRequest::GetFileFacts(item.into()))
}
#[test]
fn whole_file_facts_require_explicit_purpose_and_never_return_paths_or_unrequested_fields() {
    let f = fixture(36);
    let h = host();
    let plain = TrustedSelection::new(f.selection(), vec!["title".into()])
        .unwrap()
        .with_membership_read()
        .with_provenance(TrustedProvenanceRead::new(vec!["title".into()]).unwrap());
    let (mut s, cap) = admitted(&f, &h, vec![plain]);
    assert_eq!(
        code(files(&mut s, &cap, &h, "a")),
        ScopeErrorCode::NotFoundOrNotVisible
    );
    let grant = selected(
        &f,
        "physical",
        "'a'",
        &["primary"],
        vec![
            FileFactField::Size,
            FileFactField::RecordedRevision,
            FileFactField::HashDigest,
            FileFactField::ProbeDuration,
        ],
    );
    let (mut s, cap) = admitted(&f, &h, vec![grant]);
    let out = files(&mut s, &cap, &h, "a").unwrap();
    assert_eq!(out["physical_scope"], "whole_file_recorded");
    assert_eq!(out["scoped_total"], 1);
    let file = &out["files"][0];
    assert_eq!(file["file_id"], "shared");
    assert_eq!(file["observed"]["size"], 1000);
    assert_eq!(file["processing"]["hash"]["data"]["digest"], "a".repeat(64));
    assert_eq!(
        file["processing"]["probe"]["data"],
        json!({"duration":"120.0"})
    );
    assert_eq!(file["processing"]["probe"]["exact_revision_current"], true);
    for forbidden in [
        "DO_NOT_DISCLOSE",
        "PRIVATE_ASSOCIATION_METADATA",
        "HIDDEN_STREAM",
        "source_path",
        "filename",
        "snapshot",
        "private-file",
        "hidden",
        "other",
    ] {
        assert!(!encode(&out).contains(forbidden), "{forbidden}");
    }
    for id in ["hidden", "missing", "b"] {
        assert_eq!(
            code(files(&mut s, &cap, &h, id)),
            ScopeErrorCode::NotFoundOrNotVisible
        );
    }
}
#[test]
fn physical_fields_are_per_item_and_per_role_without_cross_role_union() {
    let f = fixture(36);
    let h = host();
    let primary = selected(
        &f,
        "primary",
        "'a'",
        &["primary"],
        vec![FileFactField::Size],
    );
    let subtitle = selected(
        &f,
        "subtitle",
        "'a'",
        &["subtitle"],
        vec![FileFactField::Status],
    );
    let b = selected(&f, "b", "'b'", &["primary"], vec![]);
    let (mut s, cap) = admitted(&f, &h, vec![primary, subtitle, b]);
    let out = files(&mut s, &cap, &h, "a").unwrap();
    assert_eq!(out["scoped_total"], 2);
    for row in out["files"].as_array().unwrap() {
        if row["role"] == "primary" {
            assert_eq!(row["observed"], json!({"size":1000}));
        } else {
            assert_eq!(row["observed"], json!({"status":"present"}));
        }
    }
    let out = files(&mut s, &cap, &h, "b").unwrap();
    assert_eq!(out["files"][0]["observed"], json!({}));
    assert_eq!(out["files"][0]["processing"], json!({}));
    assert!(TrustedFileFactsRead::whole_file(vec!["not_a_role".into()], vec![]).is_err());
    assert!(TrustedFileFactsRead::whole_file(vec![], vec![]).is_err());
}
#[test]
fn exact_recorded_revision_does_not_ignore_source_policy_changes_or_probe_live_paths() {
    let f = fixture(36);
    let h = host();
    let grant = selected(
        &f,
        "physical",
        "'a'",
        &["primary"],
        vec![
            FileFactField::RecordedRevision,
            FileFactField::ProbeDuration,
        ],
    );
    let (mut s, cap) = admitted(&f, &h, vec![grant.clone()]);
    let before = files(&mut s, &cap, &h, "a").unwrap();
    assert_eq!(
        before["files"][0]["processing"]["probe"]["exact_revision_current"],
        true
    );
    f.writer.transaction(|db|{db.execute("UPDATE source_identity_policies SET revision=revision+1 WHERE profile='default' AND location='media'",[])?;Ok(())}).unwrap();
    assert_eq!(
        code(files(&mut s, &cap, &h, "a")),
        ScopeErrorCode::StaleScope
    );
    let fresh = s
        .admit_host_scope(
            TrustedHostReadGrant::new(h.clone(), "default".into(), vec![grant], vec![]).unwrap(),
            &lease(&h),
            budget(),
        )
        .unwrap();
    let after = files(&mut s, &fresh, &h, "a").unwrap();
    assert_eq!(
        after["files"][0]["processing"]["probe"]["exact_revision_current"],
        false
    );
    assert_ne!(
        before["files"][0]["observed"]["recorded_revision"],
        after["files"][0]["observed"]["recorded_revision"]
    );
    assert_eq!(
        before["files"][0]["processing"]["probe"]["recorded_revision"],
        after["files"][0]["processing"]["probe"]["recorded_revision"]
    );
}
#[test]
fn hidden_associations_and_unrequested_facts_cannot_change_visible_payload_or_totals() {
    let f = fixture(36);
    let h = host();
    let grant = selected(
        &f,
        "physical",
        "'a'",
        &["primary"],
        vec![FileFactField::Size],
    );
    let (mut s, cap) = admitted(&f, &h, vec![grant.clone()]);
    let before = files(&mut s, &cap, &h, "a").unwrap();
    fact(
        &f,
        "shared",
        "text",
        json!({"text":"hidden-only unrequested fact"}),
    );
    curation::dispatch(
        &f.writer,
        "default",
        "associate",
        &json!({"item_id":"hidden","file_id":"other","role":"cover"}),
    )
    .unwrap();
    let fresh = s
        .admit_host_scope(
            TrustedHostReadGrant::new(h.clone(), "default".into(), vec![grant], vec![]).unwrap(),
            &lease(&h),
            budget(),
        )
        .unwrap();
    let after = files(&mut s, &fresh, &h, "a").unwrap();
    assert_eq!(before["files"], after["files"]);
    assert_eq!(before["scoped_total"], after["scoped_total"]);
}
#[test]
fn file_fact_vm_byte_output_cancellation_and_row_limits_refuse_partial_results() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let f = fixture(36);
    let h = host();
    let grant = selected(
        &f,
        "physical",
        "'a'",
        &["primary", "subtitle"],
        vec![FileFactField::Size, FileFactField::ProbeDuration],
    );
    let (mut s, cap) = admitted(&f, &h, vec![grant]);
    assert_eq!(
        code(s.read_host_scope(
            &cap,
            lease(&h),
            ScopeReadRequest::GetFileFacts("a".into()),
            ReadBudget::new(Duration::from_secs(5), 1).unwrap()
        )),
        ScopeErrorCode::IdBudget
    );
    let poisoned = Cell::new(false);
    for (bytes, work, expected) in [
        (MAX_BYTES, 1, ScopeErrorCode::WorkBudget),
        (0, 100000, ScopeErrorCode::ByteBudget),
    ] {
        let operation = OperationBudget::new(
            Duration::from_secs(5),
            BudgetLimits {
                items: 1000,
                values: 10000,
                bytes,
                work,
            },
        )
        .unwrap();
        let active = operation.enter().unwrap();
        let result = s.fresh(|slot, _| {
            details::read(
                slot.as_ref().unwrap(),
                &cap.0,
                &cap.0.members,
                details::Request::FileFacts("a".into()),
                Instant::now(),
                budget(),
                &poisoned,
            )
        });
        assert_eq!(code(result), expected);
        drop(active);
    }
    let operation = operation(budget()).unwrap();
    let active = operation.enter().unwrap();
    s.fresh(|slot, _| {
        physical::file_facts(
            slot.as_ref().unwrap(),
            &cap.0,
            "a",
            &cap.0.members["a"].file_facts,
            Instant::now(),
            budget(),
        )
    })
    .unwrap();
    let before_final_output = operation.usage().bytes;
    drop(active);
    let operation = super::operation(budget()).unwrap();
    let active = operation.enter().unwrap();
    let observer = operation.clone();
    let reached = Arc::new(AtomicBool::new(false));
    let witness = reached.clone();
    let result = crate::embedding::with_interrupt_check(
        move || {
            if observer.usage().bytes > before_final_output {
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
                    details::Request::FileFacts("a".into()),
                    Instant::now(),
                    budget(),
                    &poisoned,
                )
            }))
        },
    )
    .unwrap();
    assert_eq!(code(result), ScopeErrorCode::Cancelled);
    assert!(reached.load(Ordering::Acquire));
    drop(active);
    assert!(s.store.as_ref().unwrap().db.is_autocommit());
    assert!(files(&mut s, &cap, &h, "a").is_ok());
}
#[test]
fn prepared_physical_facts_are_discarded_on_external_commit_before_delivery() {
    let f = fixture(36);
    f.writer
        .db
        .pragma_update(None, "journal_mode", "WAL")
        .unwrap();
    let h = host();
    let (mut s, cap) = admitted(
        &f,
        &h,
        vec![selected(
            &f,
            "physical",
            "'a'",
            &["primary"],
            vec![FileFactField::Size],
        )],
    );
    let work = operation(budget()).unwrap();
    let active = work.enter().unwrap();
    let poisoned = Cell::new(false);
    let result = s.fresh(|slot, _| {
        let value = details::read(
            slot.as_ref().unwrap(),
            &cap.0,
            &cap.0.members,
            details::Request::FileFacts("a".into()),
            Instant::now(),
            budget(),
            &poisoned,
        )?;
        f.mutate("after physical read");
        Ok(value)
    });
    assert_eq!(code(result), ScopeErrorCode::StaleScope);
    drop(active);
}
#[test]
fn populated36_to37_preserves_stored_physical_facts() {
    let f = fixture(36);
    let grant = selected(
        &f,
        "physical",
        "'a'",
        &["primary"],
        vec![FileFactField::HashDigest, FileFactField::ProbeFormatName],
    );
    let f = f.upgrade37();
    let h = host();
    let (mut s, cap) = admitted(&f, &h, vec![grant]);
    let out = files(&mut s, &cap, &h, "a").unwrap();
    assert_eq!(
        out["files"][0]["processing"]["probe"]["data"],
        json!({"format_name":"matroska"})
    );
    assert!(!encode(&out).contains("recorded_revision"));
    assert_eq!(
        out["files"][0]["processing"]["hash"]["exact_revision_current"],
        true
    );
}

#[test]
fn visible_native_slice_needs_deliberate_whole_file_fields_and_never_becomes_an_interval_effect() {
    use crate::catalog_commands::{self as commands, ReceiptPolicy, TrustedCommandAuthority};
    use crate::catalog_identity_mutation::{
        self as identity, ContextChange, IdentityAuthority, IdentityMutation, TimelineChange,
        TimelineState, VersionChange,
    };
    use catabolic_core::catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility};
    use catabolic_core::catalog_identity::{
        ExactFileRevision, IntervalUs, SourceExtent, VersionBinding, VersionContract, VersionState,
    };
    let f = fixture(36);
    curation::item_put(
        &f.writer,
        &json!({"id":"edition","kind":"video","metadata":{}}),
    )
    .unwrap();
    let span = |start, end| IntervalUs::new(start, end).unwrap();
    let mut versions = Vec::new();
    let mut visible_association = String::new();
    for item in ["a", "hidden"] {
        let rows=crate::migration::rows(&f.writer.db,"SELECT id,file_id,part FROM item_files WHERE item_id=? AND role='primary' AND active=1 ORDER BY part",&[item.to_owned().into()]).unwrap();
        let mut bindings = Vec::new();
        for row in rows {
            let association = row["id"].as_str().unwrap().to_owned();
            if item == "a" {
                visible_association = association.clone();
            }
            let file = row["file_id"].as_str().unwrap();
            let part = row["part"].as_u64().unwrap() as u32;
            let start = if file == "shared" && item == "hidden" {
                60_000_000
            } else {
                0
            };
            let snapshot = crate::media_mutation::snapshot(&f.writer, "default", file).unwrap();
            bindings.push(VersionBinding {
                binding_id: format!("binding-{item}-{part}"),
                association_id: association,
                file: ExactFileRevision {
                    file_id: file.into(),
                    revision: crate::public_components::revision(&snapshot).unwrap(),
                },
                part,
                source: SourceExtent::Slice {
                    interval: span(start, start + 60_000_000),
                },
                timeline_interval: Some(span(
                    i64::from(part - 1) * 60_000_000,
                    i64::from(part) * 60_000_000,
                )),
            });
        }
        versions.push(VersionContract {
            contract_version: 1,
            version_id: format!("version-{item}"),
            association_item_id: item.into(),
            edition_item_id: "edition".into(),
            timeline_id: format!("timeline-{item}"),
            state: VersionState::Ready,
            declared_coverage: Some(span(0, bindings.len() as i64 * 60_000_000)),
            bindings,
        });
    }
    let mutation = IdentityMutation {
        contract_version: 1,
        profile: "default".into(),
        context: ContextChange {
            id: "context".into(),
            authority_namespace: "physical-test".into(),
            context_key: "shared-file".into(),
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
    };
    let authority = IdentityAuthority {
        authority_namespace: "physical-test".into(),
        context_keys: ["shared-file".into()].into(),
        allow_declared_evidence: false,
        allow_verified_evidence: false,
    };
    let generation = f
        .writer
        .db
        .query_row("SELECT generation FROM catalog_state", [], |r| r.get(0))
        .unwrap();
    let envelope = CommandEnvelope::new(
        1,
        CommandKey::new(&f.writer.database_id, "physical-test", "slices").unwrap(),
        f.writer.catalog_incarnation.as_deref().unwrap(),
        generation,
        &encode(&json!({"operation":"identity","mutation":mutation})),
    )
    .unwrap();
    commands::execute(
        &f.writer,
        &envelope,
        TrustedCommandAuthority {
            host_namespace: "physical-test",
            receipt_visibility: ReceiptVisibility::FullResult,
            allow_new_commands: true,
            identity_authority: Some(&authority),
        },
        ReceiptPolicy {
            retention: Duration::from_secs(3600),
            max_receipts: 1000,
            max_logical_bytes: 16 * 1024 * 1024,
            max_hold_records: 1000,
        },
    )
    .unwrap();
    assert!(
        identity::legacy_association_admission(&f.writer, "default", &visible_association).is_err()
    );
    let intervals=crate::migration::rows(&f.writer.db,"SELECT b.version_id,r.source_start_us,r.source_end_us FROM catalog_bindings b JOIN catalog_binding_revisions r ON r.binding_id=b.id WHERE r.file_id='shared' ORDER BY r.source_start_us",&[]).unwrap();
    assert_eq!(intervals,json!([{"version_id":"version-a","source_start_us":0,"source_end_us":60000000},{"version_id":"version-hidden","source_start_us":60000000,"source_end_us":120000000}]).as_array().unwrap().clone());
    let h = host();
    let plain = TrustedSelection::new(
        f.saved(
            "episode-only",
            "selection",
            "sql",
            "SELECT item_id FROM catalog_items WHERE item_id='a'",
        ),
        vec!["title".into()],
    )
    .unwrap();
    let (mut s, cap) = admitted(&f, &h, vec![plain]);
    assert_eq!(
        code(files(&mut s, &cap, &h, "a")),
        ScopeErrorCode::NotFoundOrNotVisible
    );
    let whole = selected(
        &f,
        "whole-file",
        "'a'",
        &["primary"],
        vec![FileFactField::Size, FileFactField::ProbeDuration],
    );
    let (mut s, cap) = admitted(&f, &h, vec![whole]);
    let out = files(&mut s, &cap, &h, "a").unwrap();
    assert_eq!(out["physical_scope"], "whole_file_recorded");
    assert_eq!(out["files"][0]["observed"]["size"], 1000);
    assert_eq!(
        out["files"][0]["processing"]["probe"]["data"]["duration"],
        "120.0"
    );
    assert!(!encode(&out).contains("version-hidden"));
    assert!(!encode(&out).contains("private-file"));
    assert!(
        identity::legacy_association_admission(&f.writer, "default", &visible_association).is_err()
    );
}
