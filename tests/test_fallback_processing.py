# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import importlib.util
import json
import shutil
import unittest

from catabolic.access import grant_put
from catabolic.fallback_policies import Policies
from catabolic.http_worker_local import run
from catabolic.saved_queries import Queries
from catabolic.store import Store
from tests import test_http_renditions as fixtures


@unittest.skipUnless(
    importlib.util.find_spec("fastapi")
    and importlib.util.find_spec("httpx")
    and shutil.which("ffmpeg"),
    "HTTP/media dependencies",
)
class LogicalProcessingTest(unittest.TestCase):
    setUp = fixtures.RenditionHTTPTest.setUp

    def test_logical_admission_is_idempotent_and_source_stays_pinned(self):
        with Store(self.database, writable=True) as store:
            query = Queries(store).put(
                "input",
                {
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS file_id FROM files",
                    }
                },
            )["id"]
            policy = Policies(store).put(
                "input",
                {
                    "fallbacks": [{"name": "input", "query_id": query}],
                    "within_tier": {"tie_break": "file_id"},
                },
            )["id"]
            for row in store.rows("SELECT * FROM api_grants"):
                definition = json.loads(row["definition"])
                if "processing:request" in definition["actions"]:
                    grant_put(
                        store,
                        row["principal"],
                        {**definition, "fallback_policy_ids": [policy]},
                        row["id"],
                    )
        body = {
            "item_id": self.item,
            "fallback_policy_id": policy,
            "operation_id": self.operation,
        }
        response = self.client.post(
            "/v1/rendition-requests/logical", headers=self.headers[0], json=body
        )
        self.assertEqual(response.status_code, 202, response.text)
        self.assertEqual(
            self.client.post(
                "/v1/rendition-requests/logical", headers=self.headers[0], json=body
            ).json()["request_id"],
            response.json()["request_id"],
        )
        conflict = self.client.post(
            "/v1/rendition-requests/logical",
            headers=self.headers[0],
            json={**body, "variant": "different"},
        )
        self.assertEqual(conflict.status_code, 409)
        with Store(self.database) as store:
            jobs = store.rows("SELECT * FROM processing_jobs")
            self.assertEqual(len(jobs), 1)
            self.assertEqual(jobs[0]["file_id"], self.file)
            self.assertEqual(len(store.rows("SELECT * FROM api_logical_demands")), 1)
        self.assertEqual(
            self.client.get(
                f"/v1/files/{self.file}/content?revision={self.body['source_revision']}",
                headers=self.headers[0],
            ).status_code,
            404,
        )
        exact = self.client.post(
            "/v1/rendition-requests",
            headers={**self.headers[0], "Idempotency-Key": "exact-same-work"},
            json=self.body,
        )
        self.assertEqual(exact.status_code, 202, exact.text)
        with Store(self.database) as store:
            self.assertEqual(len(store.rows("SELECT * FROM processing_jobs")), 1)
        run(self.database, once=True)
        status = self.client.get(response.json()["status_url"], headers=self.headers[0])
        self.assertEqual(status.json()["state"], "ready", status.text)
        with Store(self.database) as store:
            self.assertEqual(
                store.rows("SELECT file_id FROM processing_jobs")[0]["file_id"],
                self.file,
            )
