# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Independent worker-envelope/lifecycle probes on empty disposable catalogs.

These exercise only the saved supervisor adapter. No recipients, network
listeners, accounts, tokens, grants, render jobs or source files are created.
"""

import fcntl
import json
import shutil
import signal
import sqlite3
import subprocess
import tempfile
import time
import unittest
from contextlib import closing
from pathlib import Path

from tests.test_rust_legacy_cli import BINARY, ORACLE
from tests.test_rust_legacy_parser import PYTHON, REFERENCE, environment


class WorkerAdapterReview(unittest.TestCase):
    def setUp(self):
        self.assertTrue(BINARY.is_file())
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.env = environment(
            overrides={"HOME": str(self.root), "PATH": str(self.root / "empty-path")}
        )
        self.paths = {
            "reference": self.root / "reference.db",
            "native": self.root / "native.db",
        }
        self.commands = {
            "reference": [str(PYTHON), "-c", ORACLE, str(REFERENCE / "src")],
            "native": [str(BINARY)],
        }
        created = self.run_cli("reference", ["init"])
        self.assertEqual(created.returncode, 0, created.stderr)
        with closing(sqlite3.connect(self.paths["reference"])) as db:
            db.execute("INSERT INTO profiles VALUES ('alternate-雪')")
            db.commit()
        shutil.copyfile(self.paths["reference"], self.paths["native"])

    def run_cli(self, engine, arguments, *, selected=True, env=None):
        return subprocess.run(
            self.commands[engine]
            + (["--db", str(self.paths[engine])] if selected else [])
            + arguments,
            env=self.env if env is None else env,
            text=True,
            capture_output=True,
            timeout=30,
        )

    def result(self, run):
        value = (run.returncode, run.stdout, run.stderr)
        return tuple(
            text.replace(str(self.paths["reference"]), "<DB>").replace(
                str(self.paths["native"]), "<DB>"
            )
            if isinstance(text, str)
            else text
            for text in value
        )

    def heartbeat(self, engine):
        with closing(sqlite3.connect(self.paths[engine], timeout=1)) as db:
            return db.execute(
                "SELECT profile,worker_pid FROM api_worker_status"
            ).fetchall()

    def start_worker(self, engine, profile="default"):
        child = subprocess.Popen(
            self.commands[engine]
            + [
                "--db",
                str(self.paths[engine]),
                "--profile",
                profile,
                "--machine",
                "api",
                "worker",
            ],
            env=self.env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.addCleanup(self.stop_child, child)
        deadline = time.monotonic() + 15
        while True:
            if self.heartbeat(engine) == [(profile, child.pid)]:
                return child
            self.assertIsNone(child.poll(), "worker exited before its heartbeat")
            self.assertLess(time.monotonic(), deadline, "worker heartbeat timeout")
            time.sleep(0.01)

    @staticmethod
    def stop_child(child):
        if child.poll() is None:
            child.kill()
        child.communicate()

    def interrupt(self, child):
        child.send_signal(signal.SIGINT)
        out, err = child.communicate(timeout=20)
        self.assertEqual(child.returncode, 130)
        self.assertEqual(err, "")
        value = json.loads(out)
        self.assertEqual(value["data"]["error"]["type"], "KeyboardInterrupt")
        return child.returncode, out, err

    def test_profile_envelopes_environment_and_error_reuse(self):
        for prefix in ([], ["--json"], ["--machine"], ["--json", "--machine"]):
            with self.subTest(prefix=prefix):
                results = []
                for engine in self.paths:
                    run = self.run_cli(
                        engine,
                        prefix + ["--profile", "missing-雪", "api", "worker", "--once"],
                    )
                    self.assertEqual(run.returncode, 2)
                    self.assertEqual(self.heartbeat(engine), [])
                    results.append(self.result(run))
                self.assertEqual(*results)
        results = []
        for engine, path in self.paths.items():
            env = dict(
                self.env, CATABOLIC_DB=str(path), CATABOLIC_PROFILE="alternate-雪"
            )
            run = self.run_cli(
                engine,
                ["--machine", "api", "worker", "--once"],
                selected=False,
                env=env,
            )
            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertEqual(json.loads(run.stdout)["profile"], "alternate-雪")
            self.assertEqual(self.heartbeat(engine), [])
            results.append(self.result(run))
        self.assertEqual(*results)

    def test_singleton_interrupt_and_immediate_restart(self):
        observations = []
        for engine in self.paths:
            child = self.start_worker(engine, "alternate-雪")
            competing = []
            for prefix in ([], ["--json"], ["--machine"]):
                run = self.run_cli(engine, prefix + ["api", "worker", "--once"])
                self.assertEqual(run.returncode, 2)
                self.assertIn("another Catabolic writer", run.stdout + run.stderr)
                competing.append(self.result(run))
            interrupted = self.interrupt(child)
            self.assertEqual(self.heartbeat(engine), [])
            restarted = self.run_cli(
                engine,
                ["--profile", "alternate-雪", "--machine", "api", "worker", "--once"],
            )
            self.assertEqual(restarted.returncode, 0, restarted.stderr)
            self.assertEqual(self.heartbeat(engine), [])
            observations.append((competing, interrupted, self.result(restarted)))
        self.assertEqual(*observations)

    def test_singleton_precedes_invalid_profile_and_releases_after_errors(self):
        observations = []
        for engine in self.paths:
            child = self.start_worker(engine, "alternate-雪")
            competing = []
            for prefix in ([], ["--json"], ["--machine"], ["--json", "--machine"]):
                run = self.run_cli(
                    engine,
                    prefix + ["--profile", "missing-雪", "api", "worker", "--once"],
                )
                self.assertEqual(run.returncode, 2)
                self.assertIn("another Catabolic writer", run.stdout + run.stderr)
                self.assertNotIn("unknown profile", run.stdout + run.stderr)
                if prefix:
                    value = json.loads(
                        run.stdout if "--machine" in prefix else run.stderr
                    )
                    error = (
                        value["data"]["error"]
                        if "--machine" in prefix
                        else value["error"]
                    )
                    self.assertEqual(error["type"], "CatalogBusy")
                self.assertEqual(self.heartbeat(engine), [("alternate-雪", child.pid)])
                competing.append(self.result(run))
            interrupted = self.interrupt(child)
            self.assertEqual(self.heartbeat(engine), [])
            failed = []
            restarted = []
            for prefix in ([], ["--json"], ["--machine"], ["--json", "--machine"]):
                run = self.run_cli(
                    engine,
                    prefix + ["--profile", "missing-雪", "api", "worker", "--once"],
                )
                self.assertEqual(run.returncode, 2)
                self.assertIn("unknown profile:", run.stdout + run.stderr)
                if prefix:
                    value = json.loads(
                        run.stdout if "--machine" in prefix else run.stderr
                    )
                    error = (
                        value["data"]["error"]
                        if "--machine" in prefix
                        else value["error"]
                    )
                    self.assertEqual(
                        error,
                        {
                            "message": "unknown profile: missing-雪",
                            "type": "CatabolicError",
                        },
                    )
                self.assertEqual(self.heartbeat(engine), [])
                failed.append(self.result(run))
                rerun = self.run_cli(
                    engine,
                    prefix + ["--profile", "alternate-雪", "api", "worker", "--once"],
                )
                self.assertEqual(rerun.returncode, 0, rerun.stderr)
                self.assertEqual(self.heartbeat(engine), [])
                restarted.append(self.result(rerun))
            observations.append((competing, interrupted, failed, restarted))
        self.assertEqual(*observations)

    def test_catalog_writer_precedes_invalid_profile(self):
        observations = []
        for engine, path in self.paths.items():
            busy = []
            with Path(str(path) + ".lock").open("a") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                for prefix in ([], ["--json"], ["--machine"]):
                    run = self.run_cli(
                        engine,
                        prefix + ["--profile", "missing-雪", "api", "worker", "--once"],
                    )
                    self.assertEqual(run.returncode, 3)
                    self.assertIn("catalog_busy", run.stdout)
                    self.assertNotIn("unknown profile", run.stdout + run.stderr)
                    self.assertEqual(self.heartbeat(engine), [])
                    busy.append(self.result(run))
            failed = self.run_cli(
                engine,
                ["--machine", "--profile", "missing-雪", "api", "worker", "--once"],
            )
            self.assertEqual(failed.returncode, 2)
            self.assertEqual(
                json.loads(failed.stdout)["data"]["error"],
                {"message": "unknown profile: missing-雪", "type": "CatabolicError"},
            )
            self.assertEqual(self.heartbeat(engine), [])
            restarted = self.run_cli(engine, ["--machine", "api", "worker", "--once"])
            self.assertEqual(restarted.returncode, 0, restarted.stderr)
            self.assertEqual(self.heartbeat(engine), [])
            observations.append((busy, self.result(failed), self.result(restarted)))
        self.assertEqual(*observations)

    def test_missing_database_precedes_invalid_profile(self):
        for prefix in ([], ["--json"], ["--machine"]):
            with self.subTest(prefix=prefix):
                results = []
                for engine in self.paths:
                    run = self.run_cli(
                        engine,
                        prefix + ["--profile", "missing-雪", "api", "worker", "--once"],
                        selected=False,
                    )
                    self.assertEqual(run.returncode, 2)
                    self.assertIn(
                        "select --db or CATABOLIC_DB", run.stdout + run.stderr
                    )
                    self.assertNotIn("unknown profile", run.stdout + run.stderr)
                    self.assertEqual(self.heartbeat(engine), [])
                    results.append(self.result(run))
                self.assertEqual(*results)

    def test_missing_catalog_path_precedes_singleton_and_invalid_profile(self):
        observations = []
        for engine, path in self.paths.items():
            missing = Path(str(path) + ".missing")
            rejected = []
            with Path(str(missing) + ".api-worker.lock").open("a") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                for prefix in ([], ["--json"], ["--machine"]):
                    run = self.run_cli(
                        engine,
                        ["--db", str(missing)]
                        + prefix
                        + ["--profile", "missing-雪", "api", "worker", "--once"],
                        selected=False,
                    )
                    self.assertEqual(run.returncode, 2)
                    self.assertIn("database does not exist", run.stdout + run.stderr)
                    self.assertNotIn("unknown profile", run.stdout + run.stderr)
                    self.assertNotIn(
                        "another Catabolic writer", run.stdout + run.stderr
                    )
                    self.assertFalse(missing.exists())
                    self.assertEqual(self.heartbeat(engine), [])
                    rejected.append(self.result(run))
            observations.append(rejected)
        self.assertEqual(*observations)

    def test_interrupt_during_external_catalog_lock_preserves_best_effort_cleanup(self):
        observations = []
        for engine, path in self.paths.items():
            child = self.start_worker(engine)
            with Path(str(path) + ".lock").open("a") as lock:
                deadline = time.monotonic() + 10
                while True:
                    try:
                        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                        break
                    except BlockingIOError:
                        self.assertLess(
                            time.monotonic(),
                            deadline,
                            "worker never released short catalog ownership",
                        )
                        time.sleep(0.01)
                observations.append(self.interrupt(child))
                # Both saved owners intentionally skip heartbeat removal when
                # the external writer prevents cleanup. The supervisor lease
                # must nevertheless be released, and no live worker remains.
                self.assertEqual(self.heartbeat(engine), [("default", child.pid)])
            rerun = self.run_cli(engine, ["--machine", "api", "worker", "--once"])
            self.assertEqual(rerun.returncode, 0, rerun.stderr)
            self.assertEqual(self.heartbeat(engine), [])
        self.assertEqual(*observations)


if __name__ == "__main__":
    unittest.main()
