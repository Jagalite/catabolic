# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Frozen fallback watcher baselines and explicitly gated projection effects."""

import json
import unittest

from catabolic import plans
from catabolic.catalog_refresh import CatalogRefresh
from catabolic.fallback_policies import Policies
from catabolic.network_adapters import Refresh
from catabolic.operations import Operations
from catabolic.watchers import Watchers
from tests import test_rust_fallback_projection as base


class NativeWatcherProjectionTest(unittest.TestCase):
    setUp = base.NativeProjectionTest.setUp
    policy = base.NativeProjectionTest.policy
    native = base.NativeProjectionTest.native
    bind = base.NativeProjectionTest.bind
    maxDiff = None

    def watcher(self, name="logical", *, kind="fallback", reaction="event", **extra):
        self.bind()
        config = self.store.rows("SELECT * FROM fallback_bindings")[0]
        reference = (
            {
                "kind": "fallback",
                "membership_query_id": config["query_id"],
                "policy_id": config["policy_id"],
            }
            if kind == "fallback"
            else {
                "kind": "projection",
                "catalog": "global",
                "binding_digest": plans.projection_digest(self.app, "global"),
            }
        )
        definition = {"plan": reference, "reaction": reaction, **extra}
        expected = Watchers(self.app).put(name, definition)
        actual = self.native(
            "put", {"name": name, "definition": definition}, surface="watcher"
        )
        self.assertEqual(actual, expected)
        return actual["definition"]["plan"]

    def call_run(self, name="logical", **options):
        return self.native("run", {"name": name, **options}, surface="watcher")

    def test_fallback_event_baseline_matches_frozen_and_changes_once(self):
        reference = self.watcher()
        expected = plans.compare(
            None, plans.evaluate(self.app, reference, context_name="watcher:logical")
        )
        actual = self.call_run()
        self.assertTrue(actual["complete"], actual)
        self.assertEqual(actual["evaluation"], expected)
        again = self.call_run()
        self.assertFalse(again["evaluation"]["changed"])
        self.assertEqual(
            len(
                self.store.rows(
                    "SELECT * FROM consumer_events WHERE event='watcher_changed'"
                )
            ),
            1,
        )
        (self.source / "main.bin").unlink()
        previous = actual["evaluation"]["baseline"]
        expected = plans.compare(
            previous,
            plans.evaluate(
                self.app, reference, previous=previous, context_name="watcher:logical"
            ),
        )
        changed = self.call_run()
        self.assertTrue(changed["complete"], changed)
        self.assertEqual(changed["evaluation"], expected)
        self.assertEqual(
            changed["evaluation"]["baseline"]["value"][0]["file_id"],
            self.files["backup.bin"],
        )
        self.assertEqual(self.store.rows("SELECT * FROM fallback_entries"), [])

    def test_projection_report_matches_frozen_without_publication(self):
        reference = self.watcher(kind="projection", reaction="report")
        expected = plans.compare(None, plans.evaluate(self.app, reference))
        result = self.call_run(disposable_fixture=False)
        self.assertTrue(result["complete"], result)
        self.assertEqual(result["evaluation"], expected)
        self.assertEqual(self.store.rows("SELECT * FROM fallback_entries"), [])
        self.assertEqual(list(self.output.rglob("*.bin")), [])

    def test_processing_uses_resolved_fallback_and_retains_unresolved_reaction(self):
        operation = Operations(self.app).put(
            "hash", {"kind": "analysis", "operation": "hash", "options": {}}
        )["id"]
        self.watcher(reaction="processing", operation_id=operation)
        result = self.call_run()
        self.assertTrue(result["complete"], result)
        self.assertEqual(
            result["reaction"]["jobs"][0]["input"]["file_id"], self.files["main.bin"]
        )
        self.assertEqual(
            self.store.rows("SELECT state,recipe_id FROM processing_jobs"),
            [{"state": "queued", "recipe_id": operation}],
        )
        (self.source / "main.bin").unlink()
        (self.source / "backup.bin").unlink()
        failed = self.call_run()
        self.assertFalse(failed["complete"])
        self.assertEqual(failed["error"], "processing fallback has unresolved inputs")
        row = self.store.rows("SELECT * FROM watchers")[0]
        self.assertEqual(row["pending_reaction"], failed["run_id"])
        self.assertGreater(row["requested_generation"], row["completed_generation"])
        self.assertEqual(len(self.store.rows("SELECT * FROM processing_jobs")), 1)

    def test_projection_gate_precedes_claim_and_explicit_fixture_publishes(self):
        self.watcher(kind="projection", reaction="projection")
        self.native("admit", {"name": "logical"}, surface="watcher")
        before = self.store.rows("SELECT * FROM watchers")
        self.native(
            "run",
            {"name": "logical", "disposable_fixture": False},
            surface="watcher",
            failure="restricted to disposable debug fixtures",
        )
        self.assertEqual(self.store.rows("SELECT * FROM watchers"), before)
        self.assertEqual(self.store.rows("SELECT * FROM watcher_runs"), [])
        result = self.call_run()
        self.assertTrue(result["complete"], result)
        self.assertTrue(result["reaction"]["complete"])
        link = next(path for path in self.output.rglob("*") if path.is_symlink())
        self.assertEqual(link.resolve(), self.source / "main.bin")
        recorded = json.loads(
            self.store.rows("SELECT result FROM watcher_runs")[0]["result"]
        )
        self.assertEqual(recorded["reaction"]["plan_id"], result["reaction"]["plan_id"])

    def test_supervisor_requires_explicit_fixture_option_for_each_tick(self):
        self.watcher(kind="projection", reaction="projection")
        self.native("admit", {"name": "logical"}, surface="watcher")
        gated = self.native("tick", {"disposable_fixture": False}, surface="watcher")
        self.assertFalse(gated["complete"])
        self.assertEqual(self.store.rows("SELECT * FROM watcher_runs"), [])
        result = self.native("tick", {}, surface="watcher")
        self.assertTrue(result["complete"], result)
        self.native("admit", {"name": "logical"}, surface="watcher")
        before = self.store.rows("SELECT * FROM watchers")
        gated = self.native("tick", {"disposable_fixture": False}, surface="watcher")
        self.assertFalse(gated["complete"])
        self.assertEqual(self.store.rows("SELECT * FROM watchers"), before)

    def test_stale_projection_definition_refuses_before_admission(self):
        reference = self.watcher(kind="projection", reaction="report")
        reference["binding_digest"] = "0" * 64
        before = self.store.rows("SELECT * FROM watcher_definitions")
        self.native(
            "put",
            {"name": "stale", "definition": {"plan": reference}},
            surface="watcher",
            failure="stale_watcher_projection_binding",
        )
        self.assertEqual(self.store.rows("SELECT * FROM watcher_definitions"), before)

    def test_projection_ownership_transfer_matches_frozen_and_preserves_queue(self):
        self.watcher(kind="projection", reaction="projection")
        with self.store.transaction() as db:
            db.execute("UPDATE fallback_bindings SET enabled=1")
            db.execute(
                "INSERT INTO catalog_refresh_settings VALUES('default','global',1,0)"
            )
            db.execute(
                "INSERT INTO catalog_refresh_queue(profile,catalog) VALUES('default','global')"
            )
        self.native(
            "enable",
            {"name": "logical"},
            surface="watcher",
            failure="explicit transfer required",
        )
        actual = self.native(
            "enable", {"name": "logical", "transfer": True}, surface="watcher"
        )
        self.assertEqual(actual["enabled"], 1)
        self.assertEqual(
            self.store.rows("SELECT watcher FROM watcher_owners"),
            [{"watcher": "logical"}],
        )
        self.assertEqual(
            self.store.rows("SELECT generation FROM catalog_refresh_queue"),
            [{"generation": 1}],
        )
        self.native(
            "put",
            {"name": "other", "definition": actual["definition"]},
            surface="watcher",
        )
        self.native(
            "enable",
            {"name": "other", "transfer": True},
            surface="watcher",
            failure="owned by another watcher",
        )
        self.native("disable", {"name": "logical"}, surface="watcher")
        self.assertEqual(self.store.rows("SELECT * FROM watcher_owners"), [])

    def test_fallback_refresh_retains_gated_demand_then_explicitly_publishes(self):
        self.bind()
        result = self.native(
            "configure",
            {"catalog": "global", "enabled": True, "disposable_fixture": False},
            surface="catalog-refresh",
        )
        self.assertFalse(result["catalog_refresh"]["complete"])
        self.assertEqual(self.store.rows("SELECT * FROM fallback_entries"), [])
        pending = self.store.rows("SELECT * FROM catalog_refresh_queue")
        self.assertEqual(len(pending), 1)
        self.assertIn("restricted to disposable debug fixtures", pending[0]["error"])
        result = self.native("run", {"force": True}, surface="catalog-refresh")
        self.assertEqual(
            result,
            {
                "processed": 1,
                "refreshed": ["global"],
                "errors": [],
                "pending": 0,
                "complete": True,
            },
        )
        self.assertEqual(
            len([path for path in self.output.rglob("*") if path.is_symlink()]), 1
        )

    def test_fallback_refresh_preserves_frozen_legacy_notification_behavior(self):
        oracle = base.NativeProjectionTest("test_preview_is_exact_and_write_free")
        oracle.setUp()
        self.addCleanup(oracle.doCleanups)
        oracle.bind()
        Refresh(oracle.app).configure("global", "http://127.0.0.1:9", "UNUSED")
        expected = CatalogRefresh(oracle.app).configure("global", enabled=True)
        expected_events = oracle.store.rows(
            "SELECT profile,catalog FROM refresh_events"
        )
        self.assertEqual(expected_events, [{"profile": "default", "catalog": "global"}])
        # Release the oracle's writer before invoking unrelated native commands.
        oracle.store.close()
        self.bind()
        Refresh(self.app).configure("global", "http://127.0.0.1:9", "UNUSED")
        actual = self.native(
            "configure",
            {"catalog": "global", "enabled": True},
            surface="catalog-refresh",
        )
        self.assertEqual(actual, expected)
        self.assertEqual(
            self.store.rows("SELECT profile,catalog FROM refresh_events"),
            expected_events,
        )

    def test_blocked_fallback_evaluation_preserves_baseline_and_pending_demand(self):
        reference = self.watcher()
        first = self.call_run()
        self.assertTrue(first["complete"], first)
        definition = Policies(self.store).get(reference["policy_id"])["definition"]
        definition["budgets"]["max_checks"] = 1
        definition["budgets"]["max_candidates"] = 1
        policy = Policies(self.store).put("blocked", definition)["id"]
        row = self.native("get", {"name": "logical"}, surface="watcher")
        row["definition"]["plan"]["policy_id"] = policy
        self.native(
            "put",
            {"name": "blocked", "definition": row["definition"]},
            surface="watcher",
        )
        # The first tier is unusable; the cumulative check bound blocks fallback.
        (self.source / "main.bin").unlink()
        result = self.call_run("blocked")
        self.assertFalse(result["complete"], result)
        self.assertIn("incomplete_evaluation_has_no_baseline", result["error"])
        blocked = self.store.rows("SELECT * FROM watchers WHERE name='blocked'")[0]
        self.assertIsNone(blocked["baseline"])
        self.assertGreater(
            blocked["requested_generation"], blocked["completed_generation"]
        )


if __name__ == "__main__":
    unittest.main()
