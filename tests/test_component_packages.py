# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import unittest

from catabolic import components
from catabolic.fallback_policies import Policies
from catabolic.fallback_resolution import Resolver
from catabolic.saved_queries import Queries
from tests import test_components as fixtures


class PackageTest(unittest.TestCase):
    setUp = fixtures.ComponentsTest.setUp
    file = fixtures.ComponentsTest.file
    probe = fixtures.ComponentsTest.probe
    rows = fixtures.ComponentsTest.rows
    claim = fixtures.ComponentsTest.claim

    def query(self, name, sql):
        return Queries(self.store).put(
            name, {"selection": {"language": "sql", "query": sql}}
        )["id"]

    def policy(self, publication="container", operation=None):
        tiers = [
            {
                "name": name,
                "query_id": self.query(
                    name, "SELECT id AS file_id FROM files WHERE path='" + name + "'"
                ),
            }
            for name in ("a.mkv", "b.mkv")
        ]
        audio = self.query(
            "audio",
            "SELECT occurrence_id FROM catalog_component_occurrences WHERE profile=:profile AND kind='audio' AND language='en' AND current=1",
        )
        subtitle = self.query(
            "subs",
            "SELECT occurrence_id FROM catalog_component_occurrences WHERE profile=:profile AND kind='subtitle' AND language='en' AND forced=1 AND current=1",
        )
        return Policies(self.store).put(
            "package",
            {
                "fallbacks": tiers,
                "failback": {"mode": "immediate"},
                "package": {
                    "publication": publication,
                    "operation_id": operation,
                    "requirements": [
                        {
                            "name": "audio",
                            "fallbacks": [{"name": "english", "query_id": audio}],
                        },
                        {
                            "name": "subtitles",
                            "fallbacks": [{"name": "forced", "query_id": subtitle}],
                        },
                    ],
                },
            },
        )["id"]

    def video(self, name, subs=False):
        f = self.file(name)
        streams = [
            {"index": 0, "codec_type": "video", "codec_name": "h264"},
            {
                "index": 1,
                "codec_type": "audio",
                "codec_name": "aac",
                "tags": {"language": "eng"},
            },
        ]
        if subs:
            streams.append(
                {
                    "index": 2,
                    "codec_type": "subtitle",
                    "codec_name": "subrip",
                    "tags": {"language": "eng"},
                    "disposition": {"forced": 1},
                }
            )
        self.probe(f, streams)
        return f

    def resolve(self, policy):
        return Resolver(self.app).resolve([{"item_id": self.item}], policy)[
            "decisions"
        ][0]

    def test_required_component_disqualifies_preferred_video(self):
        self.video("a.mkv")
        b = self.video("b.mkv", True)
        d = self.resolve(self.policy())
        self.assertEqual(d["file_id"], b)
        self.assertEqual(d["state"], "resolved_fallback")
        p = d["evidence"]["component_package"]
        self.assertEqual(p["packaging"]["action"], "use_container")
        self.assertTrue(p["packaging"]["exposes_unselected_components"])
        self.assertEqual(len(p["components"]), 3)

    def test_external_needs_explicit_matching_timeline(self):
        a = self.video("a.mkv")
        self.file("sub.srt", "subtitle", {"language": "en", "forced": True})
        row = self.rows("kind='subtitle'")[0]
        policy = self.policy("sidecars")
        self.assertEqual(self.resolve(policy)["state"], "unresolved")
        rev = components.revision(
            __import__("catabolic.curation", fromlist=["occurrence"]).occurrence(
                self.store, "default", a
            )
        )
        compatibility = {
            "edition_id": self.item,
            "video_file_id": a,
            "video_revision": rev,
            "part": None,
            "timeline_id": "wrong",
            "coverage": "full",
            "offset_seconds": 0.0,
            "synchronization": "verified",
        }
        assertion = self.claim(row, compatibility=compatibility)
        self.assertEqual(self.resolve(policy)["state"], "unresolved")
        self.claim(
            row,
            compatibility={**compatibility, "timeline_id": "container:" + rev},
            supersedes=[assertion],
        )
        d = self.resolve(policy)
        self.assertEqual(d["file_id"], a)
        self.assertEqual(
            d["evidence"]["component_package"]["packaging"]["action"],
            "publish_sidecars",
        )

    def test_multiple_translations_block_without_arbitrary_file_tie_break(self):
        a = self.video("a.mkv", True)
        self.probe(
            a,
            [
                {"index": 0, "codec_type": "video"},
                {"index": 1, "codec_type": "audio", "tags": {"language": "eng"}},
                {
                    "index": 2,
                    "codec_type": "subtitle",
                    "tags": {"language": "eng"},
                    "disposition": {"forced": 1},
                },
                {
                    "index": 3,
                    "codec_type": "subtitle",
                    "tags": {"language": "eng"},
                    "disposition": {"forced": 1},
                },
            ],
        )
        d = self.resolve(self.policy())
        self.assertEqual(d["state"], "blocked")
        self.assertIn("ambiguous_component_requirement", d["reasons"][0])

    def test_selected_only_has_no_parent_container_content_url(self):
        file = self.video("a.mkv", True)
        self.probe(
            file,
            [
                {"index": 0, "codec_type": "video"},
                {"index": 1, "codec_type": "audio", "tags": {"language": "eng"}},
                {
                    "index": 2,
                    "codec_type": "subtitle",
                    "tags": {"language": "eng"},
                    "disposition": {"forced": 1},
                },
                {"index": 3, "codec_type": "audio", "tags": {"language": "fra"}},
            ],
        )
        d = self.resolve(self.policy("selected_only"))
        self.assertIsNone(d["content_path"])
        self.assertEqual(
            d["evidence"]["component_package"]["packaging"]["action"], "mux"
        )
        self.assertFalse(d["evidence"]["component_package"]["packaging"]["ready"])
        self.assertFalse(
            self.store.rows("SELECT 1 FROM processing_jobs WHERE operation='render'")
        )
