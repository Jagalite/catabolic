use super::*;
use crate::{curation, definitions, migration};
use catabolic_core::catalog_host_lease::LeaseError;
use std::path::PathBuf;

use crate::catalog_schema37_test_support as schema37_fixture;

struct Fixture {
    _root: tempfile::TempDir,
    path: PathBuf,
    writer: Store,
}
impl Fixture {
    fn new() -> Self {
        Self::at(34)
    }
    fn at(schema: u32) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.sqlite3");
        migration::initialize_native_at(&path, schema).unwrap();
        let writer = Store::open(&path, true, false).unwrap();
        for (id, title, secret) in [
            ("a", "Alpha", "visible-secret"),
            ("b", "Beta", "visible-secret-b"),
            ("hidden", "Hidden", "DO_NOT_DISCLOSE"),
        ] {
            curation::item_put(&writer, &json!({"id":id,"kind":"video","metadata":{"title":title,"year":2020,"secret":secret}})).unwrap();
        }
        Self {
            _root: root,
            path,
            writer,
        }
    }
    fn upgrade37(self) -> Self {
        let Self {
            _root,
            path,
            writer,
        } = self;
        drop(writer);
        schema37_fixture::upgrade(&path);
        let writer = Store::open(&path, true, false).unwrap();
        Self {
            _root,
            path,
            writer,
        }
    }
    fn saved(&self, name: &str, mode: &str, language: &str, query: &str) -> SavedQueryRef {
        let row = definitions::query_put(
            &self.writer,
            "default",
            name,
            &json!({"version":1,"mode":mode,"selection":{"language":language,"query":query}}),
        )
        .unwrap();
        SavedQueryRef::new(
            row["id"].as_str().unwrap().into(),
            row["revision"].as_i64().unwrap(),
            row["digest"].as_str().unwrap().into(),
        )
        .unwrap()
    }
    fn selection(&self) -> SavedQueryRef {
        self.saved(
            "visible",
            "selection",
            "sql",
            "SELECT item_id FROM catalog_items WHERE item_id IN ('a','b') ORDER BY item_id",
        )
    }
    fn mutate(&self, value: &str) {
        curation::item_put(
            &self.writer,
            &json!({"id":"hidden","kind":"video","metadata":{"title":value}}),
        )
        .unwrap();
    }
}
fn budget() -> ReadBudget {
    ReadBudget::new(Duration::from_secs(5), 1000).unwrap()
}
fn host() -> HostAuthorization {
    HostAuthorization::new("fixture-host".into(), "rev-1".into()).unwrap()
}
fn lease(host: &HostAuthorization) -> AuthorizationLease {
    host.acquire("rev-1").unwrap()
}
fn grant(
    host: &HostAuthorization,
    selection: SavedQueryRef,
    reports: Vec<SavedQueryRef>,
) -> TrustedHostReadGrant {
    TrustedHostReadGrant::new(
        host.clone(),
        "default".into(),
        vec![TrustedSelection::new(selection, vec!["title".into()]).unwrap()],
        reports,
    )
    .unwrap()
}
fn read(
    session: &mut ScopedReadSession,
    cap: &ScopeHandle,
    host: &HostAuthorization,
    request: ScopeReadRequest,
) -> Result<Value> {
    session
        .read_host_scope(cap, lease(host), request, budget())
        .map(|output| output.deliver(Clone::clone))
}
fn code<T>(result: Result<T>) -> ScopeErrorCode {
    match result {
        Ok(_) => panic!("unexpected success"),
        Err(e) => e.code,
    }
}

#[test]
fn scoped_item_details_hidden_missing_and_fields() {
    let f = Fixture::new();
    let selection = f.selection();
    let host = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = s
        .admit_host_scope(grant(&host, selection, vec![]), &lease(&host), budget())
        .unwrap();
    let visible = read(&mut s, &cap, &host, ScopeReadRequest::GetItem("a".into())).unwrap();
    assert_eq!(
        visible["result"]["rows"][0][2],
        encode(&json!({"title":"Alpha"}))
    );
    assert!(!encode(&visible).contains("secret"));
    assert_eq!(
        code(read(
            &mut s,
            &cap,
            &host,
            ScopeReadRequest::GetItem("hidden".into())
        )),
        ScopeErrorCode::NotFoundOrNotVisible
    );
    assert_eq!(
        code(read(
            &mut s,
            &cap,
            &host,
            ScopeReadRequest::GetItem("missing".into())
        )),
        ScopeErrorCode::NotFoundOrNotVisible
    );
    assert!(s.store.as_ref().unwrap().db.is_autocommit());
}
#[test]
fn scope_precedes_report_counts_facets_filters_and_page_offsets() {
    let f = Fixture::new();
    let selection = f.selection();
    let report=f.saved("facets","rows","sql","SELECT count(*) AS count,json_extract(metadata,'$.secret') AS secret FROM catalog_items GROUP BY json_extract(metadata,'$.secret')");
    let host = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = s
        .admit_host_scope(
            grant(&host, selection, vec![report.clone()]),
            &lease(&host),
            budget(),
        )
        .unwrap();
    let result = read(
        &mut s,
        &cap,
        &host,
        ScopeReadRequest::SavedReport {
            query_id: report.id,
            maximum_rows: 10,
        },
    )
    .unwrap();
    assert_eq!(result["result"]["rows"], json!([[2, null]]));
    let first = read(
        &mut s,
        &cap,
        &host,
        ScopeReadRequest::ItemsPage {
            cursor: None,
            limit: 1,
        },
    )
    .unwrap();
    let page = read(
        &mut s,
        &cap,
        &host,
        ScopeReadRequest::ItemsPage {
            cursor: first["next_cursor"].as_str().map(str::to_owned),
            limit: 1,
        },
    )
    .unwrap();
    assert_eq!(page["result"]["rows"][0][0], "b");
    assert_eq!(page["result"]["rows"][0][3], 2);
}
#[test]
fn complete_saved_graphql_membership_uses_existing_evaluation() {
    let f = Fixture::new();
    let selection=f.saved("graphql-scope","selection","graphql",r#"query($after: String) { items(search:"Alpha", after:$after) { nodes { id } pageInfo { hasNextPage endCursor } } }"#);
    let host = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = s
        .admit_host_scope(grant(&host, selection, vec![]), &lease(&host), budget())
        .unwrap();
    assert!(read(&mut s, &cap, &host, ScopeReadRequest::GetItem("a".into())).is_ok());
    assert_eq!(
        code(read(
            &mut s,
            &cap,
            &host,
            ScopeReadRequest::GetItem("b".into())
        )),
        ScopeErrorCode::NotFoundOrNotVisible
    );
}
#[test]
fn main_read_bypasses_fail_closed_in_approved_saved_reports() {
    let f = Fixture::new();
    let selection = f.selection();
    let attacks = [
        "SELECT metadata FROM main.items",
        "SELECT count(*) FROM main.items",
        "SELECT i.metadata FROM catalog_items c CROSS JOIN main.items i",
        "SELECT (SELECT metadata FROM main.items WHERE id='hidden') FROM catalog_items",
        "WITH items AS (SELECT * FROM main.items) SELECT metadata FROM items",
        "WITH catalog_items AS (SELECT * FROM main.items) SELECT metadata FROM catalog_items",
        "SELECT * FROM catalog_identities",
        "SELECT * FROM catalog_relationships",
        "SELECT * FROM main.saved_queries",
        "SELECT * FROM temp.sqlite_schema",
        "SELECT * FROM pragma_table_info('items')",
        "SELECT api_metadata('hidden','{}')",
        "PRAGMA main.user_version",
        "SELECT load_extension('missing')",
    ];
    let reports = attacks
        .iter()
        .enumerate()
        .map(|(i, q)| f.saved(&format!("attack-{i}"), "rows", "sql", q))
        .collect::<Vec<_>>();
    let host = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = s
        .admit_host_scope(
            grant(&host, selection, reports.clone()),
            &lease(&host),
            budget(),
        )
        .unwrap();
    for (report, attack) in reports.iter().zip(attacks) {
        let result = read(
            &mut s,
            &cap,
            &host,
            ScopeReadRequest::SavedReport {
                query_id: report.id.clone(),
                maximum_rows: 10,
            },
        );
        assert_eq!(code(result), ScopeErrorCode::QueryRejected, "{attack}");
        assert!(s.store.as_ref().unwrap().db.is_autocommit());
    }
}
#[test]
fn unknown_report_and_hidden_report_identifiers_are_indistinguishable() {
    let f = Fixture::new();
    let selection = f.selection();
    let report = f.saved("not-approved", "rows", "sql", "SELECT * FROM catalog_items");
    let host = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = s
        .admit_host_scope(grant(&host, selection, vec![]), &lease(&host), budget())
        .unwrap();
    for id in [report.id, "does-not-exist".into()] {
        assert_eq!(
            code(read(
                &mut s,
                &cap,
                &host,
                ScopeReadRequest::SavedReport {
                    query_id: id,
                    maximum_rows: 10
                }
            )),
            ScopeErrorCode::NotFoundOrNotVisible
        );
    }
}
#[test]
fn concurrent_mutation_before_every_page_requires_resynchronization() {
    let f = Fixture::new();
    let selection = f.selection();
    let host = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    // Each page attempt first succeeds at this exact generation. A separate
    // writer then commits while the scoped owner holds no retained snapshot.
    for offset in 0..2 {
        let cap = s
            .admit_host_scope(
                grant(&host, selection.clone(), vec![]),
                &lease(&host),
                budget(),
            )
            .unwrap();
        assert!(
            read(
                &mut s,
                &cap,
                &host,
                ScopeReadRequest::ItemsPage {
                    cursor: None,
                    limit: 1
                }
            )
            .is_ok()
        );
        f.mutate(&format!("changed-{offset}"));
        assert_eq!(
            code(read(
                &mut s,
                &cap,
                &host,
                ScopeReadRequest::ItemsPage {
                    cursor: None,
                    limit: 1
                }
            )),
            ScopeErrorCode::StaleScope
        );
        assert!(s.store.as_ref().unwrap().db.is_autocommit());
    }
}
#[test]
fn legacy_readonly_store_keeps_retained_snapshot() {
    let f = Fixture::new();
    f.writer
        .db
        .pragma_update(None, "journal_mode", "WAL")
        .unwrap();
    let legacy = Store::open(&f.path, false, false).unwrap();
    let before = snapshot(&legacy).unwrap();
    assert!(!legacy.db.is_autocommit());
    f.mutate("changed");
    assert_eq!(snapshot(&legacy).unwrap(), before);
    let fresh = ScopedReadSession::open(&f.path).unwrap();
    assert_ne!(snapshot(fresh.store.as_ref().unwrap()).unwrap(), before);
}
#[test]
fn ambient_transaction_is_refused_without_ending_caller_transaction() {
    let f = Fixture::new();
    let selection = f.selection();
    let host = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    s.store.as_ref().unwrap().db.execute_batch("BEGIN").unwrap();
    assert_eq!(
        code(s.admit_host_scope(grant(&host, selection, vec![]), &lease(&host), budget())),
        ScopeErrorCode::AmbientTransaction
    );
    assert!(!s.store.as_ref().unwrap().db.is_autocommit());
    s.store
        .as_ref()
        .unwrap()
        .db
        .execute_batch("ROLLBACK")
        .unwrap();
}
#[test]
fn session_and_host_arc_identity_cannot_be_rebound_by_equal_strings() {
    let f = Fixture::new();
    let selection = f.selection();
    let host = host();
    let other = HostAuthorization::new(host.namespace().into(), host.revision().into()).unwrap();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let mut second = ScopedReadSession::open(&f.path).unwrap();
    assert_eq!(
        code(s.admit_host_scope(
            grant(&host, selection.clone(), vec![]),
            &lease(&other),
            budget()
        )),
        ScopeErrorCode::WrongAuthority
    );
    let cap = s
        .admit_host_scope(grant(&host, selection, vec![]), &lease(&host), budget())
        .unwrap();
    assert_eq!(
        code(read(
            &mut second,
            &cap,
            &host,
            ScopeReadRequest::GetItem("a".into())
        )),
        ScopeErrorCode::WrongSession
    );
    assert_eq!(
        code(read(
            &mut s,
            &cap,
            &other,
            ScopeReadRequest::GetItem("a".into())
        )),
        ScopeErrorCode::WrongAuthority
    );
}
#[test]
fn immutable_definition_revision_digest_and_dependency_closure_are_pinned() {
    let f = Fixture::new();
    let a = f.selection();
    let b = f.saved(
        "b",
        "selection",
        "sql",
        "SELECT item_id FROM catalog_items WHERE item_id='b'",
    );
    let row = definitions::query_put(
        &f.writer,
        "default",
        "combined",
        &json!({"version":1,"mode":"selection","combine":"union","queries":[a.id,b.id]}),
    )
    .unwrap();
    let root = SavedQueryRef::new(
        row["id"].as_str().unwrap().into(),
        row["revision"].as_i64().unwrap(),
        row["digest"].as_str().unwrap().into(),
    )
    .unwrap();
    let host = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let mut wrong = root.clone();
    wrong.revision += 1;
    assert_eq!(
        code(s.admit_host_scope(grant(&host, wrong, vec![]), &lease(&host), budget())),
        ScopeErrorCode::SelectionChanged
    );
    let cap = s
        .admit_host_scope(grant(&host, root, vec![]), &lease(&host), budget())
        .unwrap();
    assert_eq!(cap.0.dependencies.len(), 3);
    assert_eq!(cap.0.dependencies.get(&a.id).unwrap(), &a);
    assert_eq!(cap.0.dependencies.get(&b.id).unwrap(), &b);
}
#[test]
fn complete_selection_and_report_truncation_are_typed_refusals() {
    let f = Fixture::new();
    let selection = f.selection();
    let report = f.saved("too-many", "rows", "sql", "SELECT * FROM catalog_items");
    let host = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let low = ReadBudget::new(Duration::from_secs(5), 1).unwrap();
    assert_eq!(
        code(s.admit_host_scope(grant(&host, selection.clone(), vec![]), &lease(&host), low)),
        ScopeErrorCode::IdBudget
    );
    let cap = s
        .admit_host_scope(
            grant(&host, selection, vec![report.clone()]),
            &lease(&host),
            budget(),
        )
        .unwrap();
    assert_eq!(
        code(read(
            &mut s,
            &cap,
            &host,
            ScopeReadRequest::SavedReport {
                query_id: report.id,
                maximum_rows: 1
            }
        )),
        ScopeErrorCode::IdBudget
    );
    assert!(read(&mut s, &cap, &host, ScopeReadRequest::GetItem("a".into())).is_ok());
}
#[test]
fn lease_survives_result_delivery_and_unwinding() {
    let f = Fixture::new();
    let selection = f.selection();
    let host = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = s
        .admit_host_scope(grant(&host, selection, vec![]), &lease(&host), budget())
        .unwrap();
    let output = s
        .read_host_scope(
            &cap,
            lease(&host),
            ScopeReadRequest::GetItem("a".into()),
            budget(),
        )
        .unwrap();
    let revocation = host.begin_revocation().unwrap();
    assert_eq!(
        revocation.wait_drained(Duration::ZERO),
        Err(LeaseError::Timeout)
    );
    assert!(matches!(host.acquire("rev-1"), Err(LeaseError::Revoked)));
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        output.deliver(|value| {
            assert_eq!(value["result"]["row_count"], 1);
            assert_eq!(
                revocation.wait_drained(Duration::ZERO),
                Err(LeaseError::Timeout)
            );
            panic!("host delivery failed");
        })
    }));
    assert!(unwind.is_err());
    assert_eq!(revocation.wait_drained(Duration::ZERO), Ok(()));
}
#[test]
fn oversized_raw_metadata_is_refused_before_decode_even_when_masked() {
    let f = Fixture::new();
    curation::item_put(&f.writer,&json!({"id":"a","kind":"video","metadata":{"title":"Alpha","secret":"x".repeat(MAX_DEFINITION_BYTES)}})).unwrap();
    let selection = f.selection();
    let host = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = s
        .admit_host_scope(grant(&host, selection, vec![]), &lease(&host), budget())
        .unwrap();
    assert_eq!(
        code(read(
            &mut s,
            &cap,
            &host,
            ScopeReadRequest::GetItem("a".into())
        )),
        ScopeErrorCode::ByteBudget
    );
}
#[test]
fn trusted_masks_are_explicit_validated_and_union_only_for_matching_selections() {
    let f = Fixture::new();
    let a = f.saved(
        "a-only",
        "selection",
        "sql",
        "SELECT item_id FROM catalog_items WHERE item_id='a'",
    );
    let b = f.saved(
        "b-only",
        "selection",
        "sql",
        "SELECT item_id FROM catalog_items WHERE item_id='b'",
    );
    assert!(TrustedSelection::new(a.clone(), vec!["x".repeat(257)]).is_err());
    let host = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let grant = TrustedHostReadGrant::new(
        host.clone(),
        "default".into(),
        vec![
            TrustedSelection::new(a, vec!["title".into()]).unwrap(),
            TrustedSelection::new(b, vec!["year".into()]).unwrap(),
        ],
        vec![],
    )
    .unwrap();
    let cap = s.admit_host_scope(grant, &lease(&host), budget()).unwrap();
    let a = read(&mut s, &cap, &host, ScopeReadRequest::GetItem("a".into())).unwrap();
    let b = read(&mut s, &cap, &host, ScopeReadRequest::GetItem("b".into())).unwrap();
    assert_eq!(a["result"]["rows"][0][2], encode(&json!({"title":"Alpha"})));
    assert_eq!(b["result"]["rows"][0][2], encode(&json!({"year":2020})));
}
#[test]
fn fresh_snapshot_detects_commit_during_read_before_output_exposure() {
    let f = Fixture::new();
    f.writer
        .db
        .pragma_update(None, "journal_mode", "WAL")
        .unwrap();
    let mut session = ScopedReadSession::open(&f.path).unwrap();
    let result = session.fresh(|_, _| {
        f.mutate("committed-during-read");
        Ok(json!({"would":"be stale"}))
    });
    assert_eq!(code(result), ScopeErrorCode::StaleScope);
    assert!(session.store.as_ref().unwrap().db.is_autocommit());
}
#[test]
fn cancellation_is_origin_typed_and_no_temp_or_transaction_state_leaks() {
    let f = Fixture::new();
    let selection = f.selection();
    let host = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cancelled = crate::embedding::with_interrupt_check(
        || true,
        || {
            Ok(s.admit_host_scope(
                grant(&host, selection.clone(), vec![]),
                &lease(&host),
                budget(),
            ))
        },
    )
    .unwrap();
    assert_eq!(code(cancelled), ScopeErrorCode::Cancelled);
    let cap = s
        .admit_host_scope(grant(&host, selection, vec![]), &lease(&host), budget())
        .unwrap();
    let cancelled = crate::embedding::with_interrupt_check(
        || true,
        || {
            Ok(read(
                &mut s,
                &cap,
                &host,
                ScopeReadRequest::GetItem("a".into()),
            ))
        },
    )
    .unwrap();
    assert_eq!(code(cancelled), ScopeErrorCode::Cancelled);
    assert!(s.store.as_ref().unwrap().db.is_autocommit());
    let temp_count: i64 = s
        .store
        .as_ref()
        .unwrap()
        .db
        .query_row("SELECT count(*) FROM sqlite_temp_schema", [], |r| r.get(0))
        .unwrap();
    assert_eq!(temp_count, 0);
    assert!(read(&mut s, &cap, &host, ScopeReadRequest::GetItem("a".into())).is_ok());
}
#[test]
fn snapshot_cleanup_runs_on_unwind_and_continues_with_fresh_state() {
    let f = Fixture::new();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: Result<()> = s.fresh(|slot, _| {
            slot.as_ref()
                .unwrap()
                .db
                .pragma_update(None, "query_only", false)?;
            slot.as_ref()
                .unwrap()
                .db
                .execute_batch("CREATE TEMP TABLE abandoned(id TEXT)")?;
            panic!("test admitted read unwind");
        });
    }));
    assert!(panic.is_err());
    assert!(s.store.as_ref().unwrap().db.is_autocommit());
    assert!(!s.poisoned);
    let count: i64 = s
        .store
        .as_ref()
        .unwrap()
        .db
        .query_row(
            "SELECT count(*) FROM sqlite_temp_schema WHERE name='abandoned'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    assert!(s.fresh(|_, _| Ok(())).is_ok());
}
#[test]
fn mask_expansion_is_bounded_before_per_item_field_cloning() {
    let f = Fixture::new();
    let selection = f.selection();
    let host = host();
    let fields = (0..10000)
        .map(|n| format!("{n:05}{}", "x".repeat(251)))
        .collect();
    let grant = TrustedHostReadGrant::new(
        host.clone(),
        "default".into(),
        vec![TrustedSelection::new(selection, fields).unwrap()],
        vec![],
    )
    .unwrap();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    assert_eq!(
        code(s.admit_host_scope(grant, &lease(&host), budget())),
        ScopeErrorCode::ByteBudget
    );
    assert!(s.store.as_ref().unwrap().db.is_autocommit());
}
#[test]
fn cleanup_failure_permanently_poisoned_owner_cannot_resume() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    let f = Fixture::new();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let failed = s.fresh(|slot, _| {
        slot.as_ref()
            .unwrap()
            .db
            .authorizer(Some(|context: AuthContext<'_>| {
                if matches!(context.action, AuthAction::Transaction { .. }) {
                    Authorization::Deny
                } else {
                    Authorization::Allow
                }
            }))?;
        Ok(())
    });
    assert_eq!(code(failed), ScopeErrorCode::Catalog);
    assert!(s.poisoned);
    assert_eq!(code(s.fresh(|_, _| Ok(()))), ScopeErrorCode::Catalog);
}
#[test]
fn one_item_budget_spans_membership_and_materialized_source_reads() {
    let f = Fixture::new();
    let selection = f.selection();
    let host = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = s
        .admit_host_scope(grant(&host, selection, vec![]), &lease(&host), budget())
        .unwrap();
    let work = OperationBudget::new(
        Duration::from_secs(5),
        BudgetLimits {
            items: 2,
            values: 10000,
            bytes: MAX_BYTES,
            work: 100000,
        },
    )
    .unwrap();
    let _active = work.enter().unwrap();
    let result = s.fresh(|slot, _| {
        let fields = membership(slot, &cap.0, budget())?;
        assert!(work.failure().is_none());
        materialize(
            slot.as_ref().unwrap(),
            &fields,
            Instant::now(),
            budget(),
            &Cell::new(false),
        )
    });
    assert_eq!(code(result), ScopeErrorCode::IdBudget);
    assert_eq!(work.failure(), Some(EvaluationFailure::IdBudget));
    assert!(s.store.as_ref().unwrap().db.is_autocommit());
}
#[test]
fn existing_and_new_store_session_owners_remain_send() {
    fn assert_send<T: Send>() {}
    assert_send::<Store>();
    assert_send::<crate::embedding::Session>();
    assert_send::<Evaluation>();
    assert_send::<ScopedReadSession>();
    assert_send::<ScopeHandle>();
    assert_send::<LeasedRead>();
    assert_send::<OperationBudget>();
}
#[test]
fn bounded_sql_vm_uses_existing_hook_and_shared_work_counter() {
    let f = Fixture::new();
    let work = OperationBudget::new(
        Duration::from_secs(5),
        BudgetLimits {
            items: 10,
            values: 100,
            bytes: MAX_BYTES,
            work: 0,
        },
    )
    .unwrap();
    let _active = work.enter().unwrap();
    let failure = Cell::new(None);
    let result = sql::execute_store_classified(
        &f.writer,
        Some(
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<100000) SELECT sum(x) FROM n",
        ),
        &sql::Options::default(),
        &failure,
    );
    assert!(result.is_err());
    assert_eq!(failure.get(), Some(sql::ExecutionFailure::WorkBudget));
    assert_eq!(work.failure(), Some(EvaluationFailure::WorkBudget));
    // The old SQL owner still clears its own VM hook. A regular query works once
    // this operation's bridge ends; no second hook was installed by the budget.
    drop(_active);
    assert!(sql::execute_store(&f.writer, Some("SELECT 1"), &sql::Options::default()).is_ok());
}
#[test]
fn nested_store_transactions_share_the_outer_budget_without_reset() {
    let f = Fixture::new();
    let work = OperationBudget::new(
        Duration::from_secs(5),
        BudgetLimits {
            items: 10,
            values: 100,
            bytes: 10,
            work: 100,
        },
    )
    .unwrap();
    let _active = work.enter().unwrap();
    f.writer
        .transaction(|_| {
            OperationBudget::current()
                .unwrap()
                .charge(BudgetCharge {
                    bytes: 6,
                    ..Default::default()
                })
                .unwrap();
            f.writer.transaction(|_| {
                assert_eq!(
                    OperationBudget::current().unwrap().charge(BudgetCharge {
                        bytes: 5,
                        ..Default::default()
                    }),
                    Err(EvaluationFailure::ByteBudget)
                );
                Ok(())
            })?;
            Ok(())
        })
        .unwrap();
    assert_eq!(work.failure(), Some(EvaluationFailure::ByteBudget));
}
#[test]
fn graphql_vm_reuses_the_shared_work_budget_without_a_second_hook() {
    let f = Fixture::new();
    f.writer
        .transaction(|db| {
            for n in 0..1000 {
                db.execute(
                    "INSERT INTO items VALUES(?,'video',?)",
                    params![format!("bulk-{n}"), encode(&json!({"title":"ordinary"}))],
                )?;
            }
            Ok(())
        })
        .unwrap();
    let selection=f.saved("graphql-work","selection","graphql",r#"query($after: String) { items(search:"no matching title",after:$after) { nodes { id } pageInfo { hasNextPage endCursor } } }"#);
    let host = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = s
        .admit_host_scope(grant(&host, selection, vec![]), &lease(&host), budget())
        .unwrap();
    let work = OperationBudget::new(
        Duration::from_secs(5),
        BudgetLimits {
            items: 10000,
            values: 10000,
            bytes: MAX_BYTES,
            work: 200,
        },
    )
    .unwrap();
    let _active = work.enter().unwrap();
    let result = s.fresh(|slot, _| membership(slot, &cap.0, budget()));
    assert_eq!(code(result), ScopeErrorCode::WorkBudget);
    assert_eq!(work.failure(), Some(EvaluationFailure::WorkBudget));
    drop(_active);
    assert!(s.store.as_ref().unwrap().db.is_autocommit());
    assert!(
        read(
            &mut s,
            &cap,
            &host,
            ScopeReadRequest::ItemsPage {
                cursor: None,
                limit: 1
            }
        )
        .is_ok()
    );
}
#[test]
fn budget_bridge_never_installs_or_overwrites_a_connection_progress_handler() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let db = rusqlite::Connection::open_in_memory().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    db.progress_handler(
        1,
        Some(move || {
            observed.fetch_add(1, Ordering::Relaxed);
            false
        }),
    )
    .unwrap();
    let work = OperationBudget::new(
        Duration::from_secs(5),
        BudgetLimits {
            items: 1,
            values: 1,
            bytes: 1,
            work: 1,
        },
    )
    .unwrap();
    {
        let _active = work.enter().unwrap();
        db.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))
            .unwrap();
    }
    let first = calls.load(Ordering::Relaxed);
    assert!(first > 0);
    db.query_row("SELECT 2", [], |r| r.get::<_, i64>(0))
        .unwrap();
    assert!(calls.load(Ordering::Relaxed) > first);
}
#[test]
fn aggregate_trusted_masks_are_bounded_before_scope_fingerprinting() {
    let f = Fixture::new();
    let selection = f.selection();
    let host = host();
    let fields = (0..10000)
        .map(|n| format!("{n:05}{}", "x".repeat(251)))
        .collect::<Vec<_>>();
    let selection = TrustedSelection::new(selection, fields).unwrap();
    let result = TrustedHostReadGrant::new(
        host,
        "default".into(),
        vec![selection.clone(), selection],
        vec![],
    );
    assert_eq!(code(result), ScopeErrorCode::ByteBudget);
}

fn page(cursor: Option<String>, limit: usize) -> ScopeReadRequest {
    ScopeReadRequest::ItemsPage { cursor, limit }
}
fn next(value: &Value) -> String {
    value["next_cursor"].as_str().unwrap().to_owned()
}
#[test]
fn opaque_cursors_continue_masked_binary_order_without_offset_authority() {
    let f = Fixture::new();
    let selection = f.selection();
    let host = host();
    let mut session = ScopedReadSession::open(&f.path).unwrap();
    let cap = session
        .admit_host_scope(grant(&host, selection, vec![]), &lease(&host), budget())
        .unwrap();
    let first = read(&mut session, &cap, &host, page(None, 1)).unwrap();
    assert_eq!(first["result"]["rows"][0][0], "a");
    assert_eq!(first["result"]["rows"][0][3], 2);
    assert_eq!(first["has_more"], true);
    assert_eq!(first["result"]["row_count"], 1);
    assert_eq!(first["result"]["max_rows"], 1);
    let token = next(&first);
    assert!(token.len() < MAX_CURSOR_BYTES);
    assert_ne!(token, "a");
    let second = read(&mut session, &cap, &host, page(Some(token.clone()), 1)).unwrap();
    assert_eq!(second["result"]["rows"][0][0], "b");
    assert_eq!(second["has_more"], false);
    assert!(second["next_cursor"].is_null());
    assert_eq!(
        read(&mut session, &cap, &host, page(Some(token), 1)).unwrap(),
        second
    );
    assert!(!encode(&first).contains("DO_NOT_DISCLOSE"));
    assert!(!encode(&second).contains("secret"));
    let all = read(&mut session, &cap, &host, page(None, 2)).unwrap();
    assert_eq!(all["result"]["row_count"], 2);
    assert_eq!(all["has_more"], false);
    assert!(all["next_cursor"].is_null());
}
#[test]
fn empty_scoped_page_has_no_cursor_or_unscoped_counts() {
    let f = Fixture::new();
    let selection = f.saved(
        "empty",
        "selection",
        "sql",
        "SELECT item_id FROM catalog_items WHERE 0",
    );
    let host = host();
    let mut session = ScopedReadSession::open(&f.path).unwrap();
    let cap = session
        .admit_host_scope(grant(&host, selection, vec![]), &lease(&host), budget())
        .unwrap();
    let empty = read(&mut session, &cap, &host, page(None, 1)).unwrap();
    assert_eq!(empty["result"]["rows"], json!([]));
    assert_eq!(empty["has_more"], false);
    assert!(empty["next_cursor"].is_null());
}
#[test]
fn tampering_oversize_empty_and_raw_item_ids_never_grant_cursor_scope() {
    let f = Fixture::new();
    let selection = f.selection();
    let host = host();
    let mut session = ScopedReadSession::open(&f.path).unwrap();
    let cap = session
        .admit_host_scope(grant(&host, selection, vec![]), &lease(&host), budget())
        .unwrap();
    let token = next(&read(&mut session, &cap, &host, page(None, 1)).unwrap());
    let mut tampered = token.as_bytes().to_vec();
    tampered[0] = if tampered[0] == b'A' { b'B' } else { b'A' };
    for bad in [
        String::from_utf8(tampered).unwrap(),
        "".into(),
        "hidden".into(),
        "a".into(),
        "A".repeat(MAX_CURSOR_BYTES + 1),
        "not a cursor".into(),
    ] {
        assert_eq!(
            code(read(&mut session, &cap, &host, page(Some(bad), 1))),
            ScopeErrorCode::InvalidCursor
        );
        assert!(session.store.as_ref().unwrap().db.is_autocommit());
    }
    assert!(read(&mut session, &cap, &host, page(Some(token.clone()), 1)).is_ok());
    assert_eq!(
        code(read(&mut session, &cap, &host, page(Some(token), 2))),
        ScopeErrorCode::InvalidCursor
    );
    assert_eq!(
        code(read(&mut session, &cap, &host, page(None, 0))),
        ScopeErrorCode::InvalidRequest
    );
    assert_eq!(
        code(read(&mut session, &cap, &host, page(None, 1001))),
        ScopeErrorCode::InvalidRequest
    );
}
#[test]
fn cursor_cannot_cross_capability_even_with_equal_host_labels_and_definitions() {
    let f = Fixture::new();
    let selection = f.selection();
    let host = host();
    let mut session = ScopedReadSession::open(&f.path).unwrap();
    let a = session
        .admit_host_scope(
            grant(&host, selection.clone(), vec![]),
            &lease(&host),
            budget(),
        )
        .unwrap();
    let b = session
        .admit_host_scope(
            grant(&host, selection.clone(), vec![]),
            &lease(&host),
            budget(),
        )
        .unwrap();
    let other_gate =
        HostAuthorization::new(host.namespace().into(), host.revision().into()).unwrap();
    let c = session
        .admit_host_scope(
            grant(&other_gate, selection, vec![]),
            &lease(&other_gate),
            budget(),
        )
        .unwrap();
    assert_ne!(a.0.capability_id, b.0.capability_id);
    assert_ne!(a.0.fingerprint, b.0.fingerprint);
    assert_ne!(a.0.fingerprint, c.0.fingerprint);
    let token = next(&read(&mut session, &a, &host, page(None, 1)).unwrap());
    assert_eq!(
        code(read(&mut session, &b, &host, page(Some(token.clone()), 1))),
        ScopeErrorCode::InvalidCursor
    );
    assert_eq!(
        code(read(
            &mut session,
            &c,
            &other_gate,
            page(Some(token.clone()), 1)
        )),
        ScopeErrorCode::InvalidCursor
    );
    assert_eq!(
        code(read(
            &mut session,
            &a,
            &other_gate,
            page(Some("invalid".into()), 1)
        )),
        ScopeErrorCode::WrongAuthority
    );
    assert!(
        read(
            &mut session,
            &a.clone(),
            &host,
            page(Some(token.clone()), 1)
        )
        .is_ok()
    );
    let mut other_session = ScopedReadSession::open(&f.path).unwrap();
    assert_eq!(
        code(read(&mut other_session, &a, &host, page(Some(token), 1))),
        ScopeErrorCode::WrongSession
    );
}
#[test]
fn cursor_cannot_cross_mask_report_roots_or_profile() {
    let f = Fixture::new();
    let selection = f.selection();
    let report = f.saved(
        "approved-report",
        "rows",
        "sql",
        "SELECT count(*) FROM catalog_items",
    );
    f.writer
        .db
        .execute("INSERT INTO profiles VALUES('other')", [])
        .unwrap();
    let other=definitions::query_put(&f.writer,"other","other-visible",&json!({"version":1,"mode":"selection","selection":{"language":"sql","query":"SELECT item_id FROM catalog_items WHERE item_id IN ('a','b')"}})).unwrap();
    let other_ref = SavedQueryRef::new(
        other["id"].as_str().unwrap().into(),
        other["revision"].as_i64().unwrap(),
        other["digest"].as_str().unwrap().into(),
    )
    .unwrap();
    let host = host();
    let mut session = ScopedReadSession::open(&f.path).unwrap();
    let a = session
        .admit_host_scope(
            grant(&host, selection.clone(), vec![]),
            &lease(&host),
            budget(),
        )
        .unwrap();
    let year = TrustedHostReadGrant::new(
        host.clone(),
        "default".into(),
        vec![TrustedSelection::new(selection.clone(), vec!["year".into()]).unwrap()],
        vec![],
    )
    .unwrap();
    let b = session
        .admit_host_scope(year, &lease(&host), budget())
        .unwrap();
    let c = session
        .admit_host_scope(
            grant(&host, selection, vec![report]),
            &lease(&host),
            budget(),
        )
        .unwrap();
    let other = TrustedHostReadGrant::new(
        host.clone(),
        "other".into(),
        vec![TrustedSelection::new(other_ref, vec!["title".into()]).unwrap()],
        vec![],
    )
    .unwrap();
    let d = session
        .admit_host_scope(other, &lease(&host), budget())
        .unwrap();
    let token = next(&read(&mut session, &a, &host, page(None, 1)).unwrap());
    for cap in [&b, &c, &d] {
        assert_eq!(
            code(read(&mut session, cap, &host, page(Some(token.clone()), 1))),
            ScopeErrorCode::InvalidCursor
        );
    }
}
#[test]
fn membership_changes_without_catalog_generation_invalidate_pages_and_details() {
    let f = Fixture::new();
    let selection=f.saved("receipt-selected","selection","sql","SELECT item_id FROM catalog_items WHERE item_id IN ('a','b') AND (item_id='a' OR NOT EXISTS(SELECT 1 FROM catalog_command_receipts)) ORDER BY item_id");
    let host = host();
    let mut session = ScopedReadSession::open(&f.path).unwrap();
    let cap = session
        .admit_host_scope(
            grant(&host, selection.clone(), vec![]),
            &lease(&host),
            budget(),
        )
        .unwrap();
    let token = next(&read(&mut session, &cap, &host, page(None, 1)).unwrap());
    let generation: i64 = f
        .writer
        .db
        .query_row("SELECT generation FROM catalog_state", [], |r| r.get(0))
        .unwrap();
    f.writer.db.execute("INSERT INTO catalog_command_receipts(catalog_id,host_namespace,operation_id,contract_version,canonicalization_version,payload_digest,admission_incarnation,expected_generation,committed_generation,committed_at_us,retain_until_us,result_state,result_json)
     SELECT catalog_id,'receipt-fixture','membership-change',1,1,?,incarnation,generation,generation,0,1,'full','{}' FROM catalog_state",["a".repeat(64)]).unwrap();
    assert_eq!(
        f.writer
            .db
            .query_row("SELECT generation FROM catalog_state", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        generation
    );
    assert_eq!(
        code(read(
            &mut session,
            &cap,
            &host,
            page(Some(token.clone()), 1)
        )),
        ScopeErrorCode::StaleScope
    );
    assert_eq!(
        code(read(
            &mut session,
            &cap,
            &host,
            ScopeReadRequest::GetItem("a".into())
        )),
        ScopeErrorCode::StaleScope
    );
    let fresh = session
        .admit_host_scope(grant(&host, selection, vec![]), &lease(&host), budget())
        .unwrap();
    assert_eq!(
        code(read(&mut session, &fresh, &host, page(Some(token), 1))),
        ScopeErrorCode::InvalidCursor
    );
    let first = read(&mut session, &fresh, &host, page(None, 1)).unwrap();
    assert_eq!(first["has_more"], false);
    assert_eq!(first["result"]["row_count"], 1);
}
#[test]
fn cursor_generation_fence_requires_fresh_scope_after_catalog_change() {
    let f = Fixture::new();
    let selection = f.selection();
    let host = host();
    let mut session = ScopedReadSession::open(&f.path).unwrap();
    let cap = session
        .admit_host_scope(
            grant(&host, selection.clone(), vec![]),
            &lease(&host),
            budget(),
        )
        .unwrap();
    let token = next(&read(&mut session, &cap, &host, page(None, 1)).unwrap());
    f.mutate("changed");
    assert_eq!(
        code(read(
            &mut session,
            &cap,
            &host,
            page(Some(token.clone()), 1)
        )),
        ScopeErrorCode::StaleScope
    );
    let new = session
        .admit_host_scope(grant(&host, selection, vec![]), &lease(&host), budget())
        .unwrap();
    assert_eq!(
        code(read(&mut session, &new, &host, page(Some(token), 1))),
        ScopeErrorCode::InvalidCursor
    );
}
#[test]
fn authenticated_malformed_positions_and_stale_membership_digest_are_typed() {
    let f = Fixture::new();
    let selection = f.selection();
    let host = host();
    let mut session = ScopedReadSession::open(&f.path).unwrap();
    let cap = session
        .admit_host_scope(grant(&host, selection, vec![]), &lease(&host), budget())
        .unwrap();
    for position in [
        json!({"after":"a","membership":"x"}),
        json!({"after":"","membership":cap.0.membership_digest}),
        json!({"after":"a","membership":cap.0.membership_digest,"extra":true}),
        json!(7),
    ] {
        let token = crate::api_catalog::authenticated_cursor(
            &cap.0.cursor_key,
            &cap.0.fingerprint,
            &cursor_scope(&cap.0, 1),
            &encode(&position),
        )
        .unwrap();
        assert_eq!(
            code(read(&mut session, &cap, &host, page(Some(token), 1))),
            ScopeErrorCode::InvalidCursor
        );
    }
    let token = crate::api_catalog::authenticated_cursor(
        &cap.0.cursor_key,
        &cap.0.fingerprint,
        &cursor_scope(&cap.0, 1),
        &encode(&json!({"after":"a","membership":"0".repeat(64)})),
    )
    .unwrap();
    assert_eq!(
        code(read(&mut session, &cap, &host, page(Some(token), 1))),
        ScopeErrorCode::StaleCursor
    );
}
#[test]
fn cursor_decode_charges_the_shared_byte_budget_and_recovers_cleanly() {
    let f = Fixture::new();
    let selection = f.selection();
    let host = host();
    let mut session = ScopedReadSession::open(&f.path).unwrap();
    let cap = session
        .admit_host_scope(grant(&host, selection, vec![]), &lease(&host), budget())
        .unwrap();
    let token = next(&read(&mut session, &cap, &host, page(None, 1)).unwrap());
    let work = OperationBudget::new(
        Duration::from_secs(5),
        BudgetLimits {
            items: 10000,
            values: 10000,
            bytes: token.len() - 1,
            work: 1000000,
        },
    )
    .unwrap();
    let active = work.enter().unwrap();
    assert_eq!(
        code(prepare_read_request(&cap.0, page(Some(token.clone()), 1))),
        ScopeErrorCode::ByteBudget
    );
    assert_eq!(work.failure(), Some(EvaluationFailure::ByteBudget));
    drop(active);
    assert!(read(&mut session, &cap, &host, page(Some(token), 1)).is_ok());
    assert!(session.store.as_ref().unwrap().db.is_autocommit());
}

#[test]
fn membership_digest_fences_same_connection_receipt_changes_without_data_version_change() {
    let f = Fixture::new();
    let selection = f.saved("receipt-selected", "selection", "sql", "SELECT item_id FROM catalog_items WHERE item_id IN ('a','b') AND (item_id='a' OR NOT EXISTS(SELECT 1 FROM catalog_command_receipts)) ORDER BY item_id");
    let host = host();
    drop(f.writer);
    let mut session = ScopedReadSession::open_private_writer(&f.path).unwrap();
    let cap = session
        .admit_host_scope(grant(&host, selection, vec![]), &lease(&host), budget())
        .unwrap();
    let token = next(&read(&mut session, &cap, &host, page(None, 1)).unwrap());
    let store = session.store.as_ref().unwrap();
    let before = snapshot(store).unwrap();
    store.db.execute("INSERT INTO catalog_command_receipts(catalog_id,host_namespace,operation_id,contract_version,canonicalization_version,payload_digest,admission_incarnation,expected_generation,committed_generation,committed_at_us,retain_until_us,result_state,result_json) SELECT catalog_id,'receipt-fixture','membership-change',1,1,?,incarnation,generation,generation,0,1,'full','{}' FROM catalog_state", ["a".repeat(64)]).unwrap();
    assert_eq!(snapshot(store).unwrap(), before);
    assert_eq!(
        code(read(&mut session, &cap, &host, page(Some(token), 1))),
        ScopeErrorCode::StaleCursor
    );
    assert_eq!(
        code(read(
            &mut session,
            &cap,
            &host,
            ScopeReadRequest::GetItem("a".into())
        )),
        ScopeErrorCode::StaleScope
    );
}

#[path = "catalog_scope_relations_tests.rs"]
mod relationship_tests;

#[test]
fn schema37_real_upgrade_preserves_masked_details_counts_cursor_and_exact_host_gate() {
    let f = Fixture::at(36);
    let selection = f.selection();
    let counts = f.saved("counts", "rows", "sql", "SELECT count(*),json_extract(metadata,'$.secret') FROM catalog_items GROUP BY json_extract(metadata,'$.secret')");
    let forbidden = f.saved(
        "source-denial",
        "rows",
        "sql",
        "SELECT count(*) FROM main.native_observation_companions",
    );
    let h = host();
    let admitted = || {
        grant(
            &h,
            selection.clone(),
            vec![counts.clone(), forbidden.clone()],
        )
    };
    let mut before = ScopedReadSession::open(&f.path).unwrap();
    let old = before
        .admit_host_scope(admitted(), &lease(&h), budget())
        .unwrap();
    let old_cursor = next(&read(&mut before, &old, &h, page(None, 1)).unwrap());
    drop(before);
    let f = f.upgrade37();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    assert_eq!(
        code(read(&mut s, &old, &h, page(None, 1))),
        ScopeErrorCode::WrongSession
    );
    let cap = s
        .admit_host_scope(admitted(), &lease(&h), budget())
        .unwrap();
    assert_eq!(
        code(read(&mut s, &cap, &h, page(Some(old_cursor), 1))),
        ScopeErrorCode::InvalidCursor
    );
    let detail = read(&mut s, &cap, &h, ScopeReadRequest::GetItem("a".into())).unwrap();
    assert_eq!(
        detail["result"]["rows"][0][2],
        encode(&json!({"title":"Alpha"}))
    );
    assert_eq!(
        code(read(
            &mut s,
            &cap,
            &h,
            ScopeReadRequest::GetItem("hidden".into())
        )),
        ScopeErrorCode::NotFoundOrNotVisible
    );
    let out = read(
        &mut s,
        &cap,
        &h,
        ScopeReadRequest::SavedReport {
            query_id: counts.id.clone(),
            maximum_rows: 10,
        },
    )
    .unwrap();
    assert_eq!(out["result"]["rows"], json!([[2, null]]));
    assert_eq!(
        code(read(
            &mut s,
            &cap,
            &h,
            ScopeReadRequest::SavedReport {
                query_id: forbidden.id.clone(),
                maximum_rows: 10
            }
        )),
        ScopeErrorCode::QueryRejected
    );
    let first = read(&mut s, &cap, &h, page(None, 1)).unwrap();
    let second = read(&mut s, &cap, &h, page(Some(next(&first)), 1)).unwrap();
    assert_eq!(second["result"]["rows"][0][0], "b");
    assert_eq!(second["result"]["rows"][0][3], 2);
    let other = host();
    assert_eq!(
        code(read(&mut s, &cap, &other, page(None, 1))),
        ScopeErrorCode::WrongAuthority
    );
    f.mutate("hidden modification");
    assert_eq!(
        code(read(&mut s, &cap, &h, page(None, 1))),
        ScopeErrorCode::StaleScope
    );
    let cap = s
        .admit_host_scope(admitted(), &lease(&h), budget())
        .unwrap();
    assert_eq!(
        read(&mut s, &cap, &h, page(None, 2)).unwrap()["result"]["row_count"],
        2
    );
}

#[path = "catalog_scope_details_tests.rs"]
mod detail_tests;

#[path = "catalog_scope_physical_tests.rs"]
mod physical_tests;

#[path = "catalog_scope_components_tests.rs"]
mod component_tests;

#[test]
fn expected_identity_is_checked_on_the_scoped_session_owner() {
    let f = Fixture::new();
    let catalog = f.writer.database_id.clone();
    let incarnation = f.writer.catalog_incarnation.clone().unwrap();
    assert!(ScopedReadSession::open_expected(&f.path, &catalog, &incarnation).is_ok());
    assert_eq!(
        code(ScopedReadSession::open_expected(
            &f.path,
            "other-catalog",
            &incarnation
        )),
        ScopeErrorCode::WrongAuthority
    );
    assert_eq!(
        code(ScopedReadSession::open_expected(
            &f.path,
            &catalog,
            "other-incarnation"
        )),
        ScopeErrorCode::WrongAuthority
    );
    assert_eq!(
        code(ScopedReadSession::open_expected(&f.path, "", &incarnation)),
        ScopeErrorCode::InvalidGrant
    );
    let replacement = Fixture::new();
    assert_eq!(
        code(ScopedReadSession::open_expected(
            &replacement.path,
            &catalog,
            &incarnation
        )),
        ScopeErrorCode::WrongAuthority
    );
}

#[test]
fn retained_selection_reference_checks_exact_profile_revision_and_digest() {
    let f = Fixture::new();
    let saved = f.selection();
    validate_selection_reference(&f.writer, "default", &saved).unwrap();
    assert_eq!(
        code(validate_selection_reference(
            &f.writer,
            "other-profile",
            &saved
        )),
        ScopeErrorCode::SelectionChanged
    );
    let wrong_revision =
        SavedQueryRef::new(saved.id.clone(), saved.revision + 1, saved.digest.clone()).unwrap();
    assert_eq!(
        code(validate_selection_reference(
            &f.writer,
            "default",
            &wrong_revision
        )),
        ScopeErrorCode::SelectionChanged
    );
    let wrong_digest =
        SavedQueryRef::new(saved.id.clone(), saved.revision, "0".repeat(64)).unwrap();
    assert_eq!(
        code(validate_selection_reference(
            &f.writer,
            "default",
            &wrong_digest
        )),
        ScopeErrorCode::SelectionChanged
    );
    // Validation is a read-only freshness check, not execution or authority.
    assert_eq!(
        f.writer
            .db
            .query_row("SELECT count(*) FROM items", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
}
