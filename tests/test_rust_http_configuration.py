# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Explicit native CLI configuration and frozen middleware contract checks."""

import unittest

from fastapi.testclient import TestClient

from catabolic.http.app import create_app
from tests import test_http_backend as reference
from tests import test_rust_http_transport as native


class NativeHTTPConfigurationTest(unittest.TestCase):
    stop_server = native.NativeHTTPTransportTest.stop_server

    def setUp(self):
        reference.HTTPTest.setUp(self)
        self.client.close()
        native.NativeHTTPTransportTest.start_native_server(
            self, read_only=True, arguments=["--cors-origin", "https://example.test"]
        )
        self.reference = TestClient(
            create_app(self.database, origins=["https://example.test"])
        )
        self.addCleanup(self.reference.close)

    def test_cors_preflight_order_origins_methods_and_headers(self):
        for origin, method, headers, host in [
            (
                "https://example.test",
                "GET",
                "Authorization, Idempotency-Key",
                "127.0.0.1",
            ),
            ("https://example.test", "GET", "Content-Type", "rejected.invalid"),
            ("https://example.test", "PUT", "Authorization", "127.0.0.1"),
            ("https://rejected.invalid", "PUT", "X-Rejected", "127.0.0.1"),
        ]:
            values = {
                "Origin": origin,
                "Access-Control-Request-Method": method,
                "Access-Control-Request-Headers": headers,
                "Host": host,
            }
            expected = self.reference.options("/v1/me", headers=values)
            actual = self.client.options("/v1/me", headers=values)
            self.assertEqual(actual.status_code, expected.status_code, actual.text)
            self.assertEqual(actual.text, expected.text)
            for key in (
                "content-type",
                "vary",
                "access-control-allow-origin",
                "access-control-allow-methods",
                "access-control-allow-headers",
                "access-control-max-age",
            ):
                self.assertEqual(
                    actual.headers.get(key), expected.headers.get(key), key
                )

    def test_readonly_refuses_mutations_but_allows_metadata_tickets_and_snapshots(self):
        operator = {"Authorization": "Bearer " + self.operator}
        response = self.client.post(
            "/v1/webhooks",
            headers=operator,
            json={"url": "http://localhost/hook", "events": ["scan_failed"]},
        )
        self.assertEqual(response.status_code, 403, response.text)
        self.assertEqual(response.json()["code"], "read_only")
        self.assertEqual(
            self.client.get("/v1/me", headers=self.headers).status_code, 200
        )
        snapshot = self.client.post("/v1/items/snapshots", headers=self.headers)
        self.assertEqual(snapshot.status_code, 200, snapshot.text)
        ticket = self.client.post(
            "/v1/content-access",
            headers=self.headers,
            json={"file_id": self.file, "revision": self.revision},
        )
        self.assertEqual(ticket.status_code, 200, ticket.text)
        content = self.client.get(ticket.json()["content_path"])
        self.assertEqual(content.content, self.payload)
        invalid = self.client.put(
            "/v1/operator/watchers/invalid",
            headers=operator,
            json={"plan": {"kind": "query", "query_id": 1}},
        )
        self.assertEqual(invalid.status_code, 422, invalid.text)
