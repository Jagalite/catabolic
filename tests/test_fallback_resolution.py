# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import unittest
from unittest.mock import patch

from catabolic.fallback_policies import Policies
from catabolic.fallback_resolution import Resolver
from catabolic.saved_queries import Queries
from tests import test_fallback_baseline as fixtures


class ResolverTest(unittest.TestCase):
    setUp = fixtures.FallbackBaselineTest.setUp

    def policy(self, *, tie="file_id", queries=None):
        refs = []
        for path in queries or ["main.bin", "backup.bin"]:
            query = Queries(self.store).put(
                path,
                {
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS file_id FROM files WHERE path=:path",
                        "params": {"path": path},
                    }
                },
            )
            refs.append({"name": path, "query_id": query["id"]})
        return Policies(self.store).put(
            "fallback",
            {
                "fallbacks": refs,
                "within_tier": {"tie_break": tie},
                "failback": {"mode": "immediate"},
            },
        )["id"]

    def test_ordered_live_failover_and_unresolved_membership(self):
        policy = self.policy()
        resolver = Resolver(self.app)
        first = resolver.resolve([{"item_id": self.item}], policy)
        self.assertEqual(first["decisions"][0]["file_id"], self.files["main.bin"])
        self.assertEqual(first["tiers"][1]["state"], "not_evaluated")
        (self.source / "main.bin").unlink()
        second = resolver.resolve([{"item_id": self.item}], policy)
        self.assertEqual(second["decisions"][0]["file_id"], self.files["backup.bin"])
        self.assertEqual(
            second["decisions"][0]["entry_id"], first["decisions"][0]["entry_id"]
        )
        (self.source / "backup.bin").unlink()
        third = resolver.resolve([{"item_id": self.item}], policy)
        self.assertEqual(third["decisions"][0]["state"], "unresolved")
        self.assertEqual(third["decisions"][0]["item_id"], self.item)

    def test_policy_revisions_and_malformed_tier_are_not_empty(self):
        policy = self.policy()
        self.assertEqual(policy, self.policy())
        with patch(
            "catabolic.saved_queries.Queries._select",
            side_effect=__import__(
                "catabolic.domain", fromlist=["CatabolicError"]
            ).CatabolicError("truncated"),
        ):
            result = Resolver(self.app).resolve([{"item_id": self.item}], policy)
        self.assertEqual(result["decisions"][0]["state"], "blocked")
        self.assertEqual(result["tiers"][1]["state"], "not_evaluated")

    def test_ambiguous_tier_and_no_hashing(self):
        query = Queries(self.store).put(
            "all",
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT id AS file_id FROM files",
                }
            },
        )
        policy = Policies(self.store).put(
            "ambiguous", {"fallbacks": [{"name": "all", "query_id": query["id"]}]}
        )["id"]
        result = Resolver(self.app).resolve([{"item_id": self.item}], policy)
        self.assertEqual(result["decisions"][0]["reasons"], ["ambiguous_tier"])
        self.assertEqual(self.store.rows("SELECT * FROM processing_jobs"), [])

    def test_probe_has_no_parent_session_and_stale_epoch_fails(self):
        from catabolic.domain import CatabolicError
        from catabolic.store import Store

        policy = self.policy()

        def mutate(database, snapshots, timeout):
            self.assertIsNone(self.store.db)
            with Store(database, writable=True) as writer:
                with writer.transaction() as db:
                    db.execute(
                        "UPDATE items SET metadata='{}' WHERE id=?", (self.item,)
                    )
            return {"usable": True, "reason": None}

        with patch("catabolic.fallback_resolution.probe", side_effect=mutate):
            with self.assertRaisesRegex(CatabolicError, "stale_resolution_plan"):
                Resolver(self.app).resolve([{"item_id": self.item}], policy)

    def test_projection_retargets_and_retains_all_offline(self):
        import copy
        import os

        from catabolic.fallback_projection import FallbackProjection
        from catabolic.layouts import PRESETS, Layouts

        policy = self.policy()
        query = Queries(self.store).put(
            "members",
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT id AS item_id FROM items",
                }
            },
        )
        Layouts(self.app).put("flat", copy.deepcopy(PRESETS["flat"]))
        projection = FallbackProjection(self.app)
        projection.bind("global", policy, query["id"], "flat")
        plan = projection.run("global")
        result = projection.run("global", apply=True, expected_plan=plan["plan_id"])
        self.assertTrue(result["complete"], result)
        link = next(p for p in self.output.rglob("*") if p.is_symlink())
        first = os.readlink(link)
        (self.source / "main.bin").unlink()
        plan = projection.run("global")
        result = projection.run("global", apply=True, expected_plan=plan["plan_id"])
        self.assertTrue(result["complete"], result)
        second = os.readlink(link)
        self.assertNotEqual(first, second)
        (self.source / "backup.bin").unlink()
        plan = projection.run("global")
        result = projection.run("global", apply=True, expected_plan=plan["plan_id"])
        self.assertFalse(result["applied"])
        self.assertEqual(os.readlink(link), second)
        self.assertEqual(
            len(self.store.rows("SELECT * FROM fallback_entries WHERE active=1")), 1
        )
