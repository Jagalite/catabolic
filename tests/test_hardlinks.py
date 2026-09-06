# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import contextlib
import errno
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from catabolic.app import Application
from catabolic.cli import main
from catabolic.domain import CatabolicError
from catabolic.hardlinks import Hardlinks
from catabolic.interchange.validation import document_value, validate_document
from catabolic.manifest import Manifest
from catabolic.migration import SCHEMA_VERSION, load_migrations, upgrade_database
from catabolic.reconcile import Reconciler
from catabolic.store import Store
from tests import test_application
from tests.test_application import Interrupted
from tests.test_migrations import contents, create_legacy


class HardlinkTest(unittest.TestCase):
    def setUp(self):
        test_application.CatalogTest.setUp(self)
        self.app.bind("output", "global", link_mode="hardlink")

    def retained(self):
        return self.store.rows("SELECT * FROM retained_hardlinks")

    def disable(self):
        self.app.disable_mapping(self.mapping["id"])

    def fail_stage(self, desired):
        def fail(stage, operation):
            if stage == desired:
                raise Interrupted(stage)

        return fail

    def test_creation_repeat_warnings_and_readonly_preview(self):
        before = self.database.read_bytes()
        with Store(self.database) as store:
            preview = Reconciler(Application(store)).preview()
        self.assertTrue(preview["safe"], preview)
        self.assertIn("warnings", preview)
        self.assertEqual(self.database.read_bytes(), before)
        self.assertEqual(list(self.output.iterdir()), [])
        source = self.media.stat()
        result = self.reconciler.apply()
        self.assertTrue(result["healthy"], result)
        self.assertFalse(self.link.is_symlink())
        self.assertTrue(os.path.samefile(self.link, self.media))
        self.assertEqual(self.media.stat().st_nlink, source.st_nlink + 1)
        self.assertEqual(self.media.stat().st_mtime_ns, source.st_mtime_ns)
        self.assertEqual(self.reconciler.apply()["applied"], [])
        self.assertEqual(self.retained(), [])

    def test_retirement_preserves_data_and_is_reported(self):
        self.reconciler.apply()
        self.disable()
        result = self.reconciler.apply()
        self.assertTrue(result["healthy"], result)
        self.assertFalse(self.link.exists())
        retained = self.output / self.retained()[0]["path"]
        self.assertTrue(os.path.samefile(retained, self.media))
        self.assertEqual(result["verification"]["catalogs"][0]["retained_count"], 1)
        self.media.unlink()
        self.assertEqual(retained.stat().st_nlink, 1)
        self.reconciler.apply()
        self.assertEqual(retained.read_bytes(), b"fixture media\n")

    def test_final_pointer_blocks_disabled_and_confirmed_missing_cleanup(self):
        self.reconciler.apply()
        self.media.unlink()
        self.app.scan()
        for disable in (False, True):
            if disable:
                self.disable()
            result = self.reconciler.apply()
            self.assertFalse(result["safe"])
            self.assertIn("last hardlink protected", result["blockers"][0]["reason"])
            self.assertEqual(self.link.stat().st_nlink, 1)
            self.assertEqual(self.link.read_bytes(), b"fixture media\n")
            self.assertEqual(self.retained(), [])
            self.assertEqual(self.store.rows("SELECT * FROM journal"), [])

    def test_external_unlink_after_check_cannot_destroy_final_data(self):
        self.reconciler.apply()
        self.disable()

        def remove_source(stage, operation):
            if stage == "before_retention_rename":
                self.media.unlink()

        with patch("catabolic.hardlinks.checkpoint", side_effect=remove_source):
            self.assertTrue(self.reconciler.apply()["healthy"])
        retained = self.output / self.retained()[0]["path"]
        self.assertEqual(retained.stat().st_nlink, 1)
        self.assertEqual(retained.read_bytes(), b"fixture media\n")

    def test_final_reference_recheck_between_preview_and_execution(self):
        self.reconciler.apply()
        self.disable()
        original = Hardlinks.execute

        def execute(worker, operation, **kwargs):
            self.media.unlink()
            return original(worker, operation, **kwargs)

        with (
            patch.object(Hardlinks, "execute", execute),
            self.assertRaisesRegex(CatabolicError, "last hardlink"),
        ):
            self.reconciler.apply()
        self.assertTrue(self.link.is_file())
        self.assertEqual(self.link.stat().st_nlink, 1)
        self.reconciler.recover(cancel_unapplied=True)
        self.assertEqual(self.store.rows("SELECT * FROM journal"), [])

    def test_retention_race_cannot_overwrite_another_files_final_reference(self):
        self.reconciler.apply()
        self.disable()
        destinations = []

        def collide(stage, operation):
            if stage == "before_retention_rename":
                path = self.output / ".catabolic-retained" / operation["id"] / "data"
                path.write_bytes(b"other file's final reference")
                destinations.append(path)

        with (
            patch("catabolic.hardlinks.checkpoint", side_effect=collide),
            self.assertRaisesRegex(CatabolicError, "not overwritten"),
        ):
            self.reconciler.apply()
        self.assertEqual(destinations[0].read_bytes(), b"other file's final reference")
        self.assertEqual(self.link.read_bytes(), b"fixture media\n")

    def test_cross_filesystem_preflight_blocks_entire_batch(self):
        original = self.app.binding

        def binding(kind, owner):
            value = original(kind, owner)
            return (
                {**value, "device": value["device"] + 1} if kind == "output" else value
            )

        # Keep the real output handle while simulating a distinct source device
        # observed and live-validated by the existing source-health seam.
        with (
            patch.object(
                self.reconciler, "_source_target", return_value=("unused", None)
            ),
            patch.object(self.app, "binding", side_effect=binding),
            patch("catabolic.hardlinks.root_handle") as root,
        ):
            root.return_value.__enter__.return_value = os.open(
                self.output, os.O_RDONLY | os.O_DIRECTORY
            )
            fd = root.return_value.__enter__.return_value
            try:
                result = self.reconciler.apply()
            finally:
                os.close(fd)
        self.assertFalse(result["safe"], result)
        self.assertIn("cross-filesystem", result["blockers"][0]["reason"])
        self.assertEqual(list(self.output.iterdir()), [])
        self.assertEqual(self.store.rows("SELECT * FROM journal"), [])

    def test_kernel_exdev_no_fallback_and_safe_cancellation(self):
        original = os.link

        def link(src, dst, **kwargs):
            if dst.endswith(".mkv"):
                raise OSError(errno.EXDEV, "cross-device link")
            return original(src, dst, **kwargs)

        with (
            patch("catabolic.hardlinks.os.link", side_effect=link),
            self.assertRaisesRegex(CatabolicError, "EXDEV"),
        ):
            self.reconciler.apply()
        self.assertFalse(self.link.exists())
        self.assertTrue(self.store.rows("SELECT * FROM journal"))
        result = self.reconciler.recover(cancel_unapplied=True)
        self.assertEqual(len(result["cancelled"]), 1)
        self.app.bind("output", "global", link_mode="symlink")
        self.assertTrue(self.reconciler.apply()["healthy"])
        self.assertTrue(self.link.is_symlink())

    def test_regular_file_and_symlink_collisions_are_never_adopted(self):
        self.link.parent.mkdir(parents=True)
        self.link.write_bytes(b"unowned")
        self.assertFalse(self.reconciler.apply()["safe"])
        self.assertEqual(self.link.read_bytes(), b"unowned")
        # Claim a clean output first so the individual-path guard is exercised.
        self.link.unlink()
        for parent in (self.link.parent, self.link.parent.parent):
            parent.rmdir()
        from uuid import uuid4

        from catabolic.filesystem import claim_output, root_handle

        with root_handle(self.app.binding("output", "global")) as fd:
            claim_output(fd, self.reconciler.owner("global"), str(uuid4()))
        self.link.parent.mkdir(parents=True)
        os.link(self.media, self.link)
        self.assertFalse(self.reconciler.apply()["safe"])
        self.assertEqual(self.store.rows("SELECT * FROM owned_hardlinks"), [])

    def test_owned_inode_substitution_blocks_mutation(self):
        self.reconciler.apply()
        self.link.unlink()
        self.link.write_bytes(b"foreign replacement")
        self.disable()
        self.assertFalse(self.reconciler.apply()["safe"])
        self.assertEqual(self.link.read_bytes(), b"foreign replacement")

    def test_symlink_substitution_and_retention_parent_tampering_are_blocked(self):
        self.reconciler.apply()
        self.link.unlink()
        self.link.symlink_to(self.media)
        self.assertFalse(self.reconciler.apply()["safe"])
        self.assertTrue(self.link.is_symlink())
        self.link.unlink()
        os.link(self.media, self.link)
        self.disable()
        foreign = self.root / "foreign"
        foreign.mkdir()
        (self.output / ".catabolic-retained").symlink_to(
            foreign, target_is_directory=True
        )
        self.assertFalse(self.reconciler.apply()["safe"])
        self.assertEqual(self.store.rows("SELECT * FROM journal"), [])
        self.assertEqual(list(foreign.iterdir()), [])

    def test_replacement_retains_old_inode(self):
        self.reconciler.apply()
        backup = self.source / "old-copy.mkv"
        os.link(self.media, backup)
        self.media.unlink()
        self.media.write_bytes(b"new media")
        self.app.scan()
        self.assertTrue(self.reconciler.apply()["healthy"])
        self.assertTrue(os.path.samefile(self.link, self.media))
        self.assertTrue(
            os.path.samefile(self.output / self.retained()[0]["path"], backup)
        )

    def test_replacement_of_last_old_reference_is_blocked(self):
        self.reconciler.apply()
        self.media.unlink()
        self.media.write_bytes(b"new inode")
        self.app.scan()
        result = self.reconciler.apply()
        self.assertFalse(result["safe"])
        self.assertIn("last hardlink", result["blockers"][0]["reason"])
        self.assertEqual(self.link.read_bytes(), b"fixture media\n")

    def test_create_crash_recovery_preserves_identity(self):
        with (
            patch(
                "catabolic.hardlinks.checkpoint",
                side_effect=self.fail_stage("after_link"),
            ),
            self.assertRaises(Interrupted),
        ):
            self.reconciler.apply()
        inode = self.link.stat().st_ino
        self.reconciler.recover()
        self.assertEqual(self.link.stat().st_ino, inode)
        self.assertTrue(self.reconciler.verify()["healthy"])

    def test_retirement_crash_recovery_can_preserve_a_final_reference(self):
        self.reconciler.apply()
        self.disable()
        with (
            patch(
                "catabolic.hardlinks.checkpoint",
                side_effect=self.fail_stage("after_retention"),
            ),
            self.assertRaises(Interrupted),
        ):
            self.reconciler.apply()
        self.media.unlink()
        self.reconciler.recover()
        self.assertFalse(self.link.exists())
        self.assertEqual(
            (self.output / self.retained()[0]["path"]).read_bytes(), b"fixture media\n"
        )

    def test_replacement_crash_then_obsolete_intent_is_safely_cancelled(self):
        self.reconciler.apply()
        os.link(self.media, self.source / "backup.mkv")
        self.media.unlink()
        self.media.write_bytes(b"second inode")
        self.app.scan()
        with (
            patch(
                "catabolic.hardlinks.checkpoint",
                side_effect=self.fail_stage("after_retention"),
            ),
            self.assertRaises(Interrupted),
        ):
            self.reconciler.apply()
        self.media.unlink()
        self.media.write_bytes(b"third inode")
        self.app.scan()
        result = self.reconciler.recover()
        self.assertEqual(len(result["cancelled"]), 1)
        self.assertEqual(len(self.retained()), 1)
        self.assertTrue(self.reconciler.apply()["healthy"])
        self.assertEqual(self.link.read_bytes(), b"third inode")

    def test_source_swap_during_creation_is_preserved_not_claimed(self):
        def change(stage, operation):
            if stage == "before_link":
                self.media.rename(self.source / "original.mkv")
                self.media.write_bytes(b"unexpected file")

        with (
            patch("catabolic.hardlinks.checkpoint", side_effect=change),
            self.assertRaisesRegex(CatabolicError, "during hardlink"),
        ):
            self.reconciler.apply()
        self.assertEqual(self.link.read_bytes(), b"unexpected file")
        self.assertEqual(self.store.rows("SELECT * FROM owned_hardlinks"), [])
        self.assertEqual(
            (self.source / "original.mkv").read_bytes(), b"fixture media\n"
        )

    def test_link_mode_and_rebinding_guards_include_retained_data(self):
        self.reconciler.apply()
        for when in ("owned", "retained"):
            if when == "retained":
                self.disable()
                self.reconciler.apply()
            with self.assertRaises(CatabolicError):
                self.app.bind("output", "global", link_mode="symlink")
            other = self.root / when
            other.mkdir()
            with self.assertRaises(CatabolicError):
                self.app.bind("output", "global", str(other))
            self.app.bind("output", "global")
            self.assertEqual(self.app.link_mode("global"), "hardlink")

    def test_manifest_records_mode_and_inode_without_symlink_claims(self):
        self.reconciler.apply()
        with self.store.transaction():
            manifest = Manifest(self.app).build()
        self.assertEqual(manifest["format_version"], 3)
        content = manifest["content"]
        self.assertEqual(content["catalog"]["link_mode"], "hardlink")
        self.assertIsNone(content["entries"][0]["expected_link_target"])
        self.assertEqual(content["recorded_links"], [])
        self.assertEqual(
            content["hardlinks"][0]["inode"], str(self.media.stat().st_ino)
        )
        self.assertEqual(document_value(validate_document(manifest)), manifest)
        self.disable()
        self.reconciler.apply()
        with self.store.transaction():
            manifest = Manifest(self.app).build()
        self.assertEqual(manifest["content"]["counts"]["retained_hardlinks"], 1)

    def test_refreshing_owned_v2_symlink_manifest_to_v3(self):
        # A legacy symlink manifest remains a valid explicitly replaceable export.
        self.app.bind("output", "global", link_mode="symlink")
        self.reconciler.apply()
        with self.store.transaction():
            document = Manifest(self.app).build()
        legacy = json.loads(json.dumps(document))
        legacy["format_version"] = 2
        legacy["content"]["catalog"].pop("link_mode")
        for key in ("hardlinks", "retained_hardlinks"):
            legacy["content"].pop(key)
            legacy["content"]["counts"].pop(key)
        from catabolic.manifest import digest

        legacy["content_sha256"] = digest(legacy["content"])
        validate_document(legacy)
        output = self.root / "legacy.json"
        output.write_text(json.dumps(legacy))
        with self.store.transaction():
            result = Manifest(self.app).write(document, output=output, replace=True)
        self.assertTrue(result["written"])
        self.assertEqual(json.loads(output.read_text())["format_version"], 3)

    def test_cli_option_mode_retention_and_json(self):
        self.reconciler.apply()
        self.disable()
        self.reconciler.apply()
        self.store.close()

        def run(*args):
            out, err = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                code = main(["--db", str(self.database), "--json", *args])
            self.assertEqual(code, 0, err.getvalue())
            return json.loads(out.getvalue())

        self.assertEqual(run("catalog", "list")["bindings"][0]["link_mode"], "hardlink")
        self.assertEqual(len(run("catalog", "retained", "global")["retained"]), 1)
        new = self.root / "new-output"
        new.mkdir()
        self.assertEqual(
            run(
                "catalog",
                "bind",
                "second",
                "--root",
                str(new),
                "--link-mode",
                "hardlink",
            )["link_mode"],
            "hardlink",
        )
        self.assertIn(
            "hardlink",
            run("graphql", "{ catalogs { nodes { linkMode } } }")["data"]["catalogs"][
                "nodes"
            ][0]["linkMode"],
        )


class HardlinkMigrationTest(unittest.TestCase):
    def test_schema_four_upgrade_preserves_every_existing_value(self):
        with tempfile.TemporaryDirectory() as directory:
            path = create_legacy(Path(directory))
            upgrade_database(path, migrations=load_migrations()[:4])
            before = contents(path)
            result = upgrade_database(path)
            self.assertEqual(
                [step["version"] for step in result["applied"]],
                list(range(5, SCHEMA_VERSION + 1)),
            )
            self.assertEqual(contents(Path(result["backup"])), before)
            self.assertEqual(
                {key: value for key, value in contents(path).items() if key in before},
                before,
            )
            with Store(path) as store:
                self.assertEqual(store.schema_version, SCHEMA_VERSION)
                self.assertEqual(Application(store).link_mode("global"), "symlink")

    def test_process_exit_recovery_keeps_created_and_retained_data(self):
        worker = """
import os,sys
from catabolic.app import Application
from catabolic.store import Store
from catabolic.reconcile import Reconciler
import catabolic.hardlinks as hardlinks
def stop(stage, operation):
    if stage == sys.argv[2]:
        os._exit(86)
hardlinks.checkpoint = stop
with Store(sys.argv[1], writable=True) as store:
    Reconciler(Application(store)).apply()
"""
        for stage in ("after_link", "after_retention"):
            with self.subTest(stage=stage), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                source, output = root / "source", root / "output"
                source.mkdir()
                output.mkdir()
                media = source / "file.mkv"
                media.write_bytes(b"must survive process exit")
                database = root / "database.sqlite3"
                Store.initialize(database)
                with Store(database, writable=True) as store:
                    app = Application(store)
                    app.bind("source", "media", str(source))
                    app.bind("output", "global", str(output), link_mode="hardlink")
                    app.scan()
                    item = app.put_item("movie", {}, {}, "movie")["id"]
                    mapping = app.put_mapping(
                        "global", app.files()["files"][0]["id"], item, "file.mkv"
                    )
                    if stage == "after_retention":
                        Reconciler(app).apply()
                        app.disable_mapping(mapping["id"])
                result = subprocess.run(
                    [sys.executable, "-c", worker, str(database), stage],
                    capture_output=True,
                    text=True,
                    timeout=30,
                )
                self.assertEqual(result.returncode, 86, result.stderr)
                if stage == "after_retention":
                    media.unlink()
                with Store(database, writable=True) as store:
                    app = Application(store)
                    Reconciler(app).recover()
                    self.assertEqual(store.rows("SELECT * FROM journal"), [])
                    if stage == "after_retention":
                        retained = store.rows("SELECT path FROM retained_hardlinks")[0][
                            "path"
                        ]
                        self.assertEqual(
                            (output / retained).read_bytes(),
                            b"must survive process exit",
                        )
                        self.assertEqual((output / retained).stat().st_nlink, 1)
                    else:
                        self.assertTrue(os.path.samefile(media, output / "file.mkv"))
