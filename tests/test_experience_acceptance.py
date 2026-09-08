# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Adversarial scorecard cases: a successful process must not imply acceptance."""

import copy
import json
import unittest

from scripts.experience_evaluator import evaluate


class ExperienceEvaluatorTest(unittest.TestCase):
    def setUp(self):
        self.key = {
            "namespace": "fixture",
            "cases": [
                {
                    "id": "clear",
                    "location": "a",
                    "path": "clear.mkv",
                    "identity": "correct",
                },
                {
                    "id": "ambiguous",
                    "location": "a",
                    "path": "unknown.mkv",
                    "identity": None,
                },
            ],
            "expected_targets": {"originals": ["a/clear.mkv"], "mobile": []},
        }
        self.state = {
            "associations": [["a", "clear.mkv", "fixture", "correct", "primary", 1]],
            "accepted": [],
            "pending": 0,
            "refresh": 0,
            "links": {"originals/title.mkv": {"target": "a/clear.mkv", "exists": True}},
        }
        self.review = {
            "run_type": "reference",
            "interventions": [],
            "cases": {
                "clear": {"state": "accepted", "identity": "correct"},
                "ambiguous": {"state": "deferred", "reason": "Need release year"},
            },
        }
        self.checkpoints = {
            name: copy.deepcopy(self.state)
            for name in (
                "initial",
                "repeat1",
                "repeat2",
                "late",
                "late_repeat1",
                "late_repeat2",
            )
        }
        self.checkpoints.update(
            source_preserved=True,
            outage_recovered=True,
            recovery={
                name: {"interrupted_exit": 77, "pending_before": 1, "verified": True}
                for name in ("journal", "completion")
            },
        )

    def result(self):
        return evaluate(self.key, self.state, self.review, self.checkpoints)

    def test_correct_abstention_passes_without_claiming_agent_measurement(self):
        result = self.result()
        self.assertTrue(result["passed"])
        self.assertEqual(result["judgment_status"], "not_measured")
        self.assertEqual(result["metrics"]["appropriate_unresolved"], 1)

    def test_defer_everything_fails_coverage(self):
        self.state["associations"] = []
        self.review["cases"]["clear"] = {"state": "deferred", "reason": "Unsure"}
        self.assertFalse(self.result()["passed"])
        self.assertEqual(self.result()["metrics"]["unnecessary_deferrals"], ["clear"])

    def test_wrong_identity_is_not_excused_by_later_correction(self):
        self.state["accepted"] = [
            [
                "a",
                "clear.mkv",
                json.dumps({"item": {"identities": {"fixture": "wrong"}}}),
            ]
        ]
        self.assertFalse(self.result()["passed"])
        self.assertEqual(self.result()["metrics"]["incorrect_committed"], ["clear"])

    def test_wrong_commitment_fails_even_if_review_claims_success(self):
        self.state["associations"][0][3] = "wrong"
        self.assertFalse(self.result()["passed"])

    def test_confident_wrong_claim_without_database_write_fails(self):
        self.review["cases"]["ambiguous"] = {"state": "accepted", "identity": "guess"}
        self.assertFalse(self.result()["passed"])
        self.assertEqual(self.result()["metrics"]["incorrect_confident"], ["ambiguous"])

    def test_missing_case_or_vague_abstention_fails(self):
        self.review["cases"].pop("ambiguous")
        self.assertFalse(self.result()["passed"])
        self.review["cases"]["ambiguous"] = {"state": "deferred", "reason": ""}
        self.assertFalse(self.result()["passed"])

    def test_extra_missing_or_broken_link_fails(self):
        for links in (
            {},
            {
                **self.state["links"],
                "originals/extra": {"target": "a/unknown.mkv", "exists": True},
            },
            {"originals/title.mkv": {"target": "a/clear.mkv", "exists": False}},
        ):
            with self.subTest(links=links):
                self.state["links"] = links
                self.assertFalse(self.result()["passed"])

    def test_recovery_claim_requires_observed_interruption_and_durable_work(self):
        for field, value in [
            ("pending_before", 0),
            ("interrupted_exit", 0),
            ("verified", False),
        ]:
            with self.subTest(field=field):
                original = self.checkpoints["recovery"]["journal"][field]
                self.checkpoints["recovery"]["journal"][field] = value
                self.assertFalse(self.result()["passed"])
                self.checkpoints["recovery"]["journal"][field] = original

    def test_repeat_churn_and_missing_checkpoint_fail(self):
        self.checkpoints["repeat1"]["links"]["originals/title.mkv"]["inode"] = 999
        self.assertFalse(self.result()["passed"])
        del self.checkpoints["repeat1"]
        self.assertFalse(self.result()["passed"])

    def test_source_damage_and_unrecovered_queue_fail(self):
        self.checkpoints["source_preserved"] = False
        self.assertFalse(self.result()["passed"])
        self.checkpoints["source_preserved"] = True
        self.state["refresh"] = 1
        self.assertFalse(self.result()["passed"])

    def test_agent_self_report_cannot_certify_agent_judgment(self):
        self.review["run_type"] = "agent"
        result = self.result()
        self.assertTrue(result["mechanical_passed"])
        self.assertFalse(result["passed"])
        self.assertEqual(result["judgment_status"], "requires_transcript_adjudication")

    def test_right_identity_with_wrong_title_fails(self):
        self.key["cases"][0].update(title="Correct title", year=2020)
        self.state["identified_metadata"] = [
            ["fixture", "correct", "movie", '{"title":"Wrong title","year":2020}']
        ]
        self.assertFalse(self.result()["passed"])

    def test_unexpected_regular_output_file_fails(self):
        self.state["unexpected_files"] = ["originals/unowned.mkv"]
        self.assertFalse(self.result()["passed"])
