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
    put.add_argument("--recipe", required=True)
    put.add_argument("--location", required=True)
    put.add_argument(
        "--selection", required=True, help="SQL/GraphQL selection JSON file, or -"
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
            json.loads(read_text(args.selection, 256 * 1024)),
            estimates=estimates,
            required=args.required,
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
