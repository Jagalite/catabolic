# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import json
import unittest
from importlib.resources import files
from unittest.mock import patch

from tests import test_http_backend as fixtures


@unittest.skipUnless(fixtures.HTTP_AVAILABLE, "optional HTTP dependencies")
class ContractTest(unittest.TestCase):
    setUp = fixtures.HTTPTest.setUp

    def test_every_operation_is_typed_unique_and_frozen(self):
        from fastapi.routing import APIRoute

        from catabolic.http.contract import VERSION

        schema = self.client.app.openapi()
        release = files("catabolic.http").joinpath("releases", VERSION)
        self.assertEqual(
            schema, json.loads(release.joinpath("openapi.json").read_text())
        )
        manifest = json.loads(release.joinpath("manifest.json").read_text())
        operations = {
            f"{method.upper()} {path}": value["operationId"]
            for path, methods in schema["paths"].items()
            for method, value in methods.items()
        }
        self.assertEqual(operations, manifest["operations"])
        self.assertEqual(len(operations), len(set(operations.values())))
        for route in self.client.app.routes:
            if isinstance(route, APIRoute) and not route.path.endswith("/content"):
                self.assertIsNotNone(route.response_model, route.path)
        for name, value in schema["components"]["schemas"].items():
            if name in ("HTTPValidationError", "ValidationError"):
                continue
            for field, definition in value.get("properties", {}).items():
                self.assertTrue(
                    definition.get("type")
                    or definition.get("anyOf")
                    or definition.get("oneOf")
                    or definition.get("$ref")
                    or definition.get("allOf"),
                    (name, field),
                )

    def test_graphql_artifact_and_runtime_have_identical_fields(self):
        import hashlib

        from graphql import build_schema, lexicographic_sort_schema, print_schema

        from catabolic.graphql_query import SDL
        from catabolic.http.contract import VERSION

        release = files("catabolic.http").joinpath("releases", VERSION)
        self.assertEqual(
            release.joinpath("schema.graphql").read_text(),
            print_schema(lexicographic_sort_schema(build_schema(SDL))) + "\n",
        )
        manifest = json.loads(release.joinpath("manifest.json").read_text())
        for name, digest in manifest["artifacts"].items():
            self.assertEqual(
                hashlib.sha256(release.joinpath(name).read_bytes()).hexdigest(), digest
            )
        live = self.client.get("/v1/openapi.json", headers=self.headers)
        self.assertEqual(live.status_code, 200, live.text[:200])
        self.assertEqual(live.json(), self.client.app.openapi())

    def test_public_envelopes_and_lossless_metadata(self):
        from catabolic.http import models

        for path, model in (
            ("/v1/me", models.Identity),
            ("/v1/capabilities", models.Capabilities),
            ("/v1/items", models.MetadataPage[models.Item]),
            ("/v1/files", models.MetadataPage[models.File]),
            ("/v1/renditions", models.MetadataPage[models.Rendition]),
            ("/v1/events", models.EventPage),
        ):
            response = self.client.get(path, headers=self.headers)
            self.assertEqual(response.status_code, 200, response.text)
            model.model_validate(response.json())
        from catabolic.store import Store

        with Store(self.database, writable=True) as store:
            with store.transaction() as db:
                db.execute(
                    "UPDATE observations SET size=?,mtime_ns=? WHERE file_id=?",
                    (2**60 + 1, 2**60 + 3, self.file),
                )
        response = self.client.get("/v1/files/" + self.file, headers=self.headers)
        value = models.File.model_validate(response.json())
        self.assertEqual(value.size, str(2**60 + 1))
        self.assertEqual(value.mtime_ns, str(2**60 + 3))
        self.assertNotIn("path", response.json())
        self.assertNotIn("location", response.json())

    def test_problem_and_graphql_error_shapes(self):
        from catabolic.http.models import GraphQLResult, Problem

        responses = [
            self.client.get("/v1/me"),
            self.client.get("/v1/items?limit=0", headers=self.headers),
            self.client.get("/v1/items/missing", headers=self.headers),
            self.client.post(
                "/v1/query/sql", headers=self.headers, json={"query": "SELECT 1"}
            ),
            self.client.post(
                "/v1/query/graphql", headers=self.headers, json={"query": "x" * 1048600}
            ),
        ]
        self.assertEqual([r.status_code for r in responses], [401, 422, 404, 403, 413])
        for response in responses:
            value = Problem.model_validate(response.json())
            self.assertEqual(value.status, response.status_code)
            self.assertEqual(
                response.headers["content-type"], "application/problem+json"
            )
        for query in ("{ items { nodes { id } } }", "{ nonexistent }"):
            response = self.client.post(
                "/v1/query/graphql", headers=self.headers, json={"query": query}
            )
            GraphQLResult.model_validate(response.json())
            if "nonexistent" in query:
                self.assertIn("errors", response.json())
            else:
                self.assertNotIn("errors", response.json())

    def test_schema_documents_binary_head_sse_and_admission(self):
        paths = self.client.app.openapi()["paths"]
        content = paths["/v1/files/{identifier}/content"]
        self.assertEqual(
            content["get"]["responses"]["206"]["content"]["*/*"]["schema"]["format"],
            "binary",
        )
        self.assertNotIn("content", content["head"]["responses"]["200"])
        self.assertNotIn("206", content["head"]["responses"])
        self.assertNotIn("content", content["get"]["responses"]["304"])
        self.assertIn({"ContentTicket": []}, content["get"]["security"])
        self.assertIn(
            "text/event-stream",
            paths["/v1/events"]["get"]["responses"]["200"]["content"],
        )
        demand = paths["/v1/rendition-requests"]["post"]
        self.assertIn("202", demand["responses"])
        self.assertIn("Idempotency-Key", [p["name"] for p in demand["parameters"]])
        self.assertEqual(
            paths["/v1/items"]["get"]["responses"]["422"]["content"][
                "application/problem+json"
            ]["schema"]["$ref"],
            "#/components/schemas/Problem",
        )

    def test_response_validation_rejects_uncontracted_fields(self):
        from fastapi.exceptions import ResponseValidationError

        with patch(
            "catabolic.access.catalog.Catalog.get",
            return_value={
                "id": self.item,
                "kind": "movie",
                "metadata": {},
                "private": "unexpected",
            },
        ):
            with self.assertRaises(ResponseValidationError):
                self.client.get("/v1/items/" + self.item, headers=self.headers)

    def test_saved_query_modes_and_operator_result_variants(self):
        import copy

        from catabolic.app import Application
        from catabolic.http.models import OperatorApplied, SQLResult
        from catabolic.layouts import PRESETS, Layouts
        from catabolic.operations import Operations
        from catabolic.saved_queries import Queries
        from catabolic.store import Store

        headers = {"Authorization": "Bearer " + self.operator}
        with Store(self.database, writable=True) as store:
            queries = Queries(store)
            selection = queries.put(
                "selection",
                {
                    "mode": "selection",
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS item_id FROM items",
                    },
                },
            )
            rows = queries.put(
                "rows",
                {
                    "mode": "rows",
                    "selection": {"language": "sql", "query": "SELECT 1 AS number"},
                },
            )
            document = queries.put(
                "document",
                {
                    "mode": "document",
                    "selection": {
                        "language": "graphql",
                        "query": "{ items { nodes { id } } }",
                    },
                },
            )
            app = Application(store)
            Layouts(app).put("flat", copy.deepcopy(PRESETS["flat"]))
            operation = Operations(app).put(
                "hash", {"kind": "analysis", "operation": "hash"}
            )
        for saved in (selection, rows, document):
            result = self.client.post(
                f"/v1/queries/{saved['id']}/runs", headers=headers, json={}
            )
            self.assertEqual(result.status_code, 200, result.text)
            if saved == rows:
                self.assertEqual(SQLResult.model_validate(result.json()).rows, [[1]])
        for resource, name, definition in (
            (
                "queries",
                "public",
                {
                    "mode": "selection",
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS item_id FROM items",
                    },
                },
            ),
            (
                "rules",
                "hash",
                {
                    "recipe_id": operation["id"],
                    "location": None,
                    "selection": {"query_id": selection["id"]},
                },
            ),
            ("projections", "global", {"query_id": selection["id"], "layout": "flat"}),
        ):
            path = f"/v1/operator/{resource}/{name}"
            preview = self.client.post(
                path, headers=headers, json={"definition": definition}
            )
            self.assertEqual(preview.status_code, 200, preview.text)
            applied = self.client.post(
                path,
                headers=headers,
                json={
                    "definition": definition,
                    "apply": True,
                    "expected_plan": preview.json()["plan_id"],
                },
            )
            self.assertEqual(applied.status_code, 200, applied.text)
            OperatorApplied.model_validate(applied.json())
