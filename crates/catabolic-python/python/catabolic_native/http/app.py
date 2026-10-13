# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""ASGI transport for explicitly disposable native qualification fixtures only.

Ordinary catalog paths remain refused until the native HTTP release gates close.
Python owns callback scheduling and ASGI representation, never HTTP routes/policy.
"""

import asyncio
from urllib.parse import quote

from .. import _native
from ..domain import CatabolicError


class _FixtureASGI:
    def __init__(self, owner):
        self._owner = owner
        self._calls = set()
        self._closed = False

    async def aclose(self):
        self._closed = True
        current = asyncio.current_task()
        local = []
        for task in tuple(self._calls):
            if task is current or task.done():
                continue
            if task.get_loop() is asyncio.get_running_loop():
                task.cancel()
                local.append(task)
            else:
                task.get_loop().call_soon_threadsafe(task.cancel)
        await asyncio.to_thread(self._owner.close)
        if local:
            await asyncio.gather(*local, return_exceptions=True)

    async def _lifespan(self, receive, send):
        while True:
            event = await receive()
            if event["type"] == "lifespan.startup":
                if self._closed or self._owner.closed:
                    await send(
                        {
                            "type": "lifespan.startup.failed",
                            "message": "ASGI application is closed",
                        }
                    )
                    return
                await send({"type": "lifespan.startup.complete"})
            elif event["type"] == "lifespan.shutdown":
                await self.aclose()
                await send({"type": "lifespan.shutdown.complete"})
                return
            else:
                raise ValueError("unexpected ASGI lifespan event")

    async def __call__(self, scope, receive, send):
        if scope["type"] == "lifespan":
            try:
                await self._lifespan(receive, send)
            except BaseException:
                await self.aclose()
                raise
            return
        if scope["type"] != "http":
            raise ValueError("experimental native ASGI supports HTTP and lifespan only")
        if self._closed:
            raise CatabolicError("ASGI application is closed")
        if scope.get("root_path", ""):
            raise CatabolicError(
                "mounted experimental ASGI applications are unqualified"
            )
        peer = scope.get("client")
        peer = peer[0] if isinstance(peer, (tuple, list)) and peer else ""
        raw_path = scope.get("raw_path")
        if raw_path is None:
            raw_path = quote(scope["path"], safe="/:@!$&'()*+,;=-._~").encode("ascii")
        query = scope.get("query_string", b"")
        if (
            not isinstance(raw_path, bytes)
            or not isinstance(query, bytes)
            or not raw_path.startswith(b"/")
            or b"?" in raw_path
            or b"#" in raw_path + query
        ):
            raise ValueError("invalid ASGI request target")
        uri = (raw_path + (b"?" + query if query else b"")).decode("ascii")
        exchange = self._owner.begin(
            peer, scope["method"], uri, scope.get("headers", [])
        )
        caller = asyncio.current_task()
        self._calls.add(caller)
        loop = asyncio.get_running_loop()
        ingress_ready, response_ready, finished = (
            asyncio.Event(),
            asyncio.Event(),
            asyncio.Event(),
        )

        def notify():
            try:
                for ready in (ingress_ready, response_ready, finished):
                    loop.call_soon_threadsafe(ready.set)
            except RuntimeError:
                # A closed host loop cannot accept wakes. Native cancellation
                # and object lifetime still retire this request's owners.
                pass

        exchange.set_notify(notify)

        async def feed(body, more):
            while True:
                # Clear before examining native state; a transition after the
                # examination schedules a new wake, so no readiness is lost.
                ingress_ready.clear()
                accepted = exchange.try_feed(body, more)
                if accepted is not None:
                    return accepted
                await ingress_ready.wait()

        async def read():
            complete = False
            accepted = True
            while True:
                event = await receive()
                if event["type"] == "http.disconnect":
                    return
                if event["type"] != "http.request" or complete:
                    raise ValueError("unexpected ASGI HTTP event")
                body = event.get("body", b"")
                if not isinstance(body, bytes):
                    raise TypeError("ASGI request body must be bytes")
                more = bool(event.get("more_body", False))
                for offset in range(0, len(body), 65536):
                    if not accepted:
                        break
                    accepted = await feed(body[offset : offset + 65536], True)
                if not more and accepted:
                    await feed(b"", False)
                complete = not more

        async def write():
            while True:
                response_ready.clear()
                event = exchange.poll()
                if event is None:
                    await response_ready.wait()
                    continue
                if event["type"] == "native.closed":
                    if not self._owner.closed:
                        raise CatabolicError(
                            "native ASGI request ended before response completion"
                        )
                    return
                await send(event)
                if event["type"] == "http.response.body" and not event["more_body"]:
                    return

        reader = asyncio.create_task(read())
        writer = asyncio.create_task(write())
        try:
            done, _ = await asyncio.wait(
                (reader, writer), return_when=asyncio.FIRST_COMPLETED
            )
            # Callback failures are re-raised as the same exception instance,
            # even when response completion/disconnection wins the same turn.
            for task in (reader, writer):
                if task in done and (task.cancelled() or task.exception() is not None):
                    task.result()
        finally:
            exchange.cancel()
            reader.cancel()
            writer.cancel()
            try:
                await asyncio.gather(reader, writer, return_exceptions=True)
                while True:
                    finished.clear()
                    if exchange.finished:
                        break
                    await finished.wait()
            finally:
                self._calls.discard(caller)


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
    """Return a real ASGI app only for a native-minted disposable fixture.

    Production path admission is deliberately unavailable. Use
    ``catabolic_native.http.experimental.disposable_fixture`` solely to run
    disposable qualification. This does not complete frozen create_app parity.
    """
    if not isinstance(database, _native.DisposableHTTPFixture):
        raise CatabolicError(
            "native create_app remains unqualified for ordinary catalogs; "
            "only an explicitly disposable HTTP fixture capability is admitted"
        )
    return _FixtureASGI(
        _native.NativeASGI(
            database, read_only, allowed_hosts, origins, streams, per_principal, rate
        )
    )
