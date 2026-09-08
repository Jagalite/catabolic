# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Agent-friendly saved processing rules and retroactive storage previews."""

import json

from .rules import Rules


def register(commands):
    rules = commands.add_parser(
        "rule", help="saved recipe rules, storage previews, and bounded backfills"
    ).add_subparsers(dest="operation", required=True)
    put = rules.add_parser(
        "put",
        help="save an immutable rule revision; disabled for maintenance by default",
    )
    put.add_argument("name")
    put.add_argument(
        "--recipe",
        "--operation",
        dest="recipe",
        required=True,
        help="immutable operation revision ID",
    )
    put.add_argument("--location", help="generated destination; render operations only")
    selection = put.add_mutually_exclusive_group(required=True)
    selection.add_argument("--selection", help="SQL/GraphQL selection JSON file, or -")
    selection.add_argument("--query", help="immutable saved query revision ID")
    put.add_argument(
        "--allow-derived",
        action="store_true",
        help="explicitly admit validated renditions as render inputs",
    )
    put.add_argument(
        "--estimate-video-kbps",
        type=int,
        help="planning assumption only; does not change encoder settings",
    )
    put.add_argument(
        "--required",
        action="store_true",
        help="require a current rendition for each matched input when applying the rule",
    )
    listing = rules.add_parser("list")
    listing.add_argument("--enabled", action="store_true")
    listing.add_argument("--limit", type=int, default=100)
    listing.add_argument("--after", default="")
    for op in ("show", "stats", "enable", "disable", "preview", "apply", "run"):
        command = rules.add_parser(op)
        command.add_argument("id", help="immutable rule revision ID")
        if op in ("preview", "apply", "run"):
            command.add_argument(
                "--limit",
                type=int,
                default=100,
                help="detail rows; estimates always cover the complete selection",
            )
        if op in ("apply", "run"):
            command.add_argument("--batch", type=int, default=1 if op == "run" else 100)
        if op == "run":
            command.add_argument(
                "--worker",
                help="execute external rule jobs with this fenced worker identity; one network step per selected job",
            )
        if op == "apply":
            command.add_argument(
                "--max-new-bytes",
                type=int,
                help="cap the selected batch's estimated additional bytes",
            )
            command.add_argument("--retry-failed", action="store_true")


def writable(args):
    return args.command == "rule" and args.operation in (
        "put",
        "enable",
        "disable",
        "apply",
        "run",
    )


def dispatch(app, args):
    from .cli import read_text

    rules = Rules(app)
    op = args.operation
    if op == "put":
        estimates = (
            {}
            if args.estimate_video_kbps is None
            else {"video_kbps": args.estimate_video_kbps}
        )
        return rules.put(
            args.name,
            args.recipe,
            args.location,
            {"query_id": args.query}
            if args.query
            else json.loads(read_text(args.selection, 256 * 1024)),
            estimates=estimates,
            required=args.required,
            allow_derived=args.allow_derived,
        )
    if op == "list":
        return rules.list(limit=args.limit, after=args.after, enabled=args.enabled)
    if op in ("enable", "disable"):
        return rules.enable(args.id, op == "enable")
    if op == "show":
        return rules.get(args.id)
    if op == "stats":
        return rules.statistics(args.id)
    if op == "preview":
        return rules.preview(args.id, limit=args.limit)
    if op == "apply":
        return rules.apply(
            args.id,
            batch=args.batch,
            limit=args.limit,
            max_new_bytes=args.max_new_bytes,
            retry_failed=args.retry_failed,
        )
    return rules.run(args.id, batch=args.batch, limit=args.limit)


def run_external(args):
    """Bounded CLI orchestration; every network step releases the writer lock."""
    from .app import Application
    from .curation import page_limit
    from .domain import CatabolicError
    from .operations import Operations
    from .processors import Processors, dispatch_job
    from .store import Store

    page_limit(args.batch)
    page_limit(args.limit)
    with Store(args.db, writable=True) as store:
        app = Application(store, args.profile)
        rules = Rules(app)
        rule = rules.get(args.id)
        operation = Operations(app).get(rule["recipe_id"])
        if operation["operation_kind"] != "external":
            raise CatabolicError("--worker is for external rule execution")
        plan = rules._plan(args.id)
        registered = {
            r["job_id"]
            for r in store.rows(
                "SELECT job_id FROM rule_processor_jobs WHERE rule_id=?", (args.id,)
            )
        }
        selected = list(
            dict.fromkeys(
                r["job_id"]
                for r in plan["matches"]
                if r["state"] in ("queued", "running") and r["job_id"] in registered
            )
        )[: args.batch]
    completed, errors, steps = [], [], []
    while selected:
        with Store(args.db, writable=True) as store:
            lease = Processors(Application(store, args.profile)).claim(
                operation["preset"], args.worker, resume=True, job_ids=selected
            )
        if not lease["claimed"]:
            steps.append(lease)
            break
        selected.remove(lease["id"])
        try:
            result = dispatch_job(
                args.db, args.profile, lease["id"], lease["lease_token"]
            )
            steps.append({"job_id": lease["id"], "result": result})
            with Store(args.db) as store:
                job = Processors(Application(store, args.profile)).job(lease["id"])
            if job["state"] == "complete":
                completed.append({"job_id": job["id"], "receipt_id": job["receipt_id"]})
        except (CatabolicError, OSError, ValueError) as exc:
            errors.append({"job_id": lease["id"], "error": str(exc)})
            break
    with Store(args.db) as store:
        after = Rules(Application(store, args.profile)).preview(
            args.id, limit=args.limit
        )
    return {
        "rule_id": args.id,
        "execution": {
            "completed": completed,
            "errors": errors,
            "steps": steps,
            "complete": not errors,
        },
        "after": after,
        "complete": not errors
        and after["matched_inputs"] == after["counts"]["satisfied"],
    }
