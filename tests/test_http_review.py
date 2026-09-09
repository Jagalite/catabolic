# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import unittest

from catabolic.access import authenticate, grant_put, issue, revoke
from catabolic.api_events import page, prune
from catabolic.saved_queries import Queries
from catabolic.store import Store
from tests import test_http_backend as metadata
from tests import test_http_renditions as renditions


@unittest.skipUnless(metadata.HTTP_AVAILABLE, "optional HTTP test dependencies")
class HTTPReviewTest(unittest.TestCase):
    setUp = metadata.HTTPTest.setUp

    def test_stream_rejection_does_not_debit_ticket(self):
        ticket = self.client.post(
            "/v1/content-access",
            headers=self.headers,
            json={"file_id": self.file, "revision": self.revision, "max_bytes": 8},
        ).json()
        limits = self.client.app.state.limits
        limits.admit("site")
        limits.admit("site")
        try:
            result = self.client.get(
                ticket["content_path"], headers={"Range": "bytes=0-7"}
            )
            self.assertEqual(result.status_code, 429)
        finally:
            limits.release("site")
            limits.release("site")
        result = self.client.get(ticket["content_path"], headers={"Range": "bytes=0-7"})
        self.assertEqual(result.status_code, 206, result.text)

    def test_saved_document_requires_approval(self):
        with Store(self.database, writable=True) as store:
            query = Queries(store).put(
                "private-document",
                {
                    "mode": "document",
                    "selection": {"language": "graphql", "query": "{ profile }"},
                },
            )
        path = f"/v1/queries/{query['id']}/runs"
        self.assertEqual(
            self.client.post(path, headers=self.headers, json={}).status_code, 404
        )
        with Store(self.database, writable=True) as store:
            grant_put(
                store,
                "site",
                {**self.definition, "report_ids": [query["id"]]},
                self.grant,
            )
        self.assertEqual(
            self.client.post(path, headers=self.headers, json={}).status_code, 200
        )

    def test_saved_association_selection_is_not_silently_empty(self):
        with Store(self.database, writable=True) as store:
            query = Queries(store).put(
                "associations",
                {
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS association_id FROM item_files",
                    }
                },
            )
            expected = [
                r["id"]
                for r in store.rows(
                    "SELECT id FROM item_files WHERE item_id=?", (self.item,)
                )
            ]
            grant_put(
                store,
                "site",
                {**self.definition, "report_ids": [query["id"]]},
                self.grant,
            )
        result = self.client.post(
            f"/v1/queries/{query['id']}/runs", headers=self.headers, json={}
        )
        self.assertEqual(result.status_code, 200, result.text)
        self.assertEqual(result.json()["ids"], expected)

    def test_empty_cursor_reports_lost_retained_history(self):
        from catabolic.access import AccessError

        with Store(self.database, writable=True) as store:
            access = authenticate(store, self.credential["token"])
            cursor = page(access)["cursor"]
            with store.transaction() as db:
                db.execute(
                    "INSERT INTO api_events(profile,resource_type,resource_id,state,created_at) VALUES ('default','request','gone','ready',0)"
                )
            prune(store)
            with self.assertRaises(AccessError) as error:
                page(access, cursor)
            self.assertEqual(error.exception.code, "resync_required")

    def test_saved_selection_cannot_probe_security_tables(self):
        from catabolic.domain import CatabolicError

        with Store(self.database, writable=True) as store:
            query = Queries(store).put(
                "private-predicate",
                {
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS item_id FROM items WHERE EXISTS (SELECT digest FROM api_tokens WHERE digest != '')",
                    }
                },
            )
            # CLI query semantics are retained, but HTTP membership and reports
            # must apply the same security-table authorizer as direct HTTP SQL.
            self.assertIn(self.item, Queries(store).select(query["id"])[1])
            with self.assertRaises(CatabolicError):
                grant_put(store, "site", {**self.definition, "query_id": query["id"]})
            grant_put(
                store,
                "site",
                {**self.definition, "report_ids": [query["id"]]},
                self.grant,
            )
        response = self.client.post(
            f"/v1/queries/{query['id']}/runs", headers=self.headers, json={}
        )
        self.assertNotEqual(response.status_code, 200)
        self.assertNotIn(self.item, response.text)

    def test_sse_survives_contention_and_rechecks_revocation(self):
        import asyncio

        messages, held = [], []

        async def run():
            async def receive():
                return {"type": "http.request", "body": b"", "more_body": False}

            async def send(message):
                if message["type"] == "http.response.body" and message.get("body"):
                    messages.append(message["body"])
                    if len(messages) == 1:
                        held.append(Store(self.database, writable=True))
                    elif b"catalog-busy" in message["body"]:
                        held.pop().close()
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
            await asyncio.wait_for(self.client.app(scope, receive, send), timeout=8)

        try:
            asyncio.run(run())
        finally:
            for store in held:
                store.close()
        self.assertEqual(len(messages), 3)
        self.assertIn(b"catalog-busy", messages[1])
        self.assertIn(b"unauthorized", messages[-1])
        self.assertEqual(self.client.app.state.limits.active, {})


@unittest.skipUnless(
    not getattr(renditions.RenditionHTTPTest, "__unittest_skip__", False),
    "HTTP and FFmpeg required",
)
class RenditionReviewTest(unittest.TestCase):
    setUp = renditions.RenditionHTTPTest.setUp

    def test_processing_grant_revision_pin_is_enforced(self):
        import json

        with Store(self.database, writable=True) as store:
            for row in store.rows("SELECT * FROM api_grants WHERE principal='one'"):
                definition = json.loads(row["definition"])
                definition["revisions"] = {self.file: "old-revision"}
                grant_put(store, "one", definition, row["id"])
        result = self.client.post(
            "/v1/rendition-requests", headers=self.headers[0], json=self.body
        )
        self.assertEqual(result.status_code, 403, result.text)
        with Store(self.database) as store:
            self.assertEqual(store.rows("SELECT id FROM processing_jobs"), [])

    def test_retry_rebinds_to_current_token_after_rotation(self):
        from catabolic.http_worker_local import run

        result = self.client.post(
            "/v1/rendition-requests", headers=self.headers[0], json=self.body
        ).json()
        with Store(self.database, writable=True) as store:
            old = authenticate(store, self.headers[0]["Authorization"][7:])
            replacement = issue(store, "one", [g["_id"] for g in old.grants])
            revoke(store, old.token["id"])
        run(self.database, once=True)
        headers = {"Authorization": "Bearer " + replacement["token"]}
        retried = self.client.post(result["status_url"] + "/retry", headers=headers)
        self.assertEqual(retried.status_code, 200, retried.text)
        run(self.database, once=True)
        current = self.client.get(result["status_url"], headers=headers)
        self.assertEqual(current.json()["state"], "ready", current.text)

    def test_worker_handles_writer_contention_without_losing_supervisor_lock(self):
        from catabolic.http_worker_local import run

        with Store(self.database, writable=True):
            result = run(self.database, once=True)
        self.assertEqual(result["blockers"], ["catalog_busy"])
        # A second invocation must be able to acquire the supervisor lock.
        self.assertEqual(run(self.database, once=True)["processed"], 0)

    def test_stale_queued_request_releases_reserved_capacity(self):
        from catabolic.app import Application
        from catabolic.http_worker_local import run

        response = self.client.post(
            "/v1/rendition-requests", headers=self.headers[0], json=self.body
        ).json()
        source = self.root / "source" / "sample.mkv"
        source.write_bytes(source.read_bytes() + b"changed")
        with Store(self.database, writable=True) as store:
            Application(store).scan()
        run(self.database, once=True)
        with Store(self.database) as store:
            self.assertEqual(store.rows("SELECT * FROM api_reservations"), [])
            self.assertEqual(
                store.rows("SELECT state FROM processing_jobs")[0]["state"], "changed"
            )
        self.assertEqual(
            self.client.get(response["status_url"], headers=self.headers[0]).json()[
                "state"
            ],
            "stale",
        )

    def test_cancelled_request_cannot_retry_stale_source(self):
        from catabolic.app import Application

        response = self.client.post(
            "/v1/rendition-requests", headers=self.headers[0], json=self.body
        ).json()
        self.client.post(response["status_url"] + "/cancel", headers=self.headers[0])
        source = self.root / "source" / "sample.mkv"
        source.write_bytes(source.read_bytes() + b"changed")
        with Store(self.database, writable=True) as store:
            Application(store).scan()
        retried = self.client.post(
            response["status_url"] + "/retry", headers=self.headers[0]
        )
        self.assertEqual(retried.status_code, 409)
        self.assertEqual(retried.json()["code"], "revision_mismatch")

    def test_foreign_profile_rendition_cannot_be_read_by_its_id(self):
        from catabolic.app import Application
        from catabolic.http_worker_local import run

        self.client.post(
            "/v1/rendition-requests", headers=self.headers[0], json=self.body
        )
        run(self.database, once=True)
        with Store(self.database, writable=True) as store:
            Application(store).add_profile("foreign")
            with store.transaction() as db:
                db.execute(
                    "INSERT INTO media_outputs SELECT 'foreign-output','foreign',file_id,source_file_id,source_item_id,item_id,definition_id,NULL,'registered',metadata,created_at FROM media_outputs WHERE profile='default'"
                )
        response = self.client.get(
            "/v1/renditions/foreign-output", headers=self.headers[0]
        )
        self.assertEqual(response.status_code, 404, response.text)
        listing = self.client.get("/v1/renditions", headers=self.headers[0])
        self.assertNotIn("foreign-output", listing.text)
