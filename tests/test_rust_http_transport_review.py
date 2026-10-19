# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Independent wire-level HTTP regression checks against the Python contract.

The native fixture launches a disposable loopback listener. To use the frozen
Python baseline, place .local-tests/rust-migration/reference/src before this
checkout's src directory on PYTHONPATH. No delivery workers are started.
"""

import unittest
from urllib.parse import urlsplit

from fastapi.testclient import TestClient

from catabolic.access import grant_put
from catabolic.http.app import create_app
from catabolic.saved_queries import Queries
from catabolic.store import Store
from tests import test_rust_http_transport as native_fixture


class NativeHTTPTransportReviewTest(unittest.TestCase):
    setUp = native_fixture.NativeHTTPTransportTest.setUp
    start_native_server = native_fixture.NativeHTTPTransportTest.start_native_server
    stop_server = native_fixture.NativeHTTPTransportTest.stop_server

    def reference_client(self):
        client = TestClient(create_app(self.database, read_only=False))
        self.addCleanup(client.close)
        return client

    def assert_problem(self, response, status, code):
        self.assertEqual(response.status_code, status, response.text)
        self.assertEqual(response.json()["code"], code, response.text)
        self.assertEqual(response.headers["content-type"], "application/problem+json")
        self.assertEqual(response.headers["cache-control"], "no-store")
        self.assertEqual(
            response.headers["x-request-id"], response.json()["request_id"]
        )

    def test_query_parameters_are_route_scoped_and_use_frozen_coercion(self):
        reference = self.reference_client()
        for path in (
            "/v1/me?limit=bad",
            "/v1/items?limit=1.0",
            "/v1/items?limit=%20%2B1%20",
        ):
            with self.subTest(path=path):
                expected = reference.get(path, headers=self.headers)
                actual = self.client.get(path, headers=self.headers)
                self.assertEqual(expected.status_code, 200, expected.text)
                self.assertEqual(actual.status_code, 200, actual.text)
                self.assertEqual(actual.json(), expected.json())

    def test_dynamic_method_errors_and_slash_redirects_match_reference(self):
        reference = self.reference_client()
        target = "/v1/items/" + self.item
        for method in ("POST", "OPTIONS", "HEAD"):
            with self.subTest(method=method):
                expected = reference.request(method, target, headers=self.headers)
                actual = self.client.request(method, target, headers=self.headers)
                self.assertEqual(expected.status_code, 405)
                self.assertEqual(actual.status_code, expected.status_code)
                if method != "HEAD":
                    self.assert_problem(actual, 405, "http_error")
        for path in (
            "/v1/items/",
            target + "%2F",
            "/v1/items/caf%C3%A9/",
            "/v1/items/control%0A/",
        ):
            with self.subTest(path=path):
                expected = reference.get(
                    path, headers=self.headers, follow_redirects=False
                )
                actual = self.client.get(
                    path, headers=self.headers, follow_redirects=False
                )
                self.assertEqual(expected.status_code, 307)
                self.assertEqual(actual.status_code, expected.status_code, actual.text)
                self.assertEqual(
                    urlsplit(actual.headers["location"]).path,
                    urlsplit(expected.headers["location"]).path,
                )

    def test_json_request_media_type_matches_reference(self):
        reference = self.reference_client()
        body = b'{"query":"{items{nodes{id}}}"}'
        for content_type, status in (
            (None, 422),
            ("application/json", 200),
            ("application/problem+json", 200),
            ("image/a+json", 422),
            ("text/json", 422),
        ):
            with self.subTest(content_type=content_type):
                headers = dict(self.headers)
                if content_type:
                    headers["Content-Type"] = content_type
                expected = reference.post(
                    "/v1/query/graphql", headers=headers, content=body
                )
                actual = self.client.post(
                    "/v1/query/graphql", headers=headers, content=body
                )
                self.assertEqual(expected.status_code, status, expected.text)
                self.assertEqual(actual.status_code, status, actual.text)
                if status == 422:
                    self.assert_problem(actual, 422, "invalid_request")
                else:
                    self.assertEqual(actual.json(), expected.json())

    def test_body_limit_precedes_unknown_route_dispatch(self):
        reference = self.reference_client()
        body = b"x" * 1048577
        expected = reference.request("GET", "/unknown", content=body)
        actual = self.client.request("GET", "/unknown", content=body)
        self.assertEqual(expected.status_code, 413)
        self.assert_problem(actual, 413, "body_too_large")
        self.assertEqual(actual.json()["type"], expected.json()["type"])

    def test_no_cors_configuration_does_not_intercept_options(self):
        reference = self.reference_client()
        headers = {
            "Origin": "http://example.invalid",
            "Access-Control-Request-Method": "GET",
        }
        expected = reference.options("/v1/me", headers=headers)
        actual = self.client.options("/v1/me", headers=headers)
        self.assertEqual(expected.status_code, 405)
        self.assert_problem(actual, 405, "http_error")
        self.assertNotIn("access-control-allow-methods", actual.headers)

    def test_service_error_status_and_validation_order_match_reference(self):
        reference = self.reference_client()
        operator = {"Authorization": "Bearer " + self.operator}
        demand = {
            "item_id": self.item,
            "source_file_id": self.file,
            "source_revision": self.revision,
            "operation_id": "unknown",
        }
        cases = [
            (
                "/v1/webhooks",
                {"url": "https://example.invalid/hook", "events": ["not-an-event"]},
                operator,
                400,
                "invalid_webhook_events",
            ),
            (
                "/v1/webhooks",
                {"url": "bogus", "events": ["job_completed"]},
                self.headers,
                403,
                "forbidden",
            ),
            ("/v1/playback-sessions", demand, operator, 400, "invalid_idempotency_key"),
        ]
        cases.extend(
            (
                "/v1/http-mappings/apply",
                {"definition": {}},
                headers,
                400,
                "expected_plan_required",
            )
            for headers in (operator, self.headers, {})
        )
        for path, body, headers, status, code in cases:
            with self.subTest(path=path, status=status, authorized=bool(headers)):
                expected = reference.post(path, headers=headers, json=body)
                actual = self.client.post(path, headers=headers, json=body)
                self.assert_problem(expected, status, code)
                self.assert_problem(actual, status, code)

    def test_not_modified_omits_content_type(self):
        reference = self.reference_client()
        headers = {**self.headers, "If-None-Match": f'W/"{self.revision}"'}
        expected = reference.get(self.content, headers=headers)
        actual = self.client.get(self.content, headers=headers)
        self.assertEqual(expected.status_code, 304)
        self.assertEqual(actual.status_code, 304)
        self.assertNotIn("content-type", expected.headers)
        self.assertNotIn("content-type", actual.headers)
        for header in (
            "etag",
            "accept-ranges",
            "cache-control",
            "x-content-type-options",
            "referrer-policy",
        ):
            self.assertEqual(actual.headers[header], expected.headers[header])
        self.assertEqual(actual.content, b"")

    def test_empty_event_cursor_falls_back_to_last_event_id(self):
        reference = self.reference_client()
        headers = {**self.headers, "Last-Event-ID": "invalid"}
        for path in (
            "/v1/events",
            "/v1/events?cursor=",
            "/v1/events?cursor=&follow=true",
        ):
            with self.subTest(path=path):
                expected = reference.get(path, headers=headers)
                self.assert_problem(expected, 409, "resync_required")
                # Streaming prevents an incorrect successful SSE response from
                # delaying the assertion until its entire lifetime expires.
                with self.client.stream("GET", path, headers=headers) as actual:
                    self.assertEqual(actual.status_code, 409)
                    actual.read()
                    self.assert_problem(actual, 409, "resync_required")

    def test_metadata_cursors_and_snapshots_interoperate_bidirectionally(self):
        reference = self.reference_client()
        with Store(self.database, writable=True) as store:
            grant_put(
                store,
                "site",
                {**self.definition, "item_ids": [self.item, self.hidden]},
                self.grant,
            )
        for name, producer, consumer in (
            ("python-native", reference, self.client),
            ("native-python", self.client, reference),
        ):
            with self.subTest(direction=name):
                first = producer.get("/v1/items?limit=1", headers=self.headers)
                self.assertEqual(first.status_code, 200, first.text)
                cursor = first.json()["next_cursor"]
                self.assertIsInstance(cursor, str)
                second = consumer.get(
                    "/v1/items",
                    params={"limit": 1, "cursor": cursor},
                    headers=self.headers,
                )
                self.assertEqual(second.status_code, 200, second.text)
                self.assertEqual(
                    {first.json()["data"][0]["id"], second.json()["data"][0]["id"]},
                    {self.item, self.hidden},
                )
                other = consumer.get(
                    "/v1/items",
                    params={"cursor": cursor},
                    headers={"Authorization": "Bearer " + self.other},
                )
                self.assert_problem(other, 400, "invalid_cursor")
                snapshot = producer.post("/v1/items/snapshots", headers=self.headers)
                self.assertEqual(snapshot.status_code, 200, snapshot.text)
                path = "/v1/snapshots/" + snapshot.json()["snapshot_id"]
                page = consumer.get(path, headers=self.headers)
                self.assertEqual(page.status_code, 200, page.text)
                self.assertEqual(
                    {row["id"] for row in page.json()["data"]}, {self.item, self.hidden}
                )
                self.assert_problem(
                    consumer.get(
                        path, headers={"Authorization": "Bearer " + self.other}
                    ),
                    404,
                    "not_found",
                )

    def test_operator_sql_mapping_keeps_request_owner_without_metadata_shadowing(self):
        reference = self.reference_client()
        headers = {"Authorization": "Bearer " + self.operator}
        with Store(self.database, writable=True) as store:
            query = Queries(store, "default").put(
                "mapped-items",
                {
                    "mode": "rows",
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id FROM items ORDER BY id",
                    },
                },
            )["id"]
        spec = {
            "openapi": "3.1.0",
            "info": {"title": "Test", "version": "1"},
            "paths": {
                "/updates": {
                    "post": {
                        "operationId": "update",
                        "responses": {"204": {"description": "ok"}},
                    }
                }
            },
        }
        operation = reference.post(
            "/v1/http-operations",
            headers=headers,
            json={
                "name": "test",
                "spec": spec,
                "operation_id": "update",
                "base_url": "http://127.0.0.1:9999",
            },
        )
        self.assertEqual(operation.status_code, 201, operation.text)
        definition = {
            "version": 3,
            "id": "map-items",
            "operation": operation.json()["id"],
            "query": query,
            "key": {"column": "id"},
        }
        expected = reference.post(
            "/v1/http-mappings/preview",
            headers=headers,
            json={"definition": definition},
        )
        actual = self.client.post(
            "/v1/http-mappings/preview",
            headers=headers,
            json={"definition": definition},
        )
        self.assertEqual(expected.status_code, 200, expected.text)
        self.assertEqual(actual.status_code, 200, actual.text)
        self.assertEqual(actual.json(), expected.json())
        self.assertEqual(
            {row["key"] for row in actual.json()["rows"]}, {self.item, self.hidden}
        )
        self.assertFalse(actual.json()["applied"])
        with Store(self.database) as store:
            self.assertEqual(
                store.rows("SELECT count(*) AS n FROM http_mapping_deliveries")[0]["n"],
                0,
            )

    def test_event_follow_rechecks_revoked_credentials_and_releases_writer(self):
        import time

        with self.client.stream(
            "GET", "/v1/events?follow=true", headers=self.headers
        ) as response:
            self.assertEqual(response.status_code, 200)
            lines = response.iter_lines()
            initial = []
            for line in lines:
                initial.append(line)
                if not line:
                    break
            self.assertIn("event: catalog", initial)
            # A listening SSE connection must not retain the writer owner.
            with Store(self.database, writable=True) as store:
                with store.transaction() as db:
                    db.execute(
                        "UPDATE api_tokens SET revoked=1 WHERE id=?",
                        (self.credential["token_id"],),
                    )
            deadline = time.monotonic() + 4
            terminated = []
            for line in lines:
                self.assertLess(time.monotonic(), deadline, terminated)
                terminated.append(line)
            self.assertIn("event: resync-required", terminated)
            self.assertTrue(
                any(
                    '"code": "unauthorized"' in line or '"code":"unauthorized"' in line
                    for line in terminated
                ),
                terminated,
            )
        self.assert_problem(
            self.client.get("/v1/me", headers=self.headers), 401, "unauthorized"
        )
        with Store(self.database, writable=True):
            pass

    def test_event_stream_limits_and_disconnect_release(self):
        import time
        from contextlib import ExitStack

        with ExitStack() as streams:
            readers = []
            for _ in range(2):
                response = streams.enter_context(
                    self.client.stream(
                        "GET", "/v1/events?follow=true", headers=self.headers
                    )
                )
                self.assertEqual(response.status_code, 200)
                readers.append(response.iter_raw())
                self.assertTrue(next(readers[-1]))
            with self.client.stream(
                "GET", "/v1/events?follow=true", headers=self.headers
            ) as blocked:
                self.assertEqual(blocked.status_code, 429)
                blocked.read()
                self.assert_problem(blocked, 429, "stream_limit")
            self.assertEqual(
                self.client.get("/v1/me", headers=self.headers).status_code, 200
            )
            with Store(self.database, writable=True):
                pass
        deadline = time.monotonic() + 3
        while True:
            with self.client.stream(
                "GET", "/v1/events?follow=true", headers=self.headers
            ) as reopened:
                if reopened.status_code == 200:
                    self.assertTrue(next(reopened.iter_raw()))
                    break
                reopened.read()
                self.assert_problem(reopened, 429, "stream_limit")
            self.assertLess(
                time.monotonic(),
                deadline,
                "SSE disconnect did not release stream guards",
            )
            time.sleep(0.02)
