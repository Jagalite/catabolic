# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Disposable native fallback publication through loopback consumer delivery."""

import os
import sqlite3
import unittest
from contextlib import closing
from unittest.mock import patch

from catabolic.consumer_setup import put_connection
from tests import test_rust_fallback_projection as projection_fixture
from tests.test_consumers import ProtocolServer


class NativeFallbackConsumerTest(unittest.TestCase):
    policy = projection_fixture.NativeProjectionTest.policy
    native = projection_fixture.NativeProjectionTest.native
    fallback_bind = projection_fixture.NativeProjectionTest.bind

    def setUp(self):
        projection_fixture.NativeProjectionTest.setUp(self)
        self.remote = ProtocolServer(self.database)
        self.addCleanup(self.remote.close)
        environment = patch.dict(
            os.environ, {"CATABOLIC_TEST_PLEX_TOKEN": "fixture-token"}
        )
        environment.start()
        self.addCleanup(environment.stop)
        with self.store.detached():
            put_connection(
                self.database,
                "default",
                "plex",
                "plex",
                self.remote.url,
                "CATABOLIC_TEST_PLEX_TOKEN",
                apply=True,
            )
        self.fallback_bind()

    def consumers(self, action, request=None, **kwargs):
        return self.native(action, request or {}, surface="consumers", **kwargs)

    def consumer_bind(self, **options):
        return self.consumers(
            "bind",
            {
                "id": "movies",
                "connection": "plex",
                "catalog": "global",
                "subtree": "",
                "remote_root": "/plex/Movies",
                "library_id": "1",
                "type": "movie",
                "apply": True,
                **options,
            },
        )

    def publish(self):
        plan = self.native("run", {"catalog": "global"})
        return self.native(
            "run",
            {
                "catalog": "global",
                "apply": True,
                "expected_plan": plan["plan_id"],
            },
        )

    def pending(self):
        self.consumer_bind()
        result = self.publish()
        self.assertTrue(result["publication"]["healthy"], result)
        self.assertEqual(self.remote.scans, [])

    def status(self):
        return self.consumers("status")["bindings"][0]

    def run_pending(self):
        return self.consumers("run", {"limit": 1})

    def test_logical_publication_scan_and_effective_indexing(self):
        self.pending()
        self.assertEqual(self.store.rows("SELECT path FROM mappings"), [])
        result = self.run_pending()
        self.assertTrue(result["complete"], result)
        self.assertEqual(result["deliveries"][0]["state"], "accepted")
        path = self.store.rows("SELECT path FROM fallback_entries WHERE active=1")[0][
            "path"
        ]
        self.remote.indexed = ["/plex/Movies/" + path]
        indexing = self.consumers("verify-indexing", {"id": "movies"})
        self.assertTrue(indexing["complete"], indexing)
        self.assertEqual(indexing["expected"], 1)
        self.assertEqual(self.remote.scans, ["/library/sections/1/refresh"])
        self.assertEqual(self.remote.lock_failures, [])
        row = self.status()
        self.assertEqual(row["generation"], row["acknowledged"])
        self.assertEqual(row["generation"], row["verified"])

    def test_effective_indexing_limit_stops_before_remote_discovery(self):
        second = self.app.put_item("movie", {"fixture": "second"}, {"title": "Second"})[
            "id"
        ]
        self.app.media.associate(self.files["main.bin"], second)
        self.pending()
        paths = self.store.rows(
            "SELECT path FROM fallback_entries WHERE active=1 ORDER BY path"
        )
        self.assertEqual(len(paths), 2)
        requests = len(self.remote.requests)
        self.consumers(
            "verify-indexing", {"id": "movies", "limit": 1}, failure="response_limit"
        )
        self.assertEqual(len(self.remote.requests), requests)
        self.remote.indexed = ["/plex/Movies/" + row["path"] for row in paths]
        report = self.consumers("verify-indexing", {"id": "movies", "limit": 2})
        self.assertTrue(report["complete"], report)
        self.assertEqual(report["expected"], 2)

    def test_automatic_publication_drains_once_and_initial_scan_is_idempotent(self):
        self.consumer_bind(automatic=True)
        result = self.publish()
        self.assertTrue(result["complete"], result)
        self.assertEqual(self.remote.scans, ["/library/sections/1/refresh"])
        self.assertTrue(result["publication"]["consumers"]["complete"], result)
        self.consumer_bind(
            id="initial",
            library_id="2",
            remote_root="/different",
            initial_scan=True,
            automatic=True,
        )
        self.assertEqual(len(self.remote.scans), 2)
        self.consumer_bind(
            id="initial",
            library_id="2",
            remote_root="/different",
            initial_scan=True,
            automatic=True,
        )
        self.assertEqual(len(self.remote.scans), 2)
        self.assertEqual(self.remote.lock_failures, [])

    def test_lost_source_after_discovery_never_sends_or_acknowledges(self):
        self.pending()
        original = self.remote.server.RequestHandlerClass
        fixture = self

        class Changed(original):
            changed = False

            def respond(self):
                if self.path == "/library/sections" and not Changed.changed:
                    Changed.changed = True
                    (fixture.source / "main.bin").unlink()
                super().respond()

        self.remote.server.RequestHandlerClass = Changed
        result = self.run_pending()
        self.assertEqual(result["deliveries"][0]["state"], "retry")
        self.assertEqual(
            result["deliveries"][0]["error"]["code"], "publication_unhealthy"
        )
        self.assertEqual(self.remote.scans, [])
        self.assertEqual(self.status()["acknowledged"], 0)

    def test_competing_lease_after_discovery_is_preserved(self):
        self.pending()
        original = self.remote.server.RequestHandlerClass
        fixture = self

        class Changed(original):
            changed = False

            def respond(self):
                if self.path == "/library/sections" and not Changed.changed:
                    Changed.changed = True
                    with closing(sqlite3.connect(fixture.database)) as db, db:
                        db.execute(
                            "UPDATE consumer_deliveries SET lease_token='competitor'"
                        )
                super().respond()

        self.remote.server.RequestHandlerClass = Changed
        result = self.run_pending()
        self.assertEqual(
            result["deliveries"][0]["state"],
            "stale_acknowledgement",
            {
                "report": result,
                "requests": self.remote.requests,
                "lock_failures": self.remote.lock_failures,
                "changed": Changed.changed,
            },
        )
        self.assertEqual(self.remote.scans, [])
        self.assertEqual(self.status()["acknowledged"], 0)
        self.assertEqual(
            self.store.rows("SELECT lease_token FROM consumer_deliveries")[0][
                "lease_token"
            ],
            "competitor",
        )

    def test_new_generation_during_scan_remains_pending(self):
        self.pending()

        def newer_generation():
            with closing(sqlite3.connect(self.database)) as db, db:
                db.execute("UPDATE consumer_bindings SET generation=generation+1")

        self.remote.on_scan = newer_generation
        result = self.run_pending()
        self.assertFalse(result["complete"], result)
        row = self.status()
        self.assertGreater(row["generation"], row["acknowledged"])
        self.assertTrue(self.run_pending()["complete"])
        self.assertEqual(len(self.remote.scans), 2)

    def test_unresolved_retained_membership_cannot_scan(self):
        self.pending()
        (self.source / "main.bin").unlink()
        (self.source / "backup.bin").unlink()
        result = self.publish()
        self.assertTrue(result["membership_retained"], result)
        self.assertEqual(self.run_pending()["processed"], 0)
        self.assertEqual(self.remote.scans, [])
        self.assertEqual(self.status()["acknowledged"], 0)
