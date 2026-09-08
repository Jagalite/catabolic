# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import copy
import unittest
from unittest.mock import patch

from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.manifest import Manifest
from catabolic.operations import Operations
from catabolic.projection_stats import NAMESPACE, read
from catabolic.projections import Projections
from catabolic.reconcile import Reconciler
from catabolic.rules import Rules
from catabolic.saved_queries import Queries
from tests import test_programmable_catalog, test_query_layouts


class ProjectionStatisticsTest(unittest.TestCase):
    setUp = test_query_layouts.QueryLayoutTest.setUp
    save = test_programmable_catalog.ProgrammableCatalogTest.save
    projection = test_programmable_catalog.ProgrammableCatalogTest.projection

    def build(self):
        with self.store.transaction():
            return Manifest(self.app).build("query")

    def stats(self):
        return self.build()["content"]["extra"][NAMESPACE]

    def prepare(self):
        query = self.save()
        self.projection(query)
        return query

    def test_preview_is_readonly_and_export_does_not_execute(self):
        self.prepare()
        before = test_query_layouts.QueryLayoutTest.state(self)
        preview = Projections(self.app).run("query", limit=1)
        self.assertTrue(preview["safe"])
        self.assertEqual(before, test_query_layouts.QueryLayoutTest.state(self))
        self.assertIsNone(read(self.app, "selection", "query"))
        self.assertIsNone(read(self.app, "execution", "query"))
        with (
            patch.object(
                Queries, "select", side_effect=AssertionError("query executed")
            ),
            patch.object(
                Reconciler, "verify", side_effect=AssertionError("live verify")
            ),
        ):
            first, second = self.build(), self.build()
        self.assertEqual(first["content_sha256"], second["content_sha256"])
        self.assertFalse(
            first["content"]["extra"][NAMESPACE]["filesystem_verified_at_export"]
        )

    def test_full_action_counts_and_repeat_execution(self):
        self.prepare()
        result = Projections(self.app).run("query", apply=True, limit=1)
        self.assertTrue(result["complete"])
        stats = self.stats()
        self.assertEqual(stats["current_recorded"]["desired_entries"], 2)
        self.assertEqual(
            stats["current_recorded"]["last_query_evaluation"]["query"]["selected_ids"],
            2,
        )
        execution = stats["last_execution"]
        self.assertEqual(execution["status"], "complete")
        self.assertEqual(execution["applied_actions"]["create"], 2)
        self.assertEqual(execution["verification"]["verified_links"], 2)
        self.assertEqual(execution["verification"]["issue_count"], 0)
        self.assertGreaterEqual(execution["elapsed_seconds"], 0)
        self.assertIsNotNone(execution["inputs"]["selection"]["newest_observation_at"])
        Projections(self.app).run("query", apply=True)
        repeat = self.stats()["last_execution"]
        self.assertEqual(repeat["applied_actions"], {})
        self.assertEqual(repeat["planned_actions"]["unchanged"], 2)

    def test_recorded_bytes_deduplicate_and_preserve_unknowns_and_history(self):
        self.prepare()
        Projections(self.app).run("query", apply=True)
        original = copy.deepcopy(self.stats()["last_execution"])
        rows = self.store.rows(
            "SELECT * FROM mappings WHERE catalog='query' AND active=1 ORDER BY path"
        )
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO mappings(id,catalog,file_id,item_id,path,active) VALUES ('duplicate','query',?,?, 'duplicate.flac',1)",
                (rows[0]["file_id"], rows[0]["item_id"]),
            )
            db.execute(
                "UPDATE observations SET size=?,status='missing' WHERE profile=? AND file_id=?",
                (2**60, self.app.profile, rows[0]["file_id"]),
            )
            db.execute(
                "DELETE FROM observations WHERE profile=? AND file_id=?",
                (self.app.profile, rows[1]["file_id"]),
            )
        stats = self.stats()
        current = stats["current_recorded"]
        self.assertEqual(current["desired_entries"], 3)
        self.assertEqual(current["selection"]["files"], 2)
        self.assertEqual(current["selection"]["known_referenced_bytes"], str(2**60))
        self.assertEqual(current["selection"]["unknown_size_files"], 1)
        self.assertEqual(
            current["selection"]["availability"],
            {"present": 0, "missing": 1, "unknown": 1},
        )
        self.assertEqual(current["selection"]["unobserved_files"], 1)
        self.assertEqual(stats["last_execution"], original)

    def test_interruption_never_claims_completion_and_recovery_can_retry(self):
        self.prepare()
        self.layouts.run("flat", "query", apply=True)

        def crash(_):
            raise RuntimeError("interrupted")

        with self.assertRaisesRegex(RuntimeError, "interrupted"):
            Reconciler(self.app).apply("query", after_filesystem=crash)
        saved = read(self.app, "execution", "query")
        self.assertEqual(saved["status"], "completion_unknown")
        self.assertIsNone(saved["finished_at"])
        self.assertIsNone(saved["applied_actions"])
        self.assertIsNone(saved["verification"])
        Reconciler(self.app).recover("query")
        self.assertEqual(read(self.app, "execution", "query"), saved)
        self.assertTrue(Reconciler(self.app).apply("query")["healthy"])
        self.assertEqual(self.stats()["last_execution"]["status"], "complete")

    def test_rule_progress_is_scoped_and_snapshot_is_retained(self):
        query = self.prepare()
        rules = Rules(self.app)
        operation = Operations(self.app).put(
            "checksum", {"kind": "analysis", "operation": "hash"}
        )
        rule = rules.put("checksum", operation["id"], None, {"query_id": query["id"]})
        unrelated_query = self.save(
            "movies", "SELECT item_id FROM catalog_items WHERE kind='movie'"
        )
        rules.put(
            "unrelated", operation["id"], None, {"query_id": unrelated_query["id"]}
        )
        self.assertIsNone(
            self.stats()["current_recorded"]["rules"][0]["last_evaluation"]
        )
        rules.apply(rule["id"], batch=100)
        Projections(self.app).run("query", apply=True)
        before = self.stats()
        records = before["current_recorded"]["rules"]
        self.assertEqual([r["id"] for r in records], [rule["id"]])
        self.assertEqual(records[0]["last_evaluation"]["matched_inputs"], 2)
        self.assertEqual(records[0]["recorded_job_states"]["local"], {"queued": 2})
        self.assertTrue(rules.run(rule["id"], batch=100)["complete"])
        after = self.stats()
        self.assertEqual(
            after["current_recorded"]["rules"][0]["recorded_job_states"]["local"],
            {"complete": 2},
        )
        self.assertEqual(
            after["current_recorded"]["rules"][0]["last_evaluation"]["counts"][
                "satisfied"
            ],
            2,
        )
        self.assertIsNotNone(
            after["current_recorded"]["rules"][0]["latest_local_job_finished_at"]
        )
        self.assertEqual(after["last_execution"], before["last_execution"])

    def test_extension_namespace_is_reserved_and_profiles_are_isolated(self):
        self.prepare()
        Projections(self.app).run("query", apply=True)
        with (
            self.store.transaction(),
            self.assertRaisesRegex(CatabolicError, "reserved"),
        ):
            Manifest(self.app).build("query", extra={NAMESPACE: {"version": 999}})
        self.app.add_profile("other")
        other = Application(self.store, "other")
        with self.store.transaction():
            stats = Manifest(other).build("query")["content"]["extra"][NAMESPACE]
        self.assertIsNone(stats["last_execution"])
        self.assertIsNone(stats["current_recorded"]["last_query_evaluation"])
        self.assertEqual(
            stats["current_recorded"]["selection"]["availability"]["unknown"], 2
        )

    def test_failed_verification_is_recorded_without_claiming_healthy_links(self):
        self.prepare()
        self.layouts.run("flat", "query", apply=True)
        removed = False

        def lose_source(operation):
            nonlocal removed
            if operation["kind"] == "create" and not removed:
                (self.root / "media/track01.flac").unlink()
                removed = True

        result = Reconciler(self.app).apply("query", after_filesystem=lose_source)
        self.assertFalse(result["healthy"])
        saved = read(self.app, "execution", "query")
        self.assertEqual(saved["status"], "verification_failed")
        self.assertEqual(saved["verification"]["verified_links"], 1)
        self.assertEqual(saved["verification"]["issue_count"], 1)
        self.assertEqual(saved["applied_actions"]["create"], 2)
        self.assertIsNotNone(saved["finished_at"])

    def test_rejected_empty_projection_preserves_last_execution_and_selection(self):
        self.prepare()
        Projections(self.app).run("query", apply=True)
        saved = self.stats()
        empty = self.save("empty", "SELECT item_id FROM catalog_items WHERE 0")
        self.projection(empty)
        result = Projections(self.app).run("query", apply=True)
        self.assertFalse(result["safe"])
        current = self.stats()
        self.assertEqual(current["last_execution"], saved["last_execution"])
        self.assertEqual(
            current["current_recorded"]["last_query_evaluation"],
            saved["current_recorded"]["last_query_evaluation"],
        )
