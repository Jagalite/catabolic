# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Rule planning/queueing adapter for the existing fenced processor protocol."""

from collections import Counter

from .domain import CatabolicError
from .outputs import RENDITIONS_SQL
from .processors import Processors
from .rendition_publication import ready
from .rule_estimates import total
from .selection import selected_associations


class ExternalRule:
    def __init__(self, rules):
        self.rules, self.app, self.store = rules, rules.app, rules.store
        self.processors = Processors(self.app)

    def plan(self, rule, operation, scan_ids=None):
        from .rules import STATES

        definition = operation["definition"]
        associations, selection = selected_associations(self.store, rule["selection"])
        pairs = sorted(
            {
                (a["file_id"], a["item_id"])
                for a in associations
                if a["role"] == "primary"
                or selection["entity"] in ("file_id", "association_id")
            }
        )
        estimate = {
            "expected_bytes": 0,
            "low_bytes": 0,
            "high_bytes": 0,
            "output_limit_bytes": 0,
            "exceeds_output_limit": False,
            "assumptions": [
                "No local output allocation. External storage and runtime are unknown; the processor owns destination policy."
            ],
        }
        rows = []
        for file_id, item_id in pairs:
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
                _, digest = self.processors.prepare(
                    operation["preset"],
                    file_id,
                    item_id,
                    definition["output_definition_id"],
                    definition["options"],
                    operation_id=operation["id"],
                )
                jobs = self.store.rows(
                    "SELECT id,state,receipt_id FROM processor_jobs WHERE profile=? AND processor=? AND digest=?",
                    (self.app.profile, operation["preset"], digest),
                )
                if jobs:
                    job = jobs[0]
                    row["job_id"] = job["id"]
                    row["state"] = {
                        "queued": "queued",
                        "leased": "running",
                        "submitted": "running",
                        "complete": "satisfied",
                    }.get(job["state"], "failed")
                    if job["state"] == "complete":
                        outputs = self.store.rows(
                            f"SELECT r.* FROM ({RENDITIONS_SQL}) r JOIN receipt_outputs ro ON ro.output_id=r.id WHERE ro.receipt_id=?",
                            (job["receipt_id"],),
                        )
                        if not outputs:
                            raise CatabolicError(
                                "completed processor job has no accepted outputs"
                            )
                        for output in outputs:
                            ready(self.app, output)
                if (
                    scan_ids is not None
                    and self.store.rows(
                        "SELECT scan_id FROM observations WHERE profile=? AND file_id=?",
                        (self.app.profile, file_id),
                    )[0]["scan_id"]
                    not in scan_ids
                    and row["state"] != "satisfied"
                ):
                    raise CatabolicError("input is outside this maintenance scan scope")
            except (CatabolicError, OSError) as exc:
                row.update(state="deferred", reason=str(exc))
            rows.append(row)
        counts = dict.fromkeys(STATES, 0)
        counts.update(Counter(r["state"] for r in rows))
        return {
            "rule": rule,
            "selection": selection,
            "complete": True,
            "matched_inputs": len(rows),
            "excluded_associations": len(associations) - len(rows),
            "counts": counts,
            "matches": rows,
            "calibration": None,
            "external_storage_bytes": None,
            "space": {
                **total(r["estimate"] for r in rows),
                "already_satisfied_bytes": 0,
                "available_bytes": None,
                "destination": None,
                "device": None,
                "reserve_bytes": 0,
                "fits_planning_estimate": None,
                "note": estimate["assumptions"][0],
            },
        }

    def attach(self, rule, row, identifier):
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO rule_processor_jobs(rule_id,job_id,file_id,item_id) VALUES (?,?,?,?) ON CONFLICT DO NOTHING",
                (rule["id"], identifier, row["file_id"], row["item_id"]),
            )
            if rule["required"]:
                old = db.execute(
                    "SELECT id,processor_job_id FROM rule_requirements WHERE rule_id=? AND file_id=? AND item_id=?",
                    (rule["id"], row["file_id"], row["item_id"]),
                ).fetchone()
                if old and old["processor_job_id"] != identifier:
                    db.execute(
                        "UPDATE rule_requirements SET processor_job_id=? WHERE id=?",
                        (identifier, old["id"]),
                    )
                    from .item_workflow import ItemWorkflow

                    workflow = ItemWorkflow(self.app)
                    status, revision = workflow._revision(db, row["item_id"], None)
                    workflow._append(
                        db,
                        row["item_id"],
                        "requirement",
                        "Rule requirement now tracks a fenced external job.",
                        "rule:" + rule["id"],
                        {
                            "requirement_id": old["id"],
                            "previous_processor_job_id": old["processor_job_id"],
                            "processor_job_id": identifier,
                        },
                        status,
                        revision,
                    )

    def apply(self, plan, operation, batch, limit, retry, max_bytes):
        definition = operation["definition"]
        candidates = [
            r
            for r in plan["matches"]
            if r["state"] == "missing" or retry and r["state"] == "failed"
        ]
        blockers = (
            [
                "External storage is unknown; a local byte budget cannot authorize this external batch."
            ]
            if candidates and max_bytes is not None
            else []
        )
        result = {
            **self.rules._bounded(plan, limit),
            "queued": [],
            "adopted": 0,
            "errors": [],
            "blockers": blockers,
            "safe": not blockers,
            "complete": False,
            "remaining_to_enqueue": len(candidates),
            "batch_space": total([]),
        }
        if blockers:
            return result
        self.rules._requirements(plan)
        for row in plan["matches"]:
            if row["job_id"] and row["state"] in ("queued", "running", "satisfied"):
                self.attach(plan["rule"], row, row["job_id"])
                result["adopted"] += 1
        for row in candidates[:batch]:
            try:
                job = (
                    self.processors.retry(row["job_id"])
                    if row["state"] == "failed"
                    else self.processors.enqueue(
                        operation["preset"],
                        row["file_id"],
                        row["item_id"],
                        definition["output_definition_id"],
                        definition["options"],
                        operation_id=operation["id"],
                    )
                )
                self.attach(plan["rule"], row, job["id"])
                result["queued"].append(
                    {
                        "job_id": job["id"],
                        "executor": "processor",
                        "state": job["state"],
                    }
                )
            except (CatabolicError, OSError) as exc:
                result["errors"].append(
                    {"file_id": row["file_id"], "message": str(exc)}
                )
                break
        result["remaining_to_enqueue"] -= len(result["queued"])
        result["complete"] = (
            not result["errors"]
            and result["remaining_to_enqueue"] == 0
            and not plan["counts"]["deferred"]
            and (retry or not plan["counts"]["failed"])
        )
        return result
