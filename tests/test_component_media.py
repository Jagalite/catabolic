# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Real, disposable media proves delivery isolation and output lineage."""

import hashlib
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from catabolic import components
from catabolic.artifacts import Artifacts
from catabolic.component_packaging import enqueue, extraction
from catabolic.curation import occurrence
from catabolic.domain import CatabolicError
from catabolic.fallback_policies import Policies
from catabolic.fallback_resolution import Resolver
from catabolic.processing import Processing
from tests import test_component_packages as fixtures


@unittest.skipUnless(
    shutil.which("ffmpeg") and shutil.which("ffprobe"), "FFmpeg required"
)
class MediaTest(unittest.TestCase):
    setUp = fixtures.PackageTest.setUp
    rows = fixtures.PackageTest.rows
    query = fixtures.PackageTest.query
    policy = fixtures.PackageTest.policy
    claim = fixtures.PackageTest.claim

    def prepare(self):
        with tempfile.TemporaryDirectory() as directory:
            subtitle = Path(directory) / "fixture.srt"
            subtitle.write_text(
                "1\n00:00:00,100 --> 00:00:01,700\nEnglish forced fixture\n"
            )
            subprocess.run(
                [
                    "ffmpeg",
                    "-v",
                    "error",
                    "-nostdin",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=96x64:rate=10:duration=2",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=2",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=660:duration=2",
                    "-i",
                    str(subtitle),
                    "-map",
                    "0:v",
                    "-map",
                    "1:a",
                    "-map",
                    "2:a",
                    "-map",
                    "3:s",
                    "-map",
                    "3:s",
                    "-c:v",
                    "mpeg4",
                    "-c:a",
                    "aac",
                    "-c:s",
                    "srt",
                    "-metadata:s:a:0",
                    "language=eng",
                    "-metadata:s:a:1",
                    "language=fra",
                    "-metadata:s:s:0",
                    "language=eng",
                    "-metadata:s:s:1",
                    "language=fra",
                    "-disposition:s:0",
                    "forced",
                    "-disposition:s:1",
                    "0",
                    "-threads",
                    "1",
                    str(self.source / "a.mkv"),
                ],
                check=True,
                capture_output=True,
            )
        self.app.scan()
        self.file_id = self.store.rows("SELECT id FROM files WHERE path='a.mkv'")[0][
            "id"
        ]
        self.app.media.associate(self.file_id, self.item)
        processor = Processing(self.app)
        processor.enqueue("probe", file_ids=[self.file_id])
        result = processor.run()
        self.assertTrue(result["complete"], result)
        self.a = Artifacts(self.app)
        self.generated = self.root / "generated"
        self.generated.mkdir()
        self.a.bind("generated", self.generated)
        self.original = hashlib.sha256((self.source / "a.mkv").read_bytes()).hexdigest()

    def artifact(self, result):
        self.assertTrue(result["complete"], result)
        self.assertEqual(len(result["completed"]), 1, result)
        artifact = self.a.get(result["completed"][0]["artifact_id"])
        self.assertEqual(artifact["state"], "ready", artifact)
        self.assertEqual(
            hashlib.sha256((self.source / "a.mkv").read_bytes()).hexdigest(),
            self.original,
        )
        return artifact

    def test_mux_selected_streams_then_resolve_verified_artifact(self):
        from tests.test_component_lifecycle import LifecycleTest

        self.prepare()
        recipe = self.a.recipe("selected", "component-mux")
        policy = self.policy("selected_only", recipe["id"])
        projection = LifecycleTest.bind(self, self.policy())
        self.assertTrue(LifecycleTest.apply(self, projection)["complete"])
        original_targets = {
            p.readlink() for p in self.output.rglob("*") if p.is_symlink()
        }
        plan = Resolver(self.app).resolve([{"item_id": self.item}], policy)
        self.assertFalse(
            plan["decisions"][0]["evidence"]["component_package"]["packaging"]["ready"]
        )
        self.assertFalse(
            self.store.rows("SELECT 1 FROM processing_jobs WHERE operation='render'")
        )
        enqueue(self.app, policy, self.item, "generated", plan["plan_id"])
        artifact = self.artifact(self.a.run())
        streams = artifact["validation"]["output"]["streams"]
        self.assertEqual(
            [s["codec_type"] for s in streams], ["video", "audio", "subtitle"]
        )
        self.assertEqual(streams[1]["tags"]["language"], "eng")
        self.assertEqual(streams[2]["disposition"]["forced"], 1)
        lineage = self.store.rows(
            "SELECT * FROM component_output_lineage WHERE artifact_id=? ORDER BY ordinal",
            (artifact["id"],),
        )
        self.assertEqual(len(lineage), 3)
        outputs = self.rows("file_id=? AND current=1", (artifact["file_id"],))
        self.assertEqual(len(outputs), 3)
        self.assertEqual(
            {r["component_id"] for r in outputs}, {r["component_id"] for r in lineage}
        )
        current = Resolver(self.app).resolve([{"item_id": self.item}], policy)[
            "decisions"
        ][0]
        self.assertEqual(current["file_id"], artifact["file_id"])
        self.assertTrue(current["evidence"]["component_package"]["packaging"]["ready"])
        self.assertIn(artifact["file_id"], current["content_path"])
        projection.bind(
            "global",
            policy,
            self.query("members", "SELECT id AS item_id FROM items"),
            "flat",
        )
        published = LifecycleTest.apply(self, projection)
        self.assertTrue(published["complete"], published)
        self.assertNotEqual(
            original_targets,
            {p.readlink() for p in self.output.rglob("*") if p.is_symlink()},
        )
        self.assertEqual(published["desired"][0]["file_id"], artifact["file_id"])
        links = {
            str(p): p.lstat().st_ino for p in self.output.rglob("*") if p.is_symlink()
        }
        self.assertEqual(
            LifecycleTest.apply(self, projection)["publication"]["applied"], []
        )
        self.assertEqual(
            links,
            {
                str(p): p.lstat().st_ino
                for p in self.output.rglob("*")
                if p.is_symlink()
            },
        )
        # Same exact request reuses its durable job and never rewrites an output.
        before = (self.generated / artifact["path"]).stat()
        self.assertTrue(self.a.run()["complete"])
        self.assertEqual(
            (self.generated / artifact["path"]).stat().st_ino, before.st_ino
        )
        self.assertEqual(self.store.rows("PRAGMA foreign_key_check"), [])

    def test_extraction_is_component_only_and_inherits_identity(self):
        self.prepare()
        row = components.get(
            self.store,
            "default",
            self.rows("kind='subtitle' AND language='en' AND current=1")[0][
                "occurrence_id"
            ],
        )
        wrong = self.a.recipe("wrong", "remux-mkv", {"stream_indices": [0, 3]})
        with self.assertRaisesRegex(CatabolicError, "exactly"):
            extraction(self.app, row["occurrence_id"], wrong["id"], "generated")
        wrong_kind = self.a.recipe("wrong-kind", "audio-flac")
        with self.assertRaisesRegex(CatabolicError, "different component kind"):
            extraction(self.app, row["occurrence_id"], wrong_kind["id"], "generated")
        recipe = self.a.recipe("english", "subtitle-srt", {"stream": 0})
        preview = extraction(self.app, row["occurrence_id"], recipe["id"], "generated")
        extraction(
            self.app,
            row["occurrence_id"],
            recipe["id"],
            "generated",
            apply=True,
            expected_plan=preview["plan_id"],
        )
        artifact = self.artifact(self.a.run())
        self.assertEqual(len(artifact["validation"]["output"]["streams"]), 1)
        self.assertIn(
            "English forced fixture", (self.generated / artifact["path"]).read_text()
        )
        output = components.get(
            self.store,
            "default",
            self.rows("file_id=? AND current=1", (artifact["file_id"],))[0][
                "occurrence_id"
            ],
        )
        self.assertEqual(output["component_id"], row["component_id"])
        self.assertEqual(output["language"], "en")
        self.assertEqual(output["forced"], 1)
        self.assertIsNone(output["observed"]["forced"])
        self.assertIsNone(output["compatibility"])
        self.assertTrue(output["technically_verified"])

    def test_changed_component_input_blocks_durable_job(self):
        self.prepare()
        row = self.rows("kind='subtitle' AND language='en' AND current=1")[0]
        recipe = self.a.recipe("english", "subtitle-srt")
        preview = extraction(self.app, row["occurrence_id"], recipe["id"], "generated")
        extraction(
            self.app,
            row["occurrence_id"],
            recipe["id"],
            "generated",
            apply=True,
            expected_plan=preview["plan_id"],
        )
        self.claim(row, attributes={"title": "Reviewed translation"})
        result = self.a.run()
        self.assertFalse(result["complete"])
        self.assertFalse(
            self.store.rows("SELECT 1 FROM processing_artifacts WHERE state='ready'")
        )
        self.assertFalse(list(self.generated.glob("*.srt")))

    def test_external_audio_and_subtitle_mux_with_accepted_offset(self):
        self.prepare()
        subprocess.run(
            [
                "ffmpeg",
                "-v",
                "error",
                "-nostdin",
                "-i",
                str(self.source / "a.mkv"),
                "-map",
                "0:1",
                "-c",
                "copy",
                str(self.source / "external.mka"),
            ],
            check=True,
            capture_output=True,
        )
        (self.source / "external.srt").write_text(
            "1\n00:00:00,100 --> 00:00:01,700\nExternal translation\n"
        )
        self.app.scan()
        for path, role in [
            ("external.mka", "custom:audio"),
            ("external.srt", "subtitle"),
        ]:
            fid = self.store.rows("SELECT id FROM files WHERE path=?", (path,))[0]["id"]
            self.app.media.associate(
                fid,
                self.item,
                role=role,
                metadata={
                    "language": "en",
                    **({"forced": True} if role == "subtitle" else {}),
                },
            )
            Processing(self.app).enqueue("probe", file_ids=[fid])
        result = Processing(self.app).run()
        self.assertTrue(result["complete"], result)
        rev = components.revision(occurrence(self.store, "default", self.file_id))
        for row in self.rows("storage='external' AND current=1"):
            self.claim(
                row,
                compatibility={
                    "edition_id": self.item,
                    "video_file_id": self.file_id,
                    "video_revision": rev,
                    "part": None,
                    "timeline_id": "container:" + rev,
                    "coverage": "full",
                    "offset_seconds": 0.25 if row["kind"] == "subtitle" else 0.0,
                    "synchronization": "declared",
                },
            )
        recipe = self.a.recipe("external-mux", "component-mux")
        definition = Policies(self.store).get(
            self.policy("selected_only", recipe["id"])
        )["definition"]
        for req, kind in zip(
            definition["package"]["requirements"], ["audio", "subtitle"], strict=True
        ):
            req["fallbacks"] = [
                {
                    "name": "external",
                    "query_id": self.query(
                        "external-" + kind,
                        "SELECT occurrence_id FROM catalog_component_occurrences WHERE kind='"
                        + kind
                        + "' AND storage='external' AND language='en' AND current=1",
                    ),
                }
            ]
        policy = Policies(self.store).put("external", definition)["id"]
        plan = Resolver(self.app).resolve([{"item_id": self.item}], policy)
        enqueue(self.app, policy, self.item, "generated", plan["plan_id"])
        artifact = self.artifact(self.a.run())
        self.assertEqual(
            artifact["validation"]["packet_verification"][2]["accepted_offset_seconds"],
            0.25,
        )
        self.assertEqual(
            artifact["validation"]["output"]["streams"][2]["disposition"]["forced"], 1
        )

    def test_font_dependency_is_preserved_and_hashed(self):
        self.prepare()
        # Attachment content is opaque; the runner must preserve its exact bytes.
        (self.source / "fixture.ttf").write_bytes(b"disposable font dependency bytes")
        self.app.scan()
        font = self.store.rows("SELECT id FROM files WHERE path='fixture.ttf'")[0]["id"]
        row = self.rows("kind='subtitle' AND language='en' AND current=1")[0]
        self.claim(
            row,
            dependencies_complete=True,
            dependencies=[
                {
                    "file_id": font,
                    "revision": components.revision(
                        occurrence(self.store, "default", font)
                    ),
                    "purpose": "font",
                }
            ],
        )
        recipe = self.a.recipe("font-mux", "component-mux")
        policy = self.policy("selected_only", recipe["id"])
        plan = Resolver(self.app).resolve([{"item_id": self.item}], policy)
        enqueue(self.app, policy, self.item, "generated", plan["plan_id"])
        artifact = self.artifact(self.a.run())
        self.assertEqual(
            artifact["validation"]["dependencies"][0]["sha256"],
            hashlib.sha256((self.source / "fixture.ttf").read_bytes()).hexdigest(),
        )
        self.assertEqual(len(artifact["validation"]["output"]["streams"]), 4)

    def test_component_lineage_survives_publication_recovery(self):
        self.prepare()
        row = self.rows("kind='subtitle' AND language='en' AND current=1")[0]
        recipe = self.a.recipe("recover-subtitle", "subtitle-srt")
        preview = extraction(self.app, row["occurrence_id"], recipe["id"], "generated")
        extraction(
            self.app,
            row["occurrence_id"],
            recipe["id"],
            "generated",
            apply=True,
            expected_plan=preview["plan_id"],
        )
        with (
            patch.object(Artifacts, "_register", side_effect=KeyboardInterrupt),
            self.assertRaises(KeyboardInterrupt),
        ):
            self.a.run()
        self.assertEqual(self.store.rows("SELECT * FROM component_output_lineage"), [])
        self.assertTrue(self.a.recover()["complete"])
        self.assertEqual(
            len(self.store.rows("SELECT * FROM component_output_lineage")), 1
        )
        self.assertEqual(self.a.recover()["recovered"], [])

    def test_cli_preview_and_explicit_extraction(self):
        self.prepare()
        row = self.rows("kind='subtitle' AND language='en' AND current=1")[0]
        recipe = self.a.recipe("cli-subtitle", "subtitle-srt")
        command = [
            sys.executable,
            "-m",
            "catabolic",
            "--db",
            str(self.database),
            "--profile",
            "default",
            "--json",
        ]
        args = [
            "component",
            "extract",
            row["occurrence_id"],
            "--recipe",
            recipe["id"],
            "--location",
            "generated",
        ]
        with self.store.detached():
            preview = subprocess.run(
                command + args, check=True, text=True, capture_output=True
            )
        plan = json.loads(preview.stdout)
        self.assertFalse(
            self.store.rows("SELECT 1 FROM processing_jobs WHERE operation='render'")
        )
        with self.store.detached():
            admitted = subprocess.run(
                command + args + ["--apply", "--expected-plan", plan["plan_id"]],
                check=True,
                text=True,
                capture_output=True,
            )
            executed = subprocess.run(
                command + ["artifact", "run"],
                check=True,
                text=True,
                capture_output=True,
            )
        self.assertTrue(json.loads(admitted.stdout)["job_id"])
        self.artifact(json.loads(executed.stdout))
