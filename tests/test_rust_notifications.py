# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native durable notifications against Python and controlled loopback receivers.

No live credentials or external services are used. Fixtures and mutations are
independent per engine; network delivery is always to an ephemeral local server.
"""

import hashlib
import json
import os
import sqlite3
import subprocess
import tempfile
import threading
import unittest
from collections import Counter
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

from catabolic.access import authenticate, grant_put, issue, principal_put
from catabolic.app import Application
from catabolic.notifications import configure, drain, emit
from catabolic.store import Store, encode
from catabolic.webhooks import register
from tests.test_rust_migration import BINARY


class NativeNotificationsTest(unittest.TestCase):
    def test_run_alias_matches_drain(self):
        self.assertEqual(self.native("run"), self.native("drain"))

    def setUp(self):
        self.assertTrue(BINARY.is_file(), "build catabolic-cli before qualification")
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.path = self.root / "native.db"
        self.reference = self.root / "python.db"
        Store.initialize(self.path)
        Store.initialize(self.reference)

    def native(
        self,
        operation,
        request=None,
        *,
        ok=True,
        env=None,
        profile="default",
        surface="notify",
    ):
        run = subprocess.run(
            [
                str(BINARY),
                "--db",
                str(self.path),
                "--profile",
                profile,
                *(["operation", surface] if surface != "notify" else ["notify"]),
                operation,
                "--options",
                json.dumps(request or {}),
            ],
            capture_output=True,
            text=True,
            timeout=60,
            env={**os.environ, **(env or {})},
        )
        self.assertEqual(run.returncode, 0 if ok else 2, run.stderr or run.stdout)
        return json.loads(run.stdout) if ok else run.stderr

    @staticmethod
    def policy(identifier="one", severity="info", tags=None, **extra):
        return {
            "id": identifier,
            "credential_env": "CATABOLIC_TEST_" + identifier,
            "subscriptions": ["projection_updated", "scan_failed"],
            "tags": ["home"] if tags is None else tags,
            "min_severity": severity,
            "apply": True,
            **extra,
        }

    def put(self, identifier="one", **extra):
        return self.native("put", self.policy(identifier, **extra))

    def python_put(self, request):
        with Store(self.reference, writable=True) as store:
            return configure(
                Application(store),
                request["id"],
                request["credential_env"],
                request["subscriptions"],
                request["tags"],
                request["min_severity"],
                apply=request["apply"],
            )

    def emit(
        self, identity="generation-1", event="projection_updated", severity="info"
    ):
        return self.native(
            "emit",
            {
                "event": event,
                "severity": severity,
                "subject": "private-media-title",
                "identity": identity,
            },
        )

    @staticmethod
    def rows(path, table):
        with sqlite3.connect(path) as db:
            db.row_factory = sqlite3.Row
            return [
                dict(row) for row in db.execute(f"SELECT * FROM {table} ORDER BY 1,2")
            ]

    def receiver(self, response):
        received = []

        class Handler(BaseHTTPRequestHandler):
            def do_POST(handler):
                body = json.loads(
                    handler.rfile.read(int(handler.headers["Content-Length"]))
                )
                received.append((handler.path, dict(handler.headers), body))
                status = response(handler.path)
                handler.send_response(status)
                handler.send_header("Location", "/must-not-follow")
                handler.end_headers()

            def log_message(self, *args):
                pass

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        return f"http://127.0.0.1:{server.server_port}", received

    def test_policy_dry_run_idempotency_and_python_encoding(self):
        request = self.policy(tags=["café", "home"], apply=False)
        self.assertEqual(self.native("put", request), self.python_put(request))
        self.assertEqual(self.rows(self.path, "notification_destinations"), [])
        request["apply"] = True
        for _ in range(2):
            self.assertEqual(self.native("put", request), self.python_put(request))
        self.assertEqual(
            self.rows(self.path, "notification_destinations"),
            self.rows(self.reference, "notification_destinations"),
        )
        self.assertEqual(
            self.rows(self.path, "notification_destinations")[0]["revision"], 1
        )

    def test_event_identity_routing_and_deduplication_match_python(self):
        for request in [
            self.policy("one"),
            self.policy("errors", severity="error", tags=["ops"]),
        ]:
            self.native("put", request)
            self.python_put(request)
        for identity, event, severity in [
            (["café😀", {"z": 2, "a": 1.0}], "projection_updated", "info"),
            (["café😀", {"z": 2, "a": 1.0}], "projection_updated", "info"),
            ("generation-2", "scan_failed", "error"),
        ]:
            result = self.emit(identity, event, severity)
            self.assertEqual(
                result["id"],
                hashlib.sha256(
                    encode(["default", event, identity]).encode()
                ).hexdigest(),
            )
            with (
                Store(self.reference, writable=True) as store,
                store.transaction() as db,
            ):
                emit(db, "default", event, severity, "private-media-title", identity)
        self.assertEqual(
            self.rows(self.path, "notification_deliveries"),
            self.rows(self.reference, "notification_deliveries"),
        )
        self.assertEqual(len(self.rows(self.path, "consumer_events")), 2)

    def test_loopback_partial_success_retry_envelopes_and_no_proxy(self):
        failure = [True]
        base, received = self.receiver(
            lambda path: 503 if path == "/bad" and failure[0] else 204
        )
        for request in [
            self.policy("good"),
            self.policy("bad"),
            self.policy("errors", severity="error", tags=["ops"]),
        ]:
            self.native("put", request)
        self.emit()
        environment = {
            "CATABOLIC_TEST_good": base + "/good",
            "CATABOLIC_TEST_bad": base + "/bad",
            "CATABOLIC_TEST_errors": base + "/must-not-send",
            "HTTP_PROXY": "http://127.0.0.1:1",
            "http_proxy": "http://127.0.0.1:1",
            "ALL_PROXY": "http://127.0.0.1:1",
            "all_proxy": "http://127.0.0.1:1",
            "NO_PROXY": "",
            "no_proxy": "",
        }
        result = self.native("drain", {"tag": "home"}, env=environment)
        self.assertFalse(result["complete"])
        self.assertEqual(result["unresolved"], 1)
        self.assertEqual(
            {r["state"] for r in result["deliveries"]}, {"retry", "complete"}
        )
        self.assertEqual(self.native("drain", env=environment)["deliveries"], [])
        with sqlite3.connect(self.path) as db:
            db.execute("UPDATE notification_deliveries SET due_at=0")
        failure[0] = False
        self.assertTrue(self.native("drain", env=environment)["complete"])
        self.assertEqual(Counter(row[0] for row in received), {"/bad": 2, "/good": 1})
        bad = [row for row in received if row[0] == "/bad"]
        self.assertEqual(bad[0][2], bad[1][2])
        self.assertEqual(bad[0][1]["idempotency-key"], bad[1][1]["idempotency-key"])
        self.assertNotIn("private-media-title", json.dumps(received))
        self.assertNotIn(
            base, json.dumps(self.rows(self.path, "notification_destinations"))
        )

    def test_loopback_envelope_matches_python(self):
        base, received = self.receiver(lambda _: 204)
        request = self.policy()
        self.native("put", request)
        self.python_put(request)
        self.emit()
        with Store(self.reference, writable=True) as store, store.transaction() as db:
            emit(
                db,
                "default",
                "projection_updated",
                "info",
                "private-media-title",
                "generation-1",
            )
        for path in [self.path, self.reference]:
            with sqlite3.connect(path) as db:
                db.execute(
                    "UPDATE consumer_events SET created_at='2026-01-02 03:04:05'"
                )
        env = {"CATABOLIC_TEST_one": base + "/hook"}
        actual = self.native("drain", env=env)
        with patch.dict(os.environ, env):
            expected = drain(self.reference)
        self.assertEqual(actual, expected)
        self.assertEqual(received[0][2], received[1][2])
        headers = [{k.lower(): v for k, v in row[1].items()} for row in received]
        self.assertEqual(headers[0]["idempotency-key"], headers[1]["idempotency-key"])

    def test_redirects_are_not_followed(self):
        base, received = self.receiver(lambda _: 302)
        self.put()
        self.emit()
        result = self.native("drain", env={"CATABOLIC_TEST_one": base + "/original"})
        self.assertEqual(result["deliveries"][0]["state"], "retry")
        self.assertEqual([r[0] for r in received], ["/original"])

    def test_bad_native_provider_urls_require_repair(self):
        self.put()
        self.emit()
        for value, outcome in [
            ("", "credential_unavailable"),
            ("ntfy://127.0.0.1/fake-test-token?verify=no", "invalid_destination"),
            ("json://evil/remote-config", "invalid_destination"),
            ("http://user:secret@localhost/hook", "invalid_destination"),
            ("https://localhost/hook#secret", "invalid_destination"),
            ("http:///hook", "invalid_destination"),
            ("http://localhost:0/hook", "invalid_destination"),
            ("http://localhost/\nhook", "invalid_destination"),
            ("http://[invalid/hook", "invalid_destination"),
        ]:
            with self.subTest(value=value):
                self.native("retry", {"id": "one"})
                result = self.native(
                    "drain",
                    env={
                        "CATABOLIC_TEST_one": value,
                    },
                )
                self.assertFalse(result["complete"])
                self.assertEqual(result["deliveries"][0]["state"], "repair")
                self.assertEqual(
                    self.rows(self.path, "notification_deliveries")[0]["error"], outcome
                )
                self.assertNotIn("secret", json.dumps(result))
                self.assertEqual(self.native("drain")["deliveries"], [])
        self.assertEqual(len(self.rows(self.path, "consumer_events")), 1)

    def test_inflight_policy_edit_cancels_and_fences_ack(self):
        self.put()
        self.emit()

        def respond(_):
            # This is the actual HTTP effect boundary, not a simulated reducer.
            # A new native writer must acquire the released flock successfully.
            self.put(tags=["changed"])
            return 204

        base, received = self.receiver(respond)
        result = self.native("drain", env={"CATABOLIC_TEST_one": base + "/hook"})
        self.assertEqual(len(received), 1)
        self.assertEqual(result["deliveries"][0]["state"], "stale_acknowledgement")
        self.assertEqual(
            self.rows(self.path, "notification_deliveries")[0]["state"], "cancelled"
        )
        self.assertTrue(result["complete"])

    def test_retry_preserves_live_lease_and_disable_cancels(self):
        self.put()
        self.emit()
        with sqlite3.connect(self.path) as db:
            db.execute(
                "UPDATE notification_deliveries SET state='leased',lease_token='owned',lease_until=9999999999"
            )
        self.assertIn(
            "delivery_in_progress", self.native("retry", {"id": "one"}, ok=False)
        )
        self.assertEqual(
            self.rows(self.path, "notification_deliveries")[0]["lease_token"], "owned"
        )
        self.native("disable", {"id": "one"})
        row = self.rows(self.path, "notification_deliveries")[0]
        self.assertEqual(
            (row["state"], row["lease_token"], row["lease_until"]),
            ("cancelled", None, None),
        )
        self.assertTrue(self.native("drain")["complete"])
        self.assertIn("webhook_disabled", self.native("retry", {"id": "one"}, ok=False))

    def test_process_exit_recovers_expired_lease(self):
        entered, release = threading.Event(), threading.Event()

        def respond(_):
            entered.set()
            release.wait(10)
            return 204

        base, received = self.receiver(respond)
        self.put()
        self.emit()
        environment = {**os.environ, "CATABOLIC_TEST_one": base + "/hook"}
        process = subprocess.Popen(
            [str(BINARY), "--db", str(self.path), "notify", "drain", "--options", "{}"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=environment,
        )
        try:
            self.assertTrue(entered.wait(10), "native worker never reached receiver")
            process.kill()
            process.communicate(timeout=10)
            row = self.rows(self.path, "notification_deliveries")[0]
            self.assertEqual(row["state"], "leased")
            self.assertEqual(row["attempts"], 1)
            self.assertEqual(self.native("drain", env=environment)["deliveries"], [])
            with sqlite3.connect(self.path) as db:
                db.execute("UPDATE notification_deliveries SET lease_until=0")
            release.set()
            result = self.native("drain", env=environment)
            self.assertTrue(result["complete"])
            self.assertEqual(
                self.rows(self.path, "notification_deliveries")[0]["attempts"], 2
            )
            self.assertEqual(received[0][2], received[1][2])
        finally:
            release.set()
            if process.poll() is None:
                process.kill()
                process.communicate(timeout=10)

    def test_invalid_configuration_fails_before_mutation(self):
        for extra in [
            {"credential_env": "https://secret.example/hook"},
            {"subscriptions": []},
            {"subscriptions": ["request_ready"]},
            {"tags": ["bad\ntag"]},
            {"tags": ["tag"] * 21},
            {"min_severity": "critical"},
            {"min_severity": 9},
        ]:
            self.native("put", self.policy(**extra), ok=False)
            self.assertEqual(self.rows(self.path, "notification_destinations"), [])
        for limit in [0, 101, -1, "many"]:
            self.native("drain", {"limit": limit}, ok=False)

    def test_profile_isolation_status_paging_and_unverified_publications(self):
        with sqlite3.connect(self.path) as db:
            db.execute("INSERT INTO profiles(id) VALUES ('other')")
            db.execute(
                "INSERT INTO publication_generations(profile,catalog,local_binding,generation,verified) VALUES ('default','global','{}',1,0)"
            )
        self.put("one")
        self.put("two")
        self.emit()
        status = self.native("status", {"limit": 1})
        self.assertTrue(status["destinations_truncated"])
        self.assertTrue(status["deliveries_truncated"])
        self.assertEqual(status["unverified_publications"], 1)
        self.assertFalse(status["complete"])
        isolated = self.native("status", profile="other")
        self.assertEqual(isolated["destinations"], [])
        self.assertEqual(isolated["deliveries"], [])
        self.assertTrue(isolated["complete"])

    def test_http_owned_destination_never_falls_back_to_environment(self):
        base, received = self.receiver(lambda _: 204)
        self.put()
        self.emit()
        with sqlite3.connect(self.path) as db:
            db.execute("INSERT INTO api_principals(id,profile) VALUES ('p','default')")
            db.execute(
                "INSERT INTO api_tokens(id,principal,digest,grants,expires) VALUES ('t','p','fake','[]',9999999999)"
            )
            db.execute(
                "INSERT INTO api_webhooks(profile,destination_id,principal,token_id,url) VALUES ('default','one','p','t',?)",
                (base + "/managed",),
            )
        result = self.native("drain", env={"CATABOLIC_TEST_one": base + "/fallback"})
        self.assertEqual(result["deliveries"][0]["state"], "repair")
        self.assertEqual(
            self.rows(self.path, "notification_deliveries")[0]["error"],
            "authorization_unavailable",
        )
        self.assertEqual(received, [])

    def test_managed_webhook_revalidates_token_principal_and_current_grants(self):
        base, received = self.receiver(lambda _: 204)
        with Store(self.path, writable=True) as store:
            principal_put(store, "default", "operator")
            definition = {"actions": ["webhooks:manage"], "operator": True}
            grant = grant_put(store, "operator", definition)["grant_id"]
            secret = issue(store, "operator", [grant])["token"]
            webhook = register(
                authenticate(store, secret),
                base + "/managed?fake-secret=PRIVATE",
                ["projection_updated"],
            )["id"]
        self.emit("authorized")
        self.assertTrue(self.native("drain")["complete"])
        self.assertEqual(len(received), 1)
        token = secret.split(".")[0]
        for mutation, parameters, restore in [
            (
                "UPDATE api_tokens SET revoked=1 WHERE id=?",
                (token,),
                "UPDATE api_tokens SET revoked=0",
            ),
            (
                "UPDATE api_tokens SET expires=0 WHERE id=?",
                (token,),
                "UPDATE api_tokens SET expires=9999999999",
            ),
            (
                "UPDATE api_principals SET enabled=0 WHERE id=?",
                ("operator",),
                "UPDATE api_principals SET enabled=1",
            ),
            (
                "UPDATE api_grants SET enabled=0 WHERE id=?",
                (grant,),
                "UPDATE api_grants SET enabled=1",
            ),
            ("UPDATE api_grants SET definition='{}' WHERE id=?", (grant,), None),
        ]:
            with self.subTest(mutation=mutation):
                self.emit(mutation)
                with sqlite3.connect(self.path) as db:
                    db.execute(mutation, parameters)
                result = self.native("drain")
                self.assertEqual(result["deliveries"][0]["state"], "repair")
                self.assertNotIn("PRIVATE", json.dumps(result))
                before = len(received)
                with Store(self.path, writable=True) as store:
                    if restore:
                        with store.transaction() as db:
                            db.execute(restore)
                    else:
                        grant_put(store, "operator", definition, grant)
                self.native("retry", {"id": webhook})
                self.assertTrue(self.native("drain")["complete"])
                self.assertEqual(len(received), before + 1)

    def test_callback_is_principal_bound_and_requires_current_approved_origin(self):
        base, received = self.receiver(lambda _: 204)
        body = {
            "item_id": "item",
            "operation_id": "operation",
            "source_file_id": "file",
            "source_revision": "revision",
        }
        definition = {
            "actions": ["processing:request"],
            "item_ids": ["item"],
            "operation_ids": ["operation"],
            "callback_origins": [base],
        }
        with Store(self.path, writable=True) as store:
            principal_put(store, "default", "requester")
            grant = grant_put(store, "requester", definition)["grant_id"]
            secret = issue(store, "requester", [grant])["token"]
            with store.transaction() as db:
                db.execute("INSERT INTO locations(id) VALUES ('test')")
                db.execute(
                    "INSERT INTO files(id,location,path) VALUES ('file','test','private-title')"
                )
                db.execute(
                    "INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key) VALUES ('job','default','file','probe','test','{}','{}','cache')"
                )
                db.execute(
                    "INSERT INTO api_requests(id,profile,principal,token_id,idempotency_key,body,job_id,state,reserved_bytes,reservation_device) VALUES ('request','default','requester',?,'once',?,'job','cancelled',0,0)",
                    (secret.split(".")[0], encode(body)),
                )
            webhook = register(
                authenticate(store, secret),
                base + "/callback",
                [],
                request_id="request",
            )["id"]
            grant_put(
                store,
                "requester",
                {**definition, "callback_origins": ["https://127.0.0.1"]},
                grant,
            )
        result = self.native("drain")
        self.assertEqual(result["deliveries"][0]["state"], "repair")
        self.assertEqual(received, [])
        with Store(self.path, writable=True) as store:
            grant_put(
                store,
                "requester",
                {**definition, "revisions": {"file": "wrong"}},
                grant,
            )
        self.native("retry", {"id": webhook})
        self.assertEqual(self.native("drain")["deliveries"][0]["state"], "repair")
        self.assertEqual(received, [])
        with Store(self.path, writable=True) as store:
            grant_put(store, "requester", definition, grant)
        self.native("retry", {"id": webhook})
        self.assertTrue(self.native("drain")["complete"])
        self.assertEqual(received[0][2]["request_id"], "request")
        self.assertEqual(received[0][2]["state"], "cancelled")
        self.assertNotIn("private-title", json.dumps(received))

    def test_native_provider_partial_success_and_redaction(self):
        base, received = self.receiver(
            lambda path: 503 if path == "/bad-private/message" else 200
        )
        self.put("good")
        self.put("bad")
        self.emit()
        native = base.replace("http://", "gotify://")
        env = {
            "CATABOLIC_APPRISE_PYTHON": "/does-not-exist/native-providers-ignore-python",
            "CATABOLIC_TEST_good": native + "/good-private/synthetic-token",
            "CATABOLIC_TEST_bad": native + "/bad-private/synthetic-token",
        }
        result = self.native("drain", env=env)
        self.assertEqual(
            {r["state"] for r in result["deliveries"]}, {"complete", "retry"}
        )
        self.assertNotIn("private", json.dumps(result))
        self.assertEqual(len(received), 2)
        self.assertTrue(all(body["title"] == "Catabolic" for _, _, body in received))
        self.assertTrue(
            all(
                body["message"] == "A Catabolic projection update was locally verified."
                for _, _, body in received
            )
        )
        self.assertNotIn("private-media-title", json.dumps(received))
        self.native("retry", {"id": "bad"})
        env["CATABOLIC_TEST_bad"] = native + "/good-private/synthetic-token"
        self.assertTrue(self.native("drain", env=env)["complete"])
        self.assertEqual(len(received), 3)

    def test_native_webhook_crud_bearer_auth_and_principal_isolation(self):
        base, received = self.receiver(lambda _: 204)
        with Store(self.path, writable=True) as store:
            secrets = []
            for principal in ["operator", "other"]:
                principal_put(store, "default", principal)
                grant = grant_put(
                    store, principal, {"actions": ["webhooks:manage"], "operator": True}
                )["grant_id"]
                secrets.append(issue(store, principal, [grant])["token"])
        env = {"CATABOLIC_API_TOKEN": secrets[0]}

        def call(operation, request=None, **kwargs):
            return self.native(
                operation, request, surface="webhooks", env=env, **kwargs
            )

        identifiers = set()
        for _ in range(3):
            result = call(
                "register",
                {
                    "url": base + "/hook?secret=PRIVATE",
                    "events": ["projection_updated"],
                },
            )
            self.assertNotIn("PRIVATE", json.dumps(result))
            identifiers.add(result["id"])
        first = call("list", {"limit": 2})
        second = call("list", {"limit": 2, "after": first["next_cursor"]})
        self.assertEqual(
            {row["id"] for row in first["webhooks"] + second["webhooks"]}, identifiers
        )
        self.assertIsNone(second["next_cursor"])
        identifier = next(iter(identifiers))
        self.assertIn(
            "not_found",
            call("inspect", {"id": identifier, "request_scope": True}, ok=False),
        )
        env["CATABOLIC_API_TOKEN"] = secrets[1]
        self.assertIn("not_found", call("inspect", {"id": identifier}, ok=False))
        self.assertEqual(call("list")["webhooks"], [])
        env["CATABOLIC_API_TOKEN"] = "invalid.test"
        self.assertIn("unauthorized", call("list", ok=False))
        env["CATABOLIC_API_TOKEN"] = secrets[0]
        self.emit()
        for identifier in identifiers:
            self.assertEqual(
                len(call("deliveries", {"id": identifier})["deliveries"]), 1
            )
            self.assertFalse(call("disable", {"id": identifier})["enabled"])
            self.assertIn(
                "webhook_disabled", call("retry", {"id": identifier}, ok=False)
            )
        self.assertTrue(self.native("drain")["complete"])
        self.assertEqual(received, [])


if __name__ == "__main__":
    unittest.main()
