# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import copy
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

from scripts.migration_baseline import UNIT_RUNNER, run_check
from scripts.migration_contracts import PROOFS, REFERENCE, build, sha, verify_source
from scripts.migration_scope import extend, initialize, validate


class ContractInventoryTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.source = Path(temporary.name)
        prefix = "src/catabolic/http/releases/1.7.0"
        values = {
            f"{prefix}/schema.graphql": "type Query { files(first: Int! = 100): [File!]! }\ntype File { id: ID! }\nenum Mode { ACTIVE ALL }\n",
            f"{prefix}/openapi.json": json.dumps(
                {
                    "components": {
                        "schemas": {
                            "File": {
                                "required": ["id"],
                                "properties": {"id": {"type": "string"}},
                            }
                        }
                    }
                }
            ),
            "src/catabolic/http/releases/1.0.0/openapi.json": '{"historical": true}',
            "src/catabolic/migration.py": "# synthetic source for inventory tests\n",
        }
        for name, content in values.items():
            path = self.source / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
        self.inventory = dict(
            http_version="1.7.0",
            resource_sha256={
                name: sha((self.source / name).read_bytes()) for name in values
            },
            test_methods=sorted({test for tests in PROOFS.values() for test in tests}),
            cli=[],
            http=[],
            migrations=[],
        )

    def test_field_contract_retains_arguments_defaults_and_nullability(self):
        result = build(self.source, self.inventory, "inventory-hash")
        rows = {row["id"]: row for row in result["rows"]}
        self.assertEqual(
            rows["graphql:field:Query.files"]["contract"],
            "files(first: Int! = 100): [File!]!",
        )
        self.assertIn("ACTIVE", rows["graphql:type:Mode"]["contract"])
        self.assertEqual(rows["http-schema:File"]["contract"]["required"], ["id"])
        historical = "src/catabolic/http/releases/1.0.0/openapi.json"
        self.assertEqual(
            rows[f"artifact:{historical}"]["contract"]["sha256"],
            self.inventory["resource_sha256"][historical],
        )
        self.assertTrue(all(row["status"] == "inventoried" for row in rows.values()))
        self.assertEqual(len([r for r in rows if r.startswith("migration-hook:")]), 3)

    def test_changed_or_missing_source_bytes_are_rejected(self):
        path = self.source / "src/catabolic/migration.py"
        path.write_text("changed hook")
        with self.assertRaisesRegex(ValueError, "reference byte drift"):
            verify_source(self.source, self.inventory)
        path.unlink()
        with self.assertRaisesRegex(ValueError, "reference byte drift"):
            verify_source(self.source, self.inventory)

    def test_unknown_proof_test_cannot_silently_enter_inventory(self):
        self.inventory["test_methods"] = []
        with self.assertRaisesRegex(ValueError, "unknown reference test"):
            build(self.source, self.inventory, "inventory-hash")

    def test_extension_preserves_reviews_and_requires_matching_reference(self):
        contracts = build(self.source, self.inventory, "inventory-hash")
        inventory = dict(self.inventory, cli=[{"id": "cli:scan"}])
        ledger = initialize(inventory, "inventory-hash", REFERENCE)
        ledger["rows"][0]["status"] = "blocked"
        ledger["rows"][0]["review_note"] = "Preserve this reviewed decision"
        before = copy.deepcopy(ledger)
        result = extend(ledger, contracts, "contracts-hash")
        self.assertEqual(ledger, before)
        self.assertEqual(result["rows"][0], before["rows"][0])
        validate(result, inventory, "inventory-hash", contracts, "contracts-hash")
        with self.assertRaisesRegex(ValueError, "missing or different"):
            validate(result, inventory, "inventory-hash")
        with self.assertRaisesRegex(ValueError, "already has"):
            extend(result, contracts, "contracts-hash")
        contracts["reference_commit"] = "different"
        with self.assertRaisesRegex(ValueError, "different oracle"):
            extend(ledger, contracts, "contracts-hash")

    def test_duplicate_supplemental_surface_is_rejected(self):
        contracts = build(self.source, self.inventory, "inventory-hash")
        contracts["rows"].append(contracts["rows"][0])
        ledger = initialize(self.inventory, "inventory-hash", REFERENCE)
        with self.assertRaisesRegex(ValueError, "duplicate supplemental"):
            extend(ledger, contracts, "contracts-hash")


class SelectedProofTest(unittest.TestCase):
    def test_selected_reference_test_does_not_run_unrequested_test(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "tests").mkdir()
            (root / "tests/__init__.py").write_text("")
            (root / "tests/test_example.py").write_text(
                "import unittest\nclass Example(unittest.TestCase):\n"
                "    def test_pass(self): pass\n"
                "    def test_unrequested(self): self.fail('must not run')\n"
            )
            result = run_check(
                "unit",
                [
                    sys.executable,
                    "-c",
                    UNIT_RUNNER,
                    str(root / "unit-result.json"),
                    "tests.test_example.Example.test_pass",
                ],
                root,
                root,
                os.environ.copy(),
                5,
            )
            self.assertEqual(result["status"], "passed")
            self.assertEqual(result["test_result"]["tests_run"], 1)
            self.assertTrue(result["test_result"]["complete"])


class PythonConsumerInventoryTest(unittest.TestCase):
    def test_imports_in_embedded_programs_are_retained(self):
        from scripts.migration_python_api import imports

        code = 'import catabolic\nprogram = "from catabolic.store import Store"\n'
        self.assertEqual(imports(code), {"catabolic", "catabolic.store.Store"})

    def test_package_import_does_not_promote_all_internal_symbols(self):
        from scripts.migration_python_api import build as python_inventory

        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)
            (source / "scripts").mkdir()
            (source / "tests").mkdir()
            (source / "scripts/consumer.py").write_text(
                'import catabolic\nprogram = "from catabolic.example import Facade"\n'
            )
            (source / "src/catabolic").mkdir(parents=True)
            (source / "src/catabolic/__init__.py").write_text(
                "__version__ = 'fixture'\n"
            )
            (source / "src/catabolic/example.py").write_text(
                "class Facade:\n    def __init__(self, value=1): pass\n    def call(self): pass\ndef internal(): pass\n"
            )
            result = python_inventory(
                source,
                dict(
                    resource_sha256={},
                    python_candidates=[
                        dict(
                            module="catabolic.example",
                            source="src/catabolic/example.py",
                            symbols=["Facade", "Facade.call", "internal"],
                        ),
                    ],
                ),
                "test-inventory",
            )
            rows = {row["id"]: row for row in result["rows"]}
            self.assertEqual(
                rows["python:catabolic.example.Facade.call"]["classification"],
                "preserve-repository-consumer",
            )
            self.assertEqual(
                rows["python:catabolic.example.Facade.__init__"]["signature"][
                    "arguments"
                ],
                "self, value=1",
            )
            self.assertEqual(
                rows["python:catabolic.example.internal"]["classification"],
                "no-external-import-observed",
            )


class M0CloseoutAuditTest(unittest.TestCase):
    def test_worker_timeout_cannot_be_hidden_by_zero_cli_exit(self):
        from scripts.migration_m0_audit import scale_status

        report = {"results": [{"status": "timeout"}]}
        self.assertEqual(scale_status("passed", report), "budget-limited")
        self.assertEqual(
            scale_status("budget-limited", {"results": []}), "budget-limited"
        )
        self.assertEqual(scale_status("passed", {"results": []}), "incomplete")

    def test_missing_gate_register_cannot_claim_completion(self):
        import shutil

        from scripts.migration_m0_audit import audit

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            original = Path(__file__).resolve().parent / "parity"
            for path in original.glob("*.json"):
                shutil.copyfile(path, root / path.name)
            gates = json.loads((root / "m0-gates.json").read_text())
            gates["gates"] = []
            (root / "m0-gates.json").write_text(json.dumps(gates))
            with self.assertRaisesRegex(ValueError, "Missing, duplicate or unexpected"):
                audit(root)

    def test_unexecuted_linux_lane_keeps_m0_open(self):
        import shutil

        from scripts.migration_m0_audit import audit

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for path in (Path(__file__).resolve().parent / "parity").glob("*.json"):
                shutil.copyfile(path, root / path.name)
            document = json.loads((root / "m0-gates.json").read_text())
            for gate in document["gates"]:
                if gate["id"] == "storage":
                    gate["status"] = "not-executed-linux"
            (root / "m0-gates.json").write_text(json.dumps(document))
            result = audit(root)
            self.assertFalse(result["complete"])
            self.assertTrue(
                any("not-executed-linux" in blocker for blocker in result["blockers"])
            )


if __name__ == "__main__":
    unittest.main()
