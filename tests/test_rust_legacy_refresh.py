# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Isolated frozen CLI pairs for the four preserved Jellyfin refresh leaves.

Numeric loopback and synthetic credentials only. These do not qualify the
filesystem owner, real consumer indexing, or all URL/HTTP implementations.
"""

import fcntl
import json
import os
import select
import shutil
import signal
import sqlite3
import subprocess
import threading
import time
import unittest
from contextlib import closing
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from tests import test_rust_legacy_cli as legacy
from tests.test_rust_legacy_cli import BINARY
from tests.test_rust_legacy_parser import PYTHON, REFERENCE

ERROR = "legacy refresh delivery failed; inspect endpoint and credential reference"
STAMP = "2000-01-02 03:04:05"
TOKEN = "FAKE_REFRESH_FIXTURE_ONLY"


class LegacyRefreshTest(unittest.TestCase):
    setUp = legacy.NativeLegacyCliTest.setUp
    run_process = legacy.NativeLegacyCliTest.run_process
    normalized = legacy.NativeLegacyCliTest.normalized
    compare = legacy.NativeLegacyCliTest.compare

    def tearDown(self):
        evidence = os.environ.get("CATABOLIC_REFRESH_EVIDENCE")
        results = self._outcome.result
        if evidence and any(
            test is self or test.id().startswith(self.id())
            for test, _ in results.failures + results.errors
        ):
            destination = Path(evidence) / "failed-fixtures" / self.id()
            shutil.copytree(self.root, destination, dirs_exist_ok=True)

    def sql(self, statement, parameters=()):
        for path in (self.reference, self.native):
            with closing(sqlite3.connect(path)) as db, db:
                db.execute(statement, parameters)

    def rows(self, path, table="refresh_events"):
        with closing(sqlite3.connect(path)) as db:
            db.row_factory = sqlite3.Row
            return [
                dict(row) for row in db.execute(f"SELECT * FROM {table} ORDER BY 1")
            ]

    def server(self):
        self.status = 204
        self.body = b""
        self.delay = 0
        self.hook = None
        self.active_database = None
        self.calls = []
        self.closed = threading.Event()
        self.entered = threading.Event()
        self.release = threading.Event()
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_POST(self):
                try:
                    path = fixture.active_database
                    with open(str(path) + ".lock", "a+b") as lock:
                        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                        with closing(sqlite3.connect(path, timeout=0.1)) as db, db:
                            db.execute("BEGIN IMMEDIATE")
                            row = db.execute(
                                "SELECT attempts,lease_token,lease_until FROM refresh_events WHERE id=1"
                            ).fetchone()
                            assert row[0] >= 1 and row[1] and row[2] > time.time()
                            if fixture.hook:
                                fixture.hook(db)
                        fcntl.flock(lock, fcntl.LOCK_UN)
                    fixture.calls.append(
                        (self.path, self.headers.get("X-Emby-Token"), "writer_released")
                    )
                    fixture.entered.set()
                    if fixture.delay:
                        deadline = time.monotonic() + fixture.delay
                        while (
                            time.monotonic() < deadline and not fixture.release.is_set()
                        ):
                            readable, _, _ = select.select(
                                [self.connection], [], [], 0.05
                            )
                            if readable and self.connection.recv(1) == b"":
                                fixture.closed.set()
                                return
                    self.send_response(fixture.status)
                    if fixture.status == 302:
                        self.send_header("Location", "/redirect-must-not-run")
                    self.send_header("Content-Length", str(len(fixture.body)))
                    self.end_headers()
                    self.wfile.write(fixture.body)
                except (BrokenPipeError, ConnectionResetError):
                    pass
                except Exception as error:
                    fixture.calls.append(
                        ("handler-error", type(error).__name__, str(error))
                    )
                    fixture.entered.set()
                    self.send_error(500)

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(thread.join, 5)
        self.addCleanup(server.shutdown)
        self.addCleanup(self.release.set)
        self.endpoint = f"http://127.0.0.1:{server.server_port}"
        self.env["CATABOLIC_TEST_JELLYFIN_TOKEN"] = TOKEN

    def healthy(self):
        # Frozen fixtures seed a claimed empty publication independently for each
        # database. The implementation only verifies this existing publication.
        program = r"""
import pathlib,sys
sys.path.insert(0,sys.argv[1])
from catabolic.store import Store
import catabolic.store as module
from catabolic.app import Application
from catabolic.reconcile import Reconciler
assert pathlib.Path(module.__file__).resolve() == pathlib.Path(sys.argv[1]) / 'catabolic/store.py'
with Store(sys.argv[2], writable=True) as store:
    app=Application(store)
    app.bind('output','global',sys.argv[3])
    assert Reconciler(app).apply()['healthy']
"""
        for path in (self.reference, self.native):
            output = self.root / (path.stem + "-output")
            output.mkdir()
            result = subprocess.run(
                [
                    str(PYTHON),
                    "-c",
                    program,
                    str(REFERENCE / "src"),
                    str(path),
                    str(output),
                ],
                capture_output=True,
                text=True,
                env=self.env,
                timeout=30,
            )
            self.assertEqual(result.returncode, 0, result.stderr)

    def seed(self, count=1, **values):
        target = {
            "profile": "default",
            "catalog": "global",
            "application": "jellyfin",
            "endpoint": getattr(self, "endpoint", "http://127.0.0.1:9"),
            "credential_env": "CATABOLIC_TEST_JELLYFIN_TOKEN",
        }
        for index in range(count):
            record = {
                "id": index + 1,
                "profile": "default",
                "catalog": "global",
                "target": json.dumps(target, sort_keys=True, separators=(",", ":")),
                "created_at": STAMP,
                **values,
            }
            self.sql(
                f"INSERT INTO refresh_events({','.join(record)}) VALUES ({','.join('?' for _ in record)})",
                tuple(record.values()),
            )

    def paired_run(self, scenario, *, limit=100, expected_code=0, mode="--machine"):
        results = []
        for reference, path in ((True, self.reference), (False, self.native)):
            self.active_database = path
            result = self.run_process(
                reference,
                ["--db", str(path), mode, "refresh", "run", "--limit", str(limit)],
            )
            self.assertEqual(
                result.returncode,
                expected_code,
                (scenario, result.stdout, result.stderr),
            )
            results.append(result)
        self.assertEqual(results[0].stdout, results[1].stdout, scenario)
        self.assertEqual(results[0].stderr, results[1].stderr, scenario)
        left, right = self.rows(self.reference), self.rows(self.native)
        for records in (left, right):
            for row in records:
                if row["finished_at"] and row["finished_at"] != STAMP:
                    row["finished_at"] = "<SQL_CURRENT_TIMESTAMP>"
        self.assertEqual(left, right, scenario)
        self.assertNotIn(
            TOKEN, results[1].stdout + results[1].stderr + json.dumps(right)
        )
        report = json.loads(results[1].stdout)
        return report["data"] if mode == "--machine" else report

    def test_configure_defaults_update_profiles_and_validation(self):
        for mode in ([], ["--json"], ["--machine"]):
            self.compare(
                "LR-CONFIG-DEFAULT",
                mode
                + [
                    "refresh",
                    "configure",
                    "--endpoint",
                    "http://127.0.0.1:8096/base///",
                ],
            )
        self.compare(
            "LR-CONFIG-UPDATE",
            [
                "--machine",
                "refresh",
                "configure",
                "--endpoint",
                "https://example.invalid",
                "--credential-env",
                "_OTHER_42",
            ],
        )
        self.assertEqual(
            self.rows(self.reference, "refresh_targets"),
            self.rows(self.native, "refresh_targets"),
        )
        for n, extra in enumerate(
            (
                ["--endpoint", "ftp://example.invalid"],
                ["--endpoint", "http://u:p@example.invalid"],
                ["--endpoint", "http://example.invalid?token=FAKE"],
                ["--endpoint", "http://example.invalid#fragment"],
                ["--endpoint", ""],
                ["--endpoint", "http://127.0.0.1/" + "x" * 4096],
                ["--endpoint", "http://127.0.0.1", "--credential-env", "X-Y"],
                ["--endpoint", "http://127.0.0.1", "--catalog", "absent"],
            )
        ):
            self.compare(
                f"LR-CONFIG-ERR-{n}", ["--machine", "refresh", "configure"] + extra
            )
        self.compare(
            "LR-PROFILE", ["--profile", "absent", "--machine", "refresh", "list"]
        )
        self.compare(
            "LR-PROFILE-BEFORE-LIMIT",
            ["--profile", "absent", "--machine", "refresh", "run", "--limit", "0"],
        )
        self.sql("INSERT INTO profiles(id) VALUES ('second')")
        self.compare(
            "LR-CONFIG-SECOND",
            [
                "--profile",
                "second",
                "--machine",
                "refresh",
                "configure",
                "--endpoint",
                "http://127.0.0.1",
            ],
        )
        self.assertEqual(len(self.rows(self.native, "refresh_targets")), 2)
        for command in (
            ["refresh", "list"],
            ["refresh", "run"],
            ["refresh", "retry", "1"],
        ):
            self.compare("LR-MISSING-DB", ["--machine"] + command, database=False)

    def test_lists_pagination_retry_and_integer_errors(self):
        self.seed(3, attempts=3, state="failed", error="old", finished_at=STAMP)
        for mode in ([], ["--json"], ["--machine"]):
            for extra in (
                [],
                ["--limit", "1"],
                ["--after", "1", "--limit", "1"],
                ["--after", "3"],
                ["--after", "-1"],
            ):
                self.compare("LR-LIST-PAGE", mode + ["refresh", "list"] + extra)
        for leaf in ("list", "run"):
            for limit in ("0", "1001", "-1", "not-an-int"):
                self.compare(
                    "LR-LIMIT", ["--machine", "refresh", leaf, "--limit", limit]
                )
        self.sql(
            "UPDATE refresh_events SET lease_token='old-lease',lease_until=? WHERE id=1",
            (time.time() + 1000,),
        )
        self.compare("LR-RETRY-LEASE", ["--machine", "refresh", "retry", "1"])
        row = self.rows(self.native)[0]
        self.assertEqual(
            (row["state"], row["attempts"], row["lease_token"], row["finished_at"]),
            ("queued", 0, None, STAMP),
        )
        self.sql("UPDATE refresh_events SET state='complete' WHERE id=2")
        for identifier in ("2", "99", "-1"):
            self.compare("LR-RETRY-ERR", ["--machine", "refresh", "retry", identifier])
        self.assertEqual(self.rows(self.reference), self.rows(self.native))

    def test_empty_exhausted_and_live_leases_skip_requests(self):
        self.paired_run("LR-EMPTY")
        self.seed(2, attempts=3, state="failed", error="old")
        self.sql(
            "UPDATE refresh_events SET attempts=0,lease_token='live',lease_until=? WHERE id=2",
            (time.time() + 1000,),
        )
        report = self.paired_run("LR-SKIP", expected_code=3)
        self.assertEqual(
            report,
            {
                "events": [],
                "unresolved": 2,
                "complete": False,
                "indexing_verified": False,
            },
        )

    def test_unhealthy_blocks_without_claim_or_network(self):
        self.seed(2)
        report = self.paired_run("LR-UNHEALTHY", expected_code=3)
        self.assertEqual(
            report["events"],
            [{"id": 1, "state": "blocked", "error": "publication_unhealthy"}],
        )
        self.assertEqual(self.rows(self.native)[0]["attempts"], 0)

    def test_success_limit_snapshotted_target_and_writer_release(self):
        self.server()
        self.healthy()
        self.seed(2)
        self.compare(
            "LR-CONFIG-NOT-RETARGET",
            ["--json", "refresh", "configure", "--endpoint", "http://127.0.0.1:9"],
        )
        report = self.paired_run("LR-SUCCESS-LIMIT", limit=1, expected_code=3)
        self.assertEqual(
            report["events"], [{"id": 1, "state": "complete", "error": None}]
        )
        self.assertEqual(
            self.calls, [("/Library/Refresh", TOKEN, "writer_released")] * 2
        )
        # Handler's row-one assertion is only used for the first event.
        self.sql("DELETE FROM refresh_events WHERE id=1")
        self.sql("UPDATE refresh_events SET id=1 WHERE id=2")
        self.paired_run("LR-SUCCESS-REMAINDER")
        self.assertEqual(len(self.calls), 4)

    def test_failure_retry_exhaustion_missing_and_invalid_credential(self):
        self.server()
        self.healthy()
        self.seed()
        self.status = 503
        for attempt in range(1, 4):
            report = self.paired_run(f"LR-FAILED-{attempt}", expected_code=3)
            self.assertEqual(
                report["events"], [{"id": 1, "state": "failed", "error": ERROR}]
            )
            self.assertEqual(self.rows(self.native)[0]["attempts"], attempt)
        self.assertEqual(self.paired_run("LR-EXHAUSTED", expected_code=3)["events"], [])
        self.assertEqual(len(self.calls), 6)
        self.compare("LR-RETRY", ["--machine", "refresh", "retry", "1"])
        for credential in (None, "BAD\nFAKE", "BAD\rFAKE"):
            if credential is None:
                self.env.pop("CATABOLIC_TEST_JELLYFIN_TOKEN", None)
            else:
                self.env["CATABOLIC_TEST_JELLYFIN_TOKEN"] = credential
            self.paired_run("LR-CREDENTIAL", expected_code=3)
        self.assertEqual(len(self.calls), 6)
        self.compare("LR-RETRY-AGAIN", ["--machine", "refresh", "retry", "1"])
        self.env["CATABOLIC_TEST_JELLYFIN_TOKEN"] = TOKEN
        self.status = 204
        self.paired_run("LR-RETRY-COMPLETE")

    def test_expired_lease_ack_fence_preserves_explicit_retry(self):
        self.server()
        self.healthy()
        self.seed(lease_token="expired", lease_until=0)

        def retry(db):
            db.execute(
                "UPDATE refresh_events SET attempts=0,state='queued',error=NULL,lease_token=NULL,lease_until=NULL WHERE id=1"
            )

        self.hook = retry
        report = self.paired_run("LR-DISPLACED-ACK", limit=1, expected_code=3)
        self.assertEqual(
            report["events"], [{"id": 1, "state": "complete", "error": None}]
        )
        self.assertEqual(self.rows(self.native)[0]["state"], "queued")
        self.assertEqual(self.rows(self.native)[0]["attempts"], 0)

    def test_redirect_and_oversized_body_fail_redacted(self):
        self.server()
        self.healthy()
        self.seed()
        for status, body in ((302, b""), (200, b"x" * (1024 * 1024 + 1))):
            self.status, self.body = status, body
            self.paired_run("LR-BOUNDED-HTTP", expected_code=3)
        self.assertEqual([row[0] for row in self.calls], ["/Library/Refresh"] * 4)

    def test_elapsed_request_deadline_and_interrupt_reap_helper(self):
        self.server()
        self.healthy()
        self.seed()
        self.delay = 20
        started = time.monotonic()
        self.active_database = self.native
        result = self.run_process(
            False, ["--db", str(self.native), "--machine", "refresh", "run"]
        )
        self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
        elapsed = time.monotonic() - started
        self.assertGreaterEqual(elapsed, 14)
        self.assertLess(elapsed, 18)
        self.assertTrue(self.entered.is_set())
        self.assertTrue(self.closed.wait(2))
        self.assertEqual(len(self.calls), 1)
        self.assertEqual(self.rows(self.native)[0]["state"], "failed")
        self.assertIsNone(self.rows(self.native)[0]["lease_token"])
        self.entered.clear()
        self.release.set()
        self.release = threading.Event()
        process = subprocess.Popen(
            [str(BINARY), "--db", str(self.native), "--machine", "refresh", "run"],
            env=self.env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

        def reap():
            if process.poll() is None:
                process.kill()
            process.communicate(timeout=10)

        self.addCleanup(reap)
        self.assertTrue(self.entered.wait(10))
        self.closed.clear()
        process.send_signal(signal.SIGINT)
        out, err = process.communicate(timeout=10)
        self.assertEqual(process.returncode, 130, out + err)
        self.assertEqual(err, "")
        self.assertTrue(self.closed.wait(2))
        row = self.rows(self.native)[0]
        self.assertEqual(row["attempts"], 2)
        self.assertIsNotNone(row["lease_token"])
        self.assertNotIn(TOKEN, out + err)

    def test_current_publication_health_is_rechecked_before_retry(self):
        self.server()
        self.healthy()
        self.seed()
        self.status = 503
        self.paired_run("LR-FIRST-FAILURE", expected_code=3)
        for path in (self.reference, self.native):
            output = self.root / (path.stem + "-output")
            output.rename(output.with_suffix(".unavailable"))
        self.status = 204
        report = self.paired_run("LR-HEALTH-CHANGED", expected_code=3)
        self.assertEqual(
            report["events"],
            [{"id": 1, "state": "blocked", "error": "publication_unhealthy"}],
        )
        self.assertEqual(self.rows(self.native)[0]["attempts"], 1)
        self.assertEqual(len(self.calls), 2)

    def test_refresh_help_and_parser_errors_remain_frozen(self):
        for tail in (
            ["--help"],
            ["configure", "--help"],
            ["list", "--help"],
            ["run", "--help"],
            ["retry", "--help"],
            ["configure"],
            ["retry"],
            ["list", "--after", "bad"],
            ["run", "--unknown"],
        ):
            self.compare("LR-PARSER", ["refresh"] + tail, database=False)

    def test_private_helper_rejects_malformed_oversize_and_wrong_application(self):
        for raw in ("not-json", "[]", "{}", '{"application":"plex"}', "x" * 262145):
            result = subprocess.run(
                [str(BINARY), "__legacy-refresh-worker"],
                input=raw,
                capture_output=True,
                text=True,
                env=self.env,
                timeout=5,
            )
            self.assertEqual(
                (result.returncode, result.stdout, result.stderr), (0, "false\n", "")
            )

    def test_documented_url_admission_difference(self):
        # Python only accesses urlsplit.hostname at configuration time. Native
        # uses the shared destination validator and rejects an invalid port now.
        args = [
            "--machine",
            "refresh",
            "configure",
            "--endpoint",
            "http://127.0.0.1:70000",
        ]
        oracle = self.run_process(True, ["--db", str(self.reference)] + args)
        native = self.run_process(False, ["--db", str(self.native)] + args)
        self.assertEqual(oracle.returncode, 0)
        self.assertEqual(native.returncode, 2)
        self.assertIn("endpoint must be an HTTP(S)", native.stdout)
        self.assertEqual(self.rows(self.native, "refresh_targets"), [])

    def test_documented_integer_overflow_difference(self):
        for tail in (["retry", str(2**100)], ["list", "--after", str(2**100)]):
            args = ["--machine", "refresh"] + tail
            oracle = self.run_process(True, ["--db", str(self.reference)] + args)
            native = self.run_process(False, ["--db", str(self.native)] + args)
            self.assertEqual(oracle.returncode, 1)
            self.assertIn("OverflowError: Python int too large", oracle.stderr)
            self.assertEqual(native.returncode, 2)
            self.assertEqual(native.stderr, "")
            self.assertEqual(
                json.loads(native.stdout)["data"]["error"]["message"],
                "Python int too large to convert to SQLite INTEGER",
            )
