# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native program bundle closure, remapping and atomic import qualification."""

import copy
import json
import select
import shutil
import subprocess
import unittest
from contextlib import ExitStack
from datetime import datetime, timezone
from unittest.mock import patch

from catabolic.app import Application
from catabolic.artifacts import Artifacts
from catabolic.domain import CatabolicError
from catabolic.fallback_policies import Policies
from catabolic.operations import Operations
from catabolic.outputs import Outputs
from catabolic.program_bundle import Programs
from catabolic.rules import Rules
from catabolic.saved_queries import Queries
from catabolic.store import Store
from catabolic.watchers import Watchers
from tests import test_program_bundle as fixtures
from tests.test_rust_migration import BINARY


@unittest.skipUnless(BINARY.is_file(), "build catabolic-cli before qualification")
class NativeProgramsTest(unittest.TestCase):
    def setUp(self):
        fixtures.ProgramBundleTest.setUp(self)
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

    def native(self, action, args, *, ok=True, path=None, profile="default"):
        self.worker.stdin.write(
            json.dumps(
                [
                    "--db",
                    str(path or self.path),
                    "--profile",
                    profile,
                    "programs",
                    action,
                    json.dumps(args),
                ]
            )
            + "\n"
        )
        self.worker.stdin.flush()
        self.assertTrue(
            select.select([self.worker.stdout], [], [], 120)[0], "native timeout"
        )
        result = json.loads(self.worker.stdout.readline())
        self.assertEqual(result["status"], 0 if ok else 2, result)
        return result["result"] if ok else result["error"].removeprefix("catabolic: ")

    def bundle(self, kind="analysis"):
        bundle = fixtures.ProgramBundleTest.make_program(self, kind)
        self.store.close()
        return bundle

    def destination(self, render=False):
        path = self.root / "destination.sqlite3"
        Store.initialize(path)
        with Store(path, writable=True) as store:
            app = Application(store)
            with store.transaction() as db:
                db.execute("INSERT INTO catalogs VALUES ('target')")
            if render:
                (self.root / "target-generated").mkdir()
                Artifacts(app).bind("derived", self.root / "target-generated")
        return path

    def normalize(self, row):
        row = row.copy()
        if "created_at" in row:
            raw = row.pop("created_at")
            stamp = (
                datetime.fromtimestamp(raw, timezone.utc).replace(tzinfo=None)
                if isinstance(raw, (int, float))
                else datetime.strptime(raw, "%Y-%m-%d %H:%M:%S")
            )
            self.assertLess(
                abs(
                    (
                        datetime.now(timezone.utc).replace(tzinfo=None) - stamp
                    ).total_seconds()
                ),
                120,
            )
        return row

    def differential_import(self, path, args):
        reference = self.root / "reference.sqlite3"
        shutil.copyfile(path, reference)
        actual = self.native("import", args, path=path)
        # Pin only newly generated UUIDs by their declared local definition name.
        # Each owner still independently validates, hashes and mutates its DB.
        prefix = args.get("prefix", "imported")
        with ExitStack() as stack:
            for cls, kind, method, modules in (
                (Queries, "queries", "put", ["catabolic.saved_queries"]),
                (Rules, "rules", "put", ["catabolic.rules"]),
                (
                    Operations,
                    "operations",
                    "put",
                    ["catabolic.operations", "catabolic.artifacts"],
                ),
                (Outputs, "outputs", "define", ["catabolic.outputs"]),
                (Policies, "fallback_policies", "put", ["catabolic.fallback_policies"]),
                (Watchers, "watchers", "put", ["catabolic.watchers"]),
            ):
                if kind not in args["value"]:
                    continue
                original = getattr(cls, method)
                ids = {
                    prefix + "." + row["name"]: actual["references"][kind][foreign]
                    for foreign, row in args["value"][kind].items()
                }

                def pinned(
                    owner,
                    name,
                    *values,
                    _original=original,
                    _ids=ids,
                    _modules=modules,
                    **kwargs,
                ):
                    with ExitStack() as nested:
                        for module in _modules:
                            nested.enter_context(
                                patch(module + ".uuid4", return_value=_ids[name])
                            )
                        return _original(owner, name, *values, **kwargs)

                stack.enter_context(patch.object(cls, method, pinned))
            with Store(reference, writable=True) as store:
                expected = Programs(Application(store)).import_bundle(
                    args["value"],
                    bindings=args.get("bindings"),
                    prefix=prefix,
                    dry_run=args.get("dry_run", False),
                )
        self.assertEqual(actual, expected)
        with Store(path) as native, Store(reference) as oracle:
            for table in (
                "saved_queries",
                "query_dependencies",
                "processing_recipes",
                "output_definitions",
                "processing_rules",
                "meta",
                "copy_policies",
                "rendition_policies",
                "projection_bindings",
                "fallback_policies",
                "fallback_dependencies",
                "watcher_definitions",
                "watchers",
            ):
                self.assertEqual(
                    [
                        self.normalize(r)
                        for r in native.rows(f"SELECT * FROM {table} ORDER BY 1")
                    ],
                    [
                        self.normalize(r)
                        for r in oracle.rows(f"SELECT * FROM {table} ORDER BY 1")
                    ],
                    table,
                )
            for table in ("processing_jobs", "processor_jobs", "mappings", "journal"):
                self.assertEqual(native.rows(f"SELECT * FROM {table}"), [])
            self.assertEqual(native.rows("PRAGMA foreign_key_check"), [])
        return actual

    def error(self, path, bundle, **kwargs):
        before = path.read_bytes()
        with Store(path, writable=True) as store:
            with self.assertRaises(CatabolicError) as expected:
                Programs(Application(store)).import_bundle(bundle, **kwargs)
        self.assertEqual(
            self.native("import", {"value": bundle, **kwargs}, path=path, ok=False),
            str(expected.exception),
        )
        self.assertEqual(path.read_bytes(), before)

    def test_dependency_export_is_exact_and_contains_no_runtime_credentials(self):
        bundle = self.bundle("external")
        roots = {"rules": list(bundle["rules"]), "projections": ["query"]}
        before = self.path.read_bytes()
        self.assertEqual(self.native("export", roots), bundle)
        self.assertNotIn("localhost", json.dumps(bundle))
        self.assertNotIn("TEST_TOKEN", json.dumps(bundle))
        self.assertEqual(self.path.read_bytes(), before)

    def test_analysis_import_dry_run_reference_remapping_and_idempotence(self):
        bundle = self.bundle()
        path = self.destination()
        args = {"value": bundle, "bindings": {"catalogs": {"query": "target"}}}
        before = path.read_bytes()
        preview = self.differential_import(path, {**args, "dry_run": True})
        self.assertTrue(preview["preview_ids_ephemeral"])
        self.assertEqual(path.read_bytes(), before)
        result = self.differential_import(path, args)
        rule = next(iter(result["references"]["rules"].values()))
        with Store(path, writable=True) as store:
            self.assertFalse(Rules(Application(store)).get(rule)["enabled"])
            Rules(Application(store)).enable(rule, True)
        before = path.read_bytes()
        self.assertEqual(result, self.differential_import(path, args))
        self.assertEqual(path.read_bytes(), before)

    def test_render_recipe_and_output_policy_import(self):
        bundle = self.bundle("render")
        self.assertEqual(
            self.native(
                "export", {"rules": list(bundle["rules"]), "projections": ["query"]}
            ),
            bundle,
        )
        path = self.destination(render=True)
        args = {
            "value": bundle,
            "bindings": {
                "catalogs": {"query": "target"},
                "locations": {"generated": "derived"},
            },
        }
        self.differential_import(path, args)
        self.differential_import(path, args)

    def test_missing_binding_late_policy_and_enabled_rule_conflicts_rollback(self):
        bundle = self.bundle()
        path = self.destination()
        bindings = {"catalogs": {"query": "target"}}
        self.error(path, bundle)
        invalid = copy.deepcopy(bundle)
        invalid["projections"]["query"]["copies"] = {"unknown": True}
        self.error(path, invalid, bindings=bindings)
        result = self.differential_import(path, {"value": bundle, "bindings": bindings})
        rule = next(iter(result["references"]["rules"].values()))
        with Store(path, writable=True) as store:
            Rules(Application(store)).enable(rule, True)
        invalid = copy.deepcopy(bundle)
        next(iter(invalid["operations"].values()))["definition"]["options"][
            "timeout"
        ] = 30
        self.error(path, invalid, bindings=bindings)

    def test_cycles_missing_references_and_strict_bundle_shapes(self):
        bundle = self.bundle()
        path = self.destination()
        bindings = {"catalogs": {"query": "target"}}
        cases = []
        value = copy.deepcopy(bundle)
        query = next(iter(value["queries"]))
        value["queries"][query]["definition"] = {
            "combine": "union",
            "queries": [query, query],
        }
        cases.append(value)
        value = copy.deepcopy(bundle)
        value["queries"] = {}
        cases.append(value)
        value = copy.deepcopy(bundle)
        next(iter(value["rules"].values()))["required"] = "true"
        cases.append(value)
        value = copy.deepcopy(bundle)
        value["version"] = True
        cases.append(value)
        value = copy.deepcopy(bundle)
        value["queries"] = {str(n): {"name": "q", "definition": {}} for n in range(257)}
        cases.append(value)
        for value in cases:
            self.error(path, value, bindings=bindings)

    def test_v2_fallback_policy_dependency_roundtrip(self):
        bundle = self.bundle()
        query = next(iter(bundle["queries"]))
        with Store(self.path, writable=True) as store:
            policy = Policies(store).put(
                "ranked", {"fallbacks": [{"name": "first", "query_id": query}]}
            )
            bundle = Programs(Application(store)).export(
                fallback_policies=[policy["id"]]
            )
        self.assertEqual(bundle["version"], 2)
        self.assertEqual(
            self.native("export", {"fallback_policies": [policy["id"]]}), bundle
        )
        path = self.destination()
        self.differential_import(path, {"value": bundle})
        self.differential_import(path, {"value": bundle})

    def test_v3_query_watcher_definition_roundtrip_stays_disabled(self):
        bundle = self.bundle()
        query = next(iter(bundle["queries"]))
        with Store(self.path, writable=True) as store:
            app = Application(store)
            Watchers(app).put(
                "changes",
                {
                    "plan": {"kind": "query", "query_id": query},
                    "reaction": "event",
                    "schedule": {"kind": "interval", "seconds": 90},
                },
            )
            bundle = Programs(app).export(watchers=["changes"])
        self.assertEqual(bundle["version"], 3)
        self.assertEqual(self.native("export", {"watchers": ["changes"]}), bundle)
        path = self.destination()
        self.differential_import(path, {"value": bundle, "dry_run": True})
        self.differential_import(path, {"value": bundle})
        self.differential_import(path, {"value": bundle})
        with Store(path) as store:
            self.assertEqual(
                store.rows("SELECT enabled FROM watchers"), [{"enabled": 0}]
            )
            self.assertEqual(store.rows("SELECT * FROM watcher_runs"), [])
