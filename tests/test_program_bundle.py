# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import contextlib
import copy
import io
import json
import unittest

from catabolic.app import Application
from catabolic.artifacts import Artifacts
from catabolic.cli import main
from catabolic.domain import CatabolicError
from catabolic.layouts import PRESETS
from catabolic.operations import Operations
from catabolic.outputs import Outputs
from catabolic.processors import Processors
from catabolic.program_bundle import Programs
from catabolic.projections import Projections
from catabolic.rules import Rules
from catabolic.saved_queries import Queries
from catabolic.store import Store
from tests import test_query_layouts as fixtures
from tests.test_query_layouts import sql_selection


class ProgramBundleTest(unittest.TestCase):
    setUp = fixtures.QueryLayoutTest.setUp

    def make_program(self, kind="analysis"):
        query = Queries(self.store).put(
            "tracks",
            {
                "selection": sql_selection(
                    "SELECT item_id FROM catalog_items WHERE kind='track'"
                )
            },
        )
        composed = Queries(self.store).put(
            "all-tracks", {"combine": "union", "queries": [query["id"], query["id"]]}
        )
        operation = {"kind": kind, "operation": "hash"}
        location = None
        if kind != "analysis":
            output = Outputs(self.app).define(
                "output", {"purpose": "thumbnail", "role": "thumbnail"}
            )
            operation["output_definition_id"] = output["id"]
            if kind == "render":
                operation["operation"] = "thumbnail"
                (self.root / "generated").mkdir()
                Artifacts(self.app).bind("generated", self.root / "generated")
                location = "generated"
            else:
                Processors(self.app).put(
                    "worker",
                    {
                        "endpoint": "http://localhost:9999",
                        "credential_env": "TEST_TOKEN",
                    },
                )
                operation["operation"] = "worker"
        operation = Operations(self.app).put("produce", operation)
        rule = Rules(self.app).put(
            "rule",
            operation["id"],
            location,
            {"query_id": composed["id"]},
            required=True,
        )
        Rules(self.app).enable(rule["id"], True)
        self.layouts.put("flat", copy.deepcopy(PRESETS["flat"]))
        policy = {"rule_id": rule["id"]} if kind != "analysis" else None
        Projections(self.app).put("query", composed["id"], "flat", renditions=policy)
        return Programs(self.app).export(rules=[rule["id"]], projections=["query"])

    def destination(self):
        path = self.root / "destination.sqlite3"
        Store.initialize(path)
        store = Store(path, writable=True)
        self.addCleanup(store.close)
        Application(store).add_profile("target")
        app = Application(store, "target")
        (self.root / "target").mkdir()
        app.bind("output", "target", str(self.root / "target"))
        return app, Programs(app)

    @staticmethod
    def state(app):
        return list(app.store.db.iterdump())

    def test_roundtrip_dependency_remapping_dry_run_and_repeat(self):
        bundle = self.make_program()
        self.assertEqual(len(bundle["queries"]), 2)
        self.assertNotIn(str(self.root), json.dumps(bundle))
        app, programs = self.destination()
        bindings = {"catalogs": {"query": "target"}}
        before = self.state(app)
        preview = programs.import_bundle(bundle, bindings=bindings, dry_run=True)
        self.assertTrue(preview["complete"] and preview["preview_ids_ephemeral"])
        self.assertEqual(before, self.state(app))
        result = programs.import_bundle(bundle, bindings=bindings)
        refs = result["references"]
        rule = Rules(app).get(next(iter(refs["rules"].values())))
        self.assertFalse(rule["enabled"])
        self.assertIn(rule["query_id"], refs["queries"].values())
        self.assertEqual(rule["profile"], "target")
        query = Queries(app.store, "target").get(rule["query_id"])
        self.assertTrue(
            all(
                ref in refs["queries"].values()
                for ref in query["definition"]["queries"]
            )
        )
        self.assertEqual(Queries(app.store, "target").run(rule["query_id"])["ids"], [])
        Rules(app).enable(rule["id"], True)
        before = self.state(app)
        repeated = programs.import_bundle(bundle, bindings=bindings)
        self.assertEqual(result, repeated)
        self.assertEqual(before, self.state(app))
        for table in ("processing_jobs", "processor_jobs", "mappings", "journal"):
            self.assertEqual(app.store.rows("SELECT * FROM " + table), [])
        self.assertEqual(list((self.root / "target").iterdir()), [])

    def test_missing_binding_and_late_invalid_policy_roll_back_everything(self):
        bundle = self.make_program()
        app, programs = self.destination()
        before = self.state(app)
        with self.assertRaisesRegex(CatabolicError, "explicit catalogs"):
            programs.import_bundle(bundle)
        self.assertEqual(before, self.state(app))
        bundle["projections"]["query"]["copies"] = {"unknown": True}
        with self.assertRaises(CatabolicError):
            programs.import_bundle(bundle, bindings={"catalogs": {"query": "target"}})
        self.assertEqual(before, self.state(app))

    def test_render_output_policy_and_existing_generated_location(self):
        bundle = self.make_program("render")
        app, programs = self.destination()
        (self.root / "target-generated").mkdir()
        Artifacts(app).bind("derived", self.root / "target-generated")
        bindings = {
            "catalogs": {"query": "target"},
            "locations": {"generated": "derived"},
        }
        result = programs.import_bundle(bundle, bindings=bindings)["references"]
        rule = Rules(app).get(next(iter(result["rules"].values())))
        self.assertEqual(rule["location"], "derived")
        self.assertEqual(
            Projections(app).get("target")["rendition_policy"]["rule_id"], rule["id"]
        )
        operation = Operations(app).get(rule["recipe_id"])
        self.assertIn(operation["output_definition_id"], result["outputs"].values())
        before = self.state(app)
        programs.import_bundle(bundle, bindings=bindings)
        self.assertEqual(before, self.state(app))

    def test_external_requires_local_processor_and_omits_endpoint_credentials(self):
        bundle = self.make_program("external")
        self.assertNotIn("localhost", json.dumps(bundle))
        self.assertNotIn("TEST_TOKEN", json.dumps(bundle))
        app, programs = self.destination()
        bindings = {"catalogs": {"query": "target"}}
        before = self.state(app)
        with self.assertRaisesRegex(CatabolicError, "explicit processors"):
            programs.import_bundle(bundle, bindings=bindings)
        self.assertEqual(before, self.state(app))
        Processors(app).put("local-worker", {"endpoint": "http://localhost:8888"})
        bindings["processors"] = {"worker": "local-worker"}
        result = programs.import_bundle(bundle, bindings=bindings)["references"]
        operation = Operations(app).get(next(iter(result["operations"].values())))
        self.assertEqual(operation["definition"]["operation"], "local-worker")
        self.assertEqual(app.store.rows("SELECT * FROM processor_jobs"), [])

    def test_conflicting_rule_preserves_enabled_state_and_all_definitions(self):
        bundle = self.make_program()
        app, programs = self.destination()
        bindings = {"catalogs": {"query": "target"}}
        result = programs.import_bundle(bundle, bindings=bindings)
        rule_id = next(iter(result["references"]["rules"].values()))
        Rules(app).enable(rule_id, True)
        before = self.state(app)
        next(iter(bundle["operations"].values()))["definition"]["options"][
            "timeout"
        ] = 30
        with self.assertRaisesRegex(CatabolicError, "existing rule"):
            programs.import_bundle(bundle, bindings=bindings)
        self.assertEqual(before, self.state(app))

    def test_invalid_references_cycles_limits_and_shapes_are_atomic(self):
        original = self.make_program()
        app, programs = self.destination()
        before = self.state(app)
        cases = []
        bundle = copy.deepcopy(original)
        query = next(iter(bundle["queries"]))
        bundle["queries"][query]["definition"] = {
            "combine": "union",
            "queries": [query, query],
        }
        cases.append(bundle)
        bundle = copy.deepcopy(original)
        bundle["queries"] = {}
        cases.append(bundle)
        bundle = copy.deepcopy(original)
        bundle["rules"][next(iter(bundle["rules"]))]["required"] = "true"
        cases.append(bundle)
        bundle = copy.deepcopy(original)
        bundle["queries"] = {
            str(i): {"name": "q", "definition": {}} for i in range(257)
        }
        cases.append(bundle)
        bundle = copy.deepcopy(original)
        bundle["version"] = True
        cases.append(bundle)
        for bundle in cases:
            with self.subTest(bundle_version=bundle["version"]):
                with self.assertRaises(CatabolicError):
                    programs.import_bundle(
                        bundle, bindings={"catalogs": {"query": "target"}}
                    )
                self.assertEqual(before, self.state(app))

    def test_cli_raw_export_and_machine_import(self):
        bundle = self.make_program()
        source_id = next(iter(bundle["rules"]))
        self.store.close()
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            self.assertEqual(
                main(
                    [
                        "--db",
                        str(self.path),
                        "--json",
                        "program",
                        "export",
                        "--rule",
                        source_id,
                    ]
                ),
                0,
            )
        exported = json.loads(output.getvalue())
        self.assertEqual(exported["format"], "catabolic.program")
        path = self.root / "program.json"
        path.write_text(json.dumps(exported))
        destination = self.root / "cli.sqlite3"
        Store.initialize(destination)
        for dry_run in (True, False):
            output = io.StringIO()
            args = [
                "--db",
                str(destination),
                "--machine",
                "program",
                "import",
                "--file",
                str(path),
            ]
            with contextlib.redirect_stdout(output):
                self.assertEqual(main(args + (["--dry-run"] if dry_run else [])), 0)
            payload = json.loads(output.getvalue())
            self.assertEqual(payload["operation"], "import")
            self.assertEqual(payload["data"]["applied"], not dry_run)

    def test_legacy_render_recipe_without_output_definition(self):
        bundle = self.make_program("render")
        operation_id = next(iter(bundle["operations"]))
        # Reproduce the nullable output-definition state preserved by schema 9.
        with self.store.transaction() as db:
            db.execute(
                "UPDATE processing_recipes SET output_definition_id=NULL WHERE id=?",
                (operation_id,),
            )
        bundle = Programs(self.app).export(rules=list(bundle["rules"]))
        self.assertNotIn(
            "output_definition_id", bundle["operations"][operation_id]["definition"]
        )
        app, programs = self.destination()
        (self.root / "target-generated").mkdir()
        Artifacts(app).bind("derived", self.root / "target-generated")
        result = programs.import_bundle(
            bundle, bindings={"locations": {"generated": "derived"}}
        )
        operation = Operations(app).get(
            result["references"]["operations"][operation_id]
        )
        self.assertIsNotNone(operation["output_definition_id"])

    def test_projection_and_layout_conflicts_roll_back(self):
        bundle = self.make_program()
        app, programs = self.destination()
        bindings = {"catalogs": {"query": "target"}}
        programs.import_bundle(bundle, bindings=bindings)
        before = self.state(app)
        conflicting = copy.deepcopy(bundle)
        conflicting["projections"]["query"]["copies"] = {"tie_break": "file_id"}
        with self.assertRaisesRegex(CatabolicError, "existing projection"):
            programs.import_bundle(conflicting, bindings=bindings)
        self.assertEqual(before, self.state(app))
        next(iter(bundle["layouts"].values()))["definition"]["rules"][0]["path"] = (
            "other/{file.id}"
        )
        with self.assertRaisesRegex(CatabolicError, "existing layout"):
            programs.import_bundle(bundle, bindings=bindings)
        self.assertEqual(before, self.state(app))
