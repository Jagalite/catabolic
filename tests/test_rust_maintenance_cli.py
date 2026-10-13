# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Narrow frozen-oracle checks for the ordinary maintenance owner.

Only disposable sources/catalogs are used. Output publication is expected to
refuse; these tests cannot qualify M6 or the underlying effect owners.
"""

import json
import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from contextlib import closing
from pathlib import Path

from tests.scan_report_support import assert_completed_report_facts
from tests.test_rust_legacy_parser import PYTHON, REFERENCE, environment

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(
    os.environ.get(
        "CATABOLIC_LEGACY_BINARY", ROOT / "target/debug/catabolic-legacy-native"
    )
)
ORACLE = "import sys;sys.path.insert(0,sys.argv.pop(1));from catabolic.cli import main;raise SystemExit(main(sys.argv[1:]))"
SETUP = r"""
import sys
sys.path.insert(0,sys.argv.pop(1))
from catabolic.store import Store
from catabolic.app import Application
from catabolic.processing import Processing
from catabolic.item_workflow import ItemWorkflow
path,source,output,mode=sys.argv[1:]
Store.initialize(path)
with Store(path,writable=True) as store:
    app=Application(store)
    if mode!='empty':
        app.bind('source','media',source)
        app.bind('output','global',output)
    if mode in ('seeded','failed'):
        app.scan()
        file=app.files()['files'][0]['id']
        Processing(app).enqueue('hash',file_ids=[file])
        if mode=='failed':
            job=Processing(app).enqueue('sniff',file_ids=[file])['queued'][0]
            with store.transaction() as db: db.execute("UPDATE processing_jobs SET state='failed' WHERE id=?",(job,))
    if mode=='rule':
        from catabolic.operations import Operations
        from catabolic.rules import Rules
        app.scan()
        for row in store.rows('SELECT * FROM files'):
            item=app.put_item('document',{'fixture':row['path']},{'title':row['path']})['id']
            app.media.associate(row['id'],item)
        operation=Operations(app).put('sniff',{'kind':'analysis','operation':'sniff'})
        rule=Rules(app).put('sniff',operation['id'],None,{'language':'sql','query':'SELECT item_id FROM catalog_items'},required=True)
        Rules(app).enable(rule['id'],True)
    if mode=='summary':
        for status in ('pending','in_progress','complete','deferred','ignored'):
            item=app.put_item('document',{'fixture':status},{'title':status})['id']
            if status!='pending': ItemWorkflow(app).set_status(item,status,note='Fixture')
"""


class NativeMaintenanceCliTest(unittest.TestCase):
    maxDiff = None

    def setUp(self):
        self.assertTrue(BINARY.is_file(), "build the isolated native CLI first")
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.source = self.root / "source"
        self.source.mkdir()
        self.output = self.root / "output"
        self.output.mkdir()
        self.ref = self.root / "reference.db"
        self.native = self.root / "native.db"
        self.env = environment()
        self.env["HOME"] = str(self.root / "home")
        self.env["CATABOLIC_ENGINE"] = "python"

    def test_standalone_progress_write_failure_remains_ignored(self):
        self.setup(files=0)
        reader, writer = os.pipe()
        os.close(reader)
        try:
            result = subprocess.run(
                [
                    str(BINARY),
                    "--db",
                    str(self.native),
                    "--json",
                    "maintenance",
                    "--inventory-only",
                ],
                stdout=subprocess.PIPE,
                stderr=writer,
                env=self.env,
                text=True,
                timeout=30,
            )
        finally:
            os.close(writer)
        self.assertEqual(result.returncode, 0, result.stdout)
        report = json.loads(result.stdout)
        self.assertTrue(report["complete"])
        self.assertTrue(report["inventory_updated"])
        self.assertIsNone(report["stopped_at"])
        self.assertEqual(report["errors"], [])

    def setup(self, mode="source", files=1):
        for n in range(files):
            (self.source / f"Notes-{n:04}.txt").write_text(
                f"Disposable ordinary text fixture {n}.\n"
            )
        subprocess.run(
            [
                str(PYTHON),
                "-c",
                SETUP,
                str(REFERENCE / "src"),
                str(self.ref),
                str(self.source),
                str(self.output),
                mode,
            ],
            env=self.env,
            check=True,
            capture_output=True,
            text=True,
        )
        shutil.copyfile(self.ref, self.native)

    def run_cli(self, reference, args):
        command = (
            [str(PYTHON), "-c", ORACLE, str(REFERENCE / "src")]
            if reference
            else [str(BINARY)]
        )
        return subprocess.run(
            command + ["--db", str(self.ref if reference else self.native)] + args,
            env=self.env,
            capture_output=True,
            text=True,
            timeout=90,
        )

    def text(self, value):
        return value.replace(str(self.ref), "<DB>").replace(str(self.native), "<DB>")

    def normalize(self, value):
        # Only nondeterministic execution identifiers/timing. File IDs, paths,
        # revision digests, job state and all stage counts remain compared.
        if isinstance(value, dict):
            return {
                k: (
                    "<dynamic>"
                    if k
                    in (
                        "request_id",
                        "scan_id",
                        "started_at",
                        "finished_at",
                        "created_at",
                        "elapsed_seconds",
                        "duration_seconds",
                        "duration_ms",
                        "elapsed_ms",
                        "job_id",
                        "lease_token",
                        "lease_until",
                    )
                    and v is not None
                    else self.normalize(v)
                )
                for k, v in value.items()
            }
        if isinstance(value, list):
            return [self.normalize(v) for v in value]
        if isinstance(value, str):
            return self.text(value)
        return value

    def compare(self, args, semantic=False):
        oracle = self.run_cli(True, args)
        native = self.run_cli(False, args)
        self.assertEqual(
            native.returncode,
            oracle.returncode,
            (args, oracle.stdout, oracle.stderr, native.stdout, native.stderr),
        )
        self.assertEqual(native.stderr, oracle.stderr)
        if semantic:
            self.assertEqual(
                self.normalize(json.loads(native.stdout)),
                self.normalize(json.loads(oracle.stdout)),
            )
        else:
            self.assertEqual(self.text(native.stdout), self.text(oracle.stdout))
        return oracle, native

    def rows(self, path, sql):
        with closing(sqlite3.connect(path)) as db:
            return db.execute(sql).fetchall()

    def test_admission_and_preflight_exact_all_modes(self):
        self.setup("empty", files=0)
        for mode in ([], ["--json"], ["--machine"]):
            self.compare(mode + ["maintenance"])
            for flags in (
                ["--settle", "-1"],
                ["--batch", "0"],
                ["--workers", "17"],
                ["--limit", "0"],
                ["--rule-batch", "0"],
                ["--render-rules", "1001"],
                ["--render-rules", "1"],
                ["--rule-max-new-bytes", "0"],
                ["--rules", "--rule-max-new-bytes", "-1"],
                ["--max-removals", "-1"],
                ["--max-removal-percent", "101"],
                ["--inventory-only", "--manifest"],
                ["--exclude", "../bad"],
            ):
                self.compare(mode + ["maintenance"] + flags)
        self.assertEqual(
            self.rows(self.ref, "SELECT * FROM files"),
            self.rows(self.native, "SELECT * FROM files"),
        )

    def test_inventory_text_summary_and_exclusions(self):
        self.setup("summary", files=3)
        self.compare(
            [
                "maintenance",
                "--inventory-only",
                "--exclude",
                "Notes-0001.txt",
                "--limit",
                "1",
            ]
        )
        for sql in (
            "SELECT * FROM files ORDER BY id",
            "SELECT profile,file_id,size,mtime_ns,device,inode,status FROM observations ORDER BY file_id",
            "SELECT * FROM item_worklog ORDER BY item_id,revision",
        ):
            self.assertEqual(self.rows(self.native, sql), self.rows(self.ref, sql))
        self.compare(["maintenance", "--inventory-only", "--rules"])

    def scan_nonparity_witness(self, mode):
        self.setup()
        oracle = self.run_cli(True, [mode, "maintenance", "--inventory-only"])
        native = self.run_cli(False, [mode, "maintenance", "--inventory-only"])
        self.assertEqual((oracle.returncode, native.returncode), (0, 0))
        a, b = json.loads(oracle.stdout), json.loads(native.stdout)
        if mode == "--machine":
            a, b = a["data"], b["data"]
        a, b = a["stages"][1], b["stages"][1]
        assert_completed_report_facts(self, a, b)

    def test_known_scan_json_nonparity_witness(self):
        self.scan_nonparity_witness("--json")

    def test_known_scan_machine_nonparity_witness(self):
        self.scan_nonparity_witness("--machine")

    def test_maintenance_json_shell_preserves_forwarded_owner_result(self):
        self.setup()
        for mode in ("--json", "--machine"):
            oracle = self.run_cli(True, [mode, "maintenance", "--inventory-only"])
            native = self.run_cli(False, [mode, "maintenance", "--inventory-only"])
            self.assertEqual(native.returncode, oracle.returncode)
            self.assertEqual(native.stderr, oracle.stderr)
            a, b = json.loads(oracle.stdout), json.loads(native.stdout)
            self.assertEqual(list(a), list(b))
            if mode == "--machine":
                a, b = a["data"], b["data"]
            self.assertEqual(list(a), list(b))
            # The existing scan owner's full report has explicit nonparity
            # witnesses above. Only exclude that delegated detail in this shell
            # comparison; it must still be forwarded, never silently dropped.
            for report in (a, b):
                for stage in report["stages"]:
                    if stage["stage"] == "scan":
                        self.assertEqual(len(stage.pop("scans")), 1)
                        self.assertEqual(len(stage.pop("observations")), 1)
            self.assertEqual(self.normalize(a), self.normalize(b))

    def test_analysis_settling_and_excluded_old_observations(self):
        self.setup("seeded")
        self.compare(["maintenance", "--inventory-only", "--process", "sniff"])
        self.compare(
            [
                "maintenance",
                "--inventory-only",
                "--process",
                "sniff",
                "--settle",
                "0",
                "--exclude",
                "Notes-0000.txt",
            ]
        )
        self.assertEqual(
            self.rows(self.native, "SELECT operation,state FROM processing_jobs"),
            [("hash", "queued")],
        )
        self.assertEqual(
            self.rows(
                self.native, "SELECT profile,file_id,revision FROM watch_revisions"
            ),
            self.rows(self.ref, "SELECT profile,file_id,revision FROM watch_revisions"),
        )

    def test_analysis_execution_repeat_and_bounded_backlog(self):
        self.setup("seeded", files=3)
        for _ in range(3):
            self.compare(
                [
                    "maintenance",
                    "--inventory-only",
                    "--process",
                    "sniff",
                    "--settle",
                    "0",
                    "--batch",
                    "1",
                ]
            )
        self.assertEqual(
            self.rows(
                self.native, "SELECT state FROM processing_jobs WHERE operation='hash'"
            ),
            [("queued",)],
        )
        self.assertEqual(
            self.rows(
                self.native,
                "SELECT operation,state,count(*) FROM processing_jobs GROUP BY 1,2",
            ),
            self.rows(
                self.ref,
                "SELECT operation,state,count(*) FROM processing_jobs GROUP BY 1,2",
            ),
        )
        self.assertEqual(
            self.rows(
                self.native,
                "SELECT profile,file_id,revision FROM watch_revisions ORDER BY file_id",
            ),
            self.rows(
                self.ref,
                "SELECT profile,file_id,revision FROM watch_revisions ORDER BY file_id",
            ),
        )

    def test_enabled_analysis_rules_enqueue_only_in_bounded_batches(self):
        self.setup("rule", files=3)
        for _ in range(3):
            self.compare(
                ["maintenance", "--inventory-only", "--rules", "--rule-batch", "1"]
            )
        for sql in (
            "SELECT operation,state,count(*) FROM processing_jobs GROUP BY 1,2",
            "SELECT rule_id,file_id,item_id FROM rule_jobs ORDER BY rule_id,file_id,item_id",
        ):
            self.assertEqual(self.rows(self.native, sql), self.rows(self.ref, sql))
        self.assertEqual(
            self.rows(self.native, "SELECT count(*) FROM processing_attempts"), [(0,)]
        )
        self.assertEqual(list(self.output.iterdir()), [])

    def test_cached_analysis_failure_stops_outputs(self):
        self.setup("failed")
        self.compare(["maintenance", "--process", "sniff", "--settle", "0"])
        self.assertEqual(list(self.output.iterdir()), [])

    def test_output_and_rule_render_refusals_preserve_desired_state(self):
        self.setup()
        (self.output / "unowned.txt").write_text("must remain unchanged")
        before = {
            table: self.rows(self.native, f"SELECT * FROM {table}")
            for table in ("mappings", "journal", "owned_links")
        }
        desired_meta = self.rows(
            self.native,
            "SELECT * FROM meta WHERE key NOT LIKE 'scan:%' AND key NOT LIKE 'observation:%' ORDER BY key",
        )
        for flags, stage in (
            ([], "planning"),
            (["--manifest"], "planning"),
            (
                [
                    "--all-catalogs",
                    "--max-removals",
                    "100",
                    "--max-removal-percent",
                    "100",
                ],
                "planning",
            ),
            (["--inventory-only", "--rules", "--render-rules", "1"], "rules"),
        ):
            result = self.run_cli(False, ["--machine", "maintenance"] + flags)
            self.assertEqual(result.returncode, 3, result.stderr)
            report = json.loads(result.stdout)["data"]
            self.assertFalse(report["complete"])
            self.assertEqual(report["stopped_at"], stage)
            self.assertIn("not yet qualified", report["errors"][0]["message"])
            self.assertFalse(report["summary"]["outputs"]["checked"])
        for table, expected in before.items():
            self.assertEqual(
                self.rows(self.native, f"SELECT * FROM {table}"), expected, table
            )
        self.assertEqual(
            self.rows(
                self.native,
                "SELECT * FROM meta WHERE key NOT LIKE 'scan:%' AND key NOT LIKE 'observation:%' ORDER BY key",
            ),
            desired_meta,
        )
        self.assertEqual(
            {p.name: p.read_text() for p in self.output.iterdir()},
            {"unowned.txt": "must remain unchanged"},
        )
        self.assertFalse(Path(str(self.native) + ".publication").exists())

    def test_pending_journal_precedes_inventory(self):
        self.setup()
        for path in (self.ref, self.native):
            with closing(sqlite3.connect(path)) as db:
                db.execute(
                    "INSERT INTO journal(id,profile,catalog,path,kind) VALUES ('pending','default','global','','prepare')"
                )
                db.commit()
        self.compare(["--machine", "maintenance", "--inventory-only"])
        self.assertEqual(self.rows(self.native, "SELECT count(*) FROM scans"), [(0,)])


if __name__ == "__main__":
    if "--diagnose-full-scan-parity" in sys.argv:
        # Explicit failed diagnostic, deliberately outside required discovery.
        # This must not be counted as passed full-report qualification.
        mode = "--machine" if "--machine" in sys.argv else "--json"
        case = NativeMaintenanceCliTest()
        case.setUp()
        try:
            case.setup()
            oracle = case.run_cli(True, [mode, "maintenance", "--inventory-only"])
            native = case.run_cli(False, [mode, "maintenance", "--inventory-only"])
            destination = Path(os.environ["CATABOLIC_MAINTENANCE_DIAGNOSTIC_DIR"])
            destination.mkdir(parents=True, exist_ok=True)
            for label, result in (("python", oracle), ("native", native)):
                (destination / (mode[2:] + "-" + label + ".json")).write_text(
                    result.stdout
                )
                (destination / (mode[2:] + "-" + label + ".stderr")).write_text(
                    result.stderr
                )
            case.assertEqual(oracle.returncode, native.returncode)
            case.assertEqual(
                case.normalize(json.loads(oracle.stdout)),
                case.normalize(json.loads(native.stdout)),
            )
        finally:
            case.doCleanups()
    else:
        unittest.main()
