# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import threading
import time
from uuid import uuid4

from starlette.responses import JSONResponse, StreamingResponse

from ..access import AccessError


class Limits:
    def __init__(self, streams=8, per_principal=2, rate=120):
        self.streams, self.per_principal, self.rate = streams, per_principal, rate
        self.lock = threading.Lock()
        self.active = {}
        self.requests = {}

    def admit(self, principal):
        with self.lock:
            if (
                sum(self.active.values()) >= self.streams
                or self.active.get(principal, 0) >= self.per_principal
            ):
                raise AccessError("stream_limit", 429)
            self.active[principal] = self.active.get(principal, 0) + 1

    def release(self, principal):
        with self.lock:
            self.active[principal] -= 1
            if self.active[principal] == 0:
                del self.active[principal]

    def request(self, principal):
        with self.lock:
            now = time.monotonic()
            self.requests = {k: v for k, v in self.requests.items() if now - v[0] < 60}
            start, count = self.requests.get(principal, (now, 0))
            if count >= self.rate:
                raise AccessError("rate_limit", 429)
            self.requests[principal] = (start, count + 1)


class OwnedStream(StreamingResponse):
    def __init__(self, *args, cleanup, **kwargs):
        super().__init__(*args, **kwargs)
        self.cleanup = cleanup

    async def __call__(self, scope, receive, send):
        try:
            await super().__call__(scope, receive, send)
        finally:
            self.cleanup()


class BodyLimit:
    def __init__(self, app, maximum=1048576):
        self.app, self.maximum = app, maximum

    async def __call__(self, scope, receive, send):
        if scope["type"] != "http":
            return await self.app(scope, receive, send)
        size, body = 0, bytearray()
        while True:
            message = await receive()
            if message["type"] == "http.disconnect":
                return
            size += len(message.get("body", b""))
            if size > self.maximum:
                identifier = str(uuid4())
                response = JSONResponse(
                    {
                        "type": "about:blank",
                        "title": "body_too_large",
                        "status": 413,
                        "retryable": False,
                        "code": "body_too_large",
                        "request_id": identifier,
                        "remediation": "Reduce the request body to at most 1 MiB.",
                    },
                    status_code=413,
                    media_type="application/problem+json",
                    headers={"X-Request-ID": identifier, "Cache-Control": "no-store"},
                )
                return await response(scope, receive, send)
            body.extend(message.get("body", b""))
            if not message.get("more_body", False):
                break
        buffered = iter(
            [{"type": "http.request", "body": bytes(body), "more_body": False}]
        )

        async def bounded():
            message = next(buffered, None)
            return message if message is not None else await receive()

        await self.app(scope, bounded, send)
