# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Frozen Python demand owner versus actual native admission on independent catalogs."""

import hashlib
import json
import os
import shutil
import sqlite3
import subprocess
import unittest

from catabolic.access import authenticate
from catabolic.app import Application
from catabolic.rendition_requests import admit, cancel, retry, status
from catabolic.store import Store
from tests import test_http_renditions as fixtures
from tests.test_rust_migration import BINARY


class NativeRenditionTest(unittest.TestCase):
    setUp = fixtures.RenditionHTTPTest.setUp

    def pair(self):
        self.reference = self.root / "reference.db"
        shutil.copyfile(self.database, self.reference)
        self.ids = [{}, {}]
        with Store(self.database) as store:
            self.capabilities = json.loads(
                store.rows("SELECT capabilities FROM api_worker_status")[0][
                    "capabilities"
                ]
            )

    def native(self, action, principal=0, ok=True, **request):
        run = subprocess.run(
            [
                str(BINARY),
                "--db",
                str(self.database),
                "--json",
                "operation",
                "rendition-request",
                action,
                "--options",
                json.dumps(request),
            ],
            env={
                **os.environ,
                "CATABOLIC_API_TOKEN": self.headers[principal]["Authorization"].split(
                    " ", 1
                )[1],
            },
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(run.returncode, 0 if ok else 2, run.stdout + run.stderr)
        return json.loads(run.stdout) if ok else run.stderr

    def oracle(self, action, principal=0, **request):
        token = self.headers[principal]["Authorization"].split(" ", 1)[1]
        with Store(self.reference, writable=True) as store:
            access = authenticate(store, token)
            if action == "admit":
                return admit(
                    access,
                    request["body"],
                    request["idempotency_key"],
                    self.capabilities,
                )
            return {"status": status, "cancel": cancel, "retry": retry}[action](
                access, request["id"]
            )

    def both(self, action, principal=0, key="request", **request):
        left = self.native(action, principal, **request)
        if "id" in request:
            request["id"] = self.ids[principal][request["id"]]
        right = self.oracle(action, principal, **request)
        self.ids[principal][left["request_id"]] = right["request_id"]
        for result in (left, right):
            result.pop("request_id")
            result.pop("status_url")
        self.assertEqual(left, right)
        self.compare()
        return next(reversed(self.ids[principal]))

    def compare(self):
        def snapshot(path):
            with sqlite3.connect(path) as db:
                db.row_factory = sqlite3.Row
                jobs = list(db.execute("SELECT * FROM processing_jobs ORDER BY rowid"))
                requests = list(db.execute("SELECT * FROM api_requests ORDER BY rowid"))
                ids = {r["id"]: f"job-{i}" for i, r in enumerate(jobs)}
                ids.update({r["id"]: f"request-{i}" for i, r in enumerate(requests)})

                def row(value):
                    return {
                        k: ids.get(v, v) if isinstance(v, str) else v
                        for k, v in dict(value).items()
                        if k not in ("created_at", "finished_at", "started_at")
                    }

                return {
                    table: [
                        row(r)
                        for r in db.execute(f"SELECT * FROM {table} ORDER BY rowid")
                    ]
                    for table in (
                        "processing_jobs",
                        "api_requests",
                        "api_reservations",
                        "api_events",
                        "fallback_epoch",
                    )
                }

        self.assertEqual(snapshot(self.database), snapshot(self.reference))

    def test_worker_marks_changed_source_before_disabled_operation(self):
        from catabolic.http_worker_local import tick

        self.pair()
        self.both("admit", body=self.body, idempotency_key="precedence")
        for path in (self.database, self.reference):
            with Store(path, writable=True) as store:
                with store.transaction() as db:
                    db.execute(
                        "UPDATE observations SET mtime_ns=mtime_ns+1 WHERE file_id=?",
                        (self.file,),
                    )
                    db.execute(
                        "UPDATE api_operations SET enabled=0 WHERE id=?",
                        (self.operation,),
                    )
                    db.execute("UPDATE api_worker_status SET worker_pid=0")
        result = subprocess.run(
            [
                str(BINARY),
                "--db",
                str(self.database),
                "--json",
                "operation",
                "rendition-worker",
                "tick",
                "--options",
                json.dumps({"capabilities": self.capabilities}),
            ],
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        tick(self.reference, "default", self.capabilities)
        self.compare()
        with Store(self.database) as store:
            self.assertEqual(
                store.rows("SELECT state FROM processing_jobs"), [{"state": "changed"}]
            )
            self.assertEqual(
                store.rows("SELECT state,reserved_bytes FROM api_requests"),
                [{"state": "stale", "reserved_bytes": 0}],
            )
            self.assertEqual(store.rows("SELECT * FROM api_reservations"), [])

    def test_shared_work_independent_cancel_and_pinned_retry(self):
        self.pair()
        first = self.both("admit", body=self.body, idempotency_key="one")
        second = self.both("admit", principal=1, body=self.body, idempotency_key="two")
        self.both("admit", body=self.body, idempotency_key="one")
        self.both("cancel", id=first)
        self.both("cancel", principal=1, id=second)
        with Store(self.database) as store:
            job = store.rows("SELECT * FROM processing_jobs")[0]
            self.assertEqual(job["state"], "queued")
            self.assertEqual(len(store.rows("SELECT * FROM api_reservations")), 1)
            pinned = (job["file_id"], job["snapshot"], job["options"], job["cache_key"])
        self.both("retry", id=first)
        with Store(self.database) as store:
            jobs = store.rows("SELECT * FROM processing_jobs")
            self.assertEqual(len(jobs), 1)
            self.assertEqual(
                (
                    jobs[0]["file_id"],
                    jobs[0]["snapshot"],
                    jobs[0]["options"],
                    jobs[0]["cache_key"],
                ),
                pinned,
            )

    def test_competing_native_admissions_share_one_physical_reservation(self):
        from concurrent.futures import ThreadPoolExecutor
        from threading import Barrier
        import time

        self.pair()
        barrier = Barrier(2)

        def submit(principal):
            token = self.headers[principal]["Authorization"].split(" ", 1)[1]
            request = {"body": self.body, "idempotency_key": f"parallel-{principal}"}
            command = [str(BINARY), "--db", str(self.database), "--json", "operation",
                       "rendition-request", "admit", "--options", json.dumps(request)]
            barrier.wait(timeout=10)
            for attempt in range(40):
                result = subprocess.run(command, env={**os.environ, "CATABOLIC_API_TOKEN": token},
                                        capture_output=True, text=True, timeout=120)
                if result.returncode == 0:
                    return json.loads(result.stdout)
                if "another Catabolic writer" not in result.stderr:
                    raise AssertionError((result.returncode, result.stdout, result.stderr))
                time.sleep(0.05)
            raise AssertionError("catalog writer remained busy after bounded admission retries")

        with ThreadPoolExecutor(max_workers=2) as pool:
            first, second = list(pool.map(submit, (0, 1)))
        self.assertNotEqual(first["request_id"], second["request_id"])
        with Store(self.database) as store:
            jobs = store.rows("SELECT id,state FROM processing_jobs")
            requests = store.rows("SELECT id,job_id,state FROM api_requests")
            reservations = store.rows("SELECT job_id FROM api_reservations")
        self.assertEqual(len(jobs), 1)
        self.assertEqual(len(requests), 2)
        self.assertEqual({row["job_id"] for row in requests}, {jobs[0]["id"]})
        self.assertEqual(reservations, [{"job_id": jobs[0]["id"]}])
        self.assertEqual({row["state"] for row in requests}, {"queued"})

    def test_concurrent_cancel_and_worker_completion_keep_survivor_and_release_once(self):
        from concurrent.futures import ThreadPoolExecutor
        from threading import Barrier
        import time

        self.pair()
        first = self.native("admit", body=self.body, idempotency_key="race-one")["request_id"]
        second = self.native("admit", principal=1, body=self.body, idempotency_key="race-two")["request_id"]
        token = self.headers[0]["Authorization"].split(" ", 1)[1]
        barrier = Barrier(2)
        worker = [str(BINARY), "--db", str(self.database), "--json", "operation",
                  "rendition-worker", "tick", "--options", "{}"]
        cancel = [str(BINARY), "--db", str(self.database), "--json", "operation",
                  "rendition-request", "cancel", "--options", json.dumps({"id": first})]

        def execute(command, env):
            barrier.wait(timeout=10)
            for _ in range(120):
                result = subprocess.run(command, env=env, capture_output=True, text=True, timeout=180)
                if result.returncode == 0:
                    return json.loads(result.stdout)
                if "another Catabolic writer" not in result.stderr:
                    raise AssertionError((result.returncode, result.stdout, result.stderr))
                time.sleep(0.05)
            raise AssertionError("catalog writer remained busy during completion race")

        with ThreadPoolExecutor(max_workers=2) as pool:
            worker_result = pool.submit(execute, worker, os.environ.copy())
            cancel_result = pool.submit(execute, cancel, {**os.environ, "CATABOLIC_API_TOKEN": token})
            worker_result.result()
            cancel_result.result()
        self.assertEqual(self.native("status", id=first)["state"], "cancelled")
        survivor = self.native("status", principal=1, id=second)
        self.assertEqual(survivor["state"], "ready")
        self.assertIsNotNone(survivor["result"])
        with Store(self.database) as store:
            self.assertEqual(store.rows("SELECT state FROM processing_jobs"), [{"state": "complete"}])
            self.assertEqual(store.rows("SELECT * FROM api_reservations"), [])
            self.assertEqual(len(store.rows("SELECT * FROM processing_artifacts WHERE state='ready'")), 1)
            self.assertTrue(all(row["reserved_bytes"] == 0 for row in store.rows("SELECT reserved_bytes FROM api_requests")))
            events_before = store.rows("SELECT * FROM api_events ORDER BY rowid")
        repeated = subprocess.run(worker, capture_output=True, text=True, timeout=180)
        self.assertEqual(repeated.returncode, 0, repeated.stdout + repeated.stderr)
        with Store(self.database) as store:
            self.assertEqual(store.rows("SELECT * FROM api_events ORDER BY rowid"), events_before)
            self.assertEqual(len(store.rows("SELECT * FROM processing_artifacts WHERE state='ready'")), 1)
            self.assertEqual(store.rows("SELECT * FROM api_reservations"), [])

    def test_request_limit_counts_cancelled_demands_and_rolls_back(self):
        self.pair()
        for index in range(10):
            request = self.both(
                "admit", body=self.body, idempotency_key=f"request-{index}"
            )
            self.both("cancel", id=request)
        self.assertIn(
            "request_limit",
            self.native("admit", ok=False, body=self.body, idempotency_key="eleven"),
        )
        with self.assertRaisesRegex(Exception, "request_limit"):
            self.oracle("admit", body=self.body, idempotency_key="eleven")
        self.compare()

    def test_storage_failure_rolls_back_job_request_reservation(self):
        self.pair()
        for path in (self.database, self.reference):
            with Store(path, writable=True) as store:
                recipe = store.rows("SELECT recipe_id FROM api_operations")[0][
                    "recipe_id"
                ]
                definition = json.loads(
                    store.rows(
                        "SELECT definition FROM processing_recipes WHERE id=?",
                        (recipe,),
                    )[0]["definition"]
                )
                definition["reserve_bytes"] = 2**62
                from catabolic.store import encode

                with store.transaction() as db:
                    db.execute(
                        "UPDATE processing_recipes SET definition=? WHERE id=?",
                        (encode(definition), recipe),
                    )
        self.assertIn(
            "storage_reservation_unavailable",
            self.native("admit", ok=False, body=self.body, idempotency_key="space"),
        )
        with self.assertRaisesRegex(Exception, "storage_reservation_unavailable"):
            self.oracle("admit", body=self.body, idempotency_key="space")
        self.compare()
        with Store(self.database) as store:
            self.assertEqual(store.rows("SELECT * FROM processing_jobs"), [])

    def test_live_worker_completes_surviving_demand_without_source_change(self):
        self.pair()
        first = self.both("admit", body=self.body, idempotency_key="one")
        second = self.both("admit", principal=1, body=self.body, idempotency_key="two")
        self.both("cancel", id=first)
        before = hashlib.sha256(
            (self.root / "source/sample.mkv").read_bytes()
        ).hexdigest()
        result = subprocess.run(
            [
                str(BINARY),
                "--db",
                str(self.database),
                "--json",
                "operation",
                "rendition-worker",
                "tick",
                "--options",
                "{}",
            ],
            capture_output=True,
            text=True,
            timeout=180,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.native("status", id=first)["state"], "cancelled")
        ready = self.native("status", principal=1, id=second)
        self.assertEqual(ready["state"], "ready", ready)
        self.assertIsNotNone(ready["result"])
        self.assertEqual(
            before,
            hashlib.sha256((self.root / "source/sample.mkv").read_bytes()).hexdigest(),
        )
        with Store(self.database) as store:
            from catabolic.outputs import RENDITIONS_SQL
            from catabolic.rendition_publication import ready as publication_ready

            outputs = store.rows(f"SELECT * FROM ({RENDITIONS_SQL})")
            self.assertEqual(len(outputs), 1)
            publication_ready(Application(store), outputs[0])
            self.assertEqual(store.rows("SELECT * FROM api_reservations"), [])

    def test_changed_source_and_disabled_operation_revalidate_before_retry(self):
        self.pair()
        request = self.both("admit", body=self.body, idempotency_key="pinned")
        self.both("cancel", id=request)
        for path in (self.database, self.reference):
            with Store(path, writable=True) as store:
                with store.transaction() as db:
                    db.execute(
                        "UPDATE api_operations SET enabled=0 WHERE id=?",
                        (self.operation,),
                    )
        self.assertIn(
            "operation_not_approved", self.native("retry", id=request, ok=False)
        )
        with self.assertRaisesRegex(Exception, "operation_not_approved"):
            self.oracle("retry", id=self.ids[0][request])
        self.compare()
        for path in (self.database, self.reference):
            with Store(path, writable=True) as store:
                with store.transaction() as db:
                    db.execute(
                        "UPDATE api_operations SET enabled=1 WHERE id=?",
                        (self.operation,),
                    )
                    db.execute(
                        "UPDATE observations SET mtime_ns=mtime_ns+1 WHERE file_id=?",
                        (self.file,),
                    )
        self.assertIn("revision_mismatch", self.native("retry", id=request, ok=False))
        with self.assertRaisesRegex(Exception, "revision_mismatch"):
            self.oracle("retry", id=self.ids[0][request])
        self.compare()

    def test_callback_registration_and_token_rotation_are_atomic(self):
        from catabolic.access import grant_put, issue

        self.pair()
        for path in (self.database, self.reference):
            with Store(path, writable=True) as store:
                grants = store.rows("SELECT * FROM api_grants WHERE principal='one'")
                for grant in grants:
                    definition = json.loads(grant["definition"])
                    if "processing:request" in definition["actions"]:
                        grant_put(
                            store,
                            "one",
                            {
                                **definition,
                                "callback_origins": ["https://callback.example"],
                            },
                            grant["id"],
                        )
        request = self.both(
            "admit",
            body={**self.body, "callback_url": "https://callback.example/result"},
            idempotency_key="callback",
        )
        self.both("cancel", id=request)
        with Store(self.database, writable=True) as store:
            grants = [
                r["id"]
                for r in store.rows("SELECT id FROM api_grants WHERE principal='one'")
            ]
            token = issue(store, "one", grants)["token"]
            row = store.rows(
                "SELECT * FROM api_tokens WHERE id=?", (token.split(".")[0],)
            )[0]
        with Store(self.reference, writable=True) as store:
            with store.transaction() as db:
                db.execute(
                    "INSERT INTO api_tokens VALUES (?,?,?,?,?,?)", tuple(row.values())
                )
                db.execute(
                    "INSERT INTO api_audit(principal,action,resource) VALUES ('one','token.issue',?)",
                    (row["id"],),
                )
        self.headers[0]["Authorization"] = "Bearer " + token
        self.both("retry", id=request)
        for path in (self.database, self.reference):
            with Store(path) as store:
                self.assertEqual(
                    store.rows("SELECT token_id FROM api_requests")[0]["token_id"],
                    row["id"],
                )
                self.assertEqual(
                    store.rows("SELECT token_id FROM api_webhooks")[0]["token_id"],
                    row["id"],
                )
                self.assertEqual(len(store.rows("SELECT * FROM api_webhooks")), 1)

    def test_explicit_historical_derivative_keeps_its_concrete_input(self):
        self.pair()
        self.both("admit", body=self.body, idempotency_key="generate")

        def local(surface, action, ok=True, **options):
            result = subprocess.run(
                [
                    str(BINARY),
                    "--db",
                    str(self.database),
                    "--json",
                    "operation",
                    surface,
                    action,
                    "--options",
                    json.dumps(options),
                ],
                capture_output=True,
                text=True,
                timeout=180,
            )
            self.assertEqual(
                result.returncode, 0 if ok else 2, result.stdout + result.stderr
            )
            return json.loads(result.stdout) if ok else result.stderr

        local("rendition-worker", "tick")
        with Store(self.database) as store:
            output = store.rows("SELECT file_id FROM media_outputs")[0]["file_id"]
            from pathlib import Path

            record = store.rows("SELECT * FROM files WHERE id=?", (output,))[0]
            generated = (
                Path(Application(store).binding("source", record["location"])["root"])
                / record["path"]
            )
        original_digest = hashlib.sha256(generated.read_bytes()).hexdigest()
        recipe = local(
            "artifact",
            "recipe",
            name="historical",
            preset="thumbnail",
            options={"max_output_bytes": 16777216},
        )
        (self.root / "source").rename(self.root / "source-offline")
        options = dict(
            file_id=output,
            recipe_id=recipe["id"],
            location="generated",
            item_id=self.item,
        )
        local("artifact", "enqueue", ok=False, **options)
        admitted = local(
            "artifact",
            "enqueue",
            source_lineage_mode="accepted_source_revision",
            **options,
        )
        completed = local("artifact", "run", job_ids=[admitted["job_id"]])
        self.assertTrue(completed["complete"], completed)
        with Store(self.database) as store:
            job = store.rows(
                "SELECT * FROM processing_jobs WHERE id=?", (admitted["job_id"],)
            )[0]
            self.assertEqual(job["file_id"], output)
            self.assertEqual(
                json.loads(job["options"])["source_lineage_mode"],
                "accepted_source_revision",
            )
            self.assertEqual(job["state"], "complete")
            self.assertEqual(
                store.rows(
                    "SELECT source_file_id FROM media_outputs WHERE source_file_id=?",
                    (output,),
                )[0]["source_file_id"],
                output,
            )
        self.assertEqual(
            original_digest, hashlib.sha256(generated.read_bytes()).hexdigest()
        )
