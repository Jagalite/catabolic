# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import unittest
from unittest.mock import patch
from uuid import UUID, uuid5

from catabolic.app import Application
from catabolic.curation import occurrence
from catabolic.migration import (
    SCHEMA_VERSION,
    data_snapshot,
    load_migrations,
    upgrade_database,
    validate_preservation,
)
from catabolic.store import Store, encode
from tests import test_components as fixtures


class MigrationTest(unittest.TestCase):
    setUp = fixtures.ComponentsTest.setUp

    def seed_legacy_inventory(self, store, paths):
        # Historical fixture data must not execute a newer admission protocol.
        with store.transaction() as db:
            db.execute(
                "INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('legacy-scan','default','media',1,?,'[]')",
                (len(paths),),
            )
            for path in paths:
                st = (self.source / path).stat()
                file_id = str(uuid5(UUID(store.database_id), encode(["media", path])))
                db.execute("INSERT INTO files VALUES (?,'media',?)", (file_id, path))
                db.execute(
                    "INSERT INTO observations VALUES ('default',?,?,?,?,?,?,'legacy-scan')",
                    (
                        file_id,
                        st.st_size,
                        st.st_mtime_ns,
                        st.st_dev,
                        st.st_ino,
                        "present",
                    ),
                )

    def test_populated_schema_24_adopts_probe_and_sidecar_without_rewriting_data(self):
        path = self.root / "legacy.db"
        migrations = load_migrations()
        with (
            patch("catabolic.migration.load_migrations", return_value=migrations[:24]),
            patch("catabolic.store.SCHEMA_VERSION", 24),
        ):
            Store.initialize(path)
            with Store(path, writable=True) as old:
                app = Application(old)
                (self.source / "legacy.mkv").write_bytes(b"legacy fixture")
                (self.source / "legacy.srt").write_bytes(b"legacy subtitle")
                app.bind("source", "media", str(self.source))
                self.seed_legacy_inventory(old, ["legacy.mkv", "legacy.srt"])
                item = app.put_item("movie", {"legacy": "yes"}, {"title": "Legacy"})[
                    "id"
                ]
                ids = {r["path"]: r["id"] for r in old.rows("SELECT * FROM files")}
                app.media.associate(ids["legacy.mkv"], item)
                app.media.associate(
                    ids["legacy.srt"],
                    item,
                    role="subtitle",
                    metadata={"language": "en", "forced": True},
                )
                snap = occurrence(old, "default", ids["legacy.mkv"])
                data = {
                    "streams": [
                        {"index": 0, "codec_type": "video"},
                        {
                            "index": 1,
                            "codec_type": "audio",
                            "tags": {"language": "eng"},
                        },
                    ]
                }
                with old.transaction() as db:
                    db.execute(
                        "INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state,result) VALUES ('legacy-probe','default',?,'probe','media',?,'{}','legacy-probe','complete',?)",
                        (ids["legacy.mkv"], encode(snap), encode(data)),
                    )
                    db.execute(
                        "INSERT INTO file_facts VALUES ('default',?,'probe','legacy-probe',?,?,'complete')",
                        (ids["legacy.mkv"], encode(snap), encode(data)),
                    )
                original = data_snapshot(old.db)
        result = upgrade_database(path)
        self.assertEqual(result["schema"], SCHEMA_VERSION)
        self.assertTrue(result["backup"])
        with Store(path) as store:
            validate_preservation(store.db, original)
            rows = store.rows("SELECT * FROM component_occurrences")
            self.assertEqual(len(rows), 3)
            self.assertEqual({r["file_id"] for r in rows}, set(ids.values()))
            from catabolic.component_sql import OCCURRENCES_SQL

            current = store.rows(
                "SELECT * FROM (" + OCCURRENCES_SQL + ") WHERE current=1"
            )
            self.assertEqual(len(current), 3)
            self.assertEqual(sum(r["technically_verified"] for r in current), 2)

    def test_schema_24_pending_journal_recovers_before_upgrade(self):
        import json
        import subprocess
        import sys

        from catabolic.domain import CatabolicError
        from catabolic.reconcile import Reconciler

        for schema, applied in (
            (24, False),
            (24, True),
            (25, False),
            (25, True),
            (26, False),
            (26, True),
        ):
            with self.subTest(schema=schema, filesystem_applied=applied):
                path = self.root / f"pending-{schema}-{applied}.db"
                output = self.root / f"output-{schema}-{applied}"
                output.mkdir()
                migrations = load_migrations()
                with (
                    patch(
                        "catabolic.migration.load_migrations",
                        return_value=migrations[:schema],
                    ),
                    patch("catabolic.store.SCHEMA_VERSION", schema),
                ):
                    Store.initialize(path)
                    with Store(path, writable=True) as old:
                        app = Application(old)
                        (self.source / "legacy.mkv").write_bytes(b"legacy fixture")
                        app.bind("source", "media", str(self.source))
                        app.bind("output", "global", str(output))
                        self.seed_legacy_inventory(old, ["legacy.mkv"])
                        item = app.put_item(
                            "movie", {"legacy": "yes"}, {"title": "Legacy"}
                        )["id"]
                        file = old.rows("SELECT id FROM files WHERE path='legacy.mkv'")[
                            0
                        ]["id"]
                        app.put_mapping("global", file, item, "Legacy.mkv")
                        original = Reconciler._execute

                        def interrupt(
                            reconciler,
                            operation,
                            applied=applied,
                            original=original,
                            **kwargs,
                        ):
                            if operation["kind"] == "create":
                                if applied:

                                    def after_filesystem(_):
                                        raise KeyboardInterrupt

                                    return original(
                                        reconciler,
                                        operation,
                                        after_filesystem=after_filesystem,
                                    )
                                raise KeyboardInterrupt
                            return original(reconciler, operation, **kwargs)

                        with (
                            patch.object(Reconciler, "_execute", interrupt),
                            self.assertRaises(KeyboardInterrupt),
                        ):
                            Reconciler(app).apply()
                        pending = old.rows("SELECT id FROM journal")
                        self.assertTrue(pending)
                with self.assertRaisesRegex(CatabolicError, "recover pending"):
                    upgrade_database(path)
                command = [
                    sys.executable,
                    "-m",
                    "catabolic",
                    "--json",
                    "--db",
                    str(path),
                    "recover",
                ]
                result = subprocess.run(
                    command, capture_output=True, text=True, timeout=30
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(
                    set(json.loads(result.stdout)["recovered"]),
                    {r["id"] for r in pending},
                )
                self.assertTrue((output / "Legacy.mkv").is_symlink())
                inode = (output / "Legacy.mkv").lstat().st_ino
                result = subprocess.run(
                    command, capture_output=True, text=True, timeout=30
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(json.loads(result.stdout)["recovered"], [])
                self.assertEqual((output / "Legacy.mkv").lstat().st_ino, inode)
                self.assertEqual(upgrade_database(path)["schema"], SCHEMA_VERSION)
                with Store(path) as upgraded:
                    self.assertFalse(upgraded.rows("SELECT * FROM journal"))
                    self.assertTrue(
                        Reconciler(Application(upgraded)).verify()["healthy"]
                    )
