# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Read-only naming, relationship, selection, collision and copy-policy parity."""

import copy
import hashlib
import json
import unittest

from catabolic.app import Application
from catabolic.layouts import PRESETS, Layouts
from catabolic.store import Store
from tests import test_layouts as fixtures
from tests import test_rust_queries as runner


class NativeLayoutTest(unittest.TestCase):
    native = runner.NativeQueryTest.native
    close_worker = runner.NativeQueryTest.close_worker

    def setUp(self):
        import subprocess

        from tests.test_rust_migration import BINARY

        fixtures.LayoutTest.setUp(self)
        for name, definition in PRESETS.items():
            self.layouts.put(name, copy.deepcopy(definition))
        self.store.close()
        self.worker = subprocess.Popen(
            [str(BINARY), "--stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(self.close_worker)

    def compare(self, identifier, **options):
        before = self.path.read_bytes()
        sources = {
            str(p): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in (self.root / "media").iterdir()
            if p.is_file()
        }
        with Store(self.path) as store:
            expected = Layouts(Application(store)).run(identifier, "custom", **options)
        actual = self.native(
            "layout-plan", identifier, "custom", "--options", json.dumps(options)
        )
        self.assertEqual(actual, expected)
        self.assertEqual(before, self.path.read_bytes())
        self.assertEqual(
            sources,
            {
                str(p): hashlib.sha256(p.read_bytes()).hexdigest()
                for p in (self.root / "media").iterdir()
                if p.is_file()
            },
        )
        self.assertEqual(list((self.root / "custom").iterdir()), [])

    def test_all_presets_and_relationships(self):
        for name in PRESETS:
            with self.subTest(preset=name):
                self.compare(name, limit=2)

    def test_selection_owned_changes_and_empty_guard(self):
        with Store(self.path, writable=True) as store:
            layouts = Layouts(Application(store))
            layouts.run("flat", "custom", apply=True)
            definition = copy.deepcopy(PRESETS["flat"])
            definition["selection"] = {
                "language": "sql",
                "query": "SELECT id AS item_id FROM items WHERE kind='movie'",
                "page_size": 1,
            }
            layouts.put("flat", definition)
        self.compare("flat")
        with Store(self.path, writable=True) as store:
            definition["selection"]["query"] = "SELECT id AS item_id FROM items WHERE 0"
            Layouts(Application(store)).put("flat", definition)
        self.compare("flat")
        self.compare("flat", allow_empty=True)

    def test_copy_policy_and_first_matching_rules(self):
        from catabolic.copy_selection import CopySelection

        with Store(self.path, writable=True) as store:
            app = Application(store)
            CopySelection(app).put(
                "custom",
                {
                    "prefer": [{"field": "size", "order": "desc"}],
                    "tie_break": "file_id",
                },
            )
        self.compare("flat")
        self.compare("plex")

    def test_collision_missing_fields_and_unicode_paths(self):
        definitions = (
            {"version": 1, "rules": [{"name": "all", "path": "same.mkv"}]},
            {
                "version": 1,
                "rules": [
                    {"name": "all", "path": "{item.metadata.missing}/{file.name}"}
                ],
            },
            {
                "version": 1,
                "normalization": "ascii",
                "rules": [{"name": "all", "path": "{item.kind}/{item.id}/{file.name}"}],
            },
        )
        for definition in definitions:
            with self.subTest(definition=definition):
                with Store(self.path, writable=True) as store:
                    Layouts(Application(store)).put("test", definition)
                self.compare("test")


class NativeRenditionLayoutTest(unittest.TestCase):
    native = runner.NativeQueryTest.native
    close_worker = runner.NativeQueryTest.close_worker

    def setUp(self):
        import subprocess

        from catabolic.receipts import import_receipt
        from catabolic.rendition_publication import Publication
        from tests import test_rendition_workflows as receipts
        from tests.test_rust_migration import BINARY

        receipts.ReceiptTest.setUp(self)
        folder = self.root / "links"
        folder.mkdir()
        self.app.bind("output", "external", str(folder))
        self.receipt = receipts.ReceiptTest.receipt
        self.output = import_receipt(self.app, self.receipt(self))["output_ids"][0]
        Publication(self.app).put("external", {"purpose": "custom"})
        Layouts(self.app).put("flat", copy.deepcopy(PRESETS["flat"]))
        self.store.close()
        self.worker = subprocess.Popen(
            [str(BINARY), "--stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(self.close_worker)

    def compare(self):
        before = self.path.read_bytes()
        with Store(self.path) as store:
            expected = Layouts(Application(store)).run("flat", "external")
        self.assertEqual(self.native("layout-plan", "flat", "external"), expected)
        self.assertEqual(before, self.path.read_bytes())

    def test_receipt_admission_rejection_and_stale_evidence(self):
        from catabolic.rendition_publication import Publication

        self.compare()
        with Store(self.path, writable=True) as store:
            Publication(Application(store)).decide(
                "external", self.output, True, "Not wanted"
            )
        self.compare()
        with Store(self.path, writable=True) as store:
            Publication(Application(store)).decide(
                "external", self.output, False, "Reviewed"
            )
        (self.root / "media/source.mkv").write_bytes(b"changed source")
        self.compare()
