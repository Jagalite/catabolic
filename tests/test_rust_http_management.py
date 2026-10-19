# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Frozen 1.7 generated management client over actual native loopback HTTP.

Python bootstraps disposable schema/catalog/auth fixtures and the fake receiver.
All queries, subscriptions, approvals, mappings and watcher management use the
TypeScript client, while delivery and watcher execution use the native binary.
No Python transport or Python business worker fallback is used.
"""

import json
import os
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import httpx

from catabolic.access import grant_put, issue, principal_put, revoke
from catabolic.app import Application
from catabolic.store import Store
from tests.test_rust_http_transport import BINARY as HTTP_BINARY
from tests.test_rust_migration import BINARY as NATIVE_BINARY

ROOT = Path(__file__).resolve().parents[1]
CLIENT_ROOT = ROOT / "tests/clients"
CLIENT = CLIENT_ROOT / "v1_7/build/management.js"
RECEIVER_TOKEN = "disposable-management-receiver-token"


class NativeHTTPManagementTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        for executable in (HTTP_BINARY, NATIVE_BINARY):
            if not executable.is_file():
                raise AssertionError(
                    f"Build native qualification executable: {executable}"
                )
        if not shutil.which("node") or not shutil.which("npm"):
            raise AssertionError(
                "Node and npm are required for generated-client qualification"
            )
        # Generate only the new frozen lane; the retained 1.2 lane is untouched.
        build = subprocess.run(
            ["npm", "run", "test:1.7"],
            cwd=CLIENT_ROOT,
            capture_output=True,
            text=True,
            timeout=120,
        )
        if build.returncode:
            raise AssertionError(build.stdout + build.stderr)

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.database = self.root / "catalog.db"
        self.source = self.root / "media"
        self.source.mkdir()
        (self.source / "movie.bin").write_bytes(b"disposable catalog source")
        self.title = "Café 日本語 😀"
        Store.initialize(self.database)
        self.tokens = {}
        with Store(self.database, writable=True) as store:
            app = Application(store)
            app.bind("source", "media", str(self.source))
            app.scan()
            self.file = store.rows("SELECT id FROM files")[0]["id"]
            self.item = app.put_item(
                "movie", {"fixture": "management"}, {"title": self.title}
            )["id"]
            app.media.associate(self.file, self.item)
            for name, definition in (
                ("operator", {"actions": ["*"], "operator": True}),
                ("other", {"actions": ["*"], "operator": True}),
                ("reader", {"actions": ["metadata:read"]}),
            ):
                principal_put(store, "default", name)
                grant = grant_put(store, name, definition)["grant_id"]
                self.tokens[name] = issue(store, name, [grant])["token"]
                if name == "operator":
                    self.replacement = issue(store, name, [grant])["token"]
            self.assertEqual(store.schema_version, 32)

        self.received = []
        self.receiver_status = 204
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                raw = self.rfile.read(int(self.headers["Content-Length"]))
                fixture.received.append(
                    {
                        "path": self.path,
                        "auth": self.headers.get("Authorization"),
                        "idempotency_key": self.headers.get("Idempotency-Key"),
                        "raw": raw,
                        "body": json.loads(raw),
                    }
                )
                self.send_response(fixture.receiver_status)
                self.send_header("Content-Length", "0")
                self.end_headers()

            def log_message(self, *args):
                pass

        receiver = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=receiver.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(receiver.server_close)
        self.addCleanup(receiver.shutdown)
        self.receiver = f"http://127.0.0.1:{receiver.server_port}"
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        self.base = f"http://127.0.0.1:{port}"
        log = (self.root / "http.log").open("wb")
        self.addCleanup(log.close)
        self.server = subprocess.Popen(
            [str(HTTP_BINARY), str(self.database), str(port), "writable"],
            stdout=log,
            stderr=log,
        )
        self.addCleanup(self.stop_server)
        with httpx.Client(base_url=self.base, timeout=1, trust_env=False) as client:
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                if self.server.poll() is not None:
                    self.fail((self.root / "http.log").read_text())
                try:
                    if client.get("/v1/me").status_code == 401:
                        break
                except httpx.TransportError:
                    pass
                time.sleep(0.03)
            else:
                self.fail("Native HTTP management listener did not start")

    def stop_server(self):
        if self.server.poll() is None:
            self.server.terminate()
            self.server.wait(timeout=10)

    def phase(self, phase, **values):
        payload = {
            "phase": phase,
            "base": self.base,
            "receiver": self.receiver,
            "token": self.tokens["operator"],
            "reader": self.tokens["reader"],
            "other": self.tokens["other"],
            "item": self.item,
            "title": self.title,
            **values,
        }
        result = subprocess.run(
            ["node", str(CLIENT)],
            input=json.dumps(payload),
            capture_output=True,
            text=True,
            timeout=60,
        )
        self.assertEqual(result.returncode, 0, f"{phase}: {result.stderr}")
        self.assertNotIn(RECEIVER_TOKEN, result.stdout)
        self.assertNotIn(self.tokens["operator"], result.stdout)
        return json.loads(result.stdout)

    def native(self, surface, action, **options):
        result = subprocess.run(
            [
                str(NATIVE_BINARY),
                "--db",
                str(self.database),
                "operation",
                surface,
                action,
                "--options",
                json.dumps(options),
            ],
            env={**os.environ, "CATABOLIC_MANAGEMENT_RECEIVER_TOKEN": RECEIVER_TOKEN},
            capture_output=True,
            text=True,
            timeout=60,
        )
        self.assertEqual(result.returncode, 0, result.stderr or result.stdout)
        self.assertNotIn(RECEIVER_TOKEN, result.stdout)
        return json.loads(result.stdout)

    def emit(self, identity):
        return self.native(
            "notify",
            "emit",
            event="projection_updated",
            severity="info",
            subject="private fixture title must not leave the catalog",
            identity=identity,
        )

    def revoke_operator(self):
        with Store(self.database, writable=True) as store:
            revoke(store, self.tokens["operator"].split(".")[0])
        self.phase("revoked")

    def test_generated_webhook_real_delivery_retry_revocation_and_disable(self):
        created = self.phase("webhook-create")
        self.assertEqual(self.received, [])
        self.emit("first")
        self.receiver_status = 503
        result = self.native("notify", "drain")
        self.assertEqual(result["deliveries"][0]["state"], "retry")
        self.phase("webhook-history", **created, count=1, state="retry", attempts=1)
        self.phase("webhook-retry", **created)
        self.receiver_status = 204
        self.assertTrue(self.native("notify", "drain")["complete"])
        self.assertEqual(len(self.received), 2)
        self.assertEqual(self.received[0], self.received[1])
        self.assertIsNone(self.received[0]["auth"])
        self.assertEqual(self.received[0]["body"]["event"], "projection_updated")
        self.assertNotIn("private fixture title", self.received[0]["raw"].decode())
        self.phase("webhook-history", **created, count=1, state="complete")

        self.emit("revoked")
        self.revoke_operator()
        result = self.native("notify", "drain")
        self.assertEqual(result["deliveries"][0]["state"], "repair")
        self.assertEqual(len(self.received), 2)
        self.tokens["operator"] = self.replacement
        self.phase("webhook-retry", **created)
        self.assertTrue(self.native("notify", "drain")["complete"])
        self.assertEqual(len(self.received), 3)
        self.phase("webhook-history", **created, count=2, state="complete")
        self.emit("disabled")
        self.phase("webhook-disable", **created)
        self.assertTrue(self.native("notify", "drain")["complete"])
        self.assertEqual(len(self.received), 3)

    def test_generated_sql_graphql_mapping_real_retry_and_token_revocation(self):
        created = self.phase("mapping-create")
        identifier = {"identifier": created["identifier"]}
        self.assertEqual(self.received, [])
        self.phase("mapping-apply", definition=created["sql"])
        self.assertEqual(self.received, [])
        self.receiver_status = 503
        result = self.native("notify", "drain")
        self.assertEqual(result["deliveries"][0]["state"], "retry")
        self.phase("mapping-history", **identifier, count=1, state="retry")
        self.phase("mapping-retry", **identifier)
        self.receiver_status = 204
        self.assertTrue(self.native("notify", "drain")["complete"])
        self.assertEqual(len(self.received), 2)
        self.assertEqual(self.received[0], self.received[1])
        self.assertEqual(self.received[0]["auth"], "Bearer " + RECEIVER_TOKEN)
        self.assertTrue(self.received[0]["path"].endswith("?ready=true"))
        self.assertEqual(
            self.received[0]["body"], {"id": self.item, "title": self.title}
        )
        self.assertIsNotNone(self.received[0]["idempotency_key"])

        self.phase("mapping-apply", definition=created["graphql"])
        self.assertEqual(len(self.received), 2)
        self.revoke_operator()
        result = self.native("notify", "drain")
        self.assertEqual(result["deliveries"][0]["state"], "repair")
        self.assertEqual(len(self.received), 2)
        self.tokens["operator"] = self.replacement
        self.phase("mapping-history", **identifier, count=2, state="repair")
        self.phase("mapping-retry", **identifier)
        self.assertTrue(self.native("notify", "drain")["complete"])
        self.assertEqual(len(self.received), 3)
        self.assertEqual(self.received[-1]["body"], self.received[0]["body"])
        self.assertNotEqual(
            self.received[-1]["idempotency_key"], self.received[0]["idempotency_key"]
        )
        history = self.phase("mapping-history", **identifier, count=2, state="complete")
        self.assertEqual({row["state"] for row in history["deliveries"]}, {"complete"})
        self.phase("mapping-disable", **identifier)
        self.assertTrue(self.native("notify", "drain")["complete"])
        self.assertEqual(len(self.received), 3)

    def test_generated_watcher_put_admit_native_execution_and_history(self):
        created = self.phase("watcher-create")
        result = self.native("watcher", "run", name=created["name"])
        self.assertTrue(result["complete"], result)
        history = self.phase("watcher-history")
        self.assertEqual(
            history["runs"][0]["result"]["evaluation"]["baseline"]["value"]["ids"],
            [self.file],
        )
        self.assertEqual(self.received, [])


if __name__ == "__main__":
    unittest.main()
