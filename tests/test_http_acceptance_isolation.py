# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""The installed journey reaps its writer before non-replayable ticket calls."""

import fcntl
from pathlib import Path
import runpy
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock

STOP = runpy.run_path(
    str(Path(__file__).resolve().parents[1] / "scripts/http_acceptance.py")
)["stop_owned_worker"]


class InstalledHTTPIsolationTest(unittest.TestCase):
    def test_reaped_worker_releases_its_real_writer_lock(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lock = root / "writer.lock"
            ready = root / "ready"
            worker = subprocess.Popen(
                [
                    sys.executable,
                    "-c",
                    "import fcntl,signal,sys;from pathlib import Path;"
                    "f=open(sys.argv[1],'w');fcntl.flock(f,fcntl.LOCK_EX);"
                    "Path(sys.argv[2]).touch();signal.pause()",
                    str(lock),
                    str(ready),
                ]
            )
            try:
                deadline = time.monotonic() + 10
                while not ready.exists():
                    self.assertIsNone(worker.poll(), "fixture writer exited")
                    self.assertLess(time.monotonic(), deadline, "writer not ready")
                    time.sleep(0.01)
                with lock.open("r+") as other:
                    with self.assertRaises(BlockingIOError):
                        fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    receipt = STOP(worker)
                    self.assertTrue(receipt["reaped"])
                    self.assertEqual(receipt["returncode"], -signal.SIGTERM)
                    self.assertIsNotNone(worker.poll())
                    fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)
            finally:
                if worker.poll() is None:
                    worker.kill()
                worker.wait(timeout=10)

    def test_dead_failed_or_unreaped_worker_cannot_claim_isolation(self):
        dead = Mock(**{"poll.return_value": 1})
        with self.assertRaisesRegex(RuntimeError, "exited before"):
            STOP(dead)
        dead.terminate.assert_not_called()
        for failure in (1, subprocess.TimeoutExpired("fixture-worker", 10)):
            with self.subTest(failure=type(failure).__name__):
                worker = Mock(pid=123, **{"poll.return_value": None})
                if isinstance(failure, Exception):
                    worker.wait.side_effect = failure
                    expected = subprocess.TimeoutExpired
                else:
                    worker.wait.return_value = failure
                    expected = RuntimeError
                with self.assertRaises(expected):
                    STOP(worker)


if __name__ == "__main__":
    unittest.main()
