# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native full desired mappings, reviewed digests, and immutable ownership history."""

import copy
import json
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.layouts import PRESETS, Layouts
from catabolic.store import Store
from tests.test_media_model import populate_media
from tests.test_rust_migration import BINARY


@unittest.skipUnless(BINARY.is_file(), "build catabolic-cli before qualification")
class NativeLayoutMutationTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.path = self.root / "native.sqlite3"
        self.oracle = self.root / "oracle.sqlite3"
        Store.initialize(self.path)
        with Store(self.path, writable=True) as store:
            app = Application(store)
            self.files, _ = populate_media(app, self.root)
            (self.root / "query").mkdir()
            app.bind("output", "query", str(self.root / "query"))
            definition = copy.deepcopy(PRESETS["flat"])
            definition["selection"] = {
                "language": "sql",
                "query": "SELECT item_id FROM catalog_items WHERE kind='track'",
            }
            Layouts(app).put("query", definition)

    def native(self, action, args=None, *, ok=True):
        result = subprocess.run(
            [
                str(BINARY),
                "--db",
                str(self.path),
                "--json",
                "layout",
                action,
                json.dumps({"id": "query", "catalog": "query", **(args or {})}),
            ],
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(result.returncode, 0 if ok else 2, result.stderr)
        return (
            json.loads(result.stdout)
            if ok
            else result.stderr.strip().removeprefix("catabolic: ")
        )

    def normalized(self, value):
        if isinstance(value, list):
            return [self.normalized(v) for v in value]
        if isinstance(value, dict):
            return {
                k: self.normalized(v) for k, v in value.items() if k != "evaluated_at"
            }
        return value

    def state(self, path):
        with Store(path) as store:
            return self.normalized(
                {
                    "mappings": store.rows("SELECT * FROM mappings ORDER BY id"),
                    "meta": [
                        {"key": r["key"], "value": json.loads(r["value"])}
                        for r in store.rows(
                            "SELECT * FROM meta WHERE key LIKE 'layout-catalog:%' OR key LIKE 'statistics:%' ORDER BY key"
                        )
                    ],
                }
            )

    def differential(self, action, args=None, *, ok=True):
        args = args or {}
        shutil.copyfile(self.path, self.oracle)
        actual = self.native(action, args, ok=ok)
        with Store(self.oracle, writable=True) as store:
            layouts = Layouts(Application(store))
            if ok:
                expected = layouts.run(
                    "query", "query", apply=action == "apply", **args
                )
                self.assertEqual(actual, expected)
            else:
                with self.assertRaises(CatabolicError) as caught:
                    layouts.run("query", "query", apply=action == "apply", **args)
                self.assertEqual(actual, str(caught.exception))
        self.assertEqual(self.state(self.path), self.state(self.oracle))
        return actual

    def test_complete_mapping_apply_with_limited_details_and_idempotence(self):
        before = self.path.read_bytes()
        self.differential("preview", {"limit": 1})
        self.assertEqual(before, self.path.read_bytes())
        report = self.differential("apply", {"limit": 1})
        self.assertTrue(report["applied"])
        self.assertEqual(report["desired_count"], 2)
        self.assertEqual(len(report["changes"]), 1)
        self.assertTrue(report["details_truncated"])
        self.assertEqual(list((self.root / "query").iterdir()), [])
        self.differential("apply")

    def test_explicit_mappings_and_disabled_owned_history_are_preserved(self):
        with Store(self.path, writable=True) as store:
            Application(store).put_mapping(
                "query", self.files["book.epub"], "edition", "Manual/Book.epub"
            )
        self.differential("apply")
        with Store(self.path, writable=True) as store:
            definition = Layouts(Application(store)).get("query")["definition"]
            definition["selection"]["query"] = (
                "SELECT item_id FROM catalog_items WHERE item_id='track1'"
            )
            Layouts(Application(store)).put("query", definition)
        result = self.differential("apply")
        self.assertEqual(result["change_count"], 1)
        self.assertEqual(result["changes"][0]["action"], "disable")
        with Store(self.path) as store:
            rows = store.rows("SELECT * FROM mappings WHERE catalog='query'")
            self.assertEqual(len(rows), 3)
            self.assertEqual(sum(r["active"] for r in rows), 2)
            owner = json.loads(
                store.rows("SELECT value FROM meta WHERE key='layout-catalog:query'")[
                    0
                ]["value"]
            )
            self.assertEqual(len(owner["mapping_ids"]), 2)

    def test_empty_selection_guard_and_explicit_empty_application(self):
        self.differential("apply")
        with Store(self.path, writable=True) as store:
            definition = Layouts(Application(store)).get("query")["definition"]
            definition["selection"]["query"] = (
                "SELECT item_id FROM catalog_items WHERE 0"
            )
            Layouts(Application(store)).put("query", definition)
        before = self.state(self.path)
        result = self.differential("apply")
        self.assertFalse(result["safe"])
        self.assertEqual(self.state(self.path), before)
        self.assertTrue(self.differential("apply", {"allow_empty": True})["applied"])

    def test_stale_review_rejected_before_mutations(self):
        reviewed = self.native("prepare")
        with Store(self.path, writable=True) as store:
            app = Application(store)
            app.put_item("track", {}, {"title": "Changed evidence"}, "track1")
        before = self.state(self.path)
        message = self.native(
            "apply", {"expected_plan": reviewed["plan_digest"]}, ok=False
        )
        self.assertIn("stale layout plan", message)
        self.assertEqual(before, self.state(self.path))
        current = self.native("prepare")
        self.assertNotEqual(current["plan_digest"], reviewed["plan_digest"])
        self.assertTrue(
            self.native("apply", {"expected_plan": current["plan_digest"]})["applied"]
        )

    def test_explicit_destination_collision_blocks_without_owner_changes(self):
        with Store(self.path, writable=True) as store:
            Application(store).put_mapping(
                "query", self.files["book.epub"], "edition", "track/track1/track01.flac"
            )
        before = self.state(self.path)
        self.assertFalse(self.differential("apply")["safe"])
        self.assertEqual(self.state(self.path), before)

    def test_incomplete_query_never_applies_partial_mapping_set(self):
        with Store(self.path, writable=True) as store:
            definition = Layouts(Application(store)).get("query")["definition"]
            definition["selection"]["max_ids"] = 1
            Layouts(Application(store)).put("query", definition)
        before = self.state(self.path)
        self.assertRegex(self.native("apply", ok=False), "max_ids|truncated")
        self.assertEqual(self.state(self.path), before)
