# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native consumer refresh against real disposable loopback protocol fixtures."""

import json
import os
import socket
import sqlite3
import subprocess
import threading
import time
import unittest

from catabolic.consumer_setup import discover, put_connection
from catabolic.consumers import claim
from catabolic.store import encode
from tests import test_consumers as reference_fixture
from tests.test_rust_migration import BINARY


class NativeConsumerTest(unittest.TestCase):
    add = reference_fixture.ConsumerTest.add
    force_due = reference_fixture.ConsumerTest.force_due

    def setUp(self):
        self.assertTrue(BINARY.is_file(), "build catabolic-cli before qualification")
        reference_fixture.ConsumerTest.setUp(self)

    def native(
        self, operation, request=None, *, ok=True, env=None, surface="consumers"
    ):
        run = subprocess.run(
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
            timeout=40,
            env={**os.environ, **(env or {})},
        )
        self.assertEqual(run.returncode, 0 if ok else 2, run.stderr or run.stdout)
        return json.loads(run.stdout) if ok else run.stderr

    def bind(self, **values):
        return self.native(
            "bind",
            {
                "id": "movies",
                "connection": "plex",
                "catalog": "global",
                "subtree": "Movies",
                "remote_root": "/plex/Movies",
                "library_id": "1",
                "type": "movie",
                "apply": True,
                **values,
            },
        )

    def sync(self):
        return self.native("apply", {}, surface="outputs")

    def status(self):
        return self.native("status")

    def run_pending(self, **values):
        return self.native("run", {"limit": 1, **values})

    def pending(self):
        self.bind()
        self.add()
        result = self.sync()
        self.assertTrue(result["healthy"], result)

    def test_setup_discovery_parity_and_readonly_dryrun(self):
        reference = discover(self.database, "default", "plex", "movie")
        self.assertEqual(
            self.native("discover", {"connection": "plex", "type": "movie"}), reference
        )
        request = {
            "id": "new",
            "application": "plex",
            "endpoint": self.remote.url + "/",
            "credential_env": "CATABOLIC_TEST_PLEX_TOKEN",
            "apply": False,
        }
        reference = put_connection(
            self.database,
            "default",
            "new",
            "plex",
            self.remote.url + "/",
            "CATABOLIC_TEST_PLEX_TOKEN",
        )
        self.assertEqual(self.native("connection-put", request), reference)
        self.assertEqual(len(self.native("connections")["connections"]), 1)
        request["apply"] = True
        self.assertEqual(
            self.native("connection-put", request)["connection"],
            reference["connection"],
        )
        self.bind(apply=False)
        self.assertEqual(self.status()["bindings"], [])
        self.bind()
        self.assertEqual(self.status()["bindings"][0]["library"]["uuid"], "uuid-1")
        self.assertEqual(self.remote.lock_failures, [])
        self.assertEqual(self.remote.scans, [])

    def test_native_publication_scan_and_idempotent_initial(self):
        self.pending()
        result = self.run_pending()
        self.assertTrue(result["complete"], result)
        self.assertEqual(result["deliveries"][0]["state"], "accepted")
        self.assertFalse(result["deliveries"][0]["indexing_verified"])
        self.assertEqual(self.remote.scans, ["/library/sections/1/refresh"])
        self.assertEqual(self.run_pending()["processed"], 0)
        self.bind(
            id="initial",
            library_id="2",
            remote_root="/different",
            initial_scan=True,
            automatic=True,
        )
        self.assertEqual(len(self.remote.scans), 2)
        self.bind(
            id="initial",
            library_id="2",
            remote_root="/different",
            initial_scan=True,
            automatic=True,
        )
        self.assertEqual(len(self.remote.scans), 2)
        self.assertEqual(self.remote.lock_failures, [])
        self.assertTrue(
            all(
                "force" not in query
                and "trash" not in path.lower()
                and method != "DELETE"
                for method, path, query in self.remote.requests
            )
        )

    def test_jellyfin_exact_non_destructive_refresh(self):
        self.native(
            "connection-put",
            {
                "id": "jelly",
                "application": "jellyfin",
                "endpoint": self.remote.url,
                "credential_env": "CATABOLIC_TEST_PLEX_TOKEN",
                "apply": True,
            },
        )
        self.bind(connection="jelly", type="movies")
        self.add()
        self.sync()
        self.assertTrue(self.run_pending()["complete"])
        self.assertEqual(
            self.remote.requests[-1],
            (
                "POST",
                "/Items/1/Refresh",
                {
                    "Recursive": ["true"],
                    "MetadataRefreshMode": ["Default"],
                    "ImageRefreshMode": ["Default"],
                    "ReplaceAllMetadata": ["false"],
                    "ReplaceAllImages": ["false"],
                },
            ),
        )
        self.assertEqual(self.remote.lock_failures, [])

    def test_coalescing_scope_and_overlap_rejection(self):
        self.remote.libraries[0]["roots"] = ["/plex"]
        self.bind()
        self.bind(id="tv", subtree="TV", remote_root="/plex/TV")
        self.bind(id="other", library_id="2", remote_root="/different")
        self.add()
        self.add("Two", "TV")
        self.add("Three")
        self.sync()
        self.assertEqual(self.run_pending(limit=10)["processed"], 2)
        self.assertEqual(
            sorted(self.remote.scans),
            ["/library/sections/1/refresh", "/library/sections/2/refresh"],
        )
        bad = {
            "id": "overlap",
            "connection": "plex",
            "catalog": "global",
            "subtree": "Movies",
            "remote_root": "/plex/Movies/Child",
            "library_id": "1",
            "type": "movie",
            "apply": True,
        }
        self.assertIn("overlapping_binding_roots", self.native("bind", bad, ok=False))

    def test_busy_backoff_failure_exhaustion_repair_and_redaction(self):
        self.pending()
        self.remote.busy = True
        result = self.run_pending()
        self.assertEqual(result["deliveries"][0]["state"], "busy")
        self.assertEqual(self.status()["bindings"][0]["attempts"], 0)
        self.assertEqual(self.remote.scans, [])
        self.remote.busy = False
        self.remote.mode = 429
        self.force_due()
        result = self.run_pending()
        self.assertEqual(result["deliveries"][0]["error"]["code"], "rate_limited")
        with sqlite3.connect(self.database) as db:
            self.assertGreater(
                db.execute("SELECT due_at FROM consumer_deliveries").fetchone()[0],
                time.time() + 100,
            )
        self.assertEqual(self.run_pending()["processed"], 0)
        self.remote.mode = 503
        for _ in range(4):
            self.force_due()
            self.run_pending()
        self.assertEqual(self.status()["bindings"][0]["state"], "exhausted")
        self.native("retry", {"id": "movies"})
        self.remote.mode = 401
        result = self.run_pending()
        self.assertEqual(result["deliveries"][0]["state"], "repair")
        self.assertNotIn(
            "DO_NOT_LEAK",
            encode(result) + encode(self.status()) + encode(self.native("attempts")),
        )
        self.assertEqual(self.remote.scans, [])

    def test_generation_changed_during_scan_not_acknowledged(self):
        self.pending()

        def change():
            self.add("Two")
            self.sync()

        self.remote.on_scan = change
        report = self.run_pending()
        self.assertFalse(report["complete"])
        row = self.status()["bindings"][0]
        self.assertGreater(row["generation"], row["acknowledged"])
        self.assertTrue(self.run_pending()["complete"])
        self.assertEqual(len(self.remote.scans), 2)

    def test_disable_during_scan_fences_ack_and_enable_requeues(self):
        self.pending()
        self.remote.on_scan = lambda: self.native("disable", {"id": "movies"})
        report = self.run_pending()
        self.assertEqual(report["deliveries"][0]["state"], "stale_acknowledgement")
        self.assertEqual(self.status()["bindings"][0]["state"], "disabled")
        self.assertEqual(self.status()["bindings"][0]["acknowledged"], 0)
        self.native("enable", {"id": "movies"})
        self.assertTrue(self.run_pending()["complete"])
        self.assertEqual(len(self.remote.libraries), 2)

    def test_unhealthy_shared_binding_and_output_identity_hold_scan(self):
        self.remote.libraries[0]["roots"] = ["/plex"]
        self.bind()
        self.bind(id="tv", subtree="TV", remote_root="/plex/TV")
        self.add()
        self.sync()
        self.add("Two", "TV")
        (self.source / "Two.mkv").unlink()
        self.assertFalse(self.sync()["healthy"])
        self.assertEqual(self.run_pending()["processed"], 0)
        self.output.rename(self.root / "offline")
        self.force_due()
        self.assertEqual(self.run_pending()["processed"], 0)
        self.assertEqual(self.remote.scans, [])

    def test_live_lease_contention_expiry_and_crash_recovery(self):
        self.pending()
        old = claim(self.database, "default")
        self.assertIsNotNone(old)
        self.assertEqual(self.run_pending()["processed"], 0)
        with sqlite3.connect(self.database) as db:
            db.execute("UPDATE consumer_deliveries SET lease_until=0")
        self.assertTrue(self.run_pending()["complete"])
        attempts = self.native("attempts")["attempts"]
        self.assertEqual([r["state"] for r in attempts], ["accepted", "lease_expired"])
        self.assertEqual(self.remote.lock_failures, [])

    def test_retry_preserves_inflight_refresh_lease(self):
        self.pending()
        entered, release = threading.Event(), threading.Event()
        self.remote.on_scan = lambda: (entered.set(), release.wait(15))
        process = subprocess.Popen(
            [
                str(BINARY),
                "--db",
                str(self.database),
                "operation",
                "consumers",
                "run",
                "--options",
                '{"limit":1}',
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            self.assertTrue(entered.wait(10), "refresh never reached loopback receiver")
            with sqlite3.connect(self.database) as db:
                before = db.execute("SELECT * FROM consumer_deliveries").fetchall()
            self.assertIn(
                "active lease", self.native("retry", {"id": "movies"}, ok=False)
            )
            self.assertTrue(self.native("enable", {"id": "movies"})["complete"])
            with sqlite3.connect(self.database) as db:
                self.assertEqual(
                    db.execute("SELECT * FROM consumer_deliveries").fetchall(), before
                )
            self.assertEqual(self.run_pending()["processed"], 0)
            self.assertEqual(len(self.remote.scans), 1)
            release.set()
            stdout, stderr = process.communicate(timeout=20)
            self.assertEqual(process.returncode, 0, stderr)
            self.assertTrue(json.loads(stdout)["complete"])
            self.assertEqual(len(self.remote.scans), 1)
        finally:
            release.set()
            if process.poll() is None:
                process.kill()
                process.communicate()

    def test_noop_disable_preserves_delivery_and_binding_revisions(self):
        self.bind()
        self.native("disable", {"id": "movies"})
        with sqlite3.connect(self.database) as db:
            before = (
                db.execute("SELECT * FROM consumer_bindings").fetchall(),
                db.execute("SELECT * FROM consumer_deliveries").fetchall(),
            )
        self.native("disable", {"id": "movies"})
        with sqlite3.connect(self.database) as db:
            self.assertEqual(
                (
                    db.execute("SELECT * FROM consumer_bindings").fetchall(),
                    db.execute("SELECT * FROM consumer_deliveries").fetchall(),
                ),
                before,
            )

    def test_private_file_credentials_reject_unsafe_modes_links_and_fifo(self):
        secret = self.root / "credential.json"
        secret.write_text(
            json.dumps({"token": "synthetic-fixture", "client_id": "fixture-client"})
        )
        secret.chmod(0o600)
        request = {
            "id": "private",
            "application": "plex",
            "endpoint": self.remote.url,
            "credential_env": "file:" + str(secret),
            "apply": True,
        }
        self.assertTrue(self.native("connection-put", request)["complete"])
        secret.chmod(0o644)
        count = len(self.remote.requests)
        self.assertIn("unauthorized", self.native("connection-put", request, ok=False))
        secret.chmod(0o600)
        linked = self.root / "linked"
        linked.symlink_to(secret)
        request["credential_env"] = "file:" + str(linked)
        self.assertIn("unauthorized", self.native("connection-put", request, ok=False))
        fifo = self.root / "fifo"
        os.mkfifo(fifo, 0o600)
        request["credential_env"] = "file:" + str(fifo)
        self.assertIn("unauthorized", self.native("connection-put", request, ok=False))
        self.assertEqual(len(self.remote.requests), count)

    def test_server_library_and_connection_revision_fences(self):
        self.pending()
        for key, value, expected in [
            ("server", "replaced", "server_identity_changed"),
            ("uuid", "replaced", "library_identity_changed"),
        ]:
            self.native("retry", {"id": "movies"})
            self.remote.server_id = "fixture-server"
            self.remote.libraries[0]["uuid"] = "uuid-1"
            if key == "server":
                self.remote.server_id = value
            else:
                self.remote.libraries[0]["uuid"] = value
            report = self.run_pending()
            self.assertEqual(report["deliveries"][0]["error"]["code"], expected)
            self.assertEqual(report["deliveries"][0]["state"], "repair")
        self.assertEqual(self.remote.scans, [])

    def test_bounded_lists_and_invalid_configuration(self):
        for limit in [0, True, -1, 1.5, 101]:
            self.assertIn(
                "invalid_configuration", self.native("run", {"limit": limit}, ok=False)
            )
        self.assertEqual(
            self.native("connections", {"limit": 1})["listing_complete"], True
        )
        for address in [
            "file:///tmp/fixture",
            "http://user@127.0.0.1/",
            "http://127.0.0.1/?token=nope",
        ]:
            result = self.native(
                "connection-put",
                {
                    "id": "bad",
                    "application": "plex",
                    "endpoint": address,
                    "credential_env": "CATABOLIC_TEST_PLEX_TOKEN",
                },
                ok=False,
            )
            self.assertIn("invalid_configuration", result)

    def test_discovery_generation_change_defers_without_attempt_penalty(self):
        self.pending()
        original = self.remote.server.RequestHandlerClass
        fixture = self

        class Changed(original):
            changed = False

            def respond(self):
                if self.path == "/library/sections" and not Changed.changed:
                    Changed.changed = True
                    fixture.add("Two")
                    fixture.sync()
                super().respond()

        self.remote.server.RequestHandlerClass = Changed
        result = self.run_pending()
        self.assertEqual(result["deliveries"][0]["state"], "pending")
        self.assertEqual(
            result["deliveries"][0]["error"]["code"], "publication_changed"
        )
        self.assertEqual(self.status()["bindings"][0]["attempts"], 0)
        self.assertEqual(self.remote.scans, [])
        self.force_due()
        self.assertTrue(self.run_pending()["complete"])

    def test_native_process_death_leaves_expiring_uncertain_claim(self):
        self.pending()
        entered, release = threading.Event(), threading.Event()
        original = self.remote.server.RequestHandlerClass

        class DisconnectSafe(original):
            def respond(self):
                try:
                    super().respond()
                except (BrokenPipeError, ConnectionResetError):
                    pass

        self.remote.server.RequestHandlerClass = DisconnectSafe
        self.remote.on_scan = lambda: (entered.set(), release.wait(10))
        command = [
            str(BINARY),
            "--db",
            str(self.database),
            "operation",
            "consumers",
            "run",
            "--options",
            '{"limit":1}',
        ]
        process = subprocess.Popen(
            command, stdout=subprocess.PIPE, stderr=subprocess.PIPE
        )
        try:
            self.assertTrue(entered.wait(5))
            self.assertEqual(self.run_pending()["processed"], 0)
            process.kill()
            process.communicate(timeout=5)
        finally:
            release.set()
            if process.poll() is None:
                process.kill()
                process.communicate(timeout=5)
        with sqlite3.connect(self.database) as db:
            self.assertEqual(
                db.execute("SELECT state FROM consumer_deliveries").fetchone()[0],
                "leased",
            )
            db.execute("UPDATE consumer_deliveries SET lease_until=0")
        self.assertTrue(self.run_pending()["complete"])
        self.assertEqual(
            self.native("attempts")["attempts"][1]["state"], "lease_expired"
        )
        self.assertEqual(len(self.remote.scans), 2)

    def test_xml_entities_response_size_redirect_and_missing_credentials_fail_closed(
        self,
    ):
        original = self.remote.server.RequestHandlerClass

        class Hostile(original):
            status = 200
            body = b'<!DOCTYPE MediaContainer [<!ENTITY secret SYSTEM "file:///tmp/no-such-test-file">]><MediaContainer machineIdentifier="&secret;" version="fixture"/>'

            def respond(self):
                self.send_response(Hostile.status)
                self.send_header("Content-Length", str(len(Hostile.body)))
                if Hostile.status == 302:
                    self.send_header("Location", "http://127.0.0.1:9/must-not-forward")
                self.end_headers()
                try:
                    self.wfile.write(Hostile.body)
                except (BrokenPipeError, ConnectionResetError):
                    pass

        self.remote.server.RequestHandlerClass = Hostile
        for body, status, expected in [
            (Hostile.body, 200, "invalid_response"),
            (b"<invalid>", 200, "invalid_response"),
            (b"x" * (8 * 1024 * 1024 + 1), 200, "response_limit"),
            (b"DO_NOT_LEAK_TOKEN_OR_URL", 302, "temporarily_failed"),
        ]:
            Hostile.body, Hostile.status = body, status
            result = self.native("discover", {"connection": "plex"}, ok=False)
            self.assertIn(expected, result)
            self.assertNotIn("DO_NOT_LEAK", result)
        self.remote.server.RequestHandlerClass = original
        count = len(self.remote.requests)
        result = self.native(
            "discover",
            {"connection": "plex"},
            ok=False,
            env={"CATABOLIC_TEST_PLEX_TOKEN": ""},
        )
        self.assertIn("unauthorized", result)
        self.assertEqual(len(self.remote.requests), count)

    def test_indexing_exact_paths_bounded_evidence_and_revision_fence(self):
        self.pending()
        self.assertTrue(self.run_pending()["complete"])
        result = self.native("verify-indexing", {"id": "movies"})
        self.assertEqual(result["status"], "inconclusive")
        self.remote.indexed = ["/plex/Movies/One.mkv"]
        result = self.native("verify-indexing", {"id": "movies"})
        self.assertEqual(result["status"], "indexed")
        self.assertEqual(result["matched_paths"], self.remote.indexed)
        self.assertEqual(
            self.status()["bindings"][0]["indexing"]["generation_observed"], 1
        )
        self.assertIn(
            "not playback/readability or generation causality", result["coverage"]
        )
        self.add("Two")
        self.sync()
        count = len(self.remote.requests)
        self.assertIn(
            "response_limit",
            self.native("verify-indexing", {"id": "movies", "limit": 1}, ok=False),
        )
        self.assertEqual(len(self.remote.requests), count)

    def test_creation_preview_commit_reconcile_idempotence_and_binding(self):
        spec = {
            "name": "Created fixture",
            "type": "movie",
            "root": "/plex/Created",
            "scanner": "Fixture Scanner",
            "agent": "fixture.agent",
            "language": "en",
        }
        request = {"id": "created", "connection": "plex", "spec": spec}
        preview = self.native("creation", request)
        self.assertFalse(preview["applied"])
        self.assertEqual(self.native("creations")["creations"], [])
        request["apply"] = True
        result = self.native("creation", request)
        self.assertEqual(result["origin"], "created_reconciled")
        self.assertEqual(result["library"]["id"], "3")
        self.assertTrue(self.native("creation", request)["reused"])
        self.assertEqual(
            sum(method == "POST" for method, _, _ in self.remote.requests), 1
        )
        create = {
            **request,
            "catalog": "global",
            "subtree": "Created",
            "remote_root": spec["root"],
            "type": "movie",
        }
        result = self.native("create", create)
        self.assertTrue(result["applied"])
        self.assertEqual(result["binding"]["library"]["id"], "3")
        self.assertEqual(result["binding"]["origin"], "created")
        self.assertEqual(self.remote.scans, [])
        self.assertEqual(self.remote.lock_failures, [])

    def test_uncertain_creation_never_replays_post_and_checks_immutable_intent(self):
        original = self.remote.server.RequestHandlerClass
        posts = []

        class Uncertain(original):
            def respond(self):
                if self.command == "POST":
                    posts.append(self.path)
                    self.send_response(503)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                else:
                    super().respond()

        self.remote.server.RequestHandlerClass = Uncertain
        spec = {
            "name": "Uncertain fixture",
            "type": "movie",
            "root": "/plex/Uncertain",
            "scanner": "Fixture Scanner",
            "agent": "fixture.agent",
            "language": "en",
        }
        request = {"id": "uncertain", "connection": "plex", "spec": spec, "apply": True}
        result = self.native("creation", request)
        self.assertEqual(result["state"], "uncertain")
        self.assertFalse(result["retry_post"])
        self.assertEqual(len(posts), 1)
        self.assertEqual(self.native("creation", request)["state"], "uncertain")
        self.assertEqual(len(posts), 1)
        changed = {**request, "spec": {**spec, "name": "Changed fixture"}}
        self.assertIn(
            "creation_intent_mismatch", self.native("creation", changed, ok=False)
        )
        self.assertEqual(self.native("creations")["creations"][0]["state"], "uncertain")
        library = self.remote.library("3", spec["root"])
        library["name"] = spec["name"]
        self.remote.libraries.append(library)
        self.assertTrue(
            self.native("creation", {**request, "apply": False})["would_reconcile"]
        )
        self.assertEqual(self.native("creations")["creations"][0]["state"], "uncertain")
        result = self.native("creation", {**request, "apply": False, "reconcile": True})
        self.assertEqual(result["origin"], "created_reconciled")
        self.assertEqual(len(posts), 1)

    def test_automatic_native_publication_drains_after_store_release(self):
        self.bind(automatic=True)
        self.add()
        result = self.sync()
        self.assertTrue(result["consumers"]["complete"], result)
        self.assertEqual(self.remote.scans, ["/library/sections/1/refresh"])
        self.sync()
        self.assertEqual(len(self.remote.scans), 1)
        self.assertEqual(self.remote.lock_failures, [])

    def test_http10_delayed_close_uses_fresh_connections_through_indexing(self):
        # An HTTP/1.0 response without keep-alive is not persistent even when
        # Content-Length completes before the server's FIN reaches the client.
        reference = reference_fixture.ConsumerTest("runTest")
        self.addCleanup(reference.doCleanups)
        reference.setUp()
        self.assertNotEqual(self.database, reference.database)

        def observe_closing_connections(remote):
            server = remote.server
            handler = server.RequestHandlerClass
            self.assertEqual(handler.protocol_version, "HTTP/1.0")
            evidence = {
                "requests": [],
                "closed": [],
                "unexpected_reuse": [],
                "close_timeouts": [],
                "close_errors": [],
            }
            changed = threading.Condition()
            original_respond = handler.respond
            original_shutdown = server.shutdown_request

            def respond(request):
                with changed:
                    evidence["requests"].append(
                        (
                            request.client_address[1],
                            request.path.split("?", 1)[0],
                            request.headers.get("Connection", "").lower(),
                        )
                    )
                original_respond(request)

            def shutdown(connection):
                peer = connection.getpeername()[1]
                try:
                    # Hold the finished connection until the client closes it
                    # or incorrectly sends another request. The timeout bounds
                    # fixture cleanup; it never triggers a request retry.
                    connection.settimeout(2)
                    if connection.recv(1, socket.MSG_PEEK):
                        evidence["unexpected_reuse"].append(peer)
                except TimeoutError:
                    evidence["close_timeouts"].append(peer)
                except OSError as exc:
                    evidence["close_errors"].append((peer, exc.errno))
                finally:
                    original_shutdown(connection)
                    with changed:
                        evidence["closed"].append(peer)
                        changed.notify_all()

            handler.respond = respond
            server.shutdown_request = shutdown
            return evidence, changed

        reports = {}
        expected_paths = [
            "/",
            "/library/sections",
            "/",
            "/library/sections",
            "/library/sections/1/all",
        ]
        for engine, fixture in (("python", reference), ("native", self)):
            evidence, changed = observe_closing_connections(fixture.remote)
            try:
                self.assertTrue(fixture.bind()["complete"])
                fixture.add()
                self.assertTrue(fixture.sync()["healthy"])
                fixture.remote.indexed = ["/plex/Movies/One.mkv"]
                if engine == "native":
                    reports[engine] = self.native("verify-indexing", {"id": "movies"})
                else:
                    reports[engine] = reference_fixture.verify_indexing(
                        fixture.database, "default", "movies"
                    )
            except AssertionError as exc:
                self.fail(f"{exc}; loopback transport evidence: {evidence}")
            with changed:
                self.assertTrue(
                    changed.wait_for(
                        lambda evidence=evidence: (
                            len(evidence["closed"]) == len(expected_paths)
                        ),
                        timeout=3,
                    ),
                    evidence,
                )
            self.assertEqual([row[1] for row in evidence["requests"]], expected_paths)
            self.assertEqual(
                len({row[0] for row in evidence["requests"]}),
                len(expected_paths),
                evidence,
            )
            self.assertTrue(
                all(row[2] == "close" for row in evidence["requests"]),
                evidence,
            )
            self.assertEqual(evidence["unexpected_reuse"], [], evidence)
            self.assertEqual(evidence["close_timeouts"], [], evidence)
            self.assertEqual(evidence["close_errors"], [], evidence)
            self.assertEqual(fixture.remote.lock_failures, [])
            self.assertTrue(reports[engine]["complete"])
            self.assertEqual(reports[engine]["matched_paths"], ["/plex/Movies/One.mkv"])
        self.assertEqual(reports["native"], reports["python"])

    def _assert_indexing_translation_matches_python(
        self, remote_root, subtree, expected
    ):
        reference = reference_fixture.ConsumerTest("runTest")
        self.addCleanup(reference.doCleanups)
        reference.setUp()
        self.assertNotEqual(self.database, reference.database)
        for fixture in (reference, self):
            fixture.remote.libraries[0]["roots"] = [remote_root]
            self.assertTrue(
                fixture.bind(remote_root=remote_root, subtree=subtree)["complete"]
            )
            fixture.add()
            self.assertTrue(fixture.sync()["healthy"])
            fixture.remote.indexed = [expected]
        reference_requests = len(reference.remote.requests)
        native_requests = len(self.remote.requests)
        oracle = reference_fixture.verify_indexing(
            reference.database, "default", "movies"
        )
        actual = self.native("verify-indexing", {"id": "movies"})
        self.assertEqual(actual, oracle)
        self.assertTrue(actual["complete"])
        self.assertEqual(actual["matched_paths"], [expected])
        self.assertEqual(len(reference.remote.requests) - reference_requests, 3)
        self.assertEqual(len(self.remote.requests) - native_requests, 3)
        self.assertEqual(reference.remote.lock_failures, [])
        self.assertEqual(self.remote.lock_failures, [])

    def test_long_translated_path_matches_python_on_independent_catalogs(self):
        # Regression: the frozen contract bounds each path input at 4096
        # characters, rather than rejecting their valid composed output.
        remote_root = "/" + "/".join(["r" * 200] * 20) + "/" + "s" * 74
        expected = remote_root + "/One.mkv"
        self.assertEqual(len(remote_root), 4095)
        self.assertEqual(len(expected), 4103)
        self._assert_indexing_translation_matches_python(
            remote_root, "Movies", expected
        )

    def test_root_translation_matches_python_on_independent_catalogs(self):
        # An exact subtree match has an empty suffix, but its remote root must
        # remain "/" rather than becoming the empty string after slash trimming.
        self._assert_indexing_translation_matches_python("/", "Movies/One.mkv", "/")
