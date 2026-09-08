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

        def deliver(env, event, severity):
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

        def deliver(*args):
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
