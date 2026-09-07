# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import copy
import json
import subprocess
import sys
import threading
import unittest
from concurrent.futures import ThreadPoolExecutor
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from unittest.mock import patch

from catabolic.domain import CatabolicError
from catabolic.outputs import Outputs
from catabolic.processors import Processors, dispatch_job
from catabolic.store import Store
from tests import test_outputs


class ProcessorTest(unittest.TestCase):
    setUp = test_outputs.OutputTest.setUp

    def setup_processor(self, endpoint="http://127.0.0.1:9", capacity=1):
        self.processors = Processors(self.app)
        self.processors.put("fixture", {"endpoint": endpoint, "capacity": capacity})
        self.definition = self.outputs.define("processor-output", {"purpose": "custom"})
        return self.processors.enqueue(
            "fixture",
            self.files["source.mkv"],
            self.item,
            self.definition["id"],
            {"preset": "fixture"},
        )

    def receipt(self, lease):
        request = lease["request"]
        return {
            "version": 1,
            "database_id": request["database_id"],
            "profile": "default",
            "producer": "fixture",
            "instance": lease["worker"],
            "job_id": lease["id"],
            "attempt": str(lease["generation"]),
            "configuration_digest": request["configuration_digest"],
            "tools": {"fixture": "1"},
            "outcome": "complete",
            "source": request["source"],
            "outputs": [
                {
                    "location": "media",
                    "path": "external.mkv",
                    "definition_id": self.definition["id"],
                    "size": len(b"external.mkv"),
                }
            ],
        }

    def test_capacity_expiry_fences_old_worker_and_preserves_attempts(self):
        job = self.setup_processor()
        first = self.processors.claim("fixture", "worker-a")
        self.assertEqual(first["id"], job["id"])
        self.assertNotIn("lease_token", self.processors.job(job["id"]))
        self.assertFalse(self.processors.claim("fixture", "worker-b")["claimed"])
        with self.store.transaction() as db:
            db.execute("UPDATE processor_jobs SET lease_until=0")
        second = self.processors.claim("fixture", "worker-b")
        self.assertEqual(second["generation"], 2)
        for operation in (
            lambda: self.processors.heartbeat(first["id"], first["lease_token"]),
            lambda: self.processors.complete(
                first["id"], first["lease_token"], self.receipt(first)
            ),
        ):
            with self.assertRaisesRegex(CatabolicError, "lease"):
                operation()
        result = self.processors.complete(
            second["id"], second["lease_token"], self.receipt(second)
        )
        self.assertFalse(result["reused"])
        self.assertEqual(self.processors.job(job["id"])["state"], "complete")
        self.assertEqual(
            [
                r["state"]
                for r in self.store.rows(
                    "SELECT state FROM processor_attempts ORDER BY generation"
                )
            ],
            ["expired", "complete"],
        )
        self.assertEqual((self.root / "media/source.mkv").read_bytes(), b"source.mkv")

    def test_receipt_mismatch_and_lease_expiry_during_hash_roll_back(self):
        self.setup_processor()
        lease = self.processors.claim("fixture", "worker")
        invalid = self.receipt(lease)
        invalid["attempt"] = "999"
        with self.assertRaisesRegex(CatabolicError, "match"):
            self.processors.complete(lease["id"], lease["lease_token"], invalid)
        original = Outputs.record

        def expire(adapter, *args, **kwargs):
            result = original(adapter, *args, **kwargs)
            adapter.store.db.execute("UPDATE processor_jobs SET lease_until=0")
            return result

        with (
            patch.object(Outputs, "record", expire),
            self.assertRaisesRegex(CatabolicError, "lease"),
        ):
            self.processors.complete(
                lease["id"], lease["lease_token"], self.receipt(lease)
            )
        self.assertEqual(self.store.rows("SELECT * FROM external_receipts"), [])
        self.assertEqual(self.store.rows("SELECT * FROM media_outputs"), [])
        self.assertEqual(self.processors.job(lease["id"])["state"], "leased")

    def test_idempotent_enqueue_explicit_retry_and_cancel(self):
        job = self.setup_processor()
        self.assertEqual(self.setup_processor()["id"], job["id"])
        lease = self.processors.claim("fixture", "worker")
        self.processors.transition(lease["id"], lease["lease_token"], "failed")
        self.assertFalse(self.processors.claim("fixture", "worker")["claimed"])
        self.processors.retry(job["id"])
        lease = self.processors.claim("fixture", "worker")
        self.assertEqual(lease["generation"], 2)
        self.processors.cancel(job["id"])
        with self.assertRaisesRegex(CatabolicError, "lease"):
            self.processors.complete(
                lease["id"], lease["lease_token"], self.receipt(lease)
            )

    def test_independent_processes_cannot_claim_same_job(self):
        self.setup_processor()
        self.store.close()

        def claim(worker):
            result = subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "catabolic",
                    "--db",
                    str(self.path),
                    "--json",
                    "processor",
                    "claim",
                    "fixture",
                    "--worker",
                    worker,
                ],
                capture_output=True,
                text=True,
                timeout=20,
            )
            if result.returncode == 2 and "another Catabolic writer" in result.stderr:
                return {"claimed": False, "busy": True}
            self.assertEqual(result.returncode, 0, result.stderr)
            return json.loads(result.stdout)

        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(claim, ["one", "two"]))
        self.assertEqual(sum(r["claimed"] for r in results), 1)
        self.assertFalse(claim("retry-after-contention")["claimed"])

    def test_real_http_put_poll_and_atomic_receipt_completion(self):
        requests = []
        accepted = {}
        database = self.path
        locks_available = []

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_PUT(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                requests.append(("PUT", self.path, body))
                accepted.update(body)
                with Store(database, writable=True):
                    locks_available.append(True)
                if len(requests) == 1:
                    self.close_connection = True
                    return
                self.respond({"state": "running"})

            def do_GET(self):
                requests.append(("GET", self.path, None))
                self.respond({"state": "complete", "receipt": receipt})

            def respond(self, value):
                data = json.dumps(value).encode()
                self.send_response(200)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        self.setup_processor(f"http://127.0.0.1:{server.server_port}")
        lease = self.processors.claim("fixture", "worker")
        receipt = copy.deepcopy(self.receipt(lease))
        self.store.close()
        with self.assertRaisesRegex(CatabolicError, "HTTP"):
            dispatch_job(self.path, "default", lease["id"], lease["lease_token"])
        result = dispatch_job(self.path, "default", lease["id"], lease["lease_token"])
        self.assertEqual(result["state"], "submitted")
        result = dispatch_job(self.path, "default", lease["id"], lease["lease_token"])
        self.assertFalse(result["reused"])
        self.assertEqual([r[0] for r in requests], ["PUT", "PUT", "GET"])
        self.assertEqual(requests[0], requests[1])
        self.assertEqual(locks_available, [True, True])
        self.assertEqual(requests[0][1], requests[1][1])
        self.assertEqual(accepted["source_location"]["path"], "source.mkv")
        with Store(self.path) as store:
            self.assertEqual(
                store.rows("SELECT state FROM processor_jobs")[0]["state"], "complete"
            )
            self.assertEqual(len(store.rows("SELECT * FROM external_receipts")), 1)
