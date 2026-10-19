# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Paired worker transport/lifecycle fixtures; M6 refusals are not parity.

All catalogs are disposable. No source/output bindings, media, credentials or
network services exist. Seeds use the frozen owners; execution uses real CLIs.
"""

import fcntl
import json
import os
import select
import shutil
import signal
import sqlite3
import subprocess
import tempfile
import time
import unittest
from contextlib import closing, contextmanager
from pathlib import Path

from tests.test_rust_legacy_cli import BINARY, ORACLE
from tests.test_rust_legacy_parser import PYTHON, REFERENCE, environment

GATE = (
    "native fallback publication is unqualified; only disposable debug fixtures "
    "may apply or recover"
)
SEED = r"""
import sys
sys.path.insert(0, sys.argv[1])
from catabolic.app import Application
from catabolic.fallback_policies import Policies
from catabolic.fallback_projection import FallbackProjection
from catabolic.fallback_worker import configure
from catabolic.saved_queries import Queries
from catabolic.store import Store
with Store(sys.argv[2], writable=True) as store:
    app = Application(store)
    store.db.execute("INSERT INTO profiles VALUES ('alternate')")
    store.db.execute("INSERT INTO meta VALUES ('layout:flat',?)", ('{"version":1,"rules":[{"name":"all","path":"{item.kind}/{item.id}/{file.name}"}]}',))
    query = Queries(store).put('members', {'selection': {'language':'sql', 'query':'SELECT id AS item_id FROM items WHERE 0'}})['id']
    candidates = Queries(store).put('candidates', {'selection': {'language':'sql', 'query':'SELECT id AS file_id FROM files WHERE 0'}})['id']
    policy = Policies(store).put('policy', {'fallbacks':[{'name':'empty','query_id':candidates}]})['id']
    for index in range(int(sys.argv[3])):
        catalog = f'catalog-{index:02}'
        store.db.execute('INSERT INTO catalogs VALUES (?)', (catalog,))
        FallbackProjection(app).bind(catalog, policy, query, 'flat')
        configure(app, catalog, enabled=True, interval=60)
"""


@contextmanager
def held_lock(path, suffix=".lock"):
    with Path(str(path) + suffix).open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield lock


class LegacyFallbackWorker(unittest.TestCase):
    def setUp(self):
        self.assertTrue(BINARY.is_file(), "build catabolic-legacy-native first")
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.env = environment(overrides={"HOME": str(self.root / "home")})
        self.env["PATH"] = str(self.root / "empty-path")
        self.reference = self.root / "reference.db"
        self.native = self.root / "native.db"
        result = self.run_process(True, ["--db", str(self.reference), "init"])
        self.assertEqual(result.returncode, 0, result.stderr)
        shutil.copyfile(self.reference, self.native)
        self.empty = self.root / "empty.db"
        shutil.copyfile(self.reference, self.empty)

    def command(self, reference):
        return (
            [str(PYTHON), "-c", ORACLE, str(REFERENCE / "src")]
            if reference
            else [str(BINARY)]
        )

    def run_process(self, reference, args):
        return subprocess.run(
            self.command(reference) + args,
            capture_output=True,
            text=True,
            env=self.env,
            timeout=30,
        )

    def once(self, reference, prefix=()):
        path = self.reference if reference else self.native
        return self.run_process(
            reference, ["--db", str(path), *prefix, "fallback", "worker", "--once"]
        )

    def compare(self, prefix=()):
        oracle, native = self.once(True, prefix), self.once(False, prefix)
        self.assertEqual(
            (native.returncode, native.stdout, native.stderr),
            (oracle.returncode, oracle.stdout, oracle.stderr),
        )
        return native

    def seed(self, count=1):
        shutil.copyfile(self.empty, self.reference)
        result = subprocess.run(
            [
                str(PYTHON),
                "-c",
                SEED,
                str(REFERENCE / "src"),
                str(self.reference),
                str(count),
            ],
            env=self.env,
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        shutil.copyfile(self.reference, self.native)

    def rows(self, path, sql, parameters=()):
        with closing(sqlite3.connect(path)) as db:
            return db.execute(sql, parameters).fetchall()

    def execute(self, path, sql):
        with closing(sqlite3.connect(path)) as db:
            db.executescript(sql)
            db.commit()

    def test_empty_once_exact_channels(self):
        for prefix in ((), ("--json",), ("--machine",)):
            self.assertEqual(self.compare(prefix).returncode, 0)

    def test_admission_and_profile_errors_exact_channels(self):
        for prefix in ((), ("--json",), ("--machine",)):
            self.assertEqual(
                self.compare((*prefix, "--profile", "missing")).returncode, 2
            )
            oracle = self.run_process(True, [*prefix, "fallback", "worker", "--once"])
            native = self.run_process(False, [*prefix, "fallback", "worker", "--once"])
            self.assertEqual(
                (native.returncode, native.stdout, native.stderr),
                (oracle.returncode, oracle.stdout, oracle.stderr),
            )
            self.assertEqual(native.returncode, 2)
        for path in (self.reference, self.native):
            with held_lock(path, ".fallback-worker.lock"):
                pass

    def test_catalog_contention_is_incomplete_even_for_unknown_profile(self):
        with held_lock(self.reference), held_lock(self.native):
            for prefix in ((), ("--json",), ("--machine",)):
                for profile in ("default", "missing"):
                    result = self.compare((*prefix, "--profile", profile))
                    self.assertEqual(result.returncode, 3)

    def test_singleton_contention_is_terminal_before_profile(self):
        with (
            held_lock(self.reference, ".fallback-worker.lock"),
            held_lock(self.native, ".fallback-worker.lock"),
        ):
            for prefix in ((), ("--json",), ("--machine",)):
                for profile in ("default", "missing"):
                    result = self.compare((*prefix, "--profile", profile))
                    self.assertEqual(result.returncode, 2)
                    if prefix == ("--machine",):
                        self.assertEqual(
                            json.loads(result.stdout)["data"]["error"]["type"],
                            "CatalogBusy",
                        )

    def test_sqlite_exclusive_contention_is_incomplete(self):
        with (
            closing(sqlite3.connect(self.reference)) as oracle,
            closing(sqlite3.connect(self.native)) as native,
        ):
            for connection in (oracle, native):
                connection.execute("PRAGMA journal_mode=DELETE")
                connection.execute("BEGIN EXCLUSIVE")
            self.assertEqual(self.compare(("--machine",)).returncode, 3)
        self.assertEqual(self.compare(("--machine",)).returncode, 0)

    def test_disabled_future_and_watcher_owned_skip_exact(self):
        self.seed(3)
        for path in (self.reference, self.native):
            self.execute(
                path,
                """
                UPDATE fallback_bindings SET enabled=0 WHERE catalog='catalog-00';
                UPDATE fallback_bindings SET next_attempt=9999999999 WHERE catalog='catalog-01';
                INSERT INTO watcher_definitions VALUES ('watch-definition','default','watcher',1,'{}','digest',0);
                INSERT INTO watchers(profile,name,definition_id) VALUES ('default','watcher','watch-definition');
                INSERT INTO watcher_owners VALUES ('default','catalog-02','watcher');
            """,
            )
        for prefix in ((), ("--json",), ("--machine",)):
            self.assertEqual(self.compare(prefix).returncode, 0)
        self.assertEqual(
            self.rows(
                self.native,
                "SELECT catalog,next_attempt FROM fallback_bindings ORDER BY catalog",
            ),
            [("catalog-00", 0), ("catalog-01", 9999999999), ("catalog-02", 0)],
        )

    def test_due_selection_backoff_and_explicit_gate_difference(self):
        self.seed(12)
        before = time.time()
        oracle, native = (
            self.once(True, ("--machine",)),
            self.once(False, ("--machine",)),
        )
        self.assertEqual((oracle.returncode, native.returncode), (3, 3))
        self.assertEqual((oracle.stderr, native.stderr), ("", ""))
        old, new = json.loads(oracle.stdout)["data"], json.loads(native.stdout)["data"]
        self.assertEqual((old["processed"], new["processed"]), (10, 10))
        for report in (old, new):
            self.assertFalse(report["complete"])
            self.assertEqual(
                [row["catalog"] for row in report["projections"]],
                [f"catalog-{n:02}" for n in range(10)],
            )
        self.assertTrue(all(row["error"] == GATE for row in new["projections"]))
        # The frozen owner attempts the empty projection, advances generation,
        # and reports incomplete states. Native refuses before that operation.
        self.assertEqual(
            old["projections"],
            [
                {"catalog": f"catalog-{n:02}", "complete": False, "states": []}
                for n in range(10)
            ],
        )
        for reference, payload in ((True, old), (False, new)):
            expected = {
                "interface_version": 1,
                "command": "fallback",
                "operation": "worker",
                "profile": "default",
                "outcome": "incomplete",
                "exit_code": 3,
                "data": payload,
            }
            self.assertEqual(
                (oracle if reference else native).stdout, json.dumps(expected) + "\n"
            )
        self.assertEqual(
            self.rows(
                self.reference,
                "SELECT generation FROM fallback_bindings ORDER BY catalog",
            ),
            [(1,)] * 10 + [(0,)] * 2,
        )
        self.assertEqual(
            self.rows(
                self.native, "SELECT generation FROM fallback_bindings ORDER BY catalog"
            ),
            [(0,)] * 12,
        )
        for path in (self.reference, self.native):
            rows = self.rows(
                path,
                "SELECT catalog,next_attempt FROM fallback_bindings ORDER BY catalog",
            )
            for _, timestamp in rows[:10]:
                self.assertGreaterEqual(timestamp, before + 60)
                self.assertLessEqual(timestamp, time.time() + 60)
            self.assertEqual([timestamp for _, timestamp in rows[10:]], [0, 0])
            for table in (
                "journal",
                "owned_links",
                "fallback_entries",
                "fallback_history",
                "bindings",
            ):
                self.assertEqual(self.rows(path, "SELECT * FROM " + table), [])
        second = self.once(False, ("--json",))
        self.assertEqual(json.loads(second.stdout)["processed"], 2)
        self.assertEqual(
            json.loads(self.once(False, ("--json",)).stdout)["processed"], 0
        )

    def test_recovery_refusal_preserves_journal_and_backoff(self):
        self.seed()
        self.execute(
            self.native,
            "INSERT INTO journal(id,profile,catalog,path,kind) VALUES ('pending','default','catalog-00','fixture','create')",
        )
        before = time.time()
        native = self.once(False, ("--machine",))
        self.assertEqual(native.returncode, 3)
        result = json.loads(native.stdout)["data"]
        self.assertEqual(
            result["projections"],
            [{"catalog": "catalog-00", "complete": False, "error": GATE}],
        )
        self.assertEqual(
            self.rows(self.native, "SELECT id FROM journal"), [("pending",)]
        )
        self.assertGreaterEqual(
            self.rows(self.native, "SELECT next_attempt FROM fallback_bindings")[0][0],
            before + 60,
        )
        # The public parser has no private publication bypass.
        refused = self.run_process(
            False,
            [
                "--db",
                str(self.native),
                "fallback",
                "worker",
                "--once",
                "--disposable-fixture",
            ],
        )
        self.assertEqual(refused.returncode, 2)
        self.assertIn("unrecognized arguments", refused.stderr)

    def interrupt(self, reference, prefix=(), signum=signal.SIGINT):
        path = self.reference if reference else self.native
        read_fd, write_fd = os.pipe() if reference else (None, None)
        command = self.command(reference)
        if reference:
            command = [
                str(PYTHON),
                str(
                    Path(__file__).with_name("frozen_fallback_worker_signal_fixture.py")
                ),
                str(REFERENCE / "src"),
                str(write_fd),
            ]
        child = subprocess.Popen(
            command + ["--db", str(path), *prefix, "fallback", "worker"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=self.env,
            pass_fds=(write_fd,) if reference else (),
        )
        if write_fd is not None:
            os.close(write_fd)
        try:
            deadline = time.monotonic() + 15
            if reference:
                readable, _, _ = select.select([read_fd], [], [], 15)
                self.assertTrue(readable, "frozen idle boundary was not reached")
                self.assertEqual(json.loads(os.read(read_fd, 4096))["pid"], child.pid)
            else:
                while not self.rows(
                    path, "SELECT 1 FROM fallback_bindings WHERE next_attempt>0"
                ):
                    if child.poll() is not None:
                        self.fail(str(child.communicate()))
                    self.assertLess(
                        time.monotonic(), deadline, "native tick did not commit backoff"
                    )
                    time.sleep(0.01)
            # A real external writer succeeds while the worker keeps its singleton.
            while True:
                try:
                    with held_lock(path):
                        self.execute(
                            path, "INSERT INTO profiles VALUES ('writer-progress')"
                        )
                    break
                except BlockingIOError:
                    self.assertLess(
                        time.monotonic(), deadline, "tick writer was not released"
                    )
                    time.sleep(0.01)
            duplicate = self.once(reference, ("--machine", "--profile", "missing"))
            self.assertEqual(duplicate.returncode, 2)
            self.assertEqual(
                json.loads(duplicate.stdout)["data"]["error"]["type"], "CatalogBusy"
            )
            child.send_signal(signum)
            out, err = child.communicate(timeout=15)
            self.assertEqual(
                child.returncode,
                -signal.SIGTERM if signum == signal.SIGTERM else 130,
            )
            with held_lock(path), held_lock(path, ".fallback-worker.lock"):
                pass
            restart = self.once(reference, ("--json",))
            self.assertEqual(restart.returncode, 0, restart.stderr)
            self.assertEqual(
                json.loads(restart.stdout),
                {"processed": 0, "complete": True, "projections": []},
            )
            return child.returncode, out, err
        finally:
            if read_fd is not None:
                os.close(read_fd)
            if child.poll() is None:
                child.kill()
                child.communicate(timeout=10)

    def test_sigint_idle_writer_release_singleton_and_restart_exact(self):
        for prefix in ((), ("--json",), ("--machine",)):
            with self.subTest(prefix=prefix):
                # Each loop starts fresh; no retained rows can fake readiness.
                self.seed()
                self.assertEqual(
                    self.interrupt(True, prefix), self.interrupt(False, prefix)
                )

    def test_sigterm_matches_process_exit_and_releases_owners(self):
        for prefix in ((), ("--json",), ("--machine",)):
            with self.subTest(prefix=prefix):
                self.seed()
                for reference in (True, False):
                    self.assertEqual(
                        self.interrupt(reference, prefix, signal.SIGTERM),
                        (-signal.SIGTERM, "", ""),
                    )


if __name__ == "__main__":
    unittest.main()
