# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Terminal adapter. All behavior lives in application use cases."""

from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys

from . import __version__
from .app import Application
from .domain import CatabolicError
from .media import describe_types
from .migration import inspect_database, upgrade_database
from .reconcile import Reconciler
from .sql_query import MAX_SQL_BYTES, execute_sql, render_table
from .store import Store


def page_arguments(command, sorts=None, default=None):
    command.add_argument("--limit", type=int, default=100)
    command.add_argument("--cursor")
    if sorts:
        command.add_argument("--sort", choices=sorts, default=default)
        command.add_argument("--descending", action="store_true")


def tag_filters(command):
    command.add_argument(
        "--tag",
        dest="tags",
        action="append",
        default=[],
        help="require each tag; repeat for AND",
    )
    command.add_argument(
        "--any-tag",
        dest="any_tags",
        action="append",
        default=[],
        help="require at least one of these tags",
    )
    command.add_argument(
        "--not-tag",
        dest="not_tags",
        action="append",
        default=[],
        help="exclude any of these tags",
    )
    command.add_argument(
        "--descendants",
        action="store_true",
        help="include descendants of selected tags",
    )


def item_filters(command):
    command.add_argument(
        "--kind", help="media kind; see item types; custom:name is supported"
    )
    command.add_argument("--year", type=int)
    command.add_argument("--identity", metavar="NAMESPACE=VALUE")


def options(args, *names):
    return {key: getattr(args, key) for key in names}


def parser() -> argparse.ArgumentParser:
    from .layouts import PRESETS

    root = argparse.ArgumentParser(
        prog="catabolic",
        description="Inventory media and maintain catalogs with symlinks or hardlinks.",
    )
    root.add_argument("--version", action="version", version=f"%(prog)s {__version__}")
    root.add_argument(
        "--db",
        default=os.environ.get("CATABOLIC_DB"),
        help="explicit database path (or CATABOLIC_DB)",
    )
    root.add_argument(
        "--profile", default=os.environ.get("CATABOLIC_PROFILE", "default")
    )
    root.add_argument(
        "--json",
        action="store_true",
        help="machine-readable JSON on stdout; errors on stderr",
    )
    commands = root.add_subparsers(dest="command", required=True)
    from .enrichment_cli import register

    register(commands)
    from .maintenance import register as register_maintenance

    register_maintenance(commands)
    from .rule_cli import register as register_rules

    register_rules(commands)
    commands.add_parser(
        "init", help="create a new database; its parent directory must exist"
    )
    commands.add_parser("status", help="show configuration, counts, and bindings")
    target = commands.add_parser(
        "target", help="application compatibility profiles and explicit import adapters"
    ).add_subparsers(dest="operation", required=True)
    target.add_parser("list")
    target.add_parser("show").add_argument("name")
    importing = target.add_parser(
        "import", help="preview an import; --apply runs the official application CLI"
    )
    importing.add_argument("name", choices=("calibre", "calibre-web", "immich"))
    importing.add_argument("--catalog", default="global")
    importing.add_argument(
        "--destination",
        required=True,
        help="local calibre library directory or Immich API URL",
    )
    importing.add_argument("--apply", action="store_true")
    importing.add_argument("--limit", type=int, default=1000)
    importing.add_argument(
        "--timeout", type=int, default=300, help="seconds per imported file"
    )
    export = commands.add_parser(
        "export",
        help="XSPF, OPDS or NFO metadata; optionally publish a fresh projection bundle",
    )
    export.add_argument("--format", required=True, choices=("xspf", "opds", "nfo"))
    export.add_argument("--catalog", default="global")
    export.add_argument(
        "--base-url", help="HTTP(S) URL serving the projection root; required for OPDS"
    )
    publication = export.add_mutually_exclusive_group()
    publication.add_argument(
        "--output", help="new bundle directory; existing paths are never overwritten"
    )
    publication.add_argument(
        "--raw",
        action="store_true",
        help="emit a single XSPF or OPDS document instead of the JSON envelope",
    )
    docs = commands.add_parser(
        "docs", help="read or search bundled offline guides; no database required"
    )
    docs.add_argument("topic", nargs="?", help="topic name; omitted: list all topics")
    docs.add_argument(
        "--search", metavar="TEXT", help="find topics containing all search words"
    )
    docs.add_argument(
        "--json",
        action="store_true",
        default=argparse.SUPPRESS,
        help="emit structured topics, matches or Markdown for agents",
    )
    spec = commands.add_parser(
        "spec",
        help="generate/check the interchange specification; no database required",
    ).add_subparsers(dest="operation", required=True)
    for operation, help_text in (
        ("schema", "emit generated JSON Schema"),
        ("docs", "emit generated Markdown field reference"),
    ):
        spec.add_parser(operation, help=help_text).add_argument(
            "--format-version", type=int, choices=(1, 2, 3), default=3
        )
    spec.add_parser("check", help="check models against frozen versioned artifacts")
    for operation in ("validate", "roundtrip"):
        spec.add_parser(
            operation,
            help="validate a catalog document"
            if operation == "validate"
            else "validate and re-emit JSON, preserving unknown fields",
        ).add_argument(
            "--file", required=True, help="JSON document path; - reads stdin"
        )
    spec.add_parser(
        "diff",
        help="compare a candidate schema to the generated schema; differences require review",
    ).add_argument(
        "--against", required=True, help="candidate JSON Schema path; - reads stdin"
    )
    manifest = commands.add_parser(
        "manifest", help="export catalog metadata as a versioned JSON snapshot"
    )
    manifest.add_argument("--catalog", default="global")
    destination = manifest.add_mutually_exclusive_group()
    destination.add_argument(
        "--output", metavar="PATH", help="write a JSON file; default or - writes stdout"
    )
    destination.add_argument(
        "--in-catalog",
        action="store_true",
        help="write .catabolic-manifest.json inside a synchronized output",
    )
    manifest.add_argument(
        "--replace",
        action="store_true",
        help="atomically refresh a valid manifest belonging to this catalog",
    )
    manifest.add_argument(
        "--extra", default="{}", help="additional metadata as a JSON object"
    )
    graphql = commands.add_parser(
        "graphql", help="execute a read-only GraphQL query; always emits JSON"
    )
    graphql.add_argument("document", nargs="?")
    graphql.add_argument("--file", help="UTF-8 query file; - reads stdin")
    graphql.add_argument("--variables", help="JSON object of query variables")
    graphql.add_argument("--operation-name")
    graphql.add_argument("--schema", action="store_true")
    graphql.add_argument("--timeout-ms", type=int, default=5000)
    layouts = commands.add_parser(
        "layout", help="declarative output naming and desired mapping plans"
    ).add_subparsers(dest="operation", required=True)
    layouts.add_parser("presets")
    layouts.add_parser("list")
    layouts.add_parser("show").add_argument("name")
    put_layout = layouts.add_parser("put")
    put_layout.add_argument("name")
    layout_source = put_layout.add_mutually_exclusive_group(required=True)
    layout_source.add_argument("--file", help="JSON definition file; - reads stdin")
    layout_source.add_argument("--preset", choices=tuple(PRESETS))
    selection_source = put_layout.add_mutually_exclusive_group()
    selection_source.add_argument(
        "--select-sql",
        metavar="PATH",
        help="save SQL selection from a file; - reads stdin",
    )
    selection_source.add_argument(
        "--select-graphql",
        metavar="PATH",
        help="save pageable GraphQL selection from a file; - reads stdin",
    )
    put_layout.add_argument(
        "--params", help="SQL selection parameters as a JSON object"
    )
    put_layout.add_argument(
        "--variables", help="GraphQL selection variables as a JSON object"
    )
    put_layout.add_argument(
        "--selection-profile", help="fixed query profile; defaults to default"
    )
    for action in ("preview", "apply"):
        layout_plan = layouts.add_parser(action)
        layout_plan.add_argument("name")
        layout_plan.add_argument("--catalog", default="global")
        layout_plan.add_argument("--replace-layout", action="store_true")
        layout_plan.add_argument(
            "--allow-empty",
            action="store_true",
            help="allow a query refresh to retire every generated mapping",
        )
        layout_plan.add_argument("--limit", type=int, default=100)
    query = commands.add_parser(
        "query",
        help="execute one read-only SQL statement; use --schema to discover views",
    )
    query.add_argument("sql", nargs="?")
    query.add_argument(
        "--file", metavar="PATH", help="read SQL from a UTF-8 file; - reads stdin"
    )
    query.add_argument(
        "--schema",
        action="store_true",
        help="describe query views, underlying tables, and limits",
    )
    query.add_argument(
        "--params",
        metavar="JSON_OBJECT",
        help="named scalar bindings; :profile is supplied automatically",
    )
    query.add_argument(
        "--max-rows",
        type=int,
        default=1000,
        help="output row cap, 1–10000; truncation exits with code 3",
    )
    query.add_argument(
        "--timeout-ms",
        type=int,
        default=5000,
        help="SQL execution timeout, 1–60000 milliseconds",
    )
    query.add_argument(
        "--format",
        choices=("json", "table"),
        help="query output format; global --json takes precedence",
    )
    database = commands.add_parser(
        "db", help="inspect database versions and explicitly upgrade"
    ).add_subparsers(dest="operation", required=True)
    database.add_parser(
        "status", help="show schema version and migration history without writing"
    )
    upgrade = database.add_parser(
        "upgrade", help="back up, rehearse, and apply pending migrations"
    )
    upgrade.add_argument(
        "--dry-run",
        action="store_true",
        help="read-only validation and pending migration listing",
    )
    upgrade.add_argument(
        "--backup-dir", help="backup parent directory; defaults to <database>.backups"
    )
    profiles = commands.add_parser("profile").add_subparsers(
        dest="operation", required=True
    )
    profiles.add_parser("list")
    profiles.add_parser("add").add_argument("name")
    for entity in ("location", "catalog"):
        sub = commands.add_parser(entity).add_subparsers(
            dest="operation", required=True
        )
        sub.add_parser("list")
        bind = sub.add_parser(
            "bind", help="register a directory; catalogs can create a default output"
        )
        bind.add_argument("name")
        if entity == "catalog":
            bind.add_argument(
                "--link-mode",
                choices=("symlink", "hardlink"),
                help="explicit output mode; new catalogs default to symlink, existing mode is preserved",
            )
            retained = sub.add_parser(
                "retained", help="list retained hardlinks; no automatic deletion"
            )
            retained.add_argument("name")
            retained.add_argument("--limit", type=int, default=100)
            retained.add_argument("--after", help="retained ID from the previous page")
        bind.add_argument(
            "--root",
            required=entity == "location",
            help="existing source directory"
            if entity == "location"
            else "existing output directory; omitted: reuse its binding or create ./catabolic/NAME",
        )
    scan = commands.add_parser(
        "scan", help="publish inventory only after complete source traversal"
    )
    scan.add_argument("location", nargs="?")
    scan.add_argument(
        "--exclude",
        action="append",
        default=[],
        metavar="RELATIVE_PATH",
        help="exclude an exact source-relative file or subtree; repeat as needed",
    )
    files = commands.add_parser("files", help="list inventory in stable pages")
    files.add_argument("--catalog", default="global")
    files.add_argument("--unmapped", action="store_true")
    files.add_argument(
        "--unidentified",
        action="store_true",
        help="no active file identification, regardless of catalog placement",
    )
    files.add_argument(
        "--search", help="literal, case-insensitive substring of the source path"
    )
    files.add_argument("--location")
    files.add_argument(
        "--status",
        choices=("present", "missing", "unknown"),
        help="availability recorded by the selected profile's scans",
    )
    files.add_argument("--item")
    tag_filters(files)
    item_filters(files)
    page_arguments(files, ("id", "path", "size", "mtime"), "id")
    items = commands.add_parser("item").add_subparsers(dest="operation", required=True)
    from .workflow_cli import register as register_workflow

    register_workflow(items)
    items.add_parser(
        "types", help="discover media kinds, file roles, and relationship vocabulary"
    )
    listing = items.add_parser("list")
    listing.add_argument("--search", help="literal, case-insensitive title substring")
    listing.add_argument("--catalog", help="items with active mappings in this catalog")
    listing.add_argument(
        "--curation-status",
        choices=(
            "pending",
            "in_progress",
            "complete",
            "deferred",
            "ignored",
            "needs_attention",
        ),
    )
    listing.add_argument(
        "--metadata",
        action="append",
        default=[],
        metavar="KEY=JSON_VALUE",
        help="exact top-level metadata match; repeat to combine filters",
    )
    tag_filters(listing)
    item_filters(listing)
    page_arguments(listing, ("id", "title", "year"), "id")
    show = items.add_parser(
        "show", help="show identities, source files, and catalog destinations"
    )
    show.add_argument("id", nargs="?")
    show.add_argument("--identity", metavar="NAMESPACE=VALUE")
    show.add_argument("--catalog")
    show.add_argument("--include-disabled", action="store_true")
    page_arguments(show)
    put = items.add_parser("put")
    put.add_argument("--id")
    put.add_argument("--kind", required=True, help="media kind; see item types")
    put.add_argument(
        "--identity", action="append", default=[], metavar="NAMESPACE=VALUE"
    )
    put.add_argument("--metadata", default="{}", help="JSON object")
    tags = commands.add_parser(
        "tag", help="namespaced tags, aliases, hierarchy and attributed assignments"
    ).add_subparsers(dest="operation", required=True)
    listing = tags.add_parser("list")
    for option in ("search", "namespace", "parent", "child"):
        listing.add_argument("--" + option)
    page_arguments(listing)
    put = tags.add_parser("put", help="create a tag or update its description")
    put.add_argument("name")
    put.add_argument("--description")
    rename = tags.add_parser(
        "rename", help="rename while retaining the old name as an alias"
    )
    rename.add_argument("name")
    rename.add_argument("new_name")
    alias = tags.add_parser("alias")
    alias.add_argument("name")
    alias.add_argument("alias")
    alias.add_argument("--remove", action="store_true")
    parent = tags.add_parser(
        "parent", help="add or remove a direct parent; cycles are rejected"
    )
    parent.add_argument("child")
    parent.add_argument("parent")
    parent.add_argument("--remove", action="store_true")
    for operation in ("add", "remove"):
        assign = tags.add_parser(
            operation, help="atomic batch of up to 1000 explicit tag assertions"
        )
        assign.add_argument("names", nargs="+")
        assign.add_argument("--item", dest="items", action="append", default=[])
        assign.add_argument("--file", dest="files", action="append", default=[])
        assign.add_argument(
            "--source",
            default="manual",
            help="independent provenance, e.g. manual or agent:curator",
        )
        if operation == "add":
            assign.add_argument("--confidence", type=float)
            assign.add_argument("--note", default="")
    assertions = tags.add_parser("assignments")
    for option in ("tag", "item", "file", "source"):
        assertions.add_argument("--" + option)
    assertions.add_argument(
        "--active", choices=("active", "disabled", "all"), default="active"
    )
    page_arguments(assertions)
    associations = commands.add_parser(
        "association", help="identify files independently of catalog placement"
    ).add_subparsers(dest="operation", required=True)
    listing = associations.add_parser("list")
    listing.add_argument("--item")
    listing.add_argument("--file")
    listing.add_argument("--role")
    listing.add_argument(
        "--active", choices=("active", "disabled", "all"), default="active"
    )
    page_arguments(listing)
    put = associations.add_parser("put")
    put.add_argument("--file", required=True)
    put.add_argument("--item", required=True)
    put.add_argument("--role", default="primary")
    put.add_argument("--part", type=int)
    put.add_argument("--clear-part", action="store_true")
    put.add_argument(
        "--metadata", help="JSON object with optional format, language, or evidence"
    )
    associations.add_parser("disable").add_argument("id")
    relations = commands.add_parser(
        "relationship", help="connect works, editions, ordered parts, and contributors"
    ).add_subparsers(dest="operation", required=True)
    listing = relations.add_parser("list")
    listing.add_argument("--item")
    listing.add_argument("--kind")
    listing.add_argument(
        "--direction", choices=("both", "incoming", "outgoing"), default="both"
    )
    listing.add_argument(
        "--active", choices=("active", "disabled", "all"), default="active"
    )
    page_arguments(listing)
    put = relations.add_parser("put")
    put.add_argument("--source", required=True)
    put.add_argument("--target", required=True)
    put.add_argument("--kind", required=True)
    put.add_argument("--position", type=int)
    put.add_argument("--clear-position", action="store_true")
    put.add_argument(
        "--metadata", help="JSON object with optional evidence or descriptive fields"
    )
    relations.add_parser("disable").add_argument("id")
    mappings = commands.add_parser("mapping").add_subparsers(
        dest="operation", required=True
    )
    listing = mappings.add_parser("list")
    scope = listing.add_mutually_exclusive_group()
    scope.add_argument("--catalog", default="global")
    scope.add_argument("--all-catalogs", action="store_true")
    listing.add_argument("--item")
    listing.add_argument("--file")
    listing.add_argument(
        "--active", choices=("all", "active", "disabled"), default="all"
    )
    listing.add_argument(
        "--search", help="literal, case-insensitive destination substring"
    )
    page_arguments(listing, ("id", "path", "catalog"), "path")
    put = mappings.add_parser("put")
    put.add_argument("--catalog", default="global")
    put.add_argument("--file", required=True)
    put.add_argument("--item", required=True)
    put.add_argument("--path", required=True)
    mappings.add_parser("disable").add_argument("id")
    for command in ("sync", "verify", "recover"):
        sub = commands.add_parser(command)
        if command == "recover":
            sub.add_argument(
                "--cancel-unapplied",
                action="store_true",
                help="cancel hardlink intent only if output is untouched or data safely retained",
            )
        scope = sub.add_mutually_exclusive_group()
        scope.add_argument("--catalog", default="global")
        scope.add_argument("--all-catalogs", action="store_true")
        if command == "sync":
            sub.add_argument(
                "--max-removals",
                type=int,
                help="block the entire plan above this output-removal count",
            )
            sub.add_argument(
                "--max-removal-percent",
                type=float,
                help="block above this percentage of currently owned output entries",
            )
            sub.add_argument(
                "--dry-run",
                action="store_true",
                help="read-only preview; does not scan or write inventory",
            )
    return root


def dispatch(args: argparse.Namespace) -> dict:
    if args.command == "artifact" and args.operation == "capabilities":
        from .rendering import capabilities

        return capabilities()
    if args.command == "target" and args.operation in ("list", "show"):
        from .targets import describe

        return describe(args.name if args.operation == "show" else None)
    if args.command == "docs":
        from .documentation import documentation

        return documentation(args.topic, args.search)
    if args.command == "spec":
        from .interchange import specification
        from .interchange.validation import MAX_BYTES, decode_document, document_value

        if args.operation == "schema":
            return specification.schema(args.format_version)
        if args.operation == "docs":
            return {"markdown": specification.reference_text(args.format_version)}
        if args.operation == "check":
            return specification.check_release()
        if args.operation == "diff":
            other = json.loads(read_text(args.against, 5 * 1024 * 1024))
            changed = specification.changes(specification.schema(), other)
            return {
                "complete": not changed,
                "compatibility": "identical" if not changed else "review_required",
                "changed_paths": changed,
            }
        document = decode_document(read_text(args.file, MAX_BYTES))
        if args.operation == "roundtrip":
            return document_value(document)
        return {
            "valid": True,
            "format": document.format,
            "format_version": document.format_version,
            "content_sha256": document.content_sha256,
            "counts": document.content.counts.model_dump(),
        }
    if args.command == "item" and args.operation == "types":
        return describe_types()
    if args.command == "layout" and args.operation == "presets":
        from .layouts import PRESETS

        return {"presets": PRESETS}
    if args.command == "graphql":
        from .graphql_query import (
            MAX_DOCUMENT_BYTES,
            execute_graphql,
            schema_description,
        )

        if sum((args.document is not None, args.file is not None, args.schema)) != 1:
            raise CatabolicError(
                "choose exactly one query document, --file, or --schema"
            )
        if args.schema:
            if args.variables is not None or args.operation_name is not None:
                raise CatabolicError(
                    "--schema cannot be combined with query variables or operation name"
                )
            return schema_description()
        if not args.db:
            raise CatabolicError("select --db or set CATABOLIC_DB")
        document = (
            read_text(args.file, MAX_DOCUMENT_BYTES) if args.file else args.document
        )
        return execute_graphql(
            args.db,
            document,
            profile=args.profile,
            variables=args.variables,
            operation_name=args.operation_name,
            timeout_ms=args.timeout_ms,
        )
    if not args.db:
        raise CatabolicError(
            "select --db or set CATABOLIC_DB; no database is selected implicitly"
        )
    if args.command == "watch":
        from .watching import watch

        return watch(
            args.db,
            args.profile,
            operation=args.kind,
            location=args.location,
            settle=args.settle,
            workers=args.workers,
            batch=args.batch,
            interval=args.interval,
            cycles=args.cycles,
            retry_transient=args.retry_transient,
            retry_delay=args.retry_delay,
            progress=lambda value: print(
                json.dumps(value), file=sys.stderr, flush=True
            ),
        )
    if args.command == "init":
        if args.profile != "default":
            raise CatabolicError(
                "init creates the default profile; add additional profiles explicitly"
            )
        return Store.initialize(args.db)
    if args.command == "db":
        if args.operation == "status":
            return inspect_database(args.db)
        return upgrade_database(
            args.db, dry_run=args.dry_run, backup_dir=args.backup_dir
        )
    if args.command == "query":
        if sum((args.sql is not None, args.file is not None, args.schema)) != 1:
            raise CatabolicError(
                "choose exactly one SQL statement, --file PATH, --file -, or --schema"
            )
        sql = args.sql
        if args.file is not None:
            if args.file == "-":
                sql = sys.stdin.read(MAX_SQL_BYTES + 1)
            else:
                with open(args.file, encoding="utf-8") as stream:
                    sql = stream.read(MAX_SQL_BYTES + 1)
        return execute_sql(
            args.db,
            sql,
            profile=args.profile,
            params=args.params,
            describe=args.schema,
            max_rows=args.max_rows,
            timeout_ms=args.timeout_ms,
        )
    from . import enrichment_cli, rule_cli, workflow_cli

    writable = (
        enrichment_cli.writable(args)
        or rule_cli.writable(args)
        or workflow_cli.writable(args)
        or (
            args.command == "tag"
            and args.operation not in ("list", "assignments")
            or args.command == "export"
            and args.output is not None
            or args.command == "manifest"
            and (args.in_catalog or args.output not in (None, "-"))
            or args.command in ("scan", "recover", "maintenance")
            or args.command == "sync"
            and not args.dry_run
            or getattr(args, "operation", None) in ("add", "bind", "put", "disable")
            or args.command == "layout"
            and args.operation == "apply"
        )
    )
    with Store(
        args.db, writable=writable, for_recovery=args.command == "recover"
    ) as store:
        app = Application(store, args.profile)
        command = args.command
        if command == "rule":
            return rule_cli.dispatch(app, args)
        if command == "maintenance":
            from .maintenance import run

            return run(
                app,
                catalog=None if args.all_catalogs else args.catalog,
                **options(
                    args,
                    "inventory_only",
                    "exclude",
                    "process",
                    "settle",
                    "batch",
                    "workers",
                    "max_removals",
                    "max_removal_percent",
                    "manifest",
                    "limit",
                    "rules",
                    "rule_batch",
                    "render_rules",
                    "rule_max_new_bytes",
                ),
                progress=lambda value: print(
                    json.dumps(value), file=sys.stderr, flush=True
                ),
            )
        if command in enrichment_cli.COMMANDS:
            return enrichment_cli.dispatch(app, args)
        if command in ("export", "target"):
            from contextlib import nullcontext

            from .exports import build_export, publish_bundle
            from .importers import import_catalog
            from .manifest import Manifest

            with store.transaction() if writable else nullcontext():
                document = Manifest(app).build(args.catalog)
                if command == "target":
                    return import_catalog(
                        app,
                        document,
                        args.name,
                        args.destination,
                        apply=args.apply,
                        limit=args.limit,
                        timeout=args.timeout,
                    )
                result = build_export(document, args.format, args.base_url)
                if args.output:
                    return publish_bundle(app, document, result, args.output)
                if args.raw:
                    if args.format == "nfo":
                        raise CatabolicError(
                            "NFO exports can contain multiple files; omit --raw or use --output"
                        )
                    return {"raw": result["files"][0]["content"]}
                return result
        if command == "manifest":
            from .manifest import Manifest

            exporter = Manifest(app)
            extra = json.loads(args.extra)
            if writable:
                with store.transaction():
                    document = exporter.build(args.catalog, extra=extra)
                    return exporter.write(
                        document,
                        output=args.output,
                        in_catalog=args.in_catalog,
                        replace=args.replace,
                    )
            if args.replace:
                raise CatabolicError("--replace requires --output or --in-catalog")
            return exporter.build(args.catalog, extra=extra)
        if command == "layout":
            from .layouts import PRESETS, Layouts

            layouts = Layouts(app)
            if args.operation == "list":
                return layouts.list()
            if args.operation == "show":
                return layouts.get(args.name)
            if args.operation == "put":
                definition = (
                    PRESETS[args.preset]
                    if args.preset
                    else json.loads(read_text(args.file, MAX_SQL_BYTES))
                )
                if not isinstance(definition, dict):
                    raise CatabolicError("layout definition must be a JSON object")
                if args.select_sql or args.select_graphql:
                    if args.file == "-" and "-" in (
                        args.select_sql,
                        args.select_graphql,
                    ):
                        raise CatabolicError(
                            "stdin can supply only one definition or query"
                        )
                    if (
                        args.select_sql
                        and args.variables is not None
                        or args.select_graphql
                        and args.params is not None
                    ):
                        raise CatabolicError(
                            "use --params for SQL or --variables for GraphQL"
                        )
                    language = "sql" if args.select_sql else "graphql"
                    values = args.params if args.select_sql else args.variables
                    selection = {
                        "language": language,
                        "query": read_text(
                            args.select_sql or args.select_graphql, MAX_SQL_BYTES
                        ),
                        "profile": args.selection_profile or "default",
                    }
                    if values is not None:
                        selection["params" if args.select_sql else "variables"] = (
                            json.loads(values)
                        )
                    definition = {**definition, "selection": selection}
                elif any(
                    value is not None
                    for value in (args.params, args.variables, args.selection_profile)
                ):
                    raise CatabolicError(
                        "selection options require --select-sql or --select-graphql"
                    )
                return layouts.put(args.name, definition)
            return layouts.run(
                args.name,
                args.catalog,
                apply=args.operation == "apply",
                replace_layout=args.replace_layout,
                allow_empty=args.allow_empty,
                limit=args.limit,
            )
        if command == "tag":
            if args.operation == "list":
                return app.queries.tags(
                    **options(
                        args,
                        "search",
                        "namespace",
                        "parent",
                        "child",
                        "limit",
                        "cursor",
                    )
                )
            if args.operation == "assignments":
                return app.queries.taggings(
                    **options(
                        args,
                        "tag",
                        "item",
                        "file",
                        "source",
                        "active",
                        "limit",
                        "cursor",
                    )
                )
            if args.operation == "put":
                return app.tags.put(args.name, description=args.description)
            if args.operation == "rename":
                return app.tags.rename(args.name, args.new_name)
            if args.operation == "alias":
                return app.tags.alias(args.name, args.alias, remove=args.remove)
            if args.operation == "parent":
                return app.tags.parent(args.child, args.parent, remove=args.remove)
            return app.tags.assign(
                args.names,
                items=args.items,
                files=args.files,
                source=args.source,
                confidence=getattr(args, "confidence", None),
                note=getattr(args, "note", ""),
                remove=args.operation == "remove",
            )
        if command in ("association", "relationship"):
            if args.operation == "list":
                return (
                    app.queries.associations(
                        **options(
                            args, "item", "file", "role", "active", "limit", "cursor"
                        )
                    )
                    if command == "association"
                    else app.queries.relationships(
                        **options(
                            args,
                            "item",
                            "direction",
                            "kind",
                            "active",
                            "limit",
                            "cursor",
                        )
                    )
                )
            if args.operation == "disable":
                return (
                    app.media.disable_association(args.id)
                    if command == "association"
                    else app.media.disable_relationship(args.id)
                )
            metadata = json.loads(args.metadata) if args.metadata is not None else None
            return (
                app.media.associate(
                    args.file,
                    args.item,
                    role=args.role,
                    part=args.part,
                    clear_part=args.clear_part,
                    metadata=metadata,
                )
                if command == "association"
                else app.media.relate(
                    args.source,
                    args.target,
                    args.kind,
                    position=args.position,
                    clear_position=args.clear_position,
                    metadata=metadata,
                )
            )
        if command == "status":
            return app.status()
        if command == "profile":
            return (
                app.add_profile(args.name)
                if args.operation == "add"
                else {"profiles": store.rows("SELECT id FROM profiles ORDER BY id")}
            )
        if command in ("location", "catalog"):
            kind = "source" if command == "location" else "output"
            if args.operation == "retained":
                if not 1 <= args.limit <= 1000:
                    raise CatabolicError("limit must be between 1 and 1000")
                Reconciler(app).catalogs(args.name)
                rows = store.rows(
                    "SELECT * FROM retained_hardlinks WHERE profile=? AND catalog=? AND id>? ORDER BY id LIMIT ?",
                    (args.profile, args.name, args.after or "", args.limit + 1),
                )
                more = len(rows) > args.limit
                rows = rows[: args.limit]
                return {
                    "retained": rows,
                    "next_after": rows[-1]["id"] if more else None,
                    "warning": "Recorded retained data is never automatically purged; inspect before any manual deletion.",
                }
            if args.operation == "bind":
                result = app.bind(
                    kind,
                    args.name,
                    args.root,
                    link_mode=getattr(args, "link_mode", None),
                )
                if kind == "output":
                    result = {**result, "link_mode": app.link_mode(args.name)}
                    if result["link_mode"] == "hardlink":
                        from .hardlinks import WARNING

                        result["warnings"] = [WARNING]
                return result
            rows = store.rows(
                "SELECT * FROM bindings WHERE profile=? AND kind=? ORDER BY owner",
                (args.profile, kind),
            )
            if kind == "output":
                rows = [
                    {**row, "link_mode": app.link_mode(row["owner"])} for row in rows
                ]
            return {"bindings": rows}
        if command == "scan":
            return app.scan(args.location, exclude=args.exclude)
        if command == "files":
            return app.files(
                **options(
                    args,
                    "catalog",
                    "unmapped",
                    "unidentified",
                    "search",
                    "location",
                    "status",
                    "item",
                    "kind",
                    "year",
                    "identity",
                    "tags",
                    "any_tags",
                    "not_tags",
                    "descendants",
                    "sort",
                    "descending",
                    "limit",
                    "cursor",
                )
            )
        if command == "item":
            if args.operation in workflow_cli.COMMANDS:
                return workflow_cli.dispatch(app, args)
            if args.operation == "list":
                return app.queries.items(
                    **options(
                        args,
                        "search",
                        "catalog",
                        "metadata",
                        "curation_status",
                        "kind",
                        "year",
                        "identity",
                        "tags",
                        "any_tags",
                        "not_tags",
                        "descendants",
                        "sort",
                        "descending",
                        "limit",
                        "cursor",
                    )
                )
            if args.operation == "show":
                return app.queries.item(
                    args.id,
                    **options(
                        args,
                        "identity",
                        "catalog",
                        "include_disabled",
                        "limit",
                        "cursor",
                    ),
                )
            identities = {}
            for raw in args.identity:
                namespace, separator, value = raw.partition("=")
                if not separator or not namespace or not value:
                    raise CatabolicError("identity must be NAMESPACE=VALUE")
                if namespace in identities and identities[namespace] != value:
                    raise CatabolicError("supply only one value per identity namespace")
                identities[namespace] = value
            try:
                metadata = json.loads(args.metadata)
            except ValueError as exc:
                raise CatabolicError("metadata must be valid JSON") from exc
            return app.put_item(args.kind, identities, metadata, args.id)
        if command == "mapping":
            if args.operation == "list":
                return app.queries.mappings(
                    catalog=None if args.all_catalogs else args.catalog,
                    **options(
                        args,
                        "item",
                        "file",
                        "active",
                        "search",
                        "sort",
                        "descending",
                        "limit",
                        "cursor",
                    ),
                )
            if args.operation == "disable":
                return app.disable_mapping(args.id)
            return app.put_mapping(args.catalog, args.file, args.item, args.path)
        reconciler = Reconciler(app)
        catalog = None if args.all_catalogs else args.catalog
        if command == "sync":
            limits = {
                "max_removals": args.max_removals,
                "max_removal_percent": args.max_removal_percent,
            }
            if args.dry_run:
                return reconciler.preview(catalog, **limits)
            return reconciler.apply(catalog, **limits)
        if command == "verify":
            return reconciler.verify(catalog)
        return reconciler.recover(catalog, cancel_unapplied=args.cancel_unapplied)


def read_text(path, maximum):
    if path == "-":
        value = sys.stdin.read(maximum + 1)
    else:
        with open(path, encoding="utf-8") as stream:
            value = stream.read(maximum + 1)
    if len(value.encode()) > maximum:
        raise CatabolicError(f"input exceeds {maximum} bytes")
    return value


def render(value: object, indent: int = 0) -> str:
    """Compact readable output; --json is the stable automation interface."""
    prefix = " " * indent
    if isinstance(value, dict):
        return "\n".join(
            f"{prefix}{key}:"
            + (
                "\n" + render(child, indent + 2)
                if isinstance(child, (dict, list))
                else f" {child}"
            )
            for key, child in value.items()
        )
    if isinstance(value, list):
        return (
            "\n".join(
                f"{prefix}-\n{render(child, indent + 2)}"
                if isinstance(child, (dict, list))
                else f"{prefix}- {child}"
                for child in value
            )
            or prefix + "(none)"
        )
    return prefix + str(value)


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    json_output = (
        args.json
        or args.command in ("graphql", "manifest", "spec", "target", "export")
        or getattr(args, "format", None) == "json"
    )
    try:
        result = dispatch(args)
        if (
            args.command == "rule"
            and args.operation in ("preview", "apply", "run")
            and not json_output
        ):
            from .rules import render_preview

            print(render_preview(result))
            return (
                3
                if result.get("complete") is False or result.get("safe") is False
                else 0
            )
        if args.command == "maintenance" and not json_output:
            from .maintenance import render_report

            print(render_report(result))
            return 0 if result["complete"] else 3
        if args.command == "export" and getattr(args, "raw", False):
            print(result["raw"], end="")
            return 0
        if args.command == "docs" and not args.json:
            from .documentation import render_documentation

            print(render_documentation(result))
            return 0
        print(
            result["markdown"]
            if args.command == "spec" and args.operation == "docs" and not args.json
            else json.dumps(
                result,
                indent=2,
                ensure_ascii=False,
                allow_nan=False,
                sort_keys=args.command == "spec" and args.operation == "schema",
            )
            if json_output
            else render_table(result)
            if args.command == "query" and "rows" in result
            else render(result),
            end=""
            if args.command == "spec" and args.operation == "docs" and not args.json
            else "\n",
        )
        if (
            args.command == "target"
            and args.operation == "import"
            and result.get("interrupted") is True
        ):
            return 130
        if args.command == "graphql" and result.get("errors"):
            return 2
        if args.command == "spec":
            # Unknown envelope fields are data, not this command's status.
            return 3 if args.operation == "diff" and not result["complete"] else 0
        if any(result.get(key) is False for key in ("safe", "healthy", "complete")):
            return 3
        return 0
    except (CatabolicError, OSError, sqlite3.Error, ValueError) as exc:
        error = {"error": {"message": str(exc), "type": type(exc).__name__}}
        print(json.dumps(error) if json_output else f"error: {exc}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        print("interrupted; use recover if an operation is pending", file=sys.stderr)
        return 130
