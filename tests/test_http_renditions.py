# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import importlib.util
import os
import shutil
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

from catabolic import rendering
from catabolic.access import grant_put, issue, principal_put
from catabolic.app import Application
from catabolic.artifacts import Artifacts
from catabolic.content_access import revision_of
from catabolic.http_worker_local import run
from catabolic.store import Store, encode


@unittest.skipUnless(
    importlib.util.find_spec("fastapi")
    and importlib.util.find_spec("httpx")
    and shutil.which("ffmpeg"),
    "optional HTTP and real FFmpeg required",
)
class RenditionHTTPTest(unittest.TestCase):
    def setUp(self):
        from fastapi.testclient import TestClient

        from catabolic.http.app import create_app

        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name).resolve()
        source = self.root / "source"
        source.mkdir()
        generated = self.root / "generated"
        generated.mkdir()
        subprocess.run(
            [
                "ffmpeg",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=160x120:rate=5:duration=1",
                "-c:v",
                "mpeg4",
                str(source / "sample.mkv"),
            ],
            check=True,
            capture_output=True,
        )
        self.database = self.root / "catalog.db"
        Store.initialize(self.database)
        with Store(self.database, writable=True) as store:
            app = Application(store)
            app.bind("source", "source", str(source))
            app.scan()
            self.file = store.rows("SELECT id FROM files")[0]["id"]
            self.item = app.put_item("movie", {"fixture": "movie"}, {"title": "Movie"})[
                "id"
            ]
            app.media.associate(self.file, self.item)
            artifacts = Artifacts(app)
            artifacts.bind("generated", generated)
            recipe = artifacts.recipe(
                "preview", "thumbnail", {"max_output_bytes": 16777216}
            )
            self.operation = "thumbnail-approved"
            with store.transaction() as db:
                db.execute(
                    "INSERT INTO api_operations(id,profile,recipe_id,location) VALUES (?,?,?,?)",
                    (self.operation, "default", recipe["id"], "generated"),
                )
                db.execute("INSERT INTO api_sources VALUES ('default','generated')")
                db.execute(
                    "INSERT INTO api_worker_status VALUES (?,?,?,?)",
                    (
                        "default",
                        os.getpid(),
                        encode(rendering.capabilities()),
                        time.time(),
                    ),
                )
            self.headers = []
            for principal in ("one", "two"):
                principal_put(store, "default", principal)
                grant = grant_put(
                    store,
                    principal,
                    {
                        "actions": [
                            "metadata:read",
                            "content:read",
                            "processing:request",
                            "events:read",
                        ],
                        "item_ids": [self.item],
                        "file_ids": [self.file],
                        "derivatives": True,
                        "operation_ids": [self.operation],
                    },
                )
                # Original file metadata can be inspected, but content:read is restricted below.
                definition = {
                    "actions": ["metadata:read", "processing:request", "events:read"],
                    "item_ids": [self.item],
                    "file_ids": [self.file],
                    "operation_ids": [self.operation],
                }
                grant_put(store, principal, definition, grant["grant_id"])
                derivative = grant_put(
                    store,
                    principal,
                    {
                        "actions": ["metadata:read", "content:read"],
                        "item_ids": [self.item],
                        "derivatives": True,
                    },
                )
                token = issue(
                    store, principal, [grant["grant_id"], derivative["grant_id"]]
                )["token"]
                self.headers.append(
                    {"Authorization": "Bearer " + token, "Idempotency-Key": "request-1"}
                )
            self.body = {
                "item_id": self.item,
                "source_file_id": self.file,
                "source_revision": revision_of(store, "default", self.file),
                "operation_id": self.operation,
            }
        self.client = TestClient(create_app(self.database, read_only=False))
        self.addCleanup(self.client.close)

    def test_shared_work_restart_ticket_and_independent_cancel(self):
        from concurrent.futures import ThreadPoolExecutor
        from threading import Barrier

        started = Barrier(2)

        def submit(headers):
            started.wait(timeout=5)
            for _ in range(100):
                response = self.client.post(
                    "/v1/rendition-requests", headers=headers, json=self.body
                )
                if response.status_code != 503:
                    return response
                time.sleep(0.01)
            return response

        with ThreadPoolExecutor(max_workers=2) as pool:
            responses = list(pool.map(submit, self.headers))
        for response in responses:
            self.assertEqual(response.status_code, 202, response.text)
        first, second = [response.json() for response in responses]
        self.assertNotEqual(first["request_id"], second["request_id"])
        with Store(self.database) as store:
            self.assertEqual(len(store.rows("SELECT id FROM processing_jobs")), 1)
            self.assertEqual(len(store.rows("SELECT job_id FROM api_reservations")), 1)
        self.assertEqual(
            self.client.get(second["status_url"], headers=self.headers[0]).status_code,
            404,
        )
        self.assertEqual(
            self.client.get(
                f"/v1/files/{self.file}/content?revision={self.body['source_revision']}",
                headers=self.headers[0],
            ).status_code,
            404,
        )
        self.client.post(first["status_url"] + "/cancel", headers=self.headers[0])
        replay = self.client.get("/v1/events", headers=self.headers[1]).json()
        result = run(self.database, once=True)
        self.assertTrue(result["complete"], result)
        from fastapi.testclient import TestClient

        from catabolic.http.app import create_app

        with TestClient(create_app(self.database, read_only=False)) as restarted:
            status = restarted.get(second["status_url"], headers=self.headers[1])
            self.assertEqual(status.status_code, 200, status.text)
            self.assertEqual(status.json()["state"], "ready", status.text)
            ready = status.json()["result"]
            self.assertIsNotNone(ready, status.text)
            ticket = restarted.post(
                "/v1/content-access",
                headers=self.headers[1],
                json={"file_id": ready["file_id"], "revision": ready["revision"]},
            )
            self.assertEqual(ticket.status_code, 200, ticket.text)
            response = restarted.get(
                ticket.json()["content_path"], headers={"Range": "bytes=0-7"}
            )
            self.assertEqual(response.status_code, 206, response.text)
            self.assertEqual(response.content, b"\x89PNG\r\n\x1a\n")
            events = restarted.get(
                "/v1/events",
                headers=self.headers[1],
                params={"cursor": replay["cursor"]},
            )
            self.assertEqual(events.status_code, 200, events.text)
            self.assertTrue(any(e["state"] == "ready" for e in events.json()["events"]))
            self.assertNotIn(first["request_id"], events.text)
            self.assertEqual(
                restarted.get(first["status_url"], headers=self.headers[0]).json()[
                    "state"
                ],
                "cancelled",
            )
        with Store(self.database) as store:
            self.assertEqual(store.rows("SELECT * FROM api_reservations"), [])
            self.assertEqual(store.rows("SELECT * FROM processing_rules"), [])
            self.assertEqual(store.rows("SELECT * FROM consumer_bindings"), [])

    def test_idempotency_conflict_and_approved_operations_only(self):
        first = self.client.post(
            "/v1/rendition-requests", headers=self.headers[0], json=self.body
        )
        self.assertEqual(first.status_code, 202, first.text)
        same = self.client.post(
            "/v1/rendition-requests", headers=self.headers[0], json=self.body
        )
        self.assertEqual(same.json()["request_id"], first.json()["request_id"])
        changed = {**self.body, "operation_id": "unapproved"}
        self.assertEqual(
            self.client.post(
                "/v1/rendition-requests", headers=self.headers[0], json=changed
            ).status_code,
            403,
        )
        self.assertEqual(
            self.client.post(
                "/v1/rendition-requests",
                headers=self.headers[0],
                json={**self.body, "path": "/tmp/escape"},
            ).status_code,
            422,
        )

    def test_storage_reservation_failure_rolls_back_job_and_demand(self):
        from types import SimpleNamespace
        from unittest.mock import patch

        with patch(
            "catabolic.rendition_requests.os.fstatvfs",
            return_value=SimpleNamespace(f_bavail=1, f_frsize=1),
        ):
            response = self.client.post(
                "/v1/rendition-requests", headers=self.headers[0], json=self.body
            )
        self.assertEqual(response.status_code, 409, response.text)
        self.assertEqual(response.json()["code"], "storage_reservation_unavailable")
        with Store(self.database) as store:
            self.assertEqual(store.rows("SELECT id FROM api_requests"), [])
            self.assertEqual(store.rows("SELECT id FROM processing_jobs"), [])
            self.assertEqual(store.rows("SELECT * FROM api_reservations"), [])

    def test_different_authorized_body_conflicts_and_disabled_operation_blocks_worker(
        self,
    ):
        import json

        first = self.client.post(
            "/v1/rendition-requests", headers=self.headers[0], json=self.body
        )
        self.assertEqual(first.status_code, 202, first.text)
        with Store(self.database, writable=True) as store:
            with store.transaction() as db:
                db.execute(
                    "INSERT INTO api_operations SELECT 'other-approved',profile,recipe_id,location,enabled FROM api_operations WHERE id=?",
                    (self.operation,),
                )
            grant = store.rows("SELECT * FROM api_grants WHERE principal='one'")[0]
            definition = json.loads(grant["definition"])
            definition["actions"].append("processing:request")
            definition["operation_ids"] = [self.operation, "other-approved"]
            grant_put(store, "one", definition, grant["id"])
        changed = self.client.post(
            "/v1/rendition-requests",
            headers=self.headers[0],
            json={**self.body, "operation_id": "other-approved"},
        )
        self.assertEqual(changed.status_code, 409, changed.text)
        self.assertEqual(changed.json()["code"], "idempotency_conflict")
        with Store(self.database, writable=True) as store:
            with store.transaction() as db:
                db.execute("UPDATE api_operations SET enabled=0")
        run(self.database, once=True)
        status = self.client.get(
            first.json()["status_url"], headers=self.headers[0]
        ).json()
        self.assertEqual(status["state"], "blocked")
        self.assertTrue(status["blockers"])
        with Store(self.database) as store:
            self.assertEqual(store.rows("SELECT id FROM processing_artifacts"), [])

    def test_cancelled_demand_cannot_evade_pending_request_cap(self):
        for index in range(10):
            response = self.client.post(
                "/v1/rendition-requests",
                headers={**self.headers[0], "Idempotency-Key": f"cancel-{index}"},
                json=self.body,
            )
            self.assertEqual(response.status_code, 202, response.text)
            self.client.post(
                response.json()["status_url"] + "/cancel", headers=self.headers[0]
            )
        rejected = self.client.post(
            "/v1/rendition-requests",
            headers={**self.headers[0], "Idempotency-Key": "over-limit"},
            json=self.body,
        )
        self.assertEqual(rejected.status_code, 429, rejected.text)
        run(self.database, once=True)
        with Store(self.database) as store:
            self.assertEqual(store.rows("SELECT * FROM api_reservations"), [])
            self.assertEqual(
                {row["state"] for row in store.rows("SELECT state FROM api_requests")},
                {"cancelled"},
            )
