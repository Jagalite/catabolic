# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import importlib.util
import tempfile
import unittest
from pathlib import Path

from catabolic.access import grant_put, issue, principal_put, revoke
from catabolic.app import Application
from catabolic.content_access import revision_of
from catabolic.domain import CatabolicError
from catabolic.store import Store

HTTP_AVAILABLE = (
    importlib.util.find_spec("fastapi") is not None
    and importlib.util.find_spec("httpx") is not None
)


@unittest.skipUnless(HTTP_AVAILABLE, "optional HTTP test dependencies")
class HTTPTest(unittest.TestCase):
    def setUp(self):
        from fastapi.testclient import TestClient

        from catabolic.http.app import create_app

        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.source = self.root / "media"
        self.source.mkdir()
        self.payload = bytes(range(256)) * 1024
        (self.source / "allowed.bin").write_bytes(self.payload)
        (self.source / "hidden.bin").write_bytes(b"hidden content")
        self.database = self.root / "catalog.db"
        Store.initialize(self.database)
        with Store(self.database, writable=True) as store:
            app = Application(store)
            app.bind("source", "media", str(self.source))
            app.scan()
            files = store.rows("SELECT * FROM files ORDER BY path")
            self.file, self.hidden_file = [r["id"] for r in files]
            self.item = app.put_item(
                "movie",
                {"test": "allowed"},
                {"title": "Allowed", "year": 2026, "private_note": "DO_NOT_DISCLOSE"},
            )["id"]
            self.hidden = app.put_item(
                "movie", {"test": "hidden"}, {"title": "Hidden title"}
            )["id"]
            app.media.associate(self.file, self.item)
            app.media.associate(self.hidden_file, self.hidden)
            principal_put(store, "default", "site")
            self.definition = {
                "actions": [
                    "metadata:read",
                    "content:read",
                    "events:read",
                    "processing:request",
                ],
                "item_ids": [self.item],
                "file_ids": [self.file],
                "metadata_fields": ["title", "year"],
            }
            self.grant = grant_put(store, "site", self.definition)["grant_id"]
            self.credential = issue(store, "site", [self.grant])
            principal_put(store, "default", "other")
            other = grant_put(
                store,
                "other",
                {
                    "actions": ["metadata:read", "content:read", "events:read"],
                    "item_ids": [self.hidden],
                    "file_ids": [self.hidden_file],
                },
            )["grant_id"]
            self.other = issue(store, "other", [other])["token"]
            principal_put(store, "default", "operator")
            operator = grant_put(
                store, "operator", {"actions": ["*"], "operator": True}
            )["grant_id"]
            self.operator = issue(store, "operator", [operator])["token"]
            with store.transaction() as db:
                db.execute("INSERT INTO api_sources VALUES ('default','media')")
            self.revision = revision_of(store, "default", self.file)
        self.client = TestClient(create_app(self.database, read_only=False))
        self.addCleanup(self.client.close)
        self.headers = {"Authorization": "Bearer " + self.credential["token"]}
        self.content = f"/v1/files/{self.file}/content?revision={self.revision}"

    def test_authentication_and_private_fields(self):
        self.assertEqual(self.client.get("/v1/me").status_code, 401)
        result = self.client.get("/v1/items", headers=self.headers)
        self.assertEqual(result.status_code, 200, result.text)
        self.assertEqual([r["id"] for r in result.json()["data"]], [self.item])
        self.assertNotIn("DO_NOT_DISCLOSE", result.text)
        self.assertNotIn(self.hidden, result.text)
        self.assertEqual(
            self.client.get(
                "/v1/items/" + self.hidden, headers=self.headers
            ).status_code,
            404,
        )
        self.assertEqual(
            self.client.get(
                "/v1/files/" + self.hidden_file, headers=self.headers
            ).status_code,
            404,
        )

    def test_graphql_authorizes_before_search_and_nested_results(self):
        query = "{ items { nodes { id title metadata associations { nodes { file { id size } } } } pageInfo { hasNextPage } } }"
        result = self.client.post(
            "/v1/query/graphql", headers=self.headers, json={"query": query}
        )
        self.assertEqual(result.status_code, 200, result.text)
        self.assertNotIn("errors", result.json(), result.text)
        self.assertNotIn("DO_NOT_DISCLOSE", result.text)
        self.assertNotIn(self.hidden, result.text)
        self.assertEqual(len(result.json()["data"]["items"]["nodes"]), 1)
        result = self.client.post(
            "/v1/query/graphql",
            headers=self.headers,
            json={"query": '{ items(search:"DO_NOT_DISCLOSE") { nodes { id } } }'},
        )
        self.assertNotIn(self.item, result.text)
        result = self.client.post(
            "/v1/query/graphql",
            headers=self.headers,
            json={"query": "{ files { nodes { id sourcePath } } }"},
        )
        self.assertIn("FORBIDDEN", result.text)
        self.assertNotIn(str(self.source), result.text)

    def test_sql_operator_only_and_security_aliases_denied(self):
        self.assertEqual(
            self.client.post(
                "/v1/query/sql", headers=self.headers, json={"query": "SELECT 1"}
            ).status_code,
            403,
        )
        headers = {"Authorization": "Bearer " + self.operator}
        result = self.client.post(
            "/v1/query/sql",
            headers=headers,
            json={"query": "SELECT count(*) FROM items"},
        )
        self.assertEqual(result.status_code, 200, result.text)
        for query in (
            "SELECT digest FROM api_tokens",
            "SELECT t.digest FROM api_tokens t",
            "WITH x AS (SELECT * FROM api_tokens) SELECT * FROM x",
            "SELECT token FROM execution_claims",
        ):
            result = self.client.post(
                "/v1/query/sql", headers=headers, json={"query": query}
            )
            self.assertNotEqual(result.status_code, 200, result.text)
            self.assertNotIn(self.credential["token"], result.text)

    def test_exact_full_ranges_head_and_revision(self):
        result = self.client.get(self.content, headers=self.headers)
        self.assertEqual(result.status_code, 200, result.text[:200])
        self.assertEqual(result.content, self.payload)
        self.assertEqual(result.headers["content-length"], str(len(self.payload)))
        self.assertTrue(result.headers["etag"].startswith("W/"))
        for value, expected in [
            ("bytes=1-7", self.payload[1:8]),
            ("bytes=-11", self.payload[-11:]),
            ("bytes=100-", self.payload[100:]),
        ]:
            result = self.client.get(
                self.content, headers={**self.headers, "Range": value}
            )
            self.assertEqual(result.status_code, 206, result.text[:100])
            self.assertEqual(result.content, expected)
        result = self.client.head(self.content, headers=self.headers)
        self.assertEqual(result.status_code, 200)
        self.assertEqual(result.content, b"")
        self.assertEqual(result.headers["content-length"], str(len(self.payload)))
        self.assertEqual(
            self.client.get(
                self.content.replace(self.revision, "stale"), headers=self.headers
            ).status_code,
            409,
        )

    def test_ranges_conditionals_and_forbidden_alternate(self):
        for value in ("bytes=9999999-", "bytes=-0", "bytes=0-1,4-5", "bytes=5-1"):
            self.assertEqual(
                self.client.get(
                    self.content, headers={**self.headers, "Range": value}
                ).status_code,
                416,
            )
        self.assertEqual(
            self.client.get(
                self.content,
                headers={**self.headers, "If-None-Match": f'W/"{self.revision}"'},
            ).status_code,
            304,
        )
        self.assertEqual(
            self.client.get(
                self.content, headers={"Authorization": "Bearer " + self.other}
            ).status_code,
            404,
        )

    def test_tickets_multiple_ranges_budget_and_revoke(self):
        result = self.client.post(
            "/v1/content-access",
            headers=self.headers,
            json={"file_id": self.file, "revision": self.revision, "max_bytes": 20},
        )
        self.assertEqual(result.status_code, 200, result.text)
        ticket = result.json()
        for _ in range(2):
            result = self.client.get(
                ticket["content_path"], headers={"Range": "bytes=0-9"}
            )
            self.assertEqual(result.status_code, 206, result.text)
            self.assertEqual(result.content, self.payload[:10])
        self.assertEqual(
            self.client.get(
                ticket["content_path"], headers={"Range": "bytes=0-1"}
            ).status_code,
            429,
        )
        self.client.post(
            "/v1/content-access/" + ticket["ticket_id"] + "/revoke",
            headers=self.headers,
        )
        self.assertEqual(
            self.client.get(
                ticket["content_path"], headers={"Range": "bytes=0-1"}
            ).status_code,
            401,
        )

    def test_revocation_applies_to_existing_ticket(self):
        ticket = self.client.post(
            "/v1/content-access",
            headers=self.headers,
            json={"file_id": self.file, "revision": self.revision},
        ).json()["content_path"]
        with Store(self.database, writable=True) as store:
            revoke(store, self.credential["token_id"])
        self.assertEqual(self.client.get(ticket).status_code, 401)
        self.assertEqual(
            self.client.get("/v1/items", headers=self.headers).status_code, 401
        )

    def test_source_exposure_and_changes_fail_closed(self):
        with Store(self.database, writable=True) as store:
            with store.transaction() as db:
                db.execute("DELETE FROM api_sources")
        self.assertEqual(
            self.client.get(self.content, headers=self.headers).status_code, 403
        )
        with Store(self.database, writable=True) as store:
            with store.transaction() as db:
                db.execute("INSERT INTO api_sources VALUES ('default','media')")
        (self.source / "allowed.bin").write_bytes(b"changed")
        self.assertNotEqual(
            self.client.get(self.content, headers=self.headers).status_code, 200
        )

    def test_openapi_and_host_validation(self):
        result = self.client.get("/v1/openapi.json", headers=self.headers)
        self.assertEqual(result.status_code, 200, result.text)
        self.assertIn("/v1/rendition-requests", result.json()["paths"])
        self.assertEqual(
            self.client.get(
                "/v1/me", headers={**self.headers, "Host": "evil.test"}
            ).status_code,
            400,
        )

    def test_cross_principal_cursors(self):
        with Store(self.database, writable=True) as store:
            grant_put(
                store,
                "site",
                {**self.definition, "item_ids": [self.item, self.hidden]},
                self.grant,
            )
        first = self.client.get("/v1/items?limit=1", headers=self.headers).json()
        self.assertIsNotNone(first["next_cursor"])
        result = self.client.get(
            "/v1/items",
            params={"cursor": first["next_cursor"]},
            headers={"Authorization": "Bearer " + self.other},
        )
        self.assertEqual(result.status_code, 400)
        with Store(self.database, writable=True) as store:
            grant_put(store, "site", self.definition, self.grant)
        self.assertEqual(
            self.client.get(
                "/v1/items",
                params={"cursor": first["next_cursor"]},
                headers=self.headers,
            ).status_code,
            400,
        )

    def test_dynamic_grant_rechecks_membership(self):
        from catabolic.saved_queries import Queries

        with Store(self.database, writable=True) as store:
            query = Queries(store).put(
                "public",
                {
                    "mode": "selection",
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS item_id FROM items WHERE json_extract(metadata,'$.title')='Allowed'",
                    },
                },
            )
            grant_put(
                store,
                "site",
                {"actions": ["metadata:read"], "query_id": query["id"]},
                self.grant,
            )
        result = self.client.get("/v1/items", headers=self.headers)
        self.assertEqual(result.status_code, 200, result.text)
        self.assertEqual(len(result.json()["data"]), 1)
        with Store(self.database, writable=True) as store:
            Application(store).put_item(
                "movie", {}, {"title": "Now private"}, self.item
            )
        self.assertEqual(
            self.client.get("/v1/items", headers=self.headers).json()["data"], []
        )

    def test_read_only_mode_rejects_processing(self):
        from fastapi.testclient import TestClient

        from catabolic.http.app import create_app

        with TestClient(create_app(self.database)) as client:
            result = client.post(
                "/v1/rendition-requests",
                headers=self.headers,
                json={
                    "item_id": self.item,
                    "source_file_id": self.file,
                    "source_revision": self.revision,
                    "operation_id": "none",
                },
            )
            self.assertEqual(result.status_code, 403)

    def test_no_descriptor_or_writer_leak_after_streams(self):
        for _ in range(3):
            self.assertEqual(
                self.client.get(self.content, headers=self.headers).status_code, 200
            )
        self.assertEqual(self.client.app.state.limits.active, {})
        with Store(self.database, writable=True) as store:
            self.assertEqual(
                store.rows("SELECT count(*) AS n FROM api_requests")[0]["n"], 0
            )

    def test_snapshot_does_not_override_later_revocation(self):
        result = self.client.post("/v1/items/snapshots", headers=self.headers)
        self.assertEqual(result.status_code, 200, result.text)
        identifier = result.json()["snapshot_id"]
        self.assertEqual(
            len(
                self.client.get(
                    "/v1/snapshots/" + identifier, headers=self.headers
                ).json()["data"]
            ),
            1,
        )
        self.assertEqual(
            self.client.get(
                "/v1/snapshots/" + identifier,
                headers={"Authorization": "Bearer " + self.other},
            ).status_code,
            404,
        )
        with Store(self.database, writable=True) as store:
            grant_put(store, "site", {"actions": ["metadata:read"]}, self.grant)
        self.assertEqual(
            self.client.get("/v1/snapshots/" + identifier, headers=self.headers).json()[
                "data"
            ],
            [],
        )

    def test_stream_memory_backpressure_and_database_release(self):
        import asyncio
        import tracemalloc

        filename = self.source / "allowed.bin"
        with filename.open("r+b") as stream:
            stream.truncate(32 * 1024 * 1024)
        with Store(self.database, writable=True) as store:
            Application(store).scan()
            revision = revision_of(store, "default", self.file)
        sizes = []

        async def receive():
            return {"type": "http.request", "body": b"", "more_body": False}

        async def send(message):
            if message["type"] == "http.response.start":
                self.assertEqual(message["status"], 200)
            if message["type"] == "http.response.body":
                sizes.append(len(message.get("body", b"")))
                if len(sizes) == 1:
                    with Store(self.database, writable=True) as store:
                        with store.transaction() as db:
                            db.execute(
                                "INSERT INTO api_audit(action) VALUES ('writer-during-stream')"
                            )
                await asyncio.sleep(0)

        scope = {
            "type": "http",
            "asgi": {"version": "3.0", "spec_version": "2.4"},
            "http_version": "1.1",
            "method": "GET",
            "scheme": "http",
            "path": f"/v1/files/{self.file}/content",
            "raw_path": f"/v1/files/{self.file}/content".encode(),
            "query_string": f"revision={revision}".encode(),
            "headers": [
                (b"host", b"testserver"),
                (b"authorization", self.headers["Authorization"].encode()),
            ],
            "client": ("127.0.0.1", 1234),
            "server": ("testserver", 80),
            "root_path": "",
        }
        tracemalloc.start()
        asyncio.run(self.client.app(scope, receive, send))
        _, peak = tracemalloc.get_traced_memory()
        tracemalloc.stop()
        self.assertEqual(sum(sizes), 32 * 1024 * 1024)
        self.assertLessEqual(max(sizes), 65536)
        self.assertLess(peak, 4 * 1024 * 1024)
        self.assertEqual(self.client.app.state.limits.active, {})

    def test_stream_aborts_on_in_place_mutation_and_releases_slots(self):
        import asyncio

        sent = []

        async def receive():
            return {"type": "http.request", "body": b"", "more_body": False}

        async def send(message):
            if message["type"] == "http.response.body" and message.get("body"):
                sent.append(len(message["body"]))
                if len(sent) == 1:
                    (self.source / "allowed.bin").write_bytes(b"replaced-in-place")

        scope = {
            "type": "http",
            "asgi": {"version": "3.0", "spec_version": "2.4"},
            "http_version": "1.1",
            "method": "GET",
            "scheme": "http",
            "path": f"/v1/files/{self.file}/content",
            "raw_path": f"/v1/files/{self.file}/content".encode(),
            "query_string": f"revision={self.revision}".encode(),
            "headers": [
                (b"host", b"testserver"),
                (b"authorization", self.headers["Authorization"].encode()),
            ],
            "client": ("127.0.0.1", 1234),
            "server": ("testserver", 80),
            "root_path": "",
        }
        with self.assertRaises((RuntimeError, OSError, CatabolicError)):
            asyncio.run(self.client.app(scope, receive, send))
        self.assertLess(sum(sent), len(self.payload))
        self.assertEqual(self.client.app.state.limits.active, {})

    def test_empty_file_head_and_unsatisfied_range(self):
        (self.source / "allowed.bin").write_bytes(b"")
        with Store(self.database, writable=True) as store:
            Application(store).scan()
            revision = revision_of(store, "default", self.file)
        url = f"/v1/files/{self.file}/content?revision={revision}"
        self.assertEqual(self.client.get(url, headers=self.headers).content, b"")
        result = self.client.head(url, headers=self.headers)
        self.assertEqual(result.headers["content-length"], "0")
        result = self.client.get(url, headers={**self.headers, "Range": "bytes=0-"})
        self.assertEqual(result.status_code, 416)
        self.assertEqual(result.headers["content-range"], "bytes */0")

    def test_private_catalog_bytes_and_registered_credential_are_never_served(self):
        from catabolic.api_cli import save_secret

        target = self.source / "allowed.bin"
        target.write_bytes(b"SQLite format 3\x00" + b"private database")
        with Store(self.database, writable=True) as store:
            Application(store).scan()
            revision = revision_of(store, "default", self.file)
        result = self.client.get(
            f"/v1/files/{self.file}/content?revision={revision}", headers=self.headers
        )
        self.assertEqual(result.status_code, 403)
        target.unlink()
        with Store(self.database, writable=True) as store:
            save_secret(store, target, issue(store, "site", [self.grant]))
            Application(store).scan()
            revision = revision_of(store, "default", self.file)
        result = self.client.get(
            f"/v1/files/{self.file}/content?revision={revision}", headers=self.headers
        )
        self.assertEqual(result.status_code, 403)
        self.assertNotIn('"token":', result.text)

    def test_expiry_and_revision_pins_apply_to_metadata_and_tickets(self):
        ticket = self.client.post(
            "/v1/content-access",
            headers=self.headers,
            json={"file_id": self.file, "revision": self.revision},
        ).json()
        with Store(self.database, writable=True) as store:
            with store.transaction() as db:
                db.execute("UPDATE api_tickets SET expires=0")
        self.assertEqual(self.client.get(ticket["content_path"]).status_code, 401)
        with Store(self.database, writable=True) as store:
            grant_put(
                store,
                "site",
                {**self.definition, "revisions": {self.file: "old"}},
                self.grant,
            )
        self.assertEqual(
            self.client.get("/v1/files/" + self.file, headers=self.headers).status_code,
            404,
        )
        self.assertEqual(
            self.client.get(self.content, headers=self.headers).status_code, 404
        )
        with Store(self.database, writable=True) as store:
            with store.transaction() as db:
                db.execute(
                    "UPDATE api_tokens SET expires=0 WHERE id=?",
                    (self.credential["token_id"],),
                )
        self.assertEqual(
            self.client.get("/v1/me", headers=self.headers).status_code, 401
        )

    def test_operator_preview_rolls_back_and_stale_plan_is_rejected(self):
        headers = {"Authorization": "Bearer " + self.operator}
        path = "/v1/operator/queries/public"
        definition = {
            "mode": "selection",
            "selection": {
                "language": "sql",
                "query": "SELECT id AS item_id FROM items",
            },
        }
        preview = self.client.post(
            path, headers=headers, json={"definition": definition}
        )
        self.assertEqual(preview.status_code, 200, preview.text)
        with Store(self.database) as store:
            self.assertEqual(store.rows("SELECT * FROM saved_queries"), [])
        applied = self.client.post(
            path,
            headers=headers,
            json={
                "definition": definition,
                "apply": True,
                "expected_plan": preview.json()["plan_id"],
            },
        )
        self.assertEqual(applied.status_code, 200, applied.text)
        self.assertEqual(
            self.client.post(
                path,
                headers=headers,
                json={
                    "definition": definition,
                    "apply": True,
                    "expected_plan": preview.json()["plan_id"],
                },
            ).status_code,
            409,
        )
        self.assertEqual(
            self.client.post(
                "/v1/operator/rules/bad",
                headers=headers,
                json={"definition": {"arbitrary": True}},
            ).status_code,
            422,
        )
        self.assertEqual(
            self.client.post(
                path, headers=self.headers, json={"definition": definition}
            ).status_code,
            403,
        )

    def test_worker_and_integration_leases_are_private_to_http_sql(self):
        headers = {"Authorization": "Bearer " + self.operator}
        for table in (
            "processor_jobs",
            "consumer_attempts",
            "consumer_deliveries",
            "notification_deliveries",
            "refresh_events",
        ):
            for query in (
                f"SELECT * FROM {table} secret",
                f"SELECT count(*) FROM {table}",
            ):
                result = self.client.post(
                    "/v1/query/sql", headers=headers, json={"query": query}
                )
                self.assertNotEqual(result.status_code, 200, query)

    def test_event_cursor_expiry_and_principal_binding(self):
        result = self.client.get("/v1/events", headers=self.headers)
        self.assertEqual(result.status_code, 200, result.text)
        cursor = result.json()["cursor"]
        self.assertEqual(
            self.client.get(
                "/v1/events",
                params={"cursor": cursor},
                headers={"Authorization": "Bearer " + self.other},
            ).status_code,
            409,
        )
        with Store(self.database, writable=True) as store:
            with store.transaction() as db:
                db.execute("UPDATE api_snapshots SET expires=0 WHERE id=?", (cursor,))
        response = self.client.get(
            "/v1/events", params={"cursor": cursor}, headers=self.headers
        )
        self.assertEqual(response.status_code, 409)
        self.assertEqual(response.json()["code"], "resync_required")

    def test_disconnect_releases_descriptor_and_stream_slot(self):
        import asyncio

        sent = []

        async def run():
            async def receive():
                return {"type": "http.request", "body": b"", "more_body": False}

            async def send(message):
                if message["type"] == "http.response.body" and message.get("body"):
                    sent.append(len(message["body"]))
                    raise OSError("client disconnected")

            scope = {
                "type": "http",
                "asgi": {"version": "3.0", "spec_version": "2.4"},
                "method": "GET",
                "scheme": "http",
                "path": f"/v1/files/{self.file}/content",
                "query_string": f"revision={self.revision}".encode(),
                "headers": [
                    (b"host", b"testserver"),
                    (b"authorization", self.headers["Authorization"].encode()),
                ],
                "server": ("testserver", 80),
                "client": ("127.0.0.1", 10000),
                "root_path": "",
                "http_version": "1.1",
            }
            from starlette.requests import ClientDisconnect

            with self.assertRaises(ClientDisconnect):
                await self.client.app(scope, receive, send)

        asyncio.run(run())
        self.assertEqual(len(sent), 1)
        self.assertEqual(self.client.app.state.limits.active, {})
        self.assertEqual(
            self.client.head(self.content, headers=self.headers).status_code, 200
        )

    def test_bounded_body_returns_problem_details(self):
        response = self.client.post(
            "/v1/query/graphql", headers=self.headers, json={"query": "x" * 1048600}
        )
        self.assertEqual(response.status_code, 413)
        self.assertEqual(response.json()["code"], "body_too_large")
        self.assertEqual(response.headers["content-type"], "application/problem+json")

    def test_head_range_unknown_units_and_weak_conditional_comparison(self):
        response = self.client.head(
            self.content, headers={**self.headers, "Range": "bytes=1-3"}
        )
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.headers["content-length"], str(len(self.payload)))
        self.assertNotIn("content-range", response.headers)
        response = self.client.get(
            self.content, headers={**self.headers, "Range": "unknown=1-3"}
        )
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.content, self.payload)
        response = self.client.get(
            self.content, headers={**self.headers, "Range": "Bytes=1-3"}
        )
        self.assertEqual(response.status_code, 206)
        self.assertEqual(response.content, self.payload[1:4])
        response = self.client.get(
            self.content,
            headers={**self.headers, "If-None-Match": f'"{self.revision}"'},
        )
        self.assertEqual(response.status_code, 304)
        self.assertEqual(response.content, b"")

    def test_sse_rechecks_revocation_without_holding_database_session(self):
        import asyncio

        messages = []

        async def run():
            async def receive():
                return {"type": "http.request", "body": b"", "more_body": False}

            async def send(message):
                if message["type"] == "http.response.body" and message.get("body"):
                    messages.append(message["body"])
                    if len(messages) == 1:
                        with Store(self.database, writable=True) as store:
                            revoke(store, self.credential["token_id"])

            scope = {
                "type": "http",
                "asgi": {"version": "3.0", "spec_version": "2.4"},
                "method": "GET",
                "scheme": "http",
                "path": "/v1/events",
                "query_string": b"follow=true",
                "headers": [
                    (b"host", b"testserver"),
                    (b"authorization", self.headers["Authorization"].encode()),
                ],
                "server": ("testserver", 80),
                "client": ("127.0.0.1", 10000),
                "root_path": "",
                "http_version": "1.1",
            }
            await asyncio.wait_for(self.client.app(scope, receive, send), timeout=5)

        asyncio.run(run())
        self.assertEqual(len(messages), 2)
        self.assertIn(b"resync-required", messages[-1])
        self.assertIn(b"unauthorized", messages[-1])
        self.assertEqual(self.client.app.state.limits.active, {})
