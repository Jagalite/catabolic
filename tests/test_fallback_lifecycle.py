# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import copy
import unittest
from unittest.mock import patch

from catabolic.domain import CatabolicError
from catabolic.fallback_policies import Policies
from catabolic.fallback_projection import FallbackProjection
from catabolic.fallback_resolution import Resolver
from catabolic.layouts import PRESETS, Layouts
from catabolic.reconcile import Reconciler
from catabolic.saved_queries import Queries
from catabolic.store import Store
from tests import test_fallback_resolution as fixtures


class LifecycleTest(unittest.TestCase):
    setUp = fixtures.ResolverTest.setUp
    policy = fixtures.ResolverTest.policy

    def bind(self, mode="stable", seconds=300):
        original = Policies(self.store).get(self.policy())["definition"]
        original["failback"] = {
            "mode": mode,
            "minimum_healthy_seconds": seconds,
            "minimum_successful_checks": 2,
        }
        policy = Policies(self.store).put("lifecycle", original)["id"]
        members = Queries(self.store).put(
            "members",
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT id AS item_id FROM items",
                }
            },
        )["id"]
        Layouts(self.app).put("flat", copy.deepcopy(PRESETS["flat"]))
        projection = FallbackProjection(self.app)
        projection.bind("global", policy, members, "flat")
        return projection, policy

    def apply(self, projection):
        plan = projection.run("global")
        return projection.run("global", apply=True, expected_plan=plan["plan_id"])

    def test_stability_restart_preview_and_flapping(self):
        projection, policy = self.bind()
        self.assertTrue(self.apply(projection)["complete"])
        primary = self.source / "main.bin"
        offline = self.root / "offline.bin"
        primary.rename(offline)
        self.assertEqual(
            self.apply(projection)["desired"][0]["file_id"], self.files["backup.bin"]
        )
        offline.rename(primary)
        for _ in range(3):
            self.assertEqual(
                projection.run("global")["desired"][0]["file_id"],
                self.files["backup.bin"],
            )
        self.assertEqual(self.store.rows("SELECT * FROM fallback_health"), [])
        self.apply(projection)
        health = self.store.rows("SELECT * FROM fallback_health")[0]
        self.assertEqual(health["successes"], 1)
        with self.store.detached():
            with Store(self.database, writable=True) as restarted:
                self.assertEqual(
                    restarted.rows("SELECT * FROM fallback_health")[0], health
                )
        primary.rename(offline)
        self.apply(projection)
        self.assertEqual(self.store.rows("SELECT * FROM fallback_health"), [])
        offline.rename(primary)
        self.apply(projection)
        with self.store.transaction() as db:
            db.execute("UPDATE fallback_health SET healthy_since=healthy_since-301")
        result = self.apply(projection)
        self.assertEqual(result["desired"][0]["file_id"], self.files["main.bin"])
        before = (
            next(p for p in self.output.rglob("*") if p.is_symlink()).lstat().st_ino
        )
        self.apply(projection)
        self.assertEqual(
            next(p for p in self.output.rglob("*") if p.is_symlink()).lstat().st_ino,
            before,
        )

    def test_manual_does_not_delay_escape_from_failed_fallback(self):
        projection, policy = self.bind("manual")
        primary = self.source / "main.bin"
        offline = self.root / "offline.bin"
        primary.rename(offline)
        self.apply(projection)
        offline.rename(primary)
        self.assertEqual(
            self.apply(projection)["desired"][0]["file_id"], self.files["backup.bin"]
        )
        (self.source / "backup.bin").unlink()
        self.assertEqual(
            self.apply(projection)["desired"][0]["file_id"], self.files["main.bin"]
        )

    def test_recovery_finishes_owned_retarget_before_published_generation(self):
        projection, _ = self.bind("immediate")
        self.apply(projection)
        (self.source / "main.bin").unlink()
        original = Reconciler._execute

        def crash(reconciler, operation, **kwargs):
            def interrupted(*args):
                raise RuntimeError("injected publication crash")

            return original(reconciler, operation, after_filesystem=interrupted)

        with patch.object(Reconciler, "_execute", crash):
            with self.assertRaisesRegex(RuntimeError, "injected"):
                self.apply(projection)
        row = self.store.rows("SELECT * FROM fallback_entries")[0]
        self.assertLess(row["published_generation"], row["generation"])
        self.assertTrue(self.store.rows("SELECT * FROM journal"))
        Reconciler(self.app).recover()
        self.assertFalse(self.store.rows("SELECT * FROM journal"))
        row = self.store.rows("SELECT * FROM fallback_entries")[0]
        self.assertEqual(row["published_generation"], row["generation"])
        self.assertTrue(
            next(p for p in self.output.rglob("*") if p.is_symlink()).exists()
        )

    def test_stale_manual_plan_and_foreign_destination_are_rejected(self):
        projection, _ = self.bind("immediate")
        preview = projection.run("global")
        (self.source / "main.bin").unlink()
        with self.assertRaisesRegex(CatabolicError, "stale_or_missing"):
            projection.run("global", apply=True, expected_plan=preview["plan_id"])
        self.apply(projection)
        link = next(p for p in self.output.rglob("*") if p.is_symlink())
        link.unlink()
        link.write_text("external owner")
        self.assertFalse(self.apply(projection)["complete"])
        self.assertEqual(link.read_text(), "external owner")

    def test_incomplete_budget_blocks_and_no_live_work_under_writer(self):
        policy = Policies(self.store).get(self.policy())["definition"]
        policy["budgets"]["max_checks"] = 1
        identifier = Policies(self.store).put("limited", policy)["id"]
        (self.source / "main.bin").unlink()
        decision = Resolver(self.app).resolve([{"item_id": self.item}], identifier)[
            "decisions"
        ][0]
        self.assertEqual(decision["state"], "blocked")
        self.assertEqual(decision["reasons"], ["resolution_probe_budget"])


if __name__ == "__main__":
    unittest.main()
