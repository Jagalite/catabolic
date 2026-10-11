# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Complete native rule planning, evidence attachment, and scoped execution."""

import hashlib
import json
import os
import secrets
import select
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from catabolic.app import Application
from catabolic.artifacts import Artifacts
from catabolic.domain import CatabolicError
from catabolic.item_workflow import ItemWorkflow
from catabolic.operations import Operations
from catabolic.outputs import Outputs
from catabolic.processing import Processing
from catabolic.processors import Processors
from catabolic.rules import Rules, maintain
from catabolic.store import Store
from tests.test_rust_migration import BINARY


@unittest.skipUnless(BINARY.is_file(), "build catabolic-cli before qualification")
class NativeRuleTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.source = self.root / "source"
        self.source.mkdir()
        for name in ("one.txt", "two.txt"):
            (self.source / name).write_text(name + " contents\n")
        self.path = self.root / "native.sqlite3"
        self.oracle = self.root / "oracle.sqlite3"
        self.generated = self.root / "generated"
        self.generated.mkdir()
        Store.initialize(self.path)
        with Store(self.path, writable=True) as store:
            app = Application(store)
            app.bind("source", "media", str(self.source))
            app.scan()
            self.files = {r["path"]: r["id"] for r in store.rows("SELECT * FROM files")}
            for name, file in self.files.items():
                app.put_item("book", {}, {"title": name}, name)
                app.media.associate(file, name)
            self.operation = Operations(app).put(
                "checksum", {"kind": "analysis", "operation": "hash"}
            )
            self.selection = {
                "language": "sql",
                "query": "SELECT item_id FROM catalog_items ORDER BY item_id",
            }
            self.rule = Rules(app).put(
                "checksum", self.operation["id"], None, self.selection, required=True
            )
        self.worker = subprocess.Popen(
            [str(BINARY), "--stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(self.close_worker)

    def close_worker(self):
        self.worker.stdin.close()
        try:
            self.worker.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.worker.kill()
            self.worker.wait()
        self.worker.stdout.close()
        self.worker.stderr.close()

    def native(self, action, args=None, *, ok=True):
        args = {"id": self.rule["id"], **(args or {})}
        self.worker.stdin.write(
            json.dumps(["--db", str(self.path), "rules", action, json.dumps(args)])
            + "\n"
        )
        self.worker.stdin.flush()
        self.assertTrue(
            select.select([self.worker.stdout], [], [], 120)[0], "native rule timeout"
        )
        response = json.loads(self.worker.stdout.readline())
        self.assertEqual(response["status"], 0 if ok else 2, response)
        return (
            response["result"] if ok else response["error"].removeprefix("catabolic: ")
        )

    def normalized(self, value):
        if isinstance(value, list):
            return [self.normalized(v) for v in value]
        if isinstance(value, dict):
            return {
                k: self.normalized(v)
                for k, v in value.items()
                if k
                not in (
                    "created_at",
                    "updated_at",
                    "evaluated_at",
                    "started_at",
                    "finished_at",
                    "available_bytes",
                    "elapsed_seconds",
                    "next_after",
                )
            }
        return value

    def state(self, path):
        with Store(path) as store:
            result = {}
            for table in (
                "item_workflow",
                "item_worklog",
                "rule_requirements",
                "rule_evaluations",
                "rule_jobs",
                "rule_processor_jobs",
                "processing_jobs",
                "processor_jobs",
            ):
                result[table] = store.rows(f"SELECT * FROM {table} ORDER BY rowid")
                for row in result[table]:
                    for field in (
                        "data",
                        "counts",
                        "snapshot",
                        "options",
                        "request",
                        "result",
                    ):
                        if field in row and row[field] is not None:
                            row[field] = json.loads(row[field])
            result["statistics"] = [
                json.loads(r["value"])
                for r in store.rows(
                    "SELECT value FROM meta WHERE key LIKE 'statistics:%' ORDER BY key"
                )
            ]
            return self.normalized(result)

    def differential(self, action, args=None, *, ok=True):
        args = args or {}
        shutil.copyfile(self.path, self.oracle)
        with Store(self.path) as before:
            old_ids = {
                table: {r["id"] for r in before.rows(f"SELECT id FROM {table}")}
                for table in (
                    "rule_evaluations",
                    "rule_requirements",
                    "processing_jobs",
                    "processor_jobs",
                )
            }
        actual = self.native(action, args, ok=ok)
        with Store(self.path) as after:
            new = {
                table: [
                    r["id"]
                    for r in after.rows(f"SELECT id FROM {table} ORDER BY rowid")
                    if r["id"] not in old_ids[table]
                ]
                for table in old_ids
            }
        with Store(self.oracle, writable=True) as store:
            rules = Rules(Application(store))
            method = (
                (lambda _id, **kwargs: maintain(Application(store), **kwargs))
                if action == "maintain"
                else getattr(rules, action)
            )
            if ok:
                rule_ids = new["rule_evaluations"] + new["rule_requirements"]
                external_ids = iter(new["processor_jobs"])
                token_hex = secrets.token_hex
                with (
                    patch("catabolic.rules.uuid4", side_effect=rule_ids),
                    patch(
                        "catabolic.processing.uuid4", side_effect=new["processing_jobs"]
                    ),
                    patch(
                        "catabolic.artifacts.uuid4", side_effect=new["processing_jobs"]
                    ),
                    patch(
                        "catabolic.processors.secrets.token_hex",
                        side_effect=lambda n: (
                            next(external_ids) if n == 16 else token_hex(n)
                        ),
                    ),
                ):
                    expected = method(self.rule["id"], **args)
                self.assertEqual(self.normalized(actual), self.normalized(expected))
            else:
                with self.assertRaises(CatabolicError) as caught:
                    method(self.rule["id"], **args)
                self.assertEqual(actual, str(caught.exception))
        self.assertEqual(self.state(self.path), self.state(self.oracle))
        return actual

    def test_preview_readonly_and_complete_selection_ignores_details_limit(self):
        before = self.path.read_bytes()
        result = self.differential("preview", {"limit": 1})
        self.assertEqual(self.path.read_bytes(), before)
        self.assertEqual(result["matched_inputs"], 2)
        self.assertTrue(result["details_truncated"])
        self.assertEqual(result["counts"]["missing"], 2)

    def test_required_full_selection_queue_batch_and_idempotent_adoption(self):
        result = self.differential("apply", {"batch": 1, "limit": 1})
        self.assertEqual(result["remaining_to_enqueue"], 1)
        with Store(self.path) as store:
            self.assertEqual(len(store.rows("SELECT * FROM rule_requirements")), 2)
            self.assertEqual(len(store.rows("SELECT * FROM item_worklog")), 3)
        self.differential("apply", {"batch": 1})
        result = self.differential("apply")
        self.assertEqual(result["queued"], [])
        self.assertEqual(result["adopted"], 2)
        self.assertTrue(result["complete"])

    def test_analysis_scoped_run_and_recorded_completion(self):
        self.differential("apply")
        result = self.differential("run", {"batch": 1})
        self.assertFalse(result["complete"])
        result = self.differential("run", {"batch": 1})
        self.assertTrue(result["complete"])
        self.differential("statistics")
        with Store(self.path) as store:
            for name in self.files:
                self.assertTrue(
                    all(
                        r["passed"]
                        for r in ItemWorkflow(Application(store)).checks(name)["checks"]
                    )
                )

    def test_cancel_requires_explicit_retry_and_preserves_history(self):
        self.differential("apply")
        with Store(self.path, writable=True) as store:
            processing = Processing(Application(store))
            for row in store.rows("SELECT id FROM processing_jobs"):
                processing.cancel(row["id"])
        result = self.differential("apply")
        self.assertEqual(result["counts"]["failed"], 2)
        self.assertFalse(result["complete"])
        result = self.differential("apply", {"retry_failed": True})
        self.assertEqual(len(result["queued"]), 2)
        self.differential("run", {"batch": 2})

    def test_maintenance_scan_scope_defers_all_unseen_without_queueing(self):
        result = self.differential("apply", {"scan_ids": []})
        self.assertEqual(result["counts"]["deferred"], 2)
        self.assertEqual(result["queued"], [])
        with Store(self.path) as store:
            self.assertEqual(len(store.rows("SELECT * FROM rule_requirements")), 2)

    def test_successful_text_scan_scope_admits_current_inputs(self):
        with Store(self.path) as store:
            scans = [
                r["id"] for r in store.rows("SELECT id FROM scans WHERE complete=1")
            ]
        self.assertTrue(scans and all(isinstance(v, str) for v in scans))
        result = self.differential("apply", {"scan_ids": scans})
        self.assertEqual(len(result["queued"]), 2)

    def test_full_large_selection_requirements_are_independent_of_display_and_queue_limits(
        self,
    ):
        with Store(self.path, writable=True) as store:
            with store.transaction() as db:
                db.executemany(
                    "INSERT INTO items VALUES (?,'book','{\"title\":\"Scale\"}')",
                    [(f"scale-{n:05d}",) for n in range(10001)],
                )
                db.executemany(
                    "INSERT INTO item_files(id,item_id,file_id,role,active,metadata,origin) VALUES (?,?,?,'primary',1,'{}','explicit')",
                    [
                        (
                            f"association-{n:05d}",
                            f"scale-{n:05d}",
                            self.files["one.txt"],
                        )
                        for n in range(10001)
                    ],
                )
            selection = {
                "language": "sql",
                "query": "SELECT item_id FROM catalog_items WHERE item_id LIKE 'scale-%'",
                "page_size": 1000,
                "max_ids": 20000,
                "timeout_ms": 60000,
            }
            self.rule = Rules(Application(store)).put(
                "scale", self.operation["id"], None, selection, required=True
            )
        result = self.native("apply", {"batch": 1, "limit": 1, "scan_ids": []})
        self.assertEqual(result["matched_inputs"], 10001)
        self.assertEqual(result["counts"]["deferred"], 10001)
        self.assertEqual(len(result["matches"]), 1)
        with Store(self.path) as store:
            self.assertEqual(
                store.rows("SELECT count(*) AS n FROM rule_requirements")[0]["n"], 10001
            )
            self.assertEqual(
                store.rows("SELECT count(*) AS n FROM item_worklog")[0]["n"], 10001
            )
        with Store(self.path, writable=True) as store:
            self.rule = Rules(Application(store)).put(
                "truncated-scale",
                self.operation["id"],
                None,
                {**selection, "max_ids": 10000},
                required=True,
            )
        before = self.state(self.path)
        self.assertIn("max_ids", self.native("apply", ok=False))
        self.assertEqual(self.state(self.path), before)

    def test_run_replans_and_never_executes_unregistered_or_unselected_jobs(self):
        self.differential("apply")
        with Store(self.path, writable=True) as store:
            Processing(Application(store)).enqueue(
                "sniff", file_ids=[self.files["one.txt"]]
            )
            with store.transaction() as db:
                db.execute("UPDATE item_files SET active=0 WHERE item_id='two.txt'")
        result = self.differential("run", {"batch": 10})
        self.assertEqual(result["execution"]["processed"], 1)
        with Store(self.path) as store:
            pending = store.rows(
                "SELECT operation,file_id FROM processing_jobs WHERE state='queued'"
            )
            self.assertEqual(len(pending), 2)

    def test_budget_validation_and_selection_overflow_leave_no_mutations(self):
        self.differential("apply", {"max_new_bytes": True}, ok=False)
        with Store(self.path, writable=True) as store:
            self.rule = Rules(Application(store)).put(
                "limited",
                self.operation["id"],
                None,
                {**self.selection, "max_ids": 1},
                required=True,
            )
        before = self.path.read_bytes()
        self.native("apply", ok=False)
        self.assertEqual(self.path.read_bytes(), before)
        with Store(self.path) as store:
            self.assertEqual(store.rows("SELECT * FROM rule_requirements"), [])

    def test_external_jobs_share_fenced_owner_and_unknown_budget_refusal(self):
        with Store(self.path, writable=True) as store:
            app = Application(store)
            Processors(app).put("fixture", {"endpoint": "http://127.0.0.1:9"})
            output = Outputs(app).define("external", {"purpose": "custom"})
            operation = Operations(app).put(
                "external",
                {
                    "kind": "external",
                    "operation": "fixture",
                    "output_definition_id": output["id"],
                    "options": {"preset": "fixture"},
                },
            )
            self.rule = Rules(app).put(
                "external", operation["id"], None, self.selection, required=True
            )
        self.differential("preview")
        blocked = self.differential("apply", {"max_new_bytes": 0})
        self.assertFalse(blocked["safe"])
        self.differential("apply", {"batch": 1})
        self.differential("apply")
        result = self.differential("run")
        self.assertEqual(result["execution"]["pending_external"], 2)
        self.assertFalse(result["complete"])
        self.differential("statistics")

    def test_external_receipt_satisfaction_and_stale_output_evidence(self):
        with Store(self.path, writable=True) as store:
            app = Application(store)
            (self.source / "external.mkv").write_bytes(b"external.mkv")
            app.scan()
            Processors(app).put("fixture", {"endpoint": "http://127.0.0.1:9"})
            self.definition = Outputs(app).define("external", {"purpose": "custom"})
            operation = Operations(app).put(
                "external",
                {
                    "kind": "external",
                    "operation": "fixture",
                    "output_definition_id": self.definition["id"],
                    "options": {"preset": "fixture"},
                },
            )
            self.rule = Rules(app).put(
                "receipt",
                operation["id"],
                None,
                {
                    "language": "sql",
                    "query": "SELECT item_id FROM catalog_items WHERE item_id='one.txt'",
                },
                required=True,
            )
        self.differential("apply")
        with Store(self.path, writable=True) as store:
            from tests.test_processor_workers import ProcessorTest

            processors = Processors(Application(store))
            lease = processors.claim("fixture", "worker")
            processors.complete(
                lease["id"], lease["lease_token"], ProcessorTest.receipt(self, lease)
            )
        self.assertEqual(self.differential("preview")["counts"]["satisfied"], 1)
        self.differential("apply")
        with Store(self.path) as store:
            checks = ItemWorkflow(Application(store)).checks("one.txt")["checks"]
            self.assertTrue(all(row["passed"] for row in checks))
        (self.source / "external.mkv").write_bytes(b"Changed external output")
        with Store(self.path, writable=True) as store:
            app = Application(store)
            app.scan()
            expected = Rules(app).preview(self.rule["id"])
            checks = ItemWorkflow(app).checks("one.txt")["checks"]
            self.assertFalse(all(row["passed"] for row in checks))
        actual = self.native("preview")
        self.assertEqual(actual["counts"], expected["counts"])
        self.assertEqual(actual["counts"]["deferred"], 1)

    def test_unassociated_analysis_allowed_but_required_refused(self):
        with Store(self.path, writable=True) as store:
            with store.transaction() as db:
                db.execute("UPDATE item_files SET active=0")
            selection = {
                "language": "sql",
                "query": "SELECT file_id FROM catalog_files",
            }
            self.rule = Rules(Application(store)).put(
                "orphan", self.operation["id"], None, selection
            )
        self.differential("preview")
        self.differential("apply")
        with Store(self.path, writable=True) as store:
            self.rule = Rules(Application(store)).put(
                "required-orphan", self.operation["id"], None, selection, required=True
            )
        self.differential("preview", ok=False)

    def test_render_missing_probe_requirement_and_budget_are_not_zero(self):
        with Store(self.path, writable=True) as store:
            app = Application(store)
            artifacts = Artifacts(app)
            artifacts.bind("generated", self.generated)
            operation = artifacts.recipe("thumbnail", "thumbnail")
            self.rule = Rules(app).put(
                "thumbnail", operation["id"], "generated", self.selection, required=True
            )
        before = self.path.read_bytes()
        result = self.differential("preview", {"limit": 1})
        self.assertEqual(self.path.read_bytes(), before)
        self.assertEqual(result["counts"]["deferred"], 2)
        self.assertGreater(result["space"]["planning_bytes"], 0)
        self.assertEqual(result["space"]["unknown_count"], 2)
        self.differential("apply", {"batch": 1, "limit": 1})
        with Store(self.path, writable=True) as store:
            rules = Rules(Application(store))
            twin = rules.put("z-twin", operation["id"], "generated", self.selection)
            rules.enable(self.rule["id"], True)
            rules.enable(twin["id"], True)
        maintenance = self.differential("maintain", {"batch": 1})
        self.assertEqual(len(maintenance["space_by_device"]), 1)
        self.assertEqual(maintenance["space_by_device"][0]["output_count"], 2)
        self.assertEqual(maintenance["backlog"]["deferred"], 4)

    def test_analysis_maintenance_repairs_frozen_count_assumption(self):
        with Store(self.path, writable=True) as store:
            Rules(Application(store)).enable(self.rule["id"], True)
        self.native("maintain", {"batch": 2, "render_batch": 1})
        # Independent ledger: exactly one of the two registered jobs may execute.
        with Store(self.path) as store:
            jobs = store.rows(
                "SELECT state,count(*) AS n FROM processing_jobs GROUP BY state"
            )
            self.assertEqual(
                {r["state"]: r["n"] for r in jobs}, {"complete": 1, "queued": 1}
            )
            self.assertEqual(len(store.rows("SELECT * FROM processing_attempts")), 1)
        # The frozen implementation executes the job, then indexes missing keys.
        shutil.copyfile(self.path, self.oracle)
        with Store(self.oracle, writable=True) as store:
            with self.assertRaisesRegex(KeyError, "completed"):
                maintain(Application(store), batch=1, render_batch=1)
        result = self.native("maintain", {"batch": 1, "render_batch": 1})
        self.assertTrue(result["complete"])

    @unittest.skipUnless(
        shutil.which("ffmpeg") and shutil.which("ffprobe"), "real media tools required"
    )
    def test_real_render_entire_lifecycle_preserves_source(self):
        from tests.test_artifacts import ArtifactTest

        ArtifactTest.setUpClass.__func__(ArtifactTest)
        self.addCleanup(ArtifactTest.fixture.cleanup)
        media = self.source / "clip.mkv"
        media.write_bytes(ArtifactTest.payload)
        original = hashlib.sha256(media.read_bytes()).hexdigest()
        with Store(self.path, writable=True) as store:
            app = Application(store)
            app.scan()
            file = store.rows("SELECT id FROM files WHERE path='clip.mkv'")[0]["id"]
            app.put_item("movie", {}, {"title": "Clip"}, "clip")
            app.media.associate(file, "clip")
            processing = Processing(app)
            processing.enqueue("probe", file_ids=[file])
            self.assertTrue(processing.run()["complete"])
            artifacts = Artifacts(app)
            artifacts.bind("generated", self.generated)
            recipe = artifacts.recipe("thumbnail", "thumbnail")
            self.rule = Rules(app).put(
                "thumbnail",
                recipe["id"],
                "generated",
                {
                    "language": "sql",
                    "query": "SELECT item_id FROM catalog_items WHERE item_id='clip'",
                },
                required=True,
            )
        self.differential("preview")
        blocked = self.differential("apply", {"max_new_bytes": 0})
        self.assertFalse(blocked["safe"])
        queued = self.differential("apply")
        self.assertEqual(len(queued["queued"]), 1)
        result = self.native("run")
        self.assertTrue(result["complete"], result)
        self.differential("preview")
        with Store(self.path) as store:
            artifacts = store.rows("SELECT * FROM processing_artifacts")
            self.assertEqual(len(artifacts), 1)
            self.assertEqual(artifacts[0]["state"], "ready")
            output = self.generated / artifacts[0]["path"]
            self.assertEqual(
                hashlib.sha256(output.read_bytes()).hexdigest(), artifacts[0]["sha256"]
            )
            self.assertTrue(
                all(
                    r["passed"]
                    for r in ItemWorkflow(Application(store)).checks("clip")["checks"]
                )
            )
        self.assertEqual(hashlib.sha256(media.read_bytes()).hexdigest(), original)
        self.assertEqual(os.stat(media).st_size, len(ArtifactTest.payload))
        # Derived outputs are excluded by default, including explicit file-ID selections.
        with Store(self.path, writable=True) as store:
            app = Application(store)
            selection = {
                "language": "sql",
                "query": "SELECT file_id FROM catalog_files WHERE location='generated'",
            }
            self.rule = Rules(app).put(
                "no-recursion", recipe["id"], "generated", selection
            )
        self.assertEqual(self.differential("preview")["matched_inputs"], 0)
        with Store(self.path, writable=True) as store:
            self.rule = Rules(Application(store)).put(
                "explicit-derived",
                recipe["id"],
                "generated",
                selection,
                allow_derived=True,
            )
        self.assertEqual(self.differential("preview")["matched_inputs"], 1)

    def test_shared_analysis_job_tracks_each_item_without_duplicate_execution(self):
        with Store(self.path, writable=True) as store:
            app = Application(store)
            app.put_item("book", {}, {"title": "Second identity"}, "extra")
            app.media.associate(self.files["one.txt"], "extra")
        applied = self.differential("apply", {"batch": 10})
        self.assertEqual(applied["matched_inputs"], 3)
        self.assertEqual(len(applied["queued"]), 3)
        self.assertEqual(len({row["job_id"] for row in applied["queued"]}), 2)
        executed = self.differential("run", {"batch": 10})
        self.assertEqual(executed["execution"]["processed"], 2)
        self.assertTrue(executed["complete"])
        with Store(self.path) as store:
            self.assertEqual(len(store.rows("SELECT * FROM rule_requirements")), 3)
            self.assertEqual(len(store.rows("SELECT * FROM processing_attempts")), 2)
            for item in ("one.txt", "two.txt", "extra"):
                self.assertTrue(
                    all(
                        row["passed"]
                        for row in ItemWorkflow(Application(store)).checks(item)[
                            "checks"
                        ]
                    )
                )
