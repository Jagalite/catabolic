# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Configure automatic link updates and run the durable retry worker."""

import json
import sqlite3
import time

from .app import Application
from .catalog_refresh import CatalogRefresh
from .domain import CatabolicError
from .store import Store


def register(commands):
    commands = commands.add_parser(
        "catalog-refresh",
        help="automatic saved-layout publication; optional configured consumer delivery",
    ).add_subparsers(dest="operation", required=True)
    for action in ("enable", "disable"):
        command = commands.add_parser(action)
        command.add_argument("--catalog", required=True)
        if action == "enable":
            command.add_argument("--max-removals", type=int, default=0)
    for action in ("pending", "run", "watch"):
        command = commands.add_parser(action)
        command.add_argument("--limit", type=int, default=100)
        if action in ("run", "watch"):
            command.add_argument("--catalog")
        if action == "run":
            command.add_argument(
                "--force", action="store_true", help="retry now, ignoring backoff"
            )
        if action == "watch":
            command.add_argument(
                "--interval",
                type=int,
                default=5,
                help="check due work every 1..60 seconds",
            )


def dispatch(args):
    if not args.db:
        raise CatabolicError("select --db or set CATABOLIC_DB")
    if args.operation == "watch":
        from .curation import page_limit

        page_limit(args.limit)
        if not 1 <= args.interval <= 60:
            raise CatabolicError("watch interval must be 1..60 seconds")
        while True:
            try:
                with Store(args.db, writable=True) as store:
                    result = CatalogRefresh(Application(store, args.profile)).run(
                        catalog=args.catalog, limit=args.limit
                    )
            except (CatabolicError, OSError, sqlite3.Error, ValueError) as exc:
                result = {"complete": False, "errors": [{"error": str(exc)[:4000]}]}
            if result.get("processed") or result.get("errors"):
                print(json.dumps(result), flush=True)
            # No lock or transaction is held while idle. A supervisor may restart
            # this process at any time; pending work and backoff live in SQLite.
            time.sleep(args.interval)
    with Store(args.db, writable=args.operation != "pending") as store:
        refresh = CatalogRefresh(Application(store, args.profile))
        if args.operation in ("enable", "disable"):
            return refresh.configure(
                args.catalog,
                enabled=args.operation == "enable",
                max_removals=getattr(args, "max_removals", 0),
            )
        if args.operation == "pending":
            return refresh.pending(limit=args.limit)
        return refresh.run(catalog=args.catalog, limit=args.limit, force=args.force)
