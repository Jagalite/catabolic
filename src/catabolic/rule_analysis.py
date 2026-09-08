# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Analysis planning adapter. Jobs, attempts and evidence belong to Processing."""

import hashlib
import os
from collections import Counter

from .curation import occurrence
from .domain import CatabolicError
from .processing import current_fact, operation_config
from .rule_estimates import total
from .selection import select_ids, selected_associations
from .source_access import validated_source
from .store import encode


def plan(rules, rule, operation, scan_ids=None):
    store, profile = rules.store, rules.profile
    entity, ids, selection = select_ids(store, rule["selection"])
    if entity == "file_id":
        pairs = []
        for file_id in sorted(ids):
            if not store.rows("SELECT 1 FROM files WHERE id=?", (file_id,)):
                raise CatabolicError("selection returned an unknown file")
            items = store.rows(
                "SELECT DISTINCT item_id FROM item_files WHERE file_id=? AND active=1 ORDER BY item_id",
                (file_id,),
            )
            pairs.extend((file_id, r["item_id"]) for r in items)
            if not items:
                pairs.append((file_id, None))
            if len(pairs) > 100000:
                raise CatabolicError("analysis selection exceeds 100000 inputs")
    else:
        associations, selection = selected_associations(store, rule["selection"])
        pairs = sorted({(r["file_id"], r["item_id"]) for r in associations})
    rows = []
    config = {
        **operation_config(operation["preset"], operation["definition"]["options"]),
        "operation_id": operation["id"],
    }
    estimate = {
        "expected_bytes": 0,
        "low_bytes": 0,
        "high_bytes": 0,
        "output_limit_bytes": 0,
        "exceeds_output_limit": False,
        "assumptions": [
            "Records catalog evidence; no generated file. Catalog database growth and runtime are not estimated."
        ],
    }
    for file_id, item_id in pairs:
        if rule["required"] and item_id is None:
            raise CatabolicError("required item work needs an item association")
        row = {
            "file_id": file_id,
            "item_id": item_id,
            "job_id": None,
            "state": "missing",
            "reason": None,
            "existing_bytes": 0,
            "estimate": estimate,
        }
        try:
            observed = occurrence(store, profile, file_id)
            with validated_source(observed) as fd:
                observed["ctime_ns"] = os.fstat(fd).st_ctime_ns
            key = hashlib.sha256(
                encode([observed, operation["preset"], config]).encode()
            ).hexdigest()
            jobs = store.rows(
                "SELECT id,state FROM processing_jobs WHERE profile=? AND file_id=? AND operation=? AND cache_key=? ORDER BY created_at DESC,rowid DESC LIMIT 1",
                (profile, file_id, operation["preset"], key),
            )
            if jobs:
                job = jobs[0]
                row["job_id"] = job["id"]
                if job["state"] in ("queued", "running"):
                    row["state"] = job["state"]
                elif job["state"] == "complete":
                    fact = current_fact(store, profile, file_id, operation["preset"])
                    row["state"] = (
                        "satisfied"
                        if fact and fact["current"] and fact["snapshot"] == observed
                        else "stale"
                    )
                else:
                    row.update(
                        state="failed",
                        reason="Prior attempt did not succeed; explicit retry is required.",
                    )
            if (
                scan_ids is not None
                and store.rows(
                    "SELECT scan_id FROM observations WHERE profile=? AND file_id=?",
                    (profile, file_id),
                )[0]["scan_id"]
                not in scan_ids
                and row["state"] != "satisfied"
            ):
                raise CatabolicError("input is outside this maintenance scan scope")
        except (CatabolicError, OSError) as exc:
            row.update(state="deferred", reason=str(exc))
        rows.append(row)
    from .rules import STATES

    counts = dict.fromkeys(STATES, 0)
    counts.update(Counter(r["state"] for r in rows))
    return {
        "rule": rule,
        "selection": selection,
        "complete": True,
        "matched_inputs": len(rows),
        "excluded_associations": 0,
        "counts": counts,
        "matches": rows,
        "calibration": None,
        "space": {
            **total(r["estimate"] for r in rows if r["state"] != "satisfied"),
            "already_satisfied_bytes": 0,
            "available_bytes": None,
            "destination": None,
            "device": None,
            "reserve_bytes": 0,
            "fits_planning_estimate": True,
            "note": estimate["assumptions"][0],
        },
    }
