# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Explicit single-step processor worker commands; no hidden background daemon."""

import json

from .app import Application
from .domain import CatabolicError
from .processors import Processors, dispatch_job
from .store import Store


def register(commands):
    commands = commands.add_parser(
        "processor", help="network processors and fenced worker leases"
    ).add_subparsers(dest="operation", required=True)
    put = commands.add_parser("put")
    put.add_argument("name")
    put.add_argument(
        "--definition",
        required=True,
        help="HTTP receipt-v1 processor configuration JSON",
    )
    show = commands.add_parser("show")
    show.add_argument("name")
    listing = commands.add_parser("jobs")
    listing.add_argument("name")
    listing.add_argument("--limit", type=int, default=100)
    listing.add_argument("--after", default="")
    enqueue = commands.add_parser("enqueue")
    enqueue.add_argument("name")
    for flag in ("file-id", "item-id", "output-definition"):
        enqueue.add_argument("--" + flag, required=True)
    enqueue.add_argument(
        "--configuration",
        default="{}",
        help="JSON processor options, never credentials",
    )
    for operation in ("claim", "tick"):
        parser = commands.add_parser(operation)
        parser.add_argument("name")
        parser.add_argument("--worker", required=True)
        parser.add_argument("--lease-seconds", type=int, default=300)
    for operation in ("job", "retry", "cancel"):
        commands.add_parser(operation).add_argument("id")
    for operation in ("heartbeat", "dispatch", "complete"):
        parser = commands.add_parser(operation)
        parser.add_argument(
            "--lease-file",
            required=True,
            help="JSON output of processor claim, or - for stdin",
        )
        if operation == "heartbeat":
            parser.add_argument("--lease-seconds", type=int, default=300)
        if operation == "complete":
            parser.add_argument("--receipt-file", required=True)


def dispatch(args):
    from .cli import read_text

    if not args.db:
        raise CatabolicError("select --db or set CATABOLIC_DB")
    op = args.operation
    lease = None
    if op in ("heartbeat", "dispatch", "complete"):
        lease = json.loads(read_text(args.lease_file, 256 * 1024))
        if (
            not isinstance(lease, dict)
            or not isinstance(lease.get("id"), str)
            or not isinstance(lease.get("lease_token"), str)
        ):
            raise CatabolicError(
                "lease file must contain id and lease_token from claim"
            )
    if op == "dispatch":
        return dispatch_job(args.db, args.profile, lease["id"], lease["lease_token"])
    with Store(args.db, writable=op not in ("show", "jobs", "job")) as store:
        processors = Processors(Application(store, args.profile))
        if op == "put":
            return processors.put(args.name, json.loads(args.definition))
        if op == "show":
            return processors.get(args.name)
        if op == "jobs":
            return processors.jobs(args.name, args.limit, args.after)
        if op == "job":
            return processors.job(args.id)
        if op == "enqueue":
            return processors.enqueue(
                args.name,
                args.file_id,
                args.item_id,
                args.output_definition,
                json.loads(args.configuration),
            )
        if op in ("retry", "cancel"):
            return getattr(processors, op)(args.id)
        if op == "heartbeat":
            return processors.heartbeat(
                lease["id"], lease["lease_token"], args.lease_seconds
            )
        if op == "complete":
            return processors.complete(
                lease["id"],
                lease["lease_token"],
                json.loads(read_text(args.receipt_file, 1048576)),
            )
        lease = processors.claim(
            args.name, args.worker, args.lease_seconds, resume=op == "tick"
        )
        if op == "claim" or not lease["claimed"]:
            return lease
    # Never retain the SQLite writer lock during network I/O.
    return dispatch_job(args.db, args.profile, lease["id"], lease["lease_token"])
