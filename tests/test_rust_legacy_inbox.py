# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Frozen differential scenarios for read-only inbox and retained-record adapters.

Only disposable SQLite catalogs are used. This does not execute or qualify
filesystem mutation, recovery, authorization, or suggested action descriptors.
"""

import hashlib
import json
import os
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
from contextlib import closing
from pathlib import Path

from tests import test_rust_legacy_parser as parser_fixture

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(
    os.environ.get(
        "CATABOLIC_LEGACY_BINARY", ROOT / "target/debug/catabolic-legacy-native"
    )
)
ORACLE = "import sys;sys.path.insert(0,sys.argv.pop(1));from catabolic.cli import main;raise SystemExit(main(sys.argv[1:]))"


class NativeLegacyInboxTest(unittest.TestCase):
    def setUp(self):
        self.assertTrue(BINARY.is_file(), "build the explicit native legacy CLI")
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.reference = self.root / "reference.sqlite3"
        self.native = self.root / "native.sqlite3"
        self.env = parser_fixture.environment()
        self.env["HOME"] = str(self.root / "home")
        initialized = self.invoke(True, ["init"])
        self.assertEqual(initialized.returncode, 0, initialized.stderr)
        with closing(sqlite3.connect(self.reference)) as db, db:
            db.execute("INSERT INTO profiles(id) VALUES('other')")
            for name, status in [
                ("a", "pending"),
                ("b", "deferred"),
                ("c", "complete"),
                ("雪", "in_progress"),
            ]:
                db.execute(
                    "INSERT INTO items(id,kind,metadata) VALUES(?,?,?)",
                    (
                        name,
                        "movie",
                        json.dumps(
                            {"title": name, "opaque": 2**61 + 1}, ensure_ascii=False
                        ),
                    ),
                )
                db.execute(
                    "INSERT INTO item_workflow(item_id,status,revision,updated_at,updated_by) VALUES(?,?,1,'2026-01-01 00:00:00','fixture')",
                    (name, status),
                )
            db.execute(
                "INSERT INTO catalog_refresh_settings VALUES('default','global',1,0)"
            )
            db.execute(
                "INSERT INTO catalog_refresh_queue(profile,catalog,updated_at,error) VALUES('default','global','2026-01-01 00:00:00','offline')"
            )
            for profile, name in [
                ("default", "r1"),
                ("default", "r2"),
                ("default", "r3"),
                ("other", "private"),
            ]:
                db.execute(
                    "INSERT INTO retained_hardlinks VALUES(?,?,?,?,?,?,?)",
                    (
                        name,
                        profile,
                        "global",
                        "recorded/" + name,
                        "original/" + name,
                        '{"unicode":"雪","unknown":true}',
                        "2026-01-01 00:00:00",
                    ),
                )
        shutil.copyfile(self.reference, self.native)

    def invoke(self, reference, args):
        prefix = (
            [
                str(parser_fixture.PYTHON),
                "-c",
                ORACLE,
                str(parser_fixture.REFERENCE / "src"),
            ]
            if reference
            else [str(BINARY)]
        )
        return subprocess.run(
            prefix + ["--db", str(self.reference if reference else self.native)] + args,
            env=self.env,
            capture_output=True,
            text=True,
            timeout=90,
        )

    def compare(self, args):
        oracle, native = self.invoke(True, args), self.invoke(False, args)
        context = (args, oracle.stdout, oracle.stderr, native.stdout, native.stderr)
        self.assertEqual(native.returncode, oracle.returncode, context)
        self.assertEqual(native.stderr, oracle.stderr, context)
        if oracle.returncode == 0:
            self.assertEqual(
                json.loads(native.stdout), json.loads(oracle.stdout), context
            )
            return json.loads(native.stdout)
        self.assertEqual(native.stdout, oracle.stdout, context)
        return None

    def test_inbox_listing_summary_profiles_and_inactive(self):
        for args in [
            ["inbox", "list"],
            ["inbox", "list", "--include-inactive"],
            ["inbox", "summary"],
            ["--profile", "other", "inbox", "summary"],
            ["--profile", "other", "inbox", "list"],
        ]:
            with self.subTest(args=args):
                self.compare(["--machine"] + args)
        before = hashlib.sha256(self.native.read_bytes()).hexdigest()
        self.compare(["--machine", "inbox", "list"])
        self.assertEqual(hashlib.sha256(self.native.read_bytes()).hexdigest(), before)

    def test_keyset_pages_and_context_fences(self):
        page = self.compare(["--machine", "inbox", "list", "--limit", "1"])
        # Machine success wraps the command result; its exact wire shape is compared.
        result = page["data"]
        cursor = result["next_cursor"]
        self.assertIsNotNone(cursor)
        for tail in [[], ["--include-inactive"], ["--after", "bad-cursor"]]:
            args = [
                "--machine",
                "inbox",
                "list",
                "--limit",
                "1",
                "--after",
                cursor,
            ] + tail
            self.compare(args)
        self.compare(
            ["--machine", "--profile", "other", "inbox", "list", "--after", cursor]
        )

    def test_evidence_token_current_stale_unknown_and_profile(self):
        shown = self.compare(["--machine", "inbox", "show", "item:61"])
        result = shown["data"]
        token = result["evidence_token"]
        encoded = json.dumps(
            result["entry"], sort_keys=True, ensure_ascii=False, allow_nan=False
        )
        self.assertEqual(token, hashlib.sha256(encoded.encode()).hexdigest())
        self.compare(
            ["--machine", "inbox", "show", "item:61", "--expected-evidence", token]
        )
        for database in (self.reference, self.native):
            with closing(sqlite3.connect(database)) as db, db:
                db.execute(
                    "UPDATE item_workflow SET revision=revision+1 WHERE item_id='a'"
                )
        self.compare(
            ["--machine", "inbox", "show", "item:61", "--expected-evidence", token]
        )
        self.compare(["--machine", "inbox", "show", "absent"])
        self.compare(
            ["--machine", "--profile", "other", "inbox", "show", "refresh:676C6F62616C"]
        )

    def test_read_validation_and_error_channels(self):
        for args in [
            ["inbox", "list", "--limit", "0"],
            ["inbox", "list", "--limit", "1001"],
            ["inbox", "list", "--limit", "-1"],
            ["inbox", "list", "--limit", "2147483648"],
            ["inbox", "list", "--after", "bad"],
            ["inbox", "show", "missing", "--timeout-ms", "-1"],
            ["inbox", "summary", "--timeout-ms", "60001"],
            ["--profile", "unknown", "inbox", "summary"],
            ["--profile", "unknown", "inbox", "list"],
        ]:
            with self.subTest(args=args):
                self.compare(["--machine"] + args)

    def test_graphql_scalar_coercion_before_resolvers(self):
        document = "query($first:Int!){items(first:$first){nodes{id}}}"
        for value in [-2147483649, 2147483648, 2**63, 2**90, 1.0, 1.5, True, None, "1"]:
            with self.subTest(value=value):
                self.compare(
                    [
                        "--machine",
                        "graphql",
                        document,
                        "--variables",
                        json.dumps({"first": value}),
                    ]
                )
        for value in [1.0, 1.5, "a", None, True, 2**61 + 1]:
            with self.subTest(id=value):
                self.compare(
                    [
                        "--machine",
                        "graphql",
                        "query($id:ID!){item(id:$id){id}}",
                        "--variables",
                        json.dumps({"id": value}),
                    ]
                )
        document = "query A($n:Int!){items(first:$n){nodes{id}}} query B{profile}"
        for selected in ["A", "B", "Missing", None]:
            args = ["--machine", "graphql", document, "--variables", '{"n":2147483648}']
            if selected is not None:
                args += ["--operation-name", selected]
            self.compare(args)

    def test_retained_record_pages_unknown_catalog_and_profile(self):
        for args in [
            ["catalog", "retained", "global"],
            ["catalog", "retained", "global", "--limit", "1"],
            ["catalog", "retained", "global", "--limit", "1", "--after", "r1"],
            ["catalog", "retained", "global", "--after", "r3"],
            ["--profile", "other", "catalog", "retained", "global"],
            ["catalog", "retained", "missing"],
            ["catalog", "retained", "missing", "--limit", "0"],
            ["--profile", "unknown", "catalog", "retained", "global"],
        ]:
            with self.subTest(args=args):
                self.compare(["--machine"] + args)
        with closing(sqlite3.connect(self.native)) as db:
            self.assertEqual(
                db.execute("SELECT count(*) FROM retained_hardlinks").fetchone()[0], 4
            )


if __name__ == "__main__":
    unittest.main()
