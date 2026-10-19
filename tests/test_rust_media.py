# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Independent Python/native analysis runs on disposable cloned schema-32 catalogs."""

import hashlib
import json
import os
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
import wave
from pathlib import Path
from unittest.mock import patch

from catabolic.app import Application
from catabolic.processing import Processing
from catabolic.store import Store
from tests.test_rust_migration import BINARY


class NativeMediaParityTest(unittest.TestCase):
    def setUp(self):
        self.assertTrue(BINARY.is_file(), "build catabolic-cli before qualification")
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.source = self.root / "source"
        self.source.mkdir()
        with wave.open(str(self.source / "tone.wav"), "wb") as audio:
            audio.setnchannels(1)
            audio.setsampwidth(2)
            audio.setframerate(8000)
            audio.writeframes(b"\0\0" * 800)
        (self.source / "notes.txt").write_text("Straße STRASSE foo_bar café\n第二行\n")
        (self.source / "subtitle.srt").write_text(
            "1\n00:00:01,250 --> 00:00:02,000\nFirst line\n\n"
            "2\n00:00:03.500 --> 00:00:04.000\nSecond line\n"
        )
        (self.source / "broken.mkv").write_bytes(b"\0" * 128)
        self.template = self.root / "template.db"
        Store.initialize(self.template)
        with Store(self.template, writable=True) as store:
            app = Application(store)
            app.bind("source", "media", str(self.source))
            app.scan()
            files = app.files()["files"]
            self.files = {row["path"]: row["id"] for row in files}
            item = app.put_item("track", {"fixture": "media"}, {"title": "Unchanged"})[
                "id"
            ]
            app.media.associate(self.files["tone.wav"], item)
        self.native_path = self.root / "native.db"
        self.python_path = self.root / "python.db"
        self.reset()

    def reset(self):
        for path in (self.native_path, self.python_path):
            shutil.copyfile(self.template, path)

    def native(self, *args):
        result = subprocess.run(
            [str(BINARY), "--db", str(self.native_path), "--json", *map(str, args)],
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def run_pair(self, operation, filename="tone.wav", options=None):
        options = options or {}
        file_id = self.files[filename]
        original = {
            p.name: hashlib.sha256(p.read_bytes()).hexdigest()
            for p in self.source.iterdir()
        }
        native = self.native(
            "media-enqueue", operation, file_id, "--options", json.dumps(options)
        )
        self.assertTrue(native["complete"], native)
        native_id = native["queued"][0]
        with Store(self.python_path, writable=True) as store:
            reference = Processing(Application(store))
            queued = reference.enqueue(operation, file_ids=[file_id], options=options)
            python_id = queued["queued"][0]
            reference.run(job_ids=[python_id], _refresh=False)
        self.native("media-run", native_id)
        for table in (
            "processing_jobs",
            "processing_attempts",
            "file_facts",
            "content_baselines",
            "text_segments",
            "text_terms",
            "media_components",
            "component_occurrences",
        ):

            def read(path, job, table=table):
                with sqlite3.connect(path) as db:
                    db.row_factory = sqlite3.Row
                    # Python inserts its per-segment word set in hash-randomized order.
                    # Compare this relation by its logical key, never by accidental rowid.
                    ordering = "segment_id,term" if table == "text_terms" else "rowid"
                    records = [
                        dict(r)
                        for r in db.execute(
                            f"SELECT * FROM {table} ORDER BY {ordering}"
                        )
                    ]
                for row in records:
                    for key, value in list(row.items()):
                        if key in {
                            "created_at",
                            "finished_at",
                            "started_at",
                            "retry_after",
                        }:
                            row.pop(key)
                        elif value == job:
                            row[key] = "<job>"
                return records

            self.assertEqual(
                read(self.native_path, native_id),
                read(self.python_path, python_id),
                table,
            )
        self.assertEqual(
            original,
            {
                p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                for p in self.source.iterdir()
            },
        )
        return native_id, python_id

    def test_actual_sniff_hash_probe_decode_and_component_inventory(self):
        for operation in ("sniff", "hash", "probe", "decode"):
            with self.subTest(operation=operation):
                self.reset()
                self.run_pair(operation)

    def test_text_lines_subtitles_casefold_and_partial_budget(self):
        for filename, options in (
            ("notes.txt", {}),
            ("subtitle.srt", {}),
            ("notes.txt", {"max_bytes": 1}),
        ):
            with self.subTest(filename=filename, options=options):
                self.reset()
                self.run_pair("text", filename, options)

    def test_header_defect_and_required_stream_policy(self):
        self.run_pair("probe", "broken.mkv")
        self.reset()
        self.run_pair("probe", options={"required_streams": ["video"]})

    def test_missing_optional_probe_is_durable_unsupported_evidence(self):
        tools = self.root / "identity-tools"
        tools.mkdir()
        findmnt = shutil.which("findmnt")
        if findmnt:
            (tools / "findmnt").symlink_to(findmnt)
        self.assertIsNone(shutil.which("ffprobe", path=str(tools)))
        with patch.dict(os.environ, {"PATH": str(tools)}):
            self.run_pair("probe")

    def test_immutable_analysis_revisions_have_identical_definition_bytes_and_digest(
        self,
    ):
        from catabolic.operations import Operations

        definition = {
            "kind": "analysis",
            "operation": "text",
            "options": {"max_bytes": 16},
        }
        actual = self.native("operation-put", "extract", json.dumps(definition))
        with Store(self.python_path, writable=True) as store:
            expected = Operations(Application(store)).put("extract", definition)
        for key in (
            "name",
            "revision",
            "preset",
            "definition",
            "digest",
            "operation_kind",
            "output_definition_id",
        ):
            self.assertEqual(actual[key], expected[key], key)


if __name__ == "__main__":
    unittest.main()
