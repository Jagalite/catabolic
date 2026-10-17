# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Frozen consumer-watch CLI pairs on independent disposable catalogs.

Only numeric-loopback fixtures and synthetic credential values are used. These
checks exercise the adapter and existing owners, not broad filesystem or watcher
safety qualification. No real consumer or catalog is contacted.
"""

import fcntl
import json
import os
import select
import signal
import sqlite3
import subprocess
import threading
import time
import unittest
from contextlib import closing
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from tests import process_tree
from tests import test_rust_legacy_cli as legacy
from tests.test_rust_legacy_cli import BINARY, ORACLE
from tests.test_rust_legacy_parser import PYTHON, REFERENCE


class ConsumerWatchTest(unittest.TestCase):
    run_process = legacy.NativeLegacyCliTest.run_process
    normalized = legacy.NativeLegacyCliTest.normalized
    compare = legacy.NativeLegacyCliTest.compare

    def setUp(self):
        legacy.NativeLegacyCliTest.setUp(self)
        # The frozen owners launch additional `python -m` workers. Pin those
        # imports too; a venv's ambient installation is not reference evidence.
        self.env["PYTHONPATH"] = str(REFERENCE / "src")

    def sql(self, statement, parameters=()):
        for path in (self.reference, self.native):
            with closing(sqlite3.connect(path)) as db, db:
                db.execute(statement, parameters)

    def fixture_program(self, program, *args):
        result = subprocess.run(
            [str(PYTHON), "-c", program, str(REFERENCE / "src"), *map(str, args)],
            capture_output=True,
            text=True,
            env=self.env,
            timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def assert_writer_available(self, path):
        with open(str(path) + ".lock", "a+b") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with closing(sqlite3.connect(path, timeout=0.1)) as db:
                db.execute("BEGIN IMMEDIATE")
                db.rollback()
            fcntl.flock(lock, fcntl.LOCK_UN)

    def start(self, reference, mode="--machine", interval=1, limit=10):
        path = self.reference if reference else self.native
        command = (
            [str(PYTHON), "-c", ORACLE, str(REFERENCE / "src")]
            if reference
            else [str(BINARY)]
        )
        args = ([mode] if mode else []) + [
            "--db",
            str(path),
            "consumer",
            "watch",
            "--interval",
            str(interval),
            "--limit",
            str(limit),
        ]
        process = subprocess.Popen(
            command + args,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=self.env,
            start_new_session=True,
        )
        self.addCleanup(self.clean_process, process)
        return process

    @staticmethod
    def clean_process(process):
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.communicate(timeout=5)
        for stream in (process.stdout, process.stderr):
            stream.close()

    def line(self, process, timeout=10):
        self.assertTrue(
            select.select([process.stdout], [], [], timeout)[0], "no watch line"
        )
        line = process.stdout.readline()
        if not line:
            self.fail((process.poll(), process.stderr.read()))
        return line.decode()

    def stop(self, process, signum=signal.SIGINT):
        start = time.monotonic()
        process.send_signal(signum)
        stdout, stderr = process.communicate(timeout=5)
        # Frozen Python installs no TERM handler: TERM exits by the signal with
        # no new output. Native does the same after its cleanup has finished.
        expected = -signal.SIGTERM if signum == signal.SIGTERM else 130
        self.assertEqual(process.returncode, expected, (stdout, stderr))
        self.assertLess(time.monotonic() - start, 5)
        return stdout.decode(), stderr.decode()

    def server(self):
        fixture = self
        self.active_database = None
        self.calls = []
        self.server_errors = []
        self.block_route = None
        self.notify_status = 204
        self.consumer_status = 200
        self.entered = threading.Event()
        self.closed = threading.Event()
        self.release = threading.Event()

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                self.respond()

            def do_POST(self):
                self.respond()

            def respond(self):
                try:
                    if fixture.active_database:
                        fixture.assert_writer_available(fixture.active_database)
                    body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
                    fixture.calls.append((self.command, self.path, body))
                    route = self.path.split("?")[0]
                    if route == fixture.block_route:
                        fixture.entered.set()
                        while not fixture.release.wait(0.02):
                            ready = select.select([self.connection], [], [], 0)[0]
                            if ready and self.connection.recv(1) == b"":
                                fixture.closed.set()
                                return
                    status = fixture.consumer_status
                    if route == "/System/Info":
                        response = {"Id": "watch-fixture", "Version": "fixture-1"}
                    elif route == "/Library/VirtualFolders":
                        response = [
                            {
                                "ItemId": "1",
                                "Name": "Movies",
                                "CollectionType": "movies",
                                "Locations": ["/fixture/Movies"],
                            }
                        ]
                    elif route == "/Items/1/Refresh":
                        response = None
                    elif route == "/notify":
                        status, response = fixture.notify_status, None
                    else:
                        status, response = 404, None
                    payload = json.dumps(response).encode() if response else b""
                    self.send_response(status)
                    self.send_header("Content-Length", str(len(payload)))
                    self.end_headers()
                    self.wfile.write(payload)
                except (BrokenPipeError, ConnectionResetError):
                    fixture.closed.set()
                except Exception as error:
                    fixture.server_errors.append(repr(error))
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
        self.env["CATABOLIC_TEST_WATCH_TOKEN"] = "SYNTHETIC_WATCH_ONLY"
        self.env["CATABOLIC_TEST_WATCH_NOTIFY"] = self.endpoint + "/notify"

    def seed_consumer(self):
        program = r"""
import pathlib,sys
sys.path.insert(0,sys.argv[1])
from catabolic.app import Application
from catabolic.store import Store
from catabolic.reconcile import Reconciler
from catabolic.consumer_setup import put_connection, bind
assert pathlib.Path(sys.modules['catabolic.store'].__file__).resolve() == pathlib.Path(sys.argv[1])/'catabolic/store.py'
with Store(sys.argv[2], writable=True) as store:
    app=Application(store)
    app.bind('output','global',sys.argv[3])
    assert Reconciler(app).apply()['healthy']
put_connection(sys.argv[2], 'default', 'fixture', 'jellyfin', sys.argv[4], 'CATABOLIC_TEST_WATCH_TOKEN', apply=True)
bind(sys.argv[2], 'default', 'movies', 'fixture', 'global', '', '/fixture/Movies', '1', 'movies', apply=True, initial_scan=True)
"""
        for path in (self.reference, self.native):
            output = self.root / (path.stem + "-output")
            output.mkdir()
            self.active_database = path
            self.fixture_program(program, path, output, self.endpoint)
        self.calls.clear()

    def seed_notification(self, count=1):
        self.sql(
            "INSERT INTO notification_destinations(profile,id,credential_env,subscriptions,tags,min_severity,enabled) VALUES ('default','fixture','CATABOLIC_TEST_WATCH_NOTIFY','[\"job_completed\"]','[]','info',1)"
        )
        for n in range(count):
            self.sql(
                "INSERT INTO consumer_events(id,profile,event,severity,subject,created_at) VALUES (?,'default','job_completed','info',?,'2000-01-02 03:04:05')",
                (f"event-{n}", f"tâche-{n}"),
            )
            self.sql(
                "INSERT INTO notification_deliveries(profile,event_id,destination_id) VALUES ('default',?,'fixture')",
                (f"event-{n}",),
            )

    def snapshot(self, path):
        # Whole affected delivery/attempt/event rows, with only generated lease
        # tokens, event IDs, and wall-clock timestamps normalized.
        tables = [
            "consumer_deliveries",
            "consumer_attempts",
            "consumer_events",
            "notification_destinations",
            "notification_deliveries",
        ]
        result = {}
        with closing(sqlite3.connect(path)) as db:
            db.row_factory = sqlite3.Row
            for table in tables:
                rows = [
                    dict(row)
                    for row in db.execute(f"SELECT * FROM {table} ORDER BY rowid")
                ]
                for row in rows:
                    for key in (
                        "created_at",
                        "started_at",
                        "finished_at",
                        "accepted_at",
                        "updated_at",
                        "due_at",
                        "lease_until",
                    ):
                        if row.get(key) not in (None, 0):
                            row[key] = "<time>"
                    for key in ("token", "lease_token"):
                        if row.get(key):
                            row[key] = "<token>"
                    if table == "consumer_events" and not row["id"].startswith(
                        "event-"
                    ):
                        row["id"] = "<event>"
                result[table] = rows
            result["bindings"] = [
                tuple(row)
                for row in db.execute(
                    "SELECT id,profile,generation,verified,acknowledged,enabled,error FROM consumer_bindings ORDER BY id"
                )
            ]
        return result

    def test_validation_matches_frozen_before_catalog_access(self):
        for mode in ([], ["--json"], ["--machine"]):
            for option, value in (
                ("--limit", "0"),
                ("--limit", "101"),
                ("--limit", "1001"),
                ("--interval", "0"),
                ("--interval", "61"),
            ):
                self.compare(
                    "CW-VALIDATION", mode + ["consumer", "watch", option, value]
                )
            self.compare("CW-NO-DB", mode + ["consumer", "watch"], database=False)
        self.assertEqual(self.snapshot(self.reference), self.snapshot(self.native))

    def test_idle_lines_modes_writer_release_and_restart(self):
        for mode in ("", "--json", "--machine"):
            observations = []
            for reference, path in ((True, self.reference), (False, self.native)):
                process = self.start(reference, mode)
                lines = [self.line(process)]
                self.assert_writer_available(path)
                lines.append(self.line(process))
                self.assertEqual(lines[0], lines[1])
                observations.append((lines, self.stop(process)))
                self.assert_writer_available(path)
            self.assertEqual(*observations)
        self.assertEqual(self.snapshot(self.reference), self.snapshot(self.native))

    def test_notifications_limit_and_independent_completeness(self):
        self.server()
        self.seed_notification(count=2)
        observations = []
        effects = []
        for reference, path in ((True, self.reference), (False, self.native)):
            self.active_database = path
            self.calls.clear()
            process = self.start(reference, limit=1)
            first = self.line(process)
            report = json.loads(first)
            self.assertEqual(report["exit_code"], 0)
            self.assertTrue(report["data"]["complete"])
            self.assertFalse(report["data"]["notifications"]["complete"])
            second = self.line(process)
            self.assertTrue(json.loads(second)["data"]["notifications"]["complete"])
            observations.append(([first, second], self.stop(process)))
            effects.append(list(self.calls))
        self.assertEqual(*observations)
        # Compare the complete ordinary notification wire bytes as well as
        # decoded values: both owners preserve frozen Python ASCII escaping.
        self.assertIn(b"\\u00e2", effects[0][0][2])
        self.assertEqual(*effects)
        decoded = [
            [(method, route, json.loads(body)) for method, route, body in calls]
            for calls in effects
        ]
        self.assertEqual(*decoded)
        self.assertEqual(len(effects[0]), 2)
        self.assertEqual(self.snapshot(self.reference), self.snapshot(self.native))
        self.assertEqual(self.server_errors, [])

    def test_consumer_delivery_exact_lines_and_effects(self):
        self.server()
        self.seed_consumer()
        observations, effects = [], []
        for reference, path in ((True, self.reference), (False, self.native)):
            self.active_database = path
            self.calls.clear()
            process = self.start(reference)
            line = self.line(process)
            data = json.loads(line)["data"]
            self.assertEqual(data["processed"], 1)
            self.assertEqual(data["deliveries"][0]["state"], "accepted")
            self.assertFalse(data["deliveries"][0]["indexing_verified"])
            observations.append((line, self.stop(process)))
            effects.append(list(self.calls))
        self.assertEqual(*observations)
        self.assertEqual(*effects)
        self.assertEqual(len(effects[0]), 3)
        self.assertEqual(self.snapshot(self.reference), self.snapshot(self.native))
        self.assertEqual(self.server_errors, [])

    def test_notification_wire_bytes_and_durable_retry_receipts(self):
        self.server()
        subjects = [
            "plain ASCII",
            "tâche-中-é-e\u0301",
            "supplementary-😀-\U00010000-\U0010ffff",
            'quotes-"-slash-/-backslash-\\',
            "controls-" + "".join(map(chr, range(32))) + "\x7f\x80",
            "separators-\u2028-\u2029-\uffff",
        ]
        self.seed_notification(count=len(subjects))
        for n, subject in enumerate(subjects):
            self.sql(
                "UPDATE consumer_events SET subject=? WHERE id=?",
                (subject, f"event-{n}"),
            )
        receipts = []
        for status, state, attempts in ((503, "retry", 1), (204, "complete", 2)):
            self.notify_status = status
            self.sql("UPDATE notification_deliveries SET due_at=0")
            pair = []
            for reference, path in ((True, self.reference), (False, self.native)):
                self.active_database = path
                self.calls.clear()
                run = self.run_process(
                    reference,
                    ["--machine", "--db", str(path), "notify", "run"],
                )
                self.assertEqual(run.returncode, 3 if status == 503 else 0, run.stderr)
                snapshot = self.snapshot(path)
                rows = snapshot["notification_deliveries"]
                self.assertEqual(len(rows), len(subjects))
                self.assertEqual({row["state"] for row in rows}, {state})
                self.assertEqual({row["attempts"] for row in rows}, {attempts})
                self.assertTrue(all(row["lease_token"] is None for row in rows))
                pair.append(
                    {
                        "stdout": run.stdout,
                        "stderr": run.stderr,
                        "snapshot": snapshot,
                        "requests": [
                            [method, route, body.hex()]
                            for method, route, body in self.calls
                        ],
                    }
                )
            receipts.append({"status": status, "pair": pair})
            self.assertEqual(pair[0]["snapshot"], pair[1]["snapshot"])
            self.assertEqual(pair[0]["stdout"], pair[1]["stdout"])
            self.assertEqual(pair[0]["stderr"], pair[1]["stderr"])
        # Retain the complete pre/post-fix bytes and normalized durable rows
        # when a local qualification caller requests an evidence artifact.
        if report := os.environ.get("CATABOLIC_NOTIFICATION_WIRE_REPORT"):
            Path(report).write_text(json.dumps(receipts, indent=2) + "\n")
        for receipt in receipts:
            self.assertEqual(*receipt["pair"])
            self.assertEqual(len(receipt["pair"][0]["requests"]), len(subjects))
            for _, _, body in receipt["pair"][0]["requests"]:
                self.assertTrue(bytes.fromhex(body).isascii())
        for reference, path in ((True, self.reference), (False, self.native)):
            self.calls.clear()
            run = self.run_process(
                reference, ["--machine", "--db", str(path), "notify", "run"]
            )
            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertEqual(self.calls, [])
        self.assertEqual(self.server_errors, [])

    def test_delivery_failure_reports_preserve_independent_outcomes(self):
        self.server()
        self.seed_consumer()
        self.seed_notification()
        self.consumer_status = 401
        self.notify_status = 503
        observations = []
        for reference, path in ((True, self.reference), (False, self.native)):
            self.active_database = path
            process = self.start(reference)
            first = self.line(process)
            data = json.loads(first)
            self.assertEqual(data["exit_code"], 3)
            self.assertEqual(data["data"]["deliveries"][0]["state"], "repair")
            self.assertEqual(
                data["data"]["notifications"]["deliveries"][0]["state"], "retry"
            )
            observations.append((first, self.stop(process)))
        self.assertEqual(*observations)
        self.assertEqual(self.snapshot(self.reference), self.snapshot(self.native))
        self.assertEqual(self.server_errors, [])

    def test_tick_error_is_terminal_and_preserves_error_channels(self):
        for mode in ([], ["--json"], ["--machine"]):
            self.compare(
                "CW-UNKNOWN-PROFILE",
                mode + ["--profile", "missing", "consumer", "watch"],
            )
            observations = []
            for reference, path in ((True, self.reference), (False, self.native)):
                with open(str(path) + ".lock", "a+b") as lock:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    result = self.run_process(
                        reference, mode + ["--db", str(path), "consumer", "watch"]
                    )
                    self.assertEqual(result.returncode, 2)
                    observations.append((result.stdout, result.stderr))
            self.assertEqual(*observations)

    def interrupt_and_restart(self, consumer):
        self.server()
        if consumer:
            self.seed_consumer()
        else:
            self.seed_notification()
        outcomes = []
        for reference, path in ((True, self.reference), (False, self.native)):
            self.active_database = path
            self.block_route = "/Items/1/Refresh" if consumer else "/notify"
            self.entered.clear()
            self.closed.clear()
            self.calls.clear()
            process = self.start(reference)
            self.assertTrue(self.entered.wait(10), self.server_errors)
            outcomes.append(self.stop(process))
            self.assertTrue(
                self.closed.wait(5), "interrupted network helper remained alive"
            )
            self.assert_writer_available(path)
            table = "consumer_deliveries" if consumer else "notification_deliveries"
            with closing(sqlite3.connect(path)) as db, db:
                state, token = db.execute(
                    f"SELECT state,lease_token FROM {table}"
                ).fetchone()
                self.assertEqual(state, "leased")
                self.assertTrue(token)
                # Synthetic expiry, without waiting 60 seconds in the test.
                db.execute(f"UPDATE {table} SET lease_until=0")
            self.block_route = None
            restarted = self.start(reference)
            line = self.line(restarted)
            report = json.loads(line)["data"]
            self.assertTrue(report["complete"])
            self.assertTrue(report["notifications"]["complete"])
            self.stop(restarted)
        self.assertEqual(*outcomes)
        self.assertEqual(self.snapshot(self.reference), self.snapshot(self.native))
        self.assertEqual(self.server_errors, [])

    def test_consumer_inflight_cancellation_and_restart(self):
        self.interrupt_and_restart(consumer=True)

    def test_notification_inflight_cancellation_and_restart(self):
        self.interrupt_and_restart(consumer=False)

    def test_native_sigterm_during_idle_releases_writer(self):
        outcomes = []
        for reference in (True, False):
            process = self.start(reference, interval=60)
            self.line(process)
            outcomes.append(self.stop(process, signal.SIGTERM))
        self.assertEqual(outcomes, [("", ""), ("", "")])
        self.assert_writer_available(self.native)

    def native_inflight_sigterm(self, consumer):
        self.server()
        if consumer:
            self.seed_consumer()
        else:
            self.seed_notification()
        self.active_database = self.native
        self.block_route = "/Items/1/Refresh" if consumer else "/notify"
        process = self.start(False, interval=60)
        self.assertTrue(self.entered.wait(10), self.server_errors)
        # Server readiness proves this tick is blocked in real loopback I/O,
        # rather than the supervisor already having completed its tick.
        # Exactly the tick and its parent-death guardian (the runtime's
        # lifeline supervisor, which leads the tick's process group).
        children = process_tree.children(process.pid)
        self.assertEqual(len(children), 2, children)
        guardians = [c for c in children if c[1] == process_tree.GUARDIAN]
        ticks = [c for c in children if "__legacy-consumer-watch-tick" in c[1]]
        self.assertEqual((len(guardians), len(ticks)), (1, 1), children)
        tick_pid = ticks[0][0]
        self.assert_writer_available(self.native)
        self.assertEqual(self.stop(process, signal.SIGTERM), ("", ""))
        self.assertTrue(self.closed.wait(5), "SIGTERM left the tick socket open")
        # Both are gone and reaped (not even zombies) at CLI return.
        self.assertEqual(
            process_tree.present([tick_pid, guardians[0][0]]),
            [],
            "tick or guardian still exists, possibly as a zombie",
        )
        self.assert_writer_available(self.native)
        table = "consumer_deliveries" if consumer else "notification_deliveries"
        with closing(sqlite3.connect(self.native)) as db:
            lease = db.execute(
                f"SELECT state,lease_token,lease_until FROM {table}"
            ).fetchone()
        self.assertEqual(lease[0], "leased")
        self.assertTrue(lease[1])
        self.assertGreater(lease[2], time.time())
        self.block_route = None
        self.calls.clear()
        deferred = self.start(False, interval=60)
        report = json.loads(self.line(deferred))["data"]
        self.assertFalse(
            report["complete"] if consumer else report["notifications"]["complete"]
        )
        self.stop(deferred)
        self.assertEqual(self.calls, [], "restart replayed an unexpired lease")
        with closing(sqlite3.connect(self.native)) as db, db:
            current = db.execute(
                f"SELECT state,lease_token,lease_until FROM {table}"
            ).fetchone()
            self.assertEqual(current, lease)
            # A committed write demonstrates writer reuse. Synthetic expiry
            # then exercises the existing owner's restart protocol promptly.
            db.execute(f"UPDATE {table} SET lease_until=0")
        restarted = self.start(False, interval=60)
        report = json.loads(self.line(restarted))["data"]
        self.assertTrue(report["complete"])
        self.assertTrue(report["notifications"]["complete"])
        self.stop(restarted)
        self.assertEqual(len(self.calls), 3 if consumer else 1)
        with closing(sqlite3.connect(self.native)) as db:
            current = db.execute(
                f"SELECT state,lease_token,lease_until FROM {table}"
            ).fetchone()
        self.assertEqual(current, ("accepted" if consumer else "complete", None, None))
        self.assert_writer_available(self.native)
        self.assertEqual(self.server_errors, [])

    def test_native_consumer_inflight_sigterm_reaps_tick_and_restarts(self):
        self.native_inflight_sigterm(consumer=True)

    def test_native_notification_inflight_sigterm_reaps_tick_and_restarts(self):
        self.native_inflight_sigterm(consumer=False)
