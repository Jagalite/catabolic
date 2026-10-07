# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import json
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Event, Thread
from unittest.mock import patch

from catabolic.access import grant_put, issue, principal_put, revoke
from catabolic.http_worker_local import run
from catabolic.notifications import drain, emit
from catabolic.store import Store
from tests import test_http_backend as basic
from tests import test_http_renditions as renditions


@unittest.skipUnless(basic.HTTP_AVAILABLE, "optional HTTP dependencies")
class ManagementTest(unittest.TestCase):
    setUp = basic.HTTPTest.setUp

    def test_operator_management_secret_redaction_and_revocation(self):
        body = {
            "url": "https://example.test/hook?secret=PRIVATE",
            "events": ["job_completed"],
        }
        headers = {"Authorization": "Bearer " + self.operator}
        self.assertEqual(
            self.client.post(
                "/v1/webhooks", headers=self.headers, json=body
            ).status_code,
            403,
        )
        response = self.client.post("/v1/webhooks", headers=headers, json=body)
        self.assertEqual(response.status_code, 201, response.text)
        for table in ("api_webhooks", "api_requests"):
            private = self.client.post(
                "/v1/query/sql",
                headers=headers,
                json={"query": f"SELECT * FROM {table}"},
            )
            self.assertNotEqual(private.status_code, 200)
            self.assertNotIn("PRIVATE", private.text)
        identifier = response.json()["id"]
        path = "/v1/webhooks/" + identifier
        self.assertNotIn("PRIVATE", response.text)
        self.assertEqual(self.client.get(path, headers=self.headers).status_code, 403)
        self.assertEqual(
            len(self.client.get("/v1/webhooks", headers=headers).json()["webhooks"]), 1
        )
        with Store(self.database, writable=True) as store, store.transaction() as db:
            emit(db, "default", "job_completed", "info", "job-1", "job-1")

        def deliver(*args, **kwargs):
            with Store(self.database, writable=True):
                pass
            self.assertEqual(kwargs["url"], body["url"])
            self.assertEqual(kwargs["envelope"]["job_id"], "job-1")
            return "temporarily_failed"

        with patch("catabolic.notifications.send", deliver):
            drain(self.database)
        self.assertEqual(
            self.client.get(path + "/deliveries", headers=headers).json()["deliveries"][
                0
            ]["state"],
            "retry",
        )
        self.assertEqual(
            self.client.post(path + "/retry", headers=headers).status_code, 200
        )
        with Store(self.database, writable=True) as store:
            revoke(store, self.operator.split(".")[0])
        with patch("catabolic.notifications.send") as sender:
            report = drain(self.database)
        sender.assert_not_called()
        self.assertEqual(report["deliveries"][0]["state"], "repair")

    def test_disable_read_only_and_principal_isolation(self):
        from fastapi.testclient import TestClient

        from catabolic.http.app import create_app

        headers = {"Authorization": "Bearer " + self.operator}
        body = {"url": "http://localhost:9000/hook", "events": ["scan_failed"]}
        created = self.client.post("/v1/webhooks", headers=headers, json=body).json()
        path = "/v1/webhooks/" + created["id"]
        with Store(self.database, writable=True) as store:
            principal_put(store, "default", "second-operator")
            grant = grant_put(
                store,
                "second-operator",
                {"actions": ["webhooks:manage"], "operator": True},
            )
            other = {
                "Authorization": "Bearer "
                + issue(store, "second-operator", [grant["grant_id"]])["token"]
            }
            with store.transaction() as db:
                emit(db, "default", "scan_failed", "error", "scan", "scan")
        self.assertEqual(self.client.get(path, headers=other).status_code, 404)
        self.assertEqual(
            self.client.get("/v1/webhooks", headers=other).json()["webhooks"], []
        )
        with TestClient(create_app(self.database)) as readonly:
            self.assertEqual(
                readonly.post(path + "/disable", headers=headers).status_code, 403
            )
        self.assertFalse(
            self.client.post(path + "/disable", headers=headers).json()["enabled"]
        )
        with patch("catabolic.notifications.send") as sender:
            self.assertTrue(drain(self.database)["complete"])
        sender.assert_not_called()
        for url in (
            "file:///tmp/hook",
            "http://user:secret@localhost/hook",
            "https://localhost/#fragment",
        ):
            self.assertEqual(
                self.client.post(
                    "/v1/webhooks", headers=headers, json={**body, "url": url}
                ).status_code,
                400,
            )

    def test_pagination_and_retry_cannot_steal_live_delivery(self):
        headers = {"Authorization": "Bearer " + self.operator}
        body = {"url": "https://example.test/hook", "events": ["job_completed"]}
        ids = {
            self.client.post("/v1/webhooks", headers=headers, json=body).json()["id"]
            for _ in range(3)
        }
        page = self.client.get("/v1/webhooks?limit=2", headers=headers).json()
        second = self.client.get(
            "/v1/webhooks",
            headers=headers,
            params={"limit": 2, "after": page["next_cursor"]},
        ).json()
        self.assertEqual({w["id"] for w in page["webhooks"] + second["webhooks"]}, ids)
        self.assertIsNone(second["next_cursor"])
        identifier = next(iter(ids))
        path = "/v1/webhooks/" + identifier
        with Store(self.database, writable=True) as store, store.transaction() as db:
            for i in range(3):
                emit(db, "default", "job_completed", "info", str(i), str(i))
            db.execute(
                "UPDATE notification_deliveries SET state='leased',lease_token='owned',lease_until=9999999999 WHERE destination_id=?",
                (identifier,),
            )
        first = self.client.get(path + "/deliveries?limit=2", headers=headers).json()
        last = self.client.get(
            path + "/deliveries",
            headers=headers,
            params={"limit": 2, "after": first["next_cursor"]},
        ).json()
        self.assertEqual(len(first["deliveries"] + last["deliveries"]), 3)
        self.assertEqual(
            self.client.post(path + "/retry", headers=headers).status_code, 409
        )
        with Store(self.database) as store:
            self.assertTrue(
                all(
                    r["lease_token"] == "owned"
                    for r in store.rows(
                        "SELECT lease_token FROM notification_deliveries WHERE destination_id=?",
                        (identifier,),
                    )
                )
            )
        with Store(self.database, writable=True) as store, store.transaction() as db:
            db.execute(
                "UPDATE notification_deliveries SET lease_until=0 WHERE destination_id=?",
                (identifier,),
            )
        self.assertEqual(
            self.client.post(path + "/retry", headers=headers).status_code, 200
        )


@unittest.skipIf(
    getattr(renditions.RenditionHTTPTest, "__unittest_skip__", False),
    "HTTP and FFmpeg required",
)
class CallbackTest(unittest.TestCase):
    setUp = renditions.RenditionHTTPTest.setUp

    def receiver(self, *, gate=None, entered=None):
        received = []

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                received.append(
                    json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                )
                if entered is not None:
                    entered.set()
                if gate is not None:
                    gate.wait(5)
                self.send_response(204)
                self.end_headers()

            def log_message(self, *args):
                pass

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        return f"http://127.0.0.1:{server.server_port}", received

    def allow(self, origin):
        with Store(self.database, writable=True) as store:
            for row in store.rows("SELECT * FROM api_grants"):
                definition = json.loads(row["definition"])
                if "processing:request" in definition["actions"]:
                    definition["callback_origins"] = [origin]
                    grant_put(store, row["principal"], definition, row["id"])

    def test_real_worker_callback_cached_result_and_idempotency(self):
        origin, received = self.receiver()
        body = {**self.body, "callback_url": origin + "/hook?secret=PRIVATE"}
        self.assertEqual(
            self.client.post(
                "/v1/rendition-requests", headers=self.headers[0], json=body
            ).status_code,
            403,
        )
        self.allow(origin)
        response = self.client.post(
            "/v1/rendition-requests", headers=self.headers[0], json=body
        )
        self.assertEqual(response.status_code, 202, response.text)
        request_id = response.json()["request_id"]
        repeated = self.client.post(
            "/v1/rendition-requests", headers=self.headers[0], json=body
        )
        self.assertEqual(repeated.json()["request_id"], request_id)
        conflict = self.client.post(
            "/v1/rendition-requests",
            headers=self.headers[0],
            json={**body, "callback_url": origin + "/different"},
        )
        self.assertEqual(conflict.status_code, 409)
        run(self.database, once=True)
        self.assertEqual(len(received), 1)
        from catabolic.http.models import RequestCallbackEvent

        RequestCallbackEvent.model_validate(received[0])
        self.assertEqual(received[0]["event"], "request_ready")
        self.assertEqual(received[0]["request_id"], request_id)
        self.assertNotIn("PRIVATE", json.dumps(received))
        self.assertNotIn("job_id", received[0])
        # A cached completed job never transitions again, but the new caller
        # still gets its own completion callback exactly once on admission.
        with Store(self.database, writable=True) as store, store.transaction() as db:
            db.execute(
                "INSERT INTO api_worker_status VALUES ('default',1,?,0)",
                (json.dumps(renditions.rendering.capabilities()),),
            )
        cached = self.client.post(
            "/v1/rendition-requests", headers=self.headers[1], json=body
        )
        self.assertEqual(cached.status_code, 200, cached.text)
        drain(self.database)
        self.assertEqual(len(received), 2)
        self.assertEqual(received[1]["request_id"], cached.json()["request_id"])
        self.assertNotEqual(received[0]["id"], received[1]["id"])

    def test_shared_job_cancellation_and_revoked_origin(self):
        origin, received = self.receiver()
        self.allow(origin)
        body = {**self.body, "callback_url": origin + "/hook"}
        requests = [
            self.client.post("/v1/rendition-requests", headers=h, json=body).json()
            for h in self.headers
        ]
        self.client.post(requests[0]["status_url"] + "/cancel", headers=self.headers[0])
        with Store(self.database, writable=True) as store, store.transaction() as db:
            db.execute("UPDATE processing_jobs SET state='failed'")
        drain(self.database)
        self.assertEqual(
            {(r["request_id"], r["event"]) for r in received},
            {
                (requests[0]["request_id"], "request_cancelled"),
                (requests[1]["request_id"], "request_failed"),
            },
        )
        with Store(self.database, writable=True) as store, store.transaction() as db:
            db.execute(
                "UPDATE api_requests SET state='stale' WHERE id=?",
                (requests[1]["request_id"],),
            )
        self.allow("https://no-longer-allowed.example")
        with patch("catabolic.notifications.send") as sender:
            report = drain(self.database)
        sender.assert_not_called()
        self.assertEqual(report["deliveries"][0]["state"], "repair")

    def test_scoped_callback_recovers_with_new_token_without_rerender(self):
        origin, received = self.receiver()
        self.allow(origin)
        response = self.client.post(
            "/v1/rendition-requests",
            headers=self.headers[0],
            json={**self.body, "callback_url": origin + "/hook"},
        )
        path = response.json()["status_url"] + "/callback"
        self.client.post(
            response.json()["status_url"] + "/cancel", headers=self.headers[0]
        )
        old_token = self.headers[0]["Authorization"].removeprefix("Bearer ")
        with Store(self.database, writable=True) as store:
            grants = json.loads(
                store.rows(
                    "SELECT grants FROM api_tokens WHERE id=?",
                    (old_token.split(".")[0],),
                )[0]["grants"]
            )
            revoke(store, old_token.split(".")[0])
            new_token = issue(store, "one", grants)["token"]
        with patch("catabolic.notifications.send") as sender:
            self.assertEqual(drain(self.database)["deliveries"][0]["state"], "repair")
        sender.assert_not_called()
        headers = {"Authorization": "Bearer " + new_token}
        self.assertEqual(
            self.client.get(path, headers=self.headers[1]).status_code, 404
        )
        self.assertEqual(
            self.client.post(path + "/retry", headers=self.headers[1]).status_code, 404
        )
        self.assertIn(
            "request_cancelled", self.client.get(path, headers=headers).json()["events"]
        )
        before = self.client.get(path + "/deliveries", headers=headers).json()[
            "deliveries"
        ]
        self.assertEqual(
            self.client.post(path + "/retry", headers=headers).status_code, 200
        )
        self.assertTrue(drain(self.database)["complete"])
        self.assertEqual(len(received), 1)
        self.assertEqual(received[0]["id"], before[0]["event_id"])
        with Store(self.database) as store:
            self.assertEqual(
                store.rows("SELECT attempts FROM processing_jobs")[0]["attempts"], 0
            )
        self.assertFalse(
            self.client.post(path + "/disable", headers=headers).json()["enabled"]
        )
        self.assertEqual(
            self.client.post(path + "/retry", headers=headers).status_code, 409
        )

    def test_slow_receiver_does_not_block_worker_supervision(self):
        gate, entered, supervised, stop = Event(), Event(), Event(), Event()
        origin, received = self.receiver(gate=gate, entered=entered)
        self.allow(origin)
        response = self.client.post(
            "/v1/rendition-requests",
            headers=self.headers[0],
            json={**self.body, "callback_url": origin + "/hook"},
        )
        self.client.post(
            response.json()["status_url"] + "/cancel", headers=self.headers[0]
        )
        errors = []

        class StopWorker(Exception):
            pass

        def tick(*args):
            if stop.is_set():
                raise StopWorker()
            if entered.is_set():
                supervised.set()
            return {"processed": 0}

        def worker():
            try:
                run(self.database, interval=0.01)
            except StopWorker:
                pass
            except Exception as exc:
                errors.append(exc)

        with (
            patch("catabolic.http_worker_local.tick", tick),
            patch(
                "catabolic.http_worker_local.rendering.capabilities",
                return_value={"presets": []},
            ),
        ):
            thread = Thread(target=worker, daemon=True)
            thread.start()
            try:
                self.assertTrue(entered.wait(3), errors)
                self.assertTrue(
                    supervised.wait(1),
                    "worker stopped supervising while the receiver held its response",
                )
            finally:
                gate.set()
                stop.set()
                thread.join(5)
        self.assertFalse(thread.is_alive())
        self.assertEqual(errors, [])
        self.assertEqual(len(received), 1)
        with Store(self.database) as store:
            self.assertEqual(
                store.rows("SELECT state FROM notification_deliveries")[0]["state"],
                "complete",
            )
