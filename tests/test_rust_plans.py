# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Typed plan dependencies, query evidence and semantic baseline parity."""

import json
import subprocess
import tempfile
import unittest
from pathlib import Path

from catabolic import plans
from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.fallback_policies import Policies
from catabolic.layouts import PRESETS, Layouts
from catabolic.saved_queries import Queries
from catabolic.store import Store
from tests.test_media_model import populate_media
from tests.test_rust_migration import BINARY


@unittest.skipUnless(BINARY.is_file(), "build catabolic-cli before qualification")
class NativePlansTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.path = self.root / "catalog.sqlite3"
        Store.initialize(self.path)
        with Store(self.path, writable=True) as store:
            app = Application(store)
            self.files, _ = populate_media(app, self.root)
            self.queries = {}
            for mode, entity, language, query in (
                ("selection", "item_id", "sql", "SELECT item_id FROM catalog_items"),
                (
                    "selection",
                    "occurrence_id",
                    "sql",
                    "SELECT occurrence_id FROM catalog_component_occurrences",
                ),
                (
                    "rows",
                    None,
                    "sql",
                    "SELECT item_id,kind FROM catalog_items ORDER BY item_id",
                ),
                ("document", None, "graphql", "{items(first:2){nodes{id kind}}}"),
            ):
                name = entity or mode
                definition = {
                    "mode": mode,
                    "selection": {"language": language, "query": query},
                }
                if entity:
                    definition["entity"] = entity
                self.queries[name] = Queries(store).put(name, definition)["id"]
            self.queries["composed"] = Queries(store).put(
                "composed",
                {
                    "combine": "intersection",
                    "queries": [self.queries["item_id"], self.queries["item_id"]],
                },
            )["id"]
            self.policy = Policies(store).put(
                "policy",
                {"fallbacks": [{"name": "all", "query_id": self.queries["item_id"]}]},
            )["id"]
            Layouts(app).put("flat", PRESETS["flat"])
            with store.transaction() as db:
                db.execute(
                    "INSERT INTO fallback_bindings(profile,catalog,query_id,policy_id,layout) VALUES (?,?,?,?,?)",
                    ("default", "global", self.queries["item_id"], self.policy, "flat"),
                )

    def native(self, action, args, *, ok=True):
        result = subprocess.run(
            [
                str(BINARY),
                "--db",
                str(self.path),
                "--json",
                "plans",
                action,
                json.dumps(args),
            ],
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(result.returncode, 0 if ok else 2, result.stderr)
        return (
            json.loads(result.stdout)
            if ok
            else result.stderr.strip().removeprefix("catabolic: ")
        )

    def test_description_requirements_defaults_and_projection_staleness(self):
        with Store(self.path) as store:
            app = Application(store)
            digest = plans.projection_digest(app, "global")
            self.assertEqual(
                self.native("projection-digest", {"catalog": "global"}), digest
            )
            references = [
                {"kind": "query", "query_id": self.queries["composed"]},
                {
                    "kind": "observation",
                    "sources": ["media", "media"],
                    "exclusions": ["b", "a", "b"],
                },
                {
                    "kind": "fallback",
                    "membership_query_id": self.queries["item_id"],
                    "policy_id": self.policy,
                },
                {"kind": "projection", "catalog": "global", "binding_digest": digest},
            ]
            for reference in references:
                with self.subTest(reference=reference):
                    self.assertEqual(
                        self.native("normalize", {"reference": reference}),
                        plans.PLAN.validate_python(reference).model_dump(),
                    )
                    self.assertEqual(
                        self.native("describe", {"reference": reference}),
                        plans.describe(app, reference),
                    )
                    self.assertEqual(
                        self.native("requirements", {"reference": reference}),
                        plans.observation_requirements(app, reference),
                    )
            reference = {
                "kind": "projection",
                "catalog": "global",
                "binding_digest": "stale",
            }
            with self.assertRaises(CatabolicError) as expected:
                plans.describe(app, reference)
            self.assertEqual(
                self.native("describe", {"reference": reference}, ok=False),
                str(expected.exception),
            )

    def test_query_modes_and_component_evidence_share_selection_snapshot(self):
        before = self.path.read_bytes()
        for query in self.queries.values():
            reference = {"kind": "query", "query_id": query}
            with self.subTest(query=query), Store(self.path) as store:
                expected = plans.evaluate(Application(store), reference)
            actual = self.native("evaluate", {"reference": reference})
            self.assertEqual(actual, expected)
            self.assertEqual(
                self.native("compare", {"previous": None, "current": actual}),
                plans.compare(None, expected),
            )
        self.assertEqual(before, self.path.read_bytes())

    def test_semantic_filtering_and_incomplete_baseline_refusal(self):
        decision = {
            "entry_id": "entry",
            "file_id": "file",
            "revision": "rev",
            "path": "name",
            "state": "satisfied",
            "evidence": {"output_digest": "abc", "diagnostic": "volatile"},
            "timing": "ignored",
        }
        for kind, value in (
            ("fallback", {"decisions": [decision]}),
            ("projection", {"desired": [decision], "removed": [], "safe": True}),
            ("rows", {"columns": ["a"], "rows": [[1]], "timing": 25}),
            ("document", {"data": {"a": 1}, "extensions": {"cost": 2}}),
        ):
            result = {"kind": kind, "value": value, "complete": True}
            expected = plans.semantic(result)
            self.assertEqual(self.native("semantic", {"result": result}), expected)
            self.assertEqual(
                self.native("compare", {"previous": expected, "current": result}),
                {"changed": False, "baseline": expected},
            )
            result["complete"] = False
            self.assertEqual(
                self.native("compare", {"previous": None, "current": result}, ok=False),
                "incomplete_evaluation_has_no_baseline",
            )
