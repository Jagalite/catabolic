# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native manifest snapshots compare against the unchanged Python builder."""

import copy
import json
import os
import re
import subprocess
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path

from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.layouts import PRESETS, Layouts
from catabolic.manifest import FILENAME, Manifest
from catabolic.operations import Operations
from catabolic.projection_stats import save
from catabolic.projections import Projections
from catabolic.reconcile import Reconciler
from catabolic.rules import Rules
from catabolic.saved_queries import Queries
from catabolic.store import Store, encode
from catabolic.tagging import Tagging
from tests.test_media_model import populate_media
from tests.test_rust_migration import BINARY


@unittest.skipUnless(BINARY.is_file(), "build catabolic-cli before qualification")
def crate_version():
    """The workspace version the native engine stamps as generator.version."""
    text = (Path(__file__).resolve().parents[1] / "Cargo.toml").read_text()
    return re.search(
        r'^\[workspace\.package\][^\[]*?^version = "([^"]+)"', text, re.M | re.S
    ).group(1)


class NativeManifestTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.path = self.root / "catalog.sqlite3"
        Store.initialize(self.path)
        with Store(self.path, writable=True) as store:
            app = Application(store)
            self.files, _ = populate_media(app, self.root)

    def native(self, catalog="global", *, ok=True, **options):
        reply = subprocess.run(
            [
                str(BINARY),
                "--db",
                str(self.path),
                "--json",
                "--options",
                json.dumps({"catalog": catalog, **options}),
                "manifest-build",
            ],
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(reply.returncode, 0 if ok else 2, reply.stderr)
        return (
            json.loads(reply.stdout)
            if ok
            else reply.stderr.strip().removeprefix("catabolic: ")
        )

    def compare(self, catalog="global", **options):
        before = self.path.read_bytes()
        with Store(self.path) as store:
            expected = Manifest(Application(store)).build(catalog, **options)
        actual = self.native(catalog, **options)
        for result in (expected, actual):
            self.assertLess(
                abs(
                    (
                        datetime.now(timezone.utc)
                        - datetime.fromisoformat(result.pop("generated_at"))
                    ).total_seconds()
                ),
                120,
            )
        # The native engine is the 1.0.0 distribution and stamps its own
        # version; the frozen oracle stamps 0.2.0. Everything else must match.
        self.assertEqual(expected["generator"]["version"], "0.2.0")
        version = actual["generator"].pop("version")
        self.assertEqual(version, crate_version())
        expected["generator"].pop("version")
        self.assertEqual(actual, expected)
        actual["generator"]["version"] = version
        self.assertEqual(before, self.path.read_bytes())
        return actual

    def publish(self, document, *, ok=True, **options):
        reply = subprocess.run(
            [
                str(BINARY),
                "--db",
                str(self.path),
                "--json",
                "--options",
                json.dumps({"document": document, **options}),
                "manifest-write",
            ],
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(reply.returncode, 0 if ok else 2, reply.stderr)
        return (
            json.loads(reply.stdout)
            if ok
            else reply.stderr.strip().removeprefix("catabolic: ")
        )

    def refused(self, document, **options):
        with Store(self.path, writable=True) as store:
            with store.transaction(), self.assertRaises(CatabolicError) as expected:
                Manifest(Application(store)).write(document, **options)
        return self.assertEqual(
            self.publish(document, ok=False, **options), str(expected.exception)
        )

    def test_publication_exact_bytes_unchanged_refresh_and_permissions(self):
        document = self.native(extra={"vendor": {"precise": 2**80}})
        target = self.root / "native.json"
        reference = self.root / "python.json"
        before_database = self.path.read_bytes()
        with Store(self.path, writable=True) as store:
            with store.transaction():
                expected = Manifest(Application(store)).write(
                    document, output=reference
                )
        actual = self.publish(document, output=str(target))
        expected["output"] = str(target)
        self.assertEqual(actual, expected)
        self.assertEqual(target.read_bytes(), reference.read_bytes())
        self.assertEqual(target.read_text(), encode(document) + "\n")
        self.assertEqual(target.stat().st_mode & 0o777, 0o600)
        identity = (target.stat().st_ino, target.read_bytes())
        repeated = self.native(extra={"vendor": {"precise": 2**80}})
        self.assertTrue(self.publish(repeated, output=str(target))["unchanged"])
        self.assertEqual(identity, (target.stat().st_ino, target.read_bytes()))
        changed = self.native(extra={"changed": True})
        self.refused(changed, output=str(target))
        self.assertTrue(
            self.publish(changed, output=str(target), replace=True)["written"]
        )
        self.assertEqual(target.read_text(), encode(changed) + "\n")
        self.assertEqual(before_database, self.path.read_bytes())

    def test_publication_owned_catalog_and_exact_reserved_path(self):
        document = self.native()
        self.refused(document, in_catalog=True)
        with Store(self.path, writable=True) as store:
            self.assertTrue(Reconciler(Application(store)).apply()["healthy"])
        document = self.native()
        target = self.root / "library" / FILENAME
        result = self.publish(document, in_catalog=True)
        self.assertEqual(result["output"], str(target))
        self.assertTrue(result["written"])
        identity = (target.stat().st_ino, target.read_bytes())
        self.assertTrue(self.publish(document, output=str(target))["unchanged"])
        self.assertEqual(identity, (target.stat().st_ino, target.read_bytes()))
        with Store(self.path, writable=True) as store:
            self.assertTrue(Reconciler(Application(store)).verify()["healthy"])

    def test_publication_refuses_foreign_edited_and_hardlinked_content(self):
        document = self.native()
        target = self.root / "native.json"
        for value in (
            {"unrelated": True},
            {**document, "format_version": 999},
            {**document, "content_sha256": "0" * 64},
        ):
            target.write_text(json.dumps(value))
            before = target.read_bytes()
            self.refused(document, output=str(target), replace=True)
            self.assertEqual(target.read_bytes(), before)
        target.write_text(encode(document) + "\n")
        os.link(target, self.root / "second.json")
        self.refused(document, output=str(target), replace=True)

    def test_publication_guards_all_bindings_and_renamed_ancestors(self):
        document = self.native()
        for path in (
            self.root / "media/new.json",
            self.path,
            Path(str(self.path) + ".lock"),
            Path(str(self.path) + "-wal"),
            self.root / "library/metadata.json",
            self.root / ".catabolic-owner.json",
            self.root / "media/../escaped.json",
        ):
            with self.subTest(path=path):
                self.refused(document, output=str(path), replace=True)
        alias = self.root / "alias"
        alias.symlink_to(self.root / "media", target_is_directory=True)
        link = self.root / "link.json"
        link.symlink_to(self.root / "media/movie.mkv")
        for path in (alias / "new.json", link):
            self.publish(document, output=str(path), replace=True, ok=False)
        moved = self.root / "renamed"
        (self.root / "media").rename(moved)
        self.refused(document, output=str(moved / "new.json"))
        self.assertFalse((moved / "new.json").exists())
        self.assertTrue(link.is_symlink())
        self.assertFalse((self.root / "escaped.json").exists())

    def test_publication_faults_before_commit_preserve_owned_export(self):
        target = self.root / "native.json"
        document = self.native()
        self.publish(document, output=str(target))
        identity = (target.stat().st_ino, target.read_bytes())
        changed = self.native(extra={"changed": True})
        for stage in ("manifest-before-file-sync", "manifest-before-publish"):
            error = self.publish(
                changed, output=str(target), replace=True, failpoint=stage, ok=False
            )
            self.assertIn("injected filesystem failure", error)
            self.assertEqual(identity, (target.stat().st_ino, target.read_bytes()))
            self.assertEqual(list(self.root.glob(".catabolic-manifest-*")), [])

    def test_full_manifest_outgoing_closure_and_source_paths(self):
        self.compare(extra={"vendor:data": {"precise": 2**80, "fraction": 1e-7}})

    def test_selected_catalog_and_outside_selection_refusal(self):
        self.compare(
            selection={
                "language": "sql",
                "query": "SELECT item_id FROM catalog_items WHERE kind='track'",
            }
        )
        with Store(self.path, writable=True) as store:
            app = Application(store)
            app.put_item("movie", {}, {}, "unmapped")
            app.media.associate(self.files["movie.mkv"], "unmapped")
        options = {
            "selection": {
                "language": "sql",
                "query": "SELECT item_id FROM catalog_items WHERE item_id='unmapped'",
            }
        }
        with Store(self.path) as store:
            with self.assertRaises(CatabolicError) as expected:
                Manifest(Application(store)).build(**options)
        self.assertEqual(self.native(ok=False, **options), str(expected.exception))

    def test_layout_projection_and_rule_statistics(self):
        with Store(self.path, writable=True) as store:
            app = Application(store)
            layout = Layouts(app)
            layout.put("flat", copy.deepcopy(PRESETS["flat"]))
            query = Queries(store).put(
                "all",
                {
                    "selection": {
                        "language": "sql",
                        "query": "SELECT item_id FROM catalog_items",
                    }
                },
            )
            Projections(app).put("global", query["id"], "flat")
            layout.run("flat", apply=True)
            operation = Operations(app).put(
                "hash", {"kind": "analysis", "operation": "hash"}
            )
            rule = Rules(app).put(
                "audit", operation["id"], None, {"query_id": query["id"]}
            )
            save(app, "rule", rule["id"], {"scope": "test", "counts": {"satisfied": 1}})
            save(
                app,
                "execution",
                "global",
                {"status": "complete", "verification": {"healthy": True}},
            )
        self.compare()

    def test_tag_closure_aliases_and_withdrawn_assertions(self):
        with Store(self.path, writable=True) as store:
            app = Application(store)
            tags = Tagging(app)
            tags.put("Genre: Fiction")
            tags.put("Science Fiction")
            tags.alias("Science Fiction", "sci-fi")
            tags.parent("science fiction", "genre:fiction")
            tags.assign(["sci-fi"], items=["movie"], source="fixture", confidence=0.5)
            tags.assign(["sci-fi"], files=[self.files["movie.mkv"]], source="fixture")
            tags.assign(["sci-fi"], items=["movie"], source="fixture", remove=True)
        self.compare()

    def test_unbound_and_empty_catalog_and_reserved_metadata(self):
        with Store(self.path, writable=True) as store:
            with store.transaction() as db:
                db.execute("DELETE FROM bindings WHERE kind='source'")
                db.execute("INSERT INTO catalogs VALUES ('empty')")
        self.compare()
        self.compare("empty")
        self.compare(extra=None, selection=None)
        for options in ({"extra": []}, {"extra": {"catabolic:statistics": {}}}):
            with Store(self.path) as store:
                with self.assertRaises(CatabolicError) as expected:
                    Manifest(Application(store)).build(**options)
            self.assertEqual(self.native(ok=False, **options), str(expected.exception))
