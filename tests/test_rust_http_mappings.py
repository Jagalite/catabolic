# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native OpenAPI approval, mapping admission and bounded loopback delivery."""

import copy
import json
import os
import subprocess
import unittest

from catabolic.access import grant_put, issue, principal_put
from catabolic.http_mappings import prepare
from catabolic.openapi_operations import compile_operation
from catabolic.saved_queries import Queries
from catabolic.store import Store
from tests import test_http_query_mappings as fixtures
from tests.test_rust_migration import BINARY


class NativeHTTPMappingTest(unittest.TestCase):
    title = fixtures.HTTPMappingTest.title

    def setUp(self):
        self.assertTrue(BINARY.is_file(), "build the native executable first")
        fixtures.HTTPMappingTest.setUp(self)

    def native(self, surface, operation, request=None, *, ok=True, env=None):
        completed = subprocess.run(
            [
                str(BINARY),
                "--db",
                str(self.database),
                "operation",
                surface,
                operation,
                "--options",
                json.dumps(request or {}),
            ],
            capture_output=True,
            text=True,
            timeout=60,
            env={**os.environ, **(env or {})},
        )
        self.assertEqual(
            completed.returncode, 0 if ok else 2, completed.stderr or completed.stdout
        )
        return json.loads(completed.stdout) if ok else completed.stderr

    def preview(self, definition=None, *, ok=True):
        return self.native(
            "http-mappings", "run", {"definition": definition or self.definition}, ok=ok
        )

    def apply(self, definition=None, plan=None, *, ok=True, **options):
        definition = definition or self.definition
        return self.native(
            "http-mappings",
            "run",
            {
                "definition": definition,
                "apply": True,
                "expected_plan": (plan or self.preview(definition))["plan_id"],
                **options,
            },
            ok=ok,
        )

    def drain(self, **kwargs):
        return self.native("notify", "drain", **kwargs)

    def response_operation(self, responses):
        spec = copy.deepcopy(self.spec)
        spec["paths"]["/items/{id}/notify"]["post"]["responses"] = responses
        return self.native(
            "http-operations",
            "put",
            {
                "name": "responses",
                "document": spec,
                "operation_id": "notifyItem",
                "base_url": self.base,
                "environment": "RECEIVER_TOKEN",
            },
        )["id"]

    def test_compiler_matches_python_exactly_and_retains_credentials_as_names(self):
        request = {
            "document": self.spec,
            "operation_id": "notifyItem",
            "base_url": self.base,
            "environment": "RECEIVER_TOKEN",
        }
        actual = self.native("http-operations", "compile", request)
        self.assertEqual(
            actual,
            compile_operation(self.spec, "notifyItem", self.base, "RECEIVER_TOKEN"),
        )
        result = self.native("http-operations", "put", {**request, "name": "native"})
        repeat = self.native("http-operations", "put", {**request, "name": "native"})
        self.assertEqual(result, repeat)
        with Store(self.database) as store:
            row = store.rows(
                "SELECT * FROM http_operations WHERE id=?", (result["id"],)
            )[0]
            self.assertEqual(json.loads(row["definition"]), actual)
            self.assertNotIn("private-test-token", row["definition"])
        self.assertEqual(self.received, [])

    def test_preview_exact_parity_and_stale_plan_rollback(self):
        with Store(self.database) as store:
            expected = prepare(store, "default", self.definition)
        self.assertEqual(self.preview(), expected)
        self.title("Changed")
        self.assertIn("stale_plan_preview_again", self.apply(plan=expected, ok=False))
        with Store(self.database) as store:
            self.assertEqual(store.rows("SELECT * FROM notification_deliveries"), [])
            self.assertEqual(store.rows("SELECT * FROM http_mapping_ledger"), [])
        bad = copy.deepcopy(self.definition)
        bad["body"]["id"] = {"constant": 123}
        self.assertIn("body_schema_mismatch", self.preview(bad, ok=False))

    def test_native_preview_enqueue_retry_and_immutable_request(self):
        plan = self.preview()
        self.assertEqual(self.apply(plan=plan)["queued"], 1)
        self.assertEqual(self.apply()["queued"], 0)
        self.title("Not admitted")
        self.status = 503
        result = self.drain()
        self.assertEqual(result["deliveries"][0]["state"], "retry")
        with Store(self.database, writable=True) as store, store.transaction() as db:
            db.execute("UPDATE notification_deliveries SET due_at=0")
        self.status = 204
        self.assertTrue(self.drain()["complete"])
        self.assertEqual(self.received[0], self.received[1])
        self.assertEqual(self.received[0]["auth"], "Bearer private-test-token")
        self.assertEqual(self.received[0]["body"]["title"], "First")
        self.assertEqual(self.apply()["queued"], 1)
        self.assertTrue(self.drain()["complete"])
        self.assertEqual(self.received[-1]["body"]["title"], "Not admitted")
        self.assertNotEqual(self.received[0]["id"], self.received[-1]["id"])

    def test_supersession_cancels_pending_but_live_leases_block_and_disable(self):
        self.apply()
        self.title("Second")
        self.apply()
        with Store(self.database, writable=True) as store, store.transaction() as db:
            self.assertEqual(
                [
                    r["state"]
                    for r in store.rows(
                        "SELECT state FROM notification_deliveries ORDER BY rowid"
                    )
                ],
                ["cancelled", "pending"],
            )
            db.execute(
                "UPDATE notification_deliveries SET state='leased',lease_token='live',lease_until=9999999999 WHERE state='pending'"
            )
        self.title("Third")
        self.assertIn("delivery_in_progress", self.apply(ok=False))
        self.native("http-operations", "disable", {"id": self.operation})
        self.assertTrue(self.drain()["complete"])
        self.assertEqual(self.received, [])

    def test_response_contract_rejects_invalid_json_duplicates_and_overflow(self):
        operation = self.response_operation(
            {
                "200": {
                    "description": "Accepted",
                    "content": {
                        "application/json": {
                            "schema": {
                                "type": "object",
                                "properties": {"accepted": {"const": True}},
                                "required": ["accepted"],
                                "additionalProperties": False,
                            }
                        }
                    },
                }
            }
        )
        cases = [
            (200, "application/json", b'{"accepted":false}', "invalid_response"),
            (200, "application/json", b"{}", "invalid_response"),
            (200, "application/json", b"private-secret not json", "invalid_response"),
            (200, "text/plain", b'{"accepted":true}', "invalid_response"),
            (200, "application/json", b"", "invalid_response"),
            (200, "application/json", b"\xff", "invalid_response"),
            (200, "application/json", b" " * 65537, "invalid_response"),
            (200, "application/json", b'{"accepted":NaN}', "invalid_response"),
            (200, "application/json", b'{"accepted":1e999}', "invalid_response"),
            (
                200,
                "application/json",
                b'{"accepted":false,"accepted":true}',
                "invalid_response",
            ),
            (201, "application/json", b'{"accepted":true}', "unexpected_response"),
        ]
        for index, (status, media, body, expected) in enumerate(cases):
            with self.subTest(index=index):
                mapping = {
                    **self.definition,
                    "id": f"response-{index}",
                    "operation": operation,
                }
                self.status, self.response_type, self.response_body = (
                    status,
                    media,
                    body,
                )
                self.apply(mapping)
                result = self.drain()
                self.assertEqual(result["deliveries"][0]["state"], "repair")
                self.assertNotIn("private-secret", json.dumps(result))
                with Store(self.database) as store:
                    self.assertEqual(
                        store.rows(
                            "SELECT error FROM notification_deliveries WHERE event_id=?",
                            (result["deliveries"][0]["event_id"],),
                        )[0]["error"],
                        expected,
                    )
        self.status, self.response_type, self.response_body = (
            200,
            "application/json; charset=utf-8",
            b'{"accepted":true}',
        )
        self.native("http-operations", "retry", {"id": operation})
        self.assertTrue(self.native("notify", "drain", {"limit": 100})["complete"])
        self.assertEqual(len(self.remote_events), len(cases))

    def test_response_precedence_no_content_and_no_cross_origin_redirects(self):
        operation = self.response_operation(
            {
                "2XX": {"description": "Empty"},
                "201": {
                    "description": "JSON",
                    "content": {"application/json": {"schema": {"type": "object"}}},
                },
            }
        )
        self.apply({**self.definition, "operation": operation})
        self.status = 201
        self.assertEqual(self.drain()["deliveries"][0]["state"], "repair")
        self.native("http-operations", "retry", {"id": operation})
        self.status = 202
        self.assertTrue(self.drain()["complete"])
        self.response_body = b"unexpected"
        self.apply({**self.definition, "id": "no-content"})
        self.status = 204
        self.assertEqual(self.drain()["deliveries"][0]["state"], "repair")

    def test_compiler_rejects_remote_recursive_refs_security_and_path_escapes(self):
        for reference in [
            "https://example.invalid/schema",
            "file:///tmp/schema",
            "#/components/schemas/Event",
        ]:
            spec = copy.deepcopy(self.spec)
            spec["components"]["schemas"]["Event"] = {"$ref": reference}
            error = self.native(
                "http-operations",
                "compile",
                {
                    "document": spec,
                    "operation_id": "notifyItem",
                    "base_url": self.base,
                    "environment": "RECEIVER_TOKEN",
                },
                ok=False,
            )
            self.assertIn("external_or_recursive_reference", error)
        for suffix in ["?", "#", "?key=fake", "#fragment"]:
            self.native(
                "http-operations",
                "compile",
                {
                    "document": self.spec,
                    "operation_id": "notifyItem",
                    "base_url": self.base + suffix,
                    "environment": "RECEIVER_TOKEN",
                },
                ok=False,
            )
        for value in ["../other", "%2fescape", "..", "a\\b", "bad\nvalue"]:
            mapping = copy.deepcopy(self.definition)
            mapping["parameters"]["path"]["id"] = {"constant": value}
            self.assertIn("invalid_path_value", self.preview(mapping, ok=False))
        self.assertEqual(self.received, [])

    def test_complete_graphql_connection_and_unicode_wire_bytes(self):
        with Store(self.database, writable=True) as store:
            query = Queries(store).put(
                "document",
                {
                    "mode": "document",
                    "selection": {
                        "language": "graphql",
                        "query": "{items(first:100){nodes{id title} pageInfo{hasNextPage}}}",
                    },
                },
            )["id"]
        mapping = {
            **self.definition,
            "id": "document",
            "query": query,
            "rows_path": ["items"],
        }
        self.title("Café 日本語 😀")
        with Store(self.database) as store:
            expected = prepare(store, "default", mapping)
        self.assertEqual(self.preview(mapping), expected)
        self.apply(mapping)
        self.assertTrue(self.drain()["complete"])
        self.assertEqual(self.received[0]["body"]["title"], "Café 日本語 😀")
        self.assertEqual(
            self.received[0]["body_bytes"],
            len(
                json.dumps(
                    self.received[0]["body"], sort_keys=True, ensure_ascii=False
                ).encode()
            ),
        )

    def test_credentials_missing_bad_and_tokens_revoked_never_dispatch(self):
        self.apply()
        for token, expected in [
            ("", "credential_unavailable"),
            ("bad\ntoken", "invalid_destination"),
        ]:
            result = self.drain(env={"RECEIVER_TOKEN": token})
            self.assertEqual(result["deliveries"][0]["state"], "repair")
            with Store(self.database) as store:
                self.assertEqual(
                    store.rows("SELECT error FROM notification_deliveries")[0]["error"],
                    expected,
                )
            self.native("http-operations", "retry", {"id": self.operation})
        with Store(self.database, writable=True) as store:
            principal_put(store, "default", "approver")
            grant = grant_put(
                store,
                "approver",
                {"actions": ["webhooks:manage", "sql:read"], "operator": True},
            )["grant_id"]
            token = issue(store, "approver", [grant])["token"].split(".")[0]
            with store.transaction() as db:
                db.execute(
                    "UPDATE http_operations SET token_id=? WHERE id=?",
                    (token, self.operation),
                )
                db.execute("UPDATE api_tokens SET revoked=1 WHERE id=?", (token,))
        result = self.drain()
        self.assertEqual(result["deliveries"][0]["state"], "repair")
        self.assertEqual(self.received, [])

    def test_incomplete_queries_conflicting_keys_and_late_batch_failure_are_atomic(
        self,
    ):
        with Store(self.database, writable=True) as store:
            queries = [
                Queries(store).put(
                    name,
                    {"mode": "rows", "selection": {"language": "sql", "query": query}},
                )["id"]
                for name, query in [
                    (
                        "large",
                        "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<1001) SELECT cast(x AS text) AS id,'title' AS title FROM n",
                    ),
                    (
                        "conflict",
                        "SELECT 'same' AS id,'one' AS title UNION ALL SELECT 'same','two'",
                    ),
                    (
                        "batch",
                        "SELECT 'a' AS id,title FROM catalog_items UNION ALL SELECT 'z',title FROM catalog_items",
                    ),
                ]
            ]
        self.assertIn(
            "incomplete_query",
            self.preview({**self.definition, "query": queries[0]}, ok=False),
        )
        self.assertIn(
            "conflicting_key_rows",
            self.preview({**self.definition, "query": queries[1]}, ok=False),
        )
        with Store(self.database) as store:
            self.assertEqual(store.rows("SELECT * FROM notification_deliveries"), [])
        mapping = {**self.definition, "query": queries[2]}
        self.assertEqual(self.apply(mapping)["queued"], 2)
        with Store(self.database, writable=True) as store, store.transaction() as db:
            db.execute(
                "UPDATE notification_deliveries SET state='leased',lease_token='live',lease_until=9999999999 WHERE event_id IN (SELECT event_id FROM http_mapping_ledger WHERE key='z')"
            )
            before = {
                table: store.rows(f"SELECT * FROM {table}")
                for table in [
                    "consumer_events",
                    "notification_deliveries",
                    "http_mapping_ledger",
                    "http_mapping_deliveries",
                ]
            }
        self.title("Changed")
        self.assertIn("delivery_in_progress", self.apply(mapping, ok=False))
        with Store(self.database) as store:
            after = {table: store.rows(f"SELECT * FROM {table}") for table in before}
        self.assertEqual(before, after)


if __name__ == "__main__":
    unittest.main()
