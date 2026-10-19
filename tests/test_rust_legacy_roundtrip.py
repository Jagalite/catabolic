# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Exact preserved CLI roundtrip bytes, using only disposable offline specs.

The native validation owner is unchanged. These tests exercise presentation,
including input permutations and objects whose keys resemble other CLI reports.
The reference always runs in a separate pinned Python 3.14.6 process.
"""

import copy
import hashlib
import json
import os
import random
import subprocess
import tempfile
import unittest
from pathlib import Path

from tests.test_rust_legacy_parser import PYTHON, REFERENCE, environment

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(
    os.environ.get(
        "CATABOLIC_LEGACY_BINARY", ROOT / "target/debug/catabolic-legacy-native"
    )
)
FIXTURE = ROOT / "tests/fixtures/interchange/manifest-v1.json"
ORACLE = """
import pathlib, sys
assert sys.version_info[:3] == (3, 14, 6)
source = pathlib.Path(sys.argv.pop(1)).resolve()
sys.path.insert(0, str(source))
from catabolic import cli
assert pathlib.Path(cli.__file__).resolve() == source / "catabolic/cli.py"
raise SystemExit(cli.main(sys.argv[1:]))
"""


def seal(value):
    raw = json.dumps(
        value["content"], sort_keys=True, ensure_ascii=False, allow_nan=False
    )
    value["content_sha256"] = hashlib.sha256(raw.encode()).hexdigest()
    return value


def sample(version):
    value = json.loads(FIXTURE.read_text())
    value["format_version"] = version
    if version >= 2:
        for key in ("tags", "tag_names", "tag_parents", "taggings"):
            value["content"][key] = []
            value["content"]["counts"][key] = 0
    if version == 3:
        value["content"]["catalog"]["link_mode"] = "symlink"
        for key in ("hardlinks", "retained_hardlinks"):
            value["content"][key] = []
            value["content"]["counts"][key] = 0
    return seal(value)


def permute(value, seed):
    randomizer = random.Random(seed)

    def walk(node):
        if isinstance(node, dict):
            fields = list(node.items())
            randomizer.shuffle(fields)
            return {key: walk(child) for key, child in fields}
        if isinstance(node, list):
            return [walk(child) for child in node]
        return node

    return walk(value)


class NativeLegacyRoundtripTest(unittest.TestCase):
    def setUp(self):
        self.assertTrue(BINARY.is_file(), "build catabolic-legacy-native first")
        self.assertTrue((REFERENCE / "src/catabolic/cli.py").is_file())
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.env = environment(overrides={"HOME": str(self.root / "home")})

    def run_cli(self, reference, args, raw=b""):
        command = (
            [str(PYTHON), "-c", ORACLE, str(REFERENCE / "src")]
            if reference
            else [str(BINARY)]
        )
        run = subprocess.run(
            command + args, input=raw, capture_output=True, env=self.env, timeout=90
        )
        return run.returncode, run.stdout, run.stderr

    def compare(
        self, scenario, raw, mode, *, from_file=False, code=0, operation="roundtrip"
    ):
        if isinstance(raw, str):
            raw = raw.encode()
        path = self.root / "input.json"
        if from_file:
            path.write_bytes(raw)
        args = mode + ["spec", operation, "--file", str(path) if from_file else "-"]
        with self.subTest(scenario=scenario, mode=mode, from_file=from_file):
            expected = self.run_cli(True, args, b"" if from_file else raw)
            actual = self.run_cli(False, args, b"" if from_file else raw)
            self.assertEqual(expected[0], code, expected)
            self.assertEqual(actual, expected, scenario)
            if from_file:
                self.assertEqual(path.read_bytes(), raw, "input must not be rewritten")
            return expected

    def test_declared_order_matches_every_frozen_model(self):
        # Verify presentation metadata, without running domain/model validation.
        script = """
import inspect, json, pathlib, sys, typing
assert sys.version_info[:3] == (3, 14, 6)
source = pathlib.Path(sys.argv[1]).resolve()
sys.path.insert(0, str(source))
from pydantic import BaseModel
from catabolic.interchange import v1, v2, v3
result = {}
for version, module in enumerate((v1, v2, v3), 1):
    assert pathlib.Path(module.__file__).resolve().parent == source / 'catabolic/interchange'
    fields = {}
    def visit(annotation):
        if inspect.isclass(annotation) and issubclass(annotation, BaseModel):
            if annotation.__name__ in fields:
                return
            fields[annotation.__name__] = list(annotation.model_fields)
            for field in annotation.model_fields.values():
                visit(field.annotation)
        else:
            for child in typing.get_args(annotation):
                visit(child)
    visit(module.Document)
    result[str(version)] = fields
print(json.dumps(result))
"""
        run = subprocess.run(
            [str(PYTHON), "-c", script, str(REFERENCE / "src")],
            capture_output=True,
            env=self.env,
            timeout=90,
        )
        self.assertEqual((run.returncode, run.stderr), (0, b""))
        models = json.loads(run.stdout)
        resource = json.loads(
            (ROOT / "crates/catabolic-cli/resources/legacy-cli.json").read_text()
        )
        for version, spec in resource["spec"].items():
            schema = spec["schema"]
            for name, model in [("Document", schema), *schema["$defs"].items()]:
                if "properties" not in model:
                    continue
                with self.subTest(version=version, model=name):
                    self.assertEqual(set(model["required"]), set(model["properties"]))
                    self.assertEqual(model["required"], models[version][name])

    def test_permuted_known_and_unknown_fields_exactly(self):
        extension = {
            "zulu": 10**400,
            "alpha": -(2**100),
            "unicode": 'Café 雪 😀 \\ " \n \t \u2028',
            "nested": [
                {"message": "extension", "type": "KeyboardInterrupt"},
                {"rows": [], "columns": [], "profile": "opaque"},
                {
                    "definition": {"z": 0, "a": 1},
                    "revision": 1,
                    "digest": "opaque",
                    "profile": "x",
                },
                {"next_cursor": None, "first": False},
                {"$serde_json::private::Number": "123"},
                [None, True, {}, [], 1.0, -0.0, 1e-7, 1e-5, 1e16, 5e-324],
            ],
        }

        def extend(node):
            if isinstance(node, dict):
                for child in list(node.values()):
                    extend(child)
                node["vendor:z"] = copy.deepcopy(extension)
                node["vendor:a"] = {"z": [], "a": {"雪": None, "a": True}}
            elif isinstance(node, list):
                for child in node:
                    extend(child)

        for version in (1, 2, 3):
            value = sample(version)
            extend(value)
            value["complete"] = False  # Spec success must still have exit 0.
            seal(value)
            for seed in range(6):
                raw = json.dumps(permute(value, seed), ensure_ascii=bool(seed % 2))
                for mode in ([], ["--json"], ["--machine", "--profile", "雪"]):
                    self.compare(
                        f"LC-ROUND-ORDER-v{version}-{seed}",
                        raw,
                        mode,
                        from_file=bool(seed % 2),
                    )

    def test_number_spellings_and_escaped_keys(self):
        value = sample(1)
        value["vendor:number"] = "TOKEN"
        value["vendor:keys"] = {'z\\"': 1, "a\n": 2, "😀": 3, "": 4}
        for spelling in (
            "1e2",
            "1.000",
            "-0",
            "1E-07",
            "-0.00",
            "1e-400",
            "1.234567890123456789",
        ):
            raw = json.dumps(value, ensure_ascii=True).replace('"TOKEN"', spelling)
            self.compare(f"LC-ROUND-NUMBER-{spelling}", raw, ["--machine"])

    def test_successful_validate_still_uses_ordinary_presentation(self):
        for version in (1, 2, 3):
            raw = json.dumps(sample(version))
            for mode in ([], ["--json"], ["--machine"]):
                self.compare(
                    f"LC-VALIDATE-UNCHANGED-v{version}", raw, mode, operation="validate"
                )

    def test_parser_and_owner_error_precedence_exactly(self):
        cases = {
            "empty": "",
            "truncated": '{"format":',
            "nonobject": "[]",
            "unsupported": '{"format_version":99}',
            "duplicate": '{"x":1,"x":2}',
            "duplicate-escaped": '{"x":1,"\\u0078":2}',
            "duplicate-before-tail": '{"x":1,"x":2} invalid',
            "duplicate-before-malformed-child": '{"x":1,"x":2,"child":[}',
            "nested-duplicate": '{"future":[{"z":1,"z":2}]}',
            "nonfinite-before-duplicate": '{"x":NaN,"x":1}',
            "duplicate-before-nonfinite": '{"x":1,"x":2,"y":Infinity}',
            "unsupported-before-missing": '{"format":"wrong","format_version":1}',
        }
        value = sample(1)
        value["content_sha256"] = "0" * 64
        cases["checksum"] = json.dumps(value)
        del value["generator"]
        cases["missing-before-checksum"] = json.dumps(value)
        for name, raw in cases.items():
            for mode in ([], ["--json"], ["--machine"]):
                self.compare(f"LC-ROUND-ERROR-{name}", raw, mode, code=2)
                # Same refusal owner and envelope as the unchanged validate path.
                self.compare(
                    f"LC-VALIDATE-ERROR-{name}", raw, mode, code=2, operation="validate"
                )
        for mode in ([], ["--json"], ["--machine"]):
            args = mode + ["spec", "roundtrip"]
            self.assertEqual(self.run_cli(False, args), self.run_cli(True, args))


if __name__ == "__main__":
    unittest.main()
