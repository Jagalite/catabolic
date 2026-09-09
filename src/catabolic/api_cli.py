# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Local HTTP administration; no web dependency is required for credentials."""

import json
import os
from pathlib import Path
from uuid import uuid4

from .access import grant_put, issue, principal_put, revoke
from .app import Application
from .artifacts import Artifacts
from .domain import CatabolicError
from .store import Store


def register(commands):
    api = commands.add_parser(
        "api",
        help="optional authenticated HTTP backend and local access administration",
    ).add_subparsers(dest="operation", required=True)
    serve = api.add_parser("serve")
    serve.add_argument("--host", default="127.0.0.1")
    serve.add_argument("--port", type=int, default=8421)
    serve.add_argument("--allow-network", action="store_true")
    serve.add_argument("--tls-cert")
    serve.add_argument("--tls-key")
    serve.add_argument("--trusted-local-transport", action="store_true")
    serve.add_argument("--allowed-host", action="append")
    serve.add_argument("--cors-origin", action="append")
    serve.add_argument(
        "--trusted-proxy",
        action="append",
        help="explicit proxy IP; forwarded headers are otherwise ignored",
    )
    serve.add_argument("--enable-processing", action="store_true")
    serve.add_argument("--streams", type=int, default=8)
    serve.add_argument("--streams-per-principal", type=int, default=2)
    worker = api.add_parser("worker")
    worker.add_argument("--max-render-jobs", type=int, choices=[1], default=1)
    worker.add_argument("--once", action="store_true")
    principal = api.add_parser("principal-put")
    principal.add_argument("id")
    grant = api.add_parser("grant-put")
    grant.add_argument("principal")
    grant.add_argument("--file", required=True)
    grant.add_argument("--id")
    token = api.add_parser("token-create")
    token.add_argument("principal")
    token.add_argument("--grant", action="append", required=True)
    token.add_argument("--ttl", type=int, default=3600)
    token.add_argument(
        "--output",
        required=True,
        help="new private credential JSON file; never print the secret",
    )
    rotate = api.add_parser("token-rotate")
    rotate.add_argument("id")
    rotate.add_argument("--ttl", type=int, default=3600)
    rotate.add_argument("--output", required=True)
    revoke_parser = api.add_parser("token-revoke")
    revoke_parser.add_argument("id")
    api.add_parser("tokens")
    for operation in ("principal-disable", "grant-disable", "operation-disable"):
        api.add_parser(operation).add_argument("id")
    expose = api.add_parser("source-expose")
    expose.add_argument("location")
    expose.add_argument("--disable", action="store_true")
    operation = api.add_parser("operation-approve")
    operation.add_argument("recipe_id")
    operation.add_argument("--location", required=True)
    operation.add_argument("--id")


def save_secret(store, filename, result):
    path = Path(filename).expanduser().absolute()
    fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w") as stream:
        json.dump(result, stream)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    st = path.stat()
    with store.transaction() as db:
        db.execute(
            "INSERT OR REPLACE INTO api_private_files VALUES (?,?,?)",
            (str(path), st.st_dev, st.st_ino),
        )
    return {
        "token_id": result["token_id"],
        "expires": result["expires"],
        "credential_file": str(path),
    }


def dispatch(args):
    if not args.db:
        raise CatabolicError("select --db or CATABOLIC_DB")
    if args.operation == "serve":
        if (
            args.host not in ("127.0.0.1", "::1", "localhost")
            and not args.allow_network
        ):
            raise CatabolicError("network listening requires --allow-network")
        if args.host not in ("127.0.0.1", "::1", "localhost") and not (
            args.tls_cert and args.tls_key or args.trusted_local_transport
        ):
            raise CatabolicError(
                "network listening requires TLS or explicit trusted local transport"
            )
        if (
            bool(args.tls_cert) != bool(args.tls_key)
            or not 1 <= args.port <= 65535
            or not 1 <= args.streams <= 128
            or not 1 <= args.streams_per_principal <= args.streams
        ):
            raise CatabolicError("invalid server configuration")
        if args.trusted_proxy:
            from ipaddress import ip_network

            try:
                for address in args.trusted_proxy:
                    ip_network(address, strict=False)
            except ValueError:
                raise CatabolicError(
                    "trusted proxies must be explicit IP addresses or networks"
                ) from None
        try:
            import uvicorn

            from .http.app import create_app
        except ImportError:
            raise CatabolicError("install catabolic[http] to serve HTTP") from None
        with Store(args.db):
            pass
        if args.trusted_proxy and "*" in args.trusted_proxy:
            raise CatabolicError("trusted proxies must be explicit IP addresses")
        uvicorn.run(
            create_app(
                args.db,
                read_only=not args.enable_processing,
                allowed_hosts=args.allowed_host,
                origins=args.cors_origin,
                streams=args.streams,
                per_principal=args.streams_per_principal,
            ),
            host=args.host,
            port=args.port,
            workers=1,
            proxy_headers=bool(args.trusted_proxy),
            forwarded_allow_ips=",".join(args.trusted_proxy or []),
            access_log=False,
            ssl_certfile=args.tls_cert,
            ssl_keyfile=args.tls_key,
        )
        return {"stopped": True}
    if args.operation == "worker":
        from .http_worker_local import run

        return run(args.db, args.profile, once=args.once)
    with Store(args.db, writable=args.operation != "tokens") as store:
        if args.operation in (
            "principal-disable",
            "grant-disable",
            "operation-disable",
        ):
            table = {
                "principal-disable": "api_principals",
                "grant-disable": "api_grants",
                "operation-disable": "api_operations",
            }[args.operation]
            with store.transaction() as db:
                changed = db.execute(
                    f"UPDATE {table} SET enabled=0 WHERE id=?", (args.id,)
                ).rowcount
            return {"disabled": bool(changed)}
        if args.operation == "principal-put":
            return principal_put(store, args.profile, args.id)
        if args.operation == "grant-put":
            from .cli import read_text

            return grant_put(
                store, args.principal, json.loads(read_text(args.file, 262144)), args.id
            )
        if args.operation == "token-create":
            return save_secret(
                store, args.output, issue(store, args.principal, args.grant, args.ttl)
            )
        if args.operation == "token-revoke":
            return revoke(store, args.id)
        if args.operation == "token-rotate":
            rows = store.rows(
                "SELECT principal,grants FROM api_tokens WHERE id=? AND revoked=0",
                (args.id,),
            )
            if not rows:
                raise CatabolicError("unknown active token")
            result = issue(
                store, rows[0]["principal"], json.loads(rows[0]["grants"]), args.ttl
            )
            saved = save_secret(store, args.output, result)
            revoke(store, args.id)
            return saved
        if args.operation == "tokens":
            return {
                "tokens": store.rows(
                    "SELECT t.id,t.principal,t.expires,t.revoked FROM api_tokens t JOIN api_principals p ON p.id=t.principal WHERE p.profile=? ORDER BY t.id LIMIT 1000",
                    (args.profile,),
                )
            }
        if args.operation == "source-expose":
            Application(store, args.profile).binding("source", args.location)
            with store.transaction() as db:
                if args.disable:
                    db.execute(
                        "DELETE FROM api_sources WHERE profile=? AND location=?",
                        (args.profile, args.location),
                    )
                else:
                    db.execute(
                        "INSERT INTO api_sources VALUES (?,?) ON CONFLICT DO NOTHING",
                        (args.profile, args.location),
                    )
            return {"location": args.location, "exposed": not args.disable}
        if args.operation == "operation-approve":
            artifacts = Artifacts(Application(store, args.profile))
            artifacts.get_recipe(args.recipe_id)
            if not store.rows(
                "SELECT 1 FROM generated_locations WHERE profile=? AND location=?",
                (args.profile, args.location),
            ):
                raise CatabolicError("approve an existing generated destination")
            identifier = args.id or str(uuid4())
            with store.transaction() as db:
                db.execute(
                    "INSERT INTO api_operations(id,profile,recipe_id,location) VALUES (?,?,?,?)",
                    (identifier, args.profile, args.recipe_id, args.location),
                )
            return {
                "operation_id": identifier,
                "recipe_id": args.recipe_id,
                "location": args.location,
            }
        raise CatabolicError("unknown API operation")
