# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import unittest

from catabolic.access import grant_put
from catabolic.app import Application
from catabolic.fallback_policies import Policies
from catabolic.saved_queries import Queries
from catabolic.store import Store
from tests import test_http_backend as fixtures


@unittest.skipUnless(fixtures.HTTP_AVAILABLE, "optional HTTP dependencies")
class FallbackHTTPTest(unittest.TestCase):
    setUp = fixtures.HTTPTest.setUp

    def configure(self):
        with Store(self.database, writable=True) as store:
            Application(store).media.associate(self.hidden_file, self.item)
            tiers = []
            for path in ("hidden.bin", "allowed.bin"):
                query = Queries(store).put(
                    path,
                    {
                        "selection": {
                            "language": "sql",
                            "query": "SELECT id AS file_id FROM files WHERE path=:path",
                            "params": {"path": path},
                        }
                    },
                )
                tiers.append({"name": path, "query_id": query["id"]})
            policy = Policies(store).put(
                "http", {"fallbacks": tiers, "within_tier": {"tie_break": "file_id"}}
            )["id"]
            grant_put(
                store,
                "site",
                {**self.definition, "fallback_policy_ids": [policy]},
                self.grant,
            )
        return policy

    def test_policy_approval_does_not_grant_candidate_access(self):
        policy = self.configure()
        response = self.client.post(
            f"/v1/items/{self.item}/resolve",
            headers=self.headers,
            json={"fallback_policy_id": policy},
        )
        self.assertEqual(response.status_code, 200, response.text)
        self.assertEqual(response.json()["file_id"], self.file)
        self.assertEqual(response.json()["selected_tier"], 1)
        self.assertNotIn(self.hidden_file, response.text)
        self.assertNotIn(str(self.source), response.text)
        self.assertEqual(
            self.client.get(
                response.json()["content_path"], headers=self.headers
            ).content,
            self.payload,
        )
        denied = self.client.post(
            f"/v1/items/{self.hidden}/resolve",
            headers=self.headers,
            json={"fallback_policy_id": policy},
        )
        self.assertEqual(denied.status_code, 404)
        mixed = self.client.post(
            f"/v1/items/{self.item}/resolve",
            headers=self.headers,
            json={"fallback_policy_id": policy, "file_id": self.file},
        )
        self.assertEqual(mixed.status_code, 422)

    def test_policy_query_uses_restricted_sql_and_graphql_is_scoped(self):
        policy = self.configure()
        response = self.client.post(
            "/v1/query/graphql",
            headers=self.headers,
            json={
                "query": "query($id:ID!){ fallbackPolicy(id:$id){ id name tiers } }",
                "variables": {"id": policy},
            },
        )
        self.assertNotIn("errors", response.json(), response.text)
        with Store(self.database, writable=True) as store:
            query = Queries(store).put(
                "private",
                {
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS file_id FROM api_tokens",
                    }
                },
            )
            private = Policies(store).put(
                "private", {"fallbacks": [{"name": "private", "query_id": query["id"]}]}
            )["id"]
            grant_put(
                store,
                "site",
                {**self.definition, "fallback_policy_ids": [private]},
                self.grant,
            )
        response = self.client.post(
            f"/v1/items/{self.item}/resolve",
            headers=self.headers,
            json={"fallback_policy_id": private},
        )
        self.assertEqual(response.json()["state"], "blocked", response.text)
        denied = self.client.post(
            "/v1/query/graphql",
            headers=self.headers,
            json={
                "query": "query($id:ID!){ fallbackPolicy(id:$id){ id } }",
                "variables": {"id": policy},
            },
        )
        self.assertIn("FORBIDDEN", denied.text)
