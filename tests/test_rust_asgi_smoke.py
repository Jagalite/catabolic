# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Dependency-free installed-wheel ASGI smoke, including minimal Python 3.11."""

import asyncio
import json
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from catabolic_native.domain import CatabolicError
from catabolic_native.http.app import create_app
from catabolic_native.http.experimental import disposable_fixture


def http_scope():
    return {
        "type": "http",
        "method": "GET",
        "path": "/v1/me",
        "client": ("::1", 1),
        "headers": [(b"host", b"testserver")],
    }


class InstalledASGISmoke(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.fixture = disposable_fixture()
        self.root = Path(self.fixture.path).parent
        self.app = create_app(self.fixture)

    async def asyncTearDown(self):
        await self.app.aclose()
        self.fixture.close()
        self.assertFalse(self.root.exists())

    async def exchange(self, request, incoming):
        queue = asyncio.Queue()
        for event in incoming:
            queue.put_nowait(event)
        response = []

        async def send(event):
            response.append(event)

        await self.app(request, queue.get, send)
        return response

    async def test_real_http_application_without_framework(self):
        response = await self.exchange(http_scope(), [{"type": "http.request"}])
        self.assertEqual(response[0]["status"], 401)
        body = b"".join(event.get("body", b"") for event in response)
        self.assertEqual(json.loads(body)["code"], "unauthorized")
        self.assertFalse(response[-1]["more_body"])

    async def test_ordinary_paths_and_remote_peers_are_refused(self):
        with self.assertRaisesRegex(CatabolicError, "ordinary catalogs"):
            create_app(self.fixture.path)
        request = http_scope()
        request["client"] = ("192.0.2.1", 1)
        with self.assertRaisesRegex(CatabolicError, "loopback"):
            await self.exchange(request, [{"type": "http.request"}])
        self.assertEqual(self.app._owner.active, 0)

    async def test_streamed_request_body_is_bounded(self):
        response = await self.exchange(
            http_scope(),
            [
                {"type": "http.request", "body": b"x" * 65536, "more_body": True}
                for _ in range(17)
            ]
            + [{"type": "http.request"}],
        )
        self.assertEqual(response[0]["status"], 413)

    async def test_cancelled_partial_body_retires_native_request(self):
        queue = asyncio.Queue()
        queue.put_nowait({"type": "http.request", "body": b"x", "more_body": True})

        async def send(event):
            self.fail("partial request must not complete")

        task = asyncio.create_task(self.app(http_scope(), queue.get, send))
        await asyncio.sleep(0.03)
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assertEqual(self.app._owner.active, 0)

    async def test_delayed_body_concurrency_with_one_executor_thread(self):
        asyncio.get_running_loop().set_default_executor(
            ThreadPoolExecutor(max_workers=1)
        )
        queues = [asyncio.Queue() for _ in range(4)]
        responses = [[] for _ in queues]
        ready = [asyncio.Event() for _ in queues]
        tasks = []
        for queue, response, event in zip(queues, responses, ready, strict=True):

            async def receive(queue=queue, event=event):
                event.set()
                return await queue.get()

            async def send(message, response=response):
                response.append(message)

            tasks.append(asyncio.create_task(self.app(http_scope(), receive, send)))
        try:
            await asyncio.wait_for(
                asyncio.gather(*(event.wait() for event in ready)), 3
            )
            await asyncio.sleep(0.05)
            for queue in queues:
                queue.put_nowait({"type": "http.request"})
            await asyncio.wait_for(asyncio.gather(*tasks), 5)
            self.assertTrue(all(response[0]["status"] == 401 for response in responses))
            self.assertEqual(self.app._owner.active, 0)
        finally:
            await asyncio.wait_for(self.app.aclose(), 6)
            await asyncio.gather(*tasks, return_exceptions=True)

    async def test_lifespan_is_terminal_and_cleanup_is_owned(self):
        response = await self.exchange(
            {"type": "lifespan"},
            [{"type": "lifespan.startup"}, {"type": "lifespan.shutdown"}],
        )
        self.assertEqual(
            response,
            [
                {"type": "lifespan.startup.complete"},
                {"type": "lifespan.shutdown.complete"},
            ],
        )
        with self.assertRaisesRegex(CatabolicError, "closed"):
            await self.exchange(http_scope(), [{"type": "http.request"}])


if __name__ == "__main__":
    unittest.main()
