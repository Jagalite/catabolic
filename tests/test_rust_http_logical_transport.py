# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Live native fallback resolution and logical demand, no Python execution fallback."""

import json
import os
import subprocess
import unittest
from pathlib import Path
from unittest.mock import patch

from catabolic.store import Store
from tests import test_fallback_http as fallback
from tests import test_fallback_processing as logical
from tests import test_http_renditions as rendition
from tests import test_rust_http_transport as transport

NATIVE = Path(
    os.environ.get("CATABOLIC_NATIVE_BINARY", "target/debug/catabolic-native")
).resolve()


class NativeFallbackHTTPTest(unittest.TestCase):
    stop_server = transport.NativeHTTPTransportTest.stop_server
    configure = fallback.FallbackHTTPTest.configure

    def setUp(self):
        transport.NativeHTTPTransportTest.setUp(self)

    def start_native_server(self):
        transport.NativeHTTPTransportTest.start_native_server(
            self, env={**os.environ, "CATABOLIC_NATIVE_BINARY": str(NATIVE)}
        )

    def test_generated_logical_client(self):
        policy = self.configure()
        script = Path(__file__).parent / "clients/v1_7/build/logical.js"
        self.assertTrue(script.is_file(), "run npm run test:1.7 in tests/clients")
        result = subprocess.run(
            ["node", str(script.resolve())],
            input=json.dumps(
                {
                    "base": self.base,
                    "token": self.headers["Authorization"][7:],
                    "item": self.item,
                    "policy": policy,
                    "file": self.file,
                    "revision": self.revision,
                    "payload": self.payload.hex(),
                }
            ),
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    test_policy_approval_does_not_grant_candidate_access = (
        fallback.FallbackHTTPTest.test_policy_approval_does_not_grant_candidate_access
    )
    test_policy_query_uses_restricted_sql_and_graphql_is_scoped = fallback.FallbackHTTPTest.test_policy_query_uses_restricted_sql_and_graphql_is_scoped


class NativeLogicalProcessingHTTPTest(unittest.TestCase):
    stop_server = transport.NativeHTTPTransportTest.stop_server

    def setUp(self):
        rendition.RenditionHTTPTest.setUp(self)
        self.client.close()
        transport.NativeHTTPTransportTest.start_native_server(
            self, env={**os.environ, "CATABOLIC_NATIVE_BINARY": str(NATIVE)}
        )

    def worker(self, database, once):
        self.assertTrue(once)
        run = subprocess.run(
            [str(NATIVE), "--db", str(database), "api", "worker", "--once"],
            capture_output=True,
            text=True,
            timeout=60,
        )
        self.assertEqual(run.returncode, 0, run.stdout + run.stderr)

    def test_logical_admission_is_idempotent_and_source_stays_pinned(self):
        # Reuse frozen assertions; replace only its explicit worker call with
        # the real native process. Fixture creation is the sole Python owner.
        with patch.object(logical, "run", self.worker):
            logical.LogicalProcessingTest.test_logical_admission_is_idempotent_and_source_stays_pinned(
                self
            )

        with Store(self.database) as store:
            self.assertEqual(store.rows("SELECT * FROM api_worker_status"), [])
            row = store.rows("SELECT * FROM api_logical_demands")[0]
        replay = self.client.post(
            "/v1/rendition-requests/logical",
            headers=self.headers[0],
            json=json.loads(row["body"]),
        )
        self.assertEqual(replay.status_code, 200, replay.text)
        self.assertEqual(replay.json()["request_id"], row["request_id"])
        missing = self.client.post(
            "/v1/rendition-requests/logical",
            headers={
                k: v
                for k, v in self.headers[0].items()
                if k.lower() != "idempotency-key"
            },
            json=json.loads(row["body"]),
        )
        self.assertEqual(missing.status_code, 422, missing.text)
        fresh = self.client.post(
            "/v1/rendition-requests/logical",
            headers={**self.headers[0], "Idempotency-Key": "no-worker-new-demand"},
            json=json.loads(row["body"]),
        )
        self.assertEqual(fresh.status_code, 409, fresh.text)
        self.assertEqual(fresh.json()["code"], "worker_unavailable")
