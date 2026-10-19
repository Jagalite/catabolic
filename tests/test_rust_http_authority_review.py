# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Independent HTTP authority boundary regressions against frozen 1.7.

Run with frozen 453fca983222c6665a48775eef67c489c96b527e source first
on PYTHONPATH. Fixtures and the oracle database are independent from the
experimental native loopback listener.
"""

import json
import os
import shutil
import unittest
from pathlib import Path
from unittest.mock import patch

from catabolic.access import grant_put, issue
from catabolic.app import Application
from catabolic.fallback_policies import Policies
from catabolic.saved_queries import Queries
from catabolic.store import Store, encode
from tests import test_http_renditions as rendition_fixtures
from tests import test_rust_http_dynamic_grants as fixtures
from tests import test_rust_http_transport as transport


class NativeHTTPAuthorityReviewTest(unittest.TestCase):
    setUp = fixtures.NativeHTTPDynamicGrantsTest.setUp
    sql = staticmethod(fixtures.NativeHTTPDynamicGrantsTest.sql)
    graphql = staticmethod(fixtures.NativeHTTPDynamicGrantsTest.graphql)
    start_http = fixtures.NativeHTTPDynamicGrantsTest.start_http
    start_native_server = fixtures.NativeHTTPDynamicGrantsTest.start_native_server
    stop_server = fixtures.NativeHTTPDynamicGrantsTest.stop_server
    pair = fixtures.NativeHTTPDynamicGrantsTest.pair
    assert_private = fixtures.NativeHTTPDynamicGrantsTest.assert_private
    assert_items = fixtures.NativeHTTPDynamicGrantsTest.assert_items
    update_grant = fixtures.NativeHTTPDynamicGrantsTest.update_grant

    def test_approved_selection_and_membership_have_independent_id_budgets(self):
        # The saved report and grant each complete 5,002 IDs within their own
        # 10,000-ID contract; evaluating both must not double-charge one budget.
        with Store(self.database, writable=True) as store:
            with store.transaction() as db:
                db.executemany(
                    "INSERT INTO items(id,kind,metadata) VALUES (?,?,?)",
                    [
                        (
                            f"review-{index:05d}",
                            "movie",
                            encode(
                                {
                                    "title": f"Review item {index}",
                                    "private_note": "DO_NOT_DISCLOSE",
                                }
                            ),
                        )
                        for index in range(5000)
                    ],
                )
        self.start_http("sql-public", reports=["sql-public"])
        page = self.pair("GET", "/v1/items")
        self.assertEqual(page.status_code, 200)
        report = self.pair(
            "POST", "/v1/queries/" + self.queries["sql-public"] + "/runs", json={}
        )
        self.assertEqual(report.status_code, 200)
        self.assertEqual(len(report.json()["ids"]), 100)
        self.assertTrue(report.json()["has_more"])
        self.assert_private(report)

    def test_each_dynamic_grant_retains_its_own_small_id_budget(self):
        with Store(self.database, writable=True) as store:
            for name, item in (
                ("review-first", self.item),
                ("review-second", self.second),
            ):
                definition = self.sql(
                    "SELECT id AS item_id FROM items WHERE id=:id", {"id": item}
                )
                definition["max_ids"] = 1
                self.queries[name] = Queries(store).put(name, definition)["id"]
            second = grant_put(
                store,
                "site",
                {
                    "actions": ["metadata:read"],
                    "query_id": self.queries["review-second"],
                    "metadata_fields": ["title"],
                },
            )["grant_id"]
            self.credential = issue(store, "site", [self.grant, second])
            self.headers = {"Authorization": "Bearer " + self.credential["token"]}
        self.start_http("review-first")
        self.assert_items([self.item, self.second])

    def test_administrator_paged_selection_can_raise_default_id_budget(self):
        with Store(self.database, writable=True) as store:
            with store.transaction() as db:
                db.executemany(
                    "INSERT INTO items(id,kind,metadata) VALUES (?,?,?)",
                    [
                        (
                            f"review-{index:05d}",
                            "movie",
                            encode(
                                {
                                    "title": f"Item {index}",
                                    "private_note": "DO_NOT_DISCLOSE",
                                }
                            ),
                        )
                        for index in range(10000)
                    ],
                )
            definition = self.sql(
                "SELECT id AS item_id FROM items WHERE json_extract(metadata,'$.private_note')=:marker",
                {"marker": "DO_NOT_DISCLOSE"},
            )
            definition["max_ids"] = 20000
            definition["selection"].update(page_size=1000, max_ids=20000)
            self.queries["review-large"] = Queries(store).put(
                "review-large", definition
            )["id"]
        self.start_http("review-large", reports=["review-large"])
        for method, endpoint in (
            ("GET", "/v1/items"),
            ("POST", "/v1/queries/" + self.queries["review-large"] + "/runs"),
        ):
            with self.subTest(endpoint=endpoint):
                result = self.pair(
                    method, endpoint, **({"json": {}} if method == "POST" else {})
                )
                self.assertEqual(result.status_code, 200)
                self.assert_private(result)

    def test_nonnull_private_fields_bubble_only_to_nearest_nullable_boundary(self):
        self.start_http("graphql-public")
        documents = [
            "{files{nodes{id facts}} profile}",
            "{items{nodes{id identities{namespace}}} profile}",
            "{items{nodes{id associations{nodes{id file{id path}}}}} profile}",
            "{left:files{nodes{private:location}} right:items{nodes{identities{namespace}}}}",
            "{items{nodes{id associations{nodes{file{...Private}}}}}} fragment Private on File{id path}",
        ]
        for document in documents:
            with self.subTest(document=document):
                result = self.pair(
                    "POST", "/v1/query/graphql", json={"query": document}
                )
                self.assertEqual(result.status_code, 200)
                self.assertIn("errors", result.json())
                self.assert_private(result)

    def start_policy_http(self, policy):
        native = Path(
            os.environ.get("CATABOLIC_NATIVE_BINARY", "target/debug/catabolic-native")
        ).resolve()
        with patch.dict(os.environ, {"CATABOLIC_NATIVE_BINARY": str(native)}):
            self.start_http("sql-public")
        self.update_grant(fallback_policy_ids=[policy])

    def test_ordinary_policy_graphql_uses_global_ids_then_content_authority(self):
        with Store(self.database, writable=True) as store:
            query = Queries(store).put(
                "review-policy", self.graphql(marker="DO_NOT_DISCLOSE")
            )["id"]
            policy = Policies(store).put(
                "review-policy", {"fallbacks": [{"name": "primary", "query_id": query}]}
            )["id"]
        self.start_policy_http(policy)
        result = self.pair(
            "POST",
            f"/v1/items/{self.item}/resolve",
            json={"fallback_policy_id": policy},
        )
        self.assertEqual(result.status_code, 200)
        self.assertEqual(result.json()["file_id"], self.file)
        self.assert_private(result)
        direct = self.pair(
            "POST",
            "/v1/query/graphql",
            json={
                "query": '{items(metadata:["private_note=\\"DO_NOT_DISCLOSE\\""]){nodes{id}}}'
            },
        )
        self.assertEqual(direct.json()["data"]["items"]["nodes"], [])

    def test_policy_tiers_keep_shared_budget_above_each_leaf_id_limit(self):
        with Store(self.database, writable=True) as store:
            Application(store).media.associate(self.hidden_file, self.item)
            tiers = []
            for index, file in enumerate((self.hidden_file, self.file)):
                definition = self.sql(
                    "SELECT id AS file_id FROM files WHERE id=:id", {"id": file}
                )
                definition["max_ids"] = 1
                query = Queries(store).put(f"review-tier-{index}", definition)["id"]
                tiers.append({"name": f"tier-{index}", "query_id": query})
            policy = Policies(store).put("review-tiers", {"fallbacks": tiers})["id"]
        self.start_policy_http(policy)
        result = self.pair(
            "POST",
            f"/v1/items/{self.item}/resolve",
            json={"fallback_policy_id": policy},
        )
        self.assertEqual(result.status_code, 200)
        self.assertEqual(result.json()["file_id"], self.file)
        self.assertEqual(result.json()["selected_tier"], 1)
        self.assert_private(result)


@unittest.skipUnless(
    shutil.which("ffmpeg"), "real FFmpeg required for disposable fixture"
)
class NativeHTTPProcessingAuthorityReviewTest(unittest.TestCase):
    start_native_server = transport.NativeHTTPTransportTest.start_native_server
    stop_server = transport.NativeHTTPTransportTest.stop_server

    def setUp(self):
        from fastapi.testclient import TestClient

        from catabolic.http.app import create_app

        rendition_fixtures.RenditionHTTPTest.setUp(self)
        self.client.close()
        with Store(self.database, writable=True) as store:
            self.queries = {}
            for language, selection in (
                (
                    "sql",
                    {"language": "sql", "query": "SELECT id AS item_id FROM items"},
                ),
                (
                    "graphql",
                    fixtures.NativeHTTPDynamicGrantsTest.graphql()["selection"],
                ),
            ):
                self.queries[language] = Queries(store).put(
                    "review-processing-" + language, {"selection": selection}
                )["id"]
            for row in store.rows(
                "SELECT id,definition FROM api_grants WHERE principal='one'"
            ):
                definition = json.loads(row["definition"])
                if "processing:request" in definition["actions"]:
                    self.grant, self.definition = row["id"], definition
        self.oracle_database = self.root / "frozen-processing.db"
        shutil.copyfile(self.database, self.oracle_database)
        self.oracle = TestClient(create_app(self.oracle_database, read_only=False))
        self.addCleanup(self.oracle.close)
        self.start_native_server(env=os.environ.copy())

    def test_dynamic_membership_does_not_make_mutation_owner_read_only(self):
        for language in ("sql", "graphql"):
            with self.subTest(language=language):
                definition = {**self.definition, "query_id": self.queries[language]}
                definition.pop("item_ids", None)
                for path in (self.database, self.oracle_database):
                    with Store(path, writable=True) as store:
                        grant_put(store, "one", definition, self.grant)
                headers = {**self.headers[0], "Idempotency-Key": "review-" + language}
                expected = self.oracle.post(
                    "/v1/rendition-requests", headers=headers, json=self.body
                )
                actual = self.client.post(
                    "/v1/rendition-requests", headers=headers, json=self.body
                )
                self.assertEqual(expected.status_code, 202, expected.text)
                self.assertEqual(actual.status_code, expected.status_code, actual.text)
                self.assertEqual(actual.json()["state"], "queued")
                status = self.client.get(actual.json()["status_url"], headers=headers)
                self.assertEqual(status.status_code, 200, status.text)
                self.assertEqual(status.json()["state"], "queued")


if __name__ == "__main__":
    unittest.main()
