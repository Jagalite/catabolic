# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native generation-fenced refresh and suppressed legacy consumer notifications."""

import copy
import json
import os
import sqlite3
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from catabolic.app import Application
from catabolic.layouts import PRESETS, Layouts
from catabolic.network_adapters import Refresh
from catabolic.store import Store, encode
from tests.test_consumers import ProtocolServer
from tests.test_rust_migration import BINARY


class NativeCatalogRefreshTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.database = self.root / "catalog.db"
        self.source = self.root / "source"
        self.source.mkdir()
        (self.source / "one.mkv").write_bytes(b"source bytes")
        self.output = self.root / "output"
        self.output.mkdir()
        Store.initialize(self.database)
        with Store(self.database, writable=True) as store:
            app = Application(store)
            app.bind("source", "media", str(self.source))
            app.scan("media")
            app.bind("output", "global", str(self.output))
            file_id = store.rows("SELECT id FROM files")[0]["id"]
            item = app.put_item("movie", {"fixture": "refresh"}, {"title": "One"})["id"]
            app.media.associate(file_id, item)
            Layouts(app).put("saved", copy.deepcopy(PRESETS["catabolic"]))
            Layouts(app).run("saved", "global", apply=True)
            Refresh(app).configure("global", "http://127.0.0.1:9", "UNUSED")
            binding = dict(store.rows("SELECT * FROM bindings WHERE kind='output'")[0])
            with store.transaction() as db:
                db.execute(
                    "INSERT INTO consumer_connections(profile,id,application,endpoint,credential_env,server_id,evidence) VALUES('default','offline','plex','http://127.0.0.1:9','UNUSED','fixture','{}')"
                )
                db.execute(
                    "INSERT INTO consumer_bindings(profile,id,connection_id,catalog,subtree,remote_root,local_binding,library,group_id,origin) VALUES('default','modern','offline','global','','/remote',?,'{}','group','fixture')",
                    (encode(binding),),
                )
                db.execute(
                    "INSERT INTO consumer_deliveries(profile,id) VALUES('default','group')"
                )

    def call(self, operation, *, surface="catalog-refresh", **request):
        result = subprocess.run(
            [
                str(BINARY),
                "--db",
                str(self.database),
                "operation",
                surface,
                operation,
                "--options",
                json.dumps(request),
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_refresh_suppresses_legacy_queue_preserves_dirty_and_tracks_modern_generation(
        self,
    ):
        dirty = '{"fixture":"preexisting"}'
        with Store(self.database, writable=True) as store, store.transaction() as db:
            db.execute(
                "INSERT INTO refresh_dirty VALUES('default','global',?)", (dirty,)
            )
        result = self.call("configure", catalog="global", enabled=True)
        self.assertTrue(result["catalog_refresh"]["complete"], result)
        self.assertEqual(
            len([path for path in self.output.rglob("*") if path.is_symlink()]), 1
        )
        with sqlite3.connect(self.database) as db:
            self.assertEqual(
                db.execute("SELECT target FROM refresh_dirty").fetchall(), [(dirty,)]
            )
            self.assertEqual(
                db.execute("SELECT count(*) FROM refresh_events").fetchone()[0], 0
            )
            generation, verified, acknowledged = db.execute(
                "SELECT generation,verified,acknowledged FROM consumer_bindings"
            ).fetchone()
            self.assertGreater(generation, 0)
            self.assertEqual(generation, verified)
            self.assertEqual(acknowledged, 0)
            self.assertEqual(
                db.execute("SELECT count(*) FROM consumer_attempts").fetchone()[0], 0
            )
        self.assertEqual(self.call("pending")["pending"], [])

    def test_offline_output_retains_queue_backoff_and_rolls_back_mapping_changes(self):
        with Store(self.database) as store:
            before = store.rows("SELECT * FROM mappings")
        offline = self.root / "offline"
        self.output.rename(offline)
        result = self.call("configure", catalog="global", enabled=True)
        self.assertFalse(result["catalog_refresh"]["complete"])
        self.assertEqual(result["catalog_refresh"]["pending"], 1)
        with Store(self.database) as store:
            self.assertEqual(store.rows("SELECT * FROM mappings"), before)
            self.assertEqual(
                store.rows("SELECT attempts FROM catalog_refresh_queue"),
                [{"attempts": 1}],
            )
        self.assertEqual(self.call("run")["processed"], 0)
        offline.rename(self.output)
        self.assertTrue(self.call("run", force=True)["complete"])
        self.assertEqual(self.call("run", force=True)["processed"], 0)

    def test_refresh_drains_automatic_consumers_after_detaching_without_report_changes(
        self,
    ):
        remote = ProtocolServer(self.database)
        self.addCleanup(remote.close)
        with patch.dict(os.environ, {"CATABOLIC_TEST_PLEX_TOKEN": "synthetic-fixture"}):
            self.call(
                "connection-put",
                surface="consumers",
                id="plex",
                application="plex",
                endpoint=remote.url,
                credential_env="CATABOLIC_TEST_PLEX_TOKEN",
                apply=True,
            )
            self.call(
                "bind",
                surface="consumers",
                id="online",
                connection="plex",
                catalog="global",
                subtree="",
                remote_root="/plex/Movies",
                library_id="1",
                type="movie",
                automatic=True,
                apply=True,
            )
            remote.requests.clear()
            result = self.call("configure", catalog="global", enabled=True)
            self.assertEqual(
                set(result["catalog_refresh"]),
                {"processed", "refreshed", "errors", "pending", "complete"},
            )
            self.assertTrue(result["catalog_refresh"]["complete"], result)
            self.assertEqual(remote.scans, ["/library/sections/1/refresh"])
            self.assertEqual(remote.lock_failures, [])
            with sqlite3.connect(self.database) as db:
                generation, verified, acknowledged = db.execute(
                    "SELECT generation,verified,acknowledged FROM consumer_bindings WHERE id='online'"
                ).fetchone()
                self.assertGreater(generation, 0)
                self.assertEqual((verified, acknowledged), (generation, generation))
            self.assertTrue(self.call("run")["complete"])
            self.assertEqual(len(remote.scans), 1)


if __name__ == "__main__":
    unittest.main()
