# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Preserved API-worker transport on empty disposable catalogs only.

No accounts, grants, tokens, recipients, pending work or network listeners are
created. The owner has no work to render or deliver in these fixtures.
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


class LegacyApiWorker(unittest.TestCase):
    def setUp(self):
        self.assertTrue(BINARY.is_file(), "build catabolic-legacy-native first")
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.env = environment(overrides={"HOME": str(self.root / "home")})
        # No optional tool probes are needed by an empty catalog. The known
        # interpreter and test binaries use absolute paths; child media tools
        # are deliberately absent from PATH in both engines.
        self.env["PATH"] = str(self.root / "empty-path")
        self.reference = self.root / "reference.db"
        self.native = self.root / "native.db"
        initialized = self.run_process(True, ["--db", str(self.reference), "init"])
        self.assertEqual(initialized.returncode, 0, initialized.stderr)
        shutil.copyfile(self.reference, self.native)

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

    def compare(self, scenario, args, database=True):
        oracle = self.run_process(
            True, (["--db", str(self.reference)] if database else []) + args
        )
        native = self.run_process(
            False, (["--db", str(self.native)] if database else []) + args
        )
        self.assertEqual(
            (native.returncode, native.stdout, native.stderr),
            (oracle.returncode, oracle.stdout, oracle.stderr),
            scenario,
        )
        return native

    def test_empty_once_exact_output(self):
        for index, prefix in enumerate(([], ["--json"], ["--machine"])):
            result = self.compare(
                "LC-API-WORKER-ONCE-" + str(index), prefix + ["api", "worker", "--once"]
            )
            self.assertEqual(result.returncode, 0)
        for path in (self.reference, self.native):
            with closing(sqlite3.connect(path)) as db:
                self.assertEqual(
                    db.execute("SELECT * FROM api_worker_status").fetchall(), []
                )
                for table in (
                    "api_principals",
                    "api_grants",
                    "api_tokens",
                    "api_private_files",
                ):
                    self.assertEqual(
                        db.execute("SELECT count(*) FROM " + table).fetchone()[0], 0
                    )

    def test_missing_database_and_max_render_jobs(self):
        for prefix in ([], ["--json"], ["--machine"]):
            result = self.compare(
                "LC-API-WORKER-NO-DB",
                prefix + ["api", "worker", "--once"],
                database=False,
            )
            self.assertEqual(result.returncode, 2)
        invalid = self.compare(
            "LC-API-WORKER-JOBS-2",
            ["api", "worker", "--max-render-jobs", "2", "--once"],
        )
        self.assertEqual(invalid.returncode, 2)
        accepted = self.compare(
            "LC-API-WORKER-JOBS-1",
            ["--json", "api", "worker", "--max-render-jobs", "1", "--once"],
        )
        self.assertEqual(accepted.returncode, 0)

    def test_public_server_stays_refused(self):
        result = self.run_process(
            False, ["--db", str(self.native), "--machine", "api", "serve"]
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn(
            "native HTTP is unqualified",
            json.loads(result.stdout)["data"]["error"]["message"],
        )
        self.assertEqual(result.stderr, "")

    def test_busy_once_exact_output(self):
        with (
            Path(str(self.reference) + ".lock").open("a") as reference_lock,
            Path(str(self.native) + ".lock").open("a") as native_lock,
        ):
            fcntl.flock(reference_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            fcntl.flock(native_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            for index, prefix in enumerate(([], ["--json"], ["--machine"])):
                result = self.compare(
                    "LC-API-WORKER-BUSY-" + str(index),
                    prefix + ["api", "worker", "--once"],
                )
                self.assertEqual(result.returncode, 3)

    def interrupted(self, reference, prefix, signum):
        path = self.reference if reference else self.native
        child = subprocess.Popen(
            self.command(reference) + ["--db", str(path)] + prefix + ["api", "worker"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=self.env,
        )
        try:
            deadline = time.monotonic() + 15
            while True:
                with closing(sqlite3.connect(path, timeout=1)) as db:
                    ready = db.execute(
                        "SELECT count(*) FROM api_worker_status"
                    ).fetchone()[0]
                if ready:
                    break
                self.assertIsNone(
                    child.poll(),
                    child.communicate() if child.poll() is not None else "",
                )
                self.assertLess(
                    time.monotonic(), deadline, "worker heartbeat not observed"
                )
                time.sleep(0.02)
            child.send_signal(signum)
            stdout, stderr = child.communicate(timeout=20)
        finally:
            if child.poll() is None:
                child.kill()
                child.communicate()
        with closing(sqlite3.connect(path)) as db:
            self.assertEqual(
                db.execute("SELECT * FROM api_worker_status").fetchall(), []
            )
        return child.returncode, stdout, stderr

    def test_interrupt_exact_output_and_cleanup(self):
        for prefix in ([], ["--json"], ["--machine"]):
            with self.subTest(prefix=prefix):
                oracle = self.interrupted(True, prefix, signal.SIGINT)
                native = self.interrupted(False, prefix, signal.SIGINT)
                self.assertEqual(native, oracle)
                self.assertEqual(native[0], 130)
        # The native supervisor explicitly treats SIGTERM as graceful interrupt.
        # Frozen Python SIGTERM is not handled, so this is native-only evidence.
        native = self.interrupted(False, ["--machine"], signal.SIGTERM)
        self.assertEqual(native[0], 130)
        self.assertEqual(
            json.loads(native[1])["data"]["error"]["type"], "KeyboardInterrupt"
        )


if __name__ == "__main__":
    unittest.main()
