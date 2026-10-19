# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Narrow public resume forwarding and read-only report regressions."""

import json
import os
import sqlite3
import sys
import unittest
from contextlib import closing
from pathlib import Path

from tests import test_rust_maintenance_cli as maintenance


class ScanReportCliTest(unittest.TestCase):
    maxDiff = None
    setUp = maintenance.NativeMaintenanceCliTest.setUp
    setup = maintenance.NativeMaintenanceCliTest.setup
    run_cli = maintenance.NativeMaintenanceCliTest.run_cli
    text = maintenance.NativeMaintenanceCliTest.text
    normalize = maintenance.NativeMaintenanceCliTest.normalize
    compare = maintenance.NativeMaintenanceCliTest.compare

    def snapshot(self, path):
        with closing(sqlite3.connect(path)) as db:
            tables = db.execute(
                "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"
            ).fetchall()
            return {
                name: db.execute('SELECT * FROM "' + name + '"').fetchall()
                for (name,) in tables
            }

    def test_empty_public_scan_text_is_exact(self):
        self.setup("empty", files=0)
        self.compare(["scan"])

    def test_public_resume_json_machine_are_read_only(self):
        self.setup()
        for mode in ("--json", "--machine"):
            with self.subTest(mode=mode):
                for reference, path in ((True, self.ref), (False, self.native)):
                    first = self.run_cli(reference, [mode, "scan"])
                    self.assertEqual(first.returncode, 0, first.stderr)
                    initial = json.loads(first.stdout)
                    if mode == "--machine":
                        initial = initial["data"]
                    before = self.snapshot(path)
                    request = initial["observations"][0]["request_id"]
                    resumed = self.run_cli(
                        reference, [mode, "scan", "--request-id", request]
                    )
                    self.assertEqual(resumed.returncode, 0, resumed.stderr)
                    value = json.loads(resumed.stdout)
                    if mode == "--machine":
                        value = value["data"]
                    self.assertEqual(set(value), {"complete", "scans", "observations"})
                    self.assertTrue(value["complete"])
                    self.assertEqual(value["scans"], initial["scans"])
                    expected = initial["observations"][0] | {"reuse": "resumed"}
                    self.assertEqual(value["observations"], [expected])
                    self.assertEqual(self.snapshot(path), before)
                    self.assertEqual(resumed.stderr, "")

    def test_resume_exclusions_keep_error_precedence_and_have_no_effects(self):
        self.setup()
        requests = []
        for reference in (True, False):
            first = self.run_cli(reference, ["--json", "scan"])
            self.assertEqual(first.returncode, 0, first.stderr)
            requests.append(json.loads(first.stdout)["observations"][0]["request_id"])
        for mode in ([], ["--json"], ["--machine"]):
            for extra in (
                ["--exclude", "Notes-0000.txt"],
                ["--exclude", "Notes-0000.txt", "--extended"],
                ["media", "--exclude", "Notes-0000.txt"],
                ["--scope", "subdir", "--exclude", "Notes-0000.txt"],
            ):
                with self.subTest(mode=mode, extra=extra):
                    before = [self.snapshot(path) for path in (self.ref, self.native)]
                    results = [
                        self.run_cli(
                            reference, mode + ["scan", "--request-id", request] + extra
                        )
                        for reference, request in zip(
                            (True, False), requests, strict=True
                        )
                    ]
                    self.assertEqual(tuple(p.returncode for p in results), (2, 2))
                    self.assertEqual(results[0].stdout, results[1].stdout)
                    self.assertEqual(results[0].stderr, results[1].stderr)
                    self.assertEqual(
                        [self.snapshot(path) for path in (self.ref, self.native)],
                        before,
                    )


if __name__ == "__main__":
    if "--diagnose-full-scan-text" in sys.argv:
        case = ScanReportCliTest()
        case.setUp()
        try:
            case.setup()
            results = [case.run_cli(reference, ["scan"]) for reference in (True, False)]
            destination = Path(os.environ["CATABOLIC_SCAN_DIAGNOSTIC_DIR"])
            destination.mkdir(parents=True, exist_ok=True)
            for name, result in zip(("oracle", "native"), results, strict=True):
                (destination / f"text-{name}.stdout").write_text(result.stdout)
                (destination / f"text-{name}.stderr").write_text(result.stderr)
            case.assertEqual(tuple(p.returncode for p in results), (0, 0))
            case.assertEqual(results[0].stderr, results[1].stderr)
            case.assertEqual(case.text(results[0].stdout), case.text(results[1].stdout))
        finally:
            case.doCleanups()
    else:
        unittest.main()
