# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Disposable owner-service workflow and mixed inbox query acceptance."""

import json
import shutil
import subprocess
import sys
import unittest
from unittest.mock import patch

from catabolic.curation import Curation
from catabolic.domain import CatabolicError
from catabolic.evaluation import EvaluationSession
from catabolic.graphql_query import execute_graphql
from catabolic.processing import Processing
from catabolic.saved_queries import Queries
from catabolic.sql_query import execute_sql
from catabolic.watchers import Watchers
from catabolic.work_inbox import action, decode, evidence_token, validate_evidence
from tests import test_artifacts as artifact_fixtures
from tests import test_fallback_lifecycle as projection_fixtures
from tests import test_item_workflow as fixtures
from tests import test_programmable_catalog as programmable_fixtures


class InboxTest(unittest.TestCase):
    setUp = fixtures.ItemWorkflowTest.setUp

    def rows(self, *, profile="default", inactive=False):
        result = execute_sql(
            self.path,
            "SELECT * FROM catalog_work_inbox WHERE profile=:profile "
            + (
                ""
                if inactive
                else "AND actionability IN ('ready','blocked','needs_decision') "
            )
            + "ORDER BY work_key",
            profile=profile,
            _store=self.store,
            max_rows=10000,
        )
        self.assertTrue(result["complete"])
        return [
            decode(dict(zip(result["columns"], row, strict=True)))
            for row in result["rows"]
        ]

    def test_empty_action_arguments_are_json_objects(self):
        result = self.store.rows("SELECT " + action("artifact recover") + " AS actions")
        self.assertEqual(json.loads(result[0]["actions"])[0]["arguments"], {})

    def test_end_to_end_identification_review_completion_and_attention(self):
        new = self.source / "new.txt"
        new.write_text("Disposable identification fixture")
        self.app.scan()
        initial = next(r for r in self.rows() if r["category"] == "identification")
        self.assertEqual(initial["actionability"], "ready")
        self.assertEqual(self.store.rows("SELECT count(*) AS n FROM items")[0]["n"], 1)
        self.app.scan()
        self.assertEqual(
            next(r for r in self.rows() if r["category"] == "identification")[
                "work_key"
            ],
            initial["work_key"],
        )
        curation = Curation(self.app)
        proposal = curation.put(
            initial["subject_id"],
            {
                "item": {
                    "kind": "book",
                    "identities": {"fixture": "new-book"},
                    "metadata": {"title": "New book"},
                }
            },
        )
        pending = self.rows()
        self.assertFalse(any(r["category"] == "identification" for r in pending))
        self.assertTrue(any(r["subject_id"] == proposal["id"] for r in pending))
        curation.decide(proposal["id"], accept=True, actor="test:operator")
        item = self.store.rows(
            "SELECT item_id FROM item_files WHERE file_id=?", (initial["subject_id"],)
        )[0]["item_id"]
        requirement = self.w.require(
            item, "review", "Confirm edition/component compatibility"
        )
        review = next(
            r
            for r in self.rows()
            if r["subject_id"] == item and r["category"] == "requirement"
        )
        self.assertEqual(review["actionability"], "needs_decision")
        self.assertEqual(
            sum(r["countable"] for r in self.rows() if r["subject_id"] == item), 1
        )
        self.w.resolve(
            requirement["requirement_id"],
            "complete",
            note="Edition and compatibility reviewed",
            expected_revision=review["preconditions"]["workflow_revision"],
        )
        file_requirement = self.w.require(
            item, "file", "Required original", target=initial["subject_id"]
        )
        self.w.set_status(item, "complete")
        self.assertFalse(any(r["subject_id"] == item for r in self.rows()))
        log = self.w.log(item)
        new.unlink()
        self.app.scan()
        attention = next(
            r
            for r in self.rows()
            if r["subject_id"] == item and r["category"] == "curation"
        )
        self.assertEqual(attention["reason"], "completion_evidence_unsatisfied")
        self.assertEqual(
            sum(r["countable"] for r in self.rows() if r["subject_id"] == item), 1
        )
        explicit_key = next(
            r["work_key"]
            for r in self.rows()
            if r["source_id"] == file_requirement["requirement_id"]
        )
        key = attention["work_key"]
        self.assertEqual(self.w.log(item), log)
        new.write_text("Evidence restored")
        self.app.scan()
        self.assertFalse(any(r["subject_id"] == item for r in self.rows()))
        new.unlink()
        self.app.scan()
        self.assertTrue(any(r["work_key"] == explicit_key for r in self.rows()))
        self.assertEqual(
            next(
                r
                for r in self.rows()
                if r["subject_id"] == item and r["category"] == "curation"
            )["work_key"],
            key,
        )

    def test_deferred_ignored_optional_and_profile_readiness(self):
        self.w.require("book", "review", "Review edition")
        self.w.set_status("book", "deferred", note="Later")
        self.assertFalse(self.rows())
        self.assertTrue(
            all(r["actionability"] == "deferred" for r in self.rows(inactive=True))
        )
        self.w.set_status("book", "ignored", note="Outside scope")
        self.assertFalse(self.rows())
        self.w.set_status("book", "pending")
        requirement = self.w.checks("book")["checks"][0]
        self.w.resolve(requirement["check_id"], "complete", note="Reviewed")
        (self.source / "cover.txt").unlink()
        self.app.scan()
        self.w.set_status("book", "complete")
        self.assertFalse(self.rows())
        with self.store.transaction() as db:
            db.execute("INSERT INTO profiles(id) VALUES ('remote')")
        self.assertTrue(
            any(
                r["reason"] == "completion_evidence_unsatisfied"
                for r in self.rows(profile="remote")
            )
        )
        self.assertFalse(self.rows())

    def test_required_processing_waiting_failure_retry_and_history(self):
        processing = Processing(self.app)
        job = {
            "id": processing.enqueue("sniff", file_ids=[self.files["book.txt"]])[
                "queued"
            ][0]
        }
        self.w.require("book", "job", "Required sniff", target=job["id"])
        queued = next(
            r for r in self.rows(inactive=True) if r["source_kind"] == "processing_jobs"
        )
        self.assertEqual(queued["actionability"], "waiting")
        with self.store.transaction() as db:
            db.execute(
                "UPDATE processing_jobs SET state='failed',error='test failure' WHERE id=?",
                (job["id"],),
            )
        entries = self.rows()
        self.assertEqual(sum(r["countable"] for r in entries), 1)
        processing.retry(job["id"])
        processing.run(limit=1)
        self.w.set_status("book", "complete")
        self.assertFalse(self.rows())
        self.assertTrue(
            any(
                r["source_id"] == job["id"] and r["actionability"] == "historical"
                for r in self.rows(inactive=True)
            )
        )

    def test_superseded_success_is_not_an_intervention(self):
        processing = Processing(self.app)
        job = {
            "id": processing.enqueue("sniff", file_ids=[self.files["book.txt"]])[
                "queued"
            ][0]
        }
        processing.run(limit=1)
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state) SELECT 'old-failure',profile,file_id,operation,location,snapshot,options,cache_key,'failed' FROM processing_jobs WHERE id=?",
                (job["id"],),
            )
        self.assertFalse(any(r["source_id"] == "old-failure" for r in self.rows()))
        self.assertTrue(
            any(r["source_id"] == "old-failure" for r in self.rows(inactive=True))
        )

    def test_historical_success_does_not_hide_current_failed_analysis(self):
        processing = Processing(self.app)
        job = processing.enqueue("sniff", file_ids=[self.files["book.txt"]])["queued"][
            0
        ]
        processing.run(limit=1)
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state) SELECT 'new-failure',profile,file_id,operation,location,snapshot,options,cache_key,'failed' FROM processing_jobs WHERE id=?",
                (job,),
            )
            db.execute(
                "UPDATE file_facts SET status='failed',job_id='new-failure' WHERE job_id=?",
                (job,),
            )
        self.assertTrue(
            any(
                r["source_id"] == "new-failure" and r["actionability"] == "blocked"
                for r in self.rows()
            )
        )

    def test_read_only_and_stale_preconditions(self):
        self.w.require("book", "review", "Edition review")
        row = next(r for r in self.rows() if r["category"] == "requirement")
        token = evidence_token(row)
        before = list(self.store.db.iterdump())
        self.rows()
        execute_graphql(
            self.path, "{workInbox{nodes pageInfo{hasNextPage}}}", _store=self.store
        )
        self.assertEqual(before, list(self.store.db.iterdump()))
        self.w.note("book", "Concurrent review")
        changed = next(r for r in self.rows() if r["category"] == "requirement")
        with self.assertRaisesRegex(CatabolicError, "evidence changed"):
            validate_evidence(changed, token)
        with self.assertRaisesRegex(CatabolicError, "entry changed"):
            self.w.resolve(
                row["source_id"],
                "complete",
                note="Outdated",
                expected_revision=row["preconditions"]["workflow_revision"],
            )

    def test_saved_queries_pagination_and_sql_truncation(self):
        for n in range(3):
            self.w.require("book", "review", f"Review {n}")
        all_keys = [r["work_key"] for r in self.rows()]
        cursor = None
        keys = []
        while True:
            result = execute_graphql(
                self.path,
                "query($after:String){workInbox(first:2,after:$after){nodes pageInfo{hasNextPage endCursor}}}",
                variables={"after": cursor},
                _store=self.store,
            )
            self.assertNotIn("errors", result)
            page = result["data"]["workInbox"]
            keys.extend(r["work_key"] for r in page["nodes"])
            cursor = page["pageInfo"]["endCursor"]
            if not cursor:
                break
        self.assertEqual(keys, all_keys)
        result = execute_sql(
            self.path, "SELECT * FROM catalog_work_inbox", max_rows=1, _store=self.store
        )
        self.assertFalse(result["complete"])
        self.assertTrue(result["truncated"])
        for name, definition in (
            (
                "sql-inbox",
                {
                    "mode": "rows",
                    "selection": {
                        "language": "sql",
                        "query": "SELECT work_key FROM catalog_work_inbox WHERE profile=:profile ORDER BY work_key",
                    },
                },
            ),
            (
                "graphql-inbox",
                {
                    "mode": "document",
                    "selection": {
                        "language": "graphql",
                        "query": "{workInbox{nodes pageInfo{hasNextPage}}}",
                    },
                },
            ),
        ):
            query = Queries(self.store).put(name, definition)
            self.assertTrue(query["id"])
            result = Queries(self.store).run(
                query["id"], session=EvaluationSession(self.store)
            )
            self.assertNotIn("errors", result)
            self.assertTrue(result.get("complete", True))
            if definition["mode"] == "document":
                self.assertEqual(
                    len(result["data"]["workInbox"]["nodes"]), len(all_keys)
                )

    def test_cli_show_summary_and_page(self):
        self.w.require("book", "review", "Edition review")
        self.w.require("book", "review", "Translation review")
        with self.store.detached():

            def command(*args):
                result = subprocess.run(
                    [
                        sys.executable,
                        "-m",
                        "catabolic",
                        "--db",
                        str(self.path),
                        "--json",
                        "inbox",
                        *args,
                    ],
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                return json.loads(result.stdout)

            summary = command("summary")
            self.assertEqual(summary["actionable_count"], 2)
            page = command("list", "--limit", "1")
            self.assertTrue(page["evaluation_complete"])
            self.assertFalse(page["result_complete"])
            detail = command("show", page["entries"][0]["work_key"])
            command(
                "show",
                detail["entry"]["work_key"],
                "--expected-evidence",
                detail["evidence_token"],
            )

    def test_watcher_report_event_failure_and_empty_to_nonempty(self):
        self.w.set_status("book", "complete")
        queries = Queries(self.store)
        query = queries.put(
            "inbox",
            {
                "mode": "rows",
                "selection": {
                    "language": "sql",
                    "query": "SELECT work_key,reason FROM catalog_work_inbox WHERE profile=:profile AND category='requirement' AND actionability IN ('blocked','needs_decision') ORDER BY work_key",
                },
            },
        )
        from catabolic.plans import observation_requirements

        self.assertEqual(
            observation_requirements(
                self.app, {"kind": "query", "query_id": query["id"]}
            )[0]["source"],
            "media",
        )
        watchers = Watchers(self.app)
        for reaction in ("report", "event"):
            watchers.put(
                reaction,
                {
                    "plan": {"kind": "query", "query_id": query["id"]},
                    "reaction": reaction,
                },
            )
        with patch("catabolic.plans.observation_requirements", return_value=[]):
            for name in ("report", "event"):
                self.assertTrue(watchers.run(name)["complete"])
            self.w.require("book", "review", "New review")
            for name in ("report", "event"):
                self.assertTrue(watchers.run(name)["complete"])
            with patch(
                "catabolic.plans.evaluate",
                side_effect=CatabolicError("injected evaluation failure"),
            ):
                self.assertFalse(watchers.run("report")["complete"])
            failed = next(r for r in self.rows() if r["subject_kind"] == "watcher")
            self.assertEqual(failed["actionability"], "blocked")
            self.assertEqual(failed["evidence"]["error"], "injected evaluation failure")
            self.assertTrue(watchers.run("report")["complete"])
            self.w.require("book", "review", "Another review")
            with patch(
                "catabolic.notifications.emit",
                side_effect=CatabolicError("injected reaction failure"),
            ):
                self.assertFalse(watchers.run("event")["complete"])
            pending = watchers.get("event")["pending_reaction"]
            self.assertIsNotNone(pending)
            self.assertTrue(
                any(
                    r["subject_id"] == "event" and r["actionability"] == "blocked"
                    for r in self.rows()
                )
            )
            with (
                patch(
                    "catabolic.plans.observation_requirements",
                    return_value=[{"source": "media"}],
                ),
                patch(
                    "catabolic.observations.observe",
                    return_value={"id": "pending-observation", "state": "queued"},
                ),
            ):
                self.assertFalse(watchers.run("event")["complete"])
            entry = next(r for r in self.rows() if r["subject_id"] == "event")
            self.assertEqual(entry["actionability"], "blocked")
            self.assertEqual(entry["reason"], "reaction_failed")
            self.assertEqual(entry["evidence"]["error"], "injected reaction failure")
            self.assertEqual(watchers.get("event")["pending_reaction"], pending)
            self.assertTrue(watchers.run("event")["complete"])
        self.assertFalse(any(r["subject_kind"] == "watcher" for r in self.rows()))

    def test_large_backlog_is_bounded_and_counts_are_not_page_counts(self):
        with self.store.transaction() as db:
            db.executemany(
                "INSERT INTO files(id,location,path) VALUES (?,'media',?)",
                ((f"bulk-{n:05d}", f"bulk/{n}.txt") for n in range(12000)),
            )
        page = execute_graphql(
            self.path,
            "{workInbox(first:7){nodes pageInfo{hasNextPage}}}",
            _store=self.store,
        )
        self.assertNotIn("errors", page)
        self.assertEqual(len(page["data"]["workInbox"]["nodes"]), 7)
        self.assertTrue(page["data"]["workInbox"]["pageInfo"]["hasNextPage"])
        count = execute_sql(
            self.path,
            "SELECT sum(countable) FROM catalog_work_inbox WHERE profile=:profile AND category='identification'",
            _store=self.store,
        )
        self.assertEqual(count["rows"], [[12000]])

    def test_http_isolation_and_no_implicit_work_selection(self):
        with self.assertRaises(CatabolicError):
            execute_sql(
                self.path,
                "SELECT * FROM catalog_work_inbox",
                _store=self.store,
                _http=True,
            )
        query = Queries(self.store).put(
            "work-rows",
            {
                "mode": "rows",
                "selection": {
                    "language": "sql",
                    "query": "SELECT work_key FROM catalog_work_inbox",
                },
            },
        )
        with self.assertRaises(CatabolicError):
            EvaluationSession(self.store).select(query["id"])

    def test_file_revision_preconditions_are_not_workflow_revisions(self):
        requirement = self.w.require(
            "book", "file", "Original", target=self.files["book.txt"]
        )
        first = next(
            r
            for r in self.rows(inactive=True)
            if r["source_id"] == requirement["requirement_id"]
        )
        revision = self.w.get("book")["revision"]
        (self.source / "book.txt").write_text("Changed original, still present")
        self.app.scan()
        second = next(
            r
            for r in self.rows(inactive=True)
            if r["source_id"] == requirement["requirement_id"]
        )
        self.assertEqual(first["work_key"], second["work_key"])
        self.assertEqual(self.w.get("book")["revision"], revision)
        with self.assertRaisesRegex(CatabolicError, "evidence changed"):
            validate_evidence(second, evidence_token(first))
        with self.assertRaisesRegex(CatabolicError, "cannot be manually completed"):
            self.w.resolve(
                requirement["requirement_id"], "complete", note="Cannot bypass evidence"
            )

    def test_deferred_required_job_details_and_unknown_file(self):
        processing = Processing(self.app)
        job = processing.enqueue("sniff", file_ids=[self.files["book.txt"]])["queued"][
            0
        ]
        self.w.require("book", "job", "Required sniff", target=job)
        with self.store.transaction() as db:
            db.execute("UPDATE processing_jobs SET state='failed' WHERE id=?", (job,))
        self.w.set_status("book", "deferred", note="Later")
        self.assertFalse(self.rows())
        self.w.set_status("book", "ignored", note="Not maintained")
        self.assertFalse(self.rows())
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO files(id,location,path) VALUES ('unknown','media','unknown.txt')"
            )
        unknown = next(r for r in self.rows() if r["subject_id"] == "unknown")
        self.assertEqual(unknown["source_status"], "unknown")
        self.assertIsNone(unknown["evidence_complete"])
        self.assertIsNone(unknown["current"])

    def test_success_on_new_revision_supersedes_optional_failure(self):
        processing = Processing(self.app)
        old = processing.enqueue("sniff", file_ids=[self.files["book.txt"]])["queued"][
            0
        ]
        with self.store.transaction() as db:
            db.execute("UPDATE processing_jobs SET state='failed' WHERE id=?", (old,))
        (self.source / "book.txt").write_text("New source revision")
        self.app.scan()
        processing.enqueue("sniff", file_ids=[self.files["book.txt"]])
        processing.run(limit=1)
        self.assertFalse(any(r["source_id"] == old for r in self.rows()))


class ProjectionInboxTest(unittest.TestCase):
    setUp = projection_fixtures.LifecycleTest.setUp
    bind = projection_fixtures.LifecycleTest.bind
    policy = projection_fixtures.LifecycleTest.policy
    apply = projection_fixtures.LifecycleTest.apply

    def entries(self):
        result = execute_sql(
            self.database,
            "SELECT work_key,category,actionability FROM catalog_work_inbox WHERE profile=:profile AND actionability IN ('ready','blocked','needs_decision')",
            _store=self.store,
        )
        return result["rows"]

    def test_usable_fallback_is_independent_of_original_readiness_and_reads_are_noops(
        self,
    ):
        from catabolic.item_workflow import ItemWorkflow
        from catabolic.reconcile import Reconciler

        workflow = ItemWorkflow(self.app)
        workflow.require(
            self.item, "file", "Keep original", target=self.files["main.bin"]
        )
        workflow.set_status(self.item, "complete")
        projection, _ = self.bind()
        self.assertTrue(self.apply(projection)["complete"])
        (self.source / "main.bin").unlink()
        self.app.scan()
        self.assertTrue(self.apply(projection)["complete"])
        self.assertEqual(workflow.get(self.item)["status"], "needs_attention")
        self.assertTrue(Reconciler(self.app).verify("global")["healthy"])
        before = list(self.store.db.iterdump())
        entries = self.entries()
        self.assertTrue(any(row[1] == "requirement" for row in entries))
        self.assertFalse(any(row[1] == "projection" for row in entries))
        self.assertEqual(list(self.store.db.iterdump()), before)

    def test_failed_projection_reaction_is_discoverable(self):
        from catabolic import plans
        from catabolic.reconcile import Reconciler

        self.bind("immediate")
        watchers = Watchers(self.app)
        watchers.put(
            "output",
            {
                "plan": {
                    "kind": "projection",
                    "catalog": "global",
                    "binding_digest": plans.projection_digest(self.app, "global"),
                },
                "reaction": "projection",
            },
        )
        watchers.enable("output", transfer=True)
        with (
            patch("catabolic.plans.observation_requirements", return_value=[]),
            patch(
                "catabolic.fallback_projection.FallbackProjection.apply_prepared",
                side_effect=CatabolicError("injected publication failure"),
            ),
        ):
            self.assertFalse(watchers.run("output")["complete"])
        self.assertTrue(
            any(row[1:] == ["projection_reaction", "blocked"] for row in self.entries())
        )
        with patch("catabolic.plans.observation_requirements", return_value=[]):
            self.assertTrue(watchers.run("output")["complete"])
        self.assertFalse(any(row[1] == "projection_reaction" for row in self.entries()))
        self.assertTrue(Reconciler(self.app).verify("global")["healthy"])

    def test_interrupted_link_journal_is_visible_until_owner_recovery(self):
        from catabolic.reconcile import Reconciler

        projection, _ = self.bind("immediate")
        self.apply(projection)
        (self.source / "main.bin").unlink()
        original = Reconciler._execute

        def crash(reconciler, operation, **kwargs):
            def interrupted(*args):
                raise RuntimeError("injected publication crash")

            return original(reconciler, operation, after_filesystem=interrupted)

        with (
            patch.object(Reconciler, "_execute", crash),
            self.assertRaisesRegex(RuntimeError, "injected"),
        ):
            self.apply(projection)
        self.assertTrue(any(row[1] == "projection_recovery" for row in self.entries()))
        with self.store.detached():
            result = subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "catabolic",
                    "--db",
                    str(self.database),
                    "--json",
                    "inbox",
                    "list",
                ],
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue(
                any(
                    r["category"] == "projection_recovery"
                    for r in json.loads(result.stdout)["entries"]
                )
            )
        Reconciler(self.app).recover()
        self.assertFalse(any(row[1] == "projection_recovery" for row in self.entries()))


@unittest.skipUnless(
    shutil.which("ffmpeg") and shutil.which("ffprobe"), "FFmpeg/ffprobe required"
)
class ArtifactInboxTest(unittest.TestCase):
    setUpClass = classmethod(artifact_fixtures.ArtifactTest.setUpClass.__func__)
    tearDownClass = classmethod(artifact_fixtures.ArtifactTest.tearDownClass.__func__)
    setUp = artifact_fixtures.ArtifactTest.setUp
    enqueue = artifact_fixtures.ArtifactTest.enqueue

    def artifact_action(self):
        result = execute_sql(
            self.path,
            "SELECT suggested_actions FROM catalog_work_inbox WHERE source_kind='processing_artifacts' AND source_status!='ready'",
            _store=self.store,
        )
        self.assertTrue(result["complete"], result)
        return json.loads(result["rows"][0][0])[0]

    def test_terminal_artifact_failure_suggests_working_retry(self):
        _, job = self.enqueue()
        with patch("catabolic.artifacts.os.fstatvfs") as stats:
            stats.return_value.f_bavail = 0
            stats.return_value.f_frsize = 4096
            self.assertFalse(self.a.run()["complete"])
        action = self.artifact_action()
        self.assertEqual(action["operation"], "process retry")
        self.assertEqual(action["arguments"], {"job_id": job["job_id"]})
        Processing(self.app).retry(action["arguments"]["job_id"])
        self.assertTrue(self.a.run()["complete"])

    def test_pending_publication_suggests_working_recovery(self):
        self.enqueue()
        with patch(
            "catabolic.artifacts.rename_noreplace", side_effect=KeyboardInterrupt
        ):
            with self.assertRaises(KeyboardInterrupt):
                self.a.run()
        action = self.artifact_action()
        self.assertEqual(action["operation"], "artifact recover")
        self.assertEqual(action["arguments"], {})
        self.assertTrue(self.a.recover()["complete"])
        self.assertEqual(self.a.list()["artifacts"][0]["state"], "ready")

    def test_real_artifact_failure_and_successor_keep_only_current_work(self):
        recipe, old = self.enqueue()
        self.source_file.unlink()
        self.source_file.write_bytes(self.payload)
        self.assertFalse(self.a.run()["complete"])
        failed_artifact = self.a.list()["artifacts"][0]

        def rows():
            result = execute_sql(
                self.path,
                "SELECT source_kind,source_id,actionability FROM catalog_work_inbox WHERE profile=:profile",
                _store=self.store,
            )
            self.assertTrue(result["complete"])
            return result["rows"]

        self.assertIn(
            ["processing_artifacts", failed_artifact["id"], "blocked"], rows()
        )
        self.app.scan()  # Record the changed input before evaluating retry eligibility.
        actions = execute_sql(
            self.path,
            "SELECT suggested_actions FROM catalog_work_inbox WHERE source_kind='processing_artifacts' AND source_id=:id",
            params=json.dumps({"id": failed_artifact["id"]}),
            _store=self.store,
        )
        self.assertTrue(actions["complete"], actions)
        self.assertEqual(
            json.loads(actions["rows"][0][0])[0],
            {
                "operation": "artifact show",
                "arguments": {"id": failed_artifact["id"]},
                "reload_required": True,
                "note_required": False,
            },
        )
        self.app.scan()
        self.a.enqueue(self.file_id, recipe["id"], "generated", self.item_id)
        result = self.a.run()
        self.assertTrue(result["complete"], result)
        artifact = self.a.get(result["completed"][0]["artifact_id"])
        self.assertTrue((self.generated / artifact["path"]).is_file())
        self.assertIn(["processing_jobs", old["job_id"], "historical"], rows())
        self.assertIn(
            ["processing_artifacts", failed_artifact["id"], "historical"], rows()
        )


class ExternalInboxTest(unittest.TestCase):
    setUp = programmable_fixtures.ProgrammableExternalTest.setUp

    def test_only_explicit_successful_replacement_retires_external_failure(self):
        programmable_fixtures.ProgrammableExternalTest.test_external_rule_uses_existing_fenced_receipt_jobs(
            self
        )
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO processor_jobs(id,profile,processor,request,digest,state,operation_id) SELECT 'old-external-failure',profile,processor,request,'old-digest','failed',operation_id FROM processor_jobs LIMIT 1"
            )
            db.execute(
                "INSERT INTO rule_processor_jobs(rule_id,job_id,file_id,item_id) SELECT rule_id,'old-external-failure',file_id,item_id FROM rule_processor_jobs LIMIT 1"
            )

        def state():
            return execute_sql(
                self.path,
                "SELECT actionability FROM catalog_work_inbox WHERE profile=:profile AND source_kind='processor_jobs' AND source_id='old-external-failure'",
                _store=self.store,
            )["rows"]

        self.assertEqual(state(), [["historical"]])
        with self.store.transaction() as db:
            db.execute(
                "DELETE FROM rule_processor_jobs WHERE job_id='old-external-failure'"
            )
        self.assertEqual(state(), [["blocked"]])


if __name__ == "__main__":
    unittest.main()
