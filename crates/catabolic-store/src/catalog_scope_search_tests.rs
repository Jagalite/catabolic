use super::*;
use crate::catalog_schema37_test_support as schema37_fixture;
use crate::{
    catalog_commands as commands, catalog_search_index as index, curation, definitions, migration,
};
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Fixture {
    _root: tempfile::TempDir,
    path: PathBuf,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        Self::at(35)
    }
    fn at(schema: u32) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("search.db");
        migration::initialize_native_at(&path, schema).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        Self {
            _root: root,
            path,
            store,
        }
    }
    fn upgrade37(self) -> Self {
        let Self { _root, path, store } = self;
        drop(store);
        schema37_fixture::upgrade(&path);
        let store = Store::open(&path, true, false).unwrap();
        Self { _root, path, store }
    }
    fn put(&self, id: &str, title: &str, description: &str) {
        curation::item_put(
            &self.store,
            &json!({"id":id,"kind":"movie","metadata":{"title":title,"description":description}}),
        )
        .unwrap();
    }
    fn ready(&self) {
        assert!(
            index::rebuild(&self.store, index::SearchIndexLimits::default(), None)
                .unwrap()
                .ready
        );
    }
    fn selection(
        &self,
        name: &str,
        sql: &str,
        metadata: &[&str],
        identities: &[&str],
        tags: bool,
        aliases: bool,
    ) -> TrustedSelection {
        let row = definitions::query_put(
            &self.store,
            "default",
            name,
            &json!({"version":1,"mode":"selection","selection":{"language":"sql","query":sql}}),
        )
        .unwrap();
        TrustedSelection::new(
            SavedQueryRef::new(
                row["id"].as_str().unwrap().into(),
                row["revision"].as_i64().unwrap(),
                row["digest"].as_str().unwrap().into(),
            )
            .unwrap(),
            metadata.iter().map(|s| s.to_string()).collect(),
        )
        .unwrap()
        .with_search_fields(
            TrustedSearchFields::new(
                identities.iter().map(|s| s.to_string()).collect(),
                tags,
                aliases,
            )
            .unwrap(),
        )
    }
    fn admit(
        &self,
        s: &mut ScopedReadSession,
        h: &HostAuthorization,
        selections: Vec<TrustedSelection>,
    ) -> ScopeHandle {
        s.admit_host_scope(
            TrustedHostReadGrant::new(h.clone(), "default".into(), selections, vec![]).unwrap(),
            &lease(h),
            budget(),
        )
        .unwrap()
    }
    fn envelope(&self, key: &str, payload: Value) -> CommandEnvelope {
        let generation = self
            .store
            .db
            .query_row("SELECT generation FROM catalog_state", [], |r| r.get(0))
            .unwrap();
        CommandEnvelope::new(
            1,
            CommandKey::new(&self.store.database_id, "search-fixture", key).unwrap(),
            self.store.catalog_incarnation.clone().unwrap(),
            generation,
            &payload.to_string(),
        )
        .unwrap()
    }
    fn pin(&self, id: &str, namespace: &str, value: &str) {
        use crate::catalog_pins::*;
        let grant = PinAuthority {
            host_namespace: "search-fixture".into(),
            readable: Default::default(),
            mutable: [(namespace.into(), [id.into()].into())].into(),
        };
        let batch = PinBatch {
            contract_version: 1,
            changes: vec![PinChange {
                item_id: id.into(),
                namespace: namespace.into(),
                expected_revision: 0,
                decision: PinDecision::Set {
                    value: value.into(),
                },
            }],
        };
        let preview = preview_pins(&self.store, &batch, &grant).unwrap();
        commands::execute_with_authority(&self.store,&self.envelope(&format!("pin-{id}-{namespace}"),json!({"operation":"pins","mutation":PinMutation{batch,preview_digest:preview.digest}})),commands::CatalogAuthority::new(base()).with_pins(&grant),policy()).unwrap();
    }
    fn tag(&self, id: &str, name: &str) {
        crate::tagging::put(&self.store, &json!(name), None).unwrap();
        crate::tagging::assign(
            &self.store,
            crate::tagging::Assignment {
                names: &[json!(name)],
                items: &[id.into()],
                files: &[],
                source: &json!("provider"),
                confidence: &json!(0.5),
                note: &json!("fixture"),
                remove: false,
            },
        )
        .unwrap();
    }
    fn logical(&self, source: &str, target: &str, split: bool) {
        use crate::catalog_logical_edits::*;
        let grant = LogicalEditAuthority {
            host_namespace: "search-fixture".into(),
            item_ids: [source.into(), target.into(), "child".into()].into(),
            new_item_ids: if split {
                [target.into(), "child".into()].into()
            } else {
                Default::default()
            },
        };
        let request = LogicalEditRequest {
            contract_version: 1,
            change: if split {
                LogicalChange::Split {
                    source: source.into(),
                    children: vec![target.into(), "child".into()],
                }
            } else {
                LogicalChange::Merge {
                    sources: vec![source.into()],
                    survivor: target.into(),
                }
            },
            new_items: if split {
                [target, "child"]
                    .into_iter()
                    .map(|id| NewItem {
                        id: id.into(),
                        kind: "movie".into(),
                    })
                    .collect()
            } else {
                vec![]
            },
            association_clones: vec![],
            contexts: vec![],
        };
        let preview = preview_logical_edit(&self.store, request, &grant, None).unwrap();
        commands::execute_with_authority(
            &self.store,
            &self.envelope(
                &format!("logical-{source}"),
                json!({"operation":"logical_edit","reviewed":preview.reviewed}),
            ),
            commands::CatalogAuthority::new(base()).with_logical_edits(&grant),
            policy(),
        )
        .unwrap();
    }
}
fn base() -> commands::TrustedCommandAuthority<'static> {
    commands::TrustedCommandAuthority {
        host_namespace: "search-fixture",
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: true,
        identity_authority: None,
    }
}
fn policy() -> commands::ReceiptPolicy {
    commands::ReceiptPolicy {
        retention: Duration::from_secs(3600),
        max_receipts: 1000,
        max_logical_bytes: 16 * 1024 * 1024,
        max_hold_records: 1000,
    }
}
fn host() -> HostAuthorization {
    HostAuthorization::new("search-fixture".into(), "v1".into()).unwrap()
}
fn lease(h: &HostAuthorization) -> AuthorizationLease {
    h.acquire("v1").unwrap()
}
fn budget() -> ReadBudget {
    ReadBudget::new(Duration::from_secs(5), 1000).unwrap()
}
fn request(query: &str, cursor: Option<String>, limit: usize) -> ScopedSearchRequest {
    ScopedSearchRequest {
        query: query.into(),
        cursor,
        limit,
    }
}
fn search(
    s: &mut ScopedReadSession,
    cap: &ScopeHandle,
    h: &HostAuthorization,
    query: &str,
) -> Result<Value> {
    s.search_host_scope(cap, lease(h), request(query, None, 100), budget())
        .map(|r| r.deliver(Clone::clone))
}
fn ids(result: &Value) -> Vec<String> {
    result["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["item_id"].as_str().unwrap().into())
        .collect()
}
fn code<T>(result: Result<T>) -> ScopeErrorCode {
    match result {
        Ok(_) => panic!("unexpected success"),
        Err(e) => e.code,
    }
}
#[test]
fn readiness_and_schema_are_explicit_not_empty_success() {
    let f = Fixture::new();
    f.put("a", "alpha", "");
    let selection = f.selection(
        "all",
        "SELECT item_id FROM catalog_items",
        &["title"],
        &[],
        false,
        false,
    );
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = f.admit(&mut s, &h, vec![selection.clone()]);
    assert_eq!(
        code(search(&mut s, &cap, &h, "alpha")),
        ScopeErrorCode::SearchNotReady
    );
    assert!(s.store.as_ref().unwrap().db.is_autocommit());
    f.ready();
    assert_eq!(
        code(search(&mut s, &cap, &h, "alpha")),
        ScopeErrorCode::StaleScope
    );
    let fresh = f.admit(&mut s, &h, vec![selection]);
    assert_eq!(ids(&search(&mut s, &fresh, &h, "alpha").unwrap()), ["a"]);
}
#[test]
fn real_fts_tokens_phrases_prefix_unicode_and_invalid_syntax() {
    let f = Fixture::new();
    f.put("a", "Café alpha beta", "");
    f.put("b", "alphabet soup", "");
    let sel = f.selection(
        "all",
        "SELECT item_id FROM catalog_items",
        &["title"],
        &[],
        false,
        false,
    );
    f.ready();
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = f.admit(&mut s, &h, vec![sel]);
    for (q, want) in [
        ("alpha", vec!["a"]),
        ("alph*", vec!["a", "b"]),
        ("\"alpha beta\"", vec!["a"]),
        ("cafe", vec!["a"]),
        ("pha", vec![]),
    ] {
        assert_eq!(ids(&search(&mut s, &cap, &h, q).unwrap()), want);
    }
    for q in ["\"unterminated", "alpha AND", "unknowncolumn:alpha"] {
        assert_eq!(
            code(search(&mut s, &cap, &h, q)),
            ScopeErrorCode::QueryRejected
        );
        assert!(s.store.as_ref().unwrap().db.is_autocommit());
    }
    assert!(search(&mut s, &cap, &h, "alpha").is_ok());
}
#[test]
fn denied_fields_are_removed_before_negative_match_and_column_filters() {
    let f = Fixture::new();
    f.put("a", "alpha", "secret");
    f.put("b", "alpha secret", "");
    f.put("hidden", "alpha", "");
    let sel = f.selection(
        "visible",
        "SELECT item_id FROM catalog_items WHERE item_id IN ('a','b')",
        &["title"],
        &[],
        false,
        false,
    );
    f.ready();
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = f.admit(&mut s, &h, vec![sel]);
    assert_eq!(
        ids(&search(&mut s, &cap, &h, "alpha NOT secret").unwrap()),
        ["a"]
    );
    assert_eq!(
        search(&mut s, &cap, &h, "alpha").unwrap()["scoped_total"],
        2
    );
    assert!(ids(&search(&mut s, &cap, &h, "description:secret").unwrap()).is_empty());
}
#[test]
fn masks_are_per_item_and_metadata_cannot_authorize_identity_or_tags() {
    let f = Fixture::new();
    f.put("a", "alpha", "hiddenword");
    f.put("b", "hiddenword", "alpha");
    f.pin("a", "provider", "identityword");
    f.tag("a", "tagword");
    let a = f.selection(
        "a",
        "SELECT item_id FROM catalog_items WHERE item_id='a'",
        &["title"],
        &[],
        false,
        false,
    );
    let b = f.selection(
        "b",
        "SELECT item_id FROM catalog_items WHERE item_id='b'",
        &["description"],
        &[],
        false,
        false,
    );
    f.ready();
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = f.admit(&mut s, &h, vec![a, b]);
    for q in ["hiddenword", "identityword", "tagword"] {
        assert!(ids(&search(&mut s, &cap, &h, q).unwrap()).is_empty());
    }
    let result = search(&mut s, &cap, &h, "alpha").unwrap();
    assert_eq!(ids(&result), ["a", "b"]);
    assert_eq!(result["matches"][0]["score"], 100);
    assert_eq!(result["matches"][1]["score"], 10);
}
#[test]
fn rank_uses_boolean_real_cross_field_hits_and_fixed_weights() {
    let f = Fixture::new();
    f.put("a", "alpha", "");
    f.put("b", "", "");
    f.put("c", "", "alpha");
    f.put("d", "alpha alpha alpha", "");
    f.pin("b", "provider", "alpha");
    f.tag("a", "beta");
    let all = f.selection(
        "all",
        "SELECT item_id FROM catalog_items",
        &["title", "description"],
        &["provider"],
        true,
        false,
    );
    f.ready();
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = f.admit(&mut s, &h, vec![all]);
    let result = search(&mut s, &cap, &h, "alpha").unwrap();
    assert_eq!(ids(&result), ["b", "a", "d", "c"]);
    assert_eq!(result["matches"][0]["score"], 120);
    let and = search(&mut s, &cap, &h, "alpha AND beta").unwrap();
    assert_eq!(ids(&and), ["a"]);
    assert_eq!(and["matches"][0]["score"], 140);
}
#[test]
fn hidden_documents_and_hidden_text_cannot_change_visible_order_or_totals() {
    let f = Fixture::new();
    f.put("a", "alpha", "");
    f.put("b", "alpha", "");
    f.put("hidden", "beta", "");
    let sel = f.selection(
        "visible",
        "SELECT item_id FROM catalog_items WHERE item_id IN ('a','b')",
        &["title"],
        &[],
        false,
        false,
    );
    f.ready();
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = f.admit(&mut s, &h, vec![sel.clone()]);
    let before = search(&mut s, &cap, &h, "alpha").unwrap();
    f.put("hidden", &"alpha ".repeat(3000), "");
    f.put("a", "alpha", &"alpha secret ".repeat(2000));
    assert_eq!(
        code(search(&mut s, &cap, &h, "alpha")),
        ScopeErrorCode::StaleScope
    );
    let cap = f.admit(&mut s, &h, vec![sel]);
    let after = search(&mut s, &cap, &h, "alpha").unwrap();
    assert_eq!(before["matches"], after["matches"]);
    assert_eq!(before["scoped_total"], after["scoped_total"]);
}
#[test]
fn multi_hop_aliases_require_every_original_intermediate_destination() {
    let f = Fixture::new();
    for id in ["oldword", "middleword", "target", "child"] {
        f.put(id, "unrelated", "");
    }
    f.logical("oldword", "middleword", false);
    f.logical("middleword", "target", false);
    let all = f.selection(
        "all",
        "SELECT item_id FROM catalog_items",
        &[],
        &[],
        false,
        true,
    );
    let missing_middle = f.selection(
        "missing-middle",
        "SELECT item_id FROM catalog_items WHERE item_id!='middleword'",
        &[],
        &[],
        false,
        true,
    );
    let missing_old = f.selection(
        "missing-old",
        "SELECT item_id FROM catalog_items WHERE item_id!='oldword'",
        &[],
        &[],
        false,
        true,
    );
    let missing_target = f.selection(
        "missing-target",
        "SELECT item_id FROM catalog_items WHERE item_id!='target'",
        &[],
        &[],
        false,
        true,
    );
    let metadata_only = f.selection(
        "metadata",
        "SELECT item_id FROM catalog_items",
        &["title"],
        &[],
        false,
        false,
    );
    f.ready();
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    for selection in [missing_middle, missing_old, missing_target, metadata_only] {
        let cap = f.admit(&mut s, &h, vec![selection]);
        assert!(ids(&search(&mut s, &cap, &h, "oldword").unwrap()).is_empty());
    }
    let cap = f.admit(&mut s, &h, vec![all]);
    let result = search(&mut s, &cap, &h, "oldword").unwrap();
    assert_eq!(ids(&result), ["target"]);
    assert_eq!(result["matches"][0]["score"], 80);
}
#[test]
fn split_never_redirects_alias_search_to_an_arbitrary_child() {
    let f = Fixture::new();
    f.put("oldword", "unrelated", "");
    f.logical("oldword", "target", true);
    let sel = f.selection(
        "all",
        "SELECT item_id FROM catalog_items",
        &[],
        &[],
        false,
        true,
    );
    f.ready();
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = f.admit(&mut s, &h, vec![sel]);
    assert!(ids(&search(&mut s, &cap, &h, "oldword").unwrap()).is_empty());
}
#[test]
fn search_cursors_bind_query_limit_session_scope_and_rank_position() {
    let f = Fixture::new();
    for id in ["a", "b", "c"] {
        f.put(id, "alpha", "");
    }
    let sel = f.selection(
        "all",
        "SELECT item_id FROM catalog_items",
        &["title"],
        &[],
        false,
        false,
    );
    f.ready();
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = f.admit(&mut s, &h, vec![sel.clone()]);
    let first = s
        .search_host_scope(&cap, lease(&h), request("alpha", None, 1), budget())
        .unwrap()
        .deliver(Clone::clone);
    assert_eq!(ids(&first), ["a"]);
    assert_eq!(first["scoped_total"], 3);
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();
    let second = s
        .search_host_scope(
            &cap,
            lease(&h),
            request("alpha", Some(cursor.clone()), 1),
            budget(),
        )
        .unwrap()
        .deliver(Clone::clone);
    assert_eq!(ids(&second), ["b"]);
    for (q, limit) in [("alpha*", 1), ("alpha", 2)] {
        assert_eq!(
            code(s.search_host_scope(
                &cap,
                lease(&h),
                request(q, Some(cursor.clone()), limit),
                budget()
            )),
            ScopeErrorCode::InvalidCursor
        );
    }
    let fresh = f.admit(&mut s, &h, vec![sel]);
    assert_eq!(
        code(s.search_host_scope(
            &fresh,
            lease(&h),
            request("alpha", Some(cursor.clone()), 1),
            budget()
        )),
        ScopeErrorCode::InvalidCursor
    );
    let mut other = ScopedReadSession::open(&f.path).unwrap();
    assert_eq!(
        code(other.search_host_scope(&cap, lease(&h), request("alpha", Some(cursor), 1), budget())),
        ScopeErrorCode::WrongSession
    );
}
#[test]
fn cancelled_fts_vm_cleans_up_and_refuses_partial_results() {
    let f = Fixture::new();
    for n in 0..50 {
        f.put(&format!("item{n}"), "alpha beta", "");
    }
    let sel = f.selection(
        "all",
        "SELECT item_id FROM catalog_items",
        &["title"],
        &[],
        false,
        false,
    );
    f.ready();
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = f.admit(&mut s, &h, vec![sel]);
    let calls = AtomicUsize::new(0);
    let cancelled = crate::embedding::with_interrupt_check(
        move || calls.fetch_add(1, Ordering::Relaxed) > 500,
        || Ok(search(&mut s, &cap, &h, "alpha")),
    )
    .unwrap();
    assert_eq!(code(cancelled), ScopeErrorCode::Cancelled);
    assert!(s.store.as_ref().unwrap().db.is_autocommit());
    assert_eq!(
        search(&mut s, &cap, &h, "alpha").unwrap()["scoped_total"],
        50
    );
    assert_eq!(
        s.store
            .as_ref()
            .unwrap()
            .db
            .query_row(
                "SELECT count(*) FROM temp.sqlite_schema WHERE name LIKE 'native_scope_search%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}
#[test]
fn owned_search_vm_budget_is_typed_and_rollback_has_no_active_hook() {
    let f = Fixture::new();
    f.put("a", "alpha", "");
    let sel = f.selection(
        "all",
        "SELECT item_id FROM catalog_items",
        &["title"],
        &[],
        false,
        false,
    );
    f.ready();
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = f.admit(&mut s, &h, vec![sel]);
    let work = OperationBudget::new(
        Duration::from_secs(5),
        BudgetLimits {
            items: 10000,
            values: 10000,
            bytes: MAX_BYTES,
            work: 0,
        },
    )
    .unwrap();
    let active = work.enter().unwrap();
    let prepared = prepare(&cap.0, request("alpha", None, 1));
    assert_eq!(code(prepared), ScopeErrorCode::WorkBudget);
    drop(active);
    let work = OperationBudget::new(
        Duration::from_secs(5),
        BudgetLimits {
            items: 10000,
            values: 10000,
            bytes: MAX_BYTES,
            work: 500,
        },
    )
    .unwrap();
    let active = work.enter().unwrap();
    let poisoned = Cell::new(false);
    let result = s.fresh(|slot, _| {
        execute(
            slot.as_ref().unwrap(),
            &cap.0,
            &cap.0.members,
            prepare(&cap.0, request("alpha", None, 1))?,
            Instant::now(),
            budget(),
            &poisoned,
        )
    });
    assert_eq!(code(result), ScopeErrorCode::WorkBudget);
    drop(active);
    assert!(!poisoned.get());
    assert!(search(&mut s, &cap, &h, "alpha").is_ok());
}
#[test]
fn revoked_host_lease_prevents_next_page_and_delivery_holds_drain() {
    let f = Fixture::new();
    f.put("a", "alpha", "");
    let sel = f.selection(
        "all",
        "SELECT item_id FROM catalog_items",
        &["title"],
        &[],
        false,
        false,
    );
    f.ready();
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = f.admit(&mut s, &h, vec![sel]);
    let result = s
        .search_host_scope(&cap, lease(&h), request("alpha", None, 1), budget())
        .unwrap();
    let drain = h.begin_revocation().unwrap();
    assert!(drain.wait_drained(Duration::ZERO).is_err());
    result.deliver(|value| {
        assert_eq!(ids(value), ["a"]);
        assert!(drain.wait_drained(Duration::ZERO).is_err());
    });
    assert!(drain.wait_drained(Duration::ZERO).is_ok());
    assert!(h.acquire("v1").is_err());
}

#[test]
fn schema37_real_upgrade_preserves_fts_field_authority_rank_cursor_and_invalidation() {
    let f = Fixture::at(36);
    f.put("a", "Café alpha", "secret");
    f.put("b", "alpha secret", "");
    f.put("c", "", "alpha");
    f.put("hidden", "alpha", "");
    f.pin("b", "provider", "alpha");
    f.tag("a", "beta");
    let titles = f.selection(
        "titles36",
        "SELECT item_id FROM catalog_items WHERE item_id IN ('a','b')",
        &["title"],
        &["provider"],
        true,
        false,
    );
    let description = f.selection(
        "description36",
        "SELECT item_id FROM catalog_items WHERE item_id='c'",
        &["description"],
        &[],
        false,
        false,
    );
    f.ready();
    let f = f.upgrade37();
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let selections = vec![titles, description];
    let cap = f.admit(&mut s, &h, selections.clone());
    let matches = search(&mut s, &cap, &h, "alpha").unwrap();
    assert_eq!(ids(&matches), ["b", "a", "c"]);
    assert_eq!(matches["scoped_total"], 3);
    assert_eq!(matches["matches"][0]["score"], 220);
    assert_eq!(matches["matches"][1]["score"], 100);
    assert_eq!(matches["matches"][2]["score"], 10);
    assert_eq!(
        ids(&search(&mut s, &cap, &h, "alpha NOT secret").unwrap()),
        ["a", "c"]
    );
    assert!(ids(&search(&mut s, &cap, &h, "description:secret").unwrap()).is_empty());
    assert_eq!(ids(&search(&mut s, &cap, &h, "cafe").unwrap()), ["a"]);
    let across_fields = search(&mut s, &cap, &h, "alpha AND beta").unwrap();
    assert_eq!(ids(&across_fields), ["a"]);
    assert_eq!(across_fields["matches"][0]["score"], 140);
    let first = s
        .search_host_scope(&cap, lease(&h), request("alpha", None, 1), budget())
        .unwrap()
        .deliver(Clone::clone);
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();
    let second = s
        .search_host_scope(
            &cap,
            lease(&h),
            request("alpha", Some(cursor.clone()), 1),
            budget(),
        )
        .unwrap()
        .deliver(Clone::clone);
    assert_eq!(ids(&second), ["a"]);
    assert_eq!(
        code(s.search_host_scope(
            &cap,
            lease(&h),
            request("alpha*", Some(cursor), 1),
            budget()
        )),
        ScopeErrorCode::InvalidCursor
    );
    f.put("hidden", &"alpha ".repeat(1000), "");
    assert_eq!(
        code(search(&mut s, &cap, &h, "alpha")),
        ScopeErrorCode::StaleScope
    );
    let fresh = f.admit(&mut s, &h, selections);
    let after = search(&mut s, &fresh, &h, "alpha").unwrap();
    assert_eq!(matches["matches"], after["matches"]);
    assert_eq!(matches["scoped_total"], after["scoped_total"]);
}

#[test]
fn schema37_real_upgrade_preserves_alias_path_authority_and_cancelled_vm_cleanup() {
    let f = Fixture::at(36);
    for id in ["oldword", "middleword", "target"] {
        f.put(id, "alpha", "");
    }
    f.logical("oldword", "middleword", false);
    f.logical("middleword", "target", false);
    let all = f.selection(
        "all36",
        "SELECT item_id FROM catalog_items",
        &["title"],
        &[],
        false,
        true,
    );
    let hidden_middle = f.selection(
        "hidden-middle36",
        "SELECT item_id FROM catalog_items WHERE item_id!='middleword'",
        &["title"],
        &[],
        false,
        true,
    );
    f.ready();
    let f = f.upgrade37();
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let limited = f.admit(&mut s, &h, vec![hidden_middle]);
    assert!(ids(&search(&mut s, &limited, &h, "oldword").unwrap()).is_empty());
    let cap = f.admit(&mut s, &h, vec![all]);
    let out = search(&mut s, &cap, &h, "oldword").unwrap();
    assert_eq!(ids(&out), ["target"]);
    assert_eq!(out["matches"][0]["score"], 80);
    let calls = AtomicUsize::new(0);
    let cancelled = crate::embedding::with_interrupt_check(
        move || calls.fetch_add(1, Ordering::Relaxed) > 500,
        || Ok(search(&mut s, &cap, &h, "alpha")),
    )
    .unwrap();
    assert_eq!(code(cancelled), ScopeErrorCode::Cancelled);
    assert!(s.store.as_ref().unwrap().db.is_autocommit());
    assert!(search(&mut s, &cap, &h, "oldword").is_ok());
    assert_eq!(
        s.store
            .as_ref()
            .unwrap()
            .db
            .query_row(
                "SELECT count(*) FROM temp.sqlite_schema WHERE name LIKE 'native_scope_search%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}
