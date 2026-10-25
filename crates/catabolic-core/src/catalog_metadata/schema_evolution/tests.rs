// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
use super::*;

fn exact() -> FieldPolicy {
    FieldPolicy {
        value_type: ValueType::Json,
        nullable: true,
        normalization: Normalization::Exact,
    }
}

fn policies(start: usize, count: usize) -> Vec<(String, FieldPolicy)> {
    (start..start + count)
        .map(|index| (format!("field_{index:04}"), exact()))
        .collect()
}

fn fields(start: usize, count: usize) -> String {
    serde_json::to_string(
        &(start..start + count)
            .map(|index| (format!("field_{index:04}"), json!(index)))
            .collect::<BTreeMap<_, _>>(),
    )
    .unwrap()
}

fn key(namespace: &str) -> ContributionKey {
    ContributionKey::new("item:1", namespace, "record:1").unwrap()
}

fn document(schema: &FieldSchema, source: &str, fields: &str) -> ContributionDocument {
    ContributionDocument::from_json(schema, key(source), SourceKind::Provider, 1, fields, "{}")
        .unwrap()
}

#[test]
fn explicit_empty_v2_union_has_no_sentinel_and_does_not_relax_v1() {
    assert_eq!(FieldSchema::new(vec![]), Err(MetadataError::EmptySchema));
    let empty = FieldSchema::new_v2(vec![], vec![]).unwrap();
    assert_eq!(empty.contract_version(), 2);
    assert!(empty.fields().is_empty());
    assert!(empty.system_fields().is_empty());
    let payload = json!({"contract_version":2,"fields":{},"system_fields":[]});
    assert_eq!(empty.v2_payload(), payload);
    assert_eq!(json_nodes(&payload).unwrap(), 4);
    assert_eq!(
        empty.fingerprint(),
        crate::sha256(crate::encode_with(&payload, false, true))
    );
    assert_eq!(
        plan_schema_extension(&empty, vec![], vec![]).unwrap(),
        empty
    );
    let resolution = resolve(&empty, "item:1", &[]).unwrap();
    assert_eq!(resolution.contract_version, 2);
    assert_eq!(resolution.schema_fingerprint, empty.fingerprint());
    assert!(resolution.fields.is_empty());
    assert!(resolution.inputs.is_empty());
    assert_eq!(
        ContributionDocument::from_json(
            &empty,
            key("p"),
            SourceKind::Provider,
            1,
            r#"{"undeclared":1}"#,
            "{}",
        ),
        Err(MetadataError::UnknownField)
    );
}

#[test]
fn empty_v2_documents_and_replacements_keep_exact_source_revision_semantics() {
    let empty = FieldSchema::new_v2(vec![], vec![]).unwrap();
    let request =
        ContributionReplacement::from_json(&empty, key("p"), SourceKind::Provider, 0, "{}", "{}")
            .unwrap();
    let first = plan_replace(&empty, None, &request).unwrap();
    assert_eq!(first.revision(), 1);
    assert!(first.fields().is_empty());
    assert_eq!(revalidate_document(&empty, &empty, &first).unwrap(), first);
    let resolution = resolve(&empty, "item:1", std::slice::from_ref(&first)).unwrap();
    assert!(resolution.fields.is_empty());
    assert_eq!(resolution.inputs, vec![SourceRevision::from(&first)]);
    let request = ContributionReplacement::from_json(
        &empty,
        key("p"),
        SourceKind::Provider,
        1,
        "{}",
        r#"{"observation":"empty replacement"}"#,
    )
    .unwrap();
    let second = plan_replace(&empty, Some(&first), &request).unwrap();
    assert_eq!(second.revision(), 2);
    assert!(second.fields().is_empty());
    assert_eq!(first.revision(), 1);
    let trusted = ContributionDocument::from_json_with_system_fields(
        &empty,
        key("legacy"),
        SourceKind::LegacyImport,
        1,
        "{}",
        "{}",
    )
    .unwrap();
    assert!(trusted.fields().is_empty());
    assert_eq!(trusted.kind(), SourceKind::LegacyImport);
}

#[test]
fn empty_v2_union_extends_to_first_field_with_explicit_system_classification() {
    let empty = FieldSchema::new_v2(vec![], vec![]).unwrap();
    let historical = document(&empty, "p", "{}");
    let historical_bytes = serde_json::to_string(&historical).unwrap();
    let external = plan_schema_extension(
        &empty,
        vec![("motion_named_but_external".into(), exact())],
        vec![],
    )
    .unwrap();
    assert!(external.system_fields().is_empty());
    let in_memory = revalidate_document(&external, &empty, &historical).unwrap();
    assert_eq!(in_memory.revision(), historical.revision());
    assert!(in_memory.fields().is_empty());
    assert_eq!(in_memory.schema_fingerprint(), external.fingerprint());
    let request = ContributionReplacement::from_json(
        &external,
        key("p"),
        SourceKind::Provider,
        1,
        r#"{"motion_named_but_external":42}"#,
        "{}",
    )
    .unwrap();
    let first_field = plan_replace(&external, Some(&in_memory), &request).unwrap();
    assert_eq!(first_field.revision(), 2);
    assert_eq!(first_field.fields()["motion_named_but_external"], 42);
    assert_eq!(
        serde_json::to_string(&historical).unwrap(),
        historical_bytes
    );
    assert_eq!(historical.schema_fingerprint(), empty.fingerprint());
    let system =
        plan_schema_extension(&empty, vec![], vec![("plain_name".into(), exact())]).unwrap();
    assert_eq!(
        system.system_fields(),
        &BTreeSet::from(["plain_name".to_owned()])
    );
    assert_eq!(
        ContributionDocument::from_json(
            &system,
            key("p"),
            SourceKind::Provider,
            1,
            r#"{"plain_name":1}"#,
            "{}",
        ),
        Err(MetadataError::ReservedField)
    );
    assert!(
        ContributionDocument::from_json_with_system_fields(
            &system,
            key("p"),
            SourceKind::Provider,
            1,
            r#"{"plain_name":1}"#,
            "{}",
        )
        .is_ok()
    );
    assert_eq!(
        plan_schema_extension(&system, vec![("plain_name".into(), exact())], vec![]),
        Err(MetadataError::SchemaMismatch)
    );
    assert_eq!(
        validate_schema_extension(&external, &empty),
        Err(MetadataError::SchemaMismatch)
    );
}

#[test]
fn empty_v2_union_counts_exact_snapshot_nodes_and_retains_other_budgets() {
    let empty = FieldSchema::new_v2(vec![], vec![]).unwrap();
    let make = |source: &str, nodes: usize| {
        ContributionDocument::from_json(
            &empty,
            key(source),
            SourceKind::Provider,
            1,
            "{}",
            &json!({"samples":vec![0; nodes]}).to_string(),
        )
        .unwrap()
    };
    let envelope = json_nodes(&serde_json::to_value(make("p", 0)).unwrap()).unwrap();
    let remaining =
        MAX_METADATA_JSON_NODES - json_nodes(&empty.v2_payload()).unwrap() - 3 * envelope;
    let documents = (0..3)
        .map(|index| {
            make(
                &format!("p{index}"),
                remaining / 3 + usize::from(index < remaining % 3),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        json_nodes(&empty.v2_payload()).unwrap()
            + documents
                .iter()
                .map(|doc| json_nodes(&serde_json::to_value(doc).unwrap()).unwrap())
                .sum::<usize>(),
        MAX_METADATA_JSON_NODES
    );
    assert!(resolve(&empty, "item:1", &documents).is_ok());
    let mut overflow = documents;
    overflow[0] = make("p0", remaining / 3 + usize::from(remaining % 3 > 0) + 1);
    assert_eq!(
        resolve(&empty, "item:1", &overflow),
        Err(MetadataError::JsonNodeBudget)
    );
    let sources = (0..MAX_METADATA_DOCUMENTS)
        .map(|index| document(&empty, &format!("empty{index}"), "{}"))
        .collect::<Vec<_>>();
    assert_eq!(
        resolve(&empty, "item:1", &sources).unwrap().inputs.len(),
        MAX_METADATA_DOCUMENTS
    );
    let mut overflow = sources;
    overflow.push(document(&empty, "one-too-many", "{}"));
    assert_eq!(
        resolve(&empty, "item:1", &overflow),
        Err(MetadataError::SourceBudget)
    );
    assert_eq!(
        ContributionDocument::from_json(
            &empty,
            key("p"),
            SourceKind::Provider,
            1,
            &" ".repeat(MAX_METADATA_DOCUMENT_BYTES + 1),
            "{}",
        ),
        Err(MetadataError::DocumentBudget)
    );
    assert_eq!(
        ContributionDocument::from_json(
            &empty,
            key("p"),
            SourceKind::Provider,
            1,
            "{}",
            &" ".repeat(MAX_METADATA_EVIDENCE_BYTES + 1),
        ),
        Err(MetadataError::EvidenceBudget)
    );
}

#[test]
fn replacement_field_planning_uses_lossless_bounded_validation_without_normalizing() {
    assert_eq!(replacement_fields(&fields(0, 100)).unwrap().len(), 100);
    assert_eq!(
        replacement_fields(&fields(0, 101)),
        Err(MetadataError::FieldBudget)
    );
    let parsed =
        replacement_fields(r#"{"value":"  Keep Case  ","number":1208925819614629174706176}"#)
            .unwrap();
    assert_eq!(parsed["value"], "  Keep Case  ");
    assert_eq!(parsed["number"].to_string(), "1208925819614629174706176");
    assert_eq!(replacement_fields("[]"), Err(MetadataError::ExpectedObject));
    assert_eq!(
        replacement_fields(r#"{"value":1,"\u0076alue":2}"#),
        Err(MetadataError::InvalidJson)
    );
    assert_eq!(
        replacement_fields(r#"{"value":{"duplicate":1,"duplicate":2}}"#),
        Err(MetadataError::InvalidJson)
    );
    let valid_name = serde_json::to_string(&BTreeMap::from([("é".repeat(50), json!(0))])).unwrap();
    assert!(replacement_fields(&valid_name).is_ok());
    let invalid_name =
        serde_json::to_string(&BTreeMap::from([("é".repeat(51), json!(0))])).unwrap();
    assert_eq!(
        replacement_fields(&invalid_name),
        Err(MetadataError::InvalidField)
    );
    assert_eq!(
        replacement_fields(&" ".repeat(MAX_METADATA_DOCUMENT_BYTES + 1)),
        Err(MetadataError::DocumentBudget)
    );
    let too_many_nodes = format!(
        r#"{{"value":[{}]}}"#,
        vec!["0"; MAX_METADATA_JSON_NODES].join(",")
    );
    assert_eq!(
        replacement_fields(&too_many_nodes),
        Err(MetadataError::JsonNodeBudget)
    );
    let too_deep = format!(
        r#"{{"value":{}0{}}}"#,
        "[".repeat(MAX_METADATA_JSON_DEPTH),
        "]".repeat(MAX_METADATA_JSON_DEPTH)
    );
    assert_eq!(
        replacement_fields(&too_deep),
        Err(MetadataError::JsonDepthBudget)
    );
}

#[test]
fn v1_fingerprint_payload_and_sixty_four_limits_are_unchanged() {
    let old_policies = policies(0, 64);
    let schema = FieldSchema::new(old_policies.clone()).unwrap();
    let policy_map: BTreeMap<_, _> = old_policies.into_iter().collect();
    let historical_fingerprint = crate::sha256(crate::encode_with(
        &json!({"contract_version": 1, "fields": policy_map}),
        false,
        true,
    ));
    assert_eq!(schema.fingerprint(), historical_fingerprint);
    assert_eq!(schema.contract_version(), 1);
    assert!(schema.system_fields().is_empty());
    assert_eq!(document(&schema, "p", &fields(0, 64)).fields().len(), 64);
    assert_eq!(
        FieldSchema::new(policies(0, 65)),
        Err(MetadataError::FieldBudget)
    );
    assert!(FieldSchema::new(vec![("a".repeat(64), exact())]).is_ok());
    assert_eq!(
        FieldSchema::new(vec![("a".repeat(65), exact())]),
        Err(MetadataError::InvalidField)
    );
    let document = document(&schema, "p", &fields(0, 64));
    assert_eq!(
        revalidate_document(&schema, &schema, &document).unwrap(),
        document
    );
    assert_eq!(
        resolve(&schema, "item:1", &[document])
            .unwrap()
            .contract_version,
        1
    );
}

#[test]
fn v2_accepts_one_hundred_fields_and_byte_counted_names() {
    let schema = FieldSchema::new_v2(policies(0, 101), vec![]).unwrap();
    assert_eq!(document(&schema, "p", &fields(0, 100)).fields().len(), 100);
    assert_eq!(
        ContributionDocument::from_json(
            &schema,
            key("p"),
            SourceKind::Provider,
            1,
            &fields(0, 101),
            "{}",
        ),
        Err(MetadataError::FieldBudget)
    );
    for name in ["a".repeat(100), "é".repeat(50)] {
        let schema = FieldSchema::new_v2(vec![(name.clone(), exact())], vec![]).unwrap();
        let raw = serde_json::to_string(&BTreeMap::from([(name.clone(), json!(1))])).unwrap();
        assert_eq!(document(&schema, "p", &raw).fields()[&name], json!(1));
    }
    for name in [
        "a".repeat(101),
        "é".repeat(51),
        " bad".into(),
        "bad\0name".into(),
    ] {
        assert_eq!(
            FieldSchema::new_v2(vec![(name, exact())], vec![]),
            Err(MetadataError::InvalidField)
        );
    }
}

#[test]
fn independent_sources_resolve_a_union_larger_than_one_hundred_twenty_eight() {
    let schema = FieldSchema::new_v2(policies(0, 200), vec![]).unwrap();
    let a = document(&schema, "a", &fields(0, 100));
    let b = document(&schema, "b", &fields(100, 100));
    let resolved = resolve(&schema, "item:1", &[a.clone(), b.clone()]).unwrap();
    assert_eq!(resolved.contract_version, 2);
    assert_eq!(resolved.fields.len(), 200);
    assert!(
        resolved
            .fields
            .values()
            .all(|field| matches!(field, FieldResolution::Resolved { .. }))
    );
    assert_eq!(resolved, resolve(&schema, "item:1", &[b, a]).unwrap());
}

#[test]
fn v2_indexed_resolution_preserves_v1_precedence_conflicts_and_attribution() {
    let old = FieldSchema::new(vec![
        ("title".into(), exact()),
        (
            "language".into(),
            FieldPolicy {
                value_type: ValueType::String,
                nullable: true,
                normalization: Normalization::TrimAsciiLowercase,
            },
        ),
        ("details".into(), exact()),
        ("flag".into(), exact()),
    ])
    .unwrap();
    let current = plan_schema_extension(&old, vec![], vec![]).unwrap();
    let fixtures = [
        ("z-local", SourceKind::Local, r#"{"language":null}"#),
        (
            "x-legacy",
            SourceKind::LegacyImport,
            r#"{"title":"Retained"}"#,
        ),
        (
            "a-provider",
            SourceKind::Provider,
            r#"{"title":"Provider A","language":" EN ","details":{"x":1}}"#,
        ),
        (
            "b-provider",
            SourceKind::Provider,
            r#"{"title":"Provider B","language":"en","details":{"x":2}}"#,
        ),
        (
            "c-scan",
            SourceKind::Scan,
            r#"{"title":"Scan","flag":true}"#,
        ),
        ("d-empty", SourceKind::Provider, "{}"),
    ];
    for mask in 0usize..(1usize << fixtures.len()) {
        let documents = fixtures
            .iter()
            .enumerate()
            .filter(|(index, _)| mask & (1usize << *index) != 0)
            .map(|(index, (source, kind, raw))| {
                ContributionDocument::from_json(
                    &old,
                    key(source),
                    *kind,
                    index as i64 + 1,
                    raw,
                    "{}",
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let baseline = resolve(&old, "item:1", &documents).unwrap();
        let mut migrated = documents
            .iter()
            .map(|document| revalidate_document(&current, &old, document).unwrap())
            .collect::<Vec<_>>();
        migrated.reverse();
        let resolved = resolve(&current, "item:1", &migrated).unwrap();
        assert_eq!(resolved.fields, baseline.fields);
        assert_eq!(resolved.inputs, baseline.inputs);
    }
}

#[test]
fn reserved_policies_are_explicit_separate_and_not_external_input() {
    let schema =
        FieldSchema::new_v2(policies(0, 100), vec![("native_title".into(), exact())]).unwrap();
    let mut values: BTreeMap<String, Value> = serde_json::from_str(&fields(0, 100)).unwrap();
    values.insert("native_title".into(), json!("Native title"));
    let raw = serde_json::to_string(&values).unwrap();
    assert_eq!(
        ContributionDocument::from_json(&schema, key("p"), SourceKind::Provider, 1, &raw, "{}"),
        Err(MetadataError::ReservedField)
    );
    let trusted = ContributionDocument::from_json_with_system_fields(
        &schema,
        key("p"),
        SourceKind::Provider,
        1,
        &raw,
        "{}",
    )
    .unwrap();
    assert_eq!(trusted.fields().len(), 101);
    assert_eq!(
        schema.system_fields(),
        &BTreeSet::from(["native_title".to_owned()])
    );
    assert_eq!(
        ContributionPatch::from_json(
            &schema,
            key("p"),
            SourceKind::Provider,
            1,
            r#"{"native_title":{"op":"remove"}}"#,
            "{}",
        ),
        Err(MetadataError::ReservedField)
    );
    assert_eq!(
        ContributionReplacement::from_json(
            &schema,
            key("p"),
            SourceKind::Provider,
            1,
            r#"{"native_title":"overwrite"}"#,
            "{}",
        ),
        Err(MetadataError::ReservedField)
    );
    let patch = ContributionPatch::from_json_with_system_fields(
        &schema,
        key("p"),
        SourceKind::Provider,
        1,
        r#"{"native_title":{"op":"set","value":"Renamed"}}"#,
        "{}",
    )
    .unwrap();
    assert_eq!(
        plan_update(&schema, Some(&trusted), &patch)
            .unwrap()
            .fields()["native_title"],
        "Renamed"
    );
    // A spelling resembling a reserved name is not implicitly reserved.
    let explicit = FieldSchema::new_v2(vec![("__provider_data".into(), exact())], vec![]).unwrap();
    assert!(
        ContributionDocument::from_json(
            &explicit,
            key("p"),
            SourceKind::Provider,
            1,
            r#"{"__provider_data":1}"#,
            "{}",
        )
        .is_ok()
    );
}

#[test]
fn full_replacement_changes_one_hundred_fields_without_two_hundred_patch_edits() {
    let schema =
        FieldSchema::new_v2(policies(0, 200), vec![("native_title".into(), exact())]).unwrap();
    let mut original_values: BTreeMap<String, Value> =
        serde_json::from_str(&fields(0, 100)).unwrap();
    original_values.insert("native_title".into(), json!("Keep"));
    let original = ContributionDocument::from_json_with_system_fields(
        &schema,
        key("p"),
        SourceKind::Provider,
        7,
        &serde_json::to_string(&original_values).unwrap(),
        r#"{"observation":"old"}"#,
    )
    .unwrap();
    let replacement = ContributionReplacement::from_json(
        &schema,
        key("p"),
        SourceKind::Provider,
        7,
        &fields(100, 100),
        r#"{"observation":"new"}"#,
    )
    .unwrap();
    let updated = plan_replace(&schema, Some(&original), &replacement).unwrap();
    assert_eq!(updated.revision(), 8);
    assert_eq!(updated.fields().len(), 101);
    assert_eq!(updated.fields()["native_title"], "Keep");
    assert!(!updated.fields().contains_key("field_0000"));
    assert_eq!(updated.fields()["field_0199"], 199);
    assert_eq!(updated.evidence()["observation"], "new");
    assert_eq!(original.revision(), 7);
    assert_eq!(original.fields(), &original_values);
    assert_eq!(
        plan_replace(&schema, Some(&updated), &replacement),
        Err(MetadataError::StaleSourceRevision {
            expected: 7,
            actual: 8
        })
    );
    let too_many_edits = serde_json::to_string(
        &(0..200)
            .map(|index| (format!("field_{index:04}"), json!({"op":"remove"})))
            .collect::<BTreeMap<_, _>>(),
    )
    .unwrap();
    assert_eq!(
        ContributionPatch::from_json(
            &schema,
            key("p"),
            SourceKind::Provider,
            7,
            &too_many_edits,
            "{}"
        ),
        Err(MetadataError::FieldBudget)
    );
    let deletion =
        ContributionReplacement::from_json(&schema, key("p"), SourceKind::Provider, 8, "{}", "{}")
            .unwrap();
    let emptied = plan_replace(&schema, Some(&updated), &deletion).unwrap();
    assert_eq!(emptied.revision(), 9);
    assert_eq!(
        emptied.fields(),
        &BTreeMap::from([("native_title".into(), json!("Keep"))])
    );
}

#[test]
fn patches_cannot_grow_a_source_beyond_one_hundred_external_fields() {
    let schema = FieldSchema::new_v2(policies(0, 101), vec![]).unwrap();
    let current = document(&schema, "p", &fields(0, 100));
    let patch = ContributionPatch::from_json(
        &schema,
        key("p"),
        SourceKind::Provider,
        1,
        r#"{"field_0100":{"op":"set","value":100}}"#,
        "{}",
    )
    .unwrap();
    assert_eq!(
        plan_update(&schema, Some(&current), &patch),
        Err(MetadataError::FieldBudget)
    );
}

#[test]
fn append_only_extension_keeps_v1_history_and_unrelated_revisions_unchanged() {
    let title_policy = FieldPolicy {
        value_type: ValueType::String,
        nullable: false,
        normalization: Normalization::TrimString,
    };
    let old = FieldSchema::new(vec![
        ("title".into(), title_policy),
        ("native_title".into(), exact()),
    ])
    .unwrap();
    let original = ContributionDocument::from_json(
        &old,
        key("provider"),
        SourceKind::Provider,
        41,
        r#"{"title":" Normalized ","native_title":"Old native"}"#,
        r#"{"source":"historical"}"#,
    )
    .unwrap();
    let unrelated = ContributionDocument::from_json(
        &old,
        key("other"),
        SourceKind::Scan,
        8,
        r#"{"title":"Scan"}"#,
        "{}",
    )
    .unwrap();
    let original_bytes = serde_json::to_string(&original).unwrap();
    let unrelated_bytes = serde_json::to_string(&unrelated).unwrap();
    let old_fingerprint = old.fingerprint().to_owned();
    let current = plan_schema_extension(
        &old,
        policies(0, 150),
        vec![("native_title".into(), exact())],
    )
    .unwrap();
    assert_eq!(current.contract_version(), 2);
    assert_eq!(current.fields().len(), 152);
    assert_eq!(current.fields()["title"], title_policy);
    assert_eq!(old.fingerprint(), old_fingerprint);
    assert_ne!(current.fingerprint(), old.fingerprint());
    let view = revalidate_document(&current, &old, &original).unwrap();
    let unrelated_view = revalidate_document(&current, &old, &unrelated).unwrap();
    assert_eq!(view.revision(), 41);
    assert_eq!(unrelated_view.revision(), 8);
    assert_eq!(view.fields(), original.fields());
    assert_eq!(view.evidence(), original.evidence());
    assert_eq!(view.key(), original.key());
    assert_eq!(view.kind(), original.kind());
    assert_eq!(view.schema_fingerprint(), current.fingerprint());
    assert_eq!(view.encoded_bytes, original.encoded_bytes);
    assert_eq!(serde_json::to_string(&original).unwrap(), original_bytes);
    assert_eq!(serde_json::to_string(&unrelated).unwrap(), unrelated_bytes);
    assert_eq!(original.schema_fingerprint(), old_fingerprint);
    let resolution = resolve(&current, "item:1", &[view, unrelated_view]).unwrap();
    assert_eq!(
        resolution
            .inputs
            .iter()
            .map(|source| source.revision)
            .collect::<Vec<_>>(),
        vec![8, 41]
    );
    assert_eq!(
        plan_schema_extension(&current, vec![], vec![]).unwrap(),
        current
    );
}

#[test]
fn compatibility_rejects_changed_removed_or_reclassified_policies() {
    let old = FieldSchema::new(vec![("title".into(), exact())]).unwrap();
    let current = plan_schema_extension(&old, vec![("new".into(), exact())], vec![]).unwrap();
    let changed = FieldPolicy {
        nullable: false,
        ..exact()
    };
    assert_eq!(
        plan_schema_extension(&old, vec![("title".into(), changed)], vec![]),
        Err(MetadataError::SchemaMismatch)
    );
    let removed = FieldSchema::new_v2(vec![("new".into(), exact())], vec![]).unwrap();
    assert_eq!(
        validate_schema_extension(&old, &removed),
        Err(MetadataError::SchemaMismatch)
    );
    assert_eq!(
        plan_schema_extension(&current, vec![], vec![("title".into(), exact())]),
        Err(MetadataError::SchemaMismatch)
    );
    let system = plan_schema_extension(&old, vec![], vec![("title".into(), exact())]).unwrap();
    assert_eq!(
        plan_schema_extension(&system, vec![("title".into(), exact())], vec![]),
        Err(MetadataError::SchemaMismatch)
    );
    assert_eq!(
        validate_schema_extension(&current, &old),
        Err(MetadataError::SchemaMismatch)
    );
    let other = FieldSchema::new(vec![("other".into(), exact())]).unwrap();
    let historical = document(&old, "p", r#"{"title":"old"}"#);
    assert_eq!(
        revalidate_document(&current, &other, &historical),
        Err(MetadataError::SchemaMismatch)
    );
}

#[test]
fn legacy_revalidation_never_renormalizes_or_relabels_retained_values() {
    let old = FieldSchema::new(vec![(
        "title".into(),
        FieldPolicy {
            value_type: ValueType::String,
            nullable: false,
            normalization: Normalization::TrimAsciiLowercase,
        },
    )])
    .unwrap();
    let legacy = ContributionDocument::from_json(
        &old,
        key("legacy"),
        SourceKind::LegacyImport,
        9,
        r#"{"title":"  OLD Title  "}"#,
        "{}",
    )
    .unwrap();
    let current = plan_schema_extension(&old, vec![("new".into(), exact())], vec![]).unwrap();
    let view = revalidate_document(&current, &old, &legacy).unwrap();
    assert_eq!(view.fields()["title"], "  OLD Title  ");
    assert_eq!(view.kind(), SourceKind::LegacyImport);
    assert_eq!(view.revision(), 9);
    assert_eq!(
        ContributionReplacement::from_json(
            &current,
            key("legacy"),
            SourceKind::LegacyImport,
            9,
            "{}",
            "{}"
        ),
        Err(MetadataError::LegacyReconciliationRequired)
    );
}

#[test]
fn duplicate_unknown_and_reserved_inputs_fail_without_truncation() {
    assert_eq!(
        FieldSchema::new_v2(
            vec![("same".into(), exact())],
            vec![("same".into(), exact())]
        ),
        Err(MetadataError::DuplicateField)
    );
    let schema = FieldSchema::new_v2(
        vec![("data".into(), exact())],
        vec![("reserved".into(), exact())],
    )
    .unwrap();
    assert_eq!(
        plan_schema_extension(
            &schema,
            vec![("new".into(), exact()), ("new".into(), exact())],
            vec![]
        ),
        Err(MetadataError::DuplicateField)
    );
    for (raw, error) in [
        (r#"{"data":1,"data":2}"#, MetadataError::InvalidJson),
        (r#"{"data":1,"\u0064ata":2}"#, MetadataError::InvalidJson),
        (r#"{"data":{"x":1,"x":2}}"#, MetadataError::InvalidJson),
        (r#"{"unknown":1}"#, MetadataError::UnknownField),
        (r#"{"reserved":1}"#, MetadataError::ReservedField),
    ] {
        assert_eq!(
            ContributionReplacement::from_json(
                &schema,
                key("p"),
                SourceKind::Provider,
                0,
                raw,
                "{}"
            ),
            Err(error)
        );
    }
}

#[test]
fn union_and_document_node_budgets_are_enforced() {
    assert!(FieldSchema::new_v2(policies(0, 2000), vec![]).is_ok());
    assert_eq!(
        FieldSchema::new_v2(policies(0, 2050), vec![]),
        Err(MetadataError::JsonNodeBudget)
    );
    let schema = FieldSchema::new_v2(vec![("data".into(), exact())], vec![]).unwrap();
    let oversized = format!(
        r#"{{"data":[{}]}}"#,
        vec!["0"; MAX_METADATA_JSON_NODES].join(",")
    );
    assert_eq!(
        ContributionDocument::from_json(
            &schema,
            key("p"),
            SourceKind::Provider,
            1,
            &oversized,
            "{}"
        ),
        Err(MetadataError::JsonNodeBudget)
    );
    let one = format!(r#"{{"data":[{}]}}"#, vec!["0"; 500].join(","));
    let documents = (0..20)
        .map(|index| document(&schema, &format!("p{index}"), &one))
        .collect::<Vec<_>>();
    assert_eq!(
        resolve(&schema, "item:1", &documents),
        Err(MetadataError::JsonNodeBudget)
    );
}

#[test]
fn resolution_output_is_also_node_bounded() {
    let schema = FieldSchema::new_v2(policies(0, 100), vec![]).unwrap();
    let values: BTreeMap<_, _> = (0..100)
        .map(|index| (format!("field_{index:04}"), json!(vec![0; 70])))
        .collect();
    let current = document(&schema, "p", &serde_json::to_string(&values).unwrap());
    assert_eq!(
        resolve(&schema, "item:1", &[current]),
        Err(MetadataError::JsonNodeBudget)
    );
}

#[test]
fn byte_budgets_remain_sixty_four_kib_documents_and_two_mib_snapshots() {
    let schema = FieldSchema::new_v2(vec![("data".into(), exact())], vec![]).unwrap();
    let raw = serde_json::to_string(&json!({"data": "x".repeat(MAX_METADATA_DOCUMENT_BYTES - 20)}))
        .unwrap();
    assert!(raw.len() < MAX_METADATA_DOCUMENT_BYTES);
    assert_eq!(
        ContributionDocument::from_json(&schema, key("p"), SourceKind::Provider, 1, &raw, "{}"),
        Err(MetadataError::DocumentBudget)
    );
    let raw = serde_json::to_string(&json!({"data": "x".repeat(60_000)})).unwrap();
    let documents = (0..36)
        .map(|index| document(&schema, &format!("p{index}"), &raw))
        .collect::<Vec<_>>();
    assert_eq!(
        resolve(&schema, "item:1", &documents),
        Err(MetadataError::SnapshotBudget)
    );
}

#[test]
fn source_limit_remains_one_hundred_twenty_eight_not_the_union_limit() {
    let schema = FieldSchema::new_v2(policies(0, 200), vec![]).unwrap();
    let mut documents = (0..MAX_METADATA_DOCUMENTS)
        .map(|index| document(&schema, &format!("p{index}"), "{}"))
        .collect::<Vec<_>>();
    assert_eq!(
        resolve(&schema, "item:1", &documents).unwrap().inputs.len(),
        MAX_METADATA_DOCUMENTS
    );
    documents.push(document(&schema, "extra", "{}"));
    assert_eq!(
        resolve(&schema, "item:1", &documents),
        Err(MetadataError::SourceBudget)
    );
}

#[test]
fn replacement_checks_creation_key_kind_and_revision_exhaustion() {
    let schema = FieldSchema::new_v2(policies(0, 1), vec![]).unwrap();
    let create =
        ContributionReplacement::from_json(&schema, key("p"), SourceKind::Provider, 0, "{}", "{}")
            .unwrap();
    let current = plan_replace(&schema, None, &create).unwrap();
    assert_eq!(current.revision(), 1);
    assert_eq!(
        plan_replace(&schema, Some(&current), &create),
        Err(MetadataError::StaleSourceRevision {
            expected: 0,
            actual: 1
        })
    );
    let other = ContributionReplacement::from_json(
        &schema,
        key("other"),
        SourceKind::Provider,
        1,
        "{}",
        "{}",
    )
    .unwrap();
    assert_eq!(
        plan_replace(&schema, Some(&current), &other),
        Err(MetadataError::SourceMismatch)
    );
    let other_kind =
        ContributionReplacement::from_json(&schema, key("p"), SourceKind::Scan, 1, "{}", "{}")
            .unwrap();
    assert_eq!(
        plan_replace(&schema, Some(&current), &other_kind),
        Err(MetadataError::SourceKindMismatch)
    );
    let exhausted = ContributionDocument::from_json(
        &schema,
        key("p"),
        SourceKind::Provider,
        i64::MAX,
        "{}",
        "{}",
    )
    .unwrap();
    let replace = ContributionReplacement::from_json(
        &schema,
        key("p"),
        SourceKind::Provider,
        i64::MAX,
        "{}",
        "{}",
    )
    .unwrap();
    assert_eq!(
        plan_replace(&schema, Some(&exhausted), &replace),
        Err(MetadataError::RevisionExhausted)
    );
}
