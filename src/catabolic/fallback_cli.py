# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Operator controls for opt-in shared fallback resolution."""

import json

from .app import Application
from .domain import CatabolicError
from .fallback_models import Policy
from .fallback_policies import Policies
from .fallback_projection import FallbackProjection
from .fallback_resolution import Resolver
from .store import Store


def register(commands):
    sub = commands.add_parser(
        "fallback", help="try saved candidate queries in order; no hidden processing"
    ).add_subparsers(dest="operation", required=True)
    sub.add_parser("schema")
    put = sub.add_parser("put")
    put.add_argument("name")
    put.add_argument("--definition", required=True)
    show = sub.add_parser("show")
    show.add_argument("id")
    sub.add_parser("list")
    bind = sub.add_parser("bind")
    bind.add_argument("catalog")
    bind.add_argument("--policy", required=True)
    bind.add_argument("--query", required=True)
    bind.add_argument("--layout", required=True)
    resolve = sub.add_parser("resolve")
    resolve.add_argument("item")
    resolve.add_argument("--policy", required=True)
    resolve.add_argument("--role", default="primary")
    resolve.add_argument("--part", type=int)
    resolve.add_argument("--variant", default="")
    for operation in ("preview", "execute", "enable", "disable", "history"):
        command = sub.add_parser(operation)
        command.add_argument("catalog")
        if operation in ("preview", "execute"):
            command.add_argument("--failback", action="store_true")
        if operation == "execute":
            command.add_argument("--expected-plan", required=True)
        if operation in ("execute", "enable"):
            command.add_argument("--max-changes", type=int, default=100)
            command.add_argument("--max-removals", type=int, default=0)
        if operation == "enable":
            command.add_argument("--interval", type=int, default=60)
    worker = sub.add_parser("worker")
    worker.add_argument("--once", action="store_true")


def run(args):
    from .cli import read_text
    from .fallback_worker import configure
    from .fallback_worker import run as worker

    if args.operation == "schema":
        return Policy.model_json_schema()
    if args.operation == "worker":
        return worker(args.db, args.profile, once=args.once)
    with Store(args.db, writable=True) as store:
        app = Application(store, args.profile)
        policies = Policies(store, args.profile)
        projection = FallbackProjection(app)
        if args.operation == "put":
            return policies.put(
                args.name, json.loads(read_text(args.definition, 65536))
            )
        if args.operation == "show":
            return policies.get(args.id)
        if args.operation == "list":
            return policies.listing()
        if args.operation == "bind":
            return projection.bind(args.catalog, args.policy, args.query, args.layout)
        if args.operation == "resolve":
            result = Resolver(app).resolve(
                [
                    {
                        "item_id": args.item,
                        "role": args.role,
                        "part": args.part,
                        "variant": args.variant,
                    }
                ],
                args.policy,
            )
            return projection.present(result)
        if args.operation in ("preview", "execute"):
            return projection.run(
                args.catalog,
                apply=args.operation == "execute",
                expected_plan=getattr(args, "expected_plan", None),
                manual_failback=args.failback,
                max_changes=getattr(args, "max_changes", 100),
                max_removals=getattr(args, "max_removals", 0),
            )
        if args.operation in ("enable", "disable"):
            return configure(
                app,
                args.catalog,
                enabled=args.operation == "enable",
                interval=getattr(args, "interval", 60),
                max_changes=getattr(args, "max_changes", 100),
                max_removals=getattr(args, "max_removals", 0),
            )
        if args.operation == "history":
            return {
                "history": [
                    {**r, "decision": json.loads(r["decision"])}
                    for r in store.rows(
                        "SELECT * FROM fallback_history WHERE profile=? AND catalog=? ORDER BY id DESC LIMIT 100",
                        (args.profile, args.catalog),
                    )
                ]
            }
        raise CatabolicError("unknown fallback operation")
