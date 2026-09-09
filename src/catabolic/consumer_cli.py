# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Headless consumer setup and bounded workers; network work owns no Store."""

import json
import time
from pathlib import Path

from .app import Application
from .consumer_adapters import ConsumerError, path, text
from .consumer_setup import bind, creation, discover, put_connection, verify_indexing
from .consumers import binding, drain, status
from .store import Store


def register(commands):
    sub = commands.add_parser(
        "consumer",
        help="Plex/Jellyfin connections, library bindings and durable scan delivery",
    ).add_subparsers(dest="operation", required=True)
    for op in ("plex-login", "plex-login-complete"):
        p = sub.add_parser(
            op, help="authorize Catabolic with Plex; no scans or library changes"
        )
        p.add_argument(
            "--credential-dir",
            default=str(Path.home() / ".config/catabolic/credentials"),
        )
        if op == "plex-login-complete":
            p.add_argument("id", help="login_id from plex-login")
    p = sub.add_parser(
        "import", help="preview/apply Plex metadata and file associations"
    )
    p.add_argument("connection")
    p.add_argument("--library-id", required=True)
    p.add_argument(
        "--map",
        required=True,
        help="JSON path mappings from Plex roots to scanned sources",
    )
    p.add_argument("--offset", type=int, default=0)
    p.add_argument("--limit", type=int, default=100)
    p.add_argument("--expected-plan", help="plan_id from a reviewed preview")
    p.add_argument("--apply", action="store_true")
    p = sub.add_parser("connection-put")
    p.add_argument("id")
    p.add_argument("--application", choices=("plex", "jellyfin"), required=True)
    p.add_argument("--endpoint", required=True)
    credentials = p.add_mutually_exclusive_group(required=True)
    credentials.add_argument("--credential-env")
    credentials.add_argument(
        "--credential-file", help="private JSON credential from plex-login-complete"
    )
    p.add_argument("--apply", action="store_true")
    p.add_argument("--repair", action="store_true")
    p = sub.add_parser("discover")
    p.add_argument("connection")
    p.add_argument("--type")
    for op in ("bind", "create"):
        p = sub.add_parser(op)
        p.add_argument("id", help="stable local binding/creation-intent ID")
        p.add_argument("--connection", required=True)
        p.add_argument("--catalog", required=True)
        p.add_argument("--subtree", default="")
        p.add_argument("--remote-root", required=True)
        p.add_argument("--type", required=True)
        p.add_argument("--apply", action="store_true")
        p.add_argument("--automatic", action="store_true")
        p.add_argument("--initial-scan", action="store_true")
        p.add_argument("--debounce", type=int, default=0)
        if op == "bind":
            p.add_argument("--library-id", required=True)
            p.add_argument("--rebind", action="store_true")
        else:
            p.add_argument(
                "--spec",
                required=True,
                help="local JSON name/type/root/scanner/agent/language",
            )
            p.add_argument(
                "--reconcile",
                action="store_true",
                help="reconcile uncertain creation without repeating POST",
            )
    for op in (
        "bindings",
        "connections",
        "attempts",
        "events",
        "creations",
        "publications",
    ):
        p = sub.add_parser(op)
        p.add_argument("--limit", type=int, default=100)
    for op in ("disable", "enable", "retry", "verify-indexing"):
        p = sub.add_parser(op)
        p.add_argument("id")
        if op == "verify-indexing":
            p.add_argument("--limit", type=int, default=100)
    for op in ("run", "watch"):
        p = sub.add_parser(op)
        p.add_argument("--limit", type=int, default=10)
        if op == "watch":
            p.add_argument("--interval", type=int, default=30)

    sub = commands.add_parser(
        "notify", help="optional per-destination Apprise summaries"
    ).add_subparsers(dest="operation", required=True)
    p = sub.add_parser("put")
    p.add_argument("id")
    p.add_argument("--credential-env", required=True)
    p.add_argument("--event", action="append", required=True)
    p.add_argument("--tag", action="append", default=[])
    p.add_argument("--severity", choices=("info", "warning", "error"), default="info")
    p.add_argument("--apply", action="store_true")
    for op in ("status", "run"):
        p = sub.add_parser(op)
        p.add_argument("--limit", type=int, default=100)
        if op == "run":
            p.add_argument("--tag")
    for op in ("disable", "retry"):
        sub.add_parser(op).add_argument("id", help="destination ID")


def dispatch(args):
    from . import notifications
    from .cli import read_text

    if args.command == "consumer" and args.operation in (
        "plex-login",
        "plex-login-complete",
    ):
        from . import plex_login

        if args.operation == "plex-login":
            return plex_login.start(args.credential_dir)
        return plex_login.complete(args.credential_dir, args.id)
    if not args.db:
        raise ConsumerError("database_required")
    op, database, profile = args.operation, args.db, args.profile
    if hasattr(args, "limit") and not 1 <= args.limit <= 1000:
        raise ConsumerError("invalid_configuration")
    if args.command == "notify":
        if op == "run":
            return notifications.drain(database, profile, args.limit, args.tag)
        with Store(
            database, writable=op != "status" and (op != "put" or args.apply)
        ) as store:
            app = Application(store, profile)
            if op == "put":
                return notifications.configure(
                    app,
                    args.id,
                    args.credential_env,
                    args.event,
                    args.tag,
                    args.severity,
                    apply=args.apply,
                )
            if op == "status":
                destinations = store.rows(
                    "SELECT * FROM notification_destinations WHERE profile=? ORDER BY id LIMIT ?",
                    (profile, args.limit + 1),
                )
                deliveries = store.rows(
                    "SELECT * FROM notification_deliveries WHERE profile=? ORDER BY event_id LIMIT ?",
                    (profile, args.limit + 1),
                )
                return {
                    "destinations": destinations[: args.limit],
                    "deliveries": deliveries[: args.limit],
                    "destinations_truncated": len(destinations) > args.limit,
                    "deliveries_truncated": len(deliveries) > args.limit,
                    "unverified_publications": notifications.pending_publications(
                        store, profile
                    ),
                    "complete": not notifications.pending_publications(store, profile)
                    and not store.rows(
                        "SELECT 1 FROM notification_deliveries WHERE profile=? AND state NOT IN ('complete','cancelled') LIMIT 1",
                        (profile,),
                    ),
                }
            if not store.rows(
                "SELECT 1 FROM notification_destinations WHERE profile=? AND id=?",
                (profile, args.id),
            ):
                raise ConsumerError("unknown_destination")
            with store.transaction() as db:
                if op == "disable":
                    db.execute(
                        "UPDATE notification_destinations SET enabled=0,revision=revision+1 WHERE profile=? AND id=?",
                        (profile, args.id),
                    )
                    db.execute(
                        "UPDATE notification_deliveries SET state='cancelled',lease_token=NULL,lease_until=NULL WHERE profile=? AND destination_id=? AND state!='complete'",
                        (profile, args.id),
                    )
                else:
                    db.execute(
                        "UPDATE notification_deliveries SET state='pending',attempts=0,due_at=0,error=NULL,lease_token=NULL,lease_until=NULL WHERE profile=? AND destination_id=? AND state NOT IN ('complete','cancelled')",
                        (profile, args.id),
                    )
            return {"id": args.id, "operation": op, "complete": True}
    if op == "import":
        from .plex_import import run

        return run(
            database,
            profile,
            args.connection,
            args.library_id,
            json.loads(read_text(args.map, 65536)),
            offset=args.offset,
            limit=args.limit,
            apply=args.apply,
            expected_plan=args.expected_plan,
        )
    if op == "connection-put":
        return put_connection(
            database,
            profile,
            args.id,
            args.application,
            args.endpoint,
            "file:" + str(Path(args.credential_file).expanduser().absolute())
            if args.credential_file
            else args.credential_env,
            apply=args.apply,
            repair=args.repair,
        )
    if op == "discover":
        return discover(database, profile, args.connection, args.type)
    if op in ("bind", "create"):
        origin = "adopted"
        if op == "create":
            spec = json.loads(read_text(args.spec, 65536))
            if not isinstance(spec, dict):
                raise ConsumerError("invalid_creation_spec")
            if spec.get("root") != args.remote_root or spec.get("type") != args.type:
                raise ConsumerError("creation_binding_mismatch")
            text(args.id, 255)
            path(args.subtree, relative=True)
            path(args.remote_root)
            if not 0 <= args.debounce <= 3600:
                raise ConsumerError("invalid_configuration")
            # Validate the local projection binding before authorizing remote creation.
            with Store(database) as store:
                Application(store, profile).binding("output", args.catalog)
                if store.rows(
                    "SELECT 1 FROM consumer_bindings WHERE profile=? AND id=?",
                    (profile, args.id),
                ) and not store.rows(
                    "SELECT 1 FROM consumer_creations WHERE profile=? AND id=?",
                    (profile, args.id),
                ):
                    raise ConsumerError("binding_id_already_exists")
            result = creation(
                database,
                profile,
                args.id,
                args.connection,
                spec,
                apply=args.apply,
                reconcile=args.reconcile,
            )
            if not result.get("complete") or "library" not in result:
                return result
            library_id, origin = result["library"]["id"], result["origin"]
        else:
            library_id = args.library_id
        return bind(
            database,
            profile,
            args.id,
            args.connection,
            args.catalog,
            args.subtree,
            args.remote_root,
            library_id,
            args.type,
            apply=args.apply or getattr(args, "reconcile", False),
            automatic=args.automatic,
            initial_scan=args.initial_scan,
            debounce=args.debounce,
            rebind=getattr(args, "rebind", False),
            origin=origin,
        )
    if op == "verify-indexing":
        return verify_indexing(database, profile, args.id, args.limit)
    if op in ("run", "watch"):
        if op == "watch" and not 1 <= args.interval <= 60:
            raise ConsumerError("invalid_configuration")
        while True:
            report = drain(database, profile, limit=args.limit)
            report["notifications"] = notifications.drain(
                database, profile, min(args.limit, 100)
            )
            if op == "run":
                return report
            from .program_cli import envelope

            print(
                json.dumps(
                    envelope(args, report, 0 if report["complete"] else 3)
                    if args.machine
                    else report
                ),
                flush=True,
            )
            time.sleep(args.interval)
    with Store(database, writable=op in ("disable", "enable", "retry")) as store:
        app = Application(store, profile)
        if op == "bindings":
            return status(app, args.limit)
        if op in ("connections", "attempts", "events", "creations", "publications"):
            table = {
                "connections": "consumer_connections",
                "attempts": "consumer_attempts",
                "events": "consumer_events",
                "creations": "consumer_creations",
                "publications": "publication_generations",
            }[op]
            rows = store.rows(
                f"SELECT * FROM {table} WHERE profile=? ORDER BY rowid DESC LIMIT ?",
                (profile, args.limit + 1),
            )
            return {
                op: rows[: args.limit],
                "truncated": len(rows) > args.limit,
                "listing_complete": len(rows) <= args.limit,
            }
        b = binding(app, args.id)
        with store.transaction() as db:
            if op in ("enable", "disable"):
                db.execute(
                    "UPDATE consumer_bindings SET generation=generation+CASE WHEN enabled=0 THEN 1 ELSE 0 END,enabled=?,revision=revision+1 WHERE profile=? AND id=?",
                    (int(op == "enable"), profile, args.id),
                )
            if op == "retry":
                db.execute(
                    "UPDATE consumer_bindings SET due_at=0 WHERE profile=? AND group_id=?",
                    (profile, b["group_id"]),
                )
            db.execute(
                "UPDATE consumer_deliveries SET state='pending',attempts=0,due_at=0,error=NULL,lease_token=NULL,lease_until=NULL WHERE profile=? AND id=?",
                (profile, b["group_id"]),
            )
            db.execute(
                "UPDATE consumer_deliveries SET state='cancelled' WHERE profile=? AND id=? AND NOT EXISTS (SELECT 1 FROM consumer_bindings b WHERE b.profile=consumer_deliveries.profile AND b.group_id=consumer_deliveries.id AND b.enabled=1)",
                (profile, b["group_id"]),
            )
        return {
            "id": args.id,
            "operation": op,
            "complete": True,
            "remote_library_deleted": False,
        }
