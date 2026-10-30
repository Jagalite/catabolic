# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Concurrent native HTTP consumers cannot overspend one ticket."""
import unittest
from concurrent.futures import ThreadPoolExecutor
from threading import Barrier

import httpx
from catabolic.store import Store
from tests import test_rust_http_transport as fixtures


class NativeHTTPTicketConcurrencyReviewTest(unittest.TestCase):
    setUp = fixtures.NativeHTTPTransportTest.setUp
    start_native_server = fixtures.NativeHTTPTransportTest.start_native_server
    stop_server = fixtures.NativeHTTPTransportTest.stop_server

    def test_two_simultaneous_ranges_share_one_atomic_allowance(self):
        response = self.client.post(
            "/v1/content-access", headers=self.headers,
            json={"file_id": self.file, "revision": self.revision, "max_bytes": 10},
        )
        self.assertEqual(response.status_code, 200, response.text)
        ticket = response.json()
        barrier = Barrier(2)

        def read():
            with httpx.Client(base_url=self.base, timeout=15, trust_env=False) as client:
                barrier.wait(timeout=10)
                return client.get(ticket["content_path"], headers={"Range": "bytes=0-9"})

        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(lambda _: read(), range(2)))
        self.assertEqual(sum(result.status_code == 206 for result in results), 1)
        failed = next(result for result in results if result.status_code != 206)
        if failed.status_code == 503:
            self.assertEqual(failed.json()["code"], "catalog_busy")
            failed = self.client.get(ticket["content_path"], headers={"Range": "bytes=0-9"})
        self.assertEqual(failed.status_code, 429, failed.text)
        self.assertEqual(next(result.content for result in results if result.status_code == 206), self.payload[:10])
        with Store(self.database) as store:
            self.assertEqual(
                store.rows("SELECT remaining_bytes FROM api_tickets WHERE id=?", (ticket["ticket_id"],)),
                [{"remaining_bytes": 0}],
            )
