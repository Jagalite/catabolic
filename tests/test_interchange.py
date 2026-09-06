import copy
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from jsonschema import Draft202012Validator

from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.interchange import specification
from catabolic.interchange.validation import (
    decode_document,
    digest,
    document_value,
    encode_document,
    validate_document,
)
from catabolic.manifest import Manifest
from catabolic.store import Store, encode
from tests.test_media_model import populate_media

FIXTURE = Path(__file__).parent / "fixtures/interchange/manifest-v1.json"


def seal(value):
    value["content_sha256"] = digest(value["content"])
    return value


class InterchangeTest(unittest.TestCase):
    def sample(self):
        return json.loads(FIXTURE.read_text())

    def test_v2_remains_readable_and_preserves_unknown_extensions(self):
        value = self.sample()
        value["format_version"] = 2
        for name in ("tags", "tag_names", "tag_parents", "taggings"):
            value["content"][name] = []
            value["content"]["counts"][name] = 0
        value["vendor:extension"] = {"keep": [None, 1.0, 2**80]}
        seal(value)
        Draft202012Validator(specification.schema(2)).validate(value)
        self.assertEqual(
            json.loads(encode_document(decode_document(encode(value)))), value
        )

    def test_frozen_schema_reference_and_independent_schema_validation(self):
        self.assertTrue(specification.check_release()["generated_artifacts_match"])
        Draft202012Validator.check_schema(specification.schema())
        Draft202012Validator(specification.schema(1)).validate(self.sample())
        self.assertEqual(
            document_value(decode_document(FIXTURE.read_text())), self.sample()
        )

    def test_real_manifest_and_layout_provenance_validate_without_wire_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "catalog.sqlite3"
            Store.initialize(path)
            with Store(path, writable=True) as store:
                app = Application(store)
                populate_media(app, root)
                from catabolic.layouts import PRESETS, Layouts

                layouts = Layouts(app)
                layouts.put("flat", PRESETS["flat"])
                layouts.run("flat", apply=True)
                with store.transaction():
                    value = Manifest(app).build(
                        extra={"vendor:curation": {"confidence": 0.8}}
                    )
                Draft202012Validator(specification.schema()).validate(value)
                self.assertEqual(document_value(validate_document(value)), value)
                self.assertEqual(
                    json.loads(encode_document(decode_document(encode(value)))), value
                )

    def test_unknown_fields_at_every_level_and_numbers_survive_roundtrip(self):
        value = self.sample()

        def extend(node):
            if isinstance(node, dict):
                for child in list(node.values()):
                    extend(child)
                node["vendor:future"] = {
                    "nested": [None, True, 1.0, -0.0, 2**80, 1e-20, "Café"]
                }
            elif isinstance(node, list):
                for child in node:
                    extend(child)

        extend(value)
        seal(value)
        out = json.loads(encode_document(decode_document(encode(value))))
        self.assertEqual(out, value)
        self.assertEqual(encode(out), encode(value))
        self.assertEqual(out["content_sha256"], value["content_sha256"])
        Draft202012Validator(specification.schema(1)).validate(out)

    def test_rejects_duplicate_keys_unsupported_versions_and_nonfinite_numbers(self):
        raw = FIXTURE.read_text()
        for bad in [
            raw.replace(
                '"format_version": 1', '"format_version": 1, "format_version": 1'
            ),
            raw.replace('"format_version": 1', '"format_version": true'),
            raw.replace('"format_version": 1', '"format_version": 2'),
        ]:
            with self.subTest(bad=bad[:40]), self.assertRaises(CatabolicError):
                decode_document(bad)
        for number in ["NaN", "Infinity", "-Infinity", "1e9999"]:
            with self.subTest(number=number), self.assertRaises(CatabolicError):
                decode_document(
                    raw.replace('"extra": {}', '"extra": {"number": ' + number + "}")
                )

    def test_rejects_coercion_missing_required_fields_and_edited_checksum(self):
        mutations = [
            lambda v: v.update(format_version=True),
            lambda v: v["content"].update(database_schema="3"),
            lambda v: v["content"]["files"][0].update(size=100),
            lambda v: v["content"].pop("layout"),
            lambda v: v["content"]["associations"][0].update(active=1),
        ]
        for change in mutations:
            value = self.sample()
            change(value)
            with self.assertRaises(CatabolicError):
                validate_document(seal(value))
        value = self.sample()
        value["content"]["extra"]["edited"] = True
        with self.assertRaisesRegex(CatabolicError, "checksum"):
            validate_document(value)

    def test_rejects_dangling_duplicate_and_inconsistent_records(self):
        mutations = [
            lambda c: c["entries"][0].update(item_id="absent"),
            lambda c: c["entries"][0].update(association_ids=[]),
            lambda c: c["relationships"][0].update(target_id="absent"),
            lambda c: c["counts"].update(items=100),
            lambda c: c["items"].append(copy.deepcopy(c["items"][0])),
            lambda c: c["associations"][0].update(active=False),
            lambda c: c.update(filesystem_verified=True),
            lambda c: c["entries"][0].update(recorded_link_matches_desired=True),
        ]
        for change in mutations:
            value = self.sample()
            change(value["content"])
            with self.assertRaises(CatabolicError):
                validate_document(seal(value))

    def test_record_budget_precedes_model_allocation(self):
        value = self.sample()
        value["content"]["items"] = [None] * 100001
        with patch(
            "catabolic.interchange.validation.DocumentV1.model_validate"
        ) as validate:
            with self.assertRaisesRegex(CatabolicError, "exceeds 100000 items"):
                validate_document(value)
            validate.assert_not_called()

    def test_schema_drift_fails_even_if_runtime_accepts_added_fields(self):
        candidate = specification.schema()
        candidate["properties"]["format_version"]["const"] = 99
        self.assertIn(
            "/properties/format_version/const",
            specification.changes(specification.schema(), candidate),
        )
        with patch.object(specification, "schema", return_value=candidate):
            with self.assertRaisesRegex(CatabolicError, "frozen v1"):
                specification.check_release()

    def test_cli_works_without_database_and_preserves_extensions(self):
        prefix = [sys.executable, "-m", "catabolic", "spec"]
        for operation in ["schema", "check", "docs"]:
            result = subprocess.run(
                [*prefix, operation], capture_output=True, text=True
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            if operation == "docs":
                self.assertEqual(result.stdout, specification.reference_text())
            else:
                self.assertIsInstance(json.loads(result.stdout), dict)
                if operation == "schema":
                    self.assertEqual(result.stdout, specification.schema_text())
        value = self.sample()
        value["vendor:unknown"] = {"keep": [False, 12]}
        value.update(safe=False, healthy=False, complete=False)
        result = subprocess.run(
            [*prefix, "roundtrip", "--file", "-"],
            input=encode(value),
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), value)
        result = subprocess.run(
            [*prefix, "validate", "--file", "-"],
            input='{"format": "future"}',
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("error", json.loads(result.stderr))
        result = subprocess.run(
            [*prefix, "diff", "--against", "-"],
            input="{}",
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 3)
        self.assertEqual(json.loads(result.stdout)["compatibility"], "review_required")

    def test_release_history_guard_rejects_changes_but_allows_new_versions(self):
        script = Path(__file__).resolve().parents[1] / "scripts/check_spec_releases.py"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            releases = root / "src/catabolic/interchange/releases"
            releases.mkdir(parents=True)
            artifact = releases / "manifest-v1.schema.json"
            artifact.write_text("{}")
            index = releases / "index.json"
            index.write_text('{"manifest-v1.schema.json": "original"}')

            def git(*args):
                return subprocess.run(
                    ["git", *args], cwd=root, capture_output=True, text=True, check=True
                ).stdout.strip()

            git("init", "-q")
            git("add", ".")
            git(
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                "fixture",
            )
            base = git("rev-parse", "HEAD")

            def check():
                return subprocess.run(
                    [sys.executable, str(script), base],
                    cwd=root,
                    capture_output=True,
                    text=True,
                )

            self.assertEqual(check().returncode, 0)
            (releases / "manifest-v2.schema.json").write_text("{}")
            index.write_text(
                '{"manifest-v1.schema.json": "original", "manifest-v2.schema.json": "new"}'
            )
            self.assertEqual(check().returncode, 0)
            artifact.write_text('{"changed": true}')
            self.assertNotEqual(check().returncode, 0)
            artifact.write_text("{}")
            index.write_text('{"manifest-v1.schema.json": "changed"}')
            self.assertNotEqual(check().returncode, 0)
