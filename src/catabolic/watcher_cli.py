# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import json

from .app import Application
from .store import Store
from .watchers import Watchers


def register(commands):
    sub = commands.add_parser(
        "watcher", help="independent named query watchers"
    ).add_subparsers(dest="operation", required=True)
    put = sub.add_parser("put")
    put.add_argument("name")
    put.add_argument("--file", required=True)
    for op in ("show", "enable", "disable", "run", "history", "preview"):
        command = sub.add_parser(op)
        command.add_argument("name")
        if op == "enable":
            command.add_argument("--transfer", action="store_true")
    sub.add_parser("list")
    sub.add_parser("pending")
    sub.add_parser("schema")
    supervise = commands.add_parser("supervise")
    supervise.add_argument("--once", action="store_true")
    supervise.add_argument("--native", action="store_true")


def run(args):
    if args.command == "supervise":
        from .supervisor import run

        return run(args.db, args.profile, once=args.once, native=args.native)
    if args.operation == "schema":
        from .watcher_models import WatcherDefinition

        return WatcherDefinition.model_json_schema()
    with Store(
        args.db, writable=args.operation in ("put", "enable", "disable", "run")
    ) as store:
        service = Watchers(Application(store, args.profile))
        if args.operation == "put":
            from .cli import read_text

            return service.put(args.name, json.loads(read_text(args.file, 1024 * 1024)))
        if args.operation in ("list", "pending"):
            return {"watchers": service.list()}
        if args.operation == "show":
            return service.get(args.name)
        if args.operation == "history":
            return {"runs": service.history(args.name)}
        if args.operation == "preview":
            from .plans import describe, observation_requirements

            row = service.get(args.name)
            return {
                "watcher": row,
                "plan": describe(service.app, row["definition"]["plan"]),
                "observations": observation_requirements(
                    service.app, row["definition"]["plan"]
                ),
            }
        if args.operation in ("enable", "disable"):
            return service.enable(
                args.name,
                args.operation == "enable",
                transfer=getattr(args, "transfer", False),
            )
        return service.run(args.name)
