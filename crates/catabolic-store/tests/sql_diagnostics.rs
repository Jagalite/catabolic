// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Frozen refusal diagnostics, ordinary SQL failures, and request-local state.
use catabolic_store::{Store, migration, sql};
use serde_json::json;

#[test]
fn refusal_diagnostics_remain_distinct_on_a_reused_store() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("sql-diagnostics.sqlite3");
    migration::initialize(&path).unwrap();
    let before = std::fs::read(&path).unwrap();
    let store = Store::open(&path, false, false).unwrap();
    let http = sql::Options {
        http: true,
        ..Default::default()
    };
    for (table, column) in [
        ("api_tokens", "api_tokens.id"),
        ("execution_claims", "execution_claims.job_id"),
        ("watchers", "watchers.profile"),
        ("sqlite_schema", "sqlite_master.type"),
    ] {
        for (query, expected) in [
            (
                format!("SELECT * FROM {table}"),
                format!("SQL query failed: access to {column} is prohibited"),
            ),
            (
                format!("SELECT count(*) FROM {table}"),
                format!("read-only SQL rejected an operation or function: {table}"),
            ),
        ] {
            assert_eq!(
                sql::execute_store(&store, Some(&query), &http)
                    .unwrap_err()
                    .0,
                expected,
                "{query}"
            );
            // The recorded denial must not classify a later request's parse,
            // resolution, or evaluation failure as an authorization rejection.
            for (ordinary, expected) in [
                ("SELECT FROM", "near \"FROM\": syntax error"),
                ("SELECT absent FROM items", "no such column: absent"),
                ("SELECT * FROM absent", "no such table: absent"),
                ("SELECT json_extract('not json', '$.x')", "malformed JSON"),
            ] {
                assert_eq!(
                    sql::execute_store(&store, Some(ordinary), &http)
                        .unwrap_err()
                        .0,
                    format!("SQL query failed: {expected}"),
                    "{ordinary} after {query}"
                );
            }
            assert_eq!(
                sql::execute_store(&store, Some("SELECT 1"), &http).unwrap()["rows"],
                json!([[1]])
            );
        }
        // HTTP privacy does not make the same operator read unavailable.
        let query = format!("SELECT * FROM {table}");
        assert!(sql::execute_store(&store, Some(&query), &sql::Options::default()).is_ok());
    }
    assert_eq!(
        sql::execute_store(&store, Some("SELECT * FROM schema_migrations"), &http).unwrap()["row_count"],
        32
    );
    assert_eq!(before, std::fs::read(&path).unwrap());
}
