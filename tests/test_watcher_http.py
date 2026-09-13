# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import unittest

from catabolic.saved_queries import Queries
from catabolic.store import Store
from tests import test_http_backend as fixtures


@unittest.skipUnless(fixtures.HTTP_AVAILABLE, "optional HTTP dependencies")
class WatcherHTTPTest(unittest.TestCase):
    setUp = fixtures.HTTPTest.setUp

    def test_report_session_preserves_scoped_authority(self):
        from catabolic.access import authenticate
        from catabolic.domain import CatabolicError
        from catabolic.evaluation import EvaluationSession
        from catabolic.selection import select_ids

        with Store(self.database, writable=True) as store:
            access = authenticate(store, self.credential["token"])
            query = Queries(store).put(
                "document",
                {
                    "mode": "document",
                    "selection": {
                        "language": "graphql",
                        "query": "{ items { nodes { id title } } }",
                    },
                },
            )["id"]
            result = Queries(store).run(
                query, session=EvaluationSession(store, access=access)
            )
            self.assertEqual(
                [r["id"] for r in result["data"]["items"]["nodes"]], [self.item]
            )
            query = Queries(store).put(
                "sql",
                {
                    "mode": "rows",
                    "selection": {"language": "sql", "query": "SELECT id FROM files"},
                },
            )["id"]
            with self.assertRaisesRegex(CatabolicError, "authority"):
                Queries(store).run(
                    query, session=EvaluationSession(store, access=access)
                )
            query = Queries(store).put(
                "selection",
                {
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS file_id FROM files",
                    }
                },
            )["id"]
            with self.assertRaisesRegex(CatabolicError, "authority"):
                select_ids(store, {"query_id": query}, access=access)

    def test_typed_management_auth_and_admission(self):
        with Store(self.database, writable=True) as store:
            query = Queries(store).put(
                "audit",
                {
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS file_id FROM files",
                    }
                },
            )["id"]
        body = {"plan": {"kind": "query", "query_id": query}}
        path = "/v1/operator/watchers/audit"
        self.assertEqual(
            self.client.put(path, json=body, headers=self.headers).status_code, 403
        )
        headers = {"Authorization": "Bearer " + self.operator}
        response = self.client.put(path, json=body, headers=headers)
        self.assertEqual(response.status_code, 200, response.text)
        self.assertFalse(response.json()["enabled"])
        response = self.client.get("/v1/operator/watchers", headers=headers)
        self.assertEqual(response.status_code, 200, response.text)
        self.assertEqual(response.json()["watchers"][0]["name"], "audit")
        response = self.client.post(path + "/runs", headers=headers)
        self.assertEqual(response.status_code, 202, response.text)
        with Store(self.database) as store:
            self.assertEqual(store.rows("SELECT * FROM watcher_runs"), [])
        from catabolic.supervisor import tick

        report = tick(self.database)
        self.assertTrue(report["complete"], report)
        history = self.client.get(path + "/history", headers=headers)
        self.assertEqual(history.status_code, 200, history.text)
        self.assertEqual(len(history.json()["runs"]), 1)

    def test_observation_only_http_admission_does_not_execute_inline(self):
        with Store(self.database) as store:
            source = store.rows(
                "SELECT owner FROM bindings WHERE kind='source' LIMIT 1"
            )[0]["owner"]
            scans = store.rows("SELECT count(*) AS n FROM scans")[0]["n"]
        body = {"plan": {"kind": "observation", "sources": [source]}}
        path = "/v1/operator/watchers/inventory"
        self.assertEqual(
            self.client.put(path, json=body, headers=self.headers).status_code, 403
        )
        headers = {"Authorization": "Bearer " + self.operator}
        created = self.client.put(path, json=body, headers=headers)
        self.assertEqual(created.status_code, 200, created.text)
        self.assertIsNone(created.json()["definition"]["reaction"])
        self.assertEqual(
            self.client.post(path + "/runs", headers=headers).status_code, 202
        )
        with Store(self.database) as store:
            self.assertEqual(
                store.rows("SELECT count(*) AS n FROM scans")[0]["n"], scans
            )
            self.assertEqual(store.rows("SELECT * FROM watcher_runs"), [])
        from catabolic.supervisor import tick

        report = tick(self.database)
        self.assertTrue(report["complete"], report)
        history = self.client.get(path + "/history", headers=headers).json()["runs"]
        self.assertEqual(len(history), 1)
        self.assertIsNone(history[0]["result"]["reaction"])
