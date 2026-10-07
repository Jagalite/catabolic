# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import copy
import importlib.util
import json
import os
import tempfile
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from threading import Thread
from unittest.mock import patch

from catabolic.app import Application
from catabolic.consumer_adapters import ConsumerError
from catabolic.destination_mappings import run
from catabolic.http_mappings import prepare
from catabolic.notifications import drain
from catabolic.openapi_operations import disable, put
from catabolic.saved_queries import Queries
from catabolic.store import Store
from catabolic.watchers import Watchers


@unittest.skipUnless(importlib.util.find_spec("jsonschema"), "optional OpenAPI extra")
class HTTPMappingTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.database = Path(tmp.name) / "catalog.db"
        Store.initialize(self.database)
        self.received = []
        self.status = 204
        self.response_body = b""
        self.response_type = "application/json"
        self.remote_items = {}
        self.remote_events = set()
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                raw = self.rfile.read(int(self.headers["Content-Length"]))
                fixture.received.append(
                    {
                        "path": self.path,
                        "auth": self.headers.get("Authorization"),
                        "id": self.headers["Idempotency-Key"],
                        "body": json.loads(raw),
                        "body_bytes": len(raw),
                    }
                )
                if (
                    200 <= fixture.status < 300
                    and self.headers["Idempotency-Key"] not in fixture.remote_events
                ):
                    fixture.remote_events.add(self.headers["Idempotency-Key"])
                    body = json.loads(raw)
                    fixture.remote_items[body["id"]] = body
                self.send_response(fixture.status)
                self.send_header("Content-Type", fixture.response_type)
                self.send_header("Content-Length", str(len(fixture.response_body)))
                self.end_headers()
                self.wfile.write(fixture.response_body)

            def do_GET(self):
                raw = json.dumps(fixture.remote_items).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

            def log_message(self, *args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.base = f"http://127.0.0.1:{self.server.server_port}"
        self.spec = {
            "openapi": "3.1.0",
            "info": {"title": "Receiver", "version": "1"},
            "paths": {
                "/items/{id}/notify": {
                    "post": {
                        "operationId": "notifyItem",
                        "parameters": [
                            {
                                "name": "id",
                                "in": "path",
                                "required": True,
                                "schema": {"type": "string"},
                            },
                            {
                                "name": "ready",
                                "in": "query",
                                "schema": {"type": "boolean"},
                            },
                        ],
                        "security": [{"bearer": []}],
                        "requestBody": {
                            "required": True,
                            "content": {
                                "application/json": {
                                    "schema": {"$ref": "#/components/schemas/Event"}
                                }
                            },
                        },
                        "responses": {"204": {"description": "Accepted"}},
                    }
                }
            },
            "components": {
                "securitySchemes": {"bearer": {"type": "http", "scheme": "bearer"}},
                "schemas": {
                    "Event": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string"},
                            "title": {"type": "string"},
                        },
                        "required": ["id", "title"],
                        "additionalProperties": False,
                    }
                },
            },
        }
        with Store(self.database, writable=True) as store:
            app = Application(store)
            self.item = app.put_item("movie", {"test": "one"}, {"title": "First"})["id"]
            self.operation = put(
                store,
                "default",
                "receiver",
                self.spec,
                "notifyItem",
                self.base,
                "RECEIVER_TOKEN",
            )["id"]
            self.query = Queries(store).put(
                "outgoing",
                {
                    "mode": "rows",
                    "selection": {
                        "language": "sql",
                        "query": "SELECT item_id AS id,title FROM catalog_items",
                    },
                },
            )["id"]
        self.definition = {
            "version": 3,
            "id": "notify-ready",
            "query": self.query,
            "operation": self.operation,
            "key": {"column": "id"},
            "parameters": {
                "path": {"id": {"column": "id"}},
                "query": {"ready": {"constant": True}},
            },
            "body": {"id": {"column": "id"}, "title": {"column": "title"}},
        }
        self.env = patch.dict(os.environ, {"RECEIVER_TOKEN": "private-test-token"})
        self.env.start()
        self.addCleanup(self.env.stop)

    def preview(self, definition=None):
        return run(self.database, "default", definition or self.definition)

    def apply(self, preview=None):
        return run(
            self.database,
            "default",
            self.definition,
            apply=True,
            expected_plan=(preview or self.preview())["plan_id"],
        )

    def title(self, value):
        with Store(self.database, writable=True) as store:
            Application(store).put_item("movie", {"test": "one"}, {"title": value})

    def test_preview_apply_real_delivery_retry_and_no_repeat(self):
        plan = self.preview()
        self.assertEqual(self.received, [])
        self.assertNotIn("private-test-token", json.dumps(plan))
        self.assertEqual(self.apply(plan)["queued"], 1)
        self.assertEqual(self.apply()["queued"], 0)

        self.status = 503
        self.assertFalse(drain(self.database)["complete"])
        with Store(self.database, writable=True) as store, store.transaction() as db:
            db.execute("UPDATE notification_deliveries SET due_at=0")
        self.status = 204
        self.assertTrue(drain(self.database)["complete"])
        self.assertEqual(self.received[0], self.received[1])
        self.assertEqual(self.received[0]["auth"], "Bearer private-test-token")
        self.assertEqual(self.received[0]["body"], {"id": self.item, "title": "First"})
        self.assertTrue(self.received[0]["path"].endswith("?ready=true"))
        self.assertEqual(self.apply()["queued"], 0)
        self.title("Second")
        self.assertEqual(self.apply()["queued"], 1)
        drain(self.database)
        self.assertEqual(self.received[-1]["body"]["title"], "Second")
        self.assertNotEqual(self.received[0]["id"], self.received[-1]["id"])

    def response_operation(self, responses):
        spec = copy.deepcopy(self.spec)
        spec["paths"]["/items/{id}/notify"]["post"]["responses"] = responses
        with Store(self.database, writable=True) as store:
            return put(
                store,
                "default",
                "response-receiver",
                spec,
                "notifyItem",
                self.base,
                "RECEIVER_TOKEN",
            )["id"]

    def test_response_contract_and_remote_state_end_to_end(self):
        from urllib.request import urlopen

        schema = {
            "type": "object",
            "properties": {"id": {"const": self.item}, "accepted": {"const": True}},
            "required": ["id", "accepted"],
            "additionalProperties": False,
        }
        operation = self.response_operation(
            {
                "201": {
                    "description": "Stored",
                    "content": {"application/json": {"schema": schema}},
                }
            }
        )
        mapping = {**self.definition, "operation": operation}
        self.status = 201
        self.response_body = json.dumps({"id": self.item, "accepted": True}).encode()
        plan = self.preview(mapping)
        run(
            self.database, "default", mapping, apply=True, expected_plan=plan["plan_id"]
        )
        self.assertTrue(drain(self.database)["complete"])
        with urlopen(self.base + "/items") as response:
            remote = json.load(response)
        self.assertEqual(remote[self.item], plan["rows"][0]["request"]["body"])
        self.assertEqual(len(self.remote_events), 1)

    def test_invalid_success_responses_require_repair_and_can_be_retried(self):
        from catabolic.openapi_operations import retry

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
            (200, "application/json", b"not json private-secret", "invalid_response"),
            (200, "text/plain", b'{"accepted":true}', "invalid_response"),
            (200, "application/json", b"", "invalid_response"),
            (200, "application/json", b"\xff", "invalid_response"),
            (200, "application/json", b" " * 65537, "invalid_response"),
            (200, "application/json", b'{"accepted":NaN}', "invalid_response"),
            (
                200,
                "application/json",
                b'{"accepted":false,"accepted":true}',
                "invalid_response",
            ),
            (201, "application/json", b'{"accepted":true}', "unexpected_response"),
        ]
        for index, (status, media, body, error) in enumerate(cases):
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
                plan = self.preview(mapping)
                run(
                    self.database,
                    "default",
                    mapping,
                    apply=True,
                    expected_plan=plan["plan_id"],
                )
                result = drain(self.database)
                self.assertFalse(result["complete"])
                self.assertEqual(result["deliveries"][0]["state"], "repair")
                with Store(self.database) as store:
                    row = store.rows(
                        "SELECT error FROM notification_deliveries WHERE event_id=?",
                        (result["deliveries"][0]["event_id"],),
                    )[0]
                    self.assertEqual(row["error"], error)
        self.status, self.response_type, self.response_body = (
            200,
            "application/json; charset=utf-8",
            b'{"accepted":true}',
        )
        with Store(self.database, writable=True) as store:
            retry(store, "default", operation)
        self.assertTrue(drain(self.database)["complete"])
        self.assertEqual(len(self.remote_events), len(cases))

    def test_response_status_precedence_and_fallback(self):
        for key in ("2XX", "default"):
            with self.subTest(key=key):
                operation = self.response_operation(
                    {
                        key: {"description": "No body"},
                        "201": {
                            "description": "JSON",
                            "content": {
                                "application/json": {"schema": {"type": "object"}}
                            },
                        },
                    }
                )
                mapping = {
                    **self.definition,
                    "id": "range-" + key,
                    "operation": operation,
                }
                self.status, self.response_body = 201, b""
                plan = self.preview(mapping)
                run(
                    self.database,
                    "default",
                    mapping,
                    apply=True,
                    expected_plan=plan["plan_id"],
                )
                self.assertEqual(
                    drain(self.database)["deliveries"][0]["state"], "repair"
                )
                self.status = 202
                mapping = {**mapping, "id": "fallback-" + key}
                plan = self.preview(mapping)
                run(
                    self.database,
                    "default",
                    mapping,
                    apply=True,
                    expected_plan=plan["plan_id"],
                )
                self.assertEqual(
                    drain(self.database)["deliveries"][0]["state"], "complete"
                )

    def test_response_refs_are_pinned_and_remote_refs_rejected(self):
        spec = copy.deepcopy(self.spec)
        spec["components"]["responses"] = {
            "Stored": {
                "description": "Stored",
                "content": {
                    "application/json": {
                        "schema": {"$ref": "#/components/schemas/Event"}
                    }
                },
            }
        }
        spec["paths"]["/items/{id}/notify"]["post"]["responses"] = {
            "200": {"$ref": "#/components/responses/Stored"}
        }
        with Store(self.database, writable=True) as store:
            operation = put(
                store,
                "default",
                "refs",
                spec,
                "notifyItem",
                self.base,
                "RECEIVER_TOKEN",
            )["id"]
            spec["components"]["responses"]["Stored"]["content"]["application/json"][
                "schema"
            ] = {"$ref": "https://example.invalid/schema"}
            with self.assertRaises(ConsumerError):
                put(
                    store,
                    "default",
                    "refs",
                    spec,
                    "notifyItem",
                    self.base,
                    "RECEIVER_TOKEN",
                )
        mapping = {**self.definition, "operation": operation}
        self.status = 200
        self.response_body = json.dumps({"id": self.item, "title": "Stored"}).encode()
        plan = self.preview(mapping)
        run(
            self.database, "default", mapping, apply=True, expected_plan=plan["plan_id"]
        )
        self.assertTrue(drain(self.database)["complete"])

    def test_no_content_response_rejects_declared_body(self):
        self.response_body = b"unexpected"
        self.apply()
        self.assertEqual(drain(self.database)["deliveries"][0]["state"], "repair")

    def test_response_numbers_cannot_overflow_to_infinity(self):
        operation = self.response_operation(
            {
                "200": {
                    "description": "Number",
                    "content": {"application/json": {"schema": {"type": "number"}}},
                }
            }
        )
        mapping = {**self.definition, "operation": operation}
        self.status, self.response_body = 200, b"1e999"
        plan = self.preview(mapping)
        run(
            self.database, "default", mapping, apply=True, expected_plan=plan["plan_id"]
        )
        self.assertEqual(drain(self.database)["deliveries"][0]["state"], "repair")

    def test_stale_plan_schema_validation_and_atomic_failure(self):
        plan = self.preview()
        self.title("Changed")
        with self.assertRaises(ConsumerError):
            self.apply(plan)
        bad = copy.deepcopy(self.definition)
        bad["body"]["id"] = {"constant": 123}
        with self.assertRaises(ConsumerError):
            self.preview(bad)
        with Store(self.database) as store:
            self.assertEqual(store.rows("SELECT * FROM http_mapping_ledger"), [])
            self.assertEqual(store.rows("SELECT * FROM notification_deliveries"), [])

    def test_coalescing_live_lease_and_disable(self):
        self.apply()
        self.title("Second")
        self.assertEqual(self.apply()["queued"], 1)
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
        with self.assertRaises(ConsumerError):
            self.apply()
        with Store(self.database, writable=True) as store:
            disable(store, "default", self.operation)
        with patch("catabolic.notifications.send_request") as send:
            self.assertTrue(drain(self.database)["complete"])
        send.assert_not_called()

    def test_complete_graphql_connection_and_watcher(self):
        with Store(self.database, writable=True) as store:
            query = Queries(store).put(
                "graphql",
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
                "id": "graphql-notify",
                "query": query,
                "rows_path": ["items"],
            }
            watchers = Watchers(Application(store))
            watchers.put(
                "notify",
                {
                    "plan": {"kind": "query", "query_id": query},
                    "reaction": "http",
                    "mapping": mapping,
                },
            )
            first = watchers.run("notify")
            self.assertTrue(first["complete"], first)
            self.assertEqual(first["reaction"]["queued"], 1)
            repeated = watchers.run("notify")
            self.assertEqual(repeated["reaction"]["queued"], 0)
        self.assertTrue(drain(self.database)["complete"])
        self.assertEqual(len(self.received), 1)
        with Store(self.database, writable=True) as store:
            Application(store).put_item("movie", {"test": "two"}, {"title": "Second"})
            query = Queries(store).put(
                "truncated",
                {
                    "mode": "document",
                    "selection": {
                        "language": "graphql",
                        "query": "{items(first:1){nodes{id title} pageInfo{hasNextPage}}}",
                    },
                },
            )["id"]
            with self.assertRaises(ConsumerError):
                prepare(
                    store, "default", {**mapping, "id": "incomplete", "query": query}
                )
            watchers = Watchers(Application(store))
            watchers.put(
                "incomplete",
                {
                    "plan": {"kind": "query", "query_id": query},
                    "reaction": "http",
                    "mapping": {**mapping, "id": "incomplete", "query": query},
                },
            )
            failed = watchers.run("incomplete")
            self.assertFalse(failed["complete"])
            state = store.rows("SELECT baseline FROM watchers WHERE name='incomplete'")[
                0
            ]
            self.assertIsNone(state["baseline"])

    def test_watcher_failed_admission_preserves_baseline(self):
        import time

        with Store(self.database, writable=True) as store:
            app = Application(store)
            watchers = Watchers(app)
            watchers.put(
                "notify",
                {
                    "plan": {"kind": "query", "query_id": self.query},
                    "reaction": "http",
                    "mapping": self.definition,
                },
            )
            self.assertTrue(watchers.run("notify")["complete"])
            baseline = store.rows("SELECT baseline FROM watchers WHERE name='notify'")[
                0
            ]["baseline"]
            with store.transaction() as db:
                db.execute(
                    "UPDATE notification_deliveries SET state='leased',lease_until=?",
                    (time.time() + 60,),
                )
            app.put_item("movie", {"test": "one"}, {"title": "Changed"})
            failed = watchers.run("notify")
            self.assertFalse(failed["complete"])
            self.assertIn("delivery_in_progress", failed["error"])
            self.assertEqual(
                store.rows("SELECT baseline FROM watchers WHERE name='notify'")[0][
                    "baseline"
                ],
                baseline,
            )
            self.assertEqual(
                len(store.rows("SELECT * FROM http_mapping_deliveries")), 1
            )

    def test_openapi_ref_and_unsupported_security_rejected_without_network(self):
        with Store(self.database, writable=True) as store:
            for ref in ("https://evil.example/schema", "#/components/schemas/Event"):
                spec = copy.deepcopy(self.spec)
                spec["components"]["schemas"]["Event"] = {"$ref": ref}
                with self.assertRaises(ConsumerError):
                    put(
                        store,
                        "default",
                        "bad",
                        spec,
                        "notifyItem",
                        self.base,
                        "RECEIVER_TOKEN",
                    )
            spec = copy.deepcopy(self.spec)
            spec["components"]["securitySchemes"]["bearer"] = {
                "type": "oauth2",
                "flows": {},
            }
            with self.assertRaises(ConsumerError):
                put(
                    store,
                    "default",
                    "bad",
                    spec,
                    "notifyItem",
                    self.base,
                    "RECEIVER_TOKEN",
                )
        self.assertEqual(self.received, [])

    def test_base_url_rejects_empty_query_and_fragment(self):
        with Store(self.database, writable=True) as store:
            for suffix in ("?", "#", "?query=1", "#fragment"):
                with self.subTest(suffix=suffix), self.assertRaises(ConsumerError):
                    put(
                        store,
                        "default",
                        "bad-base",
                        self.spec,
                        "notifyItem",
                        self.base + suffix,
                        "RECEIVER_TOKEN",
                    )

    def test_unicode_body_uses_preview_encoding_on_wire(self):
        from catabolic.store import encode

        with Store(self.database, writable=True) as store:
            mapping = copy.deepcopy(self.definition)
            mapping["body"]["title"] = {"constant": "界" * 20000}
            plan = prepare(store, "default", mapping)
        admitted = run(
            self.database,
            "default",
            mapping,
            apply=True,
            expected_plan=plan["plan_id"],
        )
        self.assertEqual(admitted["queued"], 1)
        with patch.dict(os.environ, {"RECEIVER_TOKEN": "test-token"}):
            self.assertTrue(drain(self.database)["complete"])
        self.assertEqual(
            self.received[0]["body_bytes"],
            len(encode(plan["rows"][0]["request"]["body"]).encode()),
        )
        self.assertLessEqual(self.received[0]["body_bytes"], 65536)

    def test_cli_preview_and_apply_use_existing_mapping_commands(self):
        import subprocess
        import sys

        path = self.database.parent / "mapping.json"
        path.write_text(json.dumps(self.definition))

        def cli(*args):
            result = subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "catabolic",
                    "--db",
                    str(self.database),
                    "--json",
                    *args,
                ],
                capture_output=True,
                text=True,
                timeout=30,
            )
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
            return json.loads(result.stdout)

        preview = cli("projection", "mapping-preview", "--definition", str(path))
        self.assertEqual(self.received, [])
        applied = cli(
            "projection",
            "mapping-apply",
            "--definition",
            str(path),
            "--expected-plan",
            preview["plan_id"],
        )
        self.assertEqual(applied["queued"], 1)
        self.assertTrue(cli("notify", "run")["complete"])
        self.assertEqual(len(self.received), 1)
        history = cli("projection", "mapping-events")["events"]
        self.assertEqual(history[0]["state"], "complete")

    def test_incomplete_sql_and_conflicting_rows_never_queue(self):
        with Store(self.database, writable=True) as store:
            query = Queries(store).put(
                "too-large",
                {
                    "mode": "rows",
                    "selection": {
                        "language": "sql",
                        "query": "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<1001) SELECT cast(x AS text) AS id,'title' AS title FROM n",
                    },
                },
            )["id"]
            with self.assertRaises(ConsumerError):
                prepare(store, "default", {**self.definition, "query": query})
            query = Queries(store).put(
                "conflicting",
                {
                    "mode": "rows",
                    "selection": {
                        "language": "sql",
                        "query": "SELECT 'same' AS id,'one' AS title UNION ALL SELECT 'same','two'",
                    },
                },
            )["id"]
            with self.assertRaises(ConsumerError):
                prepare(store, "default", {**self.definition, "query": query})
            self.assertEqual(store.rows("SELECT * FROM notification_deliveries"), [])

    def test_batch_rollback_when_later_key_is_in_flight(self):
        with Store(self.database, writable=True) as store:
            query = Queries(store).put(
                "two-keys",
                {
                    "mode": "rows",
                    "selection": {
                        "language": "sql",
                        "query": "SELECT 'a' AS id,title FROM catalog_items UNION ALL SELECT 'z',title FROM catalog_items",
                    },
                },
            )["id"]
        self.definition = {**self.definition, "query": query}
        self.assertEqual(self.apply()["queued"], 2)
        with Store(self.database, writable=True) as store, store.transaction() as db:
            db.execute(
                "UPDATE notification_deliveries SET state='leased',lease_token='live',lease_until=9999999999 WHERE event_id IN (SELECT event_id FROM http_mapping_ledger WHERE key='z')"
            )
        self.title("Changed")
        with self.assertRaises(ConsumerError):
            self.apply()
        with Store(self.database) as store:
            self.assertEqual(
                len(store.rows("SELECT * FROM http_mapping_deliveries")), 2
            )
            self.assertEqual(
                store.rows(
                    "SELECT state FROM notification_deliveries WHERE event_id IN (SELECT event_id FROM http_mapping_ledger WHERE key='a')"
                )[0]["state"],
                "pending",
            )

    def test_admitted_request_is_frozen_until_explicit_reevaluation(self):
        self.apply()
        self.title("Later")
        self.assertTrue(drain(self.database)["complete"])
        self.assertEqual(self.received[0]["body"]["title"], "First")
        self.assertEqual(self.apply()["queued"], 1)
        self.assertTrue(drain(self.database)["complete"])
        self.assertEqual(self.received[1]["body"]["title"], "Later")


@unittest.skipUnless(
    all(importlib.util.find_spec(name) for name in ("jsonschema", "fastapi", "httpx")),
    "optional HTTP/OpenAPI extras",
)
class HTTPMappingAPITest(unittest.TestCase):
    def setUp(self):
        HTTPMappingTest.setUp(self)
        from fastapi.testclient import TestClient

        from catabolic.access import grant_put, issue, principal_put
        from catabolic.http.app import create_app

        with Store(self.database, writable=True) as store:
            for name, definition in [
                ("owner", {"actions": ["*"], "operator": True}),
                ("reader", {"actions": ["metadata:read"]}),
                ("other", {"actions": ["*"], "operator": True}),
            ]:
                principal_put(store, "default", name)
                grant = grant_put(store, name, definition)["grant_id"]
                token = issue(store, name, [grant])["token"]
                setattr(self, name, {"Authorization": "Bearer " + token})
                if name == "owner":
                    self.owner_grant = grant
        self.client = TestClient(create_app(self.database, read_only=False))
        self.addCleanup(self.client.close)

    def test_http_operator_approval_mapping_revocation_and_retry(self):
        from catabolic.access import issue, revoke

        body = {
            "name": "http-receiver",
            "spec": self.spec,
            "operation_id": "notifyItem",
            "base_url": self.base,
            "credential_env": "RECEIVER_TOKEN",
        }
        self.assertEqual(
            self.client.post(
                "/v1/http-operations", headers=self.reader, json=body
            ).status_code,
            403,
        )
        result = self.client.post("/v1/http-operations", headers=self.owner, json=body)
        self.assertEqual(result.status_code, 201, result.text)
        identifier = result.json()["id"]
        self.assertEqual(
            self.client.get(
                "/v1/http-operations/" + identifier, headers=self.other
            ).status_code,
            404,
        )
        mapping = {**self.definition, "operation": identifier}
        result = self.client.post(
            "/v1/http-mappings/preview",
            headers=self.owner,
            json={"definition": mapping},
        )
        self.assertEqual(result.status_code, 200, result.text)
        plan = result.json()
        self.assertEqual(
            self.client.post(
                "/v1/http-mappings/preview",
                headers=self.reader,
                json={"definition": mapping},
            ).status_code,
            403,
        )
        applied = self.client.post(
            "/v1/http-mappings/apply",
            headers=self.owner,
            json={"definition": mapping, "expected_plan": plan["plan_id"]},
        )
        self.assertEqual(applied.status_code, 200, applied.text)
        self.assertEqual(applied.json()["queued"], 1)
        old = self.owner["Authorization"].split(" ")[1].split(".")[0]
        with Store(self.database, writable=True) as store:
            revoke(store, old)
            token = issue(store, "owner", [self.owner_grant])["token"]
        self.assertEqual(drain(self.database)["deliveries"][0]["state"], "repair")
        self.assertEqual(self.received, [])
        self.owner = {"Authorization": "Bearer " + token}
        response = self.client.post(
            "/v1/http-operations/" + identifier + "/retry", headers=self.owner
        )
        self.assertEqual(response.status_code, 200, response.text)
        self.assertTrue(drain(self.database)["complete"])
        self.assertEqual(len(self.received), 1)
        history = self.client.get(
            "/v1/http-operations/" + identifier + "/deliveries", headers=self.owner
        )
        self.assertEqual(history.status_code, 200, history.text)
        self.assertEqual(history.json()["deliveries"][0]["state"], "complete")
        self.assertNotIn("private-test-token", history.text)

    def test_http_readonly_and_private_query_are_not_bypassed(self):
        from fastapi.testclient import TestClient

        from catabolic.http.app import create_app

        with TestClient(create_app(self.database)) as readonly:
            response = readonly.post(
                "/v1/http-mappings/apply",
                headers=self.owner,
                json={"definition": self.definition, "expected_plan": "unknown"},
            )
            self.assertEqual(response.status_code, 403)
        with Store(self.database, writable=True) as store:
            query = Queries(store).put(
                "private",
                {
                    "mode": "rows",
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id,definition AS title FROM http_operations",
                    },
                },
            )["id"]
        response = self.client.post(
            "/v1/http-mappings/preview",
            headers=self.owner,
            json={"definition": {**self.definition, "query": query}},
        )
        self.assertNotEqual(response.status_code, 200, response.text)
        self.assertEqual(self.received, [])
