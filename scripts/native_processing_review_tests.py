#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Independent installed processing bridge checks; disposable fixtures only.

Run from outside the checkout with isolated Python -I and the reviewed wheel.
This supplemental suite is not final helper/extension or platform qualification.
"""

import importlib.util
import os
import signal
import threading
import time
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "processing_fixture",
    Path(__file__).with_name("native_processing_embedding_tests.py"),
)
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


class InstalledProcessingReview(unittest.TestCase):
    setUp = fixture.InstalledProcessing.setUp

    def interrupt_exactly(self, mode):
        marker = self.root / "tool-pid"
        tools = self.root / "tools"
        tools.mkdir()
        tool = tools / ("ffmpeg" if mode == "artifact" else "ffprobe")
        passthrough = (
            'if [ "$1" = -version ]; then exec /usr/bin/ffprobe "$@"; fi\n'
            if mode == "processing"
            else 'if [ "$1" = -version ] || [ "$2" = -encoders ] || [ "$2" = -muxers ] || [ "$2" = -filters ]; then exec /usr/bin/ffmpeg "$@"; fi\n'
            if mode == "artifact"
            else ""
        )
        tool.write_text(
            "#!/bin/sh\n" + passthrough + f"echo $$ > '{marker}'\nexec /bin/sleep 20\n"
        )
        tool.chmod(0o700)
        old_path = os.environ["PATH"]
        os.environ["PATH"] = str(tools) + ":" + old_path
        self.addCleanup(os.environ.__setitem__, "PATH", old_path)
        if mode == "processing":
            queued = self.processing.enqueue("probe", file_ids=["tone"])["queued"]

            def action():
                return self.processing.run(job_ids=queued, _refresh=False)

        elif mode == "artifact":
            self.artifacts.bind("generated", self.root / "generated")
            recipe = self.artifacts.recipe("flac", "audio-flac", {"reserve_bytes": 0})
            queued = self.artifacts.enqueue("tone", recipe["id"], "generated", "item")

            def action():
                return self.artifacts.run(job_ids=[queued["job_id"]], _refresh=False)

        else:

            def action():
                return self.processing.enqueue("probe", file_ids=["tone"])

        class ExactSignal(BaseException):
            pass

        expected = ExactSignal("exact independent signal object", mode)
        expected.add_note("retained exception notes")

        def handler(*_):
            raise expected

        previous = signal.signal(signal.SIGUSR1, handler)
        self.addCleanup(signal.signal, signal.SIGUSR1, previous)
        errors = []
        wrote = threading.Event()

        def interrupt_after_write():
            deadline = time.monotonic() + 10
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            if not marker.exists():
                errors.append("fixture tool never started")
                return
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                try:
                    with self.Store(self.path, writable=True) as writer:
                        with writer.transaction() as db:
                            db.execute(
                                "INSERT INTO profiles VALUES ('writer-progress')"
                            )
                    wrote.set()
                    break
                except self.Error:
                    time.sleep(0.005)
            if not wrote.is_set():
                errors.append("independent writer did not commit during tool wait")
            os.kill(os.getpid(), signal.SIGUSR1)

        interrupter = threading.Thread(target=interrupt_after_write)
        interrupter.start()
        began = time.monotonic()
        try:
            with self.assertRaises(ExactSignal) as raised:
                action()
        finally:
            interrupter.join(12)
        self.assertFalse(interrupter.is_alive())
        self.assertEqual(errors, [])
        self.assertTrue(wrote.is_set())
        self.assertIs(raised.exception, expected)
        self.assertEqual(
            raised.exception.args, ("exact independent signal object", mode)
        )
        self.assertEqual(raised.exception.__notes__, ["retained exception notes"])
        self.assertIs(signal.getsignal(signal.SIGUSR1), handler)
        self.assertLess(time.monotonic() - began, 10)
        with self.assertRaises(ProcessLookupError):
            os.kill(int(marker.read_text()), 0)
        self.assertEqual(
            self.store.db.execute(
                "SELECT count(*) FROM profiles WHERE id='writer-progress'"
            ).fetchone()[0],
            1,
        )
        self.assertEqual(
            self.store.db.execute(
                "SELECT count(*) FROM execution_claims WHERE lease_until>unixepoch()"
            ).fetchone()[0],
            0,
        )
        # Prove the borrowed owner can still start and roll back its own scope.
        with self.assertRaisesRegex(ValueError, "rollback after cancellation"):
            with self.store.transaction() as db:
                db.execute("INSERT INTO profiles VALUES ('must-roll-back')")
                raise ValueError("rollback after cancellation")
        self.assertEqual(
            self.store.db.execute(
                "SELECT count(*) FROM profiles WHERE id='must-roll-back'"
            ).fetchone()[0],
            0,
        )

    def test_admission_preserves_exact_exception_and_writer_progress(self):
        self.interrupt_exactly("admission")

    def test_processing_preserves_exact_exception_and_writer_progress(self):
        self.interrupt_exactly("processing")

    def test_artifact_preserves_exact_exception_and_writer_progress(self):
        self.interrupt_exactly("artifact")


if __name__ == "__main__":
    if not __debug__:
        raise SystemExit(
            "processing review requires assertions; do not run with -O or -OO"
        )
    unittest.main(verbosity=2)
