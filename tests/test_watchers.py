# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import copy
import time
import unittest
from unittest.mock import patch

from catabolic import observations, plans
from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.evaluation import EvaluationSession
from catabolic.saved_queries import Queries
from catabolic.store import Store
from catabolic.watchers import Watchers
from tests import test_fallback_lifecycle as fixtures


class WatcherTest(unittest.TestCase):
    setUp = fixtures.LifecycleTest.setUp
    policy = fixtures.LifecycleTest.policy
    bind = fixtures.LifecycleTest.bind

    def definition(self):
        query = Queries(self.store).put(
            "audit",
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT id AS file_id FROM files",
                }
            },
        )["id"]
        return {"plan": {"kind": "query", "query_id": query}}

    def test_admission_during_run_survives_completion_and_restart(self):
        from catabolic.supervisor import tick

        watchers = Watchers(self.app)
        watchers.put("manual", self.definition())
        original = plans.evaluate

        def admit_again(*args, **kwargs):
            watchers.admit("manual")
            return original(*args, **kwargs)

        with (
            patch("catabolic.plans.observation_requirements", return_value=[]),
            patch("catabolic.plans.evaluate", side_effect=admit_again),
        ):
            self.assertTrue(watchers.run("manual")["complete"])
        row = watchers.get("manual")
        self.assertEqual(
            (row["requested_generation"], row["completed_generation"]), (2, 1)
        )
        with self.store.detached():
            self.assertTrue(tick(self.database)["complete"])
        row = watchers.get("manual")
        self.assertEqual(row["completed_generation"], 2)
        self.assertEqual(len(watchers.history("manual")), 2)

    def test_disable_pauses_failed_reaction_until_explicit_admission(self):
        from catabolic.supervisor import tick

        self.bind("immediate")
        watchers = Watchers(self.app)
        watchers.put(
            "plex",
            {
                "plan": {
                    "kind": "projection",
                    "catalog": "global",
                    "binding_digest": plans.projection_digest(self.app, "global"),
                },
                "reaction": "projection",
            },
        )
        watchers.enable("plex", transfer=True)
        with patch(
            "catabolic.fallback_projection.FallbackProjection.apply_prepared",
            side_effect=CatabolicError("injected"),
        ):
            self.assertFalse(watchers.run("plex")["complete"])
        pending = watchers.get("plex")["pending_reaction"]
        self.assertIsNotNone(pending)
        watchers.enable("plex", False)
        with self.store.detached():
            tick(self.database)
        self.assertEqual(len(watchers.history("plex")), 1)
        self.assertEqual(watchers.get("plex")["pending_reaction"], pending)
        with self.assertRaisesRegex(CatabolicError, "watcher_not_due"):
            watchers.run("plex", trigger="scheduled")
        watchers.admit("plex")
        with self.store.detached():
            self.assertTrue(tick(self.database)["complete"])
        self.assertIsNone(watchers.get("plex")["pending_reaction"])

    def test_report_session_restrictions_and_aggregate_bytes(self):
        query = Queries(self.store).put(
            "private",
            {
                "mode": "rows",
                "selection": {
                    "language": "sql",
                    "query": "SELECT name FROM watcher_definitions",
                },
            },
        )["id"]
        with self.assertRaisesRegex(CatabolicError, "prohibited"):
            Queries(self.store).run(
                query, session=EvaluationSession(self.store, http=True)
            )
        query = Queries(self.store).put(
            "rows",
            {
                "mode": "rows",
                "selection": {"language": "sql", "query": "SELECT 1 AS n"},
            },
        )["id"]
        session = EvaluationSession(self.store)
        Queries(self.store).run(query, session=session)
        self.assertGreater(session.result_bytes, 1)
        session.max_bytes = session.result_bytes
        with self.assertRaisesRegex(CatabolicError, "byte_budget"):
            Queries(self.store).run(query, session=session)
        with self.assertRaisesRegex(CatabolicError, "byte_budget"):
            Queries(self.store).run(
                query, session=EvaluationSession(self.store, max_bytes=1)
            )

    def test_many_reports_share_inventory(self):
        watchers = Watchers(self.app)
        definition = self.definition()
        from catabolic.app import walk_files

        for r in self.store.rows("SELECT id FROM locations"):
            observations.dirty(self.app, r["id"])
        before = self.store.rows("SELECT count(*) AS n FROM scans")[0]["n"]
        with patch("catabolic.app.walk_files", wraps=walk_files):
            for i in range(10):
                watchers.put("audit" + str(i), definition)
                result = watchers.run("audit" + str(i))
                self.assertTrue(result["complete"], result)
            self.assertEqual(
                self.store.rows("SELECT count(*) AS n FROM scans")[0]["n"] - before, 1
            )
        self.assertTrue(all(not w["enabled"] for w in watchers.list()))

    def test_scan_releases_writer_and_dirty_during_scan_survives(self):
        from catabolic.app import walk_files

        source = self.store.rows("SELECT id FROM locations")[0]["id"]

        def walking(*args, **kwargs):
            with Store(self.database, writable=True) as store:
                observations.dirty(Application(store), source)
            return walk_files(*args, **kwargs)

        observations.dirty(self.app, source)
        with patch("catabolic.app.walk_files", side_effect=walking):
            result = observations.observe(self.app, source)
        self.assertTrue(result["report"]["complete"])
        following = observations.request(self.app, source, max_age=60)
        self.assertEqual(following["reuse"], "created")

    def test_stale_scanner_cannot_publish(self):
        source = self.store.rows("SELECT id FROM locations")[0]["id"]
        first = observations.claim(self.app, observations.request(self.app, source))
        observations.dirty(self.app, source)
        with self.store.transaction() as db:
            db.execute(
                "UPDATE observation_jobs SET worker_pid=2147483000 WHERE id=?",
                (first["id"],),
            )
        observations.claim(self.app, observations.request(self.app, source))
        with self.assertRaisesRegex(CatabolicError, "stale_observation"):
            observations.validate(self.app, first)

    def test_session_caches_and_aggregates(self):
        query = self.definition()["plan"]["query_id"]
        session = EvaluationSession(self.store, max_ids=100)
        session.select(query)
        count = session.context["count"]
        session.select(query)
        self.assertEqual(session.context["count"], count)
        session.cancelled = True
        with self.assertRaisesRegex(CatabolicError, "cancelled"):
            session.select(query)

    def test_projection_one_resolution_and_binding_fence(self):
        projection, _ = self.bind("immediate")
        definition = {
            "plan": {
                "kind": "projection",
                "catalog": "global",
                "binding_digest": plans.projection_digest(self.app, "global"),
            },
            "reaction": "projection",
        }
        watchers = Watchers(self.app)
        watchers.put("plex", definition)
        from catabolic.fallback_projection import FallbackProjection

        with patch.object(
            FallbackProjection,
            "plan",
            autospec=True,
            side_effect=FallbackProjection.plan,
        ) as preparing:
            result = watchers.run("plex")
            self.assertTrue(result["complete"], result)
            self.assertEqual(preparing.call_count, 1)
        changed = copy.deepcopy(definition)
        changed["plan"]["binding_digest"] = "wrong"
        with self.assertRaisesRegex(CatabolicError, "stale_watcher"):
            watchers.put("bad", changed)

    def test_failed_reaction_remains_pending(self):
        self.bind("immediate")
        watchers = Watchers(self.app)
        watchers.put(
            "plex",
            {
                "plan": {
                    "kind": "projection",
                    "catalog": "global",
                    "binding_digest": plans.projection_digest(self.app, "global"),
                },
                "reaction": "projection",
            },
        )
        with patch(
            "catabolic.fallback_projection.FallbackProjection.apply_prepared",
            side_effect=CatabolicError("injected"),
        ):
            self.assertFalse(watchers.run("plex")["complete"])
        row = watchers.get("plex")
        self.assertIsNotNone(row["pending_reaction"])
        self.assertIsNotNone(row["baseline"])
        self.assertTrue(watchers.run("plex")["complete"])
        self.assertIsNone(watchers.get("plex")["pending_reaction"])

    def test_schedules_do_not_follow_shared_scans(self):
        from catabolic.supervisor import due

        watchers = Watchers(self.app)
        definition = self.definition()
        definition["schedule"] = {"kind": "interval", "seconds": 3600}
        watchers.put("slow", definition)
        watchers.enable("slow")
        row = self.store.rows(
            "SELECT w.*,d.definition FROM watchers w JOIN watcher_definitions d ON d.id=w.definition_id"
        )[0]
        self.assertFalse(due(row, time.time()))

    def test_structural_comparison_preserves_multiplicity_and_arrays(self):
        for kind, a, b in [
            (
                "rows",
                {"columns": ["x", "x"], "rows": [[1, 2], [1, 2]]},
                {"columns": ["x", "x"], "rows": [[1, 2]]},
            ),
            ("document", {"data": {"x": [1, 2]}}, {"data": {"x": [2, 1]}}),
        ]:
            previous = plans.semantic({"kind": kind, "value": a})
            self.assertTrue(
                plans.compare(previous, {"kind": kind, "value": b, "complete": True})[
                    "changed"
                ]
            )
            with self.assertRaises(CatabolicError):
                plans.compare(previous, {"kind": kind, "value": b, "complete": False})

    def test_portable_queries_and_watchers_import_disabled(self):
        from catabolic.program_bundle import Programs

        watchers = Watchers(self.app)
        watchers.put("audit", self.definition())
        bundle = Programs(self.app).export(watchers=["audit"])
        self.assertEqual(bundle["version"], 3)
        self.assertNotIn("enabled", str(bundle))
        Programs(self.app).import_bundle(bundle, prefix="portable")
        self.assertFalse(watchers.get("portable.audit")["enabled"])
        self.assertTrue(watchers.run("portable.audit")["complete"])

    def test_automatic_ownership_and_legacy_skip(self):
        from catabolic.catalog_refresh import CatalogRefresh
        from catabolic.fallback_worker import configure, tick

        self.bind("immediate")
        configure(self.app, "global", enabled=True)
        watchers = Watchers(self.app)
        definition = {
            "plan": {
                "kind": "projection",
                "catalog": "global",
                "binding_digest": plans.projection_digest(self.app, "global"),
            },
            "reaction": "projection",
            "schedule": {"kind": "interval", "seconds": 3600},
        }
        watchers.put("plex", definition)
        with self.assertRaisesRegex(CatabolicError, "transfer"):
            watchers.enable("plex")
        watchers.enable("plex", transfer=True)
        from catabolic.maintenance import run as maintain

        with self.assertRaisesRegex(CatabolicError, "watcher_owned_projection"):
            maintain(self.app, catalog="global")
        watchers.put("other", definition)
        with self.assertRaisesRegex(CatabolicError, "owned"):
            watchers.enable("other")
        with self.store.transaction() as db:
            db.execute("UPDATE fallback_bindings SET enabled=1")
            db.execute("UPDATE catalog_refresh_settings SET enabled=1")
            db.execute(
                "INSERT OR REPLACE INTO catalog_refresh_queue(profile,catalog) VALUES ('default','global')"
            )
        with self.store.detached():
            self.assertEqual(tick(self.database)["processed"], 0)
        self.assertEqual(CatalogRefresh(self.app).run()["refreshed"], [])

    def test_offline_scan_preserves_membership_and_fallback(self):
        self.bind("immediate")
        watchers = Watchers(self.app)
        watchers.put(
            "plex",
            {
                "plan": {
                    "kind": "projection",
                    "catalog": "global",
                    "binding_digest": plans.projection_digest(self.app, "global"),
                },
                "reaction": "projection",
            },
        )
        self.assertTrue(watchers.run("plex")["complete"])
        (self.source / "main.bin").unlink()
        for row in self.store.rows("SELECT id FROM locations"):
            observations.dirty(self.app, row["id"])
        result = watchers.run("plex")
        self.assertTrue(result["complete"], result)
        self.assertEqual(
            result["reaction"]["desired"][0]["file_id"], self.files["backup.bin"]
        )

    def test_unchanged_publication_keeps_verified_generation(self):
        projection, _ = self.bind("immediate")
        first = projection.run("global", apply=True, automatic=True)
        previous = self.store.rows("SELECT published_generation FROM fallback_entries")[
            0
        ]["published_generation"]
        second = projection.run("global", apply=True, automatic=True)
        self.assertTrue(first["complete"] and second["complete"])
        self.assertEqual(second["publication"]["applied"], [])
        self.assertEqual(
            self.store.rows("SELECT published_generation FROM fallback_entries")[0][
                "published_generation"
            ],
            previous,
        )


class CronTest(unittest.TestCase):
    def test_dst_and_numeric_grammar(self):
        from datetime import datetime, timezone

        from catabolic.watcher_schedule import fields, next_due

        spring = datetime(2026, 3, 8, 6, tzinfo=timezone.utc).timestamp()
        value = next_due(
            {"kind": "cron", "cron": "30 2 * * *", "timezone": "America/New_York"},
            spring,
        )
        self.assertEqual(
            datetime.fromtimestamp(value, timezone.utc).isoformat(),
            "2026-03-09T06:30:00+00:00",
        )
        fall = datetime(2026, 11, 1, 5, 31, tzinfo=timezone.utc).timestamp()
        value = next_due(
            {"kind": "cron", "cron": "30 1 * * *", "timezone": "America/New_York"}, fall
        )
        self.assertEqual(
            datetime.fromtimestamp(value, timezone.utc).isoformat(),
            "2026-11-02T06:30:00+00:00",
        )
        with self.assertRaises(CatabolicError):
            fields("* * * * */0")


class ScaleTest(unittest.TestCase):
    setUp = WatcherTest.setUp
    definition = WatcherTest.definition

    # Reuse only fixture setup and definition construction; do not duplicate suites.
    def test_hundred_watchers_ten_sources(self):
        for index in range(9):
            root = self.root / ("extra" + str(index))
            root.mkdir()
            (root / "sample.bin").write_bytes(b"fixture")
            self.app.bind("source", "extra" + str(index), str(root))
        watchers = Watchers(self.app)
        definition = self.definition()
        for index in range(100):
            watchers.put("scale" + str(index), definition)
        from catabolic.app import walk_files

        for row in self.store.rows("SELECT id FROM locations"):
            observations.dirty(self.app, row["id"])
        before = self.store.rows("SELECT count(*) AS n FROM scans")[0]["n"]
        with patch("catabolic.app.walk_files", wraps=walk_files):
            for index in range(100):
                report = watchers.run("scale" + str(index))
                self.assertTrue(report["complete"], report)
            self.assertEqual(
                self.store.rows("SELECT count(*) AS n FROM scans")[0]["n"] - before, 10
            )


class ProcessingReactionTest(unittest.TestCase):
    setUp = WatcherTest.setUp
    definition = WatcherTest.definition

    def test_approved_analysis_admits_once_and_does_not_execute(self):
        from catabolic.operations import Operations

        operation = Operations(self.app).put(
            "sniff", {"kind": "analysis", "operation": "sniff", "options": {}}
        )["id"]
        definition = self.definition()
        definition.update(reaction="processing", operation_id=operation, max_jobs=10)
        watchers = Watchers(self.app)
        watchers.put("analysis", definition)
        result = watchers.run("analysis")
        self.assertTrue(result["complete"], result)
        jobs = self.store.rows("SELECT id,state FROM processing_jobs")
        self.assertTrue(jobs)
        self.assertTrue(watchers.run("analysis")["complete"])
        self.assertEqual(self.store.rows("SELECT id,state FROM processing_jobs"), jobs)
