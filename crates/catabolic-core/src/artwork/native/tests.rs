use super::*;
use serde_json::{Value, json};

fn asset(id: &str) -> AssetReference {
    AssetReference {
        asset_id: id.into(),
        mime: "image/png".into(),
        width: 1,
        height: 1,
        size_bytes: 3,
        content_sha256: "a".repeat(64),
        owner: ASSET_OWNER.into(),
    }
}
fn wire() -> Value {
    json!({
        "contract_version":1,"item_id":"work",
        "contributions":[{"role":"poster","source":"provider","asset_id":"asset","revision":4}],
        "selections":[{"role":"poster","asset_id":null,"revision":6}],
        "associated_asset_ids":["asset"],"assets":[asset("asset")],"semantics":SEMANTICS,
    })
}
fn parse(value: &Value) -> Result<ReferenceState> {
    ReferenceState::parse_json(&value.to_string(), "work")
}

#[test]
fn imported_contract_one_round_trips_exact_fields_and_opaque_asset_identity() {
    let input = wire();
    let state = parse(&input).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&state.to_json().unwrap()).unwrap(),
        input
    );
    assert_eq!(
        state.asset_reference("asset").unwrap().content_sha256,
        "a".repeat(64)
    );
    assert_ne!(state.assets()[0].asset_id, state.assets()[0].content_sha256);
    assert_eq!(state.selections()[0].asset_id, None);
    assert_eq!(state.selections()[0].revision, 6);
    assert_eq!(state.item_id(), "work");
}

#[test]
fn null_selection_preserves_executable_automatic_fallback_not_literal_suppression() {
    let state = parse(&wire()).unwrap();
    let resolved = resolve_role(&state, "poster").unwrap();
    assert_eq!(resolved.asset_id.as_deref(), Some("asset"));
    assert_eq!(resolved.selection_revision, 6);
    assert!(!resolved.pinned);
    assert!(!resolved.conflict);
    assert_eq!(resolved.sources, vec!["provider"]);
    assert_eq!(
        serde_json::to_value(&state).unwrap()["semantics"],
        SEMANTICS
    );
}

#[test]
fn duplicate_unknown_missing_and_cross_item_wire_data_are_rejected() {
    let raw = wire().to_string();
    let duplicate = raw.replace(
        "\"item_id\":\"work\"",
        "\"item_id\":\"work\",\"item_id\":\"work\"",
    );
    assert_eq!(
        ReferenceState::parse_json(&duplicate, "work"),
        Err(ReferenceError::InvalidJson)
    );
    let duplicate = raw.replace(
        "\"owner\":\"motion_asset_service\"",
        "\"owner\":\"motion_asset_service\",\"owner\":\"motion_asset_service\"",
    );
    assert_eq!(
        ReferenceState::parse_json(&duplicate, "work"),
        Err(ReferenceError::InvalidJson)
    );
    let mut value = wire();
    value["byte_authority"] = json!(true);
    assert_eq!(parse(&value), Err(ReferenceError::InvalidJson));
    let mut value = wire();
    value["assets"][0]["path"] = json!("/private/image");
    assert_eq!(parse(&value), Err(ReferenceError::InvalidJson));
    let mut value = wire();
    value["selections"][0]
        .as_object_mut()
        .unwrap()
        .remove("asset_id");
    assert_eq!(parse(&value), Err(ReferenceError::InvalidJson));
    let mut value = wire();
    value
        .as_object_mut()
        .unwrap()
        .remove("associated_asset_ids");
    assert_eq!(parse(&value), Err(ReferenceError::InvalidJson));
    assert_eq!(
        ReferenceState::parse_json(&raw, "another"),
        Err(ReferenceError::WrongItem)
    );
    let mut value = wire();
    value["contract_version"] = json!(2);
    assert_eq!(parse(&value), Err(ReferenceError::UnsupportedContract));
}

#[test]
fn provenance_digest_and_descriptor_bounds_are_explicit() {
    for owner in ["", "native_asset_service", "motion_asset_service "] {
        let mut value = wire();
        value["assets"][0]["owner"] = json!(owner);
        assert_eq!(parse(&value), Err(ReferenceError::InvalidProvenance));
    }
    let mut value = wire();
    value["semantics"] = json!("bytes are authorized");
    assert_eq!(parse(&value), Err(ReferenceError::InvalidProvenance));
    for digest in [
        "a".repeat(63),
        "a".repeat(65),
        "G".repeat(64),
        "A".repeat(64),
    ] {
        let mut value = wire();
        value["assets"][0]["content_sha256"] = json!(digest);
        assert_eq!(parse(&value), Err(ReferenceError::InvalidDigest));
    }
    for (field, bad) in [
        ("width", json!(0)),
        ("height", json!(-1)),
        ("width", json!(8193)),
        ("height", json!(8193)),
        ("size_bytes", json!(-1)),
        ("size_bytes", json!(MAX_ASSET_BYTES + 1)),
        ("mime", json!("image/gif")),
    ] {
        let mut value = wire();
        value["assets"][0][field] = bad;
        assert_eq!(parse(&value), Err(ReferenceError::InvalidAsset));
    }
    let mut value = wire();
    value["assets"][0]["width"] = json!(MAX_DIMENSION);
    value["assets"][0]["height"] = json!(MAX_DIMENSION);
    value["assets"][0]["size_bytes"] = json!(MAX_ASSET_BYTES);
    parse(&value).unwrap();
    value["assets"][0]["size_bytes"] = json!(0);
    parse(&value).unwrap(); // Exact exporter reference range, not decode authority.
}

#[test]
fn ownership_closure_is_exact_and_each_identity_key_is_unique() {
    let mut missing = wire();
    missing["assets"] = json!([]);
    assert_eq!(parse(&missing), Err(ReferenceError::IncompleteAssetClosure));
    let mut extra = wire();
    extra["assets"]
        .as_array_mut()
        .unwrap()
        .push(json!(asset("extra")));
    assert_eq!(parse(&extra), Err(ReferenceError::IncompleteAssetClosure));
    for collection in [
        "contributions",
        "selections",
        "associated_asset_ids",
        "assets",
    ] {
        let mut duplicate = wire();
        let first = duplicate[collection][0].clone();
        duplicate[collection].as_array_mut().unwrap().push(first);
        assert_eq!(parse(&duplicate), Err(ReferenceError::DuplicateReference));
    }
    let mut agreeing = wire();
    agreeing["contributions"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"poster","source":"other","asset_id":"asset","revision":1}));
    let resolved = resolve_role(&parse(&agreeing).unwrap(), "poster").unwrap();
    assert_eq!(resolved.asset_id.as_deref(), Some("asset"));
    assert_eq!(resolved.sources, vec!["other", "provider"]);
    assert!(!resolved.conflict);
}

#[test]
fn historical_selection_only_reference_survives_refresh_but_is_not_a_new_candidate() {
    let mut value = wire();
    value["contributions"] = json!([]);
    value["associated_asset_ids"] = json!([]);
    value["selections"][0]["asset_id"] = json!("asset");
    let state = parse(&value).unwrap();
    let resolved = resolve_role(&state, "poster").unwrap();
    assert_eq!(resolved.asset_id.as_deref(), Some("asset"));
    assert!(resolved.pinned);
    assert!(resolved.sources.is_empty());
    assert_eq!(
        plan_select(&state, "poster", 6, Some("asset")),
        Err(ReferenceError::InvalidSelection)
    );
    let cleared = plan_select(&state, "poster", 6, None).unwrap();
    assert_eq!(cleared.next_role_revision, 7);
    assert!(cleared.state.assets().is_empty());
    assert_eq!(cleared.state.selections()[0].asset_id, None);
    assert_eq!(cleared.resolved.asset_id, None);
    assert!(state.asset_reference("asset").is_some());
}

#[test]
fn existing_resolver_controls_conflict_local_override_and_explicit_pin() {
    let mut value = wire();
    value["assets"] = json!([asset("asset"), asset("other"), asset("local")]);
    value["contributions"] = json!([
        {"role":"poster","source":"z","asset_id":"asset","revision":1},
        {"role":"poster","source":"a","asset_id":"other","revision":1},
    ]);
    value["associated_asset_ids"] = json!(["local"]);
    let state = parse(&value).unwrap();
    let resolved = resolve_role(&state, "poster").unwrap();
    assert_eq!(resolved.asset_id, None);
    assert!(resolved.conflict);
    assert_eq!(resolved.sources, vec!["a", "z"]);
    value["contributions"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"poster","source":"local","asset_id":"local","revision":2}));
    let state = parse(&value).unwrap();
    let resolved = resolve_role(&state, "poster").unwrap();
    assert_eq!(resolved.asset_id.as_deref(), Some("local"));
    assert_eq!(resolved.sources, vec!["local"]);
    assert!(!resolved.pinned);
    let plan = plan_select(&state, "poster", 6, Some("other")).unwrap();
    assert_eq!(plan.resolved.asset_id.as_deref(), Some("other"));
    assert_eq!(plan.resolved.sources, vec!["a"]);
    assert!(plan.resolved.pinned);
    assert!(!plan.resolved.conflict);
}

#[test]
fn upload_association_is_idempotent_and_does_not_invent_selection_or_contribution() {
    let state = ReferenceState::empty("work").unwrap();
    let uploaded = plan_associate_upload(&state, &asset("upload")).unwrap();
    assert_eq!(uploaded.change, AssociationChange::Associated);
    assert_eq!(uploaded.state.associated_asset_ids(), &["upload"]);
    assert!(uploaded.state.contributions().is_empty());
    assert!(uploaded.state.selections().is_empty());
    assert_eq!(
        resolve_role(&uploaded.state, "poster").unwrap().asset_id,
        None
    );
    let raw = uploaded.state.to_json().unwrap();
    let repeated = plan_associate_upload(&uploaded.state, &asset("upload")).unwrap();
    assert_eq!(repeated.change, AssociationChange::Unchanged);
    assert_eq!(repeated.state.to_json().unwrap(), raw);
    let selected = plan_select(&uploaded.state, "banner", 0, Some("upload")).unwrap();
    assert_eq!(selected.next_role_revision, 1);
    assert_eq!(selected.resolved.asset_id.as_deref(), Some("upload"));
    assert!(selected.resolved.sources.is_empty());
    assert!(selected.resolved.pinned);
}

#[test]
fn association_merge_rejects_identity_rebinding_and_preserves_unrelated_arrays() {
    let state = parse(&wire()).unwrap();
    let mut changed = asset("asset");
    changed.width = 2;
    assert_eq!(
        plan_associate_upload(&state, &changed),
        Err(ReferenceError::AssetConflict)
    );
    let merged = plan_associate_upload(&state, &asset("new")).unwrap();
    assert_eq!(merged.state.contributions(), state.contributions());
    assert_eq!(merged.state.selections(), state.selections());
    assert_eq!(state.associated_asset_ids(), &["asset"]);
    let mut only_contributed = wire();
    only_contributed["associated_asset_ids"] = json!([]);
    let state = parse(&only_contributed).unwrap();
    let merged = plan_associate_upload(&state, &asset("asset")).unwrap();
    assert_eq!(merged.change, AssociationChange::Associated);
    assert_eq!(merged.state.assets().len(), 1);
}

#[test]
fn candidate_scope_role_cas_repeated_selection_and_overflow_follow_core_policy() {
    let mut value = wire();
    value["associated_asset_ids"] = json!([]);
    let state = parse(&value).unwrap();
    assert_eq!(
        plan_select(&state, "backdrop", 0, Some("asset")),
        Err(ReferenceError::InvalidSelection)
    );
    assert_eq!(
        plan_select(&state, "poster", 5, Some("asset")),
        Err(ReferenceError::SelectionConflict)
    );
    assert_eq!(
        plan_select(&state, "poster", -1, None),
        Err(ReferenceError::SelectionConflict)
    );
    assert_eq!(
        plan_select(&state, "not-a-role", 0, None),
        Err(ReferenceError::InvalidRole)
    );
    let first = plan_select(&state, "poster", 6, Some("asset")).unwrap();
    let second = plan_select(&first.state, "poster", 7, Some("asset")).unwrap();
    assert_eq!(second.next_role_revision, 8);
    value["selections"][0]["revision"] = json!(i64::MAX);
    assert_eq!(
        plan_select(&parse(&value).unwrap(), "poster", i64::MAX, None),
        Err(ReferenceError::SelectionConflict)
    );
}

#[test]
fn known_role_source_identifier_and_revision_limits_are_strict() {
    let mut value = wire();
    value["contributions"][0]["source"] = json!("a".repeat(MAX_SOURCE_BYTES));
    parse(&value).unwrap();
    value["contributions"][0]["source"] = json!("a".repeat(MAX_SOURCE_BYTES + 1));
    assert_eq!(parse(&value), Err(ReferenceError::InvalidSource));
    for source in ["", "UPPER", "has space", "a_b"] {
        let mut value = wire();
        value["contributions"][0]["source"] = json!(source);
        assert_eq!(parse(&value), Err(ReferenceError::InvalidSource));
    }
    let mut value = wire();
    value["contributions"][0]["role"] = json!("banner");
    assert_eq!(parse(&value), Err(ReferenceError::InvalidRole));
    for field in ["contributions", "selections"] {
        let mut value = wire();
        value[field][0]["revision"] = json!(0);
        assert_eq!(parse(&value), Err(ReferenceError::InvalidRevision));
    }
    ReferenceState::empty(&"é".repeat(MAX_IDENTIFIER_BYTES / 2)).unwrap();
    assert_eq!(
        ReferenceState::empty(&"é".repeat(MAX_IDENTIFIER_BYTES / 2 + 1)),
        Err(ReferenceError::InvalidIdentifier)
    );
}

#[test]
fn raw_byte_depth_and_fixed_shape_node_budgets_fail_without_truncation() {
    let raw = ReferenceState::empty("work").unwrap().to_json().unwrap();
    let exact = format!("{raw}{}", " ".repeat(MAX_REFERENCE_BYTES - raw.len()));
    ReferenceState::parse_json(&exact, "work").unwrap();
    assert_eq!(
        ReferenceState::parse_json(&(exact + " "), "work"),
        Err(ReferenceError::ByteBudget)
    );
    let nested = format!(
        "{}0{}",
        "[".repeat(MAX_REFERENCE_DEPTH + 1),
        "]".repeat(MAX_REFERENCE_DEPTH + 1)
    );
    assert_eq!(
        ReferenceState::parse_json(&nested, "work"),
        Err(ReferenceError::DepthBudget)
    );
    let mut state = ReferenceState::empty("work").unwrap();
    state.0.contributions = vec![
        ContributionReference {
            role: "poster".into(),
            source: "p".into(),
            asset_id: "asset".into(),
            revision: 1
        };
        MAX_REFERENCE_NODES
    ];
    assert_eq!(state.validate("work"), Err(ReferenceError::NodeBudget));
}
