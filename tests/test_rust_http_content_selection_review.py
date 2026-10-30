# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Direct HTTP selection compared with the pinned owner on disposable catalogs."""
import unittest

from catabolic.app import Application
from catabolic.store import Store
from tests import test_rust_http_dynamic_grants as fixtures


class NativeHTTPContentSelectionReviewTest(unittest.TestCase):
    setUp = fixtures.NativeHTTPDynamicGrantsTest.setUp
    sql = staticmethod(fixtures.NativeHTTPDynamicGrantsTest.sql)
    graphql = staticmethod(fixtures.NativeHTTPDynamicGrantsTest.graphql)
    start_http = fixtures.NativeHTTPDynamicGrantsTest.start_http
    start_native_server = fixtures.NativeHTTPDynamicGrantsTest.start_native_server
    stop_server = fixtures.NativeHTTPDynamicGrantsTest.stop_server
    pair = fixtures.NativeHTTPDynamicGrantsTest.pair

    def test_direct_selection_skips_unavailable_candidate_and_respects_filters(self):
        with Store(self.database, writable=True) as store:
            Application(store).media.associate(self.hidden_file, self.item)
            with store.transaction() as db:
                # Force the inaccessible candidate ahead of the permitted one.
                db.execute("UPDATE item_files SET id='selection-first' WHERE item_id=? AND file_id=?", (self.item, self.hidden_file))
                db.execute("UPDATE item_files SET id='selection-second' WHERE item_id=? AND file_id=?", (self.item, self.file))
        self.start_http("sql-public")
        route = f"/v1/items/{self.item}/resolve"
        chosen = self.pair("POST", route, json={})
        self.assertEqual(chosen.status_code, 200)
        self.assertEqual(chosen.json()["file_id"], self.file)
        selected = self.pair("POST", route, json={"file_id": self.file})
        self.assertEqual(selected.status_code, 200)
        self.assertEqual(selected.json()["file_id"], self.file)
        denied = self.pair("POST", route, json={"file_id": self.hidden_file})
        self.assertEqual(denied.status_code, 409)
        missing = self.pair("POST", route, json={"definition_id": "missing-definition"})
        self.assertEqual(missing.status_code, 409)
