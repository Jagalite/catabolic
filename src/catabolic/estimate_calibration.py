# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Read-only recipe calibration from retained successful local render evidence."""

import json
import math
import statistics

from .rule_estimates import estimate


def calibration(store, profile, recipe, assumptions):
    report = {
        "version": 1,
        "recipe_id": recipe["id"],
        "sample_count": 0,
        "minimum_samples": 3,
        "applied": False,
        "factor": None,
        "reason": "at least three distinct successful source files are required",
    }
    if assumptions:
        return {**report, "reason": "explicit planning assumptions take precedence"}
    # Rank before limiting so retries of one source cannot dominate the sample.
    rows = store.rows(
        """WITH samples AS (
          SELECT j.file_id,j.snapshot,a.validation,a.size,a.id,a.created_at,a.rowid AS sample_order,
          row_number() OVER (PARTITION BY j.file_id ORDER BY a.created_at DESC,a.rowid DESC) AS rank
          FROM processing_jobs j JOIN processing_artifacts a ON a.job_id=j.id
          WHERE j.profile=? AND j.recipe_id=? AND j.state='complete' AND a.state='ready'
        ) SELECT * FROM samples WHERE rank=1 ORDER BY created_at DESC,sample_order DESC LIMIT 100""",
        (profile, recipe["id"]),
    )
    ratios = []
    for row in rows:
        validation = json.loads(row["validation"] or "{}")
        baseline = estimate(
            recipe["definition"],
            json.loads(row["snapshot"]),
            validation.get("input"),
            {},
        )["expected_bytes"]
        if baseline and row["size"] and row["size"] > 0:
            ratios.append(row["size"] / baseline)
    report["sample_count"] = len(ratios)
    if len(ratios) < 3:
        return report
    return {
        **report,
        "applied": True,
        "factor": statistics.median(ratios),
        "low_factor": min(ratios),
        "high_factor": max(ratios),
        "reason": "median actual / heuristic bytes for this recipe and profile; budget never decreases",
    }


def calibrated(value, report):
    if not report["applied"] or value["expected_bytes"] is None:
        return value
    expected = max(1, math.ceil(value["expected_bytes"] * report["factor"]))
    high = max(
        value["high_bytes"],
        math.ceil(value["expected_bytes"] * report["high_factor"] * 1.25),
        expected,
    )
    return {
        **value,
        "estimator_version": 2,
        "expected_bytes": expected,
        "low_bytes": min(
            value["low_bytes"],
            math.floor(value["expected_bytes"] * report["low_factor"]),
        ),
        "high_bytes": high,
        "exceeds_output_limit": high > value["output_limit_bytes"],
        "method": value["method"] + "_measured",
        "calibration_samples": report["sample_count"],
        "assumptions": [
            *value["assumptions"],
            "Historical recipe measurements adjust expectation; original high budget is retained and may increase.",
        ],
    }
