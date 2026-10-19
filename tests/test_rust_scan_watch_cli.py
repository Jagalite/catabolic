# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Top-level polling adapter pairs, not a qualification of delegated owners.

Disposable catalogs only. Required discovery asserts actual scan report facts
and the remaining freshness-floor and timing-dependent byte differences explicitly; opt-in full-report diagnostics fail separately and retain raw output.
"""

import fcntl
import io
import json
import os
import select
import shutil
import signal
import sqlite3
import subprocess
import sys
import time
import unittest
import wave
from contextlib import closing, contextmanager, redirect_stderr, redirect_stdout
from pathlib import Path
from unittest.mock import patch

from tests import test_rust_maintenance_cli as maintenance
from tests.scan_report_support import assert_completed_report_facts
from tests.test_rust_legacy_parser import PYTHON, REFERENCE

BINARY = maintenance.BINARY


class ScanWatchCliTest(unittest.TestCase):
    maxDiff = None
    setUp = maintenance.NativeMaintenanceCliTest.setUp
    setup = maintenance.NativeMaintenanceCliTest.setup
    run_cli = maintenance.NativeMaintenanceCliTest.run_cli
    text = maintenance.NativeMaintenanceCliTest.text
    normalize = maintenance.NativeMaintenanceCliTest.normalize
    rows = maintenance.NativeMaintenanceCliTest.rows
    compare = maintenance.NativeMaintenanceCliTest.compare
    tree = maintenance.NativeMaintenanceCliTest.tree
    restore_output = maintenance.NativeMaintenanceCliTest.restore_output

    def shell_pair(self, flags=(), mode="--json", code=0):
        args = [mode, "watch", "--kind", "sniff", *flags]
        oracle, native = self.run_cli(True, args), self.run_cli(False, args)
        self.assertEqual(
            (oracle.returncode, native.returncode), (code, code), (oracle, native)
        )
        a, b = json.loads(oracle.stdout), json.loads(native.stdout)
        self.assertEqual(list(a), list(b))
        if mode == "--machine":
            a, b = a["data"], b["data"]
        progress = [
            list(map(json.loads, p.stderr.splitlines())) for p in (oracle, native)
        ]
        self.assertEqual(len(progress[0]), a["cycles"])
        self.assertEqual(len(progress[1]), b["cycles"])
        for left, right in [(a["last"], b["last"]), *zip(*progress, strict=True)]:
            self.assertEqual(list(left), list(right))
            for value in (left, right):
                scan = value.pop("scan")
                self.assertIn("complete", scan)
                self.assertIn("scans", scan)
                self.assertIn("observations", scan)
            self.assertEqual(self.normalize(left), self.normalize(right))
        self.assertEqual(self.normalize(a), self.normalize(b))
        return oracle, native

    def test_empty_exact_all_modes_and_bounded_cycles(self):
        self.setup("empty", files=0)
        for mode in ([], ["--json"], ["--machine"]):
            self.compare(mode + ["watch", "--kind", "sniff"])
        oracle, native = self.compare(
            ["--machine", "watch", "--cycles", "2", "--interval", "1"]
        )
        self.assertEqual(len(native.stderr.splitlines()), 2)
        self.assertEqual(json.loads(oracle.stdout)["data"]["cycles"], 2)

    def test_callable_watch_routes_cycle_progress_and_reaps_helper(self):
        # Development integration only: helper identity is explicitly injected.
        # Strict installed artifact qualification remains a separate gate.
        from tests.test_native_cli_callable_contract import load

        self.setup("empty", files=0)
        cli = load()
        launches = []
        original = subprocess.Popen

        def launch(*args, **kwargs):
            child = original(*args, **kwargs)
            launches.append(child)
            return child

        for mode in ([], ["--json"], ["--machine"]):
            oracle, native = self.compare(mode + ["watch", "--kind", "sniff"])
            stdout, stderr = io.StringIO(), io.StringIO()
            with (
                patch.object(cli, "legacy_binary", return_value=BINARY),
                patch.object(cli.subprocess, "Popen", side_effect=launch),
                redirect_stdout(stdout),
                redirect_stderr(stderr),
            ):
                result = cli.main(
                    ["--db", str(self.native), *mode, "watch", "--kind", "sniff"]
                )
            self.assertEqual(
                (result, stdout.getvalue(), stderr.getvalue()),
                (native.returncode, native.stdout, native.stderr),
            )
            self.assertEqual(stderr.getvalue(), oracle.stderr)
            self.assertEqual(len(stderr.getvalue().splitlines()), 1)
        self.assertEqual(len(launches), 3)
        for child in launches:
            self.assertIsNotNone(child.returncode)
            self.assertTrue(child.stdin.closed)
            self.assertTrue(child.stdout.closed)
            with self.assertRaises(ChildProcessError):
                os.waitpid(child.pid, os.WNOHANG)

    def test_callable_watch_progress_error_preserves_identity_and_cleanup(self):
        from tests.test_native_cli_callable_contract import load

        self.setup("empty", files=0)
        cli = load()
        failure = RuntimeError("watch progress callback sentinel")
        launches = []
        original = subprocess.Popen

        class Broken(io.StringIO):
            def write(self, text):
                raise failure

        def launch(*args, **kwargs):
            child = original(*args, **kwargs)
            launches.append(child)
            return child

        with (
            patch.object(cli, "legacy_binary", return_value=BINARY),
            patch.object(cli.subprocess, "Popen", side_effect=launch),
            redirect_stdout(io.StringIO()),
            redirect_stderr(Broken()),
        ):
            with self.assertRaises(RuntimeError) as caught:
                cli.main(["--db", str(self.native), "watch"])
        self.assertIs(caught.exception, failure)
        self.assertEqual(len(launches), 1)
        for child in launches:
            self.assertIsNotNone(child.returncode)
            self.assertTrue(child.stdin.closed)
            self.assertTrue(child.stdout.closed)
            with self.assertRaises(ChildProcessError):
                os.waitpid(child.pid, os.WNOHANG)
        with closing(sqlite3.connect(self.native, timeout=0)) as db:
            db.execute("BEGIN IMMEDIATE")
            db.rollback()
        self.compare(["watch"])

    def test_validation_order_and_profiles_all_modes(self):
        self.setup("empty", files=0)
        for mode in ([], ["--json"], ["--machine"]):
            for flags in (
                ["--interval", "0", "--cycles", "-1"],
                ["--interval", "86401"],
                ["--cycles", "-1", "--settle", "-1"],
                ["--cycles", "1000001"],
                ["--settle", "-1", "--batch", "0"],
                ["--settle", "86401"],
                ["--batch", "0"],
                ["--batch", "1001"],
                ["--workers", "0"],
                ["--workers", "17"],
                ["--retry-transient", "11", "--workers", "0"],
                ["--retry-delay", "0"],
                ["--location", "absent"],
            ):
                with self.subTest(mode=mode, flags=flags):
                    self.compare(mode + ["watch", *flags])
            self.compare(mode + ["--profile", "absent", "watch", "--settle", "-1"])
            self.compare(mode + ["--profile", "absent", "watch", "--interval", "0"])

    def test_bounded_backlog_repeat_and_stable_revision_parity(self):
        self.setup("seeded", files=3)
        for path in (self.ref, self.native):
            with closing(sqlite3.connect(path)) as db, db:
                db.execute(
                    "UPDATE processing_jobs SET created_at='2000-01-01 00:00:00'"
                )
        for index in range(4):
            _, native = self.shell_pair(["--settle", "0", "--batch", "1"])
            result = json.loads(native.stdout)["last"]
            self.assertEqual(result["processing"]["processed"], 1)
            self.assertEqual(result["queued"], 3 if index == 0 else 0)
            self.assertEqual(result["cached"], 0 if index == 0 else 3)
            self.assertEqual(
                self.rows(
                    self.ref,
                    "SELECT profile,file_id,revision FROM watch_revisions ORDER BY file_id",
                ),
                self.rows(
                    self.native,
                    "SELECT profile,file_id,revision FROM watch_revisions ORDER BY file_id",
                ),
            )
            # Both owners use created_at,id. New job UUIDs and second-boundary
            # timestamps are independently generated, so tied per-tick file
            # selection is not an exact cross-process identity assertion.
            for path in (self.ref, self.native):
                self.assertEqual(
                    self.rows(
                        path,
                        "SELECT count(*) FROM processing_jobs WHERE state='complete'",
                    ),
                    [(index + 1,)],
                )
        self.assertEqual(
            self.rows(
                self.ref,
                "SELECT file_id,operation,state,attempts FROM processing_jobs ORDER BY file_id,operation",
            ),
            self.rows(
                self.native,
                "SELECT file_id,operation,state,attempts FROM processing_jobs ORDER BY file_id,operation",
            ),
        )
        self.assertEqual(
            self.rows(
                self.native, "SELECT state FROM processing_jobs WHERE operation='hash'"
            ),
            [("complete",)],
        )

    def test_settling_still_runs_existing_backlog_and_cached_failure(self):
        self.setup("failed")
        _, native = self.shell_pair()
        report = json.loads(native.stdout)["last"]
        self.assertEqual(
            (report["queued"], report["cached"], report["processing"]["processed"]),
            (0, 0, 1),
        )
        _, native = self.shell_pair(["--settle", "0"], mode="--machine")
        self.assertTrue(json.loads(native.stdout)["data"]["complete"])
        self.assertEqual(
            self.rows(
                self.native,
                "SELECT operation,state FROM processing_jobs ORDER BY operation",
            ),
            [("hash", "complete"), ("sniff", "failed")],
        )

    def test_incomplete_cycle_repeats_to_requested_bound(self):
        self.setup()
        self.source.rename(self.root / "offline-source")
        oracle, native = self.shell_pair(["--cycles", "2", "--interval", "1"], code=3)
        for result in (oracle, native):
            self.assertEqual(json.loads(result.stdout)["cycles"], 2)
            self.assertEqual(
                [json.loads(line)["complete"] for line in result.stderr.splitlines()],
                [False, False],
            )

    def test_invalid_processing_bounds_follow_scan_and_enqueue(self):
        self.setup(files=2)
        self.compare(
            ["--machine", "watch", "--kind", "sniff", "--settle", "0", "--workers", "0"]
        )
        for path in (self.ref, self.native):
            self.assertEqual(self.rows(path, "SELECT count(*) FROM files"), [(2,)])
            self.assertEqual(
                self.rows(path, "SELECT count(*) FROM watch_revisions"), [(2,)]
            )
            self.assertEqual(
                self.rows(path, "SELECT state FROM processing_jobs"),
                [("queued",), ("queued",)],
            )

    def saved_refresh(self, path):
        saved = subprocess.run(
            [
                str(PYTHON),
                "-c",
                "import sys;sys.path.insert(0,sys.argv.pop(1));from catabolic.store import Store;from catabolic.app import Application;from catabolic.layouts import Layouts,PRESETS\nwith Store(sys.argv[1],writable=True) as store:\n app=Application(store);app.scan()\n for row in store.rows('SELECT * FROM files ORDER BY path'):\n  app.media.associate(row['id'],app.put_item('document',{'fixture':row['path']},{'title':row['path']})['id'])\n Layouts(app).put('saved',PRESETS['catabolic']);Layouts(app).run('saved','global',apply=True)",
                str(REFERENCE / "src"),
                str(path),
            ],
            capture_output=True,
            text=True,
            env=self.env,
            timeout=30,
        )
        self.assertEqual(saved.returncode, 0, saved.stderr)

    def queue_refresh(self, path):
        with closing(sqlite3.connect(path)) as db, db:
            db.execute(
                "INSERT INTO catalog_refresh_settings VALUES ('default','global',1,100)"
            )
            db.execute(
                "INSERT INTO catalog_refresh_queue(profile,catalog) VALUES ('default','global')"
            )

    def published(self, path):
        return {
            "owned_links": self.rows(
                path,
                "SELECT profile,catalog,path,target FROM owned_links ORDER BY path",
            ),
            "journal": self.rows(path, "SELECT count(*) FROM journal"),
            "queue": self.rows(path, "SELECT count(*) FROM catalog_refresh_queue"),
            "jobs": self.rows(path, "SELECT state FROM processing_jobs ORDER BY rowid"),
        }

    def test_automatic_refresh_publishes_like_frozen(self):
        # Processing.run refreshes pending catalogs through the shared
        # executor (track C5). Each engine runs on an identical catalog with
        # the same output contents; the published state must match.
        self.setup()
        self.saved_refresh(self.ref)
        self.queue_refresh(self.ref)
        base = self.root / "base.db"
        shutil.copyfile(self.ref, base)
        snapshot = self.root / "output-snapshot"
        shutil.copytree(self.output, snapshot, symlinks=True)
        outcome = {}
        for reference in (True, False):
            path = self.ref if reference else self.native
            shutil.copyfile(base, path)
            self.restore_output(snapshot)
            result = self.run_cli(
                reference, ["--machine", "watch", "--kind", "sniff", "--settle", "0"]
            )
            outcome[reference] = (result, self.published(path), self.tree())
        (old, old_state, old_tree), (new, new_state, new_tree) = (
            outcome[True],
            outcome[False],
        )
        self.assertEqual(new.returncode, old.returncode, (old.stdout, new.stdout))
        self.assertEqual(new.returncode, 0, new.stdout)
        self.assertEqual(new_state, old_state)
        self.assertEqual(new_tree, old_tree)
        self.assertTrue(new_state["owned_links"], "the refresh published links")
        self.assertEqual(new_state["queue"], [(0,)])

    def nonparity(self, mode):
        self.setup()
        args = [mode, "watch", "--kind", "sniff"]
        a, b = self.run_cli(True, args), self.run_cli(False, args)
        self.assertEqual((a.returncode, b.returncode), (0, 0))
        left, right = json.loads(a.stdout), json.loads(b.stdout)
        if mode == "--machine":
            left, right = left["data"], right["data"]
        left, right = left["last"]["scan"], right["last"]["scan"]
        assert_completed_report_facts(self, left, right)
        # Each emitted progress line carries the identical completed owner report.
        self.assertEqual(json.loads(b.stderr)["scan"], right)
        return a, b

    def test_known_scan_json_nonparity_witness(self):
        self.nonparity("--json")

    def test_known_scan_machine_nonparity_witness(self):
        self.nonparity("--machine")

    @contextmanager
    def writer_lock(self, path, timeout=0):
        with open(str(path) + ".lock", "a+b") as lock:
            deadline = time.monotonic() + timeout
            while True:
                try:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    break
                except BlockingIOError:
                    if time.monotonic() >= deadline:
                        raise
                    time.sleep(0.01)
            yield lock
            fcntl.flock(lock, fcntl.LOCK_UN)

    def assert_writer_available(self, path, timeout=0):
        with self.writer_lock(path, timeout):
            with closing(sqlite3.connect(path, timeout=0.1)) as db:
                db.execute("BEGIN IMMEDIATE")
                db.rollback()

    @staticmethod
    def clean_process(process):
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.communicate(timeout=5)
        for stream in (process.stdout, process.stderr):
            stream.close()

    def start(self, reference, *args, mode="--machine"):
        command = (
            [str(PYTHON), "-c", maintenance.ORACLE, str(REFERENCE / "src")]
            if reference
            else [str(BINARY)]
        )
        path = self.ref if reference else self.native
        process = subprocess.Popen(
            command + ["--db", str(path), *([mode] if mode else []), "watch", *args],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=self.env,
            start_new_session=True,
        )
        self.addCleanup(self.clean_process, process)
        return process

    def stop(self, process):
        start = time.monotonic()
        process.send_signal(signal.SIGINT)
        stdout, stderr = process.communicate(timeout=10)
        self.assertEqual(process.returncode, 130, (stdout, stderr))
        self.assertLess(time.monotonic() - start, 10)
        report = json.loads(stdout)
        self.assertEqual(report["data"]["error"]["type"], "KeyboardInterrupt")
        return report, stderr

    def test_idle_writer_release_interrupt_and_immediate_restart(self):
        self.setup("empty", files=0)
        results = []
        for reference, path in ((True, self.ref), (False, self.native)):
            process = self.start(reference, "--cycles", "0", "--interval", "86400")
            self.assertTrue(select.select([process.stderr], [], [], 15)[0])
            progress = json.loads(process.stderr.readline())
            self.assertEqual(progress["cycle"], 1)
            self.assert_writer_available(path)
            results.append(self.stop(process))
            self.assert_writer_available(path)
            restarted = self.run_cli(reference, ["--machine", "watch"])
            self.assertEqual(restarted.returncode, 0, restarted)
        self.assertEqual(results[0], results[1])

    def test_sigterm_idle_has_no_new_output_and_restarts(self):
        self.setup("empty", files=0)
        for mode in (None, "--json", "--machine"):
            for reference, path in ((True, self.ref), (False, self.native)):
                with self.subTest(mode=mode, reference=reference):
                    process = self.start(
                        reference, "--cycles", "0", "--interval", "86400", mode=mode
                    )
                    self.assertTrue(select.select([process.stderr], [], [], 15)[0])
                    self.assertEqual(json.loads(process.stderr.readline())["cycle"], 1)
                    self.assert_writer_available(path)
                    process.send_signal(signal.SIGTERM)
                    stdout, stderr = process.communicate(timeout=10)
                    self.assertEqual(
                        (process.returncode, stdout, stderr), (-15, b"", b"")
                    )
                    self.assert_writer_available(path)
                    restarted = self.run_cli(reference, ["--machine", "watch"])
                    self.assertEqual(restarted.returncode, 0, restarted)

    def test_writer_busy_exact_and_recovery(self):
        self.setup("empty", files=0)
        with (
            open(str(self.ref) + ".lock", "a+b") as a,
            open(str(self.native) + ".lock", "a+b") as b,
        ):
            for lock in (a, b):
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            self.compare(["--machine", "watch"])
            self.compare(["--machine", "watch", "--interval", "0"])
        self.compare(["--machine", "watch"])

    def blocking_probe(self):
        with wave.open(str(self.source / "tone.wav"), "wb") as stream:
            stream.setparams((1, 2, 8000, 800, "NONE", "not compressed"))
            stream.writeframes(b"\0\0" * 800)
        self.setup(files=0)
        original_path = self.env["PATH"]
        tools = self.root / "tools"
        tools.mkdir()
        marker = self.root / "probe.pid"
        resume = self.root / "probe.resume"
        executable = tools / "ffprobe"
        executable.write_text(
            f"#!{PYTHON}\n"
            "import os,sys,time,pathlib\n"
            "if '-version' in sys.argv or '--version' in sys.argv:\n"
            " print('ffprobe version disposable-fixture'); raise SystemExit(0)\n"
            f"pathlib.Path({str(marker)!r}).write_text(str(os.getpid()))\n"
            f"while not pathlib.Path({str(resume)!r}).exists(): time.sleep(0.05)\n"
            'print(\'{"format":{"format_name":"wav","duration":"0.1"},"streams":[{"codec_type":"audio","codec_name":"pcm_s16le","sample_rate":"8000","channels":1}]}\')\n'
        )
        executable.chmod(0o700)
        self.env["PATH"] = str(tools) + os.pathsep + original_path
        return marker, resume

    def wait_for_probe(self, process, path, marker):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            running = self.rows(
                path, "SELECT count(*) FROM processing_jobs WHERE state='running'"
            )[0][0]
            if marker.exists() and running == 1:
                return int(marker.read_text())
            self.assertIsNone(process.poll())
            time.sleep(0.02)
        self.fail("probe fixture did not reach the processing wait")

    def test_processing_wait_interrupt_reaps_tool_and_restarts(self):
        marker, resume = self.blocking_probe()
        results = []
        for reference, path in ((True, self.ref), (False, self.native)):
            marker.unlink(missing_ok=True)
            resume.unlink(missing_ok=True)
            process = self.start(reference, "--kind", "probe", "--settle", "0")
            tool_pid = self.wait_for_probe(process, path, marker)
            # A child may write its readiness marker just before the parent
            # enters its detached wait. Require observable lock release within
            # a bounded window, while the tool remains blocked and alive.
            self.assert_writer_available(path, timeout=3)
            results.append(self.stop(process))
            self.assert_writer_available(path)
            with self.assertRaises(ProcessLookupError):
                os.kill(tool_pid, 0)
            # Frozen Processing may leave a running row without a live claim;
            # its next run recovers that row. The contract here is released
            # execution ownership and successful immediate restart, not an
            # invented requirement that interruption rewrites every job state.
            self.assertEqual(
                self.rows(
                    path,
                    "SELECT count(*) FROM execution_claims WHERE lease_until>unixepoch()",
                ),
                [(0,)],
            )
            resume.touch()
            restart = self.run_cli(
                reference, ["--machine", "watch", "--kind", "sniff", "--settle", "0"]
            )
            self.assertEqual(restart.returncode, 0, restart)
            self.assertEqual(
                self.rows(
                    path, "SELECT count(*) FROM processing_jobs WHERE state='running'"
                ),
                [(0,)],
            )
        self.assertEqual(results[0], results[1])

    def test_sigterm_processing_wait_reaps_tool_and_restarts(self):
        marker, resume = self.blocking_probe()
        process = self.start(False, "--kind", "probe", "--settle", "0")
        tool_pid = self.wait_for_probe(process, self.native, marker)
        self.assert_writer_available(self.native, timeout=3)
        process.send_signal(signal.SIGTERM)
        stdout, stderr = process.communicate(timeout=10)
        self.assertEqual((process.returncode, stdout, stderr), (-15, b"", b""))
        self.assert_writer_available(self.native)
        with self.assertRaises(ProcessLookupError):
            os.kill(tool_pid, 0)
        self.assertEqual(
            self.rows(
                self.native,
                "SELECT count(*) FROM execution_claims WHERE lease_until>unixepoch()",
            ),
            [(0,)],
        )
        resume.touch()
        restarted = self.run_cli(
            False, ["--machine", "watch", "--kind", "sniff", "--settle", "0"]
        )
        self.assertEqual(restarted.returncode, 0, restarted)
        self.assertEqual(
            self.rows(
                self.native,
                "SELECT count(*) FROM processing_jobs WHERE state='running'",
            ),
            [(0,)],
        )

    def test_refresh_work_added_during_processing_wait_is_published(self):
        # Refresh work queued while a tool runs is published by the same
        # cycle's post-run refresh, through the shared executor (track C5).
        marker, resume = self.blocking_probe()
        self.saved_refresh(self.native)
        process = self.start(False, "--kind", "probe", "--settle", "0")
        self.wait_for_probe(process, self.native, marker)
        self.assert_writer_available(self.native, timeout=3)
        with self.writer_lock(self.native, timeout=3):
            self.queue_refresh(self.native)
        resume.touch()
        stdout, stderr = process.communicate(timeout=30)
        self.assertEqual(process.returncode, 0, (stdout, stderr))
        state = self.published(self.native)
        self.assertEqual(state["jobs"][-1], ("complete",))
        self.assertEqual(state["queue"], [(0,)])
        self.assertEqual(state["journal"], [(0,)])
        self.assertTrue(state["owned_links"], "the refresh published links")
        self.assert_writer_available(self.native)


if __name__ == "__main__":
    if "--diagnose-full-scan-parity" in sys.argv:
        mode = "--machine" if "--machine" in sys.argv else "--json"
        destination = Path(os.environ["CATABOLIC_WATCH_DIAGNOSTIC_DIR"])
        destination.mkdir(parents=True, exist_ok=True)
        case = ScanWatchCliTest()
        case.setUp()
        try:
            oracle, native = case.nonparity(mode)
            for name, result in (("oracle", oracle), ("native", native)):
                (destination / f"{mode[2:]}-{name}.stdout").write_text(result.stdout)
                (destination / f"{mode[2:]}-{name}.stderr").write_text(result.stderr)
            case.assertEqual(
                case.normalize(json.loads(oracle.stdout)),
                case.normalize(json.loads(native.stdout)),
            )
        finally:
            case.doCleanups()
    else:
        unittest.main()
