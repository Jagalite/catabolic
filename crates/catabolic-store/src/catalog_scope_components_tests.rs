use super::*;
use ComponentField::*;
use ComponentKind::*;

fn fixture(schema: u32) -> Fixture {
    let f = Fixture::at(schema);
    f.writer.transaction(|db|{
        db.execute_batch("INSERT INTO locations VALUES('media'); INSERT INTO bindings VALUES('default','source','media','/recorded/DO_NOT_OPEN',1,1); INSERT INTO source_identity_policies VALUES('default','media','strict','{}',1); INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES('scan','default','media',1,1,'[]'); INSERT INTO generated_locations VALUES('default','media');")?;
        for file in ["a-file","b-file","hidden-file","subtitle-file"] {
            db.execute("INSERT INTO files VALUES(?,'media',?)",params![file,format!("DO_NOT_DISCLOSE/{file}.mkv")])?;
            db.execute("INSERT INTO observations VALUES('default',?,1000,10,1,1,'present','scan')",[file])?;
        } Ok(())
    }).unwrap();
    for (item, file, role) in [
        ("a", "a-file", "primary"),
        ("a", "subtitle-file", "subtitle"),
        ("b", "b-file", "primary"),
        ("hidden", "hidden-file", "primary"),
    ] {
        curation::dispatch(&f.writer,"default","associate",&json!({"item_id":item,"file_id":file,"role":role,"part":1,"metadata":{"private":"DO_NOT_DISCLOSE"}})).unwrap();
    }
    for (item, file) in [("a", "a-file"), ("b", "b-file"), ("hidden", "hidden-file")] {
        let snapshot = crate::components::occurrence(&f.writer.db, "default", file).unwrap();
        let mut stream = json!({"index":0,"codec_type":"audio","codec_name":"aac"});
        if item != "a" {
            stream["tags"] = json!({"title":format!("title-{item}"),"language":"eng"});
        }
        let mut streams = vec![stream];
        if item == "a" {
            streams.push(json!({"index":1,"codec_type":"video","codec_name":"h264"}));
        }
        let data = json!({"format":{"format_name":"matroska"},"streams":streams});
        let job = format!("probe-{file}");
        f.writer.transaction(|db|{
            db.execute("INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state,result) VALUES(?,'default',?,'probe','media',?,'{}',?,'complete',?)",params![job,file,encode(&snapshot),job,encode(&data)])?;
            db.execute("INSERT INTO file_facts VALUES('default',?,'probe',?,?,?,'complete')",params![file,job,encode(&snapshot),encode(&data)])?; Ok(())
        }).unwrap();
        crate::public_components::call(
            &f.writer,
            "default",
            "index_file",
            &json!({"file_id":file}),
        )
        .unwrap();
    }
    crate::public_components::call(
        &f.writer,
        "default",
        "index_file",
        &json!({"file_id":"subtitle-file"}),
    )
    .unwrap();
    f
}
fn selection(
    f: &Fixture,
    name: &str,
    items: &str,
    roles: Vec<&str>,
    kinds: Vec<ComponentKind>,
    fields: Vec<ComponentField>,
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
    .with_components(
        TrustedComponentsRead::whole_occurrences(
            roles.into_iter().map(str::to_owned).collect(),
            kinds,
            fields,
        )
        .unwrap(),
    )
}
fn selected(f: &Fixture, name: &str, items: &str, fields: Vec<ComponentField>) -> TrustedSelection {
    selection(f, name, items, vec!["primary"], vec![Audio], fields)
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
fn get(
    s: &mut ScopedReadSession,
    cap: &ScopeHandle,
    h: &HostAuthorization,
    item: &str,
) -> Result<Value> {
    read(s, cap, h, ScopeReadRequest::GetComponents(item.into()))
}
fn occurrence(f: &Fixture, item: &str) -> String {
    f.writer
        .db
        .query_row(
            "SELECT c.id FROM component_occurrences c JOIN media_components m ON m.id=c.component_id WHERE c.item_id=? AND c.file_id=? AND m.kind='audio'",
            params![item, format!("{item}-file")],
            |r| r.get(0),
        )
        .unwrap()
}
fn derive(f: &Fixture, output: &str, input: &str, ordinal: i64) {
    let source = occurrence(f, input);
    let evidence = crate::component_mutation::get(&f.writer, "default", &source, false).unwrap();
    let job = format!("probe-{output}-file");
    let artifact = format!("artifact-{output}");
    f.writer.transaction(|db|{
        db.execute("INSERT OR IGNORE INTO processing_attempts(job_id,attempt,state,retryable,retry_after) VALUES(?,1,'complete',0,0)",[&job])?;
        db.execute("INSERT OR IGNORE INTO processing_artifacts(id,job_id,attempt,profile,location,path,temporary_path,binding,state,file_id,item_id,role,size,sha256) VALUES(?,?,1,'default','media',?,?,'{}','ready',?,?,'primary',1000,?)",params![artifact,job,format!("{output}.mkv"),format!("{output}.tmp"),format!("{output}-file"),output,"a".repeat(64)])?;
        db.execute("INSERT INTO component_job_inputs VALUES(?,?,?,?)",params![job,ordinal,source,encode(&json!({"occurrence":evidence}))])?;
        db.execute("INSERT INTO component_output_lineage VALUES(?,?,?,0,'extracted_copy',?)",params![artifact,ordinal,source,evidence["component_id"].as_str()])?;
        Ok(())
    }).unwrap();
}
#[test]
fn component_purpose_kind_role_masks_and_direct_scalar_projection() {
    let f = fixture(37);
    let h = host();
    let plain = TrustedSelection::new(f.selection(), vec!["title".into()])
        .unwrap()
        .with_search_fields(TrustedSearchFields::new(vec![], true, true).unwrap())
        .with_file_facts(
            TrustedFileFactsRead::whole_file(vec!["primary".into()], vec![FileFactField::Size])
                .unwrap(),
        );
    let (mut s, cap) = admitted(&f, &h, vec![plain]);
    assert_eq!(
        code(get(&mut s, &cap, &h, "a")),
        ScopeErrorCode::NotFoundOrNotVisible
    );
    let audio = selected(
        &f,
        "audio",
        "'a','b'",
        vec![Codec, Language, TechnicallyVerified, RecordedRevision],
    );
    let subs = selection(
        &f,
        "subs",
        "'a'",
        vec!["subtitle"],
        vec![Subtitle],
        vec![Storage],
    );
    let (mut s, cap) = admitted(&f, &h, vec![audio, subs]);
    let out = get(&mut s, &cap, &h, "a").unwrap();
    assert_eq!(out["scoped_total"], 2);
    for row in out["components"].as_array().unwrap() {
        assert_eq!(row["recorded_current"], true);
        assert_eq!(row["physical_scope"], "whole_occurrence_recorded");
        if row["kind"] == "audio" {
            assert_eq!(row["fields"]["codec"], "aac");
            assert_eq!(row["fields"].as_object().unwrap().len(), 4);
        } else {
            assert_eq!(row["fields"], json!({"storage":"external"}));
        }
    }
    for forbidden in [
        "DO_NOT_DISCLOSE",
        "DO_NOT_OPEN",
        "snapshot",
        "locator",
        "provenance",
        "dependencies",
        "technical\"",
        "hidden-file",
        "title-b",
    ] {
        assert!(!encode(&out).contains(forbidden), "{forbidden}");
    }
    for item in ["hidden", "missing"] {
        assert_eq!(
            code(get(&mut s, &cap, &h, item)),
            ScopeErrorCode::NotFoundOrNotVisible
        );
    }
    assert!(
        TrustedComponentsRead::whole_occurrences(vec!["audio".into()], vec![Audio], vec![])
            .is_err()
    );
    assert!(TrustedComponentsRead::whole_occurrences(vec![], vec![Audio], vec![]).is_err());
    assert!(
        TrustedComponentsRead::whole_occurrences(vec!["primary".into()], vec![], vec![]).is_err()
    );
}
#[test]
fn derived_scalars_require_all_paths_and_intersect_all_endpoint_masks() {
    let f = fixture(37);
    derive(&f, "a", "b", 0);
    let h = host();
    let a = selected(&f, "a", "'a'", vec![Title, Language, Codec]);
    let b = selected(&f, "b", "'b'", vec![Language]);
    let (mut s, cap) = admitted(&f, &h, vec![a.clone()]);
    assert_eq!(get(&mut s, &cap, &h, "a").unwrap()["scoped_total"], 0);
    let (mut s, cap) = admitted(&f, &h, vec![a.clone(), b.clone()]);
    let out = get(&mut s, &cap, &h, "a").unwrap();
    assert_eq!(out["scoped_total"], 1);
    assert_eq!(out["components"][0]["fields"], json!({"language":"en"}));
    assert!(!encode(&out).contains("component_id"));
    assert!(!encode(&out).contains("recorded_revision"));
    derive(&f, "a", "hidden", 1);
    let (mut s, cap) = admitted(&f, &h, vec![a.clone(), b.clone()]);
    assert_eq!(
        get(&mut s, &cap, &h, "a").unwrap()["scoped_total"],
        0,
        "the nonwinning hidden path must deny the root"
    );
    let hidden = selected(&f, "hidden", "'hidden'", vec![Language]);
    let (mut s, cap) = admitted(&f, &h, vec![a, b, hidden]);
    assert_eq!(
        get(&mut s, &cap, &h, "a").unwrap()["components"][0]["fields"],
        json!({"language":"en"})
    );
}
#[test]
fn hidden_intermediate_cycle_missing_input_and_identity_mismatch_fail_closed() {
    for case in [
        "intermediate",
        "cycle",
        "missing",
        "occurrence",
        "profile",
        "item_id",
        "file_id",
        "association_id",
        "revision",
        "component_id",
    ] {
        let f = fixture(37);
        derive(&f, "a", "b", 0);
        match case {
            "intermediate" => derive(&f, "b", "hidden", 0),
            "cycle" => derive(&f, "b", "a", 0),
            "missing" => {
                f.writer
                    .transaction(|db| {
                        db.execute(
                            "DELETE FROM component_job_inputs WHERE job_id='probe-a-file'",
                            [],
                        )?;
                        Ok(())
                    })
                    .unwrap();
            }
            key => {
                f.writer.transaction(|db|{db.execute("UPDATE component_job_inputs SET evidence=json_set(evidence,?,'WRONG_ID') WHERE job_id='probe-a-file'",[format!("$.occurrence.{}",if key=="occurrence" {"occurrence_id"}else{key})])?;Ok(())}).unwrap();
            }
        }
        let h = host();
        let (mut s, cap) = admitted(
            &f,
            &h,
            vec![selected(&f, "visible", "'a','b'", vec![Title, Language])],
        );
        let out = get(&mut s, &cap, &h, "a").unwrap();
        assert_eq!(out["scoped_total"], 0, "{case}");
        assert!(!encode(&out).contains("WRONG_ID"));
        assert!(!encode(&out).contains("hidden"));
    }
}
#[test]
fn exact_active_association_and_recorded_currentness_exclude_stale_and_mismatched_roots() {
    for mutation in [
        "UPDATE item_files SET active=0 WHERE item_id='a'",
        "UPDATE component_occurrences SET item_id='a' WHERE item_id='hidden'",
        "UPDATE source_identity_policies SET revision=revision+1",
    ] {
        let f = fixture(37);
        f.writer
            .transaction(|db| {
                db.execute_batch(mutation)?;
                Ok(())
            })
            .unwrap();
        let h = host();
        let (mut s, cap) = admitted(&f, &h, vec![selected(&f, "a", "'a'", vec![Title])]);
        assert_eq!(
            get(&mut s, &cap, &h, "a").unwrap()["scoped_total"],
            if mutation.contains("component_occurrences") {
                1
            } else {
                0
            }
        );
    }
}
#[test]
fn unrelated_hidden_malformed_large_sources_cannot_change_visible_normalized_output() {
    let f = fixture(37);
    let h = host();
    let selection = selected(&f, "a", "'a'", vec![Title, Codec]);
    let (mut s, cap) = admitted(&f, &h, vec![selection.clone()]);
    let before = get(&mut s, &cap, &h, "a").unwrap();
    f.writer.transaction(|db|{
        db.execute("UPDATE processing_jobs SET result=? WHERE file_id='hidden-file'",["x".repeat(MAX_DEFINITION_BYTES+1)])?;
        db.execute("UPDATE component_occurrences SET snapshot='not-json',association_evidence='not-json',locator='not-json' WHERE item_id='hidden'",[])?;Ok(())
    }).unwrap();
    let (mut s, cap) = admitted(&f, &h, vec![selection]);
    let after = get(&mut s, &cap, &h, "a").unwrap();
    assert_eq!(before["components"], after["components"]);
    assert_eq!(before["scoped_total"], after["scoped_total"]);
}
#[test]
fn populated36_to37_preserves_real_components_and_authorized_derived_scalars() {
    let f = fixture(36);
    derive(&f, "a", "b", 0);
    let grant = selected(&f, "visible", "'a','b'", vec![Language, Title, Codec]);
    let f = f.upgrade37();
    let h = host();
    let (mut s, cap) = admitted(&f, &h, vec![grant]);
    let out = get(&mut s, &cap, &h, "a").unwrap();
    assert_eq!(out["scoped_total"], 1);
    assert_eq!(
        out["components"][0]["fields"],
        json!({"language":"en","title":"title-b","codec":"aac"})
    );
}
#[test]
fn component_vm_raw_output_cancellation_and_complete_result_limits() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let f = fixture(37);
    let h = host();
    let grant = selection(
        &f,
        "all",
        "'a'",
        vec!["primary", "subtitle"],
        vec![Audio, Subtitle],
        vec![Codec, Title],
    );
    let (mut s, cap) = admitted(&f, &h, vec![grant]);
    let poisoned = Cell::new(false);
    for (bytes, work, expected) in [
        (MAX_BYTES, 1, ScopeErrorCode::WorkBudget),
        (1, 1_000_000, ScopeErrorCode::ByteBudget),
    ] {
        let op = OperationBudget::new(
            Duration::from_secs(5),
            BudgetLimits {
                items: 10000,
                values: 100000,
                bytes,
                work,
            },
        )
        .unwrap();
        let active = op.enter().unwrap();
        let result = s.fresh(|slot, _| {
            details::read(
                slot.as_ref().unwrap(),
                &cap.0,
                &cap.0.members,
                details::Request::Components("a".into()),
                Instant::now(),
                budget(),
                &poisoned,
            )
        });
        assert_eq!(code(result), expected);
        drop(active);
    }
    let op = operation(budget()).unwrap();
    let active = op.enter().unwrap();
    let result = s.fresh(|slot, _| {
        component_reads::read_components(
            slot.as_ref().unwrap(),
            &cap.0,
            &cap.0.members,
            "a",
            Instant::now(),
            ReadBudget::new(Duration::from_secs(5), 1).unwrap(),
        )
    });
    assert_eq!(code(result), ScopeErrorCode::IdBudget);
    drop(active);
    let op = operation(budget()).unwrap();
    let active = op.enter().unwrap();
    s.fresh(|slot, _| {
        component_reads::read_components(
            slot.as_ref().unwrap(),
            &cap.0,
            &cap.0.members,
            "a",
            Instant::now(),
            budget(),
        )
    })
    .unwrap();
    let before_output = op.usage().bytes;
    drop(active);
    let op = OperationBudget::new(
        Duration::from_secs(5),
        BudgetLimits {
            items: 10000,
            values: 100000,
            bytes: before_output,
            work: 1_000_000,
        },
    )
    .unwrap();
    let active = op.enter().unwrap();
    assert_eq!(
        code(s.fresh(|slot, _| details::read(
            slot.as_ref().unwrap(),
            &cap.0,
            &cap.0.members,
            details::Request::Components("a".into()),
            Instant::now(),
            budget(),
            &poisoned
        ))),
        ScopeErrorCode::ByteBudget
    );
    drop(active);
    let op = operation(budget()).unwrap();
    let active = op.enter().unwrap();
    let observer = op.clone();
    let reached = Arc::new(AtomicBool::new(false));
    let witness = reached.clone();
    let result = crate::embedding::with_interrupt_check(
        move || {
            if observer.usage().bytes > before_output {
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
                    details::Request::Components("a".into()),
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
    assert!(get(&mut s, &cap, &h, "a").is_ok());
}
#[test]
fn relevant_raw_snapshot_locator_job_fact_assertion_and_lineage_preflight_before_decode() {
    for target in [
        "snapshot",
        "association_evidence",
        "locator",
        "job",
        "fact",
        "assertion",
        "lineage",
    ] {
        let f = fixture(37);
        if target == "lineage" {
            derive(&f, "a", "b", 0);
        }
        let large = "x".repeat(MAX_DEFINITION_BYTES + 1);
        f.writer.transaction(|db|{
            match target {
                "job"=>{db.execute("UPDATE processing_jobs SET result=? WHERE id='probe-a-file'",[&large])?;},
                "fact"=>{db.execute("UPDATE file_facts SET data=? WHERE file_id='a-file'",[&large])?;},
                "lineage"=>{db.execute("UPDATE component_job_inputs SET evidence=? WHERE job_id='probe-a-file'",[&large])?;},
                "assertion"=>{
                    db.execute("INSERT INTO proposals(id,profile,file_id,kind,payload,evidence,source,snapshot,state) VALUES('claim','default','a-file','component','{}','{}','manual','{}','accepted')",[])?;
                    db.execute("INSERT INTO component_assertions(id,occurrence_id,component_id,payload) SELECT 'claim',c.id,c.component_id,? FROM component_occurrences c JOIN media_components m ON m.id=c.component_id WHERE c.item_id='a' AND c.file_id='a-file' AND m.kind='audio'",[&large])?;
                },
                column=>{db.execute(&format!("UPDATE component_occurrences SET {column}=? WHERE item_id='a' AND file_id='a-file'"),[&large])?;}
            }Ok(())
        }).unwrap();
        let h = host();
        let (mut s, cap) = admitted(&f, &h, vec![selected(&f, "a", "'a','b'", vec![Title])]);
        assert_eq!(
            code(get(&mut s, &cap, &h, "a")),
            ScopeErrorCode::ByteBudget,
            "{target}"
        );
    }
}
#[test]
fn components_discard_wal_stale_output_and_hold_exact_host_gate_through_delivery() {
    let f = fixture(37);
    f.writer
        .db
        .pragma_update(None, "journal_mode", "WAL")
        .unwrap();
    let h = host();
    let grant = selected(&f, "a", "'a'", vec![Codec]);
    let (mut s, cap) = admitted(&f, &h, vec![grant.clone()]);
    assert_eq!(
        code(get(&mut s, &cap, &host(), "a")),
        ScopeErrorCode::WrongAuthority
    );
    let op = operation(budget()).unwrap();
    let active = op.enter().unwrap();
    let poisoned = Cell::new(false);
    let result = s.fresh(|slot, _| {
        let out = details::read(
            slot.as_ref().unwrap(),
            &cap.0,
            &cap.0.members,
            details::Request::Components("a".into()),
            Instant::now(),
            budget(),
            &poisoned,
        )?;
        f.mutate("after components are prepared");
        Ok(out)
    });
    assert_eq!(code(result), ScopeErrorCode::StaleScope);
    drop(active);
    let (mut s, cap) = admitted(&f, &h, vec![grant]);
    let out = s
        .read_host_scope(
            &cap,
            lease(&h),
            ScopeReadRequest::GetComponents("a".into()),
            budget(),
        )
        .unwrap();
    let revoked = h.begin_revocation().unwrap();
    assert_eq!(
        revoked.wait_drained(Duration::ZERO),
        Err(LeaseError::Timeout)
    );
    out.deliver(|value| {
        assert_eq!(value["scoped_total"], 1);
        assert_eq!(
            revoked.wait_drained(Duration::ZERO),
            Err(LeaseError::Timeout)
        );
    });
    assert_eq!(revoked.wait_drained(Duration::ZERO), Ok(()));
    assert!(matches!(h.acquire("rev-1"), Err(LeaseError::Revoked)));
}
#[test]
fn visible_slice_whole_occurrence_facts_never_grant_interval_effects() {
    use crate::catalog_commands::{self as commands, ReceiptPolicy, TrustedCommandAuthority};
    use crate::catalog_identity_mutation::{
        self as identity, ContextChange, IdentityAuthority, IdentityMutation, TimelineChange,
        TimelineState, VersionChange,
    };
    use catabolic_core::catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility};
    use catabolic_core::catalog_identity::{
        ExactFileRevision, IntervalUs, SourceExtent, VersionBinding, VersionContract, VersionState,
    };
    let f = fixture(37);
    f.writer
        .transaction(|db| {
            db.execute(
                "UPDATE item_files SET file_id='a-file' WHERE item_id='hidden'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    crate::public_components::call(
        &f.writer,
        "default",
        "index_file",
        &json!({"file_id":"a-file"}),
    )
    .unwrap();
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
            let start = if file == "a-file" && item == "hidden" {
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
    let intervals=crate::migration::rows(&f.writer.db,"SELECT b.version_id,r.source_start_us,r.source_end_us FROM catalog_bindings b JOIN catalog_binding_revisions r ON r.binding_id=b.id WHERE r.file_id='a-file' ORDER BY r.source_start_us",&[]).unwrap();
    assert_eq!(intervals,json!([{"version_id":"version-a","source_start_us":0,"source_end_us":60000000},{"version_id":"version-hidden","source_start_us":60000000,"source_end_us":120000000}]).as_array().unwrap().clone());
    let h = host();
    let plain = TrustedSelection::new(f.selection(), vec!["title".into()]).unwrap();
    let (mut session, cap) = admitted(&f, &h, vec![plain]);
    assert_eq!(
        code(get(&mut session, &cap, &h, "a")),
        ScopeErrorCode::NotFoundOrNotVisible
    );
    let grant = selected(&f, "whole-occurrences", "'a'", vec![Codec, FormatName]);
    let (mut session, cap) = admitted(&f, &h, vec![grant]);
    let out = get(&mut session, &cap, &h, "a").unwrap();
    assert_eq!(out["physical_scope"], "whole_occurrence_recorded");
    assert_eq!(out["scoped_total"], 1);
    assert_eq!(out["components"][0]["fields"]["codec"], "aac");
    assert!(!encode(&out).contains("version-hidden"));
    assert!(
        identity::legacy_association_admission(&f.writer, "default", &visible_association).is_err()
    );
}
#[test]
fn logical_identity_and_revision_are_explicit_and_masks_do_not_cross_kinds() {
    let f = fixture(37);
    let h = host();
    let audio = selected(&f, "audio", "'a'", vec![LogicalIdentity, RecordedRevision]);
    let video = selection(
        &f,
        "video",
        "'a'",
        vec!["primary"],
        vec![Video],
        vec![Codec],
    );
    let (mut s, cap) = admitted(&f, &h, vec![audio, video]);
    let out = get(&mut s, &cap, &h, "a").unwrap();
    assert_eq!(out["scoped_total"], 2);
    for row in out["components"].as_array().unwrap() {
        if row["kind"] == "audio" {
            assert!(row["fields"]["component_id"].is_string());
            assert!(row["fields"]["recorded_revision"].is_string());
            assert_eq!(row["fields"].as_object().unwrap().len(), 2);
        } else {
            assert_eq!(row["fields"], json!({"codec":"h264"}));
        }
    }
}
#[test]
fn inherited_paths_require_component_purpose_exact_kind_role_and_current_source() {
    for case in [
        "metadata-only",
        "wrong-kind",
        "wrong-role",
        "inactive",
        "stale",
        "unsupported",
    ] {
        let f = fixture(37);
        derive(&f, "a", "b", 0);
        let h = host();
        let root = selected(&f, "root", "'a'", vec![Title]);
        let source = match case {
            "metadata-only" => TrustedSelection::new(
                f.saved(
                    "source",
                    "selection",
                    "sql",
                    "SELECT item_id FROM catalog_items WHERE item_id='b'",
                ),
                vec!["title".into()],
            )
            .unwrap(),
            "wrong-kind" => selection(
                &f,
                "source",
                "'b'",
                vec!["primary"],
                vec![Video],
                vec![Title],
            ),
            "wrong-role" => selection(
                &f,
                "source",
                "'b'",
                vec!["subtitle"],
                vec![Audio],
                vec![Title],
            ),
            _ => selected(&f, "source", "'b'", vec![Title]),
        };
        f.writer
            .transaction(|db| {
                match case {
                    "inactive" => {
                        db.execute("UPDATE item_files SET active=0 WHERE item_id='b'", [])?;
                    }
                    "stale" => {
                        db.execute(
                            "UPDATE observations SET size=size+1 WHERE file_id='b-file'",
                            [],
                        )?;
                    }
                    "unsupported" => {
                        db.execute(
                            "UPDATE component_output_lineage SET relationship='unknown-kind'",
                            [],
                        )?;
                    }
                    _ => {}
                }
                Ok(())
            })
            .unwrap();
        let (mut s, cap) = admitted(&f, &h, vec![root, source]);
        assert_eq!(
            get(&mut s, &cap, &h, "a").unwrap()["scoped_total"],
            0,
            "{case}"
        );
    }
}
#[test]
fn acyclic_lineage_depth_is_bounded_before_frozen_normalization() {
    let f = fixture(37);
    for n in 0..33 {
        let item = format!("chain{n}");
        let file = format!("{item}-file");
        let job = format!("probe-{file}");
        curation::item_put(&f.writer, &json!({"id":item,"kind":"video","metadata":{}})).unwrap();
        f.writer
            .transaction(|db| {
                db.execute(
                    "INSERT INTO files VALUES(?,'media',?)",
                    params![file, format!("DO_NOT_OPEN/{file}")],
                )?;
                db.execute(
                    "INSERT INTO observations VALUES('default',?,1000,10,1,1,'present','scan')",
                    [&file],
                )?;
                Ok(())
            })
            .unwrap();
        curation::dispatch(
            &f.writer,
            "default",
            "associate",
            &json!({"item_id":item,"file_id":file,"role":"primary","part":1}),
        )
        .unwrap();
        let snapshot = crate::components::occurrence(&f.writer.db, "default", &file).unwrap();
        let data = json!({"format":{"format_name":"matroska"},"streams":[{"index":0,"codec_type":"audio","codec_name":"aac"}]});
        f.writer.transaction(|db|{db.execute("INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state,result) VALUES(?,'default',?,'probe','media',?,'{}',?,'complete',?)",params![job,file,encode(&snapshot),job,encode(&data)])?;db.execute("INSERT INTO file_facts VALUES('default',?,'probe',?,?,?,'complete')",params![file,job,encode(&snapshot),encode(&data)])?;Ok(())}).unwrap();
        crate::public_components::call(
            &f.writer,
            "default",
            "index_file",
            &json!({"file_id":file}),
        )
        .unwrap();
    }
    for n in (0..32).rev() {
        derive(&f, &format!("chain{n}"), &format!("chain{}", n + 1), 0);
    }
    derive(&f, "a", "chain0", 0);
    let h = host();
    let grant = TrustedSelection::new(
        f.saved(
            "chain",
            "selection",
            "sql",
            "SELECT item_id FROM catalog_items WHERE item_id != 'hidden'",
        ),
        vec![],
    )
    .unwrap()
    .with_components(
        TrustedComponentsRead::whole_occurrences(vec!["primary".into()], vec![Audio], vec![Codec])
            .unwrap(),
    );
    // The depth bound, not wall-clock admission, is under test. A 33-link debug
    // chain can exceed the shared five-second fixture budget in parallel runs,
    // so admission and both reads use the maximum budget.
    let long = || ReadBudget::new(Duration::from_secs(60), 1000).unwrap();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = s
        .admit_host_scope(
            TrustedHostReadGrant::new(h.clone(), "default".into(), vec![grant], vec![]).unwrap(),
            &lease(&h),
            long(),
        )
        .unwrap();
    let mut get = |item: &str| {
        s.read_host_scope(
            &cap,
            lease(&h),
            ScopeReadRequest::GetComponents(item.into()),
            long(),
        )
        .map(|output| output.deliver(Clone::clone))
        .unwrap()
    };
    assert_eq!(get("a")["scoped_total"], 0);
    // The same validated chain is useful below the explicit depth bound.
    assert_eq!(
        get("chain2")["components"][0]["fields"],
        json!({"codec":"aac"})
    );
}
#[test]
fn recorded_current_components_preserve_resolver_conflicts_without_playability_claims() {
    let f = fixture(37);
    curation::dispatch(&f.writer,"default","associate",&json!({"item_id":"b","file_id":"b-file","role":"primary","part":1,"metadata":{"title":"conflicting association title"}})).unwrap();
    let h = host();
    let (mut s, cap) = admitted(
        &f,
        &h,
        vec![selected(
            &f,
            "b",
            "'b'",
            vec![Title, Codec, TechnicallyVerified],
        )],
    );
    let out = get(&mut s, &cap, &h, "b").unwrap();
    assert_eq!(out["scoped_total"], 1);
    assert_eq!(out["components"][0]["recorded_current"], true);
    assert_eq!(
        out["components"][0]["fields"],
        json!({"title":null,"codec":"aac","technically_verified":1})
    );
    for denied in [
        "compatibility",
        "playback",
        "ready",
        "conflicting association title",
        "title-b",
        "conflicts",
    ] {
        assert!(!encode(&out).contains(denied), "{denied}");
    }
}
