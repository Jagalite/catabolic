# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import json
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch

from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.filesystem import root_handle
from catabolic.migration import SCHEMA_VERSION, load_migrations, upgrade_database
from catabolic.reconcile import Reconciler
from catabolic.remount import repair
from catabolic.store import Store, encode
from tests.test_consumer_publication import attach, generations
from tests.test_migrations import create_legacy


class RemountTest(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.path = self.root / "catalog.db"
        for name in ("source", "output"):
            (self.root / name).mkdir()
        (self.root / "source/movie.mkv").write_bytes(b"media fixture")
        for module in (
            "catabolic.volume_identity.volume_uuid",
            "catabolic.remount.volume_uuid",
        ):
            mock = patch(module, return_value="fixture-volume")
            mock.start()
            self.addCleanup(mock.stop)
        Store.initialize(self.path)
        self.store = Store(self.path, writable=True)
        self.addCleanup(self.store.close)
        self.app = Application(self.store)
        self.app.bind("source", "source", str(self.root / "source"))
        self.app.bind("output", "global", str(self.root / "output"))
        self.app.scan()
        file_id = self.app.files()["files"][0]["id"]
        item = self.app.put_item("movie", {"fixture": "movie"}, {"title": "Movie"})[
            "id"
        ]
        self.app.media.associate(file_id, item)
        self.app.put_mapping("global", file_id, item, "Movie.mkv")
        self.assertTrue(Reconciler(self.app).apply()["verification"]["healthy"])
        attach(self.app, "global")
        with self.store.transaction() as db:
            db.execute("UPDATE consumer_bindings SET generation=1")
        self.before_link = (self.root / "output/Movie.mkv").lstat()

    def renumber(self, legacy=False):
        with self.store.transaction() as db:
            db.execute("INSERT INTO remount_guard VALUES ('default')")
            db.execute("UPDATE bindings SET device=device+100")
            db.execute("UPDATE observations SET device=device+100")
            db.execute("DELETE FROM remount_guard")
            old = self.store.rows("SELECT * FROM bindings WHERE kind='output'")[0]
            db.execute("UPDATE consumer_bindings SET local_binding=?", (encode(old),))
            db.execute(
                "UPDATE publication_generations SET local_binding=?", (encode(old),)
            )
            if legacy:
                db.execute("DELETE FROM binding_volumes")

    def apply(self, **kwargs):
        preview = repair(self.app, **kwargs)
        return repair(self.app, apply=True, expected_plan=preview["plan_id"], **kwargs)

    def test_remount_preserves_links_pending_generations_and_observations(self):
        self.renumber()
        before = generations(self.app)
        publication = self.store.rows(
            "SELECT generation,verified FROM publication_generations"
        )
        count = self.store.rows("SELECT count(*) AS n FROM observations")
        with (
            self.assertRaises(CatabolicError),
            root_handle(self.app.binding("output", "global")),
        ):
            pass
        preview = repair(self.app)
        self.assertFalse(preview["applied"])
        self.assertEqual(self.store.rows("SELECT * FROM remount_repairs"), [])
        result = repair(self.app, apply=True, expected_plan=preview["plan_id"])
        self.assertTrue(result["verification"]["healthy"])
        self.assertEqual(generations(self.app), before)
        self.assertEqual(
            self.store.rows("SELECT generation,verified FROM publication_generations"),
            publication,
        )
        self.assertEqual(
            self.store.rows("SELECT count(*) AS n FROM observations"), count
        )
        self.assertEqual((self.root / "output/Movie.mkv").lstat(), self.before_link)
        self.assertEqual(self.store.rows("SELECT * FROM remount_guard"), [])
        self.apply()
        self.assertEqual(generations(self.app), before)

    def test_legacy_adoption_is_explicit_and_plan_fenced(self):
        self.renumber(legacy=True)
        with self.assertRaisesRegex(CatabolicError, "adopt-existing"):
            repair(self.app)
        preview = repair(self.app, adopt_existing=True)
        self.assertTrue(all(r["adopted"] for r in preview["bindings"]))
        with self.assertRaisesRegex(CatabolicError, "preview changed"):
            repair(self.app, apply=True, adopt_existing=True, expected_plan="wrong")
        self.assertEqual(self.store.rows("SELECT * FROM binding_volumes"), [])
        self.assertTrue(self.apply(adopt_existing=True)["complete"])

    def test_replacement_or_unavailable_volume_is_rejected_even_same_device(self):
        for replacement in ("other-volume", None):
            with patch("catabolic.remount.volume_uuid", return_value=replacement):
                with self.assertRaises(CatabolicError):
                    repair(self.app, adopt_existing=True)
            with patch(
                "catabolic.volume_identity.volume_uuid", return_value=replacement
            ):
                with (
                    self.assertRaises(CatabolicError),
                    root_handle(self.app.binding("source", "source")),
                ):
                    pass

    def test_changed_source_or_link_and_missing_root_are_not_adopted(self):
        self.renumber()
        (self.root / "source/movie.mkv").write_bytes(b"changed")
        with self.assertRaisesRegex(CatabolicError, "metadata changed"):
            repair(self.app)
        (self.root / "output").rename(self.root / "moved")
        with self.assertRaises(OSError):
            repair(self.app)
        (self.root / "output").mkdir()
        with self.assertRaisesRegex(CatabolicError, "inode changed"):
            repair(self.app)

    def test_foreign_marker_and_wrong_link_are_refused(self):
        link = self.root / "output/Movie.mkv"
        link.unlink()
        link.symlink_to("/unrelated")
        with self.assertRaisesRegex(CatabolicError, "symlink or target changed"):
            repair(self.app)
        marker = self.root / "output/.catabolic-owner.json"
        marker.write_text(json.dumps({"foreign": True}))
        with self.assertRaisesRegex(CatabolicError, "different database"):
            repair(self.app)

    def test_inflight_delivery_and_interrupted_repair(self):
        self.renumber()
        before = self.store.rows("SELECT * FROM bindings")
        with self.store.transaction() as db:
            db.execute(
                "UPDATE consumer_deliveries SET lease_until=?", (time.time() + 60,)
            )
        with self.assertRaisesRegex(CatabolicError, "in flight"):
            repair(self.app)
        with self.store.transaction() as db:
            db.execute("UPDATE consumer_deliveries SET lease_until=0")
        preview = repair(self.app)
        with patch.object(Reconciler, "verify", side_effect=RuntimeError("crash")):
            with self.assertRaises(RuntimeError):
                repair(self.app, apply=True, expected_plan=preview["plan_id"])
        self.assertEqual(self.store.rows("SELECT * FROM bindings"), before)
        self.assertEqual(self.store.rows("SELECT * FROM remount_guard"), [])
        self.assertEqual(self.store.rows("SELECT * FROM remount_repairs"), [])
        self.assertTrue(self.apply()["applied"])

    def test_populated_migration_does_not_adopt_current_mounts(self):
        path = create_legacy(self.root)
        upgrade_database(path, migrations=load_migrations()[:17])
        self.assertEqual(upgrade_database(path)["schema"], SCHEMA_VERSION)
        with Store(path) as store:
            self.assertEqual(store.rows("SELECT * FROM binding_volumes"), [])
            self.assertTrue(store.rows("SELECT * FROM bindings"))
