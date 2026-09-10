# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import copy
import unittest
from unittest.mock import patch

from catabolic.domain import CatabolicError
from catabolic.fallback_policies import Policies
from catabolic.fallback_projection import FallbackProjection
from catabolic.fallback_resolution import Resolver
from catabolic.http_worker_local import tick
from catabolic.layouts import PRESETS, Layouts
from catabolic.reconcile import Reconciler
from catabolic.saved_queries import Queries
from tests import test_fallback_lifecycle as fixtures


class ReviewTest(unittest.TestCase):
    setUp = fixtures.LifecycleTest.setUp
    policy = fixtures.LifecycleTest.policy
    apply = fixtures.LifecycleTest.apply

    def interrupted(self):
        query = Queries(self.store).put(
            "filtered",
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT id AS item_id FROM items WHERE json_extract(metadata,'$.title')='Film'",
                }
            },
        )["id"]
        Layouts(self.app).put("flat", copy.deepcopy(PRESETS["flat"]))
        projection = FallbackProjection(self.app)
        projection.bind("global", self.policy(), query, "flat")
        self.apply(projection)
        (self.source / "main.bin").unlink()
        with patch.object(
            Reconciler, "_execute", side_effect=RuntimeError("interrupted")
        ):
            with self.assertRaises(RuntimeError):
                self.apply(projection)
        self.app.put_item(
            "movie", {}, {"title": "No longer selected"}, item_id=self.item
        )
        return projection

    def test_obsolete_unapplied_retarget_can_recover_and_replan(self):
        projection = self.interrupted()
        link = next(p for p in self.output.rglob("*") if p.is_symlink())
        target = link.readlink()
        result = Reconciler(self.app).recover()
        self.assertEqual(len(result["cancelled"]), 1)
        self.assertFalse(self.store.rows("SELECT * FROM journal"))
        self.assertEqual(link.readlink(), target)
        preview = projection.run("global")
        self.assertEqual(preview["desired"], [])
        self.assertEqual(len(preview["removed"]), 1)
        # Cancelling intent must not authorize removal of the retained owned link.
        with self.assertRaisesRegex(CatabolicError, "budget"):
            projection.run("global", apply=True, expected_plan=preview["plan_id"])
        self.assertTrue(link.is_symlink())

    def test_stale_recovery_preserves_foreign_temporary_link(self):
        self.interrupted()
        operation = self.store.rows("SELECT * FROM journal")[0]
        temporary = (self.output / operation["path"]).parent / (
            ".catabolic-link-" + operation["id"]
        )
        temporary.symlink_to("foreign-target")
        with self.assertRaisesRegex(CatabolicError, "changed externally"):
            Reconciler(self.app).recover(cancel_unapplied=True)
        self.assertEqual(str(temporary.readlink()), "foreign-target")
        self.assertTrue(self.store.rows("SELECT * FROM journal"))

    def test_incomplete_query_cannot_authorize_cancellation(self):
        self.interrupted()
        with patch.object(
            Queries, "select", side_effect=CatabolicError("incomplete selection")
        ):
            with self.assertRaisesRegex(CatabolicError, "incomplete selection"):
                Reconciler(self.app).recover(cancel_unapplied=True)
        self.assertTrue(self.store.rows("SELECT * FROM journal"))

    def test_stale_recovery_requires_recorded_ownership(self):
        self.interrupted()
        with self.store.transaction() as db:
            db.execute("UPDATE owned_links SET target='foreign-record'")
        with self.assertRaisesRegex(CatabolicError, "ownership changed"):
            Reconciler(self.app).recover()
        self.assertTrue(self.store.rows("SELECT * FROM journal"))

    def test_idle_heartbeat_does_not_invalidate_but_catalog_change_does(self):
        from catabolic.fallback_probe import probe
        from catabolic.store import Store

        with self.store.detached():
            tick(self.database, "default", {})
        policy = self.policy()

        def heartbeat(database, snapshots, timeout):
            tick(database, "default", {})
            return probe(database, snapshots, timeout)

        with patch("catabolic.fallback_resolution.probe", side_effect=heartbeat):
            result = Resolver(self.app).resolve([{"item_id": self.item}], policy)
        self.assertEqual(result["decisions"][0]["file_id"], self.files["main.bin"])

        def mutation(database, snapshots, timeout):
            with Store(database, writable=True) as writer:
                with writer.transaction() as db:
                    db.execute(
                        "UPDATE items SET metadata='{}' WHERE id=?", (self.item,)
                    )
            return probe(database, snapshots, timeout)

        with patch("catabolic.fallback_resolution.probe", side_effect=mutation):
            with self.assertRaisesRegex(CatabolicError, "stale_resolution_plan"):
                Resolver(self.app).resolve([{"item_id": self.item}], policy)

    def test_filesystem_repairs_obey_change_limit(self):
        other = self.app.put_item("movie", {"fixture": "other"}, {"title": "Other"})[
            "id"
        ]
        self.app.media.associate(self.files["backup.bin"], other)
        with self.store.transaction() as db:
            db.execute(
                "UPDATE item_files SET active=0 WHERE item_id=? AND file_id=?",
                (self.item, self.files["backup.bin"]),
            )
        query = Queries(self.store).put(
            "all-files",
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT id AS file_id FROM files",
                }
            },
        )["id"]
        members = Queries(self.store).put(
            "members",
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT id AS item_id FROM items",
                }
            },
        )["id"]
        policy = Policies(self.store).put(
            "all", {"fallbacks": [{"name": "all", "query_id": query}]}
        )["id"]
        Layouts(self.app).put("flat", copy.deepcopy(PRESETS["flat"]))
        projection = FallbackProjection(self.app)
        projection.bind("global", policy, members, "flat")
        self.apply(projection)
        links = [p for p in self.output.rglob("*") if p.is_symlink()]
        self.assertEqual(len(links), 2)
        for link in links:
            link.unlink()
        preview = projection.run("global")
        with self.assertRaisesRegex(CatabolicError, "budget"):
            projection.run(
                "global", apply=True, expected_plan=preview["plan_id"], max_changes=1
            )
        self.assertTrue(all(not link.is_symlink() for link in links))
        self.assertFalse(self.store.rows("SELECT * FROM journal"))
        preview = projection.run("global")
        result = projection.run(
            "global", apply=True, expected_plan=preview["plan_id"], max_changes=2
        )
        self.assertTrue(result["complete"])
        self.assertEqual(len(result["publication"]["applied"]), 2)
