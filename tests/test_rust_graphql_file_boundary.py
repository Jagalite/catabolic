# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Frozen CLI file/stdin byte admission, separate from GraphQL engine limits."""

import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from tests import test_rust_legacy_parser as fixture

ORACLE = "import sys;sys.path.insert(0,sys.argv.pop(1));from catabolic.cli import main;raise SystemExit(main(sys.argv[1:]))"
MAXIMUM = 131072


class GraphQLFileBoundaryTest(unittest.TestCase):
    def test_complete_file_and_stdin_boundaries_match_frozen_cli(self):
        legacy = Path(os.environ["CATABOLIC_LEGACY_BINARY"])
        self.assertTrue(legacy.is_file())
        from scripts.qualify_native_mutations import oracle_snapshot

        oracle_before = oracle_snapshot(fixture.REFERENCE, fixture.ROOT)
        self.assertEqual(oracle_before["reference_commit"], fixture.FROZEN_COMMIT)
        artifact_before = hashlib.sha256(legacy.read_bytes()).hexdigest()
        results = []
        with tempfile.TemporaryDirectory(prefix="graphql-file-boundary-") as scratch:
            root = Path(scratch)
            env = fixture.environment(
                overrides={
                    "HOME": str(root / "home"),
                    "PYTHONPYCACHEPREFIX": str(root / "pycache"),
                }
            )
            oracle = [str(fixture.PYTHON), "-c", ORACLE, str(fixture.REFERENCE / "src")]
            reference, native = root / "reference.db", root / "native.db"
            initialized = subprocess.run(
                oracle + ["--db", str(reference), "init"],
                capture_output=True,
                env=env,
                timeout=90,
            )
            self.assertEqual(initialized.returncode, 0, initialized.stderr)
            shutil.copyfile(reference, native)
            before = {
                str(path): hashlib.sha256(path.read_bytes()).hexdigest()
                for path in (reference, native)
            }

            def invoke(prefix, database, arguments, data):
                process = subprocess.run(
                    prefix + ["--db", str(database), *arguments],
                    input=data,
                    capture_output=True,
                    cwd=root,
                    env=env,
                    timeout=90,
                )
                stdout = process.stdout.decode("utf-8")
                return {
                    "code": process.returncode,
                    "stdout": json.loads(stdout) if stdout else "",
                    "stderr": process.stderr.decode("utf-8"),
                }

            for character in ("a", "é", "😀"):
                for size in (MAXIMUM - 1, MAXIMUM, MAXIMUM + 1):
                    prefix = "{profile}\n#"
                    length = size - len(prefix.encode())
                    width = len(character.encode())
                    document = (
                        prefix + character * (length // width) + " " * (length % width)
                    ).encode()
                    self.assertEqual(len(document), size)
                    query = root / "query.graphql"
                    query.write_bytes(document)
                    for source in (str(query), "-"):
                        for mode in ([], ["--json"], ["--machine"]):
                            arguments = mode + ["graphql", "--file", source]
                            data = document if source == "-" else b""
                            expected = invoke(oracle, reference, arguments, data)
                            actual = invoke([str(legacy)], native, arguments, data)
                            results.append(
                                {
                                    "character": character,
                                    "bytes": size,
                                    "source": "stdin" if source == "-" else "file",
                                    "mode": mode,
                                    "reference": expected,
                                    "native": actual,
                                    "match": fixture.strict(expected)
                                    == fixture.strict(actual),
                                }
                            )
                            self.assertEqual(
                                expected["code"], 2 if size > MAXIMUM else 0
                            )
            # Oversized file admission must precede malformed variable JSON.
            document = b" " * (MAXIMUM + 1)
            arguments = [
                "--machine",
                "graphql",
                "--file",
                "-",
                "--variables",
                "{",
            ]
            expected = invoke(oracle, reference, arguments, document)
            actual = invoke([str(legacy)], native, arguments, document)
            self.assertEqual(
                expected["stdout"]["data"]["error"]["message"],
                "input exceeds 131072 bytes",
            )
            results.append(
                {
                    "name": "file-limit-before-variables",
                    "reference": expected,
                    "native": actual,
                    "match": fixture.strict(expected) == fixture.strict(actual),
                }
            )
            self.assertEqual(
                {
                    str(path): hashlib.sha256(path.read_bytes()).hexdigest()
                    for path in (reference, native)
                },
                before,
            )
        self.assertEqual(
            hashlib.sha256(legacy.read_bytes()).hexdigest(), artifact_before
        )
        self.assertEqual(
            oracle_snapshot(fixture.REFERENCE, fixture.ROOT), oracle_before
        )
        if report := os.environ.get("CATABOLIC_GRAPHQL_FILE_BOUNDARY_REPORT"):
            Path(report).write_text(json.dumps(results, indent=2) + "\n")
        mismatches = [result for result in results if not result["match"]]
        self.assertFalse(mismatches, json.dumps(mismatches, indent=2))
        print(f"Frozen file/stdin admission pairs: {len(results)}")


if __name__ == "__main__":
    unittest.main()
