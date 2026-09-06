import copy
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.layouts import PRESETS, Layouts
from catabolic.manifest import FILENAME, Manifest, digest
from catabolic.reconcile import Reconciler
from catabolic.store import Store
from tests.test_media_model import populate_media


class ManifestTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.path = self.root / "catalog.sqlite3"
        Store.initialize(self.path)
        self.store = Store(self.path, writable=True)
        self.addCleanup(self.store.close)
        self.app = Application(self.store)
        self.files, _ = populate_media(self.app, self.root)
        self.exporter = Manifest(self.app)

    def build(self, **kwargs):
        with self.store.transaction():
            return self.exporter.build(**kwargs)

    def write(self, *, catalog="global", extra=None, **kwargs):
        with self.store.transaction():
            document = self.exporter.build(catalog, extra=extra)
            return self.exporter.write(document, **kwargs)

    def test_complete_metadata_snapshot_is_readonly_and_preserves_custom_fields(self):
        self.app.put_item(
            "movie",
            {"local.movie": "movie"},
            {"title": "Café", "year": 2024, "custom": {"nested": [1, None, True]}},
            "movie",
        )
        before = self.path.read_bytes()
        with Store(self.path) as reader:
            document = Manifest(Application(reader)).build(
                extra={"curated_by": "local agent"}
            )
        self.assertEqual(before, self.path.read_bytes())
        self.assertEqual(document["content_sha256"], digest(document["content"]))
        content = document["content"]
        self.assertEqual(content["counts"]["entries"], 14)
        self.assertEqual(content["counts"]["files"], 14)
        self.assertFalse(content["filesystem_verified"])
        movie = next(item for item in content["items"] if item["id"] == "movie")
        self.assertEqual(movie["metadata"]["custom"], {"nested": [1, None, True]})
        self.assertEqual(
            movie["identities"], [{"namespace": "local.movie", "value": "movie"}]
        )
        self.assertEqual(content["extra"]["curated_by"], "local agent")
        file = next(
            file for file in content["files"] if file["id"] == self.files["movie.mkv"]
        )
        self.assertIsInstance(file["mtime_ns"], str)
        self.assertEqual(file["status"], "present")
        self.assertFalse(content["entries"][0]["recorded_link_matches_desired"])
        self.assertEqual(list((self.root / "library").iterdir()), [])

    def test_outgoing_relationship_closure_and_query_layout_provenance(self):
        (self.root / "tracks").mkdir()
        self.app.bind("output", "tracks", str(self.root / "tracks"))
        definition = {
            **copy.deepcopy(PRESETS["flat"]),
            "selection": {
                "language": "sql",
                "query": "SELECT item_id FROM catalog_items WHERE kind='track'",
            },
        }
        layouts = Layouts(self.app)
        layouts.put("tracks", definition)
        layouts.run("tracks", "tracks", apply=True)
        content = self.build(catalog="tracks")["content"]
        self.assertEqual(
            {item["id"] for item in content["items"]},
            {"track1", "track2", "album", "artist"},
        )
        self.assertEqual(len(content["relationships"]), 3)
        self.assertEqual(len(content["files"]), 2)
        self.assertTrue(content["layout"]["definition_matches_last_apply"])
        definition["rules"][0]["path"] = "Changed/{file.name}"
        layouts.put("tracks", definition)
        updated = self.build(catalog="tracks")["content"]
        self.assertFalse(updated["layout"]["definition_matches_last_apply"])
        self.assertEqual(updated["entries"], content["entries"])
        self.assertEqual(updated["layout"]["current_definition"], definition)

    def test_in_catalog_publication_repeat_refresh_and_sync_compatibility(self):
        before_sources = {
            str(p): [p.stat().st_ino, hashlib.sha256(p.read_bytes()).hexdigest()]
            for p in (self.root / "media").iterdir()
        }
        with self.assertRaisesRegex(CatabolicError, "sync"):
            self.write(in_catalog=True)
        self.assertTrue(Reconciler(self.app).apply()["healthy"])
        links = {
            str(p): p.lstat().st_ino
            for p in (self.root / "library").rglob("*")
            if p.is_symlink()
        }
        result = self.write(in_catalog=True)
        target = self.root / "library" / FILENAME
        self.assertTrue(result["written"])
        before = (target.stat().st_ino, target.read_bytes())
        self.assertTrue(self.write(in_catalog=True)["unchanged"])
        self.assertEqual(before, (target.stat().st_ino, target.read_bytes()))
        self.assertEqual(Reconciler(self.app).apply()["applied"], [])
        self.assertTrue(Reconciler(self.app).verify()["healthy"])
        self.app.put_item("movie", {}, {"title": "Changed"}, "movie")
        with self.assertRaisesRegex(CatabolicError, "replace"):
            self.write(in_catalog=True)
        self.assertEqual(before, (target.stat().st_ino, target.read_bytes()))
        self.assertTrue(self.write(in_catalog=True, replace=True)["written"])
        self.assertTrue(Reconciler(self.app).verify()["healthy"])
        self.assertEqual(
            links,
            {
                str(p): p.lstat().st_ino
                for p in (self.root / "library").rglob("*")
                if p.is_symlink()
            },
        )
        self.assertEqual(
            before_sources,
            {
                str(p): [p.stat().st_ino, hashlib.sha256(p.read_bytes()).hexdigest()]
                for p in (self.root / "media").iterdir()
            },
        )

    def test_external_export_refuses_foreign_edited_and_hardlinked_files(self):
        target = self.root / "metadata.json"
        target.write_text('{"unrelated":true}')
        with self.assertRaises(CatabolicError):
            self.write(output=target, replace=True)
        self.assertEqual(target.read_text(), '{"unrelated":true}')
        own = self.root / "owned.json"
        self.write(output=own)
        valid = own.read_bytes()
        changed = json.loads(valid)
        changed["content"]["extra"] = {"user_notes": "keep"}
        own.write_text(json.dumps(changed))
        with self.assertRaises(CatabolicError):
            self.write(output=own, replace=True)
        self.assertEqual(
            json.loads(own.read_bytes())["content"]["extra"], {"user_notes": "keep"}
        )
        own.write_bytes(valid)
        os.link(own, self.root / "hardlink.json")
        with self.assertRaisesRegex(CatabolicError, "single-link"):
            self.write(output=own, replace=True)

    def test_destination_guards_sources_other_outputs_symlinks_and_database(self):
        (self.root / "alias").symlink_to(self.root / "media", target_is_directory=True)
        target = self.root / "link.json"
        target.symlink_to(self.root / "media/movie.mkv")
        for destination in (
            self.root / "media/new.json",
            self.root / "alias/new.json",
            target,
            self.path,
            Path(str(self.path) + ".lock"),
            self.root / "library/metadata.json",
            self.root / ".catabolic-owner.json",
        ):
            with (
                self.subTest(destination=destination),
                self.assertRaises((CatabolicError, OSError)),
            ):
                self.write(output=destination, replace=True)
        self.assertFalse((self.root / "media/new.json").exists())
        self.assertTrue(target.is_symlink())

    def test_inode_guard_recognizes_source_when_lexical_spelling_changes(self):
        moved = self.root / "renamed-source"
        (self.root / "media").rename(moved)
        with self.assertRaisesRegex(CatabolicError, "source tree"):
            self.write(output=moved / "metadata.json")
        self.assertFalse((moved / "metadata.json").exists())

    def test_file_fsync_failure_leaves_old_manifest_and_removes_temp(self):
        target = self.root / "metadata.json"
        self.write(output=target)
        before = target.read_bytes()
        with (
            patch("catabolic.manifest.os.fsync", side_effect=OSError("injected fsync")),
            self.assertRaises(OSError),
        ):
            self.write(output=target, replace=True, extra={"changed": True})
        self.assertEqual(before, target.read_bytes())
        self.assertEqual(list(self.root.glob(".catabolic-manifest-*")), [])

    def test_limits_and_invalid_extra_produce_no_partial_file(self):
        target = self.root / "metadata.json"
        with (
            patch("catabolic.manifest.MAX_RECORDS", 1),
            self.assertRaises(CatabolicError),
        ):
            self.write(output=target)
        with (
            patch("catabolic.manifest.MAX_BYTES", 100),
            self.assertRaises(CatabolicError),
        ):
            self.write(output=target)
        with self.assertRaises(CatabolicError):
            self.build(extra=[])
        self.assertFalse(target.exists())

    def test_profile_specific_observations_and_pending_operations(self):
        self.app.add_profile("offline")
        with self.store.transaction():
            content = Manifest(Application(self.store, "offline")).build()["content"]
        self.assertIsNone(content["catalog"]["output_root"])
        self.assertTrue(
            all(
                file["status"] == "unknown" and file["source_path"] is None
                for file in content["files"]
            )
        )
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO journal VALUES ('pending','default','global','pending','create','target',NULL)"
            )
        with self.assertRaisesRegex(CatabolicError, "pending"):
            self.build()

    def test_custom_relationship_cycles_terminate(self):
        self.app.media.relate("track1", "track2", "custom:related")
        self.app.media.relate("track2", "track1", "custom:related")
        content = self.build()["content"]
        self.assertEqual(
            len({item["id"] for item in content["items"]}), len(content["items"])
        )
        self.assertEqual(
            sum(row["kind"] == "custom:related" for row in content["relationships"]), 2
        )

    def test_cli_stdout_and_file_are_json_without_global_flag(self):
        self.store.close()
        prefix = [sys.executable, "-m", "catabolic", "--db", str(self.path), "manifest"]
        result = subprocess.run(prefix, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["format_version"], 3)
        result = subprocess.run(
            [
                *prefix,
                "--output",
                str(self.root / "manifest.json"),
                "--extra",
                '{"purpose":"demo"}',
            ],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(json.loads(result.stdout)["written"])
        self.assertEqual(
            json.loads((self.root / "manifest.json").read_text())["content"]["extra"],
            {"purpose": "demo"},
        )
