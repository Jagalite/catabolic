// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Typed bounded-owner refusals preserve legacy diagnostics and caller ownership.
use catabolic_store::{
    Store, embedding, migration,
    selection::{Evaluation, EvaluationFailure},
};
use serde_json::{Value, json};
use std::time::Duration;

fn fixture() -> (tempfile::TempDir, embedding::Session) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("bounded-failures.db");
    migration::initialize(&path).unwrap();
    let session = embedding::Session::open(&path, true, false).unwrap();
    (root, session)
}
fn save(store: &Store, id: &str, definition: Value) {
    store.db.execute(
        "INSERT INTO saved_queries(id,profile,name,revision,definition,digest) VALUES (?,'default',?,1,?,?)",
        [id, id, &catabolic_core::encode(&definition), id],
    ).unwrap();
}
fn sql(query: &str) -> Value {
    json!({"language":"sql","query":query,"profile":"default"})
}
fn definition(selection: Value) -> Value {
    json!({"mode":"selection","entity":"item_id","selection":selection,"timeout_ms":5000,"max_ids":10000})
}
fn selection_error(evaluation: &mut Evaluation, selection: Value) -> catabolic_store::Error {
    match evaluation.select(&selection) {
        Ok(_) => panic!("expected a complete-selection refusal"),
        Err(error) => error,
    }
}
const THREE: &str = "SELECT 'a' AS item_id UNION ALL SELECT 'b' UNION ALL SELECT 'c'";
const EXPENSIVE: &str = "WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<100000000) SELECT CAST(sum(x) AS TEXT) AS item_id FROM n";
const GRAPHQL: &str = "query($after:String){items(first:1000,after:$after){nodes{id} pageInfo{hasNextPage endCursor}}}";

#[test]
fn missing_mode_and_ordinary_errors_have_request_local_classification() {
    let (_root, mut owner) = fixture();
    save(
        owner.store().unwrap(),
        "ok",
        definition(sql("SELECT 'a' AS item_id")),
    );
    save(
        owner.store().unwrap(),
        "rows",
        json!({"mode":"rows","selection":sql("SELECT 1")}),
    );
    owner
        .with_evaluation("default", &json!({}), 5000, 10000, |evaluation| {
            assert_eq!(
                selection_error(evaluation, json!({"query_id":"missing"})).0,
                "unknown query revision in this profile"
            );
            assert_eq!(
                evaluation.failure(),
                Some(EvaluationFailure::MissingSavedQuery)
            );
            assert_eq!(
                selection_error(evaluation, sql("SELECT absent FROM missing_table")).0,
                "SQL query failed: no such table: missing_table"
            );
            assert_eq!(evaluation.failure(), None);
            assert_eq!(
                selection_error(evaluation, json!({"query_id":"rows"})).0,
                "rule/projection requires a complete ID selection, not rows/document"
            );
            assert_eq!(
                evaluation.failure(),
                Some(EvaluationFailure::UnsupportedSelectionMode)
            );
            evaluation.get("ok")?;
            assert_eq!(evaluation.failure(), None);
            evaluation.select(&json!({"query_id":"ok"}))?;
            assert_eq!(evaluation.failure(), None);
            Ok(())
        })
        .unwrap();
}

#[test]
fn row_truncation_and_paged_id_limit_are_typed_without_partial_success() {
    let (_root, mut owner) = fixture();
    owner
        .with_evaluation("default", &json!({}), 5000, 10000, |evaluation| {
            let mut selection = sql(THREE);
            selection["max_ids"] = json!(2);
            assert_eq!(
                selection_error(evaluation, selection.clone()).0,
                "selection query was truncated; no mappings were changed"
            );
            assert_eq!(evaluation.failure(), Some(EvaluationFailure::IdBudget));
            selection["page_size"] = json!(1);
            assert_eq!(
                selection_error(evaluation, selection).0,
                "selection exceeds max_ids; no requirements or mappings were changed"
            );
            assert_eq!(evaluation.failure(), Some(EvaluationFailure::IdBudget));
            evaluation.select(&sql("SELECT 'ok' AS item_id"))?;
            assert_eq!(evaluation.failure(), None);
            Ok(())
        })
        .unwrap();
}

#[test]
fn caller_and_saved_definition_id_budgets_are_typed() {
    let (_root, mut owner) = fixture();
    save(owner.store().unwrap(), "three", definition(sql(THREE)));
    let mut limited = definition(sql(THREE));
    limited["max_ids"] = json!(2);
    save(owner.store().unwrap(), "limited", limited);
    for (id, maximum) in [("three", 2), ("limited", 100)] {
        owner
            .with_evaluation("default", &json!({}), 5000, maximum, |evaluation| {
                assert_eq!(
                    selection_error(evaluation, json!({"query_id":id})).0,
                    "complete query selection exceeds its ID budget"
                );
                assert_eq!(evaluation.failure(), Some(EvaluationFailure::IdBudget));
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn accounting_refusal_is_typed_and_success_clears_it() {
    let (_root, mut owner) = fixture();
    owner
        .with_evaluation("default", &json!({}), 5000, 100, |evaluation| {
            assert_eq!(
                evaluation
                    .account_value(&json!("x".repeat(4 * 1024 * 1024)))
                    .unwrap_err()
                    .0,
                "evaluation_result_byte_budget"
            );
            assert_eq!(evaluation.failure(), Some(EvaluationFailure::ByteBudget));
            evaluation.account_value(&json!(null))?;
            assert_eq!(evaluation.failure(), None);
            Ok(())
        })
        .unwrap();
}

#[test]
fn sql_output_truncation_reports_bytes_not_ids() {
    let (_root, mut owner) = fixture();
    let query = "WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<9) SELECT printf('%0900000d',x) AS item_id FROM n";
    owner
        .with_evaluation("default", &json!({}), 5000, 100, |evaluation| {
            assert_eq!(
                selection_error(evaluation, sql(query)).0,
                "selection query was truncated; no mappings were changed"
            );
            assert_eq!(evaluation.failure(), Some(EvaluationFailure::ByteBudget));
            Ok(())
        })
        .unwrap();
}

#[test]
fn saved_sql_shorter_deadline_is_typed_before_the_callers_deadline() {
    let (_root, mut owner) = fixture();
    let mut query = definition(sql(EXPENSIVE));
    query["timeout_ms"] = json!(20);
    save(owner.store().unwrap(), "short", query);
    owner
        .with_evaluation("default", &json!({}), 5000, 10000, |evaluation| {
            assert_eq!(
                selection_error(evaluation, json!({"query_id":"short"})).0,
                "SQL execution timed out; simplify the query or increase --timeout-ms"
            );
            assert_eq!(evaluation.failure(), Some(EvaluationFailure::Timeout));
            // The leaf deadline did not exhaust or shrink the caller's session.
            evaluation.select(&sql("SELECT 'still-live' AS item_id"))?;
            assert_eq!(evaluation.failure(), None);
            Ok(())
        })
        .unwrap();
}

#[test]
fn sql_cancellation_origin_is_typed_and_owner_survives() {
    let (_root, mut owner) = fixture();
    owner
        .with_evaluation("default", &json!({}), 5000, 10000, |evaluation| {
            let error = embedding::with_interrupt_check(
                || true,
                || Ok(selection_error(evaluation, sql(EXPENSIVE))),
            )?;
            assert_eq!(error.0, "SQL query failed: interrupted");
            assert_eq!(evaluation.failure(), Some(EvaluationFailure::Cancelled));
            evaluation.select(&sql("SELECT 'still-live' AS item_id"))?;
            assert_eq!(evaluation.failure(), None);
            Ok(())
        })
        .unwrap();
}

#[test]
fn graphql_cancellation_and_shorter_deadline_are_typed_and_owner_survives() {
    let (_root, mut owner) = fixture();
    let mut selection = json!({"language":"graphql","query":GRAPHQL,"timeout_ms":5000});
    owner
        .with_evaluation("default", &json!({}), 5000, 10000, |evaluation| {
            let error = embedding::with_interrupt_check(
                || true,
                || Ok(selection_error(evaluation, selection.clone())),
            )?;
            assert_eq!(
                error.0,
                "selection GraphQL failed: query execution interrupted"
            );
            assert_eq!(evaluation.failure(), Some(EvaluationFailure::Cancelled));
            selection["timeout_ms"] = json!(10);
            let error = embedding::with_interrupt_check(
                || {
                    std::thread::sleep(Duration::from_millis(20));
                    false
                },
                || Ok(selection_error(evaluation, selection.clone())),
            )?;
            assert_eq!(
                error.0,
                "selection GraphQL failed: query execution deadline exceeded"
            );
            assert_eq!(evaluation.failure(), Some(EvaluationFailure::Timeout));
            evaluation.select(&sql("SELECT 'still-live' AS item_id"))?;
            assert_eq!(evaluation.failure(), None);
            Ok(())
        })
        .unwrap();
}

#[test]
fn graphql_output_budget_is_typed_at_the_graphql_owner() {
    let (_root, mut owner) = fixture();
    let db = &owner.store().unwrap().db;
    db.execute_batch("BEGIN").unwrap();
    let padding = "x".repeat(9000);
    for n in 0..950 {
        db.execute(
            "INSERT INTO items(id,kind,metadata) VALUES (?,'movie','{}')",
            [format!("{n:04}{padding}")],
        )
        .unwrap();
    }
    db.execute_batch("COMMIT").unwrap();
    owner
        .with_evaluation("default", &json!({}), 10000, 10000, |evaluation| {
            let selection = json!({"language":"graphql","query":GRAPHQL,"timeout_ms":10000});
            assert_eq!(
                selection_error(evaluation, selection).0,
                "selection GraphQL failed: query output budget exceeded"
            );
            assert_eq!(evaluation.failure(), Some(EvaluationFailure::ByteBudget));
            Ok(())
        })
        .unwrap();
}
