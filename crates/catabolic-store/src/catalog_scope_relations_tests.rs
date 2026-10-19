use super::*;
fn relate(f: &Fixture, source: &str, target: &str, kind: &str, metadata: Value) -> String {
    curation::relate(
        &f.writer,
        &json!({"source":source,"target":target,"kind":kind,"metadata":metadata}),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn selection(
    f: &Fixture,
    name: &str,
    query: &str,
    kinds: &[&str],
    metadata: &[&str],
) -> TrustedSelection {
    TrustedSelection::new(
        f.saved(name, "selection", "sql", query),
        vec!["title".into()],
    )
    .unwrap()
    .with_relationships(
        TrustedRelationshipRead::new(
            kinds.iter().map(|v| v.to_string()).collect(),
            metadata.iter().map(|v| v.to_string()).collect(),
        )
        .unwrap(),
    )
}
fn admit(
    f: &Fixture,
    s: &mut ScopedReadSession,
    h: &HostAuthorization,
    selections: Vec<TrustedSelection>,
    reports: Vec<SavedQueryRef>,
) -> ScopeHandle {
    let _ = f;
    s.admit_host_scope(
        TrustedHostReadGrant::new(h.clone(), "default".into(), selections, reports).unwrap(),
        &lease(h),
        budget(),
    )
    .unwrap()
}
fn related(item: &str, cursor: Option<String>, limit: usize) -> ScopeReadRequest {
    ScopeReadRequest::RelationshipsPage {
        item_id: item.into(),
        cursor,
        limit,
    }
}
#[test]
fn direct_edges_require_both_visible_kind_authorized_endpoints_and_mask_metadata() {
    let f = Fixture::new();
    let edge = relate(
        &f,
        "a",
        "b",
        "derived_from",
        json!({"note":"allowed","secret":"never"}),
    );
    relate(
        &f,
        "a",
        "hidden",
        "derived_from",
        json!({"note":"hidden-target"}),
    );
    relate(
        &f,
        "hidden",
        "b",
        "derived_from",
        json!({"note":"hidden-source"}),
    );
    let selected = selection(
        &f,
        "visible",
        "SELECT item_id FROM catalog_items WHERE item_id IN ('a','b')",
        &["derived_from"],
        &["note"],
    );
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = admit(&f, &mut s, &h, vec![selected], vec![]);
    for item in ["a", "b"] {
        let out = read(&mut s, &cap, &h, related(item, None, 10)).unwrap();
        assert_eq!(out["scoped_total"], 1);
        let row = &out["result"]["rows"][0];
        assert_eq!(row[0], edge);
        assert_eq!(row[1], "a");
        assert_eq!(row[4], "b");
        assert_eq!(row[9], encode(&json!({"note":"allowed"})));
        assert!(!encode(&out).contains("hidden"));
        assert!(!encode(&out).contains("secret"));
    }
    for item in ["hidden", "missing"] {
        assert_eq!(
            code(read(&mut s, &cap, &h, related(item, None, 10))),
            ScopeErrorCode::NotFoundOrNotVisible
        );
    }
}
#[test]
fn metadata_and_search_grants_never_imply_relationship_read_authority() {
    let f = Fixture::new();
    relate(&f, "a", "b", "derived_from", json!({"secret":"never"}));
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let sel = TrustedSelection::new(f.selection(), vec!["title".into()])
        .unwrap()
        .with_search_fields(TrustedSearchFields::new(vec!["provider".into()], true, true).unwrap());
    let cap = admit(&f, &mut s, &h, vec![sel], vec![]);
    assert_eq!(
        code(read(&mut s, &cap, &h, related("a", None, 1))),
        ScopeErrorCode::NotFoundOrNotVisible
    );
    let source = selection(
        &f,
        "source",
        "SELECT item_id FROM catalog_items WHERE item_id='a'",
        &["derived_from"],
        &[],
    );
    let target = TrustedSelection::new(
        f.saved(
            "target",
            "selection",
            "sql",
            "SELECT item_id FROM catalog_items WHERE item_id='b'",
        ),
        vec!["title".into()],
    )
    .unwrap();
    let cap = admit(&f, &mut s, &h, vec![source, target], vec![]);
    let out = read(&mut s, &cap, &h, related("a", None, 1)).unwrap();
    assert_eq!(out["scoped_total"], 0);
    assert_eq!(out["result"]["rows"], json!([]));
    assert_eq!(
        code(read(&mut s, &cap, &h, related("b", None, 1))),
        ScopeErrorCode::NotFoundOrNotVisible
    );
}
#[test]
fn relationship_metadata_is_intersected_per_kind_without_cross_kind_grant_union() {
    let f = Fixture::new();
    relate(
        &f,
        "a",
        "b",
        "derived_from",
        json!({"note":"public","secret":"never"}),
    );
    relate(
        &f,
        "a",
        "b",
        "custom:alternate",
        json!({"note":"never","secret":"private-allowed"}),
    );
    let a = selection(
        &f,
        "a-derived",
        "SELECT item_id FROM catalog_items WHERE item_id='a'",
        &["derived_from"],
        &["note", "secret"],
    );
    let b = selection(
        &f,
        "b-derived",
        "SELECT item_id FROM catalog_items WHERE item_id='b'",
        &["derived_from"],
        &["note"],
    );
    let other = selection(
        &f,
        "alternate",
        "SELECT item_id FROM catalog_items WHERE item_id IN ('a','b')",
        &["custom:alternate"],
        &["secret"],
    );
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = admit(&f, &mut s, &h, vec![a, b, other], vec![]);
    let out = read(&mut s, &cap, &h, related("a", None, 10)).unwrap();
    assert_eq!(out["scoped_total"], 2);
    for row in out["result"]["rows"].as_array().unwrap() {
        let expected = if row[7] == "derived_from" {
            json!({"note":"public"})
        } else {
            json!({"secret":"private-allowed"})
        };
        assert_eq!(row[9], encode(&expected));
    }
}
#[test]
fn approved_relationship_counts_facets_and_recursive_paths_use_only_masked_temp_edges() {
    let f = Fixture::new();
    for id in ["c", "d"] {
        curation::item_put(
            &f.writer,
            &json!({"id":id,"kind":"video","metadata":{"title":id}}),
        )
        .unwrap();
    }
    relate(
        &f,
        "a",
        "b",
        "derived_from",
        json!({"secret":"not-visible"}),
    );
    relate(&f, "b", "c", "derived_from", json!({}));
    relate(&f, "c", "hidden", "derived_from", json!({}));
    relate(&f, "hidden", "d", "derived_from", json!({}));
    let selected = selection(
        &f,
        "graph",
        "SELECT item_id FROM catalog_items WHERE item_id!='hidden'",
        &["derived_from"],
        &[],
    );
    let facets=f.saved("facets","rows","sql","SELECT kind,count(*),json_extract(metadata,'$.secret') FROM catalog_relationships GROUP BY kind,json_extract(metadata,'$.secret')");
    let path=f.saved("paths","rows","sql","WITH RECURSIVE reachable(id) AS (VALUES('a') UNION SELECT r.target_id FROM catalog_relationships r JOIN reachable a ON r.source_id=a.id) SELECT id FROM reachable ORDER BY id");
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = admit(
        &f,
        &mut s,
        &h,
        vec![selected],
        vec![facets.clone(), path.clone()],
    );
    assert_eq!(
        read(
            &mut s,
            &cap,
            &h,
            ScopeReadRequest::SavedReport {
                query_id: facets.id,
                maximum_rows: 10
            }
        )
        .unwrap()["result"]["rows"],
        json!([["derived_from", 2, null]])
    );
    assert_eq!(
        read(
            &mut s,
            &cap,
            &h,
            ScopeReadRequest::SavedReport {
                query_id: path.id,
                maximum_rows: 10
            }
        )
        .unwrap()["result"]["rows"],
        json!([["a"], ["b"], ["c"]])
    );
}
#[test]
fn main_graph_fts_shadow_and_schema_bypasses_remain_denied_with_relation_grants() {
    let f = Fixture::new();
    relate(&f, "a", "b", "derived_from", json!({}));
    let selected = selection(
        &f,
        "visible",
        "SELECT item_id FROM catalog_items WHERE item_id IN ('a','b')",
        &["derived_from"],
        &[],
    );
    let attacks = [
        "SELECT * FROM main.item_relationships",
        "SELECT count(*) FROM main.item_relationships",
        "SELECT r.metadata FROM temp.item_relationships t JOIN main.item_relationships r ON r.id=t.id",
        "SELECT * FROM main.catalog_search_values",
        "SELECT * FROM main.catalog_search_fts_data",
        "SELECT * FROM temp.sqlite_schema",
    ];
    let reports = attacks
        .iter()
        .enumerate()
        .map(|(n, q)| f.saved(&format!("attack{n}"), "rows", "sql", q))
        .collect::<Vec<_>>();
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = admit(&f, &mut s, &h, vec![selected], reports.clone());
    for report in reports {
        assert_eq!(
            code(read(
                &mut s,
                &cap,
                &h,
                ScopeReadRequest::SavedReport {
                    query_id: report.id,
                    maximum_rows: 10
                }
            )),
            ScopeErrorCode::QueryRejected
        );
        assert!(s.store.as_ref().unwrap().db.is_autocommit());
    }
}
#[test]
fn relationship_keyset_cursor_binds_anchor_limit_capability_and_surface() {
    let f = Fixture::new();
    for id in ["c", "d"] {
        curation::item_put(&f.writer, &json!({"id":id,"kind":"video","metadata":{}})).unwrap();
        relate(&f, "a", id, "derived_from", json!({}));
    }
    relate(&f, "a", "b", "derived_from", json!({}));
    let selected = selection(
        &f,
        "graph",
        "SELECT item_id FROM catalog_items WHERE item_id!='hidden'",
        &["derived_from"],
        &[],
    );
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = admit(&f, &mut s, &h, vec![selected.clone()], vec![]);
    let all = read(&mut s, &cap, &h, related("a", None, 10)).unwrap();
    let expected = all["result"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r[0].clone())
        .collect::<Vec<_>>();
    let first = read(&mut s, &cap, &h, related("a", None, 1)).unwrap();
    let token = first["next_cursor"].as_str().unwrap().to_owned();
    let second = read(&mut s, &cap, &h, related("a", Some(token.clone()), 1)).unwrap();
    assert_eq!(first["result"]["rows"][0][0], expected[0]);
    assert_eq!(second["result"]["rows"][0][0], expected[1]);
    assert_eq!(second["scoped_total"], 3);
    for request in [
        related("b", Some(token.clone()), 1),
        related("a", Some(token.clone()), 2),
        ScopeReadRequest::ItemsPage {
            cursor: Some(token.clone()),
            limit: 1,
        },
    ] {
        assert_eq!(
            code(read(&mut s, &cap, &h, request)),
            ScopeErrorCode::InvalidCursor
        );
    }
    let fresh = admit(&f, &mut s, &h, vec![selected], vec![]);
    assert_eq!(
        code(read(&mut s, &fresh, &h, related("a", Some(token), 1))),
        ScopeErrorCode::InvalidCursor
    );
}
#[test]
fn disabled_edges_invalidate_old_scope_and_are_absent_from_new_counts() {
    let f = Fixture::new();
    let edge = relate(&f, "a", "b", "derived_from", json!({}));
    let selected = selection(
        &f,
        "graph",
        "SELECT item_id FROM catalog_items WHERE item_id IN ('a','b')",
        &["derived_from"],
        &[],
    );
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = admit(&f, &mut s, &h, vec![selected.clone()], vec![]);
    assert_eq!(
        read(&mut s, &cap, &h, related("a", None, 1)).unwrap()["scoped_total"],
        1
    );
    curation::dispatch(
        &f.writer,
        "default",
        "relationship-disable",
        &json!({"id":edge}),
    )
    .unwrap();
    assert_eq!(
        code(read(&mut s, &cap, &h, related("a", None, 1))),
        ScopeErrorCode::StaleScope
    );
    let fresh = admit(&f, &mut s, &h, vec![selected], vec![]);
    assert_eq!(
        read(&mut s, &fresh, &h, related("a", None, 1)).unwrap()["scoped_total"],
        0
    );
}
#[test]
fn relationship_masks_validate_vocabulary_and_bound_cloned_field_expansion() {
    assert_eq!(
        code(TrustedRelationshipRead::new(vec![], vec![])),
        ScopeErrorCode::InvalidGrant
    );
    assert_eq!(
        code(TrustedRelationshipRead::new(
            vec!["invented".into()],
            vec![]
        )),
        ScopeErrorCode::InvalidGrant
    );
    assert!(TrustedRelationshipRead::new(vec!["custom:related".into()], vec![]).is_ok());
    let fields = (0..10000)
        .map(|n| format!("{n:05}{}", "x".repeat(251)))
        .collect();
    assert_eq!(
        code(TrustedRelationshipRead::new(
            vec!["derived_from".into(), "part_of".into()],
            fields
        )),
        ScopeErrorCode::ByteBudget
    );
}

#[test]
fn relationship_endpoint_titles_follow_each_endpoints_own_metadata_mask() {
    let f = Fixture::new();
    relate(&f, "a", "b", "derived_from", json!({}));
    let a = selection(
        &f,
        "a",
        "SELECT item_id FROM catalog_items WHERE item_id='a'",
        &["derived_from"],
        &[],
    );
    let b = TrustedSelection::new(
        f.saved(
            "b",
            "selection",
            "sql",
            "SELECT item_id FROM catalog_items WHERE item_id='b'",
        ),
        vec![],
    )
    .unwrap()
    .with_relationships(TrustedRelationshipRead::new(vec!["derived_from".into()], vec![]).unwrap());
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = admit(&f, &mut s, &h, vec![a, b], vec![]);
    let out = read(&mut s, &cap, &h, related("a", None, 10)).unwrap();
    assert_eq!(out["result"]["rows"][0][3], "Alpha");
    assert!(out["result"]["rows"][0][6].is_null());
    assert!(!encode(&out).contains("Beta"));
}
#[test]
fn private_materialization_vm_failure_is_typed_and_temp_graph_is_rolled_back() {
    let f = Fixture::new();
    relate(&f, "a", "b", "derived_from", json!({}));
    let selected = selection(
        &f,
        "graph",
        "SELECT item_id FROM catalog_items WHERE item_id IN ('a','b')",
        &["derived_from"],
        &[],
    );
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = admit(&f, &mut s, &h, vec![selected], vec![]);
    let work = OperationBudget::new(
        Duration::from_secs(5),
        BudgetLimits {
            items: 100,
            values: 1000,
            bytes: MAX_BYTES,
            work: 0,
        },
    )
    .unwrap();
    let active = work.enter().unwrap();
    let poisoned = Cell::new(false);
    let result = s.fresh(|slot, _| {
        materialize(
            slot.as_ref().unwrap(),
            &cap.0.members,
            Instant::now(),
            budget(),
            &poisoned,
        )
    });
    assert_eq!(code(result), ScopeErrorCode::WorkBudget);
    assert_eq!(work.failure(), Some(EvaluationFailure::WorkBudget));
    drop(active);
    assert!(!poisoned.get());
    assert!(s.store.as_ref().unwrap().db.is_autocommit());
    assert_eq!(
        read(&mut s, &cap, &h, related("a", None, 10)).unwrap()["scoped_total"],
        1
    );
}

#[test]
fn schema37_real_upgrade_preserves_endpoint_and_path_authority() {
    let f = Fixture::at(36);
    curation::item_put(
        &f.writer,
        &json!({"id":"c","kind":"video","metadata":{"title":"Gamma"}}),
    )
    .unwrap();
    relate(
        &f,
        "a",
        "b",
        "derived_from",
        json!({"note":"visible","secret":"never"}),
    );
    relate(
        &f,
        "b",
        "hidden",
        "derived_from",
        json!({"note":"hidden-target"}),
    );
    relate(
        &f,
        "hidden",
        "c",
        "derived_from",
        json!({"note":"hidden-source"}),
    );
    let selected = selection(
        &f,
        "visible36",
        "SELECT item_id FROM catalog_items WHERE item_id!='hidden'",
        &["derived_from"],
        &["note"],
    );
    let facets = f.saved("facets36", "rows", "sql", "SELECT kind,count(*),json_extract(metadata,'$.secret') FROM catalog_relationships GROUP BY kind,json_extract(metadata,'$.secret')");
    let paths = f.saved("paths36", "rows", "sql", "WITH RECURSIVE reachable(id) AS (VALUES('a') UNION SELECT r.target_id FROM catalog_relationships r JOIN reachable a ON r.source_id=a.id) SELECT id FROM reachable ORDER BY id");
    let f = f.upgrade37();
    let h = host();
    let mut s = ScopedReadSession::open(&f.path).unwrap();
    let cap = admit(
        &f,
        &mut s,
        &h,
        vec![selected],
        vec![facets.clone(), paths.clone()],
    );
    let out = read(&mut s, &cap, &h, related("a", None, 1)).unwrap();
    assert_eq!(out["scoped_total"], 1);
    assert_eq!(
        out["result"]["rows"][0][9],
        encode(&json!({"note":"visible"}))
    );
    assert!(!encode(&out).contains("hidden"));
    assert_eq!(
        code(read(&mut s, &cap, &h, related("hidden", None, 1))),
        ScopeErrorCode::NotFoundOrNotVisible
    );
    assert_eq!(
        read(&mut s, &cap, &h, related("c", None, 1)).unwrap()["scoped_total"],
        0
    );
    let out = read(
        &mut s,
        &cap,
        &h,
        ScopeReadRequest::SavedReport {
            query_id: facets.id,
            maximum_rows: 10,
        },
    )
    .unwrap();
    assert_eq!(out["result"]["rows"], json!([["derived_from", 1, null]]));
    let out = read(
        &mut s,
        &cap,
        &h,
        ScopeReadRequest::SavedReport {
            query_id: paths.id,
            maximum_rows: 10,
        },
    )
    .unwrap();
    assert_eq!(out["result"]["rows"], json!([["a"], ["b"]]));
}
