# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Signal-fixture regressions and explicitly selected frozen-only schedules.

The interrupted-close probe establishes one reproducible cleanup-miss schedule,
not the cause of any historical uninstrumented failure or native behavior.
"""

import fcntl
import os
import signal
import sqlite3
import subprocess
import tempfile
import unittest
from contextlib import closing
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from tests import test_rust_legacy_api_worker as fixture


class WorkerHeartbeatReadinessTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.path = Path(temporary.name) / "heartbeat.db"
        with closing(sqlite3.connect(self.path)) as db:
            db.execute(
                "CREATE TABLE api_worker_status(profile TEXT PRIMARY KEY, worker_pid INTEGER)"
            )

    def rows(self, rows):
        with closing(sqlite3.connect(self.path)) as db:
            db.execute("DELETE FROM api_worker_status")
            db.executemany("INSERT INTO api_worker_status VALUES (?,?)", rows)
            db.commit()

    def test_stale_other_profile_and_matching_heartbeat(self):
        for rows, expected in (
            ([], False),
            ([("default", 10)], False),
            ([("other-雪", 20)], False),
            ([("default", 10), ("other-雪", 20)], False),
            ([("default", 20)], True),
            ([("default", 20), ("other-雪", 10)], True),
        ):
            with self.subTest(rows=rows):
                self.rows(rows)
                self.assertEqual(
                    fixture.worker_heartbeat(self.path, "default", 20), expected
                )
        self.rows([("other-雪", 20)])
        self.assertTrue(fixture.worker_heartbeat(self.path, "other-雪", 20))

    def test_interrupted_does_not_signal_for_stale_or_other_profile_rows(self):
        # Exercise the actual interrupted() control flow, with a deterministic
        # fake child and mocked polling wait. An any-row readiness check sends
        # before advance() and fails this regression immediately.
        for rows in (
            [("default", 10)],
            [("other-雪", 20)],
            [("default", 10), ("other-雪", 20)],
        ):
            with self.subTest(rows=rows):
                self.rows(rows)
                child = SimpleNamespace(pid=20, returncode=None, advanced=False)

                def advance(_, child=child):
                    self.assertFalse(child.advanced)
                    child.advanced = True
                    self.rows([("default", child.pid)])

                def send_signal(signum, child=child):
                    self.assertTrue(child.advanced, "signalled for a foreign heartbeat")
                    self.assertEqual(signum, signal.SIGINT)
                    child.returncode = 130
                    self.rows([])

                child.poll = lambda child=child: child.returncode
                child.send_signal = send_signal
                child.communicate = lambda **kwargs: ("", "interrupted\n")
                child.kill = lambda child=child: setattr(child, "returncode", -9)
                case = fixture.LegacyApiWorker()
                case.native = self.path
                case.env = {}
                case.run_process = lambda *args: SimpleNamespace(
                    returncode=0, stdout="", stderr=""
                )
                with (
                    patch.object(fixture.subprocess, "Popen", return_value=child),
                    patch.object(fixture.time, "sleep", side_effect=advance) as wait,
                ):
                    self.assertEqual(
                        case.interrupted(False, [], signal.SIGINT),
                        (130, "", "interrupted\n"),
                    )
                self.assertEqual(wait.call_count, 1)


class FrozenWorkerSelectedSignalTests(unittest.TestCase):
    def setUp(self):
        self.case = fixture.LegacyApiWorker()
        self.case.setUp()
        self.addCleanup(self.case.doCleanups)

    def test_sigint_between_tick_database_close_and_writer_release(self):
        # This test deliberately selects a frozen-only cleanup-miss schedule.
        # It does not relax the idle-loop fixture's strict empty-row assertion.
        for prefix in ([], ["--json"], ["--machine"]):
            with self.subTest(prefix=prefix):
                expected = self.case.interrupted(True, prefix, signal.SIGINT)
                read_fd, write_fd = os.pipe()
                try:
                    child = subprocess.Popen(
                        fixture.selected_frozen_command("tick-close", write_fd)
                        + ["--db", str(self.case.reference)]
                        + prefix
                        + ["api", "worker"],
                        stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE,
                        text=True,
                        env=self.case.env,
                        pass_fds=(write_fd,),
                    )
                except BaseException:
                    os.close(read_fd)
                    raise
                finally:
                    os.close(write_fd)
                try:
                    self.assertEqual(
                        fixture.selected_boundary_event(read_fd, 15),
                        {
                            "boundary": "tick-close",
                            "pid": child.pid,
                            "profile": "default",
                        },
                    )
                    self.assertTrue(
                        fixture.worker_heartbeat(
                            self.case.reference, "default", child.pid
                        )
                    )
                    # The main tick still owns this writer lease. This probe
                    # never acquires or holds an external lock during SIGINT.
                    with Path(str(self.case.reference) + ".lock").open("a") as lock:
                        with self.assertRaises(BlockingIOError):
                            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    child.send_signal(signal.SIGINT)
                    out, err = child.communicate(timeout=20)
                finally:
                    os.close(read_fd)
                    if child.poll() is None:
                        child.kill()
                        child.communicate()
                self.assertEqual((child.returncode, out, err), expected)
                self.assertEqual(child.returncode, 130)
                with closing(sqlite3.connect(self.case.reference)) as db:
                    self.assertEqual(
                        db.execute(
                            "SELECT profile,worker_pid FROM api_worker_status"
                        ).fetchall(),
                        [("default", child.pid)],
                    )
                restarted = self.case.run_process(
                    True,
                    ["--db", str(self.case.reference)]
                    + prefix
                    + ["api", "worker", "--once"],
                )
                self.assertEqual(
                    restarted.returncode, 0, (restarted.stdout, restarted.stderr)
                )
                self.assertEqual(restarted.stderr, "")
                with closing(sqlite3.connect(self.case.reference)) as db:
                    self.assertEqual(
                        db.execute("SELECT * FROM api_worker_status").fetchall(), []
                    )


if __name__ == "__main__":
    unittest.main()
