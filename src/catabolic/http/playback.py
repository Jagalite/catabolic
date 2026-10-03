# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Explicit playback admission and authenticated HLS playlist/segment delivery."""

from typing import Annotated

from fastapi import Header, Request
from fastapi.responses import Response

from .. import playback
from ..access import AccessError
from ..content_access import chunks
from .models import PlaybackDemand, PlaybackStatus
from .transport import OwnedStream


def install_routes(app, session, mutation, limits):
    @app.post(
        "/v1/playback-sessions",
        response_model=PlaybackStatus,
        responses={202: {"model": PlaybackStatus}},
        operation_id="create_playback_session",
    )
    def create(
        body: PlaybackDemand,
        request: Request,
        response: Response,
        idempotency_key: Annotated[str | None, Header(alias="Idempotency-Key")] = None,
    ):
        mutation()
        with session(request, write=True) as access:
            payload = body.model_dump()
            ttl = payload.pop("ttl")
            result = playback.admit(access, payload, idempotency_key, ttl)
        response.status_code = 200 if result["state"] in ("ready", "playing") else 202
        response.headers["Location"] = result["status_url"]
        response.headers["Cache-Control"] = "no-store"
        return result

    @app.get(
        "/v1/playback-sessions/{identifier}",
        response_model=PlaybackStatus,
        operation_id="get_playback_session",
    )
    def status(identifier: str, request: Request, response: Response):
        with session(request) as access:
            result = playback.status(access, identifier)
        response.headers["Cache-Control"] = "no-store"
        return result

    @app.post(
        "/v1/playback-sessions/{identifier}/cancel",
        response_model=PlaybackStatus,
        operation_id="cancel_playback_session",
    )
    def cancel(identifier: str, request: Request):
        mutation()
        with session(request, write=True) as access:
            return playback.cancel(access, identifier)

    @app.get(
        "/v1/playback-sessions/{identifier}/index.m3u8",
        response_model=str,
        response_class=Response,
        operation_id="get_playback_playlist",
    )
    @app.head(
        "/v1/playback-sessions/{identifier}/index.m3u8",
        response_model=str,
        response_class=Response,
        operation_id="head_playback_playlist",
    )
    def playlist(identifier: str, request: Request):
        with session(request) as access:
            with playback.media(access, identifier) as data:
                headers = {
                    "Cache-Control": "no-store",
                    "Content-Length": str(len(data)),
                    "X-Content-Type-Options": "nosniff",
                }
                return Response(
                    b"" if request.method == "HEAD" else data,
                    media_type="application/vnd.apple.mpegurl",
                    headers=headers,
                )

    @app.get(
        "/v1/playback-sessions/{identifier}/segments/{segment}/content",
        response_class=Response,
        operation_id="get_playback_segment",
    )
    @app.head(
        "/v1/playback-sessions/{identifier}/segments/{segment}/content",
        response_class=Response,
        operation_id="head_playback_segment",
    )
    def segment(identifier: str, segment: str, request: Request):
        scope = None
        admitted = False
        try:
            with session(request) as access:
                principal = access.principal
                scope = playback.media(access, identifier, segment)
                fd, st = scope.__enter__()
                headers = {
                    "Cache-Control": "private, no-store",
                    "Content-Length": str(st.st_size),
                    "X-Content-Type-Options": "nosniff",
                }
                if request.headers.get("range"):
                    # Whole immutable HLS segments are the delivery unit.
                    raise AccessError("playback_segment_range_unsupported", 416)
                if request.method == "HEAD":
                    scope.__exit__(None, None, None)
                    scope = None
                    return Response(media_type="video/mp2t", headers=headers)
                limits.admit(principal)
                admitted = True

            def cleanup():
                try:
                    scope.__exit__(None, None, None)
                finally:
                    limits.release(principal)

            return OwnedStream(
                chunks(fd, 0, st.st_size, st),
                cleanup=cleanup,
                media_type="video/mp2t",
                headers=headers,
            )
        except BaseException:
            if scope is not None:
                scope.__exit__(None, None, None)
            if admitted:
                limits.release(principal)
            raise
