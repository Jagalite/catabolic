# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""One explicit maintenance cycle, using the ordinary catalog safety boundaries."""

import math
import sqlite3
from collections import Counter

from .domain import CatabolicError, relative_path
from .filesystem import root_handle
from .item_workflow import STATUSES, SUMMARY_SQL
from .layouts import Layouts
from .manifest import Manifest
from .processing import OPERATIONS, Processing
from .reconcile import Reconciler
from .watching import stable_batches


def register(commands):
    command = commands.add_parser(
        "maintenance",
        help="run one scan, optional analysis, link sync and backlog report",
        description="Run one maintenance cycle and exit. Read catabolic docs maintenance first. "
        "Scans update inventory; configured layouts and links are reconciled. "
        "Removals are blocked by default. Curation decisions remain explicit.",
    )
    scope = command.add_mutually_exclusive_group()
    scope.add_argument("--catalog", default="global")
    scope.add_argument("--all-catalogs", action="store_true")
    scope.add_argument(
        "--inventory-only", action="store_true", help="skip output folders"
    )
    command.add_argument(
        "--exclude",
        action="append",
        default=[],
        help="exact source-relative path/subtree; repeat on every cycle",
    )
    command.add_argument(
        "--process",
        choices=OPERATIONS,
        help="optional analysis type for stable files; no rendering",
    )
    command.add_argument(
        "--settle",
        type=int,
        default=30,
        help="seconds a recorded file revision must be stable (default: 30)",
    )
    command.add_argument(
        "--batch",
        type=int,
        default=100,
        help="maximum analysis jobs to run (1..1000; default: 100)",
    )
    command.add_argument("--workers", type=int, default=2)
    command.add_argument(
        "--rules",
        action="store_true",
        help="evaluate enabled processing rules and enqueue a bounded batch",
    )
    command.add_argument("--rule-batch", type=int, default=100)
    command.add_argument(
        "--render-rules",
        type=int,
        default=0,
        metavar="N",
        help="also render at most N rule jobs; requires --rules",
    )
    command.add_argument(
        "--rule-max-new-bytes",
        type=int,
        help="maximum estimated bytes enqueued by rules this cycle",
    )
    command.add_argument(
        "--max-removals",
        type=int,
        default=0,
        help="maximum output removals (default: 0)",
    )
    command.add_argument("--max-removal-percent", type=float)
    command.add_argument(
        "--manifest",
        action="store_true",
        help="refresh manifests after healthy output verification",
    )
    command.add_argument(
        "--limit",
        type=int,
        default=100,
        help="maximum detail rows per stage; totals stay exact",
    )


def statistics(app, catalogs):
    """Aggregate in SQL, independently of presentation limits and CLI pagination."""
    store, profile = app.store, app.profile

    def count(sql, args=()):
        return store.db.execute(sql, args).fetchone()[0]

    statuses = {key: 0 for key in (*STATUSES, "needs_attention")}
    statuses.update(
        {
            row["status"]: row["n"]
            for row in store.rows(
                f"SELECT status,count(*) AS n FROM ({SUMMARY_SQL}) WHERE profile=? GROUP BY status",
                (profile,),
            )
        }
    )
    availability = {key: 0 for key in ("present", "missing", "unknown")}
    availability.update(
        {
            row["status"]: row["n"]
            for row in store.rows(
                "SELECT coalesce(o.status,'unknown') AS status,count(*) AS n FROM files f "
                "LEFT JOIN observations o ON o.file_id=f.id AND o.profile=? GROUP BY 1",
                (profile,),
            )
        }
    )
    uncataloged = (
        "NOT EXISTS (SELECT 1 FROM item_files a WHERE a.file_id=f.id AND a.active=1)"
    )
    return {
        "scope": {
            "items": "whole database; readiness in selected profile",
            "files": "whole database; availability in selected profile",
            "jobs_and_proposals": "selected profile",
            "output_catalogs": catalogs,
        },
        "items": {
            "total": sum(statuses.values()),
            "by_status": statuses,
            "incomplete": sum(
                statuses[key]
                for key in ("pending", "in_progress", "deferred", "needs_attention")
            ),
        },
        "files": {
            "total": sum(availability.values()),
            "by_availability": availability,
            "uncataloged": count("SELECT count(*) FROM files f WHERE " + uncataloged),
            "uncataloged_present": count(
                "SELECT count(*) FROM files f JOIN observations o ON o.file_id=f.id WHERE o.profile=? AND o.status='present' AND "
                + uncataloged,
                (profile,),
            ),
            "unmapped_present_by_catalog": {
                catalog: count(
                    "SELECT count(*) FROM files f JOIN observations o ON o.file_id=f.id WHERE o.profile=? AND o.status='present' AND NOT EXISTS (SELECT 1 FROM mappings m WHERE m.file_id=f.id AND m.catalog=? AND m.active=1)",
                    (profile, catalog),
                )
                for catalog in catalogs
            },
        },
        "jobs": {
            row["state"]: row["n"]
            for row in store.rows(
                "SELECT state,count(*) AS n FROM processing_jobs WHERE profile=? GROUP BY state",
                (profile,),
            )
        },
        "proposals": {
            row["state"]: row["n"]
            for row in store.rows(
                "SELECT state,count(*) AS n FROM proposals WHERE profile=? GROUP BY state",
                (profile,),
            )
        },
        "inspect": [
            "files --unidentified",
            "item list --curation-status pending",
            "item list --curation-status in_progress",
            "item list --curation-status deferred",
            "item list --curation-status needs_attention",
            "process list --state failed",
            "proposal list --state pending",
        ],
    }


def _analysis(app, scan, operation, settle, batch, workers):
    processor = Processing(app)
    selected = []
    queued = cached = waiting = pending = failures = error_count = 0
    errors = []
    for ids, unstable in stable_batches(app, scan, settle=settle):
        waiting += unstable
        if not ids:
            continue
        result = processor.enqueue(operation, file_ids=ids)
        queued += len(result["queued"])
        cached += len(result["cached"])
        error_count += len(result["errors"])
        errors.extend(result["errors"][: max(0, 100 - len(errors))])
        jobs = result["queued"] + result["cached"]
        if jobs:
            marks = ",".join("?" for _ in jobs)
            for row in app.store.rows(
                f"SELECT id,state FROM processing_jobs WHERE id IN ({marks}) ORDER BY created_at,id",
                tuple(jobs),
            ):
                if row["state"] in ("queued", "running"):
                    if len(selected) < batch:
                        selected.append(row["id"])
                    else:
                        pending += 1
                elif row["state"] != "complete":
                    failures += 1
    result = processor.run(
        workers=workers, limit=batch, job_ids=selected, _refresh=False
    )
    return {
        "operation": operation,
        "queued": queued,
        "cached": cached,
        "waiting_for_stability": waiting,
        "pending_beyond_batch": pending,
        "cached_unsuccessful_jobs": failures,
        "error_count": error_count,
        "errors": errors,
        "processing": result,
        "complete": result["complete"]
        and not (failures or error_count or pending or waiting),
        "safe_to_continue": result["complete"] and not (failures or error_count),
    }


class _Blocked(Exception):
    pass


def render_report(report):
    summary = report["summary"]
    items, files = summary["items"], summary["files"]
    lines = [
        "Maintenance cycle: " + ("complete" if report["complete"] else "incomplete"),
        f"Database: {report['database']} (profile: {report['profile']})",
        f"New inventory files: {report['new_files']}",
        f"Items: {items['total']} total; {items['incomplete']} incomplete",
        "  "
        + "; ".join(f"{key}: {value}" for key, value in items["by_status"].items()),
        f"Files: {files['total']} total; {files['uncataloged']} uncataloged ({files['uncataloged_present']} present)",
        "  "
        + "; ".join(
            f"{key}: {value}" for key, value in files["by_availability"].items()
        ),
    ]
    for label in ("jobs", "proposals"):
        lines.append(
            label.capitalize()
            + ": "
            + (
                "; ".join(
                    f"{key}: {value}" for key, value in sorted(summary[label].items())
                )
                or "none"
            )
        )
    for catalog, count in files["unmapped_present_by_catalog"].items():
        lines.append(f"Unmapped present files in {catalog}: {count}")
    outputs = summary["outputs"]
    lines.append(
        "Output verification: "
        + (
            "not run"
            if not outputs["checked"]
            else "healthy"
            if outputs["healthy"]
            else "unhealthy"
        )
    )
    for entry in outputs["catalogs"]:
        lines.append(
            f"  {entry['catalog']}: {entry['verified_links']} verified links; {entry['issue_count']} issues; {entry['retained_count']} retained"
        )
    lines.extend("Warning: " + warning for warning in report["warnings"])
    for stage in report["stages"]:
        if stage["stage"] == "rules":
            from .rule_estimates import human_bytes

            lines.append(
                "Rules: "
                + "; ".join(
                    f"{key}: {value}" for key, value in stage["backlog"].items()
                )
            )
            for volume in stage["space_by_device"]:
                lines.append(
                    f"  Device {volume['device']}: additional {human_bytes(volume['expected_bytes'])}; range {human_bytes(volume['low_bytes'])} to {human_bytes(volume['high_bytes'])}; {volume['unknown_count']} unknown; free {human_bytes(volume['available_bytes'])}"
                )
        if stage["stage"] == "scan":
            for scan in stage["scans"]:
                lines.extend(
                    f"  {scan['location']}: {error}" for error in scan["errors"]
                )
        if stage["stage"] == "planning":
            for layout in stage["layouts"]:
                lines.extend("  " + blocker["reason"] for blocker in layout["blockers"])
        if stage["stage"] == "processing":
            lines.append(
                f"Analysis: {stage['processing']['processed']} processed; {stage['waiting_for_stability']} settling; {stage['pending_beyond_batch']} beyond batch; {stage['cached_unsuccessful_jobs']} cached unsuccessful jobs"
            )
            lines.extend("  " + entry["error"] for entry in stage["errors"])
        if stage["stage"] == "preview":
            budget = stage["removal_budget"]
            lines.append(
                f"Planned output removals: {budget['removals']} ({budget['percent']:.1f}%); limit: {budget['max_removals']}"
            )
            lines.extend("  " + entry["reason"] for entry in stage["blockers"])
    lines.extend(
        f"Stopped at {entry['stage']}: {entry['message']}" for entry in report["errors"]
    )
    lines.append(
        "Cycle success and curation completion are separate. Inspect recorded backlog with:"
    )
    lines.extend("  catabolic " + command for command in summary["inspect"])
    lines.append(
        "Use --json for stage details; keep the same database and profile when inspecting."
    )
    return "\n".join(lines)


def run(
    app,
    *,
    catalog="global",
    inventory_only=False,
    exclude=None,
    process=None,
    settle=30,
    batch=100,
    workers=2,
    max_removals=0,
    max_removal_percent=None,
    manifest=False,
    limit=100,
    progress=None,
    rules=False,
    rule_batch=100,
    render_rules=0,
    rule_max_new_bytes=None,
):
    if app.store.lock_fd is None:
        raise CatabolicError("maintenance requires the catalog writer lock")
    for value, low, high, label in (
        (settle, 0, 86400, "settle"),
        (batch, 1, 1000, "batch"),
        (workers, 1, 16, "workers"),
        (limit, 1, 1000, "limit"),
        (rule_batch, 1, 1000, "rule batch"),
        (render_rules, 0, 1000, "render rules"),
    ):
        if type(value) is not int or not low <= value <= high:
            raise CatabolicError(f"{label} must be {low}..{high}")
    if not rules and (render_rules or rule_max_new_bytes is not None):
        raise CatabolicError("render-rules and rule-max-new-bytes require --rules")
    if rule_max_new_bytes is not None and (
        type(rule_max_new_bytes) is not int or rule_max_new_bytes < 0
    ):
        raise CatabolicError("rule max new bytes must be a nonnegative integer")
    if process is not None and process not in OPERATIONS:
        raise CatabolicError("unsupported processing operation")
    if type(max_removals) is not int or max_removals < 0:
        raise CatabolicError("max removals must be a nonnegative integer")
    if max_removal_percent is not None and (
        type(max_removal_percent) not in (int, float)
        or not math.isfinite(max_removal_percent)
        or not 0 <= max_removal_percent <= 100
    ):
        raise CatabolicError("max removal percent must be finite and between 0 and 100")
    if inventory_only and manifest:
        raise CatabolicError("--manifest requires output catalogs")
    exclude = [relative_path(path) for path in (exclude or [])]
    reconciler = Reconciler(app)
    catalogs = [] if inventory_only else reconciler.catalogs(catalog)
    report = {
        "report_version": 1,
        "database": str(app.store.path),
        "profile": app.profile,
        "catalogs": catalogs,
        "complete": False,
        "inventory_updated": False,
        "stages": [],
        "errors": [],
    }
    stage = "preflight"

    def record(name, result):
        # Reconciler APIs retain their normal complete plans internally. Bound
        # rendered details while preserving totals in the maintenance report.
        result = dict(result)
        if name == "verify":
            result["catalogs"] = [
                {
                    **entry,
                    "issue_count": len(entry["issues"]),
                    "issues_truncated": len(entry["issues"]) > limit,
                    "issues": entry["issues"][:limit],
                }
                for entry in result["catalogs"]
            ]
        for key in ("actions", "applied", "blockers"):
            if isinstance(result.get(key), list):
                result[key + "_count"] = len(result[key])
                if key in ("actions", "applied"):
                    result[key + "_by_kind"] = dict(
                        Counter(row["kind"] for row in result[key])
                    )
                result[key + "_truncated"] = len(result[key]) > limit
                result[key] = result[key][:limit]
        report["stages"].append({"stage": name, **result})
        if progress:
            progress(
                {
                    "stage": name,
                    "complete": result.get(
                        "complete", result.get("healthy", result.get("safe", True))
                    ),
                }
            )

    before = app.store.db.execute("SELECT count(*) FROM files").fetchone()[0]
    try:
        app.require_recovered()
        if app.store.rows(
            "SELECT 1 FROM processing_artifacts WHERE profile=? AND state IN ('planned','writing','validating','publishing') LIMIT 1",
            (app.profile,),
        ):
            raise CatabolicError(
                "pending artifact publication requires artifact recover"
            )
        sources = app.store.rows("SELECT id FROM locations ORDER BY id")
        if not sources:
            raise CatabolicError("no source locations configured")
        for kind, owners in (
            ("source", [row["id"] for row in sources]),
            ("output", catalogs),
        ):
            for owner in owners:
                with root_handle(app.binding(kind, owner)):
                    pass
        record(stage, {"complete": True, "sources": [row["id"] for row in sources]})
        stage = "scan"
        scan = app.scan(exclude=exclude)
        report["inventory_updated"] = scan["complete"]
        record(stage, scan)
        if not scan["complete"]:
            raise _Blocked("incomplete scan; processing and output changes skipped")
        analysis_complete = True
        if process:
            stage = "processing"
            analysis = _analysis(app, scan, process, settle, batch, workers)
            record(stage, analysis)
            analysis_complete = analysis["complete"]
            if not analysis["safe_to_continue"]:
                raise _Blocked("analysis failed; output changes skipped")
        if rules:
            from .rules import maintain

            stage = "rules"
            rule_report = maintain(
                app,
                batch=rule_batch,
                render_batch=render_rules,
                max_new_bytes=rule_max_new_bytes,
                limit=limit,
                scan_ids={s["scan_id"] for s in scan["scans"]},
            )
            record(stage, rule_report)
            analysis_complete = analysis_complete and rule_report["complete"]
            if not rule_report["safe_to_continue"]:
                raise _Blocked(
                    "processing rules have blockers or failures; output synchronization skipped"
                )
        if catalogs:
            stage = "planning"
            layout_reports = []
            try:
                with app.store.transaction():
                    layouts = Layouts(app)
                    for selected in catalogs:
                        owner = layouts._read("layout-catalog:" + selected)
                        if owner:
                            plan = layouts.run(
                                owner["layout"], selected, apply=True, limit=limit
                            )
                            layout_reports.append(plan)
                            if not plan["safe"]:
                                raise _Blocked(
                                    "layout has blockers; all staged mappings rolled back"
                                )
                    plan = reconciler.preview(
                        catalog,
                        max_removals=max_removals,
                        max_removal_percent=max_removal_percent,
                    )
                    record("preview", plan)
                    if not plan["safe"] or any(
                        action["kind"] == "blocked_source" for action in plan["actions"]
                    ):
                        raise _Blocked(
                            "unsafe output plan; all staged mappings rolled back"
                        )
            except BaseException:
                for entry in layout_reports:
                    entry["applied"] = False
                record(
                    stage,
                    {
                        "complete": False,
                        "layouts": layout_reports,
                        "mappings_committed": False,
                    },
                )
                raise
            record(
                stage,
                {
                    "complete": True,
                    "layouts": layout_reports,
                    "mappings_committed": True,
                },
            )
            stage = "sync"
            synced = reconciler.apply(
                catalog,
                max_removals=max_removals,
                max_removal_percent=max_removal_percent,
            )
            record(
                stage,
                {key: value for key, value in synced.items() if key != "verification"},
            )
            # apply already independently verifies its result; do not walk again.
            if "verification" in synced:
                record("verify", synced["verification"])
            if not synced["safe"] or not synced["healthy"]:
                raise _Blocked("output synchronization or verification incomplete")
            if manifest:
                stage = "manifest"
                for selected in catalogs:
                    with app.store.transaction():
                        exporter = Manifest(app)
                        exported = exporter.write(
                            exporter.build(selected), in_catalog=True, replace=True
                        )
                    record(stage, {"catalog": selected, **exported})
        report["complete"] = analysis_complete
    except (_Blocked, CatabolicError, OSError, sqlite3.Error, ValueError) as exc:
        report["errors"].append(
            {"stage": stage, "message": str(exc), "type": type(exc).__name__}
        )
    report["stopped_at"] = stage if report["errors"] else None
    report["new_files"] = (
        app.store.db.execute("SELECT count(*) FROM files").fetchone()[0] - before
    )
    report["summary"] = statistics(app, catalogs)
    rule_stage = next((s for s in report["stages"] if s["stage"] == "rules"), None)
    if rule_stage:
        report["summary"]["rules"] = {
            "backlog": rule_stage["backlog"],
            "space_by_device": rule_stage["space_by_device"],
        }
    report["warnings"] = list(
        dict.fromkeys(
            warning
            for entry in report["stages"]
            for warning in entry.get("warnings", [])
        )
    )
    verification = next(
        (entry for entry in reversed(report["stages"]) if entry["stage"] == "verify"),
        None,
    )
    report["summary"]["outputs"] = {
        "checked": verification is not None,
        "healthy": verification["healthy"] if verification else None,
        "catalogs": [
            {
                "catalog": entry["catalog"],
                "healthy": entry["healthy"],
                "verified_links": entry.get("verified_links", 0),
                "issue_count": entry["issue_count"],
                "retained_count": entry.get("retained_count", 0),
            }
            for entry in verification["catalogs"]
        ]
        if verification
        else [],
    }
    return report
