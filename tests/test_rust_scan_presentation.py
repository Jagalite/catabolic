# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Complete frozen presentation envelopes and scoped live ordinary scan facts.

Fixed report replay proves presentation only. Live scans retain actual owner
values and explicitly assert the native stricter freshness floor; full report
parity is neither normalized nor asserted here.
"""

import hashlib
import json
import sqlite3
import subprocess
import unittest
from contextlib import closing
from pathlib import Path

from tests import test_rust_maintenance_cli as maintenance
from tests.scan_report_support import assert_completed_report_facts
from tests.test_rust_legacy_parser import PYTHON, REFERENCE, environment

ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "tests/parity/scan-report-presentation-v1.json"
ORACLE = r"""
import contextlib,io,json,sys
from pathlib import Path
from unittest.mock import patch
sys.path.insert(0,sys.argv[1])
from catabolic import cli
assert Path(cli.__file__).resolve()==Path(sys.argv[1]).resolve()/'catabolic/cli.py'
assert sys.version.split()[0]=='3.14.6'
for line in sys.stdin:
 case=json.loads(line);out=io.StringIO();err=io.StringIO()
 def dispatch(args):
  if args.command=='watch':
   print(json.dumps({'cycle':1,**case['value']['last']}),file=sys.stderr)
  elif args.command=='maintenance':
   for stage in case['value']['stages']:
    print(json.dumps({'stage':stage['stage'],'complete':stage['complete']}),file=sys.stderr)
  return case['value']
 with patch.object(cli,'dispatch',dispatch),contextlib.redirect_stdout(out),contextlib.redirect_stderr(err):
  code=cli.main(case['argv'])
 print(json.dumps(dict(code=code,stdout=out.getvalue(),stderr=err.getvalue())))
"""


class ScanPresentationFixtureTest(unittest.TestCase):
    maxDiff = None

    def test_complete_frozen_envelopes_remain_exact(self):
        fixture = json.loads(FIXTURE.read_text())
        self.assertEqual(fixture["schema"], 1)
        self.assertEqual(len(fixture["cases"]), 9)
        self.assertEqual(
            hashlib.sha256(
                (REFERENCE / "src/catabolic/cli.py").read_bytes()
            ).hexdigest(),
            fixture["oracle_cli_sha256"],
        )
        result = subprocess.run(
            [str(PYTHON), "-B", "-c", ORACLE, str(REFERENCE / "src")],
            input="".join(json.dumps(case) + "\n" for case in fixture["cases"]),
            text=True,
            capture_output=True,
            env=environment(),
            check=True,
            timeout=30,
        )
        self.assertEqual(result.stderr, "")
        actual = list(map(json.loads, result.stdout.splitlines()))
        self.assertEqual(len(actual), 9)
        for case, output in zip(fixture["cases"], actual, strict=True):
            with self.subTest(case=case["name"]):
                self.assertEqual(output, case["expected"])


class ScanPresentationLiveTest(unittest.TestCase):
    maxDiff = None
    setUp = maintenance.NativeMaintenanceCliTest.setUp
    setup = maintenance.NativeMaintenanceCliTest.setup
    run_cli = maintenance.NativeMaintenanceCliTest.run_cli

    def assert_presentation(self, oracle, native):
        assert_completed_report_facts(self, oracle, native)
        left, right = oracle["observations"][0], native["observations"][0]
        for path in (
            (),
            ("guarantees",),
            ("guarantees", "execution"),
            ("guarantees", "execution", "budgets"),
            ("report", "validation", "policy"),
        ):
            a, b = left, right
            for key in path:
                a, b = a[key], b[key]
            self.assertEqual(list(a), list(b), path)
        self.assertEqual(
            list(oracle["scans"][0]["validation"]["policy"]),
            list(native["scans"][0]["validation"]["policy"]),
        )
        for observation in (left, right):
            self.assertIs(type(observation["guarantees"]["max_age"]), int)
            self.assertEqual(observation["guarantees"]["max_age"], 0)
        with closing(sqlite3.connect(self.native)) as db:
            raw = db.execute(
                "SELECT guarantees FROM observation_requests WHERE id=?",
                (right["request_id"],),
            ).fetchone()[0]
        self.assertIs(type(json.loads(raw)["max_age"]), float)

    def test_direct_scan_json_machine_presentation_and_owner_types(self):
        self.setup()
        for mode in ("--json", "--machine"):
            with self.subTest(mode=mode):
                results = [self.run_cli(ref, [mode, "scan"]) for ref in (True, False)]
                self.assertEqual(tuple(p.returncode for p in results), (0, 0))
                self.assertEqual(tuple(p.stderr for p in results), ("", ""))
                bodies = [json.loads(p.stdout) for p in results]
                if mode == "--machine":
                    bodies = [body["data"] for body in bodies]
                self.assert_presentation(*bodies)

    def test_maintenance_json_machine_presentation_and_owner_types(self):
        self.setup()
        for mode in ("--json", "--machine"):
            with self.subTest(mode=mode):
                results = [
                    self.run_cli(ref, [mode, "maintenance", "--inventory-only"])
                    for ref in (True, False)
                ]
                self.assertEqual(tuple(p.returncode for p in results), (0, 0))
                self.assertEqual(results[0].stderr, results[1].stderr)
                bodies = [json.loads(p.stdout) for p in results]
                if mode == "--machine":
                    bodies = [body["data"] for body in bodies]
                self.assert_presentation(*(body["stages"][1] for body in bodies))

    def test_watch_stdout_and_progress_presentation_and_owner_types(self):
        self.setup()
        for mode in ("--json", "--machine"):
            with self.subTest(mode=mode):
                results = [
                    self.run_cli(ref, [mode, "watch", "--kind", "sniff"])
                    for ref in (True, False)
                ]
                self.assertEqual(tuple(p.returncode for p in results), (0, 0))
                bodies = [json.loads(p.stdout) for p in results]
                if mode == "--machine":
                    bodies = [body["data"] for body in bodies]
                self.assert_presentation(*(body["last"]["scan"] for body in bodies))
                progress = [json.loads(p.stderr) for p in results]
                self.assert_presentation(*(body["scan"] for body in progress))


if __name__ == "__main__":
    unittest.main()
