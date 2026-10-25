use super::*;
use crate::{
    catalog_import::{ImportItem, describe_batch},
    catalog_import_typed::{ImportedTagSet, TypedBatch},
    operation_budget::{BudgetLimits, OperationBudget},
};
use serde_json::json;
use std::time::Duration;

const SUBJECT: &str = "archived-nfo-item";
const REVISION: i64 = 9;

fn shared() -> OperationBudget {
    OperationBudget::new(
        Duration::from_secs(30),
        BudgetLimits {
            bytes: usize::MAX,
            items: usize::MAX,
            values: usize::MAX,
            work: usize::MAX,
        },
    )
    .unwrap()
}
fn budget() -> ProofBudget {
    ProofBudget::new(shared())
}
fn document(tags: &[&str]) -> Value {
    // Production supplies this same duplicate-safe parse after raw admission;
    // these finite unit fixtures do not claim an activated manifest or grant.
    let raw = json!({"values":{"title":"Original"},"tags":tags,"excluded_tags":[]}).to_string();
    catabolic_core::json::parse(&raw, true).unwrap()
}
fn record(tags: &[&str]) -> ImportedTagSet {
    ImportedTagSet {
        item_id: SUBJECT.into(),
        namespace: NAMESPACE.into(),
        source_revision: REVISION,
        tags: tags.iter().map(|name| (*name).to_owned()).collect(),
        excluded_tags: vec![],
    }
}
fn batch(records: Vec<ImportedTagSet>) -> ImportBatch {
    ImportBatch {
        items: vec![ImportItem {
            source_id: SUBJECT.into(),
            item_id: SUBJECT.into(),
            kind: "video".into(),
            legacy_metadata: json!({}),
        }],
        unresolved_evidence: vec![],
        typed: (!records.is_empty()).then(|| TypedBatch {
            tags: records,
            ..Default::default()
        }),
    }
}
fn verify(original: &Value, batches: &[ImportBatch]) -> Result<ImportedRequestedNames> {
    verify_requested_names(SUBJECT, REVISION, original, batches, &mut budget())
}

#[test]
fn nonempty_requested_names_are_exact_and_digest_binds_the_complete_input() {
    let original = document(&["genre:drama", "favorite"]);
    let batches = vec![batch(vec![record(&["favorite", "genre:drama"])])];
    describe_batch(&batches[0]).unwrap();
    let proof = verify(&original, &batches).unwrap();
    assert_eq!(proof.subject(), SUBJECT);
    assert_eq!(proof.source_revision(), REVISION);
    assert_eq!(proof.matching_record(), Some((0, 0)));
    assert_eq!(
        proof.tags(),
        &BTreeSet::from(["favorite".into(), "genre:drama".into()])
    );
    assert_eq!(proof.digest().len(), 64);
    assert_eq!(
        proof.digest(),
        verify(&original, &batches).unwrap().digest()
    );

    let mut changed = original.clone();
    changed["values"]["title"] = json!("Different archived bytes");
    assert_ne!(proof.digest(), verify(&changed, &batches).unwrap().digest());
    let mut changed = batches.clone();
    changed[0].items[0].legacy_metadata = json!({"unrelated":"retained"});
    assert_ne!(
        proof.digest(),
        verify(&original, &changed).unwrap().digest()
    );
}

#[test]
fn empty_omission_does_not_claim_independent_local_exclusions() {
    let mut local = record(&["favorite"]);
    local.namespace = "motion.local".into();
    local.excluded_tags = vec!["genre:drama".into()];
    let batches = vec![batch(vec![local])];
    describe_batch(&batches[0]).unwrap();
    let proof = verify(&document(&[]), &batches).unwrap();
    assert!(proof.tags().is_empty());
    assert_eq!(proof.matching_record(), None);
}

#[test]
fn explicit_empty_record_is_distinct_from_verified_omission() {
    let original = document(&[]);
    let present = verify(&original, &[batch(vec![record(&[])])]).unwrap();
    let omitted = verify(&original, &[batch(vec![])]).unwrap();
    assert!(present.tags().is_empty());
    assert_eq!(present.matching_record(), Some((0, 0)));
    assert_ne!(present.digest(), omitted.digest());
}

#[test]
fn unique_matching_record_may_be_in_another_batch_of_the_same_activation() {
    let first = batch(vec![]);
    let mut second = batch(vec![record(&["favorite"])]);
    second.items.clear();
    describe_batch(&first).unwrap();
    describe_batch(&second).unwrap();
    let proof = verify(&document(&["favorite"]), &[first, second]).unwrap();
    assert_eq!(proof.matching_record(), Some((1, 0)));
}

#[test]
fn missing_or_mismatched_subject_namespace_revision_and_names_refuse() {
    let original = document(&["favorite"]);
    assert!(verify(&original, &[batch(vec![])]).is_err());
    for change in 0..5 {
        let mut imported = record(&["favorite"]);
        match change {
            0 => imported.item_id = "another-item".into(),
            1 => imported.namespace = "motion.provider".into(),
            2 => imported.source_revision += 1,
            3 => imported.tags = vec!["other".into()],
            4 => imported.tags.clear(),
            _ => unreachable!(),
        }
        assert!(verify(&original, &[batch(vec![imported])]).is_err());
    }
    assert!(
        verify_requested_names(SUBJECT, 0, &original, &[batch(vec![])], &mut budget()).is_err()
    );
    assert!(
        verify_requested_names("", REVISION, &original, &[batch(vec![])], &mut budget()).is_err()
    );
}

#[test]
fn duplicate_requested_names_or_matching_records_never_collapse_silently() {
    assert!(
        verify(
            &document(&["favorite", "favorite"]),
            &[batch(vec![record(&["favorite"])])]
        )
        .is_err()
    );
    assert!(
        verify(
            &document(&["favorite"]),
            &[batch(vec![record(&["favorite", "favorite"])])]
        )
        .is_err()
    );
    assert!(verify(&document(&[]), &[batch(vec![record(&[]), record(&[])])]).is_err());
    let mut second = batch(vec![record(&[])]);
    second.items.clear();
    assert!(verify(&document(&[]), &[batch(vec![record(&[])]), second]).is_err());
}

#[test]
fn noncanonical_and_nontext_names_are_not_normalized_into_acceptance() {
    for name in ["Favorite", " favorite", "genre: drama", "e\u{301}", ""] {
        assert!(verify(&document(&[name]), &[batch(vec![record(&[name])])]).is_err());
    }
    let mut original = document(&[]);
    original["tags"] = json!([7]);
    assert!(verify(&original, &[batch(vec![])]).is_err());
    assert!(
        verify(
            &document(&["favorite"]),
            &[batch(vec![record(&["Favorite"])])]
        )
        .is_err()
    );
    // Casefold expands U+0390 to three scalars, then NFC returns the exact
    // original spelling. Reserve the expansion without rejecting that identity.
    let shared = shared();
    let mut proof_budget = ProofBudget::new(shared.clone());
    reserve_normalization("\u{390}", &mut proof_budget).unwrap();
    let scalar_buffers =
        2 * std::mem::size_of::<(u8, char)>() + std::mem::size_of::<char>() + 3 * 4;
    assert_eq!(shared.usage().bytes, 2 + 2 * 6 + scalar_buffers * 3);
    assert!(
        verify(
            &document(&["\u{390}"]),
            &[batch(vec![record(&["\u{390}"])])]
        )
        .is_ok()
    );
}

#[test]
fn provider_exclusions_refuse_even_when_both_archived_records_agree() {
    let mut original = document(&["favorite"]);
    let mut imported = record(&["favorite"]);
    original["excluded_tags"] = json!(["hidden"]);
    imported.excluded_tags = vec!["hidden".into()];
    assert!(verify(&original, &[batch(vec![imported.clone()])]).is_err());
    assert!(verify(&document(&["favorite"]), &[batch(vec![imported])]).is_err());
    assert!(verify(&original, &[batch(vec![record(&["favorite"])])]).is_err());
}

#[test]
fn required_arrays_and_duplicate_safe_raw_parse_are_explicit_boundaries() {
    for original in [
        json!({"values":{},"tags":[]}),
        json!({"values":{},"excluded_tags":[]}),
        json!({"values":{},"tags":null,"excluded_tags":[]}),
        json!({"values":{},"tags":[],"excluded_tags":{}}),
        json!([]),
    ] {
        assert!(verify(&original, &[batch(vec![])]).is_err());
    }
    // Value cannot recover duplicate keys. The production caller must use the
    // strict raw parser rather than serde's last-key-wins conversion.
    let raw = r#"{"values":{},"tags":["favorite"],"tags":[],"excluded_tags":[]}"#;
    budget().raw_json(raw, MAX_METADATA_JSON_DEPTH).unwrap();
    assert!(catabolic_core::json::parse(raw, true).is_err());
}

#[test]
fn name_limits_are_utf8_bytes_and_cardinality_is_not_truncated() {
    let hundred = "é".repeat(50);
    let batches = vec![batch(vec![record(&[&hundred])])];
    describe_batch(&batches[0]).unwrap();
    assert!(verify(&document(&[&hundred]), &batches).is_ok());
    let too_long = format!("{hundred}a");
    assert!(
        verify(
            &document(&[&too_long]),
            &[batch(vec![record(&[&too_long])])]
        )
        .is_err()
    );
    let names: Vec<String> = (0..101).map(|index| format!("tag-{index}")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(verify(&document(&names), &[batch(vec![])]).is_err());
}

#[test]
fn batch_closure_bound_is_explicit_without_accepting_a_prefix() {
    assert!(verify(&document(&[]), &[]).is_err());
    let batches = vec![batch(vec![]); MAX_IMPORT_BATCHES + 1];
    assert!(verify(&document(&[]), &batches).is_err());
}

#[test]
fn complete_input_and_inherited_budget_are_charged_before_returning_evidence() {
    let mut unrelated = batch(vec![]);
    unrelated.items[0].legacy_metadata = json!({"large":"x".repeat(2 * 1024 * 1024)});
    assert!(verify(&document(&[]), &[unrelated]).is_err());
    let mut unrelated = batch(vec![]);
    unrelated.items[0].legacy_metadata = json!(vec![0; 8192]);
    assert!(verify(&document(&[]), &[unrelated]).is_err());

    let mut cumulative = budget();
    cumulative
        .charge(BudgetCharge {
            work: 8192,
            ..Default::default()
        })
        .unwrap();
    assert!(
        verify_requested_names(
            SUBJECT,
            REVISION,
            &document(&[]),
            &[batch(vec![])],
            &mut cumulative
        )
        .is_err()
    );

    let shared = OperationBudget::new(
        Duration::from_secs(30),
        BudgetLimits {
            bytes: 64,
            items: usize::MAX,
            values: usize::MAX,
            work: usize::MAX,
        },
    )
    .unwrap();
    shared
        .charge(BudgetCharge {
            bytes: 64,
            ..Default::default()
        })
        .unwrap();
    let mut inherited = ProofBudget::new(shared.clone());
    assert!(
        verify_requested_names(
            SUBJECT,
            REVISION,
            &document(&[]),
            &[batch(vec![])],
            &mut inherited
        )
        .is_err()
    );
    assert!(shared.failure().is_some());
}
