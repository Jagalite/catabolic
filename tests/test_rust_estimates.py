# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Every recorded-only planning heuristic is compared with the frozen owner."""

import json
import select
import subprocess
import tempfile
import unittest
from pathlib import Path

from catabolic.app import Application
from catabolic.estimate_calibration import calibrated, calibration
from catabolic.rule_estimates import estimate, total
from catabolic.store import Store
from tests.test_media_model import populate_media
from tests.test_rust_migration import BINARY


@unittest.skipUnless(BINARY.is_file(), "build catabolic-cli before qualification")
class NativeEstimatesTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.path = Path(temporary.name) / "catalog.sqlite3"
        Store.initialize(self.path)
        self.worker = subprocess.Popen(
            [str(BINARY), "--stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(self.close_worker)

    def close_worker(self):
        self.worker.stdin.close()
        try:
            self.worker.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.worker.kill()
            self.worker.wait()
        self.worker.stdout.close()
        self.worker.stderr.close()

    def native(self, action, args):
        self.worker.stdin.write(
            json.dumps(
                ["--db", str(self.path), "definitions", action, json.dumps(args)]
            )
            + "\n"
        )
        self.worker.stdin.flush()
        self.assertTrue(select.select([self.worker.stdout], [], [], 60)[0])
        result = json.loads(self.worker.stdout.readline())
        self.assertEqual(result["status"], 0, result)
        return result["result"]

    def test_all_preset_heuristics_duration_audio_and_unknowns(self):
        probe = {
            "summary": {"duration": "12.125"},
            "streams": [
                {"codec_type": "video", "width": 1920, "height": 1080},
                {
                    "codec_type": "audio",
                    "sample_rate": "44100",
                    "channels": 2,
                    "bits_per_raw_sample": "24",
                },
            ],
        }
        presets = [
            "thumbnail",
            "waveform",
            "preview",
            "h264-720p",
            "h264-1080p",
            "av1-720p",
            "hdr-sdr-1080p",
            "remux-mkv",
            "audio-aac",
            "audio-opus",
            "audio-flac",
            "audio-normalize",
            "subtitle-srt",
            "image-jpeg",
            "component-mux",
        ]
        values = []
        for preset in presets:
            for facts in (
                probe,
                None,
                {},
                {"summary": {"duration": "4.5"}},
                {"format": {"duration": "8_0"}, "streams": probe["streams"]},
            ):
                for options in ({}, {"video_kbps": 8100}):
                    recipe = {
                        "preset": preset,
                        "max_output_bytes": 1_000_000,
                        "width": 1200,
                        "height": 200,
                        "duration_seconds": 5,
                        "start_seconds": 7,
                    }
                    args = {
                        "recipe": recipe,
                        "observation": {"size": "45000"},
                        "probe": facts,
                        "assumptions": options,
                    }
                    with self.subTest(preset=preset, facts=facts, options=options):
                        actual = self.native("estimate", args)
                        self.assertEqual(
                            actual,
                            estimate(recipe, args["observation"], facts, options),
                        )
                        values.append(actual)
        for estimates in (
            [],
            values,
            [v for v in values if v["expected_bytes"] is not None],
        ):
            self.assertEqual(
                self.native("estimate-total", {"values": estimates}), total(estimates)
            )

    def test_extreme_finite_facts_and_arbitrary_precision_totals(self):
        for duration in (True, "NaN", "Infinity", "1e400", 0, -1, 10**15, 10**16):
            recipe = {"preset": "audio-flac", "max_output_bytes": 2**80}
            probe = {
                "summary": {"duration": duration},
                "streams": [
                    {
                        "codec_type": "audio",
                        "sample_rate": 10**15,
                        "channels": 10**15,
                        "bits_per_raw_sample": 10**15,
                    }
                ],
            }
            expected = estimate(recipe, {}, probe, {})
            actual = self.native(
                "estimate",
                {
                    "recipe": recipe,
                    "observation": {},
                    "probe": probe,
                    "assumptions": {},
                },
            )
            self.assertEqual(actual, expected)
            self.assertEqual(
                self.native("estimate-total", {"values": [actual, actual]}),
                total([expected, expected]),
            )

    def test_calibration_adjustment_and_missing_sample_report(self):
        recipe = {
            "id": "not-a-recipe",
            "definition": {"preset": "audio-aac", "max_output_bytes": 1_000_000},
        }
        for assumptions in ({}, {"video_kbps": 5000}):
            with Store(self.path) as store:
                expected = calibration(store, "default", recipe, assumptions)
            self.assertEqual(
                self.native(
                    "estimate-calibration",
                    {"recipe": recipe, "assumptions": assumptions},
                ),
                expected,
            )
        value = estimate(recipe["definition"], {}, {"summary": {"duration": 20}}, {})
        for report in (
            {"applied": False},
            {
                "applied": True,
                "factor": 0.2,
                "high_factor": 1.7,
                "low_factor": 0.1,
                "sample_count": 4,
            },
        ):
            self.assertEqual(
                self.native("estimate-calibrated", {"value": value, "report": report}),
                calibrated(value, report),
            )

    def test_retained_calibration_ranks_distinct_sources_before_sampling(self):
        recipe = {
            "id": "recipe",
            "definition": {"preset": "audio-aac", "max_output_bytes": 1_000_000},
        }
        with Store(self.path, writable=True) as store:
            files, _ = populate_media(Application(store), self.path.parent)
            with store.transaction() as db:
                db.execute(
                    "INSERT INTO processing_recipes(id,name,revision,preset,definition,digest) VALUES ('recipe','recipe',1,'audio-aac',?,'digest')",
                    (json.dumps(recipe["definition"]),),
                )
                db.execute("INSERT INTO locations VALUES ('generated')")
                db.execute(
                    "INSERT INTO generated_locations VALUES ('default','generated')"
                )
                sources = [
                    files["movie.mkv"],
                    files["track01.flac"],
                    files["book.epub"],
                    files["movie.mkv"],
                ]
                for i, source in enumerate(sources):
                    db.execute(
                        "INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state,recipe_id) VALUES (?,'default',?,'render','media','{}','{}',?,'complete','recipe')",
                        (f"job-{i}", source, f"cache-{i}"),
                    )
                    db.execute(
                        "INSERT INTO processing_attempts(job_id,attempt,state,retryable,retry_after) VALUES (?,1,'complete',0,0)",
                        (f"job-{i}",),
                    )
                    db.execute(
                        "INSERT INTO files VALUES (?,'generated',?)",
                        (f"rendered-{i}", f"output-{i}.m4a"),
                    )
                    db.execute(
                        "INSERT INTO processing_artifacts(id,job_id,attempt,profile,location,path,temporary_path,binding,state,file_id,item_id,role,size,sha256,validation) VALUES (?,?,1,'default','generated',?,?,'{}','ready',?,'movie','primary',?,'digest',?)",
                        (
                            f"artifact-{i}",
                            f"job-{i}",
                            f"output-{i}.m4a",
                            f"temp-{i}",
                            f"rendered-{i}",
                            [1, 2000, 3000, 4000][i],
                            json.dumps({"input": {"summary": {"duration": 20}}}),
                        ),
                    )
            expected = calibration(store, "default", recipe, {})
        before = self.path.read_bytes()
        actual = self.native(
            "estimate-calibration", {"recipe": recipe, "assumptions": {}}
        )
        self.assertEqual(actual, expected)
        self.assertEqual(actual["sample_count"], 3)
        self.assertTrue(actual["applied"])
        self.assertEqual(before, self.path.read_bytes())
