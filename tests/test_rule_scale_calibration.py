# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import shutil
import unittest

from catabolic.domain import CatabolicError
from catabolic.estimate_calibration import calibrated, calibration
from catabolic.rule_estimates import estimate
from catabolic.rules import Rules
from catabolic.selection import select_ids
from tests import test_outputs, test_rules


class PagedSelectionTest(unittest.TestCase):
    setUp = test_outputs.OutputTest.setUp

    def test_more_than_ten_thousand_ids_are_complete_and_order_independent(self):
        with self.store.transaction() as db:
            db.executemany(
                "INSERT INTO items VALUES (?,'movie','{}')",
                [(f"scale-{n:05d}",) for n in range(12003)],
            )
        selection = {
            "language": "sql",
            "query": "SELECT item_id FROM catalog_items WHERE item_id LIKE 'scale-%' ORDER BY item_id DESC;",
            "page_size": 999,
            "max_ids": 20000,
            "timeout_ms": 60000,
        }
        entity, ids, report = select_ids(self.store, selection)
        self.assertEqual(entity, "item_id")
        self.assertEqual(ids, {f"scale-{n:05d}" for n in range(12003)})
        self.assertEqual(report["pages"], 13)
        self.assertTrue(report["complete"])
        with self.assertRaisesRegex(CatabolicError, "max_ids"):
            select_ids(self.store, {**selection, "max_ids": 12000})
        with self.assertRaisesRegex(CatabolicError, "truncated"):
            select_ids(self.store, {"language": "sql", "query": selection["query"]})

    def test_paged_selection_rejects_volatile_filters(self):
        with self.assertRaisesRegex(CatabolicError, "random"):
            select_ids(
                self.store,
                {
                    "language": "sql",
                    "query": "SELECT item_id FROM catalog_items WHERE random()>0",
                    "page_size": 1,
                },
            )

    def test_pagination_deduplicates_and_rejects_invalid_ids(self):
        query = "SELECT item_id FROM catalog_items UNION ALL SELECT item_id FROM catalog_items"
        _, ids, report = select_ids(
            self.store, {"language": "sql", "query": query, "page_size": 1}
        )
        self.assertEqual(ids, {self.item})
        self.assertEqual(report["returned_rows"], 1)
        with self.assertRaisesRegex(CatabolicError, "nonempty"):
            select_ids(
                self.store,
                {"language": "sql", "query": "SELECT NULL AS item_id", "page_size": 1},
            )
        with self.assertRaisesRegex(CatabolicError, "reserved"):
            select_ids(
                self.store,
                {
                    "language": "sql",
                    "query": "SELECT :__catabolic_after AS item_id",
                    "page_size": 1,
                },
            )


@unittest.skipUnless(
    shutil.which("ffmpeg") and shutil.which("ffprobe"), "real media tools required"
)
class CalibrationTest(unittest.TestCase):
    setUpClass = classmethod(test_rules.RuleTest.setUpClass.__func__)
    tearDownClass = classmethod(test_rules.RuleTest.tearDownClass.__func__)
    setUp = test_rules.RuleTest.setUp
    probe = test_rules.RuleTest.probe
    rule = test_rules.RuleTest.rule

    def test_actual_renders_calibrate_expectation_without_lowering_budget(self):
        for n in range(2):
            (self.source_file.parent / f"sample-{n}.mkv").write_bytes(self.payload)
        self.app.scan()
        for row in self.store.rows("SELECT id FROM files WHERE path LIKE 'sample-%'"):
            self.app.media.associate(row["id"], self.item_id)
        self.probe()
        rule = self.rule()
        before = self.rules.preview(rule["id"])
        self.assertFalse(before["calibration"]["applied"])
        self.assertEqual(before["matched_inputs"], 3)
        self.rules.apply(rule["id"])
        result = self.rules.run(rule["id"], batch=100)
        self.assertTrue(result["complete"], result)
        after = self.rules.preview(rule["id"])
        measured = after["calibration"]
        self.assertTrue(measured["applied"])
        self.assertEqual(measured["sample_count"], 3)
        recipe = self.a.get_recipe(rule["recipe_id"])
        self.assertFalse(
            calibration(self.store, self.app.profile, recipe, {"video_kbps": 4000})[
                "applied"
            ]
        )
        for old, new in zip(before["matches"], after["matches"], strict=True):
            self.assertGreaterEqual(
                new["estimate"]["high_bytes"], old["estimate"]["high_bytes"]
            )
            self.assertGreater(new["estimate"]["expected_bytes"], 0)
        unknown = estimate(recipe["definition"], {}, None, {})
        self.assertIsNone(calibrated(unknown, measured)["expected_bytes"])
        self.assertEqual(self.rules.statistics(rule["id"])["calibration"], measured)

    def test_large_required_rule_covers_unqueued_matches_and_rejects_truncation(self):
        # Many catalog entries legitimately refer to the same scanned source.
        with self.store.transaction() as db:
            db.executemany(
                "INSERT INTO items VALUES (?,'movie','{\"title\":\"Scale\"}')",
                [(f"scale-{n:05d}",) for n in range(10001)],
            )
            db.executemany(
                "INSERT INTO item_files(id,item_id,file_id,role,active,metadata,origin) VALUES (?,?,?,'primary',1,'{}','explicit')",
                [
                    (f"association-{n:05d}", f"scale-{n:05d}", self.file_id)
                    for n in range(10001)
                ],
            )
        recipe = self.a.recipe("scale", "thumbnail")
        selection = {
            "language": "sql",
            "query": "SELECT item_id FROM catalog_items WHERE item_id LIKE 'scale-%'",
            "page_size": 1000,
            "max_ids": 20000,
            "timeout_ms": 60000,
        }
        rule = self.rules.put(
            "scale", recipe["id"], "generated", selection, required=True
        )
        result = self.rules.apply(rule["id"], batch=1, limit=1)
        self.assertEqual(result["matched_inputs"], 10001)
        self.assertEqual(result["counts"]["deferred"], 10001)
        self.assertEqual(len(result["matches"]), 1)
        self.assertEqual(
            self.store.rows("SELECT count(*) AS n FROM rule_requirements")[0]["n"],
            10001,
        )
        truncated = self.rules.put(
            "truncated",
            recipe["id"],
            "generated",
            {**selection, "max_ids": 10000},
            required=True,
        )
        with self.assertRaisesRegex(CatabolicError, "max_ids"):
            Rules(self.app).apply(truncated["id"])
        self.assertEqual(
            self.store.rows("SELECT count(*) AS n FROM rule_evaluations")[0]["n"], 1
        )
