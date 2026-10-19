# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Private helpers refuse failpoint fields before any transport or effect.

The test build has the ``failpoints`` feature; the test-only environment
switch makes each helper take the default-feature path. A loopback server
records every request, so a refused helper must leave it untouched.
"""

import http.server
import json
import os
import shutil
import sqlite3
import subprocess
import tempfile
import threading
import unittest
from contextlib import closing
from pathlib import Path

from tests.test_rust_legacy_review import BINARY

SIMULATE = {"CATABOLIC_TEST_SIMULATE_DEFAULT_FEATURES": "1"}


class Recorder(http.server.BaseHTTPRequestHandler):
    def _record(self):
        self.server.requests.append((self.command, self.path))
        body = b"{}"
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    do_GET = do_POST = do_PUT = _record

    def log_message(self, *args):
        pass


class FailpointHelperTest(unittest.TestCase):
    def setUp(self):
        self.assertTrue(BINARY.is_file(), str(BINARY))
        temporary = tempfile.TemporaryDirectory(prefix="failpoint-helpers-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Recorder)
        self.server.requests = []
        thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        self.env = {
            k: v
            for k, v in os.environ.items()
            if not k.startswith("CATABOLIC_") and k not in ("PYTHONPATH", "PYTHONHOME")
        }
        self.env.update(HOME=str(self.root / "home"), FIXTURE_TOKEN="secret")

    def helper(self, argument, payload, *, simulate=True):
        return subprocess.run(
            [str(BINARY), argument],
            input=json.dumps(payload),
            env={**self.env, **(SIMULATE if simulate else {})},
            text=True,
            capture_output=True,
            timeout=60,
        )

    def test_processor_transport_refuses_before_the_request(self):
        request = {
            "url": self.url + "/jobs",
            "headers": {},
            "method": "GET",
            "maximum": 1000,
            "timeout": 5,
            "body": None,
            "structured_errors": False,
        }
        # Control: the same helper and server do transmit without the field.
        result = self.helper("__processor-http-worker", request, simulate=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(self.server.requests), 1)
        for field in ({"failpoint": None}, {"crash": False}):
            with self.subTest(field=field):
                result = self.helper("__processor-http-worker", {**request, **field})
                self.assertIn("failpoint_injection_unavailable", result.stdout)
                self.assertEqual(len(self.server.requests), 1)

    def test_network_helpers_send_nothing(self):
        cases = [
            (
                "__notification-provider-worker",
                {"url": self.url + "/hook", "event": "test", "severity": "info"},
            ),
            (
                "__legacy-refresh-worker",
                {
                    "application": "jellyfin",
                    "endpoint": self.url,
                    "credential_env": "FIXTURE_TOKEN",
                },
            ),
            ("__publication-immich-identity-worker", {"destination": self.url}),
            ("__identify-tmdb-worker", {"url": self.url + "/search"}),
        ]
        for argument, payload in cases:
            for field in ({"failpoint": None}, {"crash": False}):
                with self.subTest(helper=argument, field=field):
                    self.helper(argument, {**payload, **field})
                    self.assertEqual(self.server.requests, [])

    def test_consumer_watch_tick_refuses_before_catalog_effects(self):
        database = self.root / "catalog.db"
        result = subprocess.run(
            [str(BINARY), "--db", str(database), "init"],
            env=self.env,
            text=True,
            capture_output=True,
            timeout=60,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        backup = self.root / "before.db"
        shutil.copyfile(database, backup)

        def dump(path):
            with closing(sqlite3.connect(path)) as db:
                return list(db.iterdump())

        before = dump(backup)
        for field in ({"failpoint": None}, {"crash": False}):
            with self.subTest(field=field):
                result = self.helper(
                    "__legacy-consumer-watch-tick",
                    {"db": str(database), "profile": "default", "limit": 1, **field},
                )
                self.assertIn("failpoint_injection_unavailable", result.stdout)
                self.assertEqual(dump(database), before)
                self.assertEqual(self.server.requests, [])


if __name__ == "__main__":
    unittest.main()
