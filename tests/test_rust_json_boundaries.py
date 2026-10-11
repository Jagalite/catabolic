# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Numeric-token and object identity regressions at the native JSON boundary."""

import json
import subprocess
import unittest

from tests.test_rust_migration import BINARY


@unittest.skipUnless(BINARY.is_file(), "build catabolic-cli before qualification")
class NativeJsonBoundaryTest(unittest.TestCase):
    def test_tokens_and_private_sentinel_keep_python_values(self):
        for raw in (
            "1e2",
            "1.000",
            "-0",
            "-0.0",
            "1E-7",
            "1e16",
            "9999999999999999.0",
            "1208925819614629174706176",
            '{"$serde_json::private::Number":"1e400"}',
            '{"metadata":{"$serde_json::private::Number":"3"}}',
            '{"old":1,"old":2}',
        ):
            with self.subTest(raw=raw):
                result = subprocess.run(
                    [str(BINARY), "encode", raw],
                    capture_output=True,
                    text=True,
                    timeout=10,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(
                    json.loads(result.stdout)["encoded"],
                    json.dumps(
                        json.loads(raw),
                        sort_keys=True,
                        ensure_ascii=False,
                        allow_nan=False,
                    ),
                )

    def test_nonfinite_nested_tokens_reject(self):
        for raw in ("1e400", "-1e400", '{"metadata":[1e400]}'):
            with self.subTest(raw=raw):
                result = subprocess.run(
                    [str(BINARY), "encode", raw],
                    capture_output=True,
                    text=True,
                    timeout=10,
                )
                self.assertEqual(result.returncode, 2)
                self.assertEqual(result.stdout, "")

    def test_mutation_http_mode_requires_auth_before_open(self):
        result = subprocess.run(
            [
                str(BINARY),
                "--http",
                "--db",
                "/missing/catalog.db",
                "tag",
                "put",
                "--options",
                '{"name":"tag"}',
            ],
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("requires authorization context", result.stderr)
        self.assertNotIn("database does not exist", result.stderr)
