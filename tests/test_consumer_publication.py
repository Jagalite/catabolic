# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Real entrypoints, with locally configured bindings and no network policy."""

import unittest

from catabolic.catalog_refresh import CatalogRefresh
from catabolic.maintenance import run
from catabolic.projections import Projections
from catabolic.receipts import import_receipt
from catabolic.store import encode
from tests import test_catalog_refresh, test_programmable_catalog, test_query_layouts


def attach(app, catalog):
    with app.store.transaction() as db:
        db.execute(
            "INSERT INTO consumer_connections(profile,id,application,endpoint,credential_env,server_id,evidence) VALUES (?,'fixture','plex','http://127.0.0.1:9','UNUSED','fixture','{}')",
            (app.profile,),
        )
        db.execute(
            "INSERT INTO consumer_deliveries(profile,id) VALUES (?,'fixture')",
            (app.profile,),
        )
        db.execute(
            "INSERT INTO consumer_bindings(profile,id,connection_id,catalog,subtree,remote_root,local_binding,library,group_id,origin) VALUES (?,'fixture','fixture',?,'','/output',?,'{}','fixture','adopted')",
            (app.profile, catalog, encode(app.binding("output", catalog))),
        )


def generations(app):
    return app.store.rows(
        "SELECT generation,verified,acknowledged FROM consumer_bindings"
    )[0]


class ProjectionBoundaryTest(unittest.TestCase):
    setUp = test_query_layouts.QueryLayoutTest.setUp
    save = test_programmable_catalog.ProgrammableCatalogTest.save
    projection = test_programmable_catalog.ProgrammableCatalogTest.projection

    def test_projection_and_maintenance(self):
        self.projection(self.save())
        attach(self.app, "query")
        self.assertTrue(Projections(self.app).run("query", apply=True)["complete"])
        first = generations(self.app)
        self.assertGreater(first["generation"], 0)
        self.assertEqual(first["generation"], first["verified"])
        self.assertEqual(first["acknowledged"], 0)
        Projections(self.app).run("query", apply=True)
        self.assertEqual(first, generations(self.app))
        # A missing owned link is repaired by maintenance through the same journal.
        next(p for p in (self.root / "query").rglob("*") if p.is_symlink()).unlink()
        run(self.app, catalog="query", settle=0)
        after = generations(self.app)
        self.assertGreater(after["generation"], first["generation"])
        self.assertEqual(after["generation"], after["verified"])

    def test_notifications_work_without_a_consumer_and_recover_publication(self):
        from unittest.mock import patch

        from catabolic.notifications import configure, drain

        self.projection(self.save())
        configure(
            self.app,
            "human",
            "PRIVATE_NOTIFICATION_REF",
            ["projection_updated"],
            [],
            "info",
            apply=True,
        )
        with (
            patch(
                "catabolic.consumers.published",
                side_effect=RuntimeError("crash after ownership"),
            ),
            self.assertRaises(RuntimeError),
        ):
            Projections(self.app).run("query", apply=True)
        self.assertEqual(self.store.rows("SELECT * FROM consumer_bindings"), [])
        self.assertEqual(self.store.rows("SELECT * FROM notification_deliveries"), [])
        self.store.close()
        with patch("catabolic.notifications.send", return_value="complete") as send:
            result = drain(self.path)
            self.assertTrue(result["complete"])
            self.assertEqual(len(result["deliveries"]), 1)
            self.assertEqual(send.call_count, 1)
            self.assertTrue(drain(self.path)["complete"])
            self.assertEqual(send.call_count, 1)

    def test_transferred_program_never_contains_live_consumers_or_destinations(self):
        from catabolic.domain import CatabolicError
        from catabolic.manifest import Manifest
        from catabolic.notifications import configure
        from catabolic.program_bundle import Programs

        self.projection(self.save())
        attach(self.app, "query")
        configure(
            self.app,
            "private",
            "PRIVATE_NOTIFICATION_REF",
            ["projection_updated"],
            [],
            "info",
            apply=True,
        )
        program = Programs(self.app).export(projections=["query"])
        with self.store.transaction():
            manifest = Manifest(self.app).build("query")
        for value in (program, manifest):
            serialized = encode(value)
            self.assertNotIn("credential_env", serialized)
            self.assertNotIn("PRIVATE_NOTIFICATION_REF", serialized)
            self.assertNotIn("consumer_connections", serialized)
        malicious = dict(
            program, notification_destinations=[{"url": "json://untrusted"}]
        )
        with self.assertRaises(CatabolicError):
            Programs(self.app).import_bundle(malicious)


class ReceiptBoundaryTest(unittest.TestCase):
    setUp = test_catalog_refresh.ReceiptRefreshTest.setUp
    catalog = test_catalog_refresh.ReceiptRefreshTest.catalog
    receipt = test_catalog_refresh.ReceiptRefreshTest.receipt

    def test_receipt_completion_refresh_and_repeat(self):
        self.catalog()
        attach(self.app, "mobile")
        result = import_receipt(self.app, self.receipt())
        self.assertTrue(result["catalog_refresh"]["complete"])
        first = generations(self.app)
        self.assertGreater(first["generation"], 0)
        self.assertEqual(first["generation"], first["verified"])
        self.assertEqual(first["acknowledged"], 0)
        CatalogRefresh(self.app).run(force=True)
        self.assertEqual(first, generations(self.app))
