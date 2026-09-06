# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import copy
import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.layouts import PRESETS, Layouts, validate_layout
from catabolic.manifest import FILENAME, Manifest
from catabolic.reconcile import Reconciler
from catabolic.store import Store
from tests.test_media_model import populate_media


class NativeLayoutTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.path = self.root / "catalog.sqlite3"
        Store.initialize(self.path)
        self.store = Store(self.path, writable=True)
        self.addCleanup(self.store.close)
        self.app = Application(self.store)
        self.files, self.entries = populate_media(self.app, self.root)

    def test_mixed_media_duplicate_names_metadata_changes_and_manifest(self):
        source = self.root / "media"
        (source / "another").mkdir()
        (source / "another/book.epub").write_bytes(b"another edition file")
        (source / "draft?.epub").write_bytes(b"portable filename")
        self.app.scan()
        files = {row["path"]: row["id"] for row in self.app.files()["files"]}
        for filename in ("another/book.epub", "draft?.epub"):
            self.app.media.associate(files[filename], "edition", role="primary")
        # The same source can also have a second role without a destination clash.
        self.app.media.associate(files["notes.txt"], "notes", role="custom:transcript")
        before = {
            p: (p.stat().st_ino, hashlib.sha256(p.read_bytes()).hexdigest())
            for p in source.rglob("*")
            if p.is_file()
        }
        output = self.root / "native"
        output.mkdir()
        self.app.bind("output", "native", str(output))
        layouts = Layouts(self.app)
        layouts.put("native", copy.deepcopy(PRESETS["catabolic"]))
        plan = layouts.run("native", "native", apply=True)
        self.assertTrue(plan["applied"], plan)
        self.assertEqual(plan["desired_count"], 17)
        self.assertTrue(Reconciler(self.app).apply("native")["healthy"])
        self.assertTrue(Reconciler(self.app).verify("native")["healthy"])
        kinds = {
            row["id"]: row["kind"]
            for row in self.store.db.execute("SELECT id,kind FROM items")
        }
        for filename, item, role, _, _ in self.entries:
            link = (
                output
                / kinds[item].replace(":", "_")
                / item
                / role
                / files[filename]
                / filename
            )
            self.assertTrue(link.is_symlink(), link)
            self.assertEqual(link.resolve(), source / filename)
        for filename, basename in (
            ("another/book.epub", "book.epub"),
            ("draft?.epub", "draft_.epub"),
        ):
            link = output / "book_edition/edition/primary" / files[filename] / basename
            self.assertEqual(link.resolve(), source / filename)
        self.assertEqual(
            (
                output
                / "custom_research_notes/notes/custom_transcript"
                / files["notes.txt"]
                / "notes.txt"
            ).resolve(),
            source / "notes.txt",
        )
        self.app.put_item(
            "movie", {}, {"title": "Corrected title", "year": 2026}, "movie"
        )
        self.assertEqual(layouts.run("native", "native", apply=True)["changes"], [])
        self.assertEqual(Reconciler(self.app).apply("native")["applied"], [])
        with self.store.transaction():
            exporter = Manifest(self.app)
            document = exporter.build("native")
            exporter.write(document, in_catalog=True)
        saved = json.loads((output / FILENAME).read_text())
        self.assertEqual(
            saved["content"]["layout"]["current_definition"]["profile"],
            {"id": "catabolic.native", "version": 1},
        )
        self.assertTrue(saved["content"]["layout"]["definition_matches_last_apply"])
        self.assertEqual(saved["content"]["counts"]["entries"], 17)
        self.assertTrue(saved["content"]["relationships"])
        self.assertTrue(Reconciler(self.app).verify("native")["healthy"])
        self.assertEqual(
            before,
            {
                p: (p.stat().st_ino, hashlib.sha256(p.read_bytes()).hexdigest())
                for p in before
            },
        )

    def test_public_cli_default_folder_and_query_selection(self):
        query = self.root / "books.sql"
        query.write_text(
            "SELECT item_id FROM catalog_items WHERE kind = 'book_edition'"
        )
        self.store.close()
        prefix = [sys.executable, "-m", "catabolic", "--db", str(self.path), "--json"]
        for args in (
            ["layout", "presets"],
            ["catalog", "bind", "native"],
            [
                "layout",
                "put",
                "native",
                "--preset",
                "catabolic",
                "--select-sql",
                str(query),
            ],
            ["layout", "preview", "native", "--catalog", "native"],
            ["layout", "apply", "native", "--catalog", "native"],
            ["sync", "--catalog", "native", "--dry-run"],
            ["sync", "--catalog", "native"],
            ["verify", "--catalog", "native"],
            ["manifest", "--catalog", "native", "--in-catalog"],
            [
                "spec",
                "validate",
                "--file",
                str(self.root / "catabolic/native" / FILENAME),
            ],
        ):
            result = subprocess.run(
                [*prefix, *args], cwd=self.root, capture_output=True, text=True
            )
            self.assertEqual(result.returncode, 0, (args, result.stdout, result.stderr))
            self.assertIsInstance(json.loads(result.stdout), dict)
        output = self.root / "catabolic/native"
        self.assertEqual(len([p for p in output.rglob("*") if p.is_symlink()]), 2)

    def test_profile_version_and_rules_cannot_be_mislabeled(self):
        for profile in (
            None,
            {"id": "unknown", "version": 1},
            {"id": "catabolic.native", "version": True},
            {"id": "catabolic.native", "version": 2},
        ):
            with (
                self.subTest(profile=profile),
                self.assertRaisesRegex(CatabolicError, "unsupported layout profile"),
            ):
                validate_layout(
                    {**copy.deepcopy(PRESETS["catabolic"]), "profile": profile}
                )
        modified = copy.deepcopy(PRESETS["catabolic"])
        modified["rules"][0]["path"] = "Changed/{file.name}"
        with self.assertRaisesRegex(CatabolicError, "exact naming rules"):
            validate_layout(modified)
        del modified["profile"]
        validate_layout(modified)
