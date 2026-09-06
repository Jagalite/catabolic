# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""CLI surface for curation and optional processing capabilities."""

import json

from .curation import Curation
from .processing import OPERATIONS, Processing

COMMANDS = {
    "proposal",
    "sidecar",
    "expected",
    "copies",
    "process",
    "content",
    "refresh",
    "identify",
}


def paging(parser, integer=False):
    parser.add_argument("--limit", type=int, default=100)
    parser.add_argument(
        "--after", type=int if integer else str, default=0 if integer else ""
    )


def retries(parser):
    parser.add_argument(
        "--retry-transient",
        type=int,
        default=0,
        help="0 disables automatic retry; 1..10 additional attempts per job",
    )
    parser.add_argument(
        "--retry-delay",
        type=int,
        default=30,
        help="initial retry delay in seconds, doubles up to one hour",
    )


def register(commands):
    watch = commands.add_parser(
        "watch", help="periodically scan and enqueue stable files; no link sync"
    )
    watch.add_argument("--kind", choices=OPERATIONS, default="probe")
    watch.add_argument("--location")
    watch.add_argument("--settle", type=int, default=30)
    watch.add_argument("--interval", type=int, default=30)
    watch.add_argument(
        "--cycles", type=int, default=1, help="0 repeats until interrupted"
    )
    watch.add_argument("--workers", type=int, default=2)
    watch.add_argument("--batch", type=int, default=1000)
    retries(watch)

    proposals = commands.add_parser(
        "proposal", help="persist, inspect and decide identification proposals"
    ).add_subparsers(dest="operation", required=True)
    put = proposals.add_parser("put")
    put.add_argument("--file-id", required=True)
    put.add_argument("--file", required=True, help="JSON payload file or -")
    put.add_argument("--source", default="manual")
    put.add_argument("--evidence", default="{}")
    listing = proposals.add_parser("list")
    paging(listing)
    listing.add_argument("--state", choices=("pending", "accepted", "rejected"))
    proposals.add_parser("show").add_argument("id")
    for op in ("accept", "reject"):
        cmd = proposals.add_parser(op)
        cmd.add_argument("id")
        cmd.add_argument("--actor", required=True)
    paging(proposals.add_parser("history"), integer=True)
    sidecar = commands.add_parser(
        "sidecar", help="discover sidecar association proposals"
    )
    paging(sidecar)
    sidecar.add_argument("--location")
    sidecar.add_argument(
        "--apply",
        action="store_true",
        help="save candidates as proposals; does not accept them",
    )
    expected = commands.add_parser(
        "expected", help="versioned expected members and completeness reports"
    ).add_subparsers(dest="operation", required=True)
    put = expected.add_parser("put")
    put.add_argument("--collection", required=True)
    put.add_argument("--file", required=True)
    report = expected.add_parser("report")
    report.add_argument("id")
    paging(report, integer=True)
    copies = commands.add_parser(
        "copies", help="explicit per-catalog copy preferences"
    ).add_subparsers(dest="operation", required=True)
    for op in ("put", "show", "plan"):
        cmd = copies.add_parser(op)
        cmd.add_argument("--catalog", default="global")
        if op == "put":
            cmd.add_argument("--file", required=True)
        if op == "plan":
            cmd.add_argument("--limit", type=int, default=10000)
    process = commands.add_parser(
        "process",
        help="optional media jobs with bounded workers and resumable progress",
    ).add_subparsers(dest="operation", required=True)
    enqueue = process.add_parser("enqueue")
    enqueue.add_argument("kind", choices=OPERATIONS)
    enqueue.add_argument("--file-id", action="append", dest="file_ids")
    enqueue.add_argument("--location")
    enqueue.add_argument("--selection", help="saved SQL/GraphQL selection JSON file")
    enqueue.add_argument("--refresh", action="store_true")
    enqueue.add_argument("--options", default="{}")
    paging(enqueue)
    run = process.add_parser("run")
    retries(run)
    run.add_argument("--workers", type=int, default=2)
    run.add_argument("--per-device", type=int, default=1)
    run.add_argument(
        "--storage-groups",
        default="{}",
        help="JSON source-to-storage-group mapping for shared NAS limits",
    )
    run.add_argument("--limit", type=int, default=100)
    listing = process.add_parser("list")
    paging(listing)
    listing.add_argument("--state")
    listing.add_argument("--file-id")
    process.add_parser("facts").add_argument("file_id")
    process.add_parser("show").add_argument("id")
    for op in ("cancel", "retry"):
        process.add_parser(op).add_argument("id")
    history = process.add_parser("attempts")
    history.add_argument("id")
    paging(history, integer=True)
    content = commands.add_parser(
        "content", help="checksum groups and indexed content search"
    ).add_subparsers(dest="operation", required=True)
    paging(content.add_parser("duplicates"))
    search = content.add_parser("search")
    search.add_argument("text")
    paging(search, integer=True)
    refresh = commands.add_parser(
        "refresh", help="explicit media-server refresh delivery"
    ).add_subparsers(dest="operation", required=True)
    put = refresh.add_parser("configure")
    put.add_argument("--catalog", default="global")
    put.add_argument("--endpoint", required=True)
    put.add_argument("--credential-env", default="JELLYFIN_TOKEN")
    paging(refresh.add_parser("list"), integer=True)
    refresh.add_parser("run").add_argument("--limit", type=int, default=100)
    refresh.add_parser("retry").add_argument("id", type=int)
    identify = commands.add_parser(
        "identify", help="retrieve TMDB movie candidates; acceptance is separate"
    )
    identify.add_argument("--file-id", required=True)
    identify.add_argument("--query", required=True)
    identify.add_argument("--year", type=int)
    identify.add_argument("--credential-env", default="TMDB_TOKEN")
    identify.add_argument(
        "--apply", action="store_true", help="save candidates as proposals"
    )


def writable(args):
    cmd = args.command
    op = getattr(args, "operation", None)
    return (
        cmd in ("sidecar", "identify")
        and args.apply
        or cmd in COMMANDS
        and op
        in ("put", "accept", "reject", "enqueue", "run", "cancel", "retry", "configure")
    )


def dispatch(app, args):
    from .cli import read_text
    from .copy_selection import CopySelection
    from .domain import CatabolicError
    from .network_adapters import Refresh, tmdb_candidates

    def definition():
        return json.loads(read_text(args.file, 256 * 1024))

    c = Curation(app)
    p = Processing(app)
    cmd = args.command
    op = getattr(args, "operation", None)
    if cmd == "proposal":
        if op == "put":
            return c.put(
                args.file_id,
                definition(),
                source=args.source,
                evidence=json.loads(args.evidence),
            )
        if op == "show":
            return c.get(args.id)
        if op in ("accept", "reject"):
            return c.decide(args.id, accept=op == "accept", actor=args.actor)
        if op == "history":
            return c.history(limit=args.limit, after=args.after)
        return c.list(state=args.state, limit=args.limit, after=args.after)
    if cmd == "sidecar":
        return c.sidecars(
            location=args.location, limit=args.limit, after=args.after, apply=args.apply
        )
    if cmd == "expected":
        return (
            c.expected_put(args.collection, definition())
            if op == "put"
            else c.completeness(args.id, limit=args.limit, after=args.after)
        )
    if cmd == "copies":
        selection = CopySelection(app)
        if op == "put":
            return selection.put(args.catalog, definition())
        if op == "show":
            return {"catalog": args.catalog, "definition": selection.get(args.catalog)}
        return selection.plan(args.catalog, limit=args.limit)
    if cmd == "process":
        if op == "enqueue":
            if args.selection:
                if args.file_ids or args.location or args.after:
                    raise CatabolicError(
                        "selection cannot be combined with file IDs, location or after"
                    )
                from .selection import select_ids

                selection = json.loads(read_text(args.selection, 1024 * 1024))
                if (
                    not isinstance(selection, dict)
                    or selection.get("profile", "default") != app.profile
                ):
                    raise CatabolicError(
                        "selection profile must match the processing profile"
                    )
                selected = select_ids(app.store, selection)
                # select_ids returns an entity, bounded IDs, and the query report.
                entity, ids, _ = selected
                if entity != "file_id":
                    raise CatabolicError("processing selection must return file_id")
                result = {
                    "queued": [],
                    "cached": [],
                    "errors": [],
                    "complete": True,
                    "next_after": None,
                }
                ordered = sorted(ids)
                for start in range(0, len(ordered), 1000):
                    batch = p.enqueue(
                        args.kind,
                        file_ids=ordered[start : start + 1000],
                        refresh=args.refresh,
                        options=json.loads(args.options),
                    )
                    for key in ("queued", "cached", "errors"):
                        result[key].extend(batch[key])
                    result["complete"] &= batch["complete"]
                return result
            return p.enqueue(
                args.kind,
                file_ids=args.file_ids,
                location=args.location,
                limit=args.limit,
                after=args.after,
                refresh=args.refresh,
                options=json.loads(args.options),
            )
        if op == "run":
            return p.run(
                workers=args.workers,
                per_device=args.per_device,
                limit=args.limit,
                storage_groups=json.loads(args.storage_groups),
                retry_transient=args.retry_transient,
                retry_delay=args.retry_delay,
            )
        if op == "list":
            return p.list(
                state=args.state,
                limit=args.limit,
                after=args.after,
                file_id=args.file_id,
            )
        if op == "attempts":
            return p.attempts(args.id, limit=args.limit, after=args.after)
        if op == "show":
            return p.get(args.id)
        if op == "facts":
            return p.facts(args.file_id)
        return getattr(p, op)(args.id)
    if cmd == "content":
        return (
            p.duplicates(limit=args.limit, after=args.after)
            if op == "duplicates"
            else p.search(args.text, limit=args.limit, after=args.after)
        )
    if cmd == "identify":
        return tmdb_candidates(
            app,
            args.file_id,
            args.query,
            year=args.year,
            apply=args.apply,
            credential_env=args.credential_env,
        )
    refresh = Refresh(app)
    if op == "configure":
        return refresh.configure(args.catalog, args.endpoint, args.credential_env)
    if op == "list":
        return refresh.list(limit=args.limit, after=args.after)
    if op == "retry":
        return refresh.retry(args.id)
    return refresh.run(limit=args.limit)
