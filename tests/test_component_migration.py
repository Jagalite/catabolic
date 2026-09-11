# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import unittest
from unittest.mock import patch

from catabolic.app import Application
from catabolic.curation import occurrence
from catabolic.migration import (
    data_snapshot,
    load_migrations,
    upgrade_database,
    validate_preservation,
)
from catabolic.store import Store, encode
from tests import test_components as fixtures


class MigrationTest(unittest.TestCase):
    setUp = fixtures.ComponentsTest.setUp

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
                app.scan()
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
        self.assertEqual(result["schema"], 25)
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
