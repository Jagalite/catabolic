// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Versioned, recorded-only size heuristics and retained-evidence calibration.
use crate::{Result, Store, migration::rows};
use catabolic_core::decode_json;
use serde_json::{Value, json};
pub fn estimate(
    recipe: &Value,
    observation: &Value,
    probe: &Value,
    assumptions: &Value,
) -> Result<Value> {
    Ok(catabolic_core::estimates::estimate(
        recipe,
        observation,
        probe,
        assumptions,
    )?)
}
pub fn total(values: &Value) -> Result<Value> {
    Ok(catabolic_core::estimates::total(values)?)
}
pub fn calibration(
    store: &Store,
    profile: &str,
    recipe: &Value,
    assumptions: &Value,
) -> Result<Value> {
    let mut report = json!({"version":1,"recipe_id":recipe["id"],"sample_count":0,"minimum_samples":3,"applied":false,"factor":null,"reason":"at least three distinct successful source files are required"});
    if assumptions.as_object().is_some_and(|o| !o.is_empty()) {
        report["reason"] = json!("explicit planning assumptions take precedence");
        return Ok(report);
    }
    let records = rows(
        &store.db,
        "WITH samples AS (SELECT j.file_id,j.snapshot,a.validation,a.size,a.id,a.created_at,a.rowid AS sample_order,row_number() OVER (PARTITION BY j.file_id ORDER BY a.created_at DESC,a.rowid DESC) AS rank FROM processing_jobs j JOIN processing_artifacts a ON a.job_id=j.id WHERE j.profile=? AND j.recipe_id=? AND j.state='complete' AND a.state='ready') SELECT * FROM samples WHERE rank=1 ORDER BY created_at DESC,sample_order DESC LIMIT 100",
        &[
            profile.to_owned().into(),
            recipe["id"].as_str().unwrap_or("").to_owned().into(),
        ],
    )?;
    let mut ratios = vec![];
    for row in records {
        let validation = decode_json(
            row["validation"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("{}"),
        )?;
        let baseline = estimate(
            &recipe["definition"],
            &decode_json(row["snapshot"].as_str().unwrap())?,
            &validation["input"],
            &json!({}),
        )?;
        if let (Some(baseline), Some(size)) = (
            baseline["expected_bytes"].as_f64().filter(|n| *n > 0.0),
            row["size"].as_f64().filter(|n| *n > 0.0),
        ) {
            ratios.push(size / baseline);
        }
    }
    report["sample_count"] = json!(ratios.len());
    if ratios.len() < 3 {
        return Ok(report);
    }
    ratios.sort_by(f64::total_cmp);
    let n = ratios.len();
    report["applied"] = json!(true);
    report["factor"] = json!(if n % 2 == 0 {
        (ratios[n / 2 - 1] + ratios[n / 2]) / 2.0
    } else {
        ratios[n / 2]
    });
    report["low_factor"] = json!(ratios[0]);
    report["high_factor"] = json!(ratios[n - 1]);
    report["reason"] = json!(
        "median actual / heuristic bytes for this recipe and profile; budget never decreases"
    );
    Ok(report)
}
pub fn calibrated(value: &Value, report: &Value) -> Result<Value> {
    Ok(catabolic_core::estimates::calibrated(value, report)?)
}
