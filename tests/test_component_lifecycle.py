# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import copy
import unittest

from catabolic import components, plans
from catabolic.curation import occurrence
from catabolic.domain import CatabolicError
from catabolic.fallback_projection import FallbackProjection
from catabolic.layouts import PRESETS, Layouts
from catabolic.watchers import Watchers
from tests import test_component_packages as fixtures


class LifecycleTest(unittest.TestCase):
    setUp = fixtures.PackageTest.setUp
    file = fixtures.PackageTest.file
    probe = fixtures.PackageTest.probe
    rows = fixtures.PackageTest.rows
    claim = fixtures.PackageTest.claim
    query = fixtures.PackageTest.query
    policy = fixtures.PackageTest.policy
    video = fixtures.PackageTest.video
    resolve = fixtures.PackageTest.resolve

    def bind(self, policy):
        self.output = self.root / "output"
        self.output.mkdir()
        self.app.bind("output", "global", str(self.output))
        members = self.query("members", "SELECT id AS item_id FROM items")
        Layouts(self.app).put("flat", copy.deepcopy(PRESETS["flat"]))
        projection = FallbackProjection(self.app)
        projection.bind("global", policy, members, "flat")
        return projection

    def apply(self, projection):
        preview = projection.run("global")
        return projection.run(
            "global", apply=True, expected_plan=preview["plan_id"], max_removals=20
        )

    def test_component_change_invalidates_watcher_without_item_membership_change(self):
        self.video("a.mkv", True)
        policy = self.policy()
        members = self.query("members", "SELECT id AS item_id FROM items")
        watchers = Watchers(self.app)
        watchers.put(
            "components",
            {
                "plan": {
                    "kind": "fallback",
                    "policy_id": policy,
                    "membership_query_id": members,
                }
            },
        )
        first = watchers.run("components")
        self.assertTrue(first["complete"], first)
        old = watchers.get("components")["baseline"]
        row = self.rows("kind='subtitle' AND current=1")[0]
        self.claim(row, attributes={"title": "Accepted translation name"})
        watchers.admit("components")
        result = watchers.run("components")
        self.assertTrue(result["complete"], result)
        new = watchers.get("components")["baseline"]
        self.assertNotEqual(old, new)
        watchers.admit("components")
        self.assertTrue(watchers.run("components")["complete"])
        self.assertEqual(new, watchers.get("components")["baseline"])

    def test_sidecar_group_and_noop_publication(self):
        video = self.video("a.mkv")
        self.file("english.srt", "subtitle", {"language": "en", "forced": True})
        row = self.rows("kind='subtitle'")[0]
        rev = components.revision(occurrence(self.store, "default", video))
        self.claim(
            row,
            compatibility={
                "edition_id": self.item,
                "video_file_id": video,
                "video_revision": rev,
                "part": None,
                "timeline_id": "container:" + rev,
                "coverage": "full",
                "offset_seconds": 0.0,
                "synchronization": "declared",
            },
        )
        projection = self.bind(self.policy("sidecars"))
        first = self.apply(projection)
        self.assertTrue(first["complete"], first)
        self.assertEqual(len(first["desired"]), 2)
        links = {
            str(p): p.lstat().st_ino for p in self.output.rglob("*") if p.is_symlink()
        }
        self.assertEqual(len(links), 2)
        history = len(self.store.rows("SELECT * FROM fallback_history"))
        second = self.apply(projection)
        self.assertTrue(second["complete"], second)
        self.assertEqual(second["publication"]["applied"], [])
        self.assertEqual(
            links,
            {
                str(p): p.lstat().st_ino
                for p in self.output.rglob("*")
                if p.is_symlink()
            },
        )
        self.assertEqual(
            len(self.store.rows("SELECT * FROM fallback_history")), history
        )
        # A subtitle disappearing retains the last complete group.
        (self.source / "english.srt").unlink()
        self.app.scan()
        blocked = self.apply(projection)
        self.assertTrue(blocked["membership_retained"])
        self.assertEqual(
            links,
            {
                str(p): p.lstat().st_ino
                for p in self.output.rglob("*")
                if p.is_symlink()
            },
        )

    def test_revision_pinned_paired_subtitle_dependencies(self):
        video = self.video("a.mkv")
        self.file("english.idx", "subtitle", {"language": "en", "forced": True})
        dep = self.file("english.sub", "custom:support")
        row = self.rows("kind='subtitle'")[0]
        rev = components.revision(occurrence(self.store, "default", video))
        self.claim(
            row,
            compatibility={
                "edition_id": self.item,
                "video_file_id": video,
                "video_revision": rev,
                "part": None,
                "timeline_id": "container:" + rev,
                "coverage": "full",
                "offset_seconds": 0.0,
                "synchronization": "declared",
            },
            dependencies_complete=True,
            dependencies=[
                {
                    "file_id": dep,
                    "revision": components.revision(
                        occurrence(self.store, "default", dep)
                    ),
                    "purpose": "subtitle_data",
                }
            ],
        )
        policy = self.policy("sidecars")
        projection = self.bind(policy)
        first = self.apply(projection)
        self.assertTrue(first["complete"], first)
        self.assertEqual(len(first["desired"]), 3)
        self.assertEqual(
            {p.suffix for p in self.output.rglob("*") if p.is_symlink()},
            {".mkv", ".idx", ".sub"},
        )
        self.assertEqual(self.resolve(self.policy("selected_only"))["state"], "blocked")
        (self.source / "english.sub").write_bytes(b"changed data")
        self.app.scan()
        self.assertEqual(self.resolve(policy)["state"], "unresolved")

    def test_wrong_edition_part_and_nonfinite_timing_rejected(self):
        video = self.video("a.mkv")
        self.file("english.srt", "subtitle", {"language": "en", "forced": True})
        row = self.rows("kind='subtitle'")[0]
        rev = components.revision(occurrence(self.store, "default", video))
        compat = {
            "edition_id": self.item,
            "video_file_id": video,
            "video_revision": rev,
            "part": None,
            "timeline_id": "container:" + rev,
            "coverage": "full",
            "offset_seconds": 0.0,
            "synchronization": "declared",
        }
        for patch in (
            {"edition_id": "different-cut"},
            {"part": 2},
            {"offset_seconds": float("inf")},
            {"coverage": "partial"},
        ):
            with (
                self.subTest(patch=patch),
                self.assertRaises((CatabolicError, ValueError)),
            ):
                self.claim(row, compatibility={**compat, **patch})

    def test_component_query_baseline_includes_evidence_and_composition(self):
        self.video("a.mkv", True)
        query = self.query(
            "components",
            "SELECT occurrence_id FROM catalog_component_occurrences WHERE kind='subtitle' AND current=1",
        )
        reference = {"kind": "query", "query_id": query}
        old = plans.semantic(plans.evaluate(self.app, reference))
        row = self.rows("kind='subtitle'")[0]
        self.claim(row, attributes={"title": "Translation"})
        new = plans.evaluate(self.app, reference)
        self.assertTrue(plans.compare(old, new)["changed"])
        self.assertEqual(old["value"]["ids"], new["value"]["ids"])
