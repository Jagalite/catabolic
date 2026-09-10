# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Query and projection adapters over the catalog's existing owners."""

import json

from .app import Application
from .domain import CatabolicError
from .projections import Projections
from .saved_queries import Queries
from .store import Store

QUERY_COMMANDS = ("save", "show", "list", "run", "contract")


def query(args):
    from .cli import read_text

    if args.file or args.schema or args.params:
        raise CatabolicError("saved queries use their pinned definition and parameters")
    if (args.sql == "list") != (args.query_name is None):
        raise CatabolicError("save requires a name; show/run require a revision ID")
    if (args.sql == "save") != bool(args.definition):
        raise CatabolicError("only query save requires --definition FILE")
    with Store(args.db, writable=args.sql == "save") as store:
        queries = Queries(store, args.profile)
        if args.sql == "save":
            return queries.put(
                args.query_name, json.loads(read_text(args.definition, 1024 * 1024))
            )
        if args.sql == "show":
            return queries.get(args.query_name)
        if args.sql == "list":
            return queries.list(args.max_rows, args.after)
        return queries.run(args.query_name, args.max_rows)


def register(commands):
    programs = commands.add_parser(
        "program",
        help="export and atomically import query/rule/projection configuration",
    ).add_subparsers(dest="operation", required=True)
    export = programs.add_parser("export")
    export.add_argument("--query", action="append", default=[])
    export.add_argument("--rule", action="append", default=[])
    export.add_argument("--projection", action="append", default=[])
    export.add_argument("--fallback-policy", action="append", default=[])
    importing = programs.add_parser("import")
    importing.add_argument("--file", required=True)
    importing.add_argument(
        "--bindings", help="JSON maps for catalogs, locations and processors"
    )
    importing.add_argument("--prefix", default="imported")
    importing.add_argument("--dry-run", action="store_true")
    operations = commands.add_parser(
        "operation",
        help="immutable processing definitions and supported operation types",
    ).add_subparsers(dest="operation", required=True)
    operations.add_parser("types")
    operations.add_parser("schema")
    put = operations.add_parser("put")
    put.add_argument("name")
    put.add_argument("--definition", required=True)
    show = operations.add_parser("show")
    show.add_argument("id")
    listing = operations.add_parser("list")
    listing.add_argument("--limit", type=int, default=100)
    listing.add_argument("--after", default="")
    sub = commands.add_parser(
        "projection", help="query, copy policy, layout and safe output reconciliation"
    ).add_subparsers(dest="operation", required=True)
    sub.add_parser("schema")
    put = sub.add_parser("put")
    put.add_argument("catalog")
    put.add_argument("--query", required=True)
    put.add_argument("--layout", required=True)
    put.add_argument("--copies", help="copy policy JSON file")
    put.add_argument("--renditions", help="rendition publication policy JSON file")
    for op in (
        "show",
        "preview",
        "execute",
        "verify",
        "recover",
        "enable-auto",
        "disable-auto",
    ):
        command = sub.add_parser(op)
        command.add_argument("catalog")
        if op in ("preview", "execute", "enable-auto"):
            command.add_argument("--max-removals", type=int, default=0)
        if op in ("preview", "execute"):
            command.add_argument("--allow-empty", action="store_true")
            command.add_argument("--replace-layout", action="store_true")
            command.add_argument("--limit", type=int, default=100)


def projection(args):
    from .catalog_refresh import CatalogRefresh
    from .cli import read_text
    from .reconcile import Reconciler

    with Store(
        args.db,
        writable=args.operation not in ("show", "verify"),
        for_recovery=args.operation == "recover",
    ) as store:
        app = Application(store, args.profile)
        projections = Projections(app)
        if args.operation == "put":
            return projections.put(
                args.catalog,
                args.query,
                args.layout,
                copies=json.loads(read_text(args.copies, 65536))
                if args.copies
                else None,
                renditions=json.loads(read_text(args.renditions, 65536))
                if args.renditions
                else None,
            )
        if args.operation == "show":
            return projections.get(args.catalog)
        if args.operation in ("preview", "execute"):
            return projections.run(
                args.catalog,
                apply=args.operation == "execute",
                max_removals=args.max_removals,
                allow_empty=args.allow_empty,
                replace_layout=args.replace_layout,
                limit=args.limit,
            )
        if args.operation == "verify":
            return Reconciler(app).verify(args.catalog)
        if args.operation == "recover":
            return Reconciler(app).recover(args.catalog)
        return CatalogRefresh(app).configure(
            args.catalog,
            enabled=args.operation == "enable-auto",
            max_removals=getattr(args, "max_removals", 0),
        )


def operation(args):
    from .cli import read_text
    from .operations import Operations, capabilities

    if args.operation == "schema":
        from .program_contracts import definition_schema

        return definition_schema("operation")
    if args.operation == "types":
        return capabilities()
    with Store(args.db, writable=args.operation == "put") as store:
        operations = Operations(Application(store, args.profile))
        if args.operation == "put":
            return operations.put(
                args.name, json.loads(read_text(args.definition, 256 * 1024))
            )
        if args.operation == "show":
            return operations.get(args.id)
        return operations.list(args.limit, args.after)


def program(args):
    from .cli import read_text
    from .program_bundle import MAX_BYTES, Programs

    with Store(args.db, writable=args.operation == "import") as store:
        programs = Programs(Application(store, args.profile))
        if args.operation == "export":
            return programs.export(
                queries=args.query,
                rules=args.rule,
                projections=args.projection,
                fallback_policies=args.fallback_policy,
            )
        return programs.import_bundle(
            json.loads(read_text(args.file, MAX_BYTES)),
            bindings=json.loads(read_text(args.bindings, 65536))
            if args.bindings
            else None,
            prefix=args.prefix,
            dry_run=args.dry_run,
        )


def envelope(args, data, code):
    return {
        "interface_version": 1,
        "command": args.command,
        "operation": getattr(args, "operation", None)
        or (
            getattr(args, "sql", None)
            if args.command == "query" and getattr(args, "sql", None) in QUERY_COMMANDS
            else None
        ),
        "profile": args.profile,
        "outcome": "success" if code == 0 else "incomplete" if code == 3 else "error",
        "exit_code": code,
        "data": data,
    }


def exit_code(args, result):
    if args.command == "spec":
        return 3 if args.operation == "diff" and not result["complete"] else 0
    if (
        args.command == "target"
        and args.operation == "import"
        and result.get("interrupted")
    ):
        return 130
    if args.command in ("query", "graphql") and result.get("errors"):
        return 2
    return (
        3 if any(result.get(k) is False for k in ("safe", "healthy", "complete")) else 0
    )
