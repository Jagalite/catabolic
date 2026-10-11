# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Exact resolver plans against the frozen oracle, using native bounded helpers."""

import json
import subprocess
import unittest

from catabolic.app import Application
from catabolic.fallback_policies import Policies
from catabolic.fallback_resolution import Resolver
from catabolic.saved_queries import Queries
from catabolic.store import Store
from tests import test_fallback_baseline as fixtures
from tests import test_rust_component_selection as components_fixture
from tests.test_rust_migration import BINARY


class NativeFallbackResolutionTest(unittest.TestCase):
    setUp = fixtures.FallbackBaselineTest.setUp

    def policy(self, *, all_files=False, tie="file_id", prefer=None, budget=None):
        tiers = []
        for name in ["all"] if all_files else ["main.bin", "backup.bin"]:
            query = Queries(self.store).put(
                name,
                {
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS file_id FROM files"
                        if all_files
                        else "SELECT id AS file_id FROM files WHERE path=:path",
                        "params": {} if all_files else {"path": name},
                    }
                },
            )
            tiers.append({"name": name, "query_id": query["id"]})
        return Policies(self.store).put(
            "test-policy",
            {
                "fallbacks": tiers,
                "within_tier": {"tie_break": tie, "prefer": prefer or []},
                "failback": {"mode": "immediate"},
                "budgets": {"probe_timeout_ms": 5000, **(budget or {})},
            },
        )["id"]

    def pair(self, policy, entries=None):
        entries = entries or [{"item_id": self.item}]
        expected = json.loads(json.dumps(Resolver(self.app).resolve(entries, policy)))
        # Release the oracle's writer before the native owner starts.
        path = self.store.path
        self.store.close()
        try:
            result = subprocess.run(
                [
                    str(BINARY),
                    "--db",
                    str(path),
                    "--json",
                    "operation",
                    "fallback",
                    "resolve",
                    "--options",
                    json.dumps({"policy_id": policy, "entries": entries}),
                ],
                capture_output=True,
                text=True,
                timeout=60,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            actual = json.loads(result.stdout)
        finally:
            self.store = Store(path, writable=True)
            self.addCleanup(self.store.close)
            self.app = Application(self.store)
        self.assertEqual(actual, expected)
        self.assertEqual(actual["plan_id"], expected["plan_id"])
        self.assertEqual(self.store.rows("SELECT * FROM processing_jobs"), [])
        self.assertTrue(
            all(
                r["pid"] is None and r["owner"] is None
                for r in self.store.rows("SELECT * FROM fallback_probe_slots")
            )
        )
        return actual

    def test_policy_session_keeps_budget_across_individually_bounded_tiers(self):
        tiers = []
        for name in ("main.bin", "backup.bin"):
            query = Queries(self.store).put(
                name,
                {
                    "max_ids": 1,
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS file_id FROM files WHERE path=:path",
                        "params": {"path": name},
                    },
                },
            )["id"]
            tiers.append({"name": name, "query_id": query})
        policy = Policies(self.store).put(
            "bounded-tiers",
            {"fallbacks": tiers, "budgets": {"max_candidates": 2}},
        )["id"]
        (self.source / "main.bin").unlink()
        result = self.pair(policy)
        self.assertEqual(result["decisions"][0]["file_id"], self.files["backup.bin"])

    def test_large_candidate_evidence_is_charged_once(self):
        self.app.media.associate(
            self.files["main.bin"], self.item, metadata={"description": "x" * 2_200_000}
        )
        result = self.pair(self.policy())
        self.assertEqual(result["decisions"][0]["file_id"], self.files["main.bin"])

    def test_ordered_live_failover_and_unresolved_membership(self):
        policy = self.policy()
        first = self.pair(policy)
        self.assertEqual(first["decisions"][0]["file_id"], self.files["main.bin"])
        (self.source / "main.bin").unlink()
        second = self.pair(policy)
        self.assertEqual(second["decisions"][0]["file_id"], self.files["backup.bin"])
        (self.source / "backup.bin").unlink()
        self.assertEqual(self.pair(policy)["decisions"][0]["state"], "unresolved")

    def test_ambiguous_tier_and_rank_plan_identity(self):
        self.assertEqual(
            self.pair(self.policy(all_files=True, tie=None))["decisions"][0]["reasons"],
            ["ambiguous_tier"],
        )
        for prefer in (
            [{"field": "path", "values": ["backup.bin", "main.bin"]}],
            [{"field": "size", "order": "desc"}],
            [{"field": "height", "order": "asc"}],
        ):
            self.pair(self.policy(all_files=True, prefer=prefer))

    def test_multipart_requires_coherent_explicit_group(self):
        for part, name in enumerate(("main.bin", "backup.bin"), 1):
            self.app.media.associate(self.files[name], self.item, part=part)
        entries = [{"item_id": self.item, "part": part} for part in (1, 2)]
        policy = self.policy(all_files=True)
        self.assertTrue(
            all(
                d["state"] == "blocked" for d in self.pair(policy, entries)["decisions"]
            )
        )
        for part, name in enumerate(("main.bin", "backup.bin"), 1):
            self.app.media.associate(
                self.files[name],
                self.item,
                part=part,
                metadata={"representation_group": "edition"},
            )
        self.assertTrue(
            all(
                d["state"].startswith("resolved")
                for d in self.pair(policy, entries)["decisions"]
            )
        )

    def test_probe_budget_fails_closed(self):
        policy = self.policy(budget={"max_checks": 1})
        (self.source / "main.bin").unlink()
        self.assertEqual(
            self.pair(policy)["decisions"][0]["reasons"], ["resolution_probe_budget"]
        )


class NativeComponentFailureDispositionTest(unittest.TestCase):
    setUp = components_fixture.NativeComponentSelectionTest.setUp
    file = components_fixture.NativeComponentSelectionTest.file
    probe = components_fixture.NativeComponentSelectionTest.probe
    rows = components_fixture.NativeComponentSelectionTest.rows
    claim = components_fixture.NativeComponentSelectionTest.claim
    query = components_fixture.NativeComponentSelectionTest.query
    policy = components_fixture.NativeComponentSelectionTest.policy
    video = components_fixture.NativeComponentSelectionTest.video
    resolve = components_fixture.NativeComponentSelectionTest.resolve

    def test_conflicting_preferred_video_blocks_without_silent_failover(self):
        first = self.video("a.mkv", True)
        self.video("b.mkv", True)
        streams = [
            json.loads(row["technical"])
            for row in self.rows("file_id=? AND current=1", (first,))
        ]
        for stream in streams:
            if stream["codec_type"] == "video":
                stream["tags"] = {"title": "observed"}
        self.probe(first, streams)
        video = self.rows("file_id=? AND kind='video' AND current=1", (first,))[0]
        self.claim(video, attributes={"title": "asserted"})
        decision = self.resolve(self.policy())
        self.assertEqual(decision["state"], "blocked")
        self.assertIsNone(decision["file_id"])
        self.assertEqual(
            decision["reasons"], ["stale_or_conflicting_component_occurrence"]
        )
