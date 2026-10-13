# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Bounded source/qualification contracts for four immutable public imports."""

import ast
import copy
import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def load(name):
    spec = importlib.util.spec_from_file_location(
        name, ROOT / "scripts" / (name + ".py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


GENERATOR = load("generate_public_import_bindings")


class PublicImportSourceContracts(unittest.TestCase):
    def test_exact_inventory_and_source_hashes(self):
        inventory = json.loads((ROOT / "tests/parity/python-api.json").read_text())
        rows = GENERATOR.select_rows(inventory)
        contract = json.loads((ROOT / GENERATOR.CONTRACT).read_text())
        self.assertEqual([r["id"] for r in rows], [r["id"] for r in contract["rows"]])
        self.assertEqual(
            contract["inventory_sha256"],
            GENERATOR.sha((ROOT / "tests/parity/python-api.json").read_bytes()),
        )
        sources = json.loads((ROOT / "tests/parity/inventory.json").read_text())[
            "resource_sha256"
        ]
        for row in contract["rows"]:
            self.assertEqual(row["source_sha256"], sources[row["source"]])

    def test_inventory_rejects_missing_duplicate_changed_classification_and_source(
        self,
    ):
        original = json.loads((ROOT / "tests/parity/python-api.json").read_text())
        for change in ("missing", "duplicate", "classification", "source", "commit"):
            inventory = copy.deepcopy(original)
            row = next(
                row for row in inventory["rows"] if row["id"] in GENERATOR.BINDINGS
            )
            if change == "missing":
                inventory["rows"].remove(row)
            elif change == "duplicate":
                inventory["rows"].append(row)
            elif change == "commit":
                inventory["reference_commit"] = "wrong"
            else:
                row[change] = "wrong"
            with self.subTest(change=change), self.assertRaises(AssertionError):
                GENERATOR.select_rows(inventory)

    def test_new_python_modules_are_only_native_reexports(self):
        package = ROOT / "crates/catabolic-python/python/catabolic_native"
        for name, level, symbol, alias in (
            ("component_sql.py", 1, "OCCURRENCES_SQL", "OCCURRENCES_SQL"),
            ("documentation.py", 1, "TOPICS", "TOPICS"),
            ("http/contract.py", 2, "HTTP_CONTRACT_VERSION", "VERSION"),
        ):
            body = ast.parse((package / name).read_text()).body
            self.assertEqual(len(body), 2)
            self.assertIsInstance(body[0], ast.Expr)
            self.assertIsInstance(body[0].value, ast.Constant)
            self.assertIsInstance(body[1], ast.ImportFrom)
            self.assertEqual((body[1].module, body[1].level), ("_native", level))
            self.assertEqual(
                [(n.name, n.asname) for n in body[1].names], [(symbol, alias)]
            )
        http = ast.parse((package / "http/__init__.py").read_text()).body
        self.assertEqual(len(http), 1)
        self.assertIsInstance(http[0].value, ast.Constant)
        graphql = ast.parse((package / "graphql_query.py").read_text()).body
        imports = [
            node
            for node in graphql
            if isinstance(node, ast.ImportFrom) and node.module == "_native"
        ]
        self.assertEqual(len(imports), 1)
        self.assertEqual(
            [(n.name, n.asname) for n in imports[0].names], [("SDL", "SDL")]
        )

    def test_exact_reused_native_resource_bytes(self):
        contract = json.loads((ROOT / GENERATOR.CONTRACT).read_text())
        rows = {row["id"].rsplit(".", 1)[-1]: row for row in contract["rows"]}
        views = json.loads(
            (ROOT / rows["OCCURRENCES_SQL"]["native_owner"]["resource"]).read_text()
        )["views"]
        sql = next(
            row["sql"]
            for row in views
            if row["name"] == "catalog_component_occurrences"
        )
        self.assertEqual(GENERATOR.metadata(sql), rows["OCCURRENCES_SQL"]["value"])
        sdl = (ROOT / rows["SDL"]["native_owner"]["resource"]).read_bytes()
        self.assertEqual(
            GENERATOR.sha(sdl), rows["SDL"]["native_owner"]["resource_sha256"]
        )
        self.assertEqual(len(sdl), rows["SDL"]["native_owner"]["resource_utf8_bytes"])
        public = sdl[: rows["SDL"]["value"]["utf8_bytes"]].decode()
        self.assertEqual(GENERATOR.metadata(public), rows["SDL"]["value"])
        self.assertNotEqual(len(public.encode()), len(sdl))

    def test_native_owner_registration_and_generated_literal_boundary(self):
        registration = (
            ROOT / "crates/catabolic-python/src/public_imports.rs"
        ).read_text()
        self.assertIn(
            'catabolic_store::query::view_sql("catalog_component_occurrences")',
            registration,
        )
        self.assertIn(
            "PyTuple::new(module.py(), TOPICS.iter().copied())?", registration
        )
        self.assertIn(
            "catabolic_store::graphql::schema_text()[..GRAPHQL_PUBLIC_SDL_BYTES]",
            registration,
        )
        self.assertNotIn("Store::", registration)
        self.assertNotIn("catabolic_runtime", registration)
        generated = (ROOT / GENERATOR.GENERATED).read_text()
        self.assertIn(
            "Generated by scripts/generate_public_import_bindings.py; do not edit.",
            generated,
        )
        self.assertEqual(generated.count("pub const "), 3)
        self.assertNotIn("fn ", generated)
        self.assertEqual(
            (ROOT / "crates/catabolic-python/src/lib.rs")
            .read_text()
            .count("public_imports::register(m)?;"),
            1,
        )

    def test_qualification_exact_cases_and_early_stale_receipt_refusal(self):
        gate = load("native_public_import_tests")
        self.assertEqual(
            set(
                unittest.defaultTestLoader.getTestCaseNames(gate.InstalledPublicImports)
            ),
            set(gate.EXPECTED_TESTS),
        )
        self.assertEqual(len(gate.EXPECTED_TESTS), 8)
        required = [
            "--inventory",
            "unused",
            "--reference-root",
            "unused",
            "--reference-python",
            "unused",
        ]
        cases = (
            ("missing_required", [], ["--dev"], 2, "following arguments are required"),
            (
                "mutually_exclusive",
                [],
                ["--dev", "--wheel", "unused", *required],
                2,
                "not allowed with argument",
            ),
            (
                "malformed_option",
                [],
                ["--dev", *required, "--unknown-option"],
                2,
                "unrecognized arguments",
            ),
            (
                "optimized",
                ["-O"],
                ["--dev", *required],
                1,
                "requires assertions and isolated Python -I",
            ),
        )
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / "report.json"
            attempt = report.with_suffix(".attempt.json")
            for name, flags, arguments, status, diagnostic in cases:
                with self.subTest(case=name):
                    report.write_text('{"passed_without_skips":true}')
                    attempt.write_text('{"passed_without_skips":true}')
                    run = subprocess.run(
                        [
                            sys.executable,
                            "-I",
                            *flags,
                            str(ROOT / "scripts/native_public_import_tests.py"),
                            *arguments,
                            "--report",
                            str(report),
                        ],
                        text=True,
                        capture_output=True,
                        timeout=15,
                    )
                    self.assertEqual(run.returncode, status, run.stderr)
                    self.assertIn(diagnostic, run.stderr)
                    self.assertEqual(run.stdout, "")
                    self.assertFalse(report.exists())
                    self.assertFalse(attempt.exists())


EXPECTED_TESTS = (
    "test_exact_inventory_and_source_hashes",
    "test_inventory_rejects_missing_duplicate_changed_classification_and_source",
    "test_new_python_modules_are_only_native_reexports",
    "test_exact_reused_native_resource_bytes",
    "test_native_owner_registration_and_generated_literal_boundary",
    "test_qualification_exact_cases_and_early_stale_receipt_refusal",
)

if __name__ == "__main__":
    if not __debug__:
        raise RuntimeError("source contracts require assertions")
    load("native_qualification_results").run_required_suite(
        unittest.defaultTestLoader.loadTestsFromTestCase(PublicImportSourceContracts),
        {PublicImportSourceContracts: EXPECTED_TESTS},
        expected_count=6,
    )
