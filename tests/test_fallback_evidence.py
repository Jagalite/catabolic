# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import copy
import shutil
import unittest

from catabolic.fallback_policies import Policies
from catabolic.fallback_projection import FallbackProjection
from catabolic.fallback_resolution import Resolver
from catabolic.layouts import PRESETS, Layouts
from catabolic.saved_queries import Queries
from tests import test_artifacts as fixtures


@unittest.skipUnless(
    shutil.which("ffmpeg") and shutil.which("ffprobe"), "real media tools"
)
class FallbackEvidenceTest(unittest.TestCase):
    setUpClass = classmethod(fixtures.ArtifactTest.setUpClass.__func__)
    tearDownClass = classmethod(fixtures.ArtifactTest.tearDownClass.__func__)
    setUp = fixtures.ArtifactTest.setUp

    def generate_media(self):
        recipe = self.a.recipe("fallback", "h264-720p", {"max_output_bytes": 16777216})
        self.a.enqueue(self.file_id, recipe["id"], "generated", self.item_id)
        result = self.a.run()
        self.assertTrue(result["complete"], result)
        return self.store.rows("SELECT file_id FROM media_outputs")[0]["file_id"]

    def policy(self, mode):
        tiers = []
        for location in ("media", "generated"):
            query = Queries(self.store).put(
                location,
                {
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS file_id FROM files WHERE location=:location",
                        "params": {"location": location},
                    }
                },
            )
            tiers.append({"name": location, "query_id": query["id"]})
        return Policies(self.store).put(
            mode,
            {
                "fallbacks": tiers,
                "within_tier": {"tie_break": "file_id"},
                "lineage_requirement": mode,
                "failback": {"mode": "immediate"},
            },
        )["id"]

    def test_offline_original_and_container_transition(self):
        generated = self.generate_media()
        accepted = self.policy("accepted_source_revision")
        strict = self.policy("current_source_revision")
        query = Queries(self.store).put(
            "membership",
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT id AS item_id FROM items",
                }
            },
        )
        output = self.root / "projection"
        output.mkdir()
        self.app.bind("output", "global", str(output))
        Layouts(self.app).put("flat", copy.deepcopy(PRESETS["flat"]))
        projection = FallbackProjection(self.app)
        projection.bind("global", accepted, query["id"], "flat")
        preview = projection.run("global")
        result = projection.run(
            "global", apply=True, expected_plan=preview["plan_id"], max_removals=1
        )
        self.assertTrue(result["complete"], result)
        self.source.rename(self.root / "offline")
        preview = projection.run("global")
        self.assertEqual(preview["desired"][0]["file_id"], generated)
        self.assertEqual(
            preview["desired"][0]["evidence"]["source_currentness"], "not_live_checked"
        )
        result = projection.run(
            "global", apply=True, expected_plan=preview["plan_id"], max_removals=1
        )
        self.assertTrue(result["complete"], result)
        links = [p for p in output.rglob("*") if p.is_symlink()]
        self.assertEqual(len(links), 1)
        self.assertEqual(links[0].suffix, ".mp4")
        self.assertTrue(links[0].exists())
        strict_result = Resolver(self.app).resolve([{"item_id": self.item_id}], strict)
        self.assertEqual(strict_result["decisions"][0]["state"], "unresolved")

    def test_explicit_historical_processing_keeps_derived_input_pinned(self):
        from catabolic.domain import CatabolicError

        generated = self.generate_media()
        self.source.rename(self.root / "offline")
        recipe = self.a.recipe(
            "historical-thumbnail", "thumbnail", {"max_output_bytes": 16777216}
        )
        with self.assertRaises(CatabolicError):
            self.a.enqueue(generated, recipe["id"], "generated", self.item_id)
        admitted = self.a.enqueue(
            generated,
            recipe["id"],
            "generated",
            self.item_id,
            _source_lineage_mode="accepted_source_revision",
        )
        result = self.a.run()
        self.assertTrue(result["complete"], result)
        jobs = self.store.rows(
            "SELECT file_id FROM processing_jobs WHERE file_id=?", (generated,)
        )
        self.assertEqual(len(jobs), 1, admitted)
