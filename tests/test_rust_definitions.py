# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Differential native M4 definition mutations, errors and atomic rollback.

Python creates disposable fixtures and independently applies the reference change.
Native mutations run only through the compiled executable, with no Python fallback.
"""

import copy
import json
import select
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path
from unittest.mock import patch

from catabolic.app import Application
from catabolic.copy_selection import CopySelection
from catabolic.domain import CatabolicError
from catabolic.layouts import PRESETS, Layouts
from catabolic.operations import Operations
from catabolic.projections import Projections
from catabolic.rendition_publication import Publication
from catabolic.rules import Rules
from catabolic.saved_queries import Queries
from catabolic.store import Store
from tests.test_rust_migration import BINARY


def selection(query="SELECT item_id FROM catalog_items", **kwargs):
    return {"language": "sql", "query": query, **kwargs}


@unittest.skipUnless(BINARY.is_file(), "build catabolic-cli before qualification")
class NativeDefinitionsTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.path = self.root / "native.sqlite3"
        self.reference_path = self.root / "reference.sqlite3"
        Store.initialize(self.path)
        with Store(self.path, writable=True) as store:
            app = Application(store)
            app.add_profile("laptop")
            with store.transaction() as db:
                db.execute("INSERT OR IGNORE INTO catalogs VALUES ('global')")
            self.operation = Operations(app).put(
                "hash", {"kind": "analysis", "operation": "hash", "options": {}}
            )["id"]
        self.worker = subprocess.Popen(
            [str(BINARY), "--stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(self.close_worker)

    def close_worker(self):
        self.worker.stdin.close()
        try:
            self.worker.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.worker.kill()
            self.worker.wait()
        self.worker.stdout.close()
        self.worker.stderr.close()

    def native(self, action, args=None, *, ok=True, profile="default"):
        self.worker.stdin.write(
            json.dumps(
                [
                    "--db",
                    str(self.path),
                    "--profile",
                    profile,
                    "definitions",
                    action,
                    json.dumps(args or {}),
                ]
            )
            + "\n"
        )
        self.worker.stdin.flush()
        self.assertTrue(select.select([self.worker.stdout], [], [], 120)[0])
        line = self.worker.stdout.readline()
        self.assertTrue(line, "native worker terminated")
        result = json.loads(line)
        self.assertEqual(result["status"], 0 if ok else 2, result)
        return result["result"] if ok else result["error"].removeprefix("catabolic: ")

    def assert_values(self, native, reference):
        # SQLite creates new timestamps in each independent execution. All other
        # values, including IDs pinned by the fixture, are compared exactly.
        if isinstance(native, dict):
            self.assertEqual(set(native), set(reference))
            for key in native:
                if key == "created_at":
                    for value in (native[key], reference[key]):
                        timestamp = datetime.strptime(value, "%Y-%m-%d %H:%M:%S")
                        self.assertLess(
                            abs(
                                (
                                    datetime.now(timezone.utc).replace(tzinfo=None)
                                    - timestamp
                                ).total_seconds()
                            ),
                            120,
                        )
                else:
                    self.assert_values(native[key], reference[key])
        elif isinstance(native, list):
            self.assertEqual(len(native), len(reference))
            for first, second in zip(native, reference, strict=True):
                self.assert_values(first, second)
        else:
            self.assertIs(type(native), type(reference))
            self.assertEqual(native, reference)

    def differential(self, action, args, call, uuid_module=None):
        shutil.copyfile(self.path, self.reference_path)
        actual = self.native(action, args)
        with Store(self.reference_path, writable=True) as store:
            app = Application(store)
            if uuid_module:
                with patch(uuid_module + ".uuid4", return_value=actual["id"]):
                    expected = call(app)
            else:
                expected = call(app)
        self.assert_values(actual, expected)
        with Store(self.path) as native, Store(self.reference_path) as reference:
            for table in (
                "saved_queries",
                "query_dependencies",
                "processing_rules",
                "meta",
                "copy_policies",
                "rendition_policies",
                "projection_bindings",
            ):
                self.assert_values(
                    native.rows(f"SELECT * FROM {table} ORDER BY 1,2"),
                    reference.rows(f"SELECT * FROM {table} ORDER BY 1,2"),
                )
            self.assertEqual(native.rows("PRAGMA foreign_key_check"), [])
            self.assertEqual(
                native.rows("PRAGMA integrity_check"), [{"integrity_check": "ok"}]
            )
        return actual

    def assert_error(self, action, args, call):
        before = self.path.read_bytes()
        with Store(self.path, writable=True) as store:
            with self.assertRaises(CatabolicError) as expected:
                call(Application(store))
        self.assertEqual(self.native(action, args, ok=False), str(expected.exception))
        self.assertEqual(self.path.read_bytes(), before)

    def test_query_revisions_composition_dependencies_and_profile_isolation(self):
        value = {"selection": selection(params={"text": "Café😀", "v": 1e-7})}
        first = self.differential(
            "query-put",
            {"name": "movies", "value": value},
            lambda app: Queries(app.store).put("movies", value),
            "catabolic.saved_queries",
        )
        self.assertEqual(
            first, self.native("query-put", {"name": "movies", "value": value})
        )
        other = {"selection": selection("SELECT file_id FROM catalog_files")}
        second = self.differential(
            "query-put",
            {"name": "movies", "value": other},
            lambda app: Queries(app.store).put("movies", other),
            "catabolic.saved_queries",
        )
        self.assertEqual(second["revision"], 2)
        composed = {"combine": "union", "queries": [first["id"], first["id"]]}
        third = self.differential(
            "query-put",
            {"name": "composed", "value": composed},
            lambda app: Queries(app.store).put("composed", composed),
            "catabolic.saved_queries",
        )
        with Store(self.path) as store:
            self.assertEqual(
                store.rows("SELECT * FROM query_dependencies"),
                [{"query_id": third["id"], "dependency_id": first["id"]}],
            )
            self.assertEqual(
                self.native("query-list", {"limit": 1}), Queries(store).list(limit=1)
            )
        self.assertEqual(
            self.native("query-get", {"id": first["id"]}, ok=False, profile="laptop"),
            "unknown query revision in this profile",
        )

    def test_query_modes_budgets_unknown_fields_and_validation_errors(self):
        valid = [
            {"selection": selection()},
            {
                "version": 1,
                "mode": "rows",
                "selection": selection("SELECT :x", params={"x": True}),
            },
            {
                "mode": "document",
                "selection": {
                    "language": "graphql",
                    "query": "{ items { nodes { id } } }",
                    "variables": {"x": None},
                },
            },
            {"selection": selection(max_ids=3, timeout_ms=4)},
            {"entity": "occurrence_id", "selection": selection()},
        ]
        with Store(self.path) as store:
            for value in valid:
                self.assertEqual(
                    self.native("query-validate", {"value": value}),
                    Queries(store).validate(value),
                )
        invalid = [
            None,
            [],
            {},
            {"unknown": 1},
            {"version": True, "selection": selection()},
            {"selection": selection(), "mode": "invalid"},
            {"selection": selection(), "entity": "invalid"},
            {"selection": selection(), "max_ids": True},
            {"selection": selection(), "timeout_ms": 0},
            {"selection": selection(profile="laptop")},
            {"selection": {"query_id": "missing"}},
            {"selection": selection(params={"profile": "evil"})},
            {"selection": selection(params={"not-valid": 1})},
            {"mode": "rows", "selection": selection(params={"x": 2**63})},
            {
                "mode": "document",
                "selection": {"language": "graphql", "query": "x", "variables": []},
            },
            {"mode": "rows", "selection": selection(query=" ")},
            {"combine": "union", "queries": []},
            {"combine": "invalid", "queries": ["a", "b"]},
            {"combine": "union", "queries": ["a", "b"]},
            {"selection": selection(), "queries": []},
        ]
        for value in invalid:
            with self.subTest(value=value):
                self.assert_error(
                    "query-put",
                    {"name": "invalid", "value": value},
                    lambda app, value=value: Queries(app.store).put("invalid", value),
                )

    def test_query_large_definition_omission_and_read_immutability(self):
        value = {"mode": "rows", "selection": selection("SELECT '" + "é" * 9000 + "'")}
        row = self.differential(
            "query-put",
            {"name": "large", "value": value},
            lambda app: Queries(app.store).put("large", value),
            "catabolic.saved_queries",
        )
        before = self.path.read_bytes()
        with Store(self.path) as store:
            self.assertEqual(self.native("query-list"), Queries(store).list())
            self.assertEqual(
                self.native("query-get", {"id": row["id"]}),
                Queries(store).get(row["id"]),
            )
        self.assertEqual(before, self.path.read_bytes())

    def test_rule_revision_digest_enable_disable_and_derived_identity(self):
        base = {
            "name": "verify",
            "recipe_id": self.operation,
            "selection": selection(),
            "required": True,
        }

        def put(app, arg):
            return Rules(app).put(
                arg["name"],
                arg["recipe_id"],
                arg.get("location"),
                arg["selection"],
                estimates=arg.get("estimates"),
                required=arg.get("required", False),
                allow_derived=arg.get("allow_derived", False),
            )

        first = self.differential(
            "rule-put", base, lambda app: put(app, base), "catabolic.rules"
        )
        self.differential(
            "rule-enable",
            {"id": first["id"], "enabled": True},
            lambda app: Rules(app).enable(first["id"], True),
        )
        derived = {**base, "allow_derived": True, "estimates": {"video_kbps": 3000}}
        second = self.differential(
            "rule-put", derived, lambda app: put(app, derived), "catabolic.rules"
        )
        self.assertNotEqual(first["digest"], second["digest"])
        self.assertEqual(second["revision"], 2)
        self.assertFalse(self.native("rule-get", {"id": first["id"]})["enabled"])
        self.differential(
            "rule-enable",
            {"id": second["id"], "enabled": True},
            lambda app: Rules(app).enable(second["id"], True),
        )
        # Re-putting an existing revision does not unexpectedly enable it.
        repeat = self.differential(
            "rule-put", base, lambda app: put(app, base), "catabolic.rules"
        )
        self.assertFalse(repeat["enabled"])
        with Store(self.path) as store:
            self.assertEqual(
                self.native("rule-list", {"enabled": True, "limit": 1}),
                Rules(Application(store)).list(enabled=True, limit=1),
            )
        for changed in (
            {"selection": []},
            {"location": "wrong"},
            {"allow_derived": 1},
            {"estimates": {"other": 1}},
            {"estimates": {"video_kbps": True}},
            {"selection": selection(profile="laptop")},
        ):
            value = {**base, **changed}
            self.assert_error(
                "rule-put", value, lambda app, value=value: put(app, value)
            )

    def test_layout_copy_rendition_projection_atomicity(self):
        query = self.native(
            "query-put", {"name": "all", "value": {"selection": selection()}}
        )
        definition = copy.deepcopy(PRESETS["flat"])
        self.differential(
            "layout-put",
            {"name": "flat", "value": definition},
            lambda app: Layouts(app).put("flat", definition),
        )
        with Store(self.path) as store:
            self.assertEqual(
                self.native("layout-list"), Layouts(Application(store)).list()
            )
            self.assertEqual(
                self.native("projection-get", {"catalog": "global"}),
                Projections(Application(store)).get("global"),
            )
        copies = {
            "prefer": [{"field": "height", "order": "desc"}],
            "tie_break": "file_id",
        }
        renditions = {"purpose": "transcode", "mode": "preferred"}
        args = {
            "catalog": "global",
            "query_id": query["id"],
            "layout": "flat",
            "copies": copies,
            "renditions": renditions,
        }
        self.differential(
            "projection-put",
            args,
            lambda app: Projections(app).put(
                "global", query["id"], "flat", copies=copies, renditions=renditions
            ),
        )
        invalid = {
            **args,
            "copies": {"available_only": True},
            "renditions": {"purpose": "unknown"},
        }
        self.assert_error(
            "projection-put",
            invalid,
            lambda app: Projections(app).put(
                "global",
                query["id"],
                "flat",
                copies=invalid["copies"],
                renditions=invalid["renditions"],
            ),
        )
        for value in (
            {"unknown": True},
            {"tie_break": True},
            {"available_only": 1},
            {"require": {"nope": 1}},
            {"prefer": [{"field": "height"}]},
            {"prefer": [{"field": "height", "order": "bad"}]},
        ):
            self.assert_error(
                "copy-put",
                {"catalog": "global", "value": value},
                lambda app, value=value: CopySelection(app).put("global", value),
            )
        for value in (
            {},
            {"version": True},
            {"unknown": 1},
            {"purpose": "bad"},
            {"definition_id": "missing"},
            {"rule_id": "missing"},
            {"rule_id": "\0"},
        ):
            self.assert_error(
                "rendition-put",
                {"catalog": "global", "value": value},
                lambda app, value=value: Publication(app).put("global", value),
            )

    def test_pending_journal_refuses_desired_state_and_writer_contention(self):
        with sqlite3.connect(self.path) as db:
            db.execute(
                "INSERT INTO journal VALUES ('pending','default','global','file','create',NULL,NULL)"
            )

        self.assert_error(
            "copy-put",
            {"catalog": "global", "value": {}},
            lambda app: CopySelection(app).put("global", {}),
        )
        self.assert_error(
            "rule-put",
            {"name": "rule", "recipe_id": self.operation, "selection": selection()},
            lambda app: Rules(app).put("rule", self.operation, None, selection()),
        )
        with Store(self.path, writable=True):
            self.assertIn(
                "another Catabolic writer",
                self.native(
                    "query-put",
                    {"name": "all", "value": {"selection": selection()}},
                    ok=False,
                ),
            )

    def test_rows_parameters_reject_arbitrary_precision_integer_overflow(self):
        for integer in (2**63, -(2**63) - 1, 2**64, -(2**64), 2**80, -(2**80)):
            with self.subTest(integer=integer):
                value = {
                    "mode": "rows",
                    "selection": selection(
                        "SELECT item_id FROM catalog_items WHERE item_id=:n",
                        params={"n": integer},
                    ),
                }
                self.assert_error(
                    "query-put",
                    {"name": "overflow", "value": value},
                    lambda app, value=value: Queries(app.store).put("overflow", value),
                )
