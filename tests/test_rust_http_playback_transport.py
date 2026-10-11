# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Actual native HTTP and combined-worker HLS/lifecycle qualification."""

import json
import os
import shutil
import signal
import subprocess
import threading
import time
import unittest
from pathlib import Path

from catabolic.store import Store
from tests import test_http_playback as reference
from tests import test_rust_http_transport as transport

NATIVE = Path(
    os.environ.get("CATABOLIC_NATIVE_BINARY", "target/debug/catabolic-native")
).resolve()


class NativePlaybackTransportTest(unittest.TestCase):
    start_native_server = transport.NativeHTTPTransportTest.start_native_server
    stop_server = transport.NativeHTTPTransportTest.stop_server

    def setUp(self):
        reference.PlaybackTest.setUp(self)
        self.client.close()
        self.start_native_server()

    def request(self, method, path, **kwargs):
        deadline = time.monotonic() + 10
        while True:
            result = self.client.request(method, path, **kwargs)
            if result.status_code != 503 or time.monotonic() >= deadline:
                return result
            time.sleep(0.03)

    def create(self, index, key):
        result = self.request(
            "POST",
            "/v1/playback-sessions",
            headers={**self.headers[index], "Idempotency-Key": key},
            json=self.body,
        )
        self.assertIn(result.status_code, (200, 202), result.text)
        return result.json()

    def test_shared_native_hls_worker_and_exact_segment_transport(self):
        first = self.create(0, "first")
        second = self.create(1, "second")
        with Store(self.database) as store:
            self.assertEqual(len(store.rows("SELECT id FROM api_playback_jobs")), 1)
            before = len(store.rows("SELECT id FROM files"))
        cancelled = self.request(
            "POST", first["status_url"] + "/cancel", headers=self.headers[0]
        )
        self.assertEqual(cancelled.status_code, 200, cancelled.text)
        run = subprocess.run(
            [str(NATIVE), "--db", str(self.database), "api", "worker", "--once"],
            capture_output=True,
            text=True,
            timeout=60,
        )
        self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
        report = json.loads(run.stdout)
        self.assertEqual(report["processed"], 1, report)
        self.assertIn("notifications", report)
        status = self.client.get(second["status_url"], headers=self.headers[1])
        self.assertEqual(status.status_code, 200, status.text)
        self.assertEqual(status.json()["state"], "ready", status.text)
        playlist_path = f"/v1/playback-sessions/{second['session_id']}/index.m3u8"
        playlist = self.client.get(playlist_path, headers=self.headers[1])
        self.assertEqual(playlist.status_code, 200, playlist.text)
        self.assertIn("#EXT-X-ENDLIST", playlist.text)
        segment = next(
            line
            for line in playlist.text.splitlines()
            if line and not line.startswith("#")
        )
        segment_path = playlist_path.rsplit("/", 1)[0] + "/" + segment
        data = self.client.get(segment_path, headers=self.headers[1])
        self.assertEqual(
            data.status_code, 200, data.text[:100] if data.status_code != 200 else ""
        )
        self.assertEqual(data.content[0], 0x47)
        self.assertEqual(data.headers["content-type"], "video/mp2t")
        head = self.client.head(segment_path, headers=self.headers[1])
        self.assertEqual(head.status_code, 200)
        self.assertEqual(head.content, b"")
        self.assertEqual(head.headers["content-length"], str(len(data.content)))
        partial = self.client.get(
            segment_path, headers={**self.headers[1], "Range": "bytes=0-7"}
        )
        self.assertEqual(partial.status_code, 416, partial.text)
        forbidden = self.client.get(segment_path, headers=self.headers[0])
        self.assertEqual(forbidden.status_code, 404, forbidden.text)
        self.assertEqual(
            self.client.get(first["status_url"], headers=self.headers[0]).json()[
                "state"
            ],
            "cancelled",
        )
        with Store(self.database, writable=True) as store:
            self.assertEqual(store.rows("SELECT * FROM api_worker_status"), [])
            self.assertEqual(len(store.rows("SELECT id FROM files")), before)
            self.assertEqual(store.rows("SELECT * FROM media_outputs"), [])

    def test_slow_tool_identity_releases_writer_and_refreshes_auth(self):
        session = self.create(0, "before-slow-tool")
        self.client.close()
        self.stop_server()
        tools = self.root / "slow-tools"
        tools.mkdir()
        marker = self.root / "probe-entered"
        release = self.root / "probe-release"
        ffmpeg = shutil.which("ffmpeg")
        self.assertIsNotNone(ffmpeg)
        wrapper = tools / "ffmpeg"
        wrapper.write_text(
            '#!/bin/sh\nif [ "$1" = -version ]; then\n  touch \''
            + str(marker)
            + "'\n  while [ ! -e '"
            + str(release)
            + "' ]; do sleep 0.01; done\nfi\nexec '"
            + ffmpeg
            + '\' "$@"\n'
        )
        wrapper.chmod(0o700)
        self.start_native_server(
            env={**os.environ, "PATH": str(tools) + os.pathsep + os.environ["PATH"]}
        )
        result = []
        errors = []

        def cancel():
            try:
                result.append(
                    self.client.post(
                        session["status_url"] + "/cancel", headers=self.headers[0]
                    )
                )
            except Exception as error:
                errors.append(error)

        thread = threading.Thread(target=cancel)
        thread.start()
        try:
            deadline = time.monotonic() + 10
            while not marker.exists():
                self.assertLess(time.monotonic(), deadline)
                time.sleep(0.02)
            # A version probe must retain neither the catalog nor its writer.
            with (
                Store(self.database, writable=True) as store,
                store.transaction() as db,
            ):
                token = self.headers[0]["Authorization"][7:].split(".")[0]
                db.execute("UPDATE api_tokens SET revoked=1 WHERE id=?", (token,))
        finally:
            release.touch()
            thread.join(timeout=15)
        self.assertFalse(thread.is_alive())
        self.assertEqual(errors, [])
        self.assertEqual(result[0].status_code, 401, result[0].text)
        with Store(self.database, writable=True) as store:
            self.assertEqual(
                store.rows(
                    "SELECT cancelled FROM api_playback_sessions WHERE id=?",
                    (session["session_id"],),
                )[0]["cancelled"],
                0,
            )

    def test_worker_singleton_idle_writer_release_and_sigterm_cleanup(self):
        log = (self.root / "native-worker.log").open("wb")
        self.addCleanup(log.close)
        worker = subprocess.Popen(
            [
                str(NATIVE),
                "--db",
                str(self.database),
                "api",
                "worker",
                "--interval",
                "0.1",
            ],
            stdout=log,
            stderr=log,
        )

        def stop():
            if worker.poll() is None:
                worker.terminate()
                worker.wait(timeout=25)

        self.addCleanup(stop)
        deadline = time.monotonic() + 15
        while True:
            self.assertIsNone(
                worker.poll(), (self.root / "native-worker.log").read_text()
            )
            with Store(self.database) as store:
                rows = store.rows("SELECT worker_pid FROM api_worker_status")
            if rows and rows[0]["worker_pid"] == worker.pid:
                break
            self.assertLess(time.monotonic(), deadline)
            time.sleep(0.03)
        second = subprocess.run(
            [str(NATIVE), "--db", str(self.database), "api", "worker", "--once"],
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertEqual(second.returncode, 2, second.stdout + second.stderr)
        self.assertIn("another Catabolic writer", second.stderr)
        while True:
            try:
                with Store(self.database, writable=True):
                    time.sleep(0.2)
                break
            except Exception:
                self.assertLess(time.monotonic(), deadline)
                time.sleep(0.02)
        worker.send_signal(signal.SIGTERM)
        self.assertEqual(
            worker.wait(timeout=25), 0, (self.root / "native-worker.log").read_text()
        )
        with Store(self.database, writable=True) as store:
            self.assertEqual(store.rows("SELECT * FROM api_worker_status"), [])
