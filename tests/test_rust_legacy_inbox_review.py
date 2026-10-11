# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Independent read-adapter regressions using disposable catalog copies only."""

import hashlib
import json
import shutil
import signal
import sqlite3
import subprocess
import time
import unittest
from contextlib import closing

from tests import test_rust_legacy_inbox as inbox_fixture

BINARY = inbox_fixture.BINARY


class NativeLegacyInboxReviewTest(unittest.TestCase):
    setUp = inbox_fixture.NativeLegacyInboxTest.setUp
    invoke = inbox_fixture.NativeLegacyInboxTest.invoke
    compare = inbox_fixture.NativeLegacyInboxTest.compare

    def graphql(self, document, variables, selected=None):
        args = ["--machine", "graphql", document, "--variables", json.dumps(variables)]
        if selected is not None:
            args += ["--operation-name", selected]
        return self.compare(args)

    def test_required_missing_errors_keep_definition_order(self):
        for declarations in ["$n:Int!,$id:ID!", "$id:ID!,$n:Int!"]:
            self.graphql(
                f"query({declarations}){{items(first:$n){{nodes{{id}}}} item(id:$id){{id}}}}",
                {"n": 2147483648},
            )
        self.graphql(
            "query($id:ID!,$n:Int!){items(first:$n){nodes{id}} item(id:$id){id}}",
            {},
        )
        self.graphql("query($n:Int! = 1){items(first:$n){nodes{id}}}", {})
        self.graphql("query($id:ID!){item(id:$id){id}}", {"id": None})

    def test_large_numeric_ids_and_integral_floats_remain_exact(self):
        numbers = [2**90 + 1, -(2**90 + 1), 10**400, -(10**400), 1, 0, int(1e100)]
        for database in (self.reference, self.native):
            with closing(sqlite3.connect(database)) as db, db:
                db.executemany(
                    "INSERT INTO items(id,kind,metadata) VALUES(?,'movie','{}')",
                    [(str(value),) for value in numbers],
                )
        for value in numbers + [1.0, -0.0, 1e100]:
            with self.subTest(value=value):
                result = self.graphql("query($id:ID!){item(id:$id){id}}", {"id": value})
                self.assertEqual(result["data"]["data"]["item"]["id"], str(int(value)))

    def test_large_integer_errors_and_selection_precedence(self):
        document = "query A($n:Int!){items(first:$n){nodes{id}}} query B{profile}"
        for value in [
            10**238,
            10**239,
            10**240,
            -(10**239),
            10**400,
            -(10**400),
            1e100,
        ]:
            with self.subTest(value=value):
                self.graphql(document, {"n": value}, "A")
        for selected in ["B", "Missing", "", None]:
            self.graphql(document, {"n": 10**400}, selected)
        self.graphql("query {profile}", {}, "A")
        self.graphql(
            "query($n:Int!){notAField items(first:$n){nodes{id}}}",
            {"n": 2147483648},
            "Missing",
        )

    def test_scalar_error_budget_matches_frozen_boundary(self):
        for count in [49, 50, 51, 52]:
            document = (
                "query("
                + ",".join(f"$v{i}:Int!" for i in range(count))
                + "){"
                + "".join(
                    f"a{i}:items(first:$v{i}){{nodes{{id}}}}" for i in range(count)
                )
                + "}"
            )
            for variables in [{}, {f"v{i}": 2147483648 for i in range(count)}]:
                with self.subTest(count=count, supplied=bool(variables)):
                    self.graphql(document, variables)

    def test_timeout_validation_precedes_catalog_open(self):
        self.reference = self.native = self.root / "missing.sqlite3"
        for operation in [["list"], ["show", "missing"], ["summary"]]:
            for timeout in ["0", "60001"]:
                self.compare(
                    ["--machine", "inbox"] + operation + ["--timeout-ms", timeout]
                )
        self.assertFalse(self.native.exists())

    def test_all_read_adapters_leave_database_bytes_unchanged(self):
        before = hashlib.sha256(self.native.read_bytes()).digest()
        for args in [
            ["inbox", "list", "--include-inactive", "--limit", "1"],
            ["inbox", "summary"],
            ["inbox", "show", "item:61", "--expected-evidence", ""],
            ["inbox", "show", "item:61", "--expected-evidence", "stale"],
            ["catalog", "retained", "global", "--limit", "1", "--after", "r1"],
            ["--profile", "other", "catalog", "retained", "global"],
            ["--profile", "unknown", "catalog", "retained", "missing", "--limit", "0"],
        ]:
            self.compare(["--machine"] + args)
            self.assertEqual(hashlib.sha256(self.native.read_bytes()).digest(), before)

    def test_inbox_cursor_rejects_another_catalog_identity(self):
        page = self.compare(["--machine", "inbox", "list", "--limit", "1"])
        cursor = page["data"]["next_cursor"]
        self.assertIsNotNone(cursor)
        self.reference = self.root / "second-reference.sqlite3"
        self.native = self.root / "second-native.sqlite3"
        initialized = self.invoke(True, ["init"])
        self.assertEqual(initialized.returncode, 0, initialized.stderr)
        shutil.copyfile(self.reference, self.native)
        self.compare(["--machine", "inbox", "list", "--after", cursor])

    def test_native_inbox_sigint_channels_and_catalog_reopen(self):
        # Hold a disposable SQLite read lock boundary so SIGINT cannot race a
        # fast empty inbox. Release it promptly after signalling; no tracing or
        # live files/processes are used.
        before = self.native.read_bytes()
        for machine in [False, True]:
            with closing(sqlite3.connect(self.native)) as lock:
                lock.execute("BEGIN EXCLUSIVE")
                args = [str(BINARY), "--db", str(self.native)]
                if machine:
                    args.append("--machine")
                args += ["inbox", "list", "--timeout-ms", "60000"]
                with subprocess.Popen(
                    args,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                    env=self.env,
                ) as process:
                    time.sleep(0.15)
                    self.assertIsNone(process.poll())
                    process.send_signal(signal.SIGINT)
                    lock.rollback()
                    out, err = process.communicate(timeout=10)
                self.assertEqual(process.returncode, 130)
                if machine:
                    self.assertEqual(err, "")
                    value = json.loads(out)
                    self.assertEqual(value["exit_code"], 130)
                    self.assertEqual(
                        value["data"]["error"]["type"], "KeyboardInterrupt"
                    )
                else:
                    self.assertEqual(out, "")
                    self.assertEqual(
                        err, "interrupted; use recover if an operation is pending\n"
                    )
            self.assertEqual(self.native.read_bytes(), before)
        self.compare(["--machine", "inbox", "summary"])


if __name__ == "__main__":
    unittest.main()
