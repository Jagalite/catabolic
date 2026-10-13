# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Exact skipped-object completion pairs on disposable native query surfaces.

The extension path must identify a copied development build. These focused
checks are not an installed-wheel or complete GraphQL qualification.
"""

import hashlib
import json
import os
import subprocess
import sys
import unittest
from pathlib import Path

from tests import test_rust_legacy_inbox as fixture

FIXTURE = json.loads(
    (fixture.ROOT / "tests/parity/graphql-empty-completion.json").read_text()
)
CASES = FIXTURE["cases"]

EMBEDDING = r"""
import json, pathlib, sys
sys.path.insert(0, sys.argv[1])
import catabolic_native as native
from catabolic_native import graphql_query
from catabolic_native.store import Store
path, cases = sys.argv[2], json.load(sys.stdin)
results = []
raw = native._native.NativeStore(path, False, False)
try:
    with Store(path) as store:
        for case in cases:
            doc, variables = case["document"], case["variables"]
            operation = case.get("operation_name")
            options = dict(variables=variables, operation_name=operation)
            request = json.dumps(dict(document=doc, **options))
            results.append(dict(
                name=case["name"],
                direct_raw=json.loads(native._native.execute_graphql(path, doc, "default", json.dumps(variables), operation, 5000)),
                package_root=native.execute_graphql(path, doc, **options),
                persistent_raw=json.loads(raw.call("query_execution", "graphql", request, "default")),
                persistent_public=graphql_query.execute_graphql(path, doc, _store=store, **options),
            ))
finally:
    raw.close()
print(json.dumps(results))
"""


class EmptyCompletionTest(unittest.TestCase):
    setUp = fixture.NativeLegacyInboxTest.setUp
    invoke = fixture.NativeLegacyInboxTest.invoke

    def test_frozen_legacy_and_companion_complete_responses(self):
        binary = Path(os.environ["CATABOLIC_NATIVE_BINARY"])
        before = hashlib.sha256(self.native.read_bytes()).hexdigest()
        for case in CASES:
            with self.subTest(case=case["name"]):
                args = [
                    "graphql",
                    case["document"],
                    "--variables",
                    json.dumps(case["variables"]),
                ]
                if case.get("operation_name"):
                    args += ["--operation-name", case["operation_name"]]
                for reference in (True, False):
                    result = self.invoke(reference, ["--machine", *args])
                    self.assertEqual(result.returncode, case["exit_code"])
                    self.assertEqual(result.stderr, case["stderr"])
                    self.assertEqual(json.loads(result.stdout), case["expected"])
                companion = subprocess.run(
                    [str(binary), "--db", str(self.native), "--json", *args],
                    capture_output=True,
                    text=True,
                    env=self.env,
                    timeout=90,
                )
                # The companion returns GraphQL error envelopes with exit zero.
                self.assertEqual(companion.returncode, 0, companion.stderr)
                self.assertEqual(companion.stderr, "")
                self.assertEqual(json.loads(companion.stdout), case["expected"]["data"])
        self.assertEqual(hashlib.sha256(self.native.read_bytes()).hexdigest(), before)

    def test_direct_and_persistent_embedding_complete_responses(self):
        root = Path(os.environ["CATABOLIC_COMPLETION_EXTENSION_ROOT"])
        self.assertTrue((root / "catabolic_native/_native.abi3.so").is_file())
        before = hashlib.sha256(self.native.read_bytes()).hexdigest()
        result = subprocess.run(
            [
                sys.executable,
                "-c",
                EMBEDDING,
                str(root),
                str(self.native),
            ],
            input=json.dumps(CASES),
            capture_output=True,
            text=True,
            env=self.env,
            timeout=90,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        rows = json.loads(result.stdout)
        self.assertEqual(len(rows), len(CASES))
        for case, row in zip(CASES, rows, strict=True):
            self.assertEqual(row.pop("name"), case["name"])
            for surface, response in row.items():
                with self.subTest(case=case["name"], surface=surface):
                    self.assertEqual(response, case["expected"]["data"])
        self.assertEqual(hashlib.sha256(self.native.read_bytes()).hexdigest(), before)

    def test_actual_loopback_http_and_scoped_absence(self):
        import httpx
        from fastapi.testclient import TestClient

        # The test invocation must select the pinned Python oracle, not a
        # source-tree or installed native replacement for the HTTP reference.
        import catabolic
        from catabolic.access import grant_put, issue, principal_put
        from catabolic.http.app import create_app
        from catabolic.store import Store
        from tests.test_rust_http_transport import NativeHTTPTransportTest

        self.assertEqual(
            Path(catabolic.__file__).resolve().parent.parent,
            (fixture.parser_fixture.REFERENCE / "src").resolve(),
        )
        self.database = self.native
        with Store(self.database, writable=True) as store:
            principal_put(store, "default", "completion-operator")
            operator = grant_put(
                store, "completion-operator", {"actions": ["*"], "operator": True}
            )["grant_id"]
            operator = issue(store, "completion-operator", [operator])["token"]
            principal_put(store, "default", "completion-scoped")
            scoped = grant_put(
                store,
                "completion-scoped",
                {"actions": ["metadata:read"], "item_ids": ["a", "b"]},
            )["grant_id"]
            scoped = issue(store, "completion-scoped", [scoped])["token"]
        self.stop_server = lambda: NativeHTTPTransportTest.stop_server(self)
        NativeHTTPTransportTest.start_native_server(self)
        self.assertIsInstance(self.client, httpx.Client)
        with TestClient(create_app(self.database, read_only=True)) as reference:
            for case in CASES:
                if not case["http"]:
                    continue
                with self.subTest(case=case["name"]):
                    body = dict(query=case["document"], variables=case["variables"])
                    if case.get("operation_name"):
                        body["operationName"] = case["operation_name"]
                    headers = {"Authorization": "Bearer " + operator}
                    expected = reference.post(
                        "/v1/query/graphql", json=body, headers=headers
                    )
                    actual = self.client.post(
                        "/v1/query/graphql", json=body, headers=headers
                    )
                    self.assertEqual(actual.status_code, expected.status_code)
                    self.assertEqual(actual.json(), expected.json())
                    self.assertEqual(actual.json(), case["expected"]["data"])
            for case in FIXTURE["http_scoped_cases"]:
                with self.subTest(scoped=case["name"]):
                    body = {"query": case["document"]}
                    headers = {"Authorization": "Bearer " + scoped}
                    expected = reference.post(
                        "/v1/query/graphql", json=body, headers=headers
                    )
                    actual = self.client.post(
                        "/v1/query/graphql", json=body, headers=headers
                    )
                    self.assertEqual(actual.status_code, case["status"])
                    self.assertEqual(actual.status_code, expected.status_code)
                    self.assertEqual(actual.json(), expected.json())
                    self.assertEqual(actual.json(), case["expected"])


if __name__ == "__main__":
    unittest.main()
