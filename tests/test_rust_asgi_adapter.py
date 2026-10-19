# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Narrow, disposable ASGI transport evidence, not native HTTP qualification.

Run with the frozen Python source on PYTHONPATH and an installed native wheel.
No listener, production catalog, credential, source, or output is used.
"""

import asyncio
import inspect
import os
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from unittest.mock import patch

import httpx
from catabolic_native import _native
from catabolic_native.domain import CatabolicError
from catabolic_native.http.app import create_app
from catabolic_native.http.experimental import disposable_fixture

from catabolic.access import grant_put, issue, principal_put
from catabolic.app import Application
from catabolic.content_access import revision_of
from catabolic.http.app import create_app as reference_app
from catabolic.store import Store


def scope(path="/v1/me", *, headers=(), peer=("127.0.0.1", 12000), method="GET"):
    path, _, query = path.partition("?")
    return {
        "type": "http",
        "asgi": {"version": "3.0", "spec_version": "2.4"},
        "http_version": "1.1",
        "method": method,
        "scheme": "http",
        "path": path,
        "raw_path": path.encode(),
        "query_string": query.encode(),
        "root_path": "",
        "client": peer,
        "server": ("testserver", 80),
        "headers": [(b"host", b"testserver"), *headers],
    }


class CallbackFailure(BaseException):
    pass


class ASGIAdapterTest(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        reference = Path(os.environ["CATABOLIC_PYTHON_REFERENCE"]).resolve()
        self.assertTrue(
            Path(inspect.getfile(reference_app)).resolve().is_relative_to(reference)
        )
        self.fixture = disposable_fixture()
        self.database = Path(self.fixture.path)
        self.root = self.database.parent
        self.payload = bytes(range(256)) * 2048
        media = self.root / "media"
        media.mkdir()
        (media / "fixture.bin").write_bytes(self.payload)
        with Store(self.database, writable=True) as store:
            application = Application(store)
            application.bind("source", "fixture", str(media))
            application.scan()
            self.file = store.rows("SELECT id FROM files")[0]["id"]
            principal_put(store, "default", "asgi-fixture")
            grant = grant_put(
                store, "asgi-fixture", {"actions": ["*"], "operator": True}
            )
            self.token = issue(store, "asgi-fixture", [grant["grant_id"]])["token"]
            with store.transaction() as db:
                db.execute("INSERT INTO api_sources VALUES ('default','fixture')")
            self.revision = revision_of(store, "default", self.file)
        self.headers = {"Authorization": "Bearer " + self.token}
        self.raw_headers = [(b"authorization", self.headers["Authorization"].encode())]
        self.content = f"/v1/files/{self.file}/content?revision={self.revision}"
        self.apps = []

    async def asyncTearDown(self):
        for app in self.apps:
            await app.aclose()
        # Stream owners may be completing a previously accepted blocking read;
        # close() has joined the native runtime before releasing this catalog.
        self.fixture.close()
        self.assertFalse(self.root.exists())

    def app(self, **options):
        app = create_app(self.fixture, **options)
        self.apps.append(app)
        return app

    async def request(self, app, method="GET", path="/v1/me", **kwargs):
        async with httpx.AsyncClient(
            transport=httpx.ASGITransport(app=app), base_url="http://testserver"
        ) as client:
            return await client.request(method, path, **kwargs)

    async def idle(self, app):
        async with asyncio.timeout(3):
            while app._owner.active:
                await asyncio.sleep(0.01)

    async def test_exact_signature_and_ordinary_constructor_refusal(self):
        self.assertEqual(
            inspect.signature(create_app), inspect.signature(reference_app)
        )
        for path in (self.database, str(self.database), self.root / "does-not-exist"):
            with self.assertRaisesRegex(CatabolicError, "ordinary catalogs"):
                create_app(path)
        self.assertFalse((self.root / "does-not-exist").exists())
        with self.assertRaises(TypeError):
            _native.DisposableHTTPFixture(str(self.database))
        with self.assertRaises(AttributeError):
            self.fixture.path = str(self.root / "different.db")

    async def test_native_and_python_refuse_remote_missing_and_proxy_peers_before_effects(
        self,
    ):
        app = self.app(rate=1)
        called = []

        async def callback(*args):
            called.append(args)
            raise AssertionError("refusal must precede all callbacks")

        for peer in (
            None,
            ("203.0.113.4", 1),
            ("0.0.0.0", 1),
            ("localhost", 1),
            ("::ffff:192.0.2.1", 1),
        ):
            with (
                self.subTest(peer=peer),
                self.assertRaisesRegex(CatabolicError, "loopback"),
            ):
                await app(scope(peer=peer), callback, callback)
        for name in (
            b"forwarded",
            b"x-forwarded-for",
            b"x-forwarded-proto",
            b"x-real-ip",
            b"via",
        ):
            with (
                self.subTest(name=name),
                self.assertRaisesRegex(CatabolicError, "proxy"),
            ):
                await app(scope(headers=[(name, b"fixture")]), callback, callback)
        with self.assertRaisesRegex(CatabolicError, "loopback"):
            app._owner.begin("192.0.2.1", "POST", "/v1/content-access", [])
        self.assertEqual(called, [])
        self.assertEqual(app._owner.active, 0)
        with Store(self.database) as store:
            self.assertEqual(
                store.rows("SELECT count(*) AS n FROM api_tickets")[0]["n"], 0
            )
        self.assertEqual(
            (await self.request(app, headers=self.headers)).status_code, 200
        )

    async def test_fresh_capability_and_live_owner_cleanup(self):
        app = self.app()
        with self.assertRaisesRegex(CatabolicError, "close all ASGI"):
            self.fixture.close()
        second = disposable_fixture()
        second_root = Path(second.path).parent
        self.assertNotEqual(second.path, self.fixture.path)
        second.close()
        self.assertFalse(second_root.exists())
        with self.assertRaisesRegex(CatabolicError, "closed"):
            create_app(second)
        await app.aclose()
        self.fixture.close()
        self.assertFalse(self.root.exists())

    async def test_fixture_catalog_identity_cannot_be_replaced(self):
        app = self.app()
        with Store(self.database, writable=True) as store:
            with store.transaction() as db:
                db.execute(
                    "UPDATE meta SET value='00000000-0000-4000-8000-000000000001' WHERE key='database_id'"
                )
        with self.assertRaisesRegex(CatabolicError, "identity changed"):
            create_app(self.fixture)
        with self.assertRaisesRegex(CatabolicError, "identity changed"):
            await self.request(app, headers=self.headers)
        await self.idle(app)

    async def test_lifespan_callback_failure_preserves_exception_and_closes(self):
        app = self.app()
        failure = CallbackFailure("startup callback fixture failed")

        async def receive():
            return {"type": "lifespan.startup"}

        async def send(event):
            raise failure

        with self.assertRaises(CallbackFailure) as raised:
            await app({"type": "lifespan"}, receive, send)
        self.assertIs(raised.exception, failure)
        self.assertTrue(app._owner.closed)

    async def test_paired_identity_methods_and_content_frames(self):
        native = self.app()
        frozen = reference_app(self.database)
        cases = [
            ("GET", "/v1/me", {}),
            ("GET", "/v1/me", {"headers": self.headers}),
            ("GET", self.content, {"headers": self.headers}),
            ("HEAD", self.content, {"headers": self.headers}),
            (
                "GET",
                self.content,
                {"headers": {**self.headers, "Range": "bytes=7-101"}},
            ),
            (
                "GET",
                self.content,
                {"headers": {**self.headers, "If-None-Match": f'W/"{self.revision}"'}},
            ),
            ("GET", "/missing", {}),
            ("POST", "/v1/me", {}),
            (
                "POST",
                "/v1/query/sql",
                {"headers": self.headers, "json": {"query": "SELECT 1 AS fixture"}},
            ),
            (
                "POST",
                "/v1/query/sql",
                {
                    "headers": {**self.headers, "Content-Type": "application/json"},
                    "content": b"{",
                },
            ),
        ]
        for method, path, options in cases:
            with self.subTest(method=method, path=path):
                left = await self.request(frozen, method, path, **options)
                right = await self.request(native, method, path, **options)
                self.assertEqual(right.status_code, left.status_code)
                if "application/problem+json" in left.headers.get("content-type", ""):
                    a, b = left.json(), right.json()
                    a.pop("request_id")
                    b.pop("request_id")
                    self.assertEqual(b, a)
                elif "application/json" in left.headers.get("content-type", ""):
                    self.assertEqual(right.json(), left.json())
                else:
                    self.assertEqual(right.content, left.content)
                for header in (
                    "content-type",
                    "content-length",
                    "content-range",
                    "etag",
                    "cache-control",
                ):
                    self.assertEqual(
                        right.headers.get(header), left.headers.get(header), header
                    )

    async def test_paired_hosts_cors_and_body_bounds(self):
        settings = {
            "allowed_hosts": ["testserver"],
            "origins": ["http://fixture.invalid"],
        }
        native = self.app(**settings)
        frozen = reference_app(self.database, **settings)
        for app in (native, frozen):
            response = await self.request(app, headers={"Host": "other.invalid"})
            self.assertEqual(response.status_code, 400)
            response = await self.request(
                app,
                "OPTIONS",
                headers={
                    "Origin": "http://fixture.invalid",
                    "Access-Control-Request-Method": "POST",
                },
            )
            self.assertEqual(response.status_code, 200)
            self.assertEqual(
                response.headers["access-control-allow-origin"],
                "http://fixture.invalid",
            )
            response = await self.request(
                app,
                "POST",
                "/v1/query/sql",
                headers=self.headers,
                content=b"x" * (1048576 + 1),
            )
            self.assertEqual(response.status_code, 413)
        for options in (
            {"streams": 0},
            {"per_principal": 0},
            {"rate": 0},
            {"allowed_hosts": ["*"]},
            {"origins": ["*"]},
        ):
            with self.subTest(options=options), self.assertRaises(CatabolicError):
                create_app(self.fixture, **options)

    async def test_persistent_rate_state_and_application_isolation(self):
        first, second = self.app(rate=2), self.app(rate=2)
        results = await asyncio.gather(
            *(self.request(first, headers=self.headers) for _ in range(3))
        )
        self.assertEqual(sorted(r.status_code for r in results), [200, 200, 429])
        self.assertEqual(
            (await self.request(first, headers=self.headers)).status_code, 429
        )
        self.assertEqual(
            (await self.request(second, headers=self.headers)).status_code, 200
        )

    async def test_chunked_request_and_response_streaming(self):
        app = self.app()
        queue = asyncio.Queue()
        for body, more in [(b'{"query":', True), (b'"SELECT 1 AS fixture"}', False)]:
            queue.put_nowait({"type": "http.request", "body": body, "more_body": more})
        events = []

        async def send(event):
            events.append(event)

        await app(
            scope(
                "/v1/query/sql",
                method="POST",
                headers=[*self.raw_headers, (b"content-type", b"application/json")],
            ),
            queue.get,
            send,
        )
        self.assertEqual(events[0]["status"], 200)
        self.assertIn(b"fixture", b"".join(e.get("body", b"") for e in events))
        queue = asyncio.Queue()
        queue.put_nowait({"type": "http.request"})
        events.clear()
        await app(scope(self.content, headers=self.raw_headers), queue.get, send)
        chunks = [e["body"] for e in events if e["type"] == "http.response.body"]
        self.assertGreater(len(chunks), 2)
        self.assertTrue(all(len(chunk) <= 65536 for chunk in chunks))
        self.assertEqual(b"".join(chunks), self.payload)
        self.assertFalse(events[-1]["more_body"])

    async def stalled(self, app, path=None, *, send_failure=None):
        queue = asyncio.Queue()
        queue.put_nowait({"type": "http.request"})
        started = asyncio.Event()
        block = asyncio.Event()
        events = []

        async def send(event):
            events.append(event)
            if event["type"] == "http.response.body" and event["body"]:
                started.set()
                if send_failure is not None:
                    raise send_failure
                await block.wait()

        task = asyncio.create_task(
            app(
                scope(path or "/v1/events?follow=true", headers=self.raw_headers),
                queue.get,
                send,
            )
        )
        await asyncio.wait_for(started.wait(), 3)
        return task, queue, events

    async def test_disconnect_and_cancel_release_streams_for_reuse(self):
        app = self.app(streams=1, per_principal=1)
        for action in ("disconnect", "cancel"):
            task, queue, events = await self.stalled(app)
            self.assertEqual(events[0]["status"], 200)
            response = await self.request(app, path=self.content, headers=self.headers)
            self.assertEqual(response.status_code, 429)
            if action == "disconnect":
                queue.put_nowait({"type": "http.disconnect"})
                await asyncio.wait_for(task, 3)
            else:
                task.cancel()
                with self.assertRaises(asyncio.CancelledError):
                    await task
            await self.idle(app)
            self.assertEqual(
                (
                    await self.request(app, path=self.content, headers=self.headers)
                ).status_code,
                200,
            )
            with Store(self.database, writable=True):
                pass

    async def test_callback_exception_identity_and_cleanup(self):
        app = self.app(streams=1)
        send_error = CallbackFailure("send fixture failed")
        task, _, _ = await self.stalled(app, self.content, send_failure=send_error)
        with self.assertRaises(CallbackFailure) as raised:
            await task
        self.assertIs(raised.exception, send_error)
        receive_error = CallbackFailure("receive fixture failed")

        async def receive():
            raise receive_error

        async def send(event):
            self.fail("no response before input")

        with self.assertRaises(CallbackFailure) as raised:
            await app(scope(headers=self.raw_headers), receive, send)
        self.assertIs(raised.exception, receive_error)
        await self.idle(app)
        self.assertEqual(
            (
                await self.request(app, path=self.content, headers=self.headers)
            ).status_code,
            200,
        )

    async def test_delayed_bodies_progress_with_one_executor_worker(self):
        loop = asyncio.get_running_loop()
        loop.set_default_executor(ThreadPoolExecutor(max_workers=1))
        app = self.app()
        entered = [asyncio.Event() for _ in range(16)]
        queues = [asyncio.Queue() for _ in entered]
        outputs = [[] for _ in entered]
        tasks = []
        for seen, queue, output in zip(entered, queues, outputs, strict=True):

            async def receive(seen=seen, queue=queue):
                seen.set()
                return await queue.get()

            async def send(event, output=output):
                output.append(event)

            tasks.append(asyncio.create_task(app(scope(), receive, send)))
        try:
            await asyncio.wait_for(
                asyncio.gather(*(event.wait() for event in entered)), 3
            )
            # Let every egress task reach its empty response channel before
            # supplying EOF. The rejected adapter exhausts the sole executor
            # thread here and cannot deliver any of these bodies.
            await asyncio.sleep(0.05)
            for queue in queues:
                queue.put_nowait({"type": "http.request"})
            await asyncio.wait_for(asyncio.gather(*tasks), 5)
            self.assertTrue(all(output[0]["status"] == 401 for output in outputs))
            self.assertTrue(all(output[-1]["more_body"] is False for output in outputs))
            self.assertEqual(app._owner.active, 0)
        finally:
            await asyncio.wait_for(app.aclose(), 6)
            await asyncio.gather(*tasks, return_exceptions=True)

    async def test_delayed_body_cancellation_and_close_do_not_need_executor_slots(self):
        loop = asyncio.get_running_loop()
        loop.set_default_executor(ThreadPoolExecutor(max_workers=1))
        app = self.app()
        queues = [asyncio.Queue() for _ in range(8)]
        entered = [asyncio.Event() for _ in queues]
        tasks = []
        for seen, queue in zip(entered, queues, strict=True):

            async def receive(seen=seen, queue=queue):
                seen.set()
                return await queue.get()

            async def send(event):
                self.fail("no response before request EOF")

            tasks.append(asyncio.create_task(app(scope(), receive, send)))
        try:
            await asyncio.wait_for(
                asyncio.gather(*(event.wait() for event in entered)), 3
            )
            await asyncio.sleep(0.05)
            tasks[-1].cancel()
            with self.assertRaises(asyncio.CancelledError):
                await asyncio.wait_for(tasks[-1], 2)
            self.assertEqual(app._owner.active, 7)
            await asyncio.wait_for(app.aclose(), 6)
            self.assertTrue(all(task.cancelled() for task in tasks))
            self.assertEqual(app._owner.active, 0)
        finally:
            await asyncio.wait_for(app.aclose(), 6)
            await asyncio.gather(*tasks, return_exceptions=True)

    async def test_shutdown_stops_new_calls_before_background_close_runs(self):
        app = self.app()
        started, release = asyncio.Event(), asyncio.Event()
        to_thread = asyncio.to_thread

        async def delayed(function, *args, **kwargs):
            if function == app._owner.close:
                started.set()
                await release.wait()
            return await to_thread(function, *args, **kwargs)

        with patch("asyncio.to_thread", delayed):
            shutdown = asyncio.create_task(app.aclose())
            await started.wait()
            try:
                with self.assertRaisesRegex(CatabolicError, "closed"):
                    await self.request(app, headers=self.headers)
                self.assertEqual(app._owner.active, 0)
            finally:
                release.set()
                await shutdown

    async def test_request_body_cancellation_and_lifespan_shutdown(self):
        app = self.app()
        queue = asyncio.Queue()
        queue.put_nowait({"type": "http.request", "body": b"{", "more_body": True})
        events = []

        async def send(event):
            events.append(event)

        task = asyncio.create_task(
            app(
                scope("/v1/query/sql", headers=self.raw_headers, method="POST"),
                queue.get,
                send,
            )
        )
        await asyncio.sleep(0.05)
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task
        await self.idle(app)
        self.assertEqual(events, [])
        stream, _, _ = await self.stalled(app)
        lifecycle = asyncio.Queue()
        lifecycle.put_nowait({"type": "lifespan.startup"})
        lifecycle.put_nowait({"type": "lifespan.shutdown"})
        await app({"type": "lifespan"}, lifecycle.get, send)
        self.assertEqual(
            [e["type"] for e in events],
            ["lifespan.startup.complete", "lifespan.shutdown.complete"],
        )
        self.assertTrue(stream.cancelled())
        self.assertTrue(app._owner.closed)
        self.assertEqual(app._owner.active, 0)
        with self.assertRaisesRegex(CatabolicError, "closed"):
            await self.request(app, headers=self.headers)
        lifecycle.put_nowait({"type": "lifespan.startup"})
        await app({"type": "lifespan"}, lifecycle.get, send)
        self.assertEqual(events[-1]["type"], "lifespan.startup.failed")


if __name__ == "__main__":
    unittest.main()
