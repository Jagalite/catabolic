# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Rollback-only projection orchestration; independent frozen/native catalogs.

No filesystem publication, real media, external service, or credentials. Native
execute and fallback are refusal witnesses, never mandatory expected failures.
"""

import fcntl
import hashlib
import json
import os
import select
import shutil
import signal
import sqlite3
import subprocess
import tempfile
import time
import unittest
from contextlib import closing
from pathlib import Path

from tests.test_rust_legacy_review import BINARY, ORACLE, PYTHON, REFERENCE

# Observation-only connection instrumentation for the frozen signal fixture.
# It preserves the owner's progress callback and its return value. Readiness is
# emitted only from SQLite progress while the uniquely tagged selection runs,
# never from Store admission, migration validation or a wall-clock delay.
SIGNAL_READY_ORACLE = r"""
import os, sqlite3
_projection_ready_fd = int(os.environ.pop('CATABOLIC_PROJECTION_READY_FD'))
_projection_ready_sent = False
_projection_original_connect = sqlite3.connect
class _ProjectionObservedConnection(sqlite3.Connection):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self._projection_selection = False
        super().set_trace_callback(self._projection_trace)
    def _projection_trace(self, statement):
        self._projection_selection = (
            statement.lstrip().startswith('WITH RECURSIVE projection_run_signal_fixture(')
        )
    def set_progress_handler(self, callback, instructions):
        if callback is None:
            return super().set_progress_handler(None, instructions)
        def observed_progress():
            global _projection_ready_sent
            if self._projection_selection and not _projection_ready_sent:
                _projection_ready_sent = True
                os.write(_projection_ready_fd, b'selection-sqlite-progress\n')
                os.close(_projection_ready_fd)
            return callback()
        return super().set_progress_handler(observed_progress, instructions)
def _projection_connect(*args, **kwargs):
    if 'factory' in kwargs:
        raise RuntimeError('signal fixture expected the ordinary SQLite factory')
    return _projection_original_connect(*args, factory=_ProjectionObservedConnection, **kwargs)
sqlite3.connect = _projection_connect
"""

SEED = r"""
from catabolic.store import Store
from catabolic.app import Application
from catabolic.layouts import Layouts
from catabolic.saved_queries import Queries
from catabolic.projections import Projections
import json,sys
with Store(sys.argv[1],writable=True) as store:
    app=Application(store)
    app.bind('output','global',sys.argv[2])
    with store.transaction() as db:
        db.execute("INSERT INTO locations(id) VALUES ('offline')")
        for n in range(3):
            db.execute("INSERT INTO files(id,location,path) VALUES (?,'offline',?)",(f'f{n}',f'file{n}.txt'))
            db.execute("INSERT INTO items(id,kind,metadata) VALUES (?,'book','{}')",(f'i{n}',))
            db.execute("INSERT INTO item_files(id,file_id,item_id,role,origin) VALUES (?,?,?,'primary','explicit')",(f'a{n}',f'f{n}',f'i{n}'))
    app.bind('source','offline',sys.argv[3])
    with store.transaction() as db:
        db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors,finished_at) VALUES ('fixture','default','offline',1,0,'[]','2026-01-01 00:00:00')")
        db.execute("INSERT INTO observations(profile,file_id,size,mtime_ns,device,inode,status,scan_id) SELECT 'default',id,0,0,0,0,'missing','fixture' FROM files")
    layout=Layouts(app).put('flat',{'version':1,'rules':[{'name':'all','path':'{item.id}/{file.name}'}]})
    query=Queries(store).put('selected',{'mode':'selection','selection':{'language':'sql','query':'SELECT item_id FROM catalog_items'}})
    Projections(app).put('global',query['id'],'flat')
"""


def typed(value):
    if isinstance(value, dict):
        return {key: typed(child) for key, child in value.items()}
    if isinstance(value, list):
        return [typed(child) for child in value]
    return [type(value).__name__, value]


class NativeLegacyProjectionRunTest(unittest.TestCase):
    signal_outcomes = []

    def setUp(self):
        for path in (PYTHON, REFERENCE / "catabolic/projections.py", BINARY):
            self.assertTrue(path.is_file(), str(path))
        temporary = tempfile.TemporaryDirectory(prefix="projection-run-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.db = {True: self.root / "python.db", False: self.root / "native.db"}
        self.output = self.root / "output"
        self.output.mkdir()
        self.source = self.root / "source"
        self.source.mkdir()
        self.env = {
            k: v
            for k, v in os.environ.items()
            if not k.startswith("CATABOLIC_") and k not in ("PYTHONPATH", "PYTHONHOME")
        }
        self.env.update(
            HOME=str(self.root / "home"),
            COLUMNS="80",
            TERM="dumb",
            NO_COLOR="1",
            PYTHONUTF8="1",
            PYTHONHASHSEED="0",
        )
        initialized = self.run_cli(True, ["init"])
        self.assertEqual(initialized.returncode, 0, initialized.stderr)
        result = self.python(
            SEED, str(self.db[True]), str(self.output), str(self.source)
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        shutil.copyfile(self.db[True], self.db[False])

    def python(self, program, *args):
        return subprocess.run(
            [
                str(PYTHON),
                "-c",
                "import sys;sys.path.insert(0,sys.argv.pop(1));" + program,
                str(REFERENCE),
                *args,
            ],
            env=self.env,
            capture_output=True,
            text=True,
            timeout=30,
        )

    def command(self, oracle, args):
        command = (
            [str(PYTHON), "-c", ORACLE, str(REFERENCE)] if oracle else [str(BINARY)]
        )
        return command + ["--db", str(self.db[oracle])] + args

    def run_cli(self, oracle, args):
        return subprocess.run(
            self.command(oracle, args),
            env=self.env,
            capture_output=True,
            text=True,
            timeout=30,
        )

    def snapshot(self, oracle):
        with closing(sqlite3.connect(self.db[oracle])) as db:
            return list(db.iterdump())

    def effects(self):
        return {flag: self.snapshot(flag) for flag in (True, False)}, [
            (
                str(p.relative_to(self.output)),
                p.lstat().st_mode,
                hashlib.sha256(p.read_bytes()).hexdigest() if p.is_file() else "",
            )
            for p in self.output.rglob("*")
        ]

    def normalize(self, text):
        for path in self.db.values():
            text = text.replace(str(path), "<DB>")
        return text

    def pair(self, args, *, exact=False):
        before = self.effects()
        old, new = (self.run_cli(flag, args) for flag in (True, False))
        self.assertEqual(
            new.returncode,
            old.returncode,
            (old.stdout, old.stderr, new.stdout, new.stderr),
        )
        self.assertEqual(self.normalize(new.stderr), self.normalize(old.stderr))
        if (
            exact
            or not old.stdout
            or not any(mode in args for mode in ("--json", "--machine"))
        ):
            self.assertEqual(self.normalize(new.stdout), self.normalize(old.stdout))
        else:
            self.assertEqual(
                typed(json.loads(self.normalize(new.stdout))),
                typed(json.loads(self.normalize(old.stdout))),
            )
        self.assertEqual(self.normalize(new.stdout), self.normalize(old.stdout))
        self.assertEqual(before, self.effects())
        for flag in (True, False):
            with open(str(self.db[flag]) + ".lock", "a") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            self.assertFalse(Path(str(self.db[flag]) + ".publication.lock").exists())
        return old, new

    def mutate(self, sql):
        for flag in (True, False):
            with sqlite3.connect(self.db[flag]) as db:
                db.executescript(sql)

    def test_preview_full_and_truncated_reports_rollback_all_effects(self):
        for mode in ([], ["--json"], ["--machine"]):
            for limit in (1, 100):
                with self.subTest(mode=mode, limit=limit):
                    _, result = self.pair(
                        mode
                        + ["projection", "preview", "global", "--limit", str(limit)]
                    )
                    if not mode:
                        continue
                    report = json.loads(result.stdout)
                    if mode == ["--machine"]:
                        report = report["data"]
                    self.assertEqual(report["layout"]["desired_count"], 3)
                    self.assertEqual(
                        report["reconciliation"]["blocked_sources_count"], 3
                    )
                    self.assertFalse(report["safe"])
                    self.assertFalse(report["layout"]["applied"])
                    self.assertEqual(report["reconciliation"]["action_count"], 4)
                    self.assertEqual(
                        report["reconciliation"]["actions_truncated"], limit == 1
                    )
                    self.assertEqual(
                        len(report["reconciliation"]["blocked_sources"]), min(limit, 3)
                    )

    def test_empty_selection_guard_and_allowed_disable_both_rollback(self):
        program = r"""
from catabolic.store import Store
from catabolic.app import Application
from catabolic.layouts import Layouts
from catabolic.saved_queries import Queries
from catabolic.projections import Projections
with Store(sys.argv[1],writable=True) as store:
    app=Application(store)
    Layouts(app).run('flat','global',apply=True)
    query=Queries(store).put('empty',{'mode':'selection','selection':{'language':'sql','query':'SELECT item_id FROM catalog_items WHERE 0'}})
    Projections(app).put('global',query['id'],'flat')
"""
        result = self.python(program, str(self.db[True]))
        self.assertEqual(result.returncode, 0, result.stderr)
        shutil.copyfile(self.db[True], self.db[False])
        for mode in ([], ["--json"], ["--machine"]):
            for allow in ([], ["--allow-empty"]):
                _, result = self.pair(
                    mode + ["projection", "preview", "global", "--limit", "1"] + allow
                )
                if not mode:
                    continue
                report = json.loads(result.stdout)
                if mode == ["--machine"]:
                    report = report["data"]
                self.assertEqual(report["layout"]["change_count"], 3)
                self.assertEqual(report["layout"]["safe"], bool(allow))
                self.assertFalse(report["layout"]["applied"])
                self.assertEqual("reconciliation" in report, bool(allow))

    def test_safe_empty_preview_and_repeat_have_no_metadata_or_output_effects(self):
        self.mutate(
            "DELETE FROM observations; DELETE FROM item_files; DELETE FROM files; DELETE FROM items;"
        )
        for mode in ([], ["--json"], ["--machine"]):
            for _ in range(2):
                _, result = self.pair(mode + ["projection", "preview", "global"])
                if not mode:
                    continue
                report = json.loads(result.stdout)
                if mode == ["--machine"]:
                    report = report["data"]
                self.assertTrue(report["safe"])
                self.assertTrue(report["complete"])
                self.assertFalse(report["applied"])
                self.assertEqual(report["reconciliation"]["action_count"], 1)

    def test_admission_order_errors_and_post_staging_budget_rollback(self):
        for args in [
            ["--limit", "0"],
            ["--limit", "1001"],
            ["--max-removals", "-1"],
        ]:
            for mode in ([], ["--json"], ["--machine"]):
                with self.subTest(args=args, mode=mode):
                    self.pair(
                        mode + ["projection", "preview", "global"] + args, exact=True
                    )
        for mode in ([], ["--json"], ["--machine"]):
            self.pair(mode + ["projection", "preview", "missing"], exact=True)
            self.pair(
                mode
                + [
                    "--profile",
                    "missing",
                    "projection",
                    "preview",
                    "global",
                    "--limit",
                    "0",
                ],
                exact=True,
            )
        self.mutate("DELETE FROM projection_bindings;")
        self.pair(["--machine", "projection", "preview", "global"], exact=True)

    def test_unsafe_layout_short_circuits_reconciliation_and_rolls_back(self):
        program = r"""
from catabolic.store import Store
from catabolic.app import Application
from catabolic.layouts import Layouts
with Store(sys.argv[1],writable=True) as store:
    Layouts(Application(store)).put('flat',{'version':1,'rules':[{'name':'collision','path':'same.txt'}]})
"""
        for flag in (True, False):
            result = self.python(program, str(self.db[flag]))
            self.assertEqual(result.returncode, 0, result.stderr)
        for mode in ([], ["--json"], ["--machine"]):
            _, result = self.pair(
                mode
                + [
                    "projection",
                    "preview",
                    "global",
                    "--limit",
                    "1",
                    "--max-removals",
                    "-1",
                ]
            )
            if not mode:
                continue
            report = json.loads(result.stdout)
            if mode == ["--machine"]:
                report = report["data"]
            self.assertFalse(report["layout"]["safe"])
            self.assertFalse(report["complete"])
            self.assertNotIn("reconciliation", report)

    def test_catalog_writer_contention_and_release_exact_channels(self):
        for mode in ([], ["--json"], ["--machine"]):
            outputs = []
            before = self.effects()
            for flag in (True, False):
                with open(str(self.db[flag]) + ".lock", "a") as lock:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    result = self.run_cli(
                        flag, mode + ["projection", "preview", "global"]
                    )
                    outputs.append(
                        (
                            result.returncode,
                            self.normalize(result.stdout),
                            self.normalize(result.stderr),
                        )
                    )
            self.assertEqual(outputs[0], outputs[1])
            self.assertEqual(outputs[0][0], 2)
            self.assertEqual(before, self.effects())
        self.pair(["--machine", "projection", "preview", "global"])

    def test_execute_early_gate_is_explicit_nonparity_and_no_effects(self):
        # Frozen execute with an invalid limit proves the intentionally earlier
        # gate. No frozen publication is invoked by this fixture.
        for mode in ([], ["--json"], ["--machine"]):
            before = self.effects()
            old, new = (
                self.run_cli(
                    flag, mode + ["projection", "execute", "global", "--limit", "0"]
                )
                for flag in (True, False)
            )
            self.assertEqual(old.returncode, 2)
            self.assertEqual(new.returncode, 2)
            self.assertIn("limit must be between 1 and 1000", old.stdout + old.stderr)
            self.assertIn(
                "native projection execute is blocked", new.stdout + new.stderr
            )
            self.assertEqual(before, self.effects())
        absent = self.root / "absent.db"
        result = subprocess.run(
            [str(BINARY), "--db", str(absent), "projection", "execute", "global"],
            env=self.env,
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertIn("native projection execute is blocked", result.stderr)
        self.assertFalse(absent.exists())
        self.assertFalse(Path(str(absent) + ".lock").exists())

    def test_fallback_refuses_before_invalid_layout_or_output_planning(self):
        # Insert a fixture binding with its FK target, using the frozen binding
        # APIs would need unrelated policy planning. No fallback owner executes.
        with sqlite3.connect(self.db[False]) as db:
            columns = list(db.execute("PRAGMA table_info(fallback_bindings)"))
        self.assertTrue(columns)
        self.mutate(
            "INSERT INTO fallback_policies(id,profile,name,revision,definition,digest) VALUES ('fixture','default','fixture',1,'{}','fixture'); INSERT INTO fallback_bindings(profile,catalog,policy_id,query_id,layout) SELECT profile,catalog,'fixture',query_id,'missing-layout' FROM projection_bindings;"
        )
        before = self.effects()
        for mode in ([], ["--json"], ["--machine"]):
            result = self.run_cli(False, mode + ["projection", "preview", "global"])
            self.assertEqual(result.returncode, 2)
            self.assertIn(
                "native projection preview is blocked for fallback bindings",
                result.stdout + result.stderr,
            )
        self.assertEqual(before, self.effects())

    def test_interrupt_after_writer_admission_nonparity_and_rollback(self):
        # First check the readiness mechanism itself on an in-memory SQLite
        # connection: unrelated SQL cannot signal, and wrapping/clearing the
        # real callback preserves its ordinary return and execution behavior.
        reader, writer = os.pipe()
        os.set_blocking(reader, False)
        control = r"""
reader = int(os.environ.pop('CATABOLIC_PROJECTION_CONTROL_READER'))
db = sqlite3.connect(':memory:')
calls = 0
def progress():
    global calls
    calls += 1
    return 0
db.set_progress_handler(progress, 1)
db.execute('WITH RECURSIVE unrelated_sql(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM unrelated_sql WHERE x<2000) SELECT sum(x) FROM unrelated_sql').fetchone()
assert calls > 0
try:
    os.read(reader, 128)
except BlockingIOError:
    pass
else:
    raise AssertionError('unrelated SQL must not signal selection readiness')
before = calls
row = db.execute('WITH RECURSIVE projection_run_signal_fixture(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM projection_run_signal_fixture WHERE x<2000) SELECT sum(x) FROM projection_run_signal_fixture').fetchone()
assert row == (2001000,) and calls > before
assert os.read(reader, 128) == b'selection-sqlite-progress\n'
db.set_progress_handler(None, 0)
before = calls
db.execute('SELECT 1').fetchone()
assert calls == before
db.close()
"""
        try:
            result = subprocess.run(
                [str(PYTHON), "-c", SIGNAL_READY_ORACLE + control],
                env={
                    **self.env,
                    "CATABOLIC_PROJECTION_READY_FD": str(writer),
                    "CATABOLIC_PROJECTION_CONTROL_READER": str(reader),
                },
                pass_fds=(reader, writer),
                capture_output=True,
                text=True,
                timeout=10,
            )
            self.assertEqual(
                (result.returncode, result.stdout, result.stderr), (0, "", "")
            )
        finally:
            os.close(reader)
            os.close(writer)
        slow = r"""
from catabolic.store import Store
from catabolic.saved_queries import Queries
from catabolic.app import Application
from catabolic.projections import Projections
with Store(sys.argv[1],writable=True) as store:
    q=Queries(store).put('slow',{'mode':'selection','selection':{'language':'sql','query':'WITH RECURSIVE projection_run_signal_fixture(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM projection_run_signal_fixture WHERE x<100000000) SELECT item_id FROM catalog_items WHERE (SELECT sum(x) FROM projection_run_signal_fixture)>0'}})
    Projections(Application(store)).put('global',q['id'],'flat')
"""
        for flag in (True, False):
            result = self.python(slow, str(self.db[flag]))
            self.assertEqual(result.returncode, 0, result.stderr)
        for mode in ([], ["--json"], ["--machine"]):
            outputs = []
            before = self.effects()
            for flag in (True, False):
                command = self.command(flag, mode + ["projection", "preview", "global"])
                environment = self.env.copy()
                reader = writer = None
                if flag:
                    reader, writer = os.pipe()
                    command[2] = SIGNAL_READY_ORACLE + ORACLE
                    environment["CATABOLIC_PROJECTION_READY_FD"] = str(writer)
                process = subprocess.Popen(
                    command,
                    env=environment,
                    pass_fds=(writer,) if flag else (),
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                )
                if writer is not None:
                    os.close(writer)
                try:
                    deadline = time.monotonic() + 10
                    admitted = False
                    while time.monotonic() < deadline and process.poll() is None:
                        with open(str(self.db[flag]) + ".lock", "a") as lock:
                            try:
                                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                            except BlockingIOError:
                                admitted = True
                                break
                        time.sleep(0.01)
                    self.assertTrue(admitted, "projection writer not observed")
                    if flag:
                        ready, _, _ = select.select([reader], [], [], 10)
                        self.assertTrue(
                            ready, "frozen selection progress was not observed"
                        )
                        marker = os.read(reader, 128)
                        self.assertEqual(marker, b"selection-sqlite-progress\n")
                        self.assertIsNone(
                            process.poll(), "frozen selection exited before SIGINT"
                        )
                    else:
                        # The unmodified native executable exposes no test trace
                        # hook. This branch proves writer admission only; its
                        # Rust tests separately synchronize actual staged writes.
                        time.sleep(0.1)
                    process.send_signal(signal.SIGINT)
                    stdout, stderr = process.communicate(timeout=10)
                    outputs.append(
                        (
                            process.returncode,
                            self.normalize(stdout),
                            self.normalize(stderr),
                        )
                    )
                finally:
                    if reader is not None:
                        os.close(reader)
                    if process.poll() is None:
                        process.kill()
                        process.communicate(timeout=5)
            old, new = outputs
            self.assertEqual(new[0], 130)
            self.assertIn(old[0], (2, 130))
            if old[0] == 130:
                self.assertEqual(old, new)
            else:
                # A signal reaching the frozen SQL owner can be caught there
                # as CatabolicError. This schedule-dependent difference is
                # recorded explicitly; it is not interruption exit parity.
                if mode == ["--machine"]:
                    self.assertEqual(old[2], "")
                    envelope = json.loads(old[1])
                    self.assertEqual(envelope["exit_code"], 2)
                    self.assertEqual(
                        envelope["data"]["error"],
                        {
                            "message": "SQL query failed: interrupted",
                            "type": "CatabolicError",
                        },
                    )
                elif mode == ["--json"]:
                    self.assertEqual(old[1], "")
                    self.assertEqual(
                        old[2],
                        '{"error": {"message": "SQL query failed: interrupted", "type": "CatabolicError"}}\n',
                    )
                else:
                    self.assertEqual(
                        old[1:], ("", "error: SQL query failed: interrupted\n")
                    )
                self.assertIn("interrupted", new[1] + new[2])
            self.signal_outcomes.append(
                {
                    "mode": mode,
                    "oracle": old,
                    "native": new,
                    "oracle_readiness": "first SQLite progress callback for the uniquely tagged selection query",
                    "native_readiness": "writer lock observed, then 0.1 seconds; exact SQL phase is not established",
                    "readiness_control": "untagged SQL cannot signal; tagged progress signals and preserves callback behavior",
                    "signal_oracle_program_sha256": hashlib.sha256(
                        (SIGNAL_READY_ORACLE + ORACLE).encode()
                    ).hexdigest(),
                }
            )
            self.assertEqual(before, self.effects())
            for flag in (True, False):
                with open(str(self.db[flag]) + ".lock", "a") as lock:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)


if __name__ == "__main__":
    unittest.main()
