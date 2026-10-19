# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native reviewed remount repair against Python on disposable source roots."""

import shutil
import sqlite3
import tempfile
import unittest
from pathlib import Path

from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.remount import recover_device_numbers, repair
from catabolic.store import Store
from tests.test_rust_migration import BINARY
from tests.test_rust_observation_filesystem import native


@unittest.skipUnless(BINARY.is_file(), "build catabolic-cli before qualification")
class NativeRemountTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.database = self.root / "native.sqlite3"
        self.reference = self.root / "python.sqlite3"
        self.source = self.root / "source"
        self.source.mkdir()
        (self.source / "one.mkv").write_bytes(b"original source bytes")
        Store.initialize(self.database)
        with Store(self.database, writable=True) as store:
            app = Application(store)
            app.bind("source", "media", str(self.source))
            app.scan("media")
        shutil.copyfile(self.database, self.reference)

    def call(self, **args):
        return native(self.database, "observe", "remount", **args)

    def python(self, **args):
        with Store(self.reference, writable=True) as store:
            return repair(Application(store), **args)

    def facts(self, path):
        with sqlite3.connect(path) as db:
            return {
                table: db.execute("SELECT * FROM " + table).fetchall()
                for table in (
                    "bindings",
                    "binding_volumes",
                    "observations",
                    "remount_guard",
                )
            }

    def test_reviewed_named_replacement_has_exact_preview_digest_and_atomic_state(self):
        self.source.rename(self.root / "old-source")
        self.source.mkdir()
        (self.source / "one.mkv").write_bytes(b"replacement source bytes")
        args = {"trust_sources": ["media"]}
        expected = self.python(**args)
        actual = self.call(**args)
        self.assertEqual(actual, expected)
        before = self.facts(self.database)
        with self.assertRaisesRegex(CatabolicError, "preview changed"):
            self.call(**args, apply=True, expected_plan="stale")
        self.assertEqual(before, self.facts(self.database))
        self.assertEqual(
            self.call(**args, apply=True, expected_plan=actual["plan_id"]),
            self.python(**args, apply=True, expected_plan=expected["plan_id"]),
        )
        self.assertEqual(self.facts(self.database), self.facts(self.reference))
        with sqlite3.connect(self.database) as db:
            self.assertEqual(
                db.execute("SELECT count(*) FROM remount_repairs").fetchone()[0], 1
            )
        self.assertEqual(
            (self.root / "old-source/one.mkv").read_bytes(), b"original source bytes"
        )
        self.assertEqual(
            (self.source / "one.mkv").read_bytes(), b"replacement source bytes"
        )

    def test_preview_root_replacement_invalidates_reviewed_plan(self):
        preview = self.call(trust_sources=["media"])
        self.source.rename(self.root / "old-source")
        self.source.mkdir()
        before = self.facts(self.database)
        with self.assertRaisesRegex(CatabolicError, "preview changed"):
            self.call(
                trust_sources=["media"], apply=True, expected_plan=preview["plan_id"]
            )
        self.assertEqual(before, self.facts(self.database))

    def test_invalid_trust_and_inventory_combinations_match_reference_without_mutation(
        self,
    ):
        before = self.facts(self.database)
        for args in (
            {"trust_sources": ["unknown"]},
            {"trust_sources": ["media"], "inventory_recovery": True},
            {"adopt_existing": True, "inventory_recovery": True},
        ):
            with self.assertRaises(CatabolicError) as expected:
                self.python(**args)
            with self.assertRaises(CatabolicError) as actual:
                self.call(**args)
            self.assertEqual(str(actual.exception), str(expected.exception))
        self.assertEqual(before, self.facts(self.database))

    def test_automatic_noop_and_skipped_device_policy_match_reference(self):
        self.assertIsNone(native(self.database, "observe", "recover_device_numbers"))
        native(
            self.database, "observe", "trust", source="media", policy="path", apply=True
        )
        with Store(self.reference, writable=True) as store:
            from catabolic.source_trust import configure

            configure(Application(store), "media", "path", apply=True)
        for path in (self.database, self.reference):
            with sqlite3.connect(path) as db:
                db.execute("UPDATE bindings SET device=device+100")
        before = self.facts(self.database)
        with Store(self.reference, writable=True) as store:
            self.assertIsNone(recover_device_numbers(Application(store)))
        self.assertIsNone(native(self.database, "observe", "recover_device_numbers"))
        self.assertEqual(before, self.facts(self.database))

    def test_automatic_repair_cannot_adopt_unknown_volume(self):
        for path in (self.database, self.reference):
            with sqlite3.connect(path) as db:
                db.execute("UPDATE bindings SET device=device+100")
                db.execute("DELETE FROM binding_volumes")
        before = self.facts(self.database)
        with Store(self.reference, writable=True) as store:
            with self.assertRaises(CatabolicError) as expected:
                recover_device_numbers(Application(store))
        with self.assertRaises(CatabolicError) as actual:
            native(self.database, "observe", "recover_device_numbers")
        self.assertEqual(str(actual.exception), str(expected.exception))
        self.assertEqual(before, self.facts(self.database))

    def test_untrusted_replacement_is_refused_without_changing_observations(self):
        self.source.rename(self.root / "old-source")
        self.source.mkdir()
        before = self.facts(self.database)
        with self.assertRaises(CatabolicError) as expected:
            self.python()
        with self.assertRaises(CatabolicError) as actual:
            self.call()
        self.assertEqual(str(actual.exception), str(expected.exception))
        self.assertEqual(before, self.facts(self.database))
