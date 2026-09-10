# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import unittest
from unittest.mock import patch

from catabolic.app import Application
from catabolic.fallback_policies import Policies
from catabolic.fallback_resolution import Resolver
from catabolic.program_bundle import Programs
from catabolic.saved_queries import Queries
from catabolic.store import Store
from tests import test_fallback_resolution as fixtures


class IdentityTest(unittest.TestCase):
    setUp = fixtures.ResolverTest.setUp
    policy = fixtures.ResolverTest.policy

    def all_policy(self):
        query = Queries(self.store).put(
            "all",
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT id AS file_id FROM files",
                }
            },
        )["id"]
        return Policies(self.store).put(
            "all",
            {
                "fallbacks": [{"name": "all", "query_id": query}],
                "within_tier": {"tie_break": "file_id"},
            },
        )["id"]

    def test_sidecar_compatibility_and_edition_are_explicit(self):
        self.app.media.associate(
            self.files["backup.bin"],
            self.item,
            role="subtitle",
            metadata={"for_file_id": self.files["main.bin"]},
        )
        policy = self.all_policy()
        resolver = Resolver(self.app)
        entries = [{"item_id": self.item, "role": "subtitle"}]
        self.assertIsNone(resolver.resolve(entries, policy)["decisions"][0]["file_id"])
        resolver.primary_choices[self.item] = self.files["main.bin"]
        self.assertEqual(
            resolver.resolve(entries, policy)["decisions"][0]["file_id"],
            self.files["backup.bin"],
        )
        self.assertIsNone(
            resolver.resolve(
                [{"item_id": self.item, "variant": "directors-cut"}], policy
            )["decisions"][0]["file_id"]
        )

    def test_multipart_requires_coherent_explicit_group(self):
        for part, path in enumerate(("main.bin", "backup.bin"), 1):
            self.app.media.associate(self.files[path], self.item, part=part)
        policy = self.all_policy()
        entries = [{"item_id": self.item, "part": part} for part in (1, 2)]
        result = Resolver(self.app).resolve(entries, policy)
        self.assertTrue(all(d["state"] == "blocked" for d in result["decisions"]))
        for part, path in enumerate(("main.bin", "backup.bin"), 1):
            self.app.media.associate(
                self.files[path],
                self.item,
                part=part,
                metadata={"representation_group": "edition-1"},
            )
        result = Resolver(self.app).resolve(entries, policy)
        self.assertTrue(
            all(d["state"].startswith("resolved") for d in result["decisions"])
        )

    def test_queries_execute_once_for_many_entries(self):
        items = [self.item]
        for i in range(20):
            item = self.app.put_item("movie", {"fixture": str(i)}, {"title": str(i)})[
                "id"
            ]
            self.app.media.associate(self.files["main.bin"], item)
            items.append(item)
        policy = self.policy()
        original = Queries._select
        calls = []

        def count(queries, *args):
            calls.append(args[0])
            return original(queries, *args)

        with patch.object(Queries, "_select", count):
            result = Resolver(self.app).resolve(
                [{"item_id": item} for item in items], policy
            )
        self.assertEqual(len(calls), 1)
        self.assertEqual(result["checks"], 1)
        self.assertEqual(len(result["decisions"]), 21)

    def test_portable_import_keeps_profile_policy_unbound(self):
        policy = self.policy()
        bundle = Programs(self.app).export(fallback_policies=[policy])
        self.assertEqual(bundle["version"], 2)
        self.assertEqual(len(bundle["queries"]), 2)
        database = self.root / "import.db"
        Store.initialize(database)
        with Store(database, writable=True) as store:
            app = Application(store)
            result = Programs(app).import_bundle(bundle)
            self.assertTrue(result["complete"], result)
            self.assertEqual(len(store.rows("SELECT * FROM fallback_policies")), 1)
            self.assertFalse(store.rows("SELECT * FROM fallback_bindings"))
            self.assertFalse(store.rows("SELECT * FROM fallback_health"))
            self.assertFalse(store.rows("SELECT * FROM api_tokens"))

    def test_probe_capacity_is_bounded_without_spawning(self):
        import os

        from catabolic.fallback_probe import probe

        with self.store.transaction() as db:
            db.execute("UPDATE fallback_probe_slots SET pid=?", (os.getpid(),))
        with (
            self.store.detached(),
            patch("catabolic.fallback_probe.subprocess.Popen") as child,
        ):
            self.assertEqual(probe(self.database, [{}], 10)["reason"], "probe_capacity")
        child.assert_not_called()

    def test_profiles_choose_independent_winners(self):
        import copy

        from catabolic.fallback_projection import FallbackProjection
        from catabolic.layouts import PRESETS, Layouts

        self.app.add_profile("second")
        other = Application(self.store, "second")
        other.bind("source", "source", str(self.source))
        other.scan()
        output = self.root / "second-output"
        output.mkdir()
        other.bind("output", "global", str(output))
        for app, path in ((self.app, "main.bin"), (other, "backup.bin")):
            queries = Queries(self.store, app.profile)
            query = queries.put(
                "choice",
                {
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS file_id FROM files WHERE path=:path",
                        "params": {"path": path},
                    }
                },
            )["id"]
            members = queries.put(
                "members",
                {
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS item_id FROM items",
                    }
                },
            )["id"]
            policy = Policies(self.store, app.profile).put(
                "choice", {"fallbacks": [{"name": "choice", "query_id": query}]}
            )["id"]
            Layouts(app).put("flat", copy.deepcopy(PRESETS["flat"]))
            projection = FallbackProjection(app)
            projection.bind("global", policy, members, "flat")
            preview = projection.run("global")
            self.assertTrue(
                projection.run("global", apply=True, expected_plan=preview["plan_id"])[
                    "complete"
                ]
            )
        rows = self.store.rows(
            "SELECT profile,id,file_id FROM fallback_entries ORDER BY profile"
        )
        self.assertEqual(
            [r["file_id"] for r in rows],
            [self.files["main.bin"], self.files["backup.bin"]],
        )
        self.assertNotEqual(rows[0]["id"], rows[1]["id"])
        self.assertEqual(self.store.rows("SELECT * FROM mappings"), [])
