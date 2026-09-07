# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Real boundary failures from the audit, using disposable sources and tools."""

import copy
import json
import os
import shlex
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.importers import import_catalog
from catabolic.manifest import Manifest
from catabolic.network_adapters import request
from catabolic.process_runner import CommandFailure, command_output
from catabolic.store import Store


class ImportSafetyTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        source = self.root / "source"
        source.mkdir()
        self.file = source / "book.epub"
        self.file.write_bytes(b"original")
        database = self.root / "catalog.sqlite3"
        Store.initialize(database)
        self.store = Store(database, writable=True)
        self.addCleanup(self.store.close)
        self.app = Application(self.store)
        self.app.bind("source", "books", str(source))
        self.app.scan()
        self.file_id = self.app.files()["files"][0]["id"]
        item = self.app.put_item("book", {"fixture": "book"}, {})
        self.app.put_mapping("global", self.file_id, item["id"], "book.epub")
        with self.store.transaction():
            self.document = Manifest(self.app).build()
        self.destination = self.root / "library"
        self.destination.mkdir()

    def replace_source(self):
        before = self.file.stat()
        other = self.root / "replacement"
        other.write_bytes(b"replaced")
        os.utime(other, ns=(before.st_atime_ns, before.st_mtime_ns))
        other.replace(self.file)

    def importer(self, code):
        executable = self.root / "importer"
        executable.write_text(f"#!{sys.executable}\n" + code)
        executable.chmod(0o700)
        return patch("catabolic.importers.shutil.which", return_value=str(executable))

    def run_import(self, **kwargs):
        return import_catalog(
            self.app,
            self.document,
            "calibre",
            str(self.destination),
            apply=True,
            **kwargs,
        )

    def sleeping_importer(self, marker, name="calibredb"):
        # Exercise importer deadlines, not cold Python interpreter startup.
        # The shell performs the observable side effect before exec'ing sleep.
        executable = self.root / name
        executable.write_text(
            "#!/bin/sh\n: > " + shlex.quote(str(marker)) + "\nexec sleep 30\n"
        )
        executable.chmod(0o700)
        return executable

    def test_replacement_with_preserved_size_and_time_is_rejected(self):
        self.replace_source()
        with (
            self.importer("raise AssertionError('must not launch')"),
            self.assertRaisesRegex(CatabolicError, "changed since scan"),
        ):
            self.run_import()

    def test_manifest_path_must_match_recorded_file_id(self):
        document = copy.deepcopy(self.document)
        document["content"]["files"][0]["path"] = "other.epub"
        with self.assertRaisesRegex(CatabolicError, "disagrees with inventory"):
            import_catalog(self.app, document, "calibre", str(self.destination))

    def test_replacement_during_staging_never_launches_importer(self):
        import shutil

        original = shutil.copyfileobj

        def replace_after_copy(source, output):
            original(source, output)
            self.replace_source()

        with (
            self.importer("raise AssertionError('must not launch')"),
            patch(
                "catabolic.importers.shutil.copyfileobj", side_effect=replace_after_copy
            ),
        ):
            result = self.run_import()
        self.assertEqual(result["outcome"], "failed_before_launch")
        self.assertFalse(result["applied"])
        self.assertEqual(result["completed"], [])
        self.assertIn("changed", result["error"])

    def test_failed_external_tool_reports_unknown_side_effects(self):
        marker = self.destination / "changed"
        with self.importer(
            f"from pathlib import Path\nPath({str(marker)!r}).touch()\nraise SystemExit(7)\n"
        ):
            result = self.run_import()
        self.assertTrue(marker.exists())
        self.assertIsNone(result["applied"])
        self.assertFalse(result["complete"])
        self.assertFalse(result["safe_to_retry"])
        self.assertEqual(result["outcome"], "external_outcome_unknown")
        self.assertEqual(result["attempts"][0]["file_ids"], [self.file_id])
        self.assertTrue(result["attempts"][0]["id"])

    def test_launch_failure_is_known_unapplied(self):
        with patch(
            "catabolic.importers.shutil.which", return_value=str(self.root / "absent")
        ):
            result = self.run_import()
        self.assertFalse(result["applied"])
        self.assertTrue(result["safe_to_retry"])
        self.assertEqual(result["outcome"], "failed_before_launch")

    def test_timeout_reports_unknown_and_preserves_source(self):
        marker = self.destination / "changed"
        executable = self.sleeping_importer(marker)
        with patch("catabolic.importers.shutil.which", return_value=str(executable)):
            result = self.run_import(timeout=1)
        self.assertTrue(marker.exists())
        self.assertEqual(result["outcome"], "external_outcome_unknown")
        self.assertIsNone(result["applied"])
        self.assertEqual(self.file.read_bytes(), b"original")

    def test_interrupted_import_keeps_unknown_outcome(self):
        def interrupted(*args, **kwargs):
            kwargs["on_start"]()
            raise KeyboardInterrupt

        with (
            self.importer("pass"),
            patch("catabolic.importers.command_output", side_effect=interrupted),
        ):
            result = self.run_import()
        self.assertTrue(result["interrupted"])
        self.assertIsNone(result["applied"])
        self.assertFalse(result["safe_to_retry"])

    def test_cli_interruption_returns_json_and_exit_130(self):
        marker = self.destination / "started"
        self.sleeping_importer(marker)
        database = self.store.path
        self.store.close()
        proc = subprocess.Popen(
            [
                sys.executable,
                "-m",
                "catabolic",
                "--json",
                "--db",
                str(database),
                "target",
                "import",
                "calibre",
                "--destination",
                str(self.destination),
                "--apply",
            ],
            env={
                **os.environ,
                "PATH": str(self.root) + os.pathsep + os.environ.get("PATH", ""),
            },
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            # Wait for CLI startup separately from the interruption deadline.
            deadline = time.monotonic() + 30
            while (
                not marker.exists()
                and proc.poll() is None
                and time.monotonic() < deadline
            ):
                time.sleep(0.01)
            self.assertTrue(marker.exists())
            proc.send_signal(signal.SIGINT)
            stdout, stderr = proc.communicate(timeout=5)
            self.assertEqual(proc.returncode, 130, stderr)
            self.assertEqual(json.loads(stdout)["outcome"], "external_outcome_unknown")
            self.assertIsNone(json.loads(stdout)["applied"])
        finally:
            if proc.poll() is None:
                proc.send_signal(signal.SIGINT)
            proc.communicate(timeout=5)


class ProcessLifetimeTest(unittest.TestCase):
    def test_descendants_are_retired_even_after_leader_exit(self):
        # A heartbeat avoids treating a briefly unreaped zombie as a live worker.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for keep_pipes in (True, False):
                with self.subTest(keep_pipes=keep_pipes):
                    heartbeat = root / f"heartbeat-{keep_pipes}"
                    child_pid = root / f"pid-{keep_pipes}"
                    code = """import os,sys,time
from pathlib import Path
heartbeat,pidfile=map(Path,sys.argv[1:3])
pid=os.fork()
if pid:
    pidfile.write_text(str(pid))
    while not heartbeat.exists(): time.sleep(.001)
    os._exit(0)
if sys.argv[3]=='False':
    os.close(1); os.close(2)
while True:
    heartbeat.write_text(str(time.monotonic_ns()))
    time.sleep(.01)
"""
                    try:
                        if keep_pipes:
                            with self.assertRaises(CommandFailure) as failure:
                                command_output(
                                    [
                                        sys.executable,
                                        "-c",
                                        code,
                                        str(heartbeat),
                                        str(child_pid),
                                        str(keep_pipes),
                                    ],
                                    timeout=0.5,
                                )
                            self.assertEqual(failure.exception.state, "timeout")
                        else:
                            command_output(
                                [
                                    sys.executable,
                                    "-c",
                                    code,
                                    str(heartbeat),
                                    str(child_pid),
                                    str(keep_pipes),
                                ],
                                timeout=2,
                            )
                        time.sleep(0.05)
                        stopped = heartbeat.read_text()
                        time.sleep(0.1)
                        self.assertEqual(heartbeat.read_text(), stopped)
                    finally:
                        if child_pid.exists():
                            try:
                                os.kill(int(child_pid.read_text()), signal.SIGKILL)
                            except ProcessLookupError:
                                pass

    def test_input_pipe_and_discarded_error_output(self):
        data = b"secret-input" * 20000
        result = command_output(
            [
                sys.executable,
                "-c",
                "import sys;sys.stdout.buffer.write(sys.stdin.buffer.read())",
            ],
            input_bytes=data,
        )
        self.assertEqual(result, data)
        with self.assertRaises(CommandFailure) as failure:
            command_output(
                [
                    sys.executable,
                    "-c",
                    "import sys;sys.stderr.write('secret');sys.exit(1)",
                ],
                capture=False,
            )
        self.assertNotIn("secret", str(failure.exception))


class HttpDeadlineTest(unittest.TestCase):
    def test_slow_body_and_headers_obey_total_deadline(self):
        stop = threading.Event()
        started = threading.Event()

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                started.set()
                if self.path == "/headers":
                    stop.wait(5)
                    return
                self.send_response(200)
                self.send_header("Content-Length", "10000")
                self.end_headers()
                try:
                    while not stop.wait(0.02):
                        self.wfile.write(b"x")
                        self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    pass

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            for path in ("/body", "/headers"):
                started.clear()
                before = time.monotonic()
                with self.assertRaisesRegex(
                    CatabolicError, "elapsed-time limit"
                ) as failure:
                    request(
                        f"http://127.0.0.1:{server.server_port}{path}",
                        headers={"Authorization": "fixture-secret"},
                        timeout=1,
                    )
                self.assertTrue(started.is_set())
                self.assertLess(time.monotonic() - before, 3)
                self.assertNotIn("fixture-secret", str(failure.exception))
        finally:
            stop.set()
            server.shutdown()
            thread.join()
            server.server_close()

    def test_parent_deadline_covers_worker_startup_and_dns(self):
        from catabolic.process_runner import command_output as run

        # Delay before any HTTP code: simulates an unresponsive resolver without
        # relying on the host DNS configuration or a real unreachable endpoint.
        def stalled_worker(argv, **kwargs):
            return run([sys.executable, "-c", "import time;time.sleep(30)"], **kwargs)

        with (
            patch(
                "catabolic.network_adapters.command_output", side_effect=stalled_worker
            ),
            self.assertRaisesRegex(CatabolicError, "elapsed-time limit"),
        ):
            request("https://example.invalid", timeout=0.1)
