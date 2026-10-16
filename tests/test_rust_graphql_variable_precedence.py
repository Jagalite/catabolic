# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Variable-JSON refusal precedence: integer conversion versus later syntax.

Frozen ``json.loads`` converts each integer token while scanning, so a token
beyond CPython's default 4,300-digit limit fails before any later syntax defect,
while an earlier syntax defect still wins. Every case compares complete stdout,
stderr and exit status with the live frozen oracle on disposable catalogs.
"""

import hashlib
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from tests import test_rust_legacy_parser as fixture
from tests.test_rust_graphql_variable_admission import ORACLE

BIG = "1" * 4301
LIMIT = "1" * 4300
CASES = {
    # Integer conversion precedes the later syntax defect.
    "huge-then-trailing-comma": '{"x":' + BIG + ", }",
    "negative-huge-then-garbage": '{"x":-' + BIG + "x",
    "huge-in-array-then-trailing-comma": '{"x":[' + BIG + ", ]}",
    "top-level-array-huge-then-comma": "[" + BIG + ", ]",
    "surrogate-then-huge-then-eof": '{"a":"\\ud800","x":' + BIG,
    "nonfinite-then-huge-then-missing-colon": '{"a":NaN,"x":' + BIG + ',"y" 1}',
    "huge-key-value-then-unterminated": '{"x":' + BIG + ',"y":"open',
    # The earlier syntax defect, or a non-integer token, still wins.
    "syntax-before-huge": '{"x":[1, ],"y":' + BIG + "}",
    "missing-colon-before-huge": '{"x" ' + BIG + "}",
    "leading-zero-then-huge": '{"x":0' + BIG + "}",
    "huge-fraction-then-comma": '{"x":' + BIG + ".5, }",
    "huge-exponent-then-comma": '{"x":' + BIG + "e1, }",
    "extra-data-huge": '{"x":1} ' + BIG,
    "limit-integer-then-comma": '{"x":' + LIMIT + ", }",
    "bom-before-huge": "﻿" + '{"x":' + BIG + "}",
}


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


class GraphQLVariablePrecedenceTest(unittest.TestCase):
    def test_integer_conversion_and_syntax_precedence(self):
        legacy = Path(os.environ["CATABOLIC_LEGACY_BINARY"])
        identity = digest(legacy)
        with tempfile.TemporaryDirectory(prefix="graphql-variable-precedence-") as raw:
            root = Path(raw)
            env = fixture.environment(overrides={"HOME": str(root / "home")})
            oracle = [str(fixture.PYTHON), "-c", ORACLE, str(fixture.REFERENCE / "src")]
            databases = {"oracle": root / "oracle.db", "native": root / "native.db"}
            initialized = subprocess.run(
                oracle + ["--db", str(databases["oracle"]), "init"],
                capture_output=True,
                env=env,
                timeout=90,
            )
            self.assertEqual(initialized.returncode, 0, initialized.stderr)
            shutil.copyfile(databases["oracle"], databases["native"])
            before = {name: digest(path) for name, path in databases.items()}
            commands = {"oracle": oracle, "native": [str(legacy)]}
            compared = 0
            for name, variables in CASES.items():
                for mode in ([], ["--json"], ["--machine"]):
                    with self.subTest(case=name, mode=mode):
                        outputs = {}
                        for engine, command in commands.items():
                            result = subprocess.run(
                                command
                                + ["--db", str(databases[engine])]
                                + mode
                                + ["graphql", "{profile}", "--variables", variables],
                                capture_output=True,
                                env=env,
                                timeout=90,
                            )
                            outputs[engine] = (
                                result.returncode,
                                result.stdout.decode(),
                                result.stderr.decode(),
                            )
                        self.assertEqual(outputs["native"], outputs["oracle"])
                        self.assertNotEqual(outputs["oracle"][0], 0)
                        compared += 1
            self.assertEqual(
                {name: digest(path) for name, path in databases.items()}, before
            )
        self.assertEqual(digest(legacy), identity)
        self.assertEqual(compared, 3 * len(CASES))


if __name__ == "__main__":
    unittest.main()
