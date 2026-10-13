# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Ordinary local administration transport; synthetic facts, no secret files."""

import hashlib
import importlib.util
import json
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
from contextlib import closing
from pathlib import Path

from tests.test_rust_legacy_cli import BINARY, ORACLE
from tests.test_rust_legacy_parser import PYTHON, REFERENCE, environment


class LegacyAccessAdmin(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        root = Path(__file__).resolve().parents[1]
        spec = importlib.util.spec_from_file_location(
            "access_cli_provenance", root / "scripts/qualify_native_embedding.py"
        )
        cls.provenance = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.provenance)
        cls.frozen = cls.provenance.reference_provenance(
            REFERENCE, "453fca983222c6665a48775eef67c489c96b527e"
        )
        version = subprocess.check_output(
            [
                str(PYTHON),
                "-I",
                "-c",
                "import sys;print('.'.join(map(str,sys.version_info[:3])))",
            ],
            text=True,
        ).strip()
        if version != "3.14.6":
            raise AssertionError("CLI oracle requires Python 3.14.6")
        cls.binary_sha256 = hashlib.sha256(BINARY.read_bytes()).hexdigest()

    @classmethod
    def tearDownClass(cls):
        if (
            cls.provenance.reference_provenance(REFERENCE, cls.frozen["head"])
            != cls.frozen
        ):
            raise AssertionError("CLI oracle changed during fixtures")
        if hashlib.sha256(BINARY.read_bytes()).hexdigest() != cls.binary_sha256:
            raise AssertionError("CLI binary changed during fixtures")

    def setUp(self):
        self.assertTrue(BINARY.is_file(), "build catabolic-legacy-native first")
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.paths = {True: self.root / "reference.db", False: self.root / "native.db"}
        self.env = environment(overrides={"HOME": str(self.root / "home")})
        self.env["PATH"] = str(self.root / "empty-path")
        result = self.run_cli(True, ["init"], prefix=[])
        self.assertEqual(result.returncode, 0, result.stderr)
        with closing(sqlite3.connect(self.paths[True])) as db:
            db.execute("INSERT INTO profiles VALUES ('alternate')")
            db.execute("INSERT INTO locations VALUES ('source')")
            db.execute("INSERT INTO locations VALUES ('output')")
            db.execute(
                "INSERT INTO bindings VALUES('default','source','source','/synthetic-admin-source',1,2)"
            )
            db.execute("INSERT INTO generated_locations VALUES('default','output')")
            db.execute(
                "INSERT INTO processing_recipes(id,name,revision,preset,definition,digest,operation_kind) VALUES ('recipe','recipe',1,'fixture','{}','fixture','render')"
            )
            db.execute(
                "INSERT INTO processing_recipes(id,name,revision,preset,definition,digest,operation_kind) VALUES ('analysis','analysis',1,'fixture','{}','analysis','analysis')"
            )
            db.execute("INSERT INTO api_principals VALUES('fixture','default',1)")
            db.execute("INSERT INTO api_principals VALUES('other','alternate',1)")
            for name, principal in (
                ("fake-token", "fixture"),
                ("other-token", "other"),
            ):
                db.execute(
                    "INSERT INTO api_tokens VALUES(?,?,?,'[]',4102444800,0)",
                    (
                        name,
                        principal,
                        hashlib.sha256(b"fake-local-fixture").hexdigest(),
                    ),
                )
            db.commit()
        shutil.copyfile(self.paths[True], self.paths[False])
        self.grant = self.root / "grant.json"
        self.grant.write_text(json.dumps({"actions": ["metadata:read"]}))

    def run_cli(self, reference, arguments, prefix=None, database=True):
        command = (
            [str(PYTHON), "-c", ORACLE, str(REFERENCE / "src")]
            if reference
            else [str(BINARY)]
        )
        return subprocess.run(
            command
            + (["--db", str(self.paths[reference])] if database else [])
            + (prefix if prefix is not None else ["--machine"])
            + arguments,
            env=self.env,
            cwd=self.root,
            capture_output=True,
            text=True,
            timeout=30,
        )

    def state(self, reference):
        with closing(sqlite3.connect(self.paths[reference])) as db:
            return {
                name: db.execute(query).fetchall()
                for name, query in (
                    ("principals", "SELECT * FROM api_principals ORDER BY id"),
                    ("grants", "SELECT * FROM api_grants ORDER BY id"),
                    ("tokens", "SELECT * FROM api_tokens ORDER BY id"),
                    ("sources", "SELECT * FROM api_sources ORDER BY profile,location"),
                    ("operations", "SELECT * FROM api_operations ORDER BY id"),
                    (
                        "audit",
                        "SELECT id,principal,action,resource FROM api_audit ORDER BY id",
                    ),
                    ("private", "SELECT * FROM api_private_files"),
                    ("sequences", "SELECT name,seq FROM sqlite_sequence ORDER BY name"),
                )
            }

    def compare(self, arguments, **kwargs):
        reference, native = [
            self.run_cli(engine, arguments, **kwargs) for engine in (True, False)
        ]
        left, right = (
            (native.returncode, native.stdout, native.stderr),
            (reference.returncode, reference.stdout, reference.stderr),
        )
        if left != right and "IntegrityError" in reference.stdout:
            # Existing Store::Error is string-only: SQLite class parity remains
            # an explicit inherited gap, not a claimed exact envelope match.
            self.assertIn("CatabolicError", native.stdout)
            right = (
                reference.returncode,
                reference.stdout.replace("IntegrityError", "CatabolicError"),
                reference.stderr,
            )
        self.assertEqual(left, right, arguments)
        self.assertEqual(self.state(False), self.state(True), arguments)
        return native

    def test_principal_grant_disable_and_envelopes(self):
        for prefix in ([], ["--json"], ["--machine"]):
            for arguments in (
                ["api", "principal-put", "new"],
                ["api", "grant-put", "new", "--file", str(self.grant), "--id", "grant"],
                ["api", "grant-disable", "grant"],
                ["api", "principal-disable", "new"],
                ["api", "principal-disable", "missing"],
                ["--profile", "alternate", "api", "principal-put", "new"],
            ):
                self.compare(arguments, prefix=prefix)

    def test_tokens_profile_revoke_and_audit_rollback(self):
        for arguments in (
            ["api", "tokens"],
            ["--profile", "alternate", "api", "tokens"],
            ["api", "token-revoke", "fake-token"],
            ["api", "token-revoke", "fake-token"],
            ["api", "token-revoke", "missing"],
        ):
            self.compare(arguments)
        for path in self.paths.values():
            with closing(sqlite3.connect(path)) as db:
                # A trigger installed before CLI launch fails schema admission,
                # so it cannot establish mutation rollback. This valid row
                # exhausts AUTOINCREMENT only when the owner inserts its audit.
                db.execute(
                    "INSERT INTO api_audit(id,action) VALUES "
                    "(9223372036854775807,'fixture-sequence-exhaustion')"
                )
                db.commit()
        before = self.state(True)
        attempts = (
            ["api", "token-revoke", "other-token"],
            [
                "api",
                "grant-put",
                "fixture",
                "--file",
                str(self.grant),
                "--id",
                "rollback",
            ],
        )
        for prefix in ([], ["--json"], ["--machine"]):
            for arguments in attempts:
                results = [
                    self.run_cli(engine, arguments, prefix=prefix)
                    for engine in (True, False)
                ]
                for engine, result in zip((True, False), results, strict=True):
                    self.assertEqual(result.returncode, 2)
                    self.assertEqual(self.state(engine), before)
                    if prefix:
                        payload = json.loads(
                            result.stdout if prefix == ["--machine"] else result.stderr
                        )
                        # This SQLite exception-class difference remains open.
                        error = (
                            payload["data"]["error"]
                            if prefix == ["--machine"]
                            else payload["error"]
                        )
                        self.assertEqual(
                            error,
                            {
                                "message": "database or disk is full",
                                "type": "OperationalError"
                                if engine
                                else "CatabolicError",
                            },
                        )
                    else:
                        self.assertEqual(
                            result.stderr, "error: database or disk is full\n"
                        )
                self.assertEqual(
                    results[0].stdout.replace("OperationalError", "CatabolicError"),
                    results[1].stdout,
                )
                self.assertEqual(
                    results[0].stderr.replace("OperationalError", "CatabolicError"),
                    results[1].stderr,
                )
        for path in self.paths.values():
            with closing(sqlite3.connect(path)) as db:
                db.execute("DELETE FROM api_audit WHERE id=9223372036854775807")
                db.execute(
                    "UPDATE sqlite_sequence SET seq=(SELECT max(id) FROM api_audit) WHERE name='api_audit'"
                )
                db.commit()
        for arguments in attempts:
            self.assertEqual(self.compare(arguments).returncode, 0)

    def test_source_exposure_binding_and_profiles(self):
        for arguments in (
            ["api", "source-expose", "source"],
            ["api", "source-expose", "source"],
            ["api", "source-expose", "source", "--disable"],
            ["--profile", "alternate", "api", "source-expose", "source"],
            ["api", "source-expose", "missing", "--disable"],
        ):
            self.compare(arguments)

    def test_operation_approval_destination_and_conflicts(self):
        for arguments in (
            [
                "api",
                "operation-approve",
                "missing",
                "--location",
                "output",
                "--id",
                "op",
            ],
            [
                "api",
                "operation-approve",
                "analysis",
                "--location",
                "output",
                "--id",
                "op",
            ],
            [
                "api",
                "operation-approve",
                "recipe",
                "--location",
                "source",
                "--id",
                "op",
            ],
            [
                "api",
                "operation-approve",
                "recipe",
                "--location",
                "output",
                "--id",
                "op",
            ],
            [
                "api",
                "operation-approve",
                "recipe",
                "--location",
                "output",
                "--id",
                "op",
            ],
            ["api", "operation-disable", "op"],
            ["api", "operation-disable", "missing"],
        ):
            self.compare(arguments)

    def test_bounded_grant_file_and_error_envelopes(self):
        for text in ("{}", "[]", '{"actions": [true]}', " " * 262145):
            self.grant.write_text(text)
            self.compare(
                [
                    "api",
                    "grant-put",
                    "fixture",
                    "--file",
                    str(self.grant),
                    "--id",
                    "invalid",
                ]
            )
        self.compare(["api", "principal-put", "fixture"], database=False)
        # Shared native JSON decoding deliberately does not emulate CPython's
        # diagnostic wording/location. Both refuse without any row mutation.
        self.grant.write_text("{")
        before = self.state(False)
        for engine in (True, False):
            result = self.run_cli(
                engine, ["api", "grant-put", "fixture", "--file", str(self.grant)]
            )
            self.assertEqual(result.returncode, 2)
            self.assertIn(
                "JSONDecodeError" if engine else "invalid JSON", result.stdout
            )
            self.assertEqual(self.state(engine), before)

        # Grant-return definitions use the caller's insertion order in Python.
        # Native's shared JSON value transport canonicalizes that presentation;
        # compare semantic output/state and record this as an explicit gap.
        self.grant.write_text(
            '{"revisions":{"z":"last","a":"first"},"actions":["metadata:read"]}'
        )
        results = [
            self.run_cli(
                engine,
                [
                    "api",
                    "grant-put",
                    "fixture",
                    "--file",
                    str(self.grant),
                    "--id",
                    "ordered",
                ],
            )
            for engine in (True, False)
        ]
        self.assertEqual([result.returncode for result in results], [0, 0])
        self.assertEqual(json.loads(results[0].stdout), json.loads(results[1].stdout))
        self.assertNotEqual(results[0].stdout, results[1].stdout)
        self.assertEqual(self.state(True), self.state(False))

    def test_invalid_issuance_and_serve_remain_refused(self):
        before = self.state(False)
        destination = self.root / "must-not-exist.json"
        for arguments in (
            [
                "api",
                "token-create",
                "fixture",
                "--grant",
                "grant",
                "--output",
                str(destination),
            ],
            ["api", "token-rotate", "fake-token", "--output", str(destination)],
            ["api", "serve"],
        ):
            result = self.run_cli(False, arguments)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn("fake-local-fixture", result.stdout + result.stderr)
        self.assertEqual(self.state(False), before)
        self.assertFalse(destination.exists())


if __name__ == "__main__":
    unittest.main()
