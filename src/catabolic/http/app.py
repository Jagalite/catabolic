# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Explicit authenticated HTTP routes over existing application services."""

import hashlib
import hmac
import json
import os
import sqlite3
import time
from contextlib import contextmanager
from typing import Annotated, Literal
from uuid import uuid4

from fastapi import FastAPI, Header, Query, Request
from fastapi.exceptions import RequestValidationError
from fastapi.openapi.models import OpenAPI
from fastapi.responses import JSONResponse, Response
from starlette.exceptions import HTTPException
from starlette.middleware.cors import CORSMiddleware
from starlette.middleware.trustedhost import TrustedHostMiddleware

from ..access import AccessError, authenticate, secret_pair
from ..access.catalog import Catalog
from ..access.graphql import AuthorizedContext
from ..api_events import page as event_page
from ..app import Application
from ..content_access import authorize, byte_range, chunks, opened, revision_of
from ..database_io import is_catalog_busy
from ..domain import CatabolicError
from ..graphql_query import execute_graphql
from ..rendition_requests import admit, cancel, retry, status
from ..saved_queries import Queries
from ..sql_query import execute_sql
from ..store import Store, encode
from . import models
from .contract import VERSION
from .models import (
    SQL,
    Definition,
    Demand,
    DemandStatus,
    GraphQL,
    MetadataPage,
    Page,
    Resolve,
    Ticket,
)
from .transport import BodyLimit, Limits, OwnedStream


def create_app(
    database,
    *,
    read_only=True,
    allowed_hosts=None,
    origins=None,
    streams=8,
    per_principal=2,
    rate=120,
):
    if any("*" in value for value in (allowed_hosts or []) + (origins or [])):
        raise CatabolicError("HTTP hosts and CORS origins must be explicit")
    # Validate schema without migration or exposure changes.
    with Store(database):
        pass
    app = FastAPI(
        title="Catabolic HTTP",
        version=VERSION,
        docs_url=None,
        redoc_url=None,
        openapi_url=None,
    )
    app.add_middleware(BodyLimit)
    app.add_middleware(
        TrustedHostMiddleware,
        allowed_hosts=allowed_hosts
        or ["localhost", "127.0.0.1", "[::1]", "testserver"],
    )
    if origins:
        app.add_middleware(
            CORSMiddleware,
            allow_origins=origins,
            allow_methods=["GET", "HEAD", "POST"],
            allow_headers=[
                "Authorization",
                "Content-Type",
                "Idempotency-Key",
                "Last-Event-ID",
            ],
            allow_credentials=False,
        )
    limits = Limits(streams, per_principal, rate)
    app.state.limits = limits

    def problem(exc, request):
        code = getattr(exc, "code", "catalog_error")
        status_code = getattr(exc, "status", 409)
        if is_catalog_busy(exc):
            code, status_code = "catalog_busy", 503
        elif isinstance(exc, sqlite3.Error):
            code, status_code = "catalog_error", 500
        request_id = str(uuid4())
        headers = {
            "X-Request-ID": request_id,
            "Cache-Control": "no-store",
            **getattr(exc, "headers", {}),
        }
        if status_code == 401:
            headers["WWW-Authenticate"] = "Bearer"
        if status_code in (429, 503):
            headers["Retry-After"] = "1"
        return JSONResponse(
            {
                "type": f"urn:catabolic:problem:{code}",
                "title": code.replace("_", " "),
                "status": status_code,
                "code": code,
                "request_id": request_id,
                "retryable": status_code in (429, 503),
                "remediation": "retry_later"
                if status_code in (429, 503)
                else "inspect_request_and_permissions",
            },
            status_code=status_code,
            headers=headers,
            media_type="application/problem+json",
        )

    @app.exception_handler(AccessError)
    @app.exception_handler(CatabolicError)
    @app.exception_handler(sqlite3.Error)
    @app.exception_handler(OSError)
    async def failure(request, exc):
        return problem(exc, request)

    @app.exception_handler(HTTPException)
    async def protocol_error(request, exc):
        return problem(
            AccessError(
                "invalid_request" if exc.status_code == 400 else "http_error",
                exc.status_code,
            ),
            request,
        )

    @app.exception_handler(RequestValidationError)
    async def invalid(request, exc):
        return problem(AccessError("invalid_request", 422), request)

    def bearer(request):
        header = request.headers.get("authorization", "")
        if not header.startswith("Bearer "):
            raise AccessError("unauthorized", 401)
        return header[7:]

    @contextmanager
    def session(request, *, write=False):
        with Store(database, writable=write) as store:
            access = authenticate(store, bearer(request))
            limits.request(access.principal)
            if write:
                from ..api_events import prune

                prune(store)
            yield access

    def mutation():
        if read_only:
            raise AccessError("read_only", 403)

    @app.get(
        "/v1/openapi.json",
        operation_id="get_openapi",
        response_model=OpenAPI,
        response_model_exclude_unset=True,
    )
    def openapi(request: Request):
        with session(request):
            return app.openapi()

    @app.get(
        "/v1/me",
        operation_id="get_identity",
        response_model=models.Identity,
        response_model_exclude_unset=True,
    )
    def me(request: Request):
        with session(request) as access:
            return {
                "principal": access.principal,
                "profile": access.profile,
                "actions": sorted(
                    {action for grant in access.grants for action in grant["actions"]}
                ),
                "expires": access.token["expires"],
            }

    @app.get(
        "/v1/capabilities",
        operation_id="get_capabilities",
        response_model=models.Capabilities,
        response_model_exclude_unset=True,
    )
    def capabilities(request: Request):
        with session(request) as access:
            workers = access.store.rows(
                "SELECT worker_pid FROM api_worker_status WHERE profile=?",
                (access.profile,),
            )
            ready = False
            if workers:
                try:
                    os.kill(workers[0]["worker_pid"], 0)
                    ready = True
                except OSError:
                    pass
            operations = [
                r["id"]
                for r in access.store.rows(
                    "SELECT id FROM api_operations WHERE profile=? AND enabled=1",
                    (access.profile,),
                )
                if any(
                    g.get("operator") or r["id"] in g.get("operation_ids", [])
                    for g in access.matching("processing:request")
                )
            ]
            return {
                "version": 1,
                "schema": access.store.schema_version,
                "read_only": read_only,
                "processing_admission": not read_only
                and ready
                and bool(access.matching("processing:request")),
                "worker_ready": ready,
                "operations": operations,
                "limits": {
                    "page_size": 1000,
                    "streams": streams,
                    "streams_per_principal": per_principal,
                    "stream_buffer_bytes": 65536,
                    "requests_per_minute": rate,
                },
                "content": {
                    "ranges": "single",
                    "etag": "weak_revision",
                    "in_place_mutation": "abort_remaining_transfer",
                },
            }

    @app.post(
        "/v1/query/sql",
        operation_id="execute_sql",
        response_model=models.SQLResult,
        response_model_exclude_unset=True,
    )
    def sql(body: SQL, request: Request):
        with session(request) as access:
            if not access.operator("sql:read"):
                raise AccessError()
            return execute_sql(
                database,
                body.query,
                profile=access.profile,
                params=encode(body.params),
                max_rows=body.limit,
                _store=access.store,
                _http=True,
            )

    @app.post(
        "/v1/query/graphql",
        operation_id="execute_graphql",
        response_model=models.GraphQLResult,
        response_model_exclude_unset=True,
    )
    def graphql(body: GraphQL, request: Request):
        with session(request) as access:
            access.require("metadata:read")
            return execute_graphql(
                database,
                body.query,
                profile=access.profile,
                variables=body.variables,
                operation_name=body.operationName,
                _store=access.store,
                _context_factory=lambda s, p, d: AuthorizedContext(s, p, d, access),
            )

    @app.post(
        "/v1/queries/{revision_id}/runs",
        operation_id="run_saved_query",
        response_model=models.SelectionResult | models.SQLResult | models.GraphQLResult,
        response_model_exclude_unset=True,
    )
    def saved(revision_id: str, body: Page, request: Request):
        with session(request) as access:
            access.require("metadata:read")
            if not (
                access.operator("metadata:read")
                or access.operator("sql:read")
                or any(
                    revision_id in g.get("report_ids", [])
                    or g.get("query_id") == revision_id
                    for g in access.matching("metadata:read")
                )
            ):
                raise AccessError("not_found", 404)
            query = Queries(access.store, access.profile).get(revision_id)
            definition = query["definition"]
            approved = access.operator("sql:read") or any(
                revision_id in g.get("report_ids", [])
                for g in access.matching("metadata:read")
            )
            if definition["mode"] == "rows":
                if not approved:
                    raise AccessError()
                value = definition["selection"]
                return execute_sql(
                    database,
                    value["query"],
                    params=encode(value.get("params", {})),
                    profile=access.profile,
                    max_rows=body.limit,
                    _store=access.store,
                    _http=True,
                )
            if definition["mode"] == "document":
                value = definition["selection"]
                return execute_graphql(
                    database,
                    value["query"],
                    profile=access.profile,
                    variables=value.get("variables", {}),
                    _store=access.store,
                    _context_factory=lambda s, p, d: AuthorizedContext(s, p, d, access),
                )
            if (
                not approved
                and not access.operator("metadata:read")
                and not any(g.get("query_id") == revision_id for g in access.grants)
            ):
                raise AccessError()
            entity, ids, report = Queries(access.store, access.profile).select(
                revision_id, _http=True
            )
            if not report["complete"]:
                raise AccessError("selection_incomplete", 422)
            catalog = Catalog(access)
            table = {
                "item_id": "items",
                "file_id": "files",
                "association_id": "item_files",
            }[entity]
            allowed = [
                row["id"]
                for row in access.store.rows(
                    "SELECT id FROM "
                    + table
                    + " WHERE id IN (SELECT value FROM json_each(?)) ORDER BY id",
                    (encode(sorted(ids)),),
                )
            ]
            after = catalog.after(["saved", revision_id], body.cursor)
            remaining = [i for i in allowed if i > after]
            selected = remaining[: body.limit]
            return {
                "mode": "selection",
                "entity": entity,
                "ids": selected,
                "page_complete": True,
                "has_more": len(remaining) > body.limit,
                "selection_complete": len(remaining) <= body.limit,
                "next_cursor": catalog.cursor(["saved", revision_id], selected[-1])
                if len(remaining) > body.limit
                else None,
            }

    @app.get(
        "/v1/items",
        operation_id="list_items",
        response_model=MetadataPage[models.Item],
        response_model_exclude_unset=True,
    )
    def items(
        request: Request,
        limit: Annotated[int, Query(ge=1, le=1000)] = 100,
        cursor: str | None = None,
    ):
        with session(request) as access:
            return Catalog(access).listing("items", limit, cursor)

    @app.post(
        "/v1/items/snapshots",
        operation_id="create_item_snapshot",
        response_model=models.Snapshot,
        response_model_exclude_unset=True,
    )
    def snapshot_items(request: Request):
        with session(request, write=True) as access:
            access.require("metadata:read")
            Catalog(access)
            ids = [
                r["id"]
                for r in access.store.rows(
                    "SELECT id FROM items ORDER BY id LIMIT 10001"
                )
            ]
            if len(ids) > 10000:
                raise AccessError("snapshot_limit", 422)
            identifier = str(uuid4())
            expires = time.time() + 900
            with access.store.transaction() as db:
                db.execute(
                    "INSERT INTO api_snapshots VALUES (?,?,?,?,?)",
                    (identifier, access.principal, "items", encode(ids), expires),
                )
            return {
                "snapshot_id": identifier,
                "expires": expires,
                "selection_complete": True,
            }

    @app.get(
        "/v1/snapshots/{identifier}",
        operation_id="get_snapshot_page",
        response_model=MetadataPage[models.Item],
        response_model_exclude_unset=True,
    )
    def snapshot_page(
        identifier: str,
        request: Request,
        limit: Annotated[int, Query(ge=1, le=1000)] = 100,
        cursor: str | None = None,
    ):
        with session(request) as access:
            if not 1 <= limit <= 1000:
                raise AccessError("invalid_page", 400)
            rows = access.store.rows(
                "SELECT ids FROM api_snapshots WHERE id=? AND principal=? AND scope='items' AND expires>?",
                (identifier, access.principal, time.time()),
            )
            if not rows:
                raise AccessError("not_found", 404)
            catalog = Catalog(access)
            after = catalog.after(["snapshot", identifier], cursor)
            values = access.store.rows(
                "SELECT * FROM items WHERE id>? AND id IN (SELECT value FROM json_each(?)) ORDER BY id LIMIT ?",
                (after, rows[0]["ids"], limit + 1),
            )
            return {
                "data": [catalog.present("items", v) for v in values[:limit]],
                "page_complete": True,
                "has_more": len(values) > limit,
                "selection_complete": len(values) <= limit,
                "next_cursor": catalog.cursor(
                    ["snapshot", identifier], values[limit - 1]["id"]
                )
                if len(values) > limit
                else None,
            }

    @app.get(
        "/v1/items/{identifier}",
        operation_id="get_item",
        response_model=models.Item,
        response_model_exclude_unset=True,
    )
    def item(identifier: str, request: Request):
        with session(request) as access:
            return Catalog(access).get("items", identifier)

    @app.get(
        "/v1/files",
        operation_id="list_files",
        response_model=MetadataPage[models.File],
        response_model_exclude_unset=True,
    )
    def files(
        request: Request,
        limit: Annotated[int, Query(ge=1, le=1000)] = 100,
        cursor: str | None = None,
    ):
        with session(request) as access:
            return Catalog(access).listing("files", limit, cursor)

    @app.get(
        "/v1/files/{identifier}",
        operation_id="get_file",
        response_model=models.File,
        response_model_exclude_unset=True,
    )
    def file(identifier: str, request: Request):
        with session(request) as access:
            return Catalog(access).get("files", identifier)

    @app.get(
        "/v1/renditions",
        operation_id="list_renditions",
        response_model=MetadataPage[models.Rendition],
        response_model_exclude_unset=True,
    )
    def renditions(
        request: Request,
        limit: Annotated[int, Query(ge=1, le=1000)] = 100,
        cursor: str | None = None,
    ):
        with session(request) as access:
            return Catalog(access).listing("renditions", limit, cursor)

    @app.get(
        "/v1/renditions/{identifier}",
        operation_id="get_rendition",
        response_model=models.Rendition,
        response_model_exclude_unset=True,
    )
    def rendition(identifier: str, request: Request):
        with session(request) as access:
            return Catalog(access).get("renditions", identifier)

    @app.post(
        "/v1/items/{identifier}/resolve",
        operation_id="resolve_item",
        response_model=models.ResolvedContent | models.Decision,
        response_model_exclude_unset=True,
    )
    def resolve(identifier: str, body: Resolve, request: Request):
        if body.fallback_policy_id:
            from .fallback import logical_resolve

            if body.file_id or body.definition_id:
                raise AccessError("exact_and_logical_selection_conflict", 422)
            with session(request, write=True) as access:
                return logical_resolve(access, identifier, body)
        with session(request) as access:
            if not access.item(identifier):
                raise AccessError("not_found", 404)
            rows = access.store.rows(
                "SELECT file_id FROM item_files WHERE item_id=? AND active=1 ORDER BY id LIMIT 1001",
                (identifier,),
            )
            rows += access.store.rows(
                "SELECT file_id FROM media_outputs WHERE source_item_id=? AND profile=? ORDER BY id LIMIT 1001",
                (identifier, access.profile),
            )
            for row in rows:
                file_id = row["file_id"]
                if body.file_id and file_id != body.file_id:
                    continue
                if body.definition_id and not access.store.rows(
                    "SELECT 1 FROM media_outputs WHERE file_id=? AND definition_id=? AND profile=?",
                    (file_id, body.definition_id, access.profile),
                ):
                    continue
                revision = revision_of(access.store, access.profile, file_id)
                try:
                    with opened(access, file_id, revision):
                        pass
                except (AccessError, CatabolicError, OSError):
                    continue
                return {
                    "file_id": file_id,
                    "revision": revision,
                    "content_path": f"/v1/files/{file_id}/content?revision={revision}",
                }
            raise AccessError("no_eligible_content", 409)

    @app.get(
        "/v1/projections/{identifier}/entries",
        operation_id="list_projection_entries",
        response_model=models.ProjectionPage,
        response_model_exclude_unset=True,
    )
    def entries(
        identifier: str,
        request: Request,
        limit: Annotated[int, Query(ge=1, le=1000)] = 100,
        cursor: str | None = None,
    ):
        with session(request) as access:
            catalog = Catalog(access)
            if not access.store.rows(
                "SELECT 1 FROM catalogs WHERE id=?", (identifier,)
            ):
                raise AccessError("not_found", 404)
            if not 1 <= limit <= 1000:
                raise AccessError("invalid_page", 400)
            after = catalog.after(["projection", identifier], cursor)
            rows = access.store.rows(
                "SELECT id,item_id,file_id,path FROM mappings WHERE catalog=? AND active=1 AND id>? ORDER BY id LIMIT ?",
                (identifier, after, limit + 1),
            )
            return {
                "data": rows[:limit],
                "page_complete": True,
                "has_more": len(rows) > limit,
                "next_cursor": catalog.cursor(
                    ["projection", identifier], rows[limit - 1]["id"]
                )
                if len(rows) > limit
                else None,
            }

    @app.post(
        "/v1/content-access",
        operation_id="create_content_ticket",
        response_model=models.ContentTicket,
        response_model_exclude_unset=True,
    )
    def ticket(body: Ticket, request: Request):
        with session(request, write=True) as access:
            authorize(access, body.file_id, body.revision)
            identifier, secret, digest = secret_pair()
            expires = min(time.time() + body.ttl, access.token["expires"])
            with access.store.transaction() as db:
                count = db.execute(
                    "SELECT count(*) FROM api_tickets WHERE token_id IN (SELECT id FROM api_tokens WHERE principal=?) AND expires>? AND revoked=0",
                    (access.principal, time.time()),
                ).fetchone()[0]
                if count >= 256:
                    raise AccessError("ticket_limit", 429)
                db.execute(
                    "INSERT INTO api_tickets(id,digest,token_id,file_id,revision,expires,remaining_bytes) VALUES (?,?,?,?,?,?,?)",
                    (
                        identifier,
                        digest,
                        access.token["id"],
                        body.file_id,
                        body.revision,
                        expires,
                        body.max_bytes,
                    ),
                )
            return {
                "ticket_id": identifier,
                "expires": expires,
                "content_path": f"/v1/files/{body.file_id}/content?revision={body.revision}&ticket={secret}",
            }

    @app.post(
        "/v1/content-access/{identifier}/revoke",
        operation_id="revoke_content_ticket",
        response_model=models.Revocation,
        response_model_exclude_unset=True,
    )
    def revoke_ticket(identifier: str, request: Request):
        with session(request, write=True) as access:
            with access.store.transaction() as db:
                db.execute(
                    "UPDATE api_tickets SET revoked=1 WHERE id=? AND token_id IN (SELECT id FROM api_tokens WHERE principal=?)",
                    (identifier, access.principal),
                )
            return {"revoked": True}

    def content(identifier, request, revision, ticket, rendition=False):
        store = Store(database, writable=bool(ticket))
        owned = None
        admitted = False
        try:
            if ticket:
                if len(ticket) > 256:
                    raise AccessError("unauthorized", 401)
                rows = store.rows(
                    "SELECT * FROM api_tickets WHERE id=?", (ticket.split(".", 1)[0],)
                )
                if (
                    not rows
                    or not hmac.compare_digest(
                        rows[0]["digest"], hashlib.sha256(ticket.encode()).hexdigest()
                    )
                    or rows[0]["expires"] <= time.time()
                    or rows[0]["revoked"]
                ):
                    raise AccessError("unauthorized", 401)
                grant = rows[0]
                access = authenticate(store, token_id=grant["token_id"])
            else:
                access = authenticate(store, bearer(request))
            limits.request(access.principal)
            if rendition:
                rows = store.rows(
                    "SELECT file_id FROM media_outputs WHERE id=? AND profile=?",
                    (identifier, access.profile),
                )
                if not rows:
                    raise AccessError("not_found", 404)
                identifier = rows[0]["file_id"]
            if ticket and (
                grant["file_id"] != identifier or grant["revision"] != revision
            ):
                raise AccessError("not_found", 404)
            owned = opened(access, identifier, revision)
            fd, stat, mime = owned.__enter__()
            etag = f'W/"{revision}"'
            headers = {
                "ETag": etag,
                "Accept-Ranges": "bytes",
                "Cache-Control": "private, no-store",
                "X-Content-Type-Options": "nosniff",
                "Referrer-Policy": "no-referrer",
            }
            if mime in ("text/html", "image/svg+xml", "application/xhtml+xml"):
                headers["Content-Disposition"] = "attachment"
            if request.headers.get("if-match") not in (None, "*"):
                raise AccessError("precondition_failed", 412)
            if any(
                value.strip().removeprefix("W/") in (etag.removeprefix("W/"), "*")
                for value in request.headers.get("if-none-match", "").split(",")
            ):
                owned.__exit__(None, None, None)
                owned = None
                return Response(status_code=304, headers=headers)
            range_header = (
                request.headers.get("range") if request.method == "GET" else None
            )
            if request.headers.get("if-range"):
                range_header = None  # Weak revision validators cannot satisfy If-Range.
            try:
                start, length, partial = byte_range(range_header, stat.st_size)
            except AccessError as exc:
                exc.headers = {"Content-Range": f"bytes */{stat.st_size}"}
                raise
            headers["Content-Length"] = str(length)
            if partial:
                headers["Content-Range"] = (
                    f"bytes {start}-{start + length - 1}/{stat.st_size}"
                )
            if request.method != "HEAD":
                limits.admit(access.principal)
                admitted = True
            if ticket and request.method != "HEAD":
                with store.transaction() as db:
                    changed = db.execute(
                        "UPDATE api_tickets SET remaining_bytes=remaining_bytes-? WHERE id=? AND remaining_bytes>=?",
                        (length, grant["id"], length),
                    ).rowcount
                    if not changed:
                        raise AccessError("ticket_transfer_limit", 429)
            store.close()
            if request.method == "HEAD":
                owned.__exit__(None, None, None)
                owned = None
                return Response(
                    status_code=206 if partial else 200,
                    headers=headers,
                    media_type=mime,
                )

            def cleanup():
                try:
                    owned.__exit__(None, None, None)
                finally:
                    limits.release(access.principal)

            return OwnedStream(
                chunks(fd, start, length, stat),
                status_code=206 if partial else 200,
                media_type=mime,
                headers=headers,
                cleanup=cleanup,
            )
        except BaseException:
            try:
                if owned:
                    owned.__exit__(None, None, None)
            finally:
                if admitted:
                    limits.release(access.principal)
            raise
        finally:
            store.close()

    @app.get(
        "/v1/files/{identifier}/content",
        operation_id="get_file_content",
        response_class=Response,
    )
    @app.head(
        "/v1/files/{identifier}/content",
        operation_id="head_file_content",
        response_class=Response,
    )
    def file_content(
        identifier: str, request: Request, revision: str, ticket: str | None = None
    ):
        return content(identifier, request, revision, ticket)

    @app.get(
        "/v1/renditions/{identifier}/content",
        operation_id="get_rendition_content",
        response_class=Response,
    )
    @app.head(
        "/v1/renditions/{identifier}/content",
        operation_id="head_rendition_content",
        response_class=Response,
    )
    def rendition_content(
        identifier: str, request: Request, revision: str, ticket: str | None = None
    ):
        return content(identifier, request, revision, ticket, True)

    @app.post(
        "/v1/rendition-requests",
        response_model=DemandStatus,
        responses={202: {"model": DemandStatus}},
        operation_id="create_rendition_request",
    )
    def demand(
        body: Demand,
        request: Request,
        response: Response,
        idempotency_key: Annotated[
            str | None,
            Header(
                alias="Idempotency-Key",
                description="Required for processing admission; missing keys are rejected by admission.",
            ),
        ] = None,
    ):
        mutation()
        with session(request, write=True) as access:
            workers = access.store.rows(
                "SELECT * FROM api_worker_status WHERE profile=?", (access.profile,)
            )
            if not workers:
                raise AccessError("worker_unavailable", 409)
            result = admit(
                access,
                body.model_dump(),
                idempotency_key,
                json.loads(workers[0]["capabilities"]),
            )
            response.status_code = 200 if result["state"] == "ready" else 202
            response.headers["Location"] = result["status_url"]
            return result

    @app.get(
        "/v1/rendition-requests/{identifier}",
        response_model=DemandStatus,
        operation_id="get_rendition_request",
    )
    def demand_status(identifier: str, request: Request):
        with session(request) as access:
            return status(access, identifier)

    @app.post(
        "/v1/rendition-requests/{identifier}/cancel",
        response_model=DemandStatus,
        operation_id="cancel_rendition_request",
    )
    def demand_cancel(identifier: str, request: Request):
        mutation()
        with session(request, write=True) as access:
            return cancel(access, identifier)

    @app.post(
        "/v1/rendition-requests/{identifier}/retry",
        response_model=DemandStatus,
        operation_id="retry_rendition_request",
    )
    def demand_retry(identifier: str, request: Request):
        mutation()
        with session(request, write=True) as access:
            return retry(access, identifier)

    @app.get(
        "/v1/events",
        operation_id="list_events",
        response_model=models.EventPage,
        response_model_exclude_unset=True,
    )
    def events(request: Request, cursor: str | None = None, follow: bool = False):
        with session(request, write=True) as access:
            initial = event_page(access, cursor or request.headers.get("last-event-id"))
        if not follow:
            return initial
        secret = bearer(request)

        def stream():
            result = initial
            replay_cursor = initial["cursor"]
            for _ in range(60):
                if result is not None:
                    replay_cursor = result["cursor"]
                    yield (
                        "id: "
                        + replay_cursor
                        + "\nevent: catalog\ndata: "
                        + encode(result["events"])
                        + "\n\n"
                    )
                result = None
                time.sleep(1)
                try:
                    with Store(database, writable=True) as store:
                        current_access = authenticate(store, secret)
                        result = event_page(current_access, replay_cursor)
                except (CatabolicError, sqlite3.Error) as exc:
                    if is_catalog_busy(exc):
                        yield ": catalog-busy\n\n"
                        continue
                    if not isinstance(exc, AccessError):
                        raise
                    yield (
                        "event: resync-required\ndata: "
                        + encode({"code": exc.code})
                        + "\n\n"
                    )
                    return

        principal = access.principal
        limits.admit(principal)
        return OwnedStream(
            stream(),
            media_type="text/event-stream",
            headers={"Cache-Control": "no-store"},
            cleanup=lambda: limits.release(principal),
        )

    @app.post(
        "/v1/operator/{resource}/{name}",
        operation_id="manage_definition",
        response_model=models.OperatorPreview | models.OperatorApplied,
        response_model_exclude_unset=True,
    )
    def operator(
        resource: Literal["queries", "rules", "projections"],
        name: str,
        body: Definition,
        request: Request,
    ):
        mutation()
        with session(request, write=True) as access:
            if not access.operator("operator:write"):
                raise AccessError()
            if resource not in ("queries", "rules", "projections"):
                raise AccessError("not_found", 404)
            # Definition-management plans pin all relevant program definitions.
            from ..projections import Projections
            from ..rules import Rules

            tables = [
                "saved_queries",
                "processing_rules",
                "projection_bindings",
                "processing_recipes",
                "copy_policies",
                "rendition_policies",
                "catalog_refresh_settings",
                "bindings",
                "output_definitions",
            ]
            state = []
            for table in tables:
                rows = access.store.rows(
                    "SELECT * FROM " + table + " ORDER BY rowid LIMIT 10001"
                )
                if len(rows) > 10000:
                    raise AccessError("operator_plan_limit", 422)
                state.append(rows)
            digest = hashlib.sha256(
                encode(
                    [access.profile, resource, name, body.definition, state]
                ).encode()
            ).hexdigest()
            if body.apply and body.expected_plan != digest:
                raise AccessError("stale_plan", 409)
            application = Application(access.store, access.profile)

            def put():
                if resource == "queries":
                    return Queries(access.store, access.profile).put(
                        name, body.definition
                    )
                service = (
                    Rules(application)
                    if resource == "rules"
                    else Projections(application)
                )
                import inspect

                try:
                    inspect.signature(service.put).bind(name, **body.definition)
                except TypeError:
                    raise AccessError("invalid_definition", 422) from None
                return service.put(name, **body.definition)

            # Existing services validate the actual definition in a rollback-only
            # transaction for preview. Definition edits have no filesystem effects.
            class PreviewComplete(Exception):
                pass

            try:
                with access.store.transaction() as db:
                    result = put()
                    if not body.apply:
                        raise PreviewComplete()
                    db.execute(
                        "INSERT INTO api_audit(principal,action,resource) VALUES (?,?,?)",
                        (access.principal, "operator." + resource, name),
                    )
            except PreviewComplete:
                return {
                    "plan_id": digest,
                    "definition": body.definition,
                    "applied": False,
                }
            return {"applied": True, "result": result}

    from .fallback import install_routes

    install_routes(app, session, mutation)
    from .contract import install_contract

    install_contract(app)
    return app
