# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Entry completion must agree across interfaces and never discard the worklog."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from catabolic.app import Application
from catabolic.curation import Curation
from catabolic.domain import CatabolicError
from catabolic.graphql_query import execute_graphql
from catabolic.item_workflow import ItemWorkflow
from catabolic.manifest import Manifest
from catabolic.migration import load_migrations, upgrade_database, validate_preservation
from catabolic.processing import Processing
from catabolic.sql_query import execute_sql
from catabolic.store import Store
from tests.test_migrations import contents, create_legacy


class ItemWorkflowTest(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.source = self.root / "media"
        self.source.mkdir()
        (self.source / "book.txt").write_text("A small book.\n")
        (self.source / "cover.txt").write_text("Optional file.\n")
        self.path = self.root / "catalog.sqlite3"
        Store.initialize(self.path)
        self.store = Store(self.path, writable=True)
        self.addCleanup(self.store.close)
        self.app = Application(self.store)
        self.app.bind("source", "media", str(self.source))
        self.app.scan()
        self.files = {
            r["path"]: r["id"] for r in self.store.rows("SELECT * FROM files")
        }
        self.app.put_item("book", {}, {"title": "Book"}, "book")
        self.app.media.associate(self.files["book.txt"], "book")
        self.app.media.associate(self.files["cover.txt"], "book", role="cover")
        self.w = ItemWorkflow(self.app)

    def query(self, query, **kwargs):
        return execute_graphql(self.path, query, _store=self.store, **kwargs)

    def test_defaults_and_missing_title_block_completion_without_mutation(self):
        self.assertEqual(self.w.get("book")["status"], "pending")
        self.assertEqual(self.w.get("book")["revision"], 0)
        self.assertEqual(self.w.log("book")["entries"], [])
        self.app.put_item("other", {}, {}, "untitled")
        with self.assertRaisesRegex(CatabolicError, "title"):
            self.w.set_status("untitled", "complete")
        self.assertEqual(self.w.log("untitled")["entries"], [])
        self.assertEqual(self.store.rows("SELECT * FROM item_workflow"), [])

    def test_multiline_worklog_preserves_notes_and_status_history(self):
        self.w.note("book", "First line\nSecond line — 中文", actor="agent:cataloger")
        self.w.set_status(
            "book", "deferred", note="Need edition evidence", actor="reviewer"
        )
        self.w.note("book", "Edition confirmed", kind="decision")
        self.w.set_status("book", "complete", note="Cataloging finished")
        rows = self.w.log("book")["entries"]
        self.assertEqual([r["revision"] for r in rows], [1, 2, 3, 4])
        self.assertEqual(rows[0]["body"], "First line\nSecond line — 中文")
        self.assertEqual(rows[1]["data"], {"before": "pending", "after": "deferred"})
        self.assertTrue(all(r["actor"] and r["created_at"] for r in rows))
        with self.assertRaisesRegex(CatabolicError, "entry changed"):
            self.w.note("book", "stale update", expected_revision=1)
        self.assertEqual(len(self.w.log("book")["entries"]), 4)

    def test_active_primary_file_loss_changes_effective_status_without_rewriting_history(
        self,
    ):
        self.w.set_status("book", "complete")
        history = self.w.log("book")
        (self.source / "book.txt").unlink()
        self.app.scan()
        state = self.w.get("book")
        self.assertEqual(
            (state["status"], state["requested_status"]),
            ("needs_attention", "complete"),
        )
        self.assertEqual(self.w.log("book"), history)
        with self.assertRaisesRegex(CatabolicError, "missing"):
            self.w.set_status("book", "complete")
        (self.source / "book.txt").write_text("A small book.\n")
        self.app.scan()
        self.assertEqual(self.w.get("book")["status"], "complete")
        self.assertEqual(self.w.log("book"), history)

    def test_optional_files_and_jobs_do_not_block_and_explicit_requirements_do(self):
        (self.source / "cover.txt").unlink()
        self.app.scan()
        job = Processing(self.app).enqueue("hash", file_ids=[self.files["book.txt"]])[
            "queued"
        ][0]
        self.w.set_status("book", "complete")
        required = self.w.require("book", "job", "Checksum", target=job)
        self.assertEqual(self.w.get("book")["status"], "needs_attention")
        with self.assertRaisesRegex(CatabolicError, "cannot be manually completed"):
            self.w.resolve(
                required["requirement_id"], "complete", note="Pretend it passed"
            )
        with self.assertRaisesRegex(CatabolicError, "queued"):
            self.w.set_status("book", "complete")
        self.assertTrue(Processing(self.app).run()["complete"])
        self.assertEqual(self.w.get("book")["status"], "complete")
        (self.source / "book.txt").write_text("Changed content and size.\n")
        self.app.scan()
        self.assertEqual(self.w.get("book")["status"], "needs_attention")
        self.assertFalse(self.w.checks("book")["checks"][0]["passed"])

    def test_required_job_retry_uses_latest_attempt_of_same_job(self):
        processing = Processing(self.app)
        job = processing.enqueue("hash", file_ids=[self.files["book.txt"]])["queued"][0]
        self.w.require("book", "job", "Hash", target=job)
        processing.cancel(job)
        with self.assertRaisesRegex(CatabolicError, "cancelled"):
            self.w.set_status("book", "complete")
        processing.retry(job)
        self.assertTrue(processing.run()["complete"])
        self.w.set_status("book", "complete")
        self.assertEqual(self.w.get("book")["status"], "complete")

    def test_manual_review_waiver_and_reopening_preserve_reasons(self):
        task = self.w.require("book", "review", "Confirm edition")["requirement_id"]
        with self.assertRaisesRegex(CatabolicError, "Confirm edition"):
            self.w.set_status("book", "complete")
        with self.assertRaises(CatabolicError):
            self.w.resolve(task, "waived", note="")
        self.w.resolve(task, "waived", note="No edition claim is being made")
        self.w.set_status("book", "complete")
        self.w.resolve(task, "pending", note="New edition evidence found")
        self.assertEqual(self.w.get("book")["status"], "needs_attention")
        self.w.resolve(task, "complete", note="Checked publisher page")
        self.assertEqual(self.w.get("book")["status"], "complete")
        self.assertEqual(len(self.w.log("book")["entries"]), 5)

    def test_requirements_cannot_reference_unrelated_targets(self):
        self.app.put_item("book", {}, {"title": "Other"}, "other")
        with self.assertRaisesRegex(CatabolicError, "does not belong"):
            self.w.require("other", "file", "Wrong file", target=self.files["book.txt"])
        with self.assertRaisesRegex(CatabolicError, "unknown requirement target"):
            self.w.require("book", "job", "Missing", target="missing")
        self.assertEqual(self.store.rows("SELECT * FROM item_requirements"), [])

    def test_required_proposal_must_be_accepted_or_explicitly_waived(self):
        c = Curation(self.app)
        proposal = c.put(
            self.files["book.txt"],
            {"item_id": "book", "role": "primary"},
            source="fixture",
        )
        req = self.w.require(
            "book", "proposal", "Accept identification", target=proposal["id"]
        )["requirement_id"]
        with self.assertRaisesRegex(CatabolicError, "pending"):
            self.w.set_status("book", "complete")
        c.decide(proposal["id"], accept=True, actor="reviewer")
        self.w.set_status("book", "complete")
        self.assertEqual(self.w.checks("book")["checks"][0]["check_id"], req)

    def test_atomic_worklog_and_requirement_changes_roll_back_together(self):
        original = ItemWorkflow._append

        def fail_after_write(*args, **kwargs):
            original(*args, **kwargs)
            raise RuntimeError("before commit")

        with (
            patch.object(ItemWorkflow, "_append", new=fail_after_write),
            self.assertRaises(RuntimeError),
        ):
            self.w.require("book", "review", "Must be atomic")
        self.assertEqual(self.store.rows("SELECT * FROM item_requirements"), [])
        self.assertEqual(self.w.log("book")["entries"], [])
        self.assertEqual(self.w.get("book")["revision"], 0)

    def test_sql_graphql_filters_and_worklog_pagination_agree(self):
        self.w.note("book", "First")
        self.w.note("book", "Second")
        self.w.set_status("book", "complete")
        req = self.w.require("book", "review", "Review again")
        self.assertEqual(
            self.app.queries.items(curation_status="needs_attention")["items"][0]["id"],
            "book",
        )
        sql = execute_sql(
            self.path,
            "SELECT status FROM catalog_item_workflow WHERE item_id='book' AND profile=:profile",
            _store=self.store,
        )
        self.assertEqual(sql["rows"], [["needs_attention"]])
        result = self.query(
            "{ items(curationStatus:NEEDS_ATTENTION) { nodes { id workflow { status requestedStatus revision } workflowChecks { nodes } worklog(first:1) { nodes pageInfo { hasNextPage endCursor } } } } }"
        )
        self.assertNotIn("errors", result, result)
        row = result["data"]["items"]["nodes"][0]
        self.assertEqual(row["workflow"]["status"], "NEEDS_ATTENTION")
        self.assertEqual(
            row["workflowChecks"]["nodes"][0]["check_id"], req["requirement_id"]
        )
        page = row["worklog"]
        next_page = self.query(
            'query($after:String){worklog(item:"book",first:1,after:$after){nodes pageInfo{endCursor}}}',
            variables={"after": page["pageInfo"]["endCursor"]},
        )
        self.assertNotIn("errors", next_page, next_page)
        self.assertEqual(next_page["data"]["worklog"]["nodes"][0]["body"], "Second")
        first = self.w.log("book", limit=1)
        self.assertEqual(
            self.w.log("book", limit=1, after=first["next_after"])["entries"][0][
                "body"
            ],
            "Second",
        )

    def test_profile_readiness_and_global_worklog(self):
        self.app.add_profile("offline")
        self.w.set_status("book", "complete")
        other = ItemWorkflow(Application(self.store, "offline"))
        self.assertEqual(other.get("book")["requested_status"], "complete")
        self.assertEqual(other.get("book")["status"], "needs_attention")
        self.assertEqual(other.log("book"), self.w.log("book"))
        self.assertEqual(self.w.get("book")["status"], "complete")

    def test_export_contains_summary_and_metadata_updates_preserve_worklog(self):
        self.app.put_mapping("global", self.files["book.txt"], "book", "Books/Book.txt")
        self.w.note("book", "Keep this history")
        self.w.set_status("book", "deferred")
        self.app.put_item("book", {}, {"title": "Updated"}, "book")
        with self.store.transaction():
            document = Manifest(self.app).build()
        row = next(r for r in document["content"]["items"] if r["id"] == "book")
        self.assertEqual(row["workflow"]["status"], "deferred")
        self.assertEqual(len(self.w.log("book")["entries"]), 2)

    def test_cli_stdin_and_readonly_status(self):
        self.store.close()
        command = [
            sys.executable,
            "-m",
            "catabolic",
            "--db",
            str(self.path),
            "--json",
            "item",
        ]
        result = subprocess.run(
            command + ["note", "book", "--file", "-", "--actor", "agent:test"],
            input="Line one\nLine two\n",
            text=True,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        before = self.path.read_bytes()
        result = subprocess.run(
            command + ["status", "book"], text=True, capture_output=True
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["revision"], 1)
        self.assertEqual(self.path.read_bytes(), before)

    def test_invalid_inputs_leave_history_unchanged(self):
        for note in ("", "\0", "x" * 65537):
            with self.assertRaises(CatabolicError):
                self.w.note("book", note)
        with self.assertRaises(CatabolicError):
            self.w.note("book", "note", actor="")
        with self.assertRaises(CatabolicError):
            self.w.set_status("book", "needs_attention")
        self.assertEqual(self.w.log("book")["entries"], [])


class WorkflowMigrationTest(unittest.TestCase):
    def test_schema_nine_upgrade_preserves_all_rows_and_does_not_invent_history(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = create_legacy(Path(tmp))
            upgrade_database(path, migrations=load_migrations()[:9])
            before = contents(path)
            result = upgrade_database(path)
            self.assertEqual(contents(Path(result["backup"])), before)
            with Store(path) as store:
                validate_preservation(store.db, before)
                self.assertEqual(store.rows("SELECT * FROM item_worklog"), [])
                self.assertEqual(store.rows("SELECT * FROM item_workflow"), [])
                self.assertEqual(store.rows("PRAGMA foreign_key_check"), [])
