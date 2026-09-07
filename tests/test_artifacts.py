# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Generated-media workflows and recovery against disposable filesystems."""

import hashlib
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from catabolic.app import Application
from catabolic.artifacts import Artifacts
from catabolic.domain import CatabolicError
from catabolic.graphql_query import execute_graphql
from catabolic.processing import Processing
from catabolic.rendering import PRESETS
from catabolic.sql_query import execute_sql
from catabolic.store import Store


@unittest.skipUnless(
    shutil.which("ffmpeg") and shutil.which("ffprobe"),
    "real FFmpeg and ffprobe required",
)
class ArtifactTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fixture = tempfile.TemporaryDirectory()
        root = Path(cls.fixture.name)
        (root / "input.srt").write_text(
            "1\n00:00:00,000 --> 00:00:01,900\nFixture subtitle\n"
        )
        subprocess.run(
            [
                "ffmpeg",
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=160x120:rate=10:duration=2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-i",
                str(root / "input.srt"),
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-map",
                "2:s",
                "-c:v",
                "mpeg4",
                "-c:a",
                "aac",
                "-c:s",
                "srt",
                "-threads",
                "1",
                str(root / "input.mkv"),
            ],
            check=True,
            capture_output=True,
        )
        cls.payload = (root / "input.mkv").read_bytes()

    @classmethod
    def tearDownClass(cls):
        cls.fixture.cleanup()

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.source = self.root / "source"
        self.source.mkdir()
        self.generated = self.root / "generated"
        self.generated.mkdir()
        self.source_file = self.source / "input.mkv"
        self.source_file.write_bytes(self.payload)
        self.path = self.root / "catalog.sqlite3"
        Store.initialize(self.path)
        self.store = Store(self.path, writable=True)
        self.addCleanup(self.store.close)
        self.app = Application(self.store)
        self.app.bind("source", "media", str(self.source))
        self.app.scan()
        self.file_id = self.app.files()["files"][0]["id"]
        self.item_id = self.app.put_item(
            "movie", {"fixture": "movie"}, {"title": "Example", "year": 2026}
        )["id"]
        self.app.media.associate(self.file_id, self.item_id)
        self.a = Artifacts(self.app)
        self.a.bind("generated", self.generated)

    def enqueue(self, preset="thumbnail", options=None):
        recipe = self.a.recipe(preset, preset, options)
        job = self.a.enqueue(self.file_id, recipe["id"], "generated", self.item_id)
        return recipe, job

    def test_all_presets_generate_separate_valid_files_and_keep_one_item(self):
        for preset in PRESETS:
            with self.subTest(preset=preset):
                recipe, job = self.enqueue(preset)
                result = self.a.run()
                self.assertTrue(result["complete"], result)
                artifact = self.a.get(result["completed"][0]["artifact_id"])
                self.assertEqual(artifact["state"], "ready")
                path = self.generated / artifact["path"]
                self.assertEqual(
                    hashlib.sha256(path.read_bytes()).hexdigest(), artifact["sha256"]
                )
                self.assertEqual(self.source_file.read_bytes(), self.payload)
                self.assertEqual(len(self.store.rows("SELECT * FROM items")), 1)
                association = self.store.rows(
                    "SELECT * FROM item_files WHERE file_id=?", (artifact["file_id"],)
                )[0]
                self.assertEqual(association["item_id"], self.item_id)
                self.assertEqual(association["active"], 0)
                self.assertEqual(
                    self.a.enqueue(
                        self.file_id, recipe["id"], "generated", self.item_id
                    )["job_id"],
                    job["job_id"],
                )
        self.assertTrue(self.app.scan()["complete"])
        self.assertEqual(len(self.store.rows("SELECT * FROM files")), len(PRESETS) + 1)
        self.assertEqual(self.store.rows("PRAGMA foreign_key_check"), [])

    def test_recipes_are_immutable_and_unknown_options_are_rejected(self):
        first = self.a.recipe("thumb", "thumbnail")
        self.assertEqual(self.a.recipe("thumb", "thumbnail")["id"], first["id"])
        second = self.a.recipe("thumb", "thumbnail", {"start_seconds": 1})
        self.assertEqual(second["revision"], 2)
        self.assertEqual(
            self.a.get_recipe(first["id"])["definition"]["start_seconds"], 0
        )
        for options in (
            {"command": "-y original.mkv"},
            {"timeout": True},
            {"stream": -1},
        ):
            with self.assertRaises(CatabolicError):
                self.a.recipe("bad", "thumbnail", options)

    def test_abrupt_process_exit_recovers_durable_publication(self):
        for stage in ("writing", "publishing", "register"):
            with self.subTest(stage=stage):
                self.enqueue("thumbnail", {"timeout": 50 + len(stage)})
                self.store.close()
                script = """
import os, sys
from unittest.mock import patch
from catabolic.store import Store
from catabolic.app import Application
from catabolic.artifacts import Artifacts
target = {'writing': 'catabolic.rendering.render',
          'publishing': 'catabolic.artifacts.rename_noreplace',
          'register': 'catabolic.artifacts.Artifacts._register'}[sys.argv[2]]
with Store(sys.argv[1], writable=True) as store, patch(target, side_effect=lambda *a, **k: os._exit(93)):
    Artifacts(Application(store)).run()
"""
                process = subprocess.run(
                    [sys.executable, "-c", script, str(self.path), stage],
                    capture_output=True,
                )
                self.assertEqual(process.returncode, 93, process.stderr)
                self.store = Store(self.path, writable=True)
                self.addCleanup(self.store.close)
                self.app = Application(self.store)
                self.a = Artifacts(self.app)
                result = self.a.recover()
                self.assertTrue(result["complete"], result)
                recovered = self.a.get(result["recovered"][0])
                self.assertEqual(
                    recovered["state"], "interrupted" if stage == "writing" else "ready"
                )
                self.assertEqual(self.a.recover()["recovered"], [])

    def test_generated_roots_cannot_be_scanned_through_another_profile_alias(self):
        self.app.add_profile("remote")
        with self.assertRaises(CatabolicError):
            Application(self.store, "remote").bind(
                "source", "alias", str(self.generated)
            )

    def test_capabilities_need_no_database_and_missing_tools_are_explicit(self):
        from catabolic.rendering import capabilities

        with patch("catabolic.processing.shutil.which", return_value=None):
            self.assertTrue(
                all(not preset["available"] for preset in capabilities()["presets"])
            )
        result = subprocess.run(
            [
                sys.executable,
                "-m",
                "catabolic",
                "--db",
                str(self.root / "absent.db"),
                "--json",
                "artifact",
                "capabilities",
            ],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.root / "absent.db").exists())

    def test_existing_media_locations_and_nonempty_roots_cannot_be_adopted(self):
        with self.assertRaises(CatabolicError):
            self.a.bind("media", self.source)
        extra = self.root / "foreign"
        extra.mkdir()
        (extra / "keep").write_text("keep")
        with self.assertRaises(CatabolicError):
            self.a.bind("foreign", extra)
        replacement = self.root / "replacement"
        replacement.mkdir()
        with self.assertRaises(CatabolicError):
            self.app.bind("source", "generated", str(replacement))
        self.assertEqual(self.source_file.read_bytes(), self.payload)

    def test_partial_failure_is_not_inventoried_or_reused(self):
        recipe, job = self.enqueue(options={"max_output_bytes": 1024})
        result = self.a.run()
        self.assertFalse(result["complete"])
        artifact = self.a.list()["artifacts"][0]
        self.assertEqual(artifact["state"], "failed")
        self.assertIsNone(artifact["file_id"])
        (self.generated / "foreign.mkv").write_bytes(self.payload)
        self.app.scan()
        self.assertEqual(len(self.store.rows("SELECT * FROM files")), 1)
        retry = self.a.enqueue(self.file_id, recipe["id"], "generated", self.item_id)
        self.assertNotEqual(retry["job_id"], job["job_id"])
        self.assertEqual(self.source_file.read_bytes(), self.payload)

    def test_source_replacement_refuses_output_and_records_attempt(self):
        self.enqueue()
        self.source_file.unlink()
        self.source_file.write_bytes(self.payload)
        result = self.a.run()
        self.assertFalse(result["complete"])
        self.assertEqual(self.a.list()["artifacts"][0]["state"], "failed")
        self.assertEqual(
            self.store.rows("SELECT state FROM processing_attempts")[0]["state"],
            "failed",
        )

    def test_disk_full_failure_preserves_source(self):
        self.enqueue()
        with patch("catabolic.artifacts.os.fstatvfs") as stats:
            stats.return_value.f_bavail = 0
            stats.return_value.f_frsize = 4096
            self.assertFalse(self.a.run()["complete"])
        self.assertIsNone(self.a.list()["artifacts"][0]["file_id"])
        self.assertEqual(self.source_file.read_bytes(), self.payload)

    def test_crash_before_or_after_rename_recovers_once(self):
        for stage in ("rename", "register"):
            with self.subTest(stage=stage):
                self.enqueue("thumbnail", {"start_seconds": int(stage == "register")})
                target = (
                    "catabolic.artifacts.rename_noreplace"
                    if stage == "rename"
                    else "catabolic.artifacts.Artifacts._register"
                )
                with (
                    patch(target, side_effect=KeyboardInterrupt),
                    self.assertRaises(KeyboardInterrupt),
                ):
                    self.a.run()
                pending = self.a.list(state="publishing")["artifacts"][0]
                self.app.scan("generated")
                self.assertFalse(
                    self.store.rows(
                        "SELECT id FROM files WHERE location=? AND path=?",
                        ("generated", pending["path"]),
                    )
                )
                self.assertTrue(self.a.recover()["complete"])
                self.assertEqual(self.a.get(pending["id"])["state"], "ready")
                self.assertEqual(self.a.recover()["recovered"], [])
        self.assertEqual(len(self.store.rows("SELECT * FROM files")), 3)

    def test_crash_during_write_retains_partial_and_allows_explicit_retry(self):
        _, job = self.enqueue()
        with (
            patch("catabolic.rendering.render", side_effect=KeyboardInterrupt),
            self.assertRaises(KeyboardInterrupt),
        ):
            self.a.run()
        self.assertEqual(self.a.list()["artifacts"][0]["state"], "interrupted")
        Processing(self.app).retry(job["job_id"])
        self.assertTrue(self.a.run()["complete"])
        self.assertEqual(
            len(Processing(self.app).attempts(job["job_id"])["attempts"]), 2
        )

    def test_destination_collision_and_replacement_cannot_overwrite(self):
        self.enqueue()
        with (
            patch(
                "catabolic.artifacts.rename_noreplace", side_effect=KeyboardInterrupt
            ),
            self.assertRaises(KeyboardInterrupt),
        ):
            self.a.run()
        artifact = self.a.get(self.a.list()["artifacts"][0]["id"])
        target = self.generated / artifact["path"]
        target.symlink_to(self.source_file)
        self.assertFalse(self.a.recover()["complete"])
        self.assertTrue(target.is_symlink())
        self.assertEqual(self.source_file.read_bytes(), self.payload)
        target.unlink()
        partial = self.generated / artifact["temporary_path"]
        partial.write_bytes(b"changed")
        self.assertFalse(self.a.recover()["complete"])
        self.assertFalse(target.exists())

    def test_missing_or_changed_completed_output_is_not_reused(self):
        recipe, job = self.enqueue()
        self.assertTrue(self.a.run()["complete"])
        artifact = self.a.list()["artifacts"][0]
        (self.generated / artifact["path"]).write_bytes(b"changed")
        replacement = self.a.enqueue(
            self.file_id, recipe["id"], "generated", self.item_id
        )
        self.assertNotEqual(replacement["job_id"], job["job_id"])

    def test_analysis_runner_does_not_claim_output_jobs(self):
        _, job = self.enqueue()
        Processing(self.app).run()
        self.assertEqual(Processing(self.app).get(job["job_id"])["state"], "queued")
        self.assertEqual(self.a.list()["artifacts"], [])

    def test_sql_graphql_and_attempt_history_expose_lineage(self):
        recipe, job = self.enqueue()
        self.assertTrue(self.a.run()["complete"])
        result = execute_sql(
            self.path,
            "SELECT input_file_id,recipe_id,state FROM catalog_artifacts",
            _store=self.store,
        )
        self.assertIn(self.file_id, str(result))
        query = "{ artifacts(first:1) { nodes pageInfo { hasNextPage endCursor } } recipes(first:1) { nodes } }"
        result = execute_graphql(self.path, query, _store=self.store)
        self.assertNotIn("errors", result, result)
        self.assertIn(self.file_id, str(result))
        history = Processing(self.app).attempts(job["job_id"])["attempts"][0]
        self.assertIsNotNone(history["started_at"])
        self.assertIsNotNone(history["finished_at"])
        self.assertEqual(
            json.loads(history["result"])["artifact_id"],
            self.a.list()["artifacts"][0]["id"],
        )
        self.assertEqual(self.a.get_recipe(recipe["id"])["revision"], 1)
