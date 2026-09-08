# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Bounded recorded statistics and durable publication snapshots; no live reads."""

import json
import time
from collections import Counter
from contextlib import nullcontext
from datetime import datetime, timezone

from .domain import CatabolicError
from .store import encode

NAMESPACE = "catabolic:statistics"
MAX_RECORDS = 100000


def now():
    return datetime.now(timezone.utc).isoformat()


def key(app, kind, identifier):
    return "statistics:" + encode([app.profile, kind, identifier])


def read(app, kind, identifier):
    rows = app.store.rows(
        "SELECT value FROM meta WHERE key=?", (key(app, kind, identifier),)
    )
    return json.loads(rows[0]["value"]) if rows else None


def save(app, kind, identifier, value):
    with (
        nullcontext(app.store.db)
        if app.store.db.in_transaction
        else app.store.transaction()
    ) as db:
        db.execute(
            "INSERT INTO meta(key,value) VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            (key(app, kind, identifier), encode(value)),
        )


def file_totals(app, ids):
    ids = sorted(set(ids))
    if len(ids) > MAX_RECORDS:
        raise CatabolicError("statistics exceed 100000 files")
    known_bytes, unknown_sizes, observed = 0, 0, []
    availability = Counter()
    for start in range(0, len(ids), 500):
        batch = ids[start : start + 500]
        rows = app.store.rows(
            f"""SELECT o.size,coalesce(o.status,'unknown') AS status,s.finished_at
            FROM files f LEFT JOIN observations o ON o.file_id=f.id AND o.profile=?
            LEFT JOIN scans s ON s.id=o.scan_id WHERE f.id IN ({",".join("?" for _ in batch)})""",
            (app.profile, *batch),
        )
        for row in rows:
            availability[row["status"]] += 1
            if row["size"] is None:
                unknown_sizes += 1
            else:
                known_bytes += int(row["size"])
            if row["finished_at"]:
                observed.append(row["finished_at"])
    return {
        "files": len(ids),
        "known_referenced_bytes": str(known_bytes),
        "unknown_size_files": unknown_sizes,
        "availability": {
            state: availability[state] for state in ("present", "missing", "unknown")
        },
        "oldest_observation_at": min(observed) if observed else None,
        "newest_observation_at": max(observed) if observed else None,
        "unobserved_files": len(ids) - len(observed),
        "byte_scope": "sum once per file ID, including recorded missing files; not unique content, physical allocation or symlink storage",
    }


def selection(app, associations, report):
    return {
        "scope": "selected active associations before copy and rendition filtering",
        "query": report,
        "items": len({r["item_id"] for r in associations}),
        "associations": len(associations),
        **file_totals(app, (r["file_id"] for r in associations)),
    }


def rule_evaluation(app, plan):
    reasons = Counter(
        row.get("reason", "unspecified")
        for row in plan["matches"]
        if row["state"] == "deferred"
    )
    save(
        app,
        "rule",
        plan["rule"]["id"],
        {
            "evaluated_at": now(),
            "scope": "last explicit rule evaluation; full evaluated rule selection at evaluated_at, not live satisfaction",
            "matched_inputs": plan["matched_inputs"],
            "counts": plan["counts"],
            "deferred_reasons": [
                {"reason": reason, "count": count}
                for reason, count in reasons.most_common(100)
            ],
            "deferred_reasons_truncated": len(reasons) > 100,
        },
    )


def recorded(app, catalog):
    rows = app.store.rows(
        "SELECT file_id,item_id FROM mappings WHERE catalog=? AND active=1 LIMIT ?",
        (catalog, MAX_RECORDS + 1),
    )
    if len(rows) > MAX_RECORDS:
        raise CatabolicError("statistics exceed 100000 mappings")
    owner = app.store.rows(
        "SELECT value FROM meta WHERE key=?", ("layout-catalog:" + catalog,)
    )
    owner = json.loads(owner[0]["value"]) if owner else None
    result = {
        "as_of": "enclosing execution started_at or manifest generated_at",
        "scope": {"profile": app.profile, "catalog": catalog},
        "desired_entries": len(rows),
        "mapped_items": len({r["item_id"] for r in rows}),
        "selection": file_totals(app, (r["file_id"] for r in rows)),
        "last_query_evaluation": read(app, "selection", catalog),
        "layout_definition_sha256": owner["definition_sha256"] if owner else None,
        "recorded_links": app.store.rows(
            "SELECT count(*) AS n FROM owned_links WHERE profile=? AND catalog=?",
            (app.profile, catalog),
        )[0]["n"],
        "recorded_hardlinks": app.store.rows(
            "SELECT count(*) AS n FROM owned_hardlinks WHERE profile=? AND catalog=?",
            (app.profile, catalog),
        )[0]["n"]
        if app.store.schema_version >= 10
        else None,
        "rules": [],
    }
    if app.store.schema_version < 16:
        return result
    rules = app.store.rows(
        """SELECT DISTINCT r.id,r.name,r.revision,r.query_id FROM processing_rules r
        WHERE r.profile=? AND (
          r.query_id IN (SELECT query_id FROM projection_bindings WHERE profile=? AND catalog=?)
          OR r.id IN (SELECT json_extract(definition,'$.rule_id') FROM rendition_policies WHERE profile=? AND catalog=?)
          OR EXISTS (SELECT 1 FROM rule_jobs j JOIN mappings m ON m.file_id=j.file_id OR m.item_id=j.item_id WHERE j.rule_id=r.id AND m.catalog=? AND m.active=1)
          OR EXISTS (SELECT 1 FROM rule_processor_jobs j JOIN mappings m ON m.file_id=j.file_id OR m.item_id=j.item_id WHERE j.rule_id=r.id AND m.catalog=? AND m.active=1)
        ) ORDER BY r.id LIMIT 1001""",
        (app.profile, app.profile, catalog, app.profile, catalog, catalog, catalog),
    )
    if len(rules) > 1000:
        raise CatabolicError("statistics exceed 1000 associated rules")
    for rule in rules:
        jobs = {}
        last_activity = {}
        for label, link, table in (
            ("local", "rule_jobs", "processing_jobs"),
            ("external", "rule_processor_jobs", "processor_jobs"),
        ):
            jobs[label] = {
                r["state"]: r["n"]
                for r in app.store.rows(
                    f"SELECT j.state,count(*) AS n FROM {link} r JOIN {table} j ON j.id=r.job_id WHERE r.rule_id=? GROUP BY j.state",
                    (rule["id"],),
                )
            }
            field = "finished_at" if label == "local" else "updated_at"
            last_activity[label] = app.store.rows(
                f"SELECT max(j.{field}) AS at FROM {link} r JOIN {table} j ON j.id=r.job_id WHERE r.rule_id=?",
                (rule["id"],),
            )[0]["at"]
        result["rules"].append(
            {
                **rule,
                "scope": "whole rule revision associated by projection query, rendition policy, or jobs for mapped files/items",
                "last_evaluation": read(app, "rule", rule["id"]),
                "recorded_job_states": jobs,
                "latest_local_job_finished_at": last_activity["local"],
                "latest_external_job_updated_at": last_activity["external"],
                "job_scope": "all attached historical jobs; completion does not imply current rule satisfaction",
            }
        )
    return result


def exported(app, catalog):
    return {
        "version": 1,
        "current_recorded": recorded(app, catalog),
        "last_execution": read(app, "execution", catalog),
        "filesystem_verified_at_export": False,
    }


def begin_execution(app, catalogs, actions):
    snapshots = {}
    with app.store.transaction():
        for catalog in catalogs:
            snapshot = {
                "started_at": now(),
                "finished_at": None,
                "elapsed_seconds": None,
                "status": "completion_unknown",
                "planned_actions": dict(
                    Counter(a["kind"] for a in actions if a["catalog"] == catalog)
                ),
                "applied_actions": None,
                "verification": None,
                "inputs": recorded(app, catalog),
            }
            save(app, "execution", catalog, snapshot)
            snapshots[catalog] = snapshot
    return time.monotonic(), snapshots


def finish_execution(app, started, snapshots, applied, verification):
    with app.store.transaction():
        for report in verification["catalogs"]:
            catalog = report["catalog"]
            snapshot = snapshots[catalog]
            snapshot.update(
                finished_at=now(),
                elapsed_seconds=max(0, time.monotonic() - started),
                status="complete" if report["healthy"] else "verification_failed",
                applied_actions=dict(
                    Counter(a["kind"] for a in applied if a["catalog"] == catalog)
                ),
                verification={
                    "verified_at": now(),
                    "healthy": report["healthy"],
                    "verified_links": report.get("verified_links"),
                    "issue_count": len(report["issues"]),
                },
            )
            save(app, "execution", catalog, snapshot)
