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
from catabolic.filesystem import MARKER
from catabolic.reconcile import Reconciler
from catabolic.store import Store


class Interrupted(RuntimeError):
    pass


class CatalogTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.source = self.root / "source"
        self.output = self.root / "plex"
        self.source.mkdir()
        self.output.mkdir()
        self.media = self.source / "example.mkv"
        self.media.write_bytes(b"fixture media\n")
        self.database = self.root / "catalog.sqlite3"
        Store.initialize(self.database)
        self.store = Store(self.database, writable=True)
        self.addCleanup(self.store.close)
        self.app = Application(self.store)
        self.app.bind("source", "media", str(self.source))
        self.app.bind("output", "global", str(self.output))
        self.app.scan()
        self.file_id = self.app.files()["files"][0]["id"]
        self.item_id = self.app.put_item(
            "movie", {"fixture": "movie-1"}, {"title": "Example"}
        )["id"]
        self.mapping = self.app.put_mapping(
            "global", self.file_id, self.item_id, "Movies/Example/Example.mkv"
        )
        self.link = self.output / self.mapping["path"]
        self.reconciler = Reconciler(self.app)

    def test_end_to_end_and_repeat_preserves_source_and_link(self):
        before = self.media.stat()
        digest = hashlib.sha256(self.media.read_bytes()).digest()
        result = self.reconciler.apply()
        self.assertTrue(result["healthy"], result)
        self.assertEqual(self.link.resolve(), self.media)
        self.assertFalse(os.path.isabs(os.readlink(self.link)))
        link_before = self.link.lstat()
        self.assertEqual(self.reconciler.apply()["applied"], [])
        self.assertEqual(self.link.lstat().st_ino, link_before.st_ino)
        self.assertEqual(self.link.lstat().st_mtime_ns, link_before.st_mtime_ns)
        self.assertEqual(self.media.stat().st_mtime_ns, before.st_mtime_ns)
        self.assertEqual(self.media.stat().st_ino, before.st_ino)
        self.assertEqual(hashlib.sha256(self.media.read_bytes()).digest(), digest)

    def test_preview_is_read_only(self):
        before = self.database.read_bytes()
        with Store(self.database) as store:
            result = Reconciler(Application(store)).preview()
        self.assertTrue(result["safe"])
        self.assertEqual(self.database.read_bytes(), before)
        self.assertEqual(list(self.output.iterdir()), [])

    def test_missing_without_scan_blocks_all_changes(self):
        self.reconciler.apply()
        self.media.unlink()
        self.assertFalse(self.reconciler.apply()["safe"])
        self.assertTrue(self.link.is_symlink())

    def test_confirmed_missing_removes_link_and_restores_mapping(self):
        self.reconciler.apply()
        self.media.unlink()
        self.app.scan()
        result = self.reconciler.apply()
        self.assertTrue(result["safe"])
        self.assertFalse(result["healthy"])
        self.assertFalse(self.link.is_symlink())
        self.assertEqual(self.store.rows("SELECT active FROM mappings")[0]["active"], 1)
        self.media.write_bytes(b"restored")
        self.assertFalse(self.reconciler.preview()["safe"])
        self.app.scan()
        self.assertTrue(self.reconciler.apply()["healthy"])

    def test_incomplete_scan_preserves_previous_inventory(self):
        self.reconciler.apply()
        self.media.unlink()
        with patch(
            "catabolic.app.walk_files", return_value=([], ["permission denied"])
        ):
            self.assertFalse(self.app.scan()["complete"])
        self.assertEqual(self.app.files()["files"][0]["status"], "present")
        self.assertFalse(self.reconciler.preview()["safe"])
        self.assertTrue(self.link.is_symlink())

    def test_changed_root_blocks_scan_and_sync(self):
        self.reconciler.apply()
        self.source.rename(self.root / "detached")
        self.source.mkdir()
        self.assertFalse(self.app.scan()["complete"])
        self.assertEqual(self.app.files()["files"][0]["status"], "present")
        self.assertFalse(self.reconciler.apply()["safe"])
        self.assertTrue(self.link.is_symlink())

    def test_missing_root_blocks_scan_and_sync(self):
        self.source.rename(self.root / "detached")
        self.assertFalse(self.app.scan()["complete"])
        self.assertFalse(self.reconciler.preview()["safe"])

    def test_source_symlink_is_not_followed(self):
        external = self.root / "private.mkv"
        external.write_bytes(b"private")
        (self.source / "alias.mkv").symlink_to(external)
        (self.source / "loop").symlink_to(self.source)
        self.assertTrue(self.app.scan()["complete"])
        self.assertEqual(len(self.app.files()["files"]), 1)

    def test_excluded_paths_are_not_traversed_or_marked_missing(self):
        folder = self.source / "excluded"
        folder.mkdir()
        tracked = folder / "tracked.mkv"
        tracked.write_bytes(b"fixture")
        self.app.scan()
        tracked.unlink()
        original_open = os.open

        def guarded_open(path, *args, **kwargs):
            if path == "excluded":
                raise AssertionError("excluded directory must not be opened")
            return original_open(path, *args, **kwargs)

        with patch("catabolic.filesystem.os.open", side_effect=guarded_open):
            result = self.app.scan(exclude=["excluded"])
        self.assertTrue(result["complete"])
        self.assertEqual(result["scans"][0]["excluded"], ["excluded"])
        file = next(
            row
            for row in self.app.files()["files"]
            if row["path"] == "excluded/tracked.mkv"
        )
        self.assertEqual(file["status"], "present")
        scope = self.store.rows(
            "SELECT value FROM meta WHERE key=?",
            (f"scan:{result['scans'][0]['scan_id']}:scope",),
        )[0]["value"]
        self.assertEqual(json.loads(scope), {"exclude": ["excluded"]})
        self.app.scan()
        file = next(
            row
            for row in self.app.files()["files"]
            if row["path"] == "excluded/tracked.mkv"
        )
        self.assertEqual(file["status"], "missing")

    def test_exclusion_is_literal_and_hidden_media_remains_visible(self):
        for folder in ("data_1", "dataX1", ".hidden-media"):
            (self.source / folder).mkdir()
            (self.source / folder / "movie.mkv").write_bytes(b"fixture")
        self.app.scan()
        for folder in ("data_1", "dataX1"):
            (self.source / folder / "movie.mkv").unlink()
        self.app.scan(exclude=["data_1"])
        files = {row["path"]: row["status"] for row in self.app.files()["files"]}
        self.assertEqual(files["data_1/movie.mkv"], "present")
        self.assertEqual(files["dataX1/movie.mkv"], "missing")
        self.assertEqual(files[".hidden-media/movie.mkv"], "present")

    def test_source_changed_requires_rescan(self):
        self.media.write_bytes(b"changed fixture")
        self.assertFalse(self.reconciler.preview()["safe"])
        self.app.scan()
        self.assertTrue(self.reconciler.apply()["healthy"])

    def test_source_is_revalidated_after_preflight(self):
        def change_after_prepare(operation):
            if operation["kind"] == "prepare":
                self.media.write_bytes(b"changed after the preflight")

        with self.assertRaises(CatabolicError):
            self.reconciler.apply(after_filesystem=change_after_prepare)
        self.assertFalse(self.link.is_symlink())

    def test_batch_apply_uses_linear_source_checks(self):
        from catabolic.filesystem import source_stat

        for index in range(20):
            self.app.put_mapping(
                "global", self.file_id, self.item_id, f"Copies/copy-{index}.mkv"
            )
        mapping_count = 21
        with patch("catabolic.reconcile.source_stat", wraps=source_stat) as observed:
            self.assertTrue(self.reconciler.apply()["healthy"])
        self.assertLessEqual(observed.call_count, 4 * mapping_count)

    def test_empty_source_is_blocked(self):
        self.media.write_bytes(b"")
        self.app.scan()
        result = self.reconciler.apply()
        self.assertTrue(result["safe"])
        self.assertFalse(result["healthy"])
        self.assertFalse(self.link.is_symlink())

    def test_unknown_output_is_never_claimed(self):
        sentinel = self.output / "keep.txt"
        sentinel.write_text("keep")
        self.assertFalse(self.reconciler.apply()["safe"])
        self.assertEqual(sentinel.read_text(), "keep")
        self.assertFalse((self.output / MARKER).exists())

    def test_wrong_ownership_blocks(self):
        self.reconciler.apply()
        marker = self.output / MARKER
        owner = json.loads(marker.read_text())
        owner["database_id"] = "unrelated"
        marker.write_text(json.dumps(owner))
        self.assertFalse(self.reconciler.apply()["safe"])
        self.assertFalse(self.reconciler.verify()["healthy"])

    def test_symlink_parent_blocks_without_touching_external_tree(self):
        self.reconciler.apply()
        self.link.unlink()
        self.link.parent.rmdir()
        outside = self.root / "outside"
        outside.mkdir()
        self.link.parent.symlink_to(outside, target_is_directory=True)
        self.assertFalse(self.reconciler.apply()["safe"])
        self.assertEqual(list(outside.iterdir()), [])

    def test_external_regular_file_is_not_replaced_or_removed(self):
        self.reconciler.apply()
        self.link.unlink()
        self.link.write_text("someone else's file")
        self.app.disable_mapping(self.mapping["id"])
        self.assertFalse(self.reconciler.apply()["safe"])
        self.assertEqual(self.link.read_text(), "someone else's file")

    def test_external_retargeted_symlink_is_not_removed(self):
        self.reconciler.apply()
        self.link.unlink()
        self.link.symlink_to("other")
        self.app.disable_mapping(self.mapping["id"])
        self.assertFalse(self.reconciler.apply()["safe"])
        self.assertEqual(os.readlink(self.link), "other")

    def test_repeated_item_and_mapping_are_idempotent(self):
        item = self.app.put_item("movie", {"fixture": "movie-1"}, {"title": "Example"})
        mapping = self.app.put_mapping(
            "global", self.file_id, item["id"], self.mapping["path"]
        )
        self.assertEqual(item["id"], self.item_id)
        self.assertEqual(mapping["id"], self.mapping["id"])
        self.assertEqual(
            self.store.rows("SELECT count(*) AS n FROM mappings")[0]["n"], 1
        )

    def test_conflicting_identities_are_not_merged(self):
        self.app.put_item("movie", {"other": "2"}, {})
        with self.assertRaises(CatabolicError):
            self.app.put_item("movie", {"fixture": "movie-1", "other": "2"}, {})
        self.assertEqual(self.store.rows("SELECT count(*) AS n FROM items")[0]["n"], 2)

    def test_series_identity_can_group_episode_occurrences(self):
        series = self.app.put_item(
            "series", {"fixture.series": "1"}, {"title": "Example Series"}
        )
        second = self.source / "second.mkv"
        second.write_bytes(b"second episode")
        self.app.scan()
        second_id = next(
            row["id"] for row in self.app.files()["files"] if row["path"] == second.name
        )
        for file_id, episode in ((self.file_id, "01"), (second_id, "02")):
            self.app.put_mapping(
                "global",
                file_id,
                series["id"],
                f"TV Shows/Example Series/Season 01/S01E{episode}.mkv",
            )
        self.assertTrue(self.reconciler.apply()["healthy"])

    def test_path_validation_and_prefix_collisions(self):
        for path in (
            "../escape",
            "/absolute",
            "a//b",
            "a/./b",
            "a/../b",
            ".catabolic-owner.json",
            "a\\b",
        ):
            with self.subTest(path=path), self.assertRaises(CatabolicError):
                self.app.put_mapping("global", self.file_id, self.item_id, path)
        with self.assertRaises(CatabolicError):
            self.app.put_mapping("global", self.file_id, self.item_id, "Movies")
        with self.assertRaises(CatabolicError):
            self.app.put_mapping(
                "global", self.file_id, self.item_id, self.mapping["path"] + "/child"
            )

    def test_pagination_and_catalog_scope(self):
        for index in range(7):
            (self.source / f"file-{index}.mkv").write_bytes(b"video")
        self.app.scan()
        ids = []
        cursor = None
        while True:
            page = self.app.files(limit=3, cursor=cursor)
            ids.extend(row["id"] for row in page["files"])
            cursor = page["next_cursor"]
            if cursor is None:
                break
        self.assertEqual(len(ids), 8)
        self.assertEqual(len(set(ids)), 8)
        other = self.root / "other-catalog"
        other.mkdir()
        self.app.bind("output", "other", str(other))
        self.assertEqual(len(self.app.files(unmapped=True)["files"]), 7)
        self.assertEqual(
            len(self.app.files(catalog="other", unmapped=True)["files"]), 8
        )
        cursor = self.app.files(limit=1)["next_cursor"]
        with self.assertRaises(CatabolicError):
            self.app.files(cursor=cursor, catalog="other")

    def test_profile_observations_are_isolated(self):
        self.app.add_profile("laptop")
        laptop = Application(self.store, "laptop")
        other = self.root / "laptop-source"
        other.mkdir()
        laptop.bind("source", "media", str(other))
        laptop.scan()
        self.assertEqual(self.app.files()["files"][0]["status"], "present")
        self.assertEqual(laptop.files()["files"][0]["status"], "missing")

    def test_overlap_is_rejected_in_both_directions(self):
        for kind, path in (
            ("output", self.source),
            ("output", self.root),
            ("source", self.output),
            ("source", self.root),
        ):
            with self.subTest(kind=kind, path=path), self.assertRaises(CatabolicError):
                self.app.bind(kind, "bad", str(path))

    def test_write_lock_rejects_second_writer(self):
        with self.assertRaises(CatabolicError):
            Store(self.database, writable=True)

    def interrupt(self, kind):
        def hook(operation):
            if operation["kind"] == kind:
                raise Interrupted(kind)

        return hook

    def test_recovery_after_claim_publication(self):
        with self.assertRaises(Interrupted):
            self.reconciler.apply(after_filesystem=self.interrupt("prepare"))
        self.assertFalse(self.reconciler.preview()["safe"])
        self.assertEqual(len(self.reconciler.recover()["recovered"]), 1)
        self.assertTrue(self.reconciler.apply()["healthy"])

    def test_recovery_after_link_creation_before_database_commit(self):
        with self.assertRaises(Interrupted):
            self.reconciler.apply(after_filesystem=self.interrupt("create"))
        self.assertTrue(self.link.is_symlink())
        self.assertEqual(self.store.rows("SELECT * FROM owned_links"), [])
        self.reconciler.recover()
        self.assertTrue(self.reconciler.verify()["healthy"])

    def test_recovery_before_link_creation(self):
        with patch(
            "catabolic.reconcile.os.symlink", side_effect=OSError("injected failure")
        ):
            with self.assertRaises(OSError):
                self.reconciler.apply()
        self.assertFalse(self.link.is_symlink())
        self.reconciler.recover()
        self.assertTrue(self.reconciler.verify()["healthy"])

    def test_recovery_refuses_external_collision(self):
        with patch(
            "catabolic.reconcile.os.symlink", side_effect=OSError("injected failure")
        ):
            with self.assertRaises(OSError):
                self.reconciler.apply()
        self.link.write_text("keep")
        with self.assertRaises(CatabolicError):
            self.reconciler.recover()
        self.assertEqual(self.link.read_text(), "keep")
        self.assertEqual(len(self.store.rows("SELECT * FROM journal")), 1)

    def test_recovery_after_removal(self):
        self.reconciler.apply()
        self.app.disable_mapping(self.mapping["id"])
        with self.assertRaises(Interrupted):
            self.reconciler.apply(after_filesystem=self.interrupt("remove"))
        self.assertFalse(self.link.is_symlink())
        self.reconciler.recover()
        self.assertTrue(self.reconciler.verify()["healthy"])

    def test_claim_recovers_before_marker_publication(self):
        with patch(
            "catabolic.filesystem.os.link", side_effect=OSError("interrupted publish")
        ):
            with self.assertRaises(OSError):
                self.reconciler.apply()
        self.assertFalse((self.output / MARKER).exists())
        self.assertEqual(len(list(self.output.glob(".catabolic-claim-*"))), 1)
        self.reconciler.recover()
        self.assertTrue(self.reconciler.apply()["healthy"])
        self.assertEqual(list(self.output.glob(".catabolic-claim-*")), [])

    def test_recovery_cancels_create_after_confirmed_disappearance(self):
        with patch(
            "catabolic.reconcile.os.symlink", side_effect=OSError("interrupted create")
        ):
            with self.assertRaises(OSError):
                self.reconciler.apply()
        self.media.unlink()
        self.app.scan()
        self.assertEqual(len(self.reconciler.recover()["cancelled"]), 1)
        self.assertFalse(self.link.is_symlink())
        self.assertEqual(self.store.rows("SELECT * FROM journal"), [])

    def test_recovery_cancels_removal_when_source_returns(self):
        self.reconciler.apply()
        self.media.unlink()
        self.app.scan()
        with patch(
            "catabolic.reconcile.os.unlink", side_effect=OSError("interrupted remove")
        ):
            with self.assertRaises(OSError):
                self.reconciler.apply()
        self.media.write_bytes(b"returned")
        self.app.scan()
        self.assertEqual(len(self.reconciler.recover()["cancelled"]), 1)
        self.assertTrue(self.reconciler.verify()["healthy"])

    def test_pending_work_blocks_catalog_edits(self):
        with self.assertRaises(Interrupted):
            self.reconciler.apply(after_filesystem=self.interrupt("create"))
        with self.assertRaises(CatabolicError):
            self.app.disable_mapping(self.mapping["id"])
        with self.assertRaises(CatabolicError):
            self.app.put_mapping("global", self.file_id, self.item_id, "another.mkv")
        self.reconciler.recover()
        self.app.disable_mapping(self.mapping["id"])

    def test_all_catalogs_preflight_blocks_before_any_mutation(self):
        second = self.root / "other"
        second.mkdir()
        (second / "keep.txt").write_text("keep")
        self.app.bind("output", "other", str(second))
        result = self.reconciler.apply(None)
        self.assertFalse(result["safe"])
        self.assertEqual(list(self.output.iterdir()), [])
        self.assertEqual((second / "keep.txt").read_text(), "keep")

    def test_directory_change_during_scan_preserves_inventory(self):

        original_listdir = os.listdir
        changed = False

        def changing_listdir(fd):
            nonlocal changed
            entries = original_listdir(fd)
            if not changed:
                changed = True
                (self.source / "new.mkv").write_bytes(b"new")
            return entries

        with patch("catabolic.filesystem.os.listdir", side_effect=changing_listdir):
            result = self.app.scan()
        self.assertFalse(result["complete"])
        self.assertEqual(len(self.app.files()["files"]), 1)
        self.assertIn("directory changed", str(result))

    def test_readers_do_not_create_writer_lock(self):
        other = self.root / "read-only.sqlite3"
        Store.initialize(other)
        # Initialization takes the writer lock. With no writer alive, remove
        # this fixture's lock to prove that a reader does not recreate it.
        Path(str(other) + ".lock").unlink()
        before = other.read_bytes()
        with Store(other) as store:
            Application(store).status()
        self.assertEqual(other.read_bytes(), before)
        self.assertFalse(Path(str(other) + ".lock").exists())

    def test_database_alias_cannot_bypass_writer_lock(self):
        alias = self.root / "alias"
        alias.symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(CatabolicError):
            Store(alias / self.database.name, writable=True)

    def test_incomplete_download_suffix_is_never_projected(self):
        download = self.source / "download.mkv.PART"
        download.write_bytes(b"still downloading")
        self.app.scan()
        file_id = next(
            row["id"]
            for row in self.app.files()["files"]
            if row["path"] == download.name
        )
        self.app.put_mapping("global", file_id, self.item_id, "Movies/download.mkv")
        result = self.reconciler.apply()
        self.assertTrue(result["safe"])
        self.assertFalse(result["healthy"])
        self.assertFalse((self.output / "Movies/download.mkv").exists())
        self.assertTrue(self.link.is_symlink())

    def test_atomic_replacement_and_recovery(self):
        self.reconciler.apply()
        second = self.source / "edition.mkv"
        second.write_bytes(b"second edition")
        self.app.scan()
        file_id = next(
            row["id"]
            for row in self.app.files()["files"]
            if row["path"] == "edition.mkv"
        )
        self.app.disable_mapping(self.mapping["id"])
        self.app.put_mapping("global", file_id, self.item_id, self.mapping["path"])
        with self.assertRaises(Interrupted):
            self.reconciler.apply(after_filesystem=self.interrupt("replace"))
        self.assertEqual(self.link.resolve(), second)
        self.reconciler.recover()
        self.assertTrue(self.reconciler.verify()["healthy"])

    def test_replacement_recovers_after_temporary_link_creation(self):
        self.reconciler.apply()
        replacement = self.root / "replacement-source"
        replacement.mkdir()
        (replacement / self.media.name).write_bytes(b"replacement")
        self.app.bind("source", "media", str(replacement))
        self.app.scan()
        with patch(
            "catabolic.reconcile.os.replace", side_effect=OSError("injected failure")
        ):
            with self.assertRaises(OSError):
                self.reconciler.apply()
        self.assertEqual(self.link.resolve(), self.media)
        self.reconciler.recover()
        self.assertEqual(self.link.resolve(), replacement / self.media.name)
        self.assertEqual(list(self.link.parent.glob(".catabolic-link-*")), [])

    def test_verify_does_not_depend_on_planner(self):
        self.reconciler.apply()
        with patch.object(
            self.reconciler,
            "preview",
            side_effect=AssertionError("must inspect independently"),
        ):
            self.assertTrue(self.reconciler.verify()["healthy"])
            self.link.unlink()
            self.assertFalse(self.reconciler.verify()["healthy"])

    def test_default_output_structure_and_end_to_end_generation(self):
        from catabolic.layouts import PRESETS, Layouts

        with patch("catabolic.app.Path.cwd", return_value=self.root):
            plex = self.app.bind("output", "plex")
            jellyfin = self.app.bind("output", "jellyfin")
        self.assertEqual(plex["root"], str(self.root / "catabolic/plex"))
        self.assertEqual(jellyfin["root"], str(self.root / "catabolic/jellyfin"))
        self.assertEqual(list((self.root / "catabolic/plex").iterdir()), [])
        layouts = Layouts(self.app)
        layouts.put("flat", PRESETS["flat"])
        self.assertTrue(layouts.run("flat", "plex", apply=True)["applied"])
        result = Reconciler(self.app).apply("plex")
        self.assertTrue(result["healthy"])
        self.assertEqual(result["verification"]["catalogs"][0]["verified_links"], 1)
        self.assertEqual(self.media.read_bytes(), b"fixture media\n")
        with patch("catabolic.app.Path.cwd", return_value=self.source):
            self.assertEqual(self.app.bind("output", "plex"), plex)
            self.assertEqual(
                self.app.bind("output", "global")["root"], str(self.output)
            )
        self.assertFalse((self.source / "catabolic").exists())

    def test_default_output_refuses_source_overlap_before_creating_directories(self):
        with patch("catabolic.app.Path.cwd", return_value=self.source):
            with self.assertRaisesRegex(CatabolicError, "overlaps registered source"):
                self.app.bind("output", "new")
        self.assertFalse((self.source / "catabolic").exists())
        self.assertEqual(self.media.read_bytes(), b"fixture media\n")

    def test_default_output_rejects_symlink_parents_and_leaf(self):
        parent = self.root / "catabolic"
        parent.symlink_to(self.source, target_is_directory=True)
        with patch("catabolic.app.Path.cwd", return_value=self.root):
            with self.assertRaises((OSError, CatabolicError)):
                self.app.bind("output", "new")
        self.assertFalse((self.source / "new").exists())
        parent.unlink()
        parent.mkdir()
        (parent / "new").symlink_to(self.source, target_is_directory=True)
        with patch("catabolic.app.Path.cwd", return_value=self.root):
            with self.assertRaises((OSError, CatabolicError)):
                self.app.bind("output", "new")
        self.assertEqual(self.media.read_bytes(), b"fixture media\n")
        self.assertFalse(self.store.rows("SELECT * FROM catalogs WHERE id='new'"))

    def test_default_output_rejects_nonempty_unbound_folder(self):
        output = self.root / "catabolic/new"
        output.mkdir(parents=True)
        external = output / "keep.txt"
        external.write_text("external")
        with patch("catabolic.app.Path.cwd", return_value=self.root):
            with self.assertRaisesRegex(CatabolicError, "already contains files"):
                self.app.bind("output", "new")
        self.assertEqual(external.read_text(), "external")
        self.assertFalse(self.store.rows("SELECT * FROM catalogs WHERE id='new'"))

    def test_default_output_detects_registered_source_case_alias(self):
        alias = self.source.with_name(self.source.name.upper())
        if not alias.exists() or not alias.samefile(self.source):
            self.skipTest("requires a case-insensitive filesystem")
        self.app.bind("source", "media", str(alias))
        with patch("catabolic.app.Path.cwd", return_value=self.source):
            with self.assertRaisesRegex(CatabolicError, "overlaps registered source"):
                self.app.bind("output", "new")
        self.assertFalse((self.source / "catabolic").exists())

    def test_default_binding_does_not_recreate_a_missing_existing_root(self):
        with patch("catabolic.app.Path.cwd", return_value=self.root):
            before = self.app.bind("output", "new")
            (self.root / "catabolic/new").rename(self.root / "old-new")
            with self.assertRaisesRegex(CatabolicError, "unavailable output root"):
                self.app.bind("output", "new")
        self.assertFalse((self.root / "catabolic/new").exists())
        self.assertEqual(self.app.binding("output", "new"), before)

    def test_default_binding_requires_writer_and_explicit_source_root(self):
        with self.assertRaisesRegex(CatabolicError, "explicit root"):
            self.app.bind("source", "new")
        with (
            Store(self.database) as reader,
            patch("catabolic.app.Path.cwd", return_value=self.root),
        ):
            with self.assertRaisesRegex(CatabolicError, "writable catalog"):
                Application(reader).bind("output", "new")
        self.assertFalse((self.root / "catabolic").exists())


class CliTest(unittest.TestCase):
    def test_default_output_cli_and_custom_override(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            database = root / "catalog.sqlite3"
            Store.initialize(database)
            custom = root / "custom-output"
            custom.mkdir()
            other = root / "other"
            other.mkdir()
            prefix = [
                sys.executable,
                "-m",
                "catabolic",
                "--db",
                str(database),
                "--json",
            ]

            def run(*args, cwd=root):
                result = subprocess.run(
                    [*prefix, *args], cwd=cwd, capture_output=True, text=True
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                return json.loads(result.stdout)

            plex = run("catalog", "bind", "plex")
            self.assertEqual(plex["root"], str(root / "catabolic/plex"))
            self.assertEqual(run("catalog", "bind", "plex", cwd=other), plex)
            self.assertFalse((other / "catabolic").exists())
            custom_binding = run("catalog", "bind", "jellyfin", "--root", str(custom))
            self.assertEqual(custom_binding["root"], str(custom))
            self.assertFalse((root / "catabolic/jellyfin").exists())
            self.assertEqual(
                run("catalog", "bind", "global")["root"], str(root / "catabolic/global")
            )
            missing = subprocess.run(
                [*prefix, "location", "bind", "source"],
                cwd=root,
                capture_output=True,
                text=True,
            )
            self.assertEqual(missing.returncode, 2)

    def test_public_cli_workflow_and_exit_codes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            source = root / "source"
            output = root / "output"
            source.mkdir()
            output.mkdir()
            (source / "movie.mkv").write_bytes(b"fixture")
            (source / "ignored").mkdir()
            (source / "ignored" / "another.mkv").write_bytes(b"excluded fixture")
            database = root / "db.sqlite3"

            def run(*args, expected=0):
                result = subprocess.run(
                    [
                        sys.executable,
                        "-m",
                        "catabolic",
                        "--json",
                        "--db",
                        str(database),
                        *args,
                    ],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertEqual(
                    result.returncode, expected, result.stderr + result.stdout
                )
                return json.loads(
                    result.stdout if result.returncode in (0, 3) else result.stderr
                )

            run("status", expected=2)
            self.assertFalse(database.exists())
            run("init")
            run("init", expected=2)
            run("location", "bind", "media", "--root", str(source))
            run("catalog", "bind", "global", "--root", str(output))
            run("scan", "--exclude", "ignored")
            inventory = run("files")["files"]
            self.assertEqual(len(inventory), 1)
            file_id = inventory[0]["id"]
            item_id = run(
                "item", "put", "--kind", "movie", "--identity", "fixture=movie"
            )["id"]
            run(
                "mapping",
                "put",
                "--file",
                file_id,
                "--item",
                item_id,
                "--path",
                "Movies/Example.mkv",
            )
            run("sync", "--dry-run")
            self.assertEqual(list(output.iterdir()), [])
            run("sync")
            self.assertTrue(run("verify")["healthy"])
            (source / "movie.mkv").unlink()
            run("sync", expected=3)


if __name__ == "__main__":
    unittest.main()
