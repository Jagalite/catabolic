# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from catabolic.app import Application
from catabolic.notification_worker import notify
from catabolic.notifications import configure, drain, emit, send
from catabolic.store import Store


class NotificationsTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.database = Path(tmp.name) / "catalog.db"
        Store.initialize(self.database)

    def configure(self, name, severity="info", tags=None):
        with Store(self.database, writable=True) as store:
            configure(
                Application(store),
                name,
                "PRIVATE_" + name,
                ["projection_updated", "scan_failed"],
                tags or ["home"],
                severity,
                apply=True,
            )

    def event(self, event="projection_updated", severity="info"):
        with Store(self.database, writable=True) as store, store.transaction() as db:
            emit(db, "default", event, severity, "private-media-name", "generation-1")

    def test_routing_partial_retry_and_secrets_are_references(self):
        self.configure("good")
        self.configure("bad")
        self.configure("errors", "error", ["ops"])
        self.event()
        calls = []

        def deliver(env, event, severity, **kwargs):
            with Store(self.database, writable=True):
                pass  # Notification I/O also owns neither lock nor transaction.
            calls.append(env)
            return "complete" if env == "PRIVATE_good" else "temporarily_failed"

        with patch("catabolic.notifications.send", deliver):
            result = drain(self.database, tag="home")
            self.assertFalse(result["complete"])
            with (
                Store(self.database, writable=True) as store,
                store.transaction() as db,
            ):
                db.execute("UPDATE notification_deliveries SET due_at=0")
            drain(self.database)
        self.assertEqual(calls.count("PRIVATE_good"), 1)
        self.assertEqual(calls.count("PRIVATE_bad"), 2)
        self.assertNotIn("PRIVATE_errors", calls)
        with Store(self.database) as store:
            self.assertNotIn(
                "ntfy://",
                json.dumps(store.rows("SELECT * FROM notification_destinations")),
            )
            self.assertEqual(len(store.rows("SELECT * FROM consumer_events")), 1)

    def test_disabled_uninstalled_and_no_recursive_failures(self):
        self.event()
        self.assertEqual(drain(self.database)["deliveries"], [])
        self.configure("missing")
        self.event("scan_failed", "error")
        with patch("catabolic.notifications.send", return_value="uninstalled"):
            self.assertEqual(drain(self.database)["deliveries"][0]["state"], "repair")
        self.assertFalse(drain(self.database)["complete"])
        with Store(self.database) as store:
            self.assertEqual(len(store.rows("SELECT * FROM consumer_events")), 2)
        with patch.dict("sys.modules", {"apprise": None}):
            self.assertEqual(
                notify(
                    {
                        "url": "ntfy://localhost/topic",
                        "event": "scan_failed",
                        "severity": "error",
                    }
                ),
                "uninstalled",
            )
        self.assertEqual(
            notify({"url": "json://evil/remote-config", "event": "scan_failed"}),
            "invalid_destination",
        )
        with patch.dict(os.environ, {"PRIVATE_URL": "ntfy://localhost/token-secret"}):
            with patch(
                "catabolic.notifications.command_output",
                side_effect=OSError("token-secret"),
            ):
                self.assertEqual(
                    send("PRIVATE_URL", "scan_failed", "error"), "temporarily_failed"
                )

    def test_idempotent_setup_and_edit_fences_inflight_ack(self):
        self.configure("one")
        self.event()
        self.configure("one")

        def deliver(*args, **kwargs):
            self.configure("one", tags=["new"])
            return "complete"

        with patch("catabolic.notifications.send", deliver):
            drain(self.database)
        with Store(self.database) as store:
            self.assertEqual(
                store.rows("SELECT state FROM notification_deliveries")[0]["state"],
                "cancelled",
            )
        self.assertTrue(drain(self.database)["complete"])

    def test_http_webhook_subprocess_retry_and_redirect(self):
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
        from threading import Thread

        received = []
        status = [503]

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                received.append(
                    (
                        self.headers["Idempotency-Key"],
                        json.loads(
                            self.rfile.read(int(self.headers["Content-Length"]))
                        ),
                    )
                )
                self.send_response(status[0])
                self.send_header("Location", "/must-not-follow")
                self.end_headers()

            def log_message(self, *args):
                pass

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        self.configure("http")
        self.event()
        with patch.dict(
            os.environ, {"PRIVATE_http": f"http://127.0.0.1:{server.server_port}/hook"}
        ):
            self.assertEqual(drain(self.database)["deliveries"][0]["state"], "retry")
            with (
                Store(self.database, writable=True) as store,
                store.transaction() as db,
            ):
                db.execute("UPDATE notification_deliveries SET due_at=0")
            status[0] = 204
            self.assertTrue(drain(self.database)["complete"])
            self.assertEqual(received[0], received[1])
            self.assertNotIn("private-media-name", json.dumps(received))
            status[0] = 302
            self.assertEqual(
                send(
                    "PRIVATE_http",
                    "scan_failed",
                    "error",
                    envelope={"id": "redirect-test"},
                ),
                "temporarily_failed",
            )
            self.assertEqual(len(received), 3)

    def test_job_events_are_atomic_filtered_and_identifiable(self):
        source = self.database.parent / "source"
        source.mkdir()
        (source / "sample.txt").write_text("fixture")
        with Store(self.database, writable=True) as store:
            app = Application(store)
            app.bind("source", "media", str(source))
            app.scan()
            configure(
                app,
                "jobs",
                "JOB_URL",
                ["job_completed", "job_failed"],
                [],
                "info",
                apply=True,
            )
            configure(
                app,
                "errors",
                "ERROR_URL",
                ["job_completed", "job_failed"],
                [],
                "error",
                apply=True,
            )
            file_id = store.rows("SELECT id FROM files")[0]["id"]
            with store.transaction() as db:
                db.execute(
                    "INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key) VALUES ('job','default',?,'probe','media','{}','{}','key')",
                    (file_id,),
                )
                db.execute("UPDATE processing_jobs SET state='complete' WHERE id='job'")
                db.execute("UPDATE processing_jobs SET state='complete' WHERE id='job'")
            self.assertEqual(
                len(store.rows("SELECT * FROM notification_deliveries")), 1
            )
            with self.assertRaises(RuntimeError), store.transaction() as db:
                db.execute("UPDATE processing_jobs SET state='failed' WHERE id='job'")
                raise RuntimeError("rollback")
            self.assertEqual(len(store.rows("SELECT * FROM consumer_events")), 1)
            with store.transaction() as db:
                db.execute("UPDATE processing_jobs SET state='timeout' WHERE id='job'")
            self.assertEqual(
                len(store.rows("SELECT * FROM notification_deliveries")), 3
            )
        with patch("catabolic.notifications.send", return_value="complete") as sender:
            self.assertTrue(drain(self.database)["complete"])
        envelopes = [call.kwargs["envelope"] for call in sender.call_args_list]
        self.assertTrue(all(e["job_id"] == "job" for e in envelopes))
        self.assertEqual(
            {e["event"] for e in envelopes}, {"job_completed", "job_failed"}
        )

    def test_webhook_rejects_userinfo_fragments_and_missing_identity(self):
        for url in (
            "http://user:secret@localhost/hook",
            "https://localhost/hook#secret",
            "http:///hook",
            "http://@localhost/hook",
            "http://localhost:0/hook",
            "http://localhost/\nhook",
            "http://[invalid/hook",
        ):
            self.assertEqual(
                notify({"url": url, "envelope": {"id": "event"}}), "invalid_destination"
            )
        self.assertEqual(
            notify({"url": "https://localhost/hook"}), "invalid_destination"
        )

    def test_cli_retry_preserves_live_lease(self):
        from catabolic.consumer_adapters import ConsumerError
        from catabolic.notifications import retry_delivery

        self.configure("one")
        self.event()
        with Store(self.database, writable=True) as store, store.transaction() as db:
            db.execute(
                "UPDATE notification_deliveries SET state='leased',lease_token='owned',lease_until=9999999999"
            )
            with self.assertRaises(ConsumerError) as caught:
                retry_delivery(db, "default", "one")
            self.assertEqual(caught.exception.code, "delivery_in_progress")
            self.assertEqual(
                db.execute(
                    "SELECT lease_token FROM notification_deliveries"
                ).fetchone()[0],
                "owned",
            )
