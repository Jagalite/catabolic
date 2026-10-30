// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Production plan comparison must ignore nonsemantic evidence and retain fences.
use catabolic_store::plans;
use serde_json::json;
#[test]
fn plan_comparison_retains_identity_and_ignores_probe_diagnostics() {
    let previous = json!({"kind":"fallback","complete":true,"value":{"decisions":[{"entry_id":"entry","file_id":"file","revision":"r1","state":"ready","evidence":{"byte_identity":"same","elapsed_ms":1}}]}});
    let baseline = plans::semantic(&previous).unwrap();
    let mut current = previous.clone();
    current["value"]["decisions"][0]["evidence"]["elapsed_ms"] = json!(99);
    assert_eq!(
        plans::compare(&baseline, &current).unwrap()["changed"],
        false
    );
    current["value"]["decisions"][0]["revision"] = json!("r2");
    assert_eq!(
        plans::compare(&baseline, &current).unwrap()["changed"],
        true
    );
    current["complete"] = json!(false);
    assert_eq!(
        plans::compare(&baseline, &current).unwrap_err().0,
        "incomplete_evaluation_has_no_baseline"
    );
}
#[test]
fn projection_and_rows_compare_only_persisted_semantic_values() {
    let previous = json!({"kind":"projection","complete":true,"value":{"desired":[{"entry_id":"e","path":"a","file_id":"f","evidence":{"output_digest":"one","probe_time":1}}],"removed":[],"safe":true,"elapsed":1}});
    let baseline = plans::semantic(&previous).unwrap();
    let mut later = previous.clone();
    later["value"]["desired"][0]["evidence"]["probe_time"] = json!(2);
    later["value"]["elapsed"] = json!(999);
    assert_eq!(plans::compare(&baseline, &later).unwrap()["changed"], false);
    later["value"]["desired"][0]["evidence"]["output_digest"] = json!("two");
    assert_eq!(plans::compare(&baseline, &later).unwrap()["changed"], true);
    let rows =
        json!({"kind":"rows","complete":true,"value":{"columns":["id"],"rows":[[1]],"timing":5}});
    assert_eq!(
        plans::semantic(&rows).unwrap(),
        json!({"kind":"rows","value":{"columns":["id"],"rows":[[1]]}})
    );
    let document = json!({"kind":"document","complete":true,"value":{"data":{"x":1},"metadata":2}});
    assert_eq!(
        plans::semantic(&document).unwrap(),
        json!({"kind":"document","value":{"x":1}})
    );
}
