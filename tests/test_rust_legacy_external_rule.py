# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Ordinary external-definition and rule-worker frozen/native differentials.

Only disposable, copied catalogs and numeric-loopback HTTP are used. Tiny source
files are fixtures for the existing request owner, not source/filesystem review.
No response can publish a receipt: physical publication and M6 stay out of scope.
All tables and typed values are compared. Only individually recorded generated
operation IDs, lease tokens and wall-clock fields receive narrow normalization.
"""

import fcntl
import json
import os
import shutil
import signal
import sqlite3
import subprocess
import tempfile
import threading
import time
import unittest
from contextlib import closing
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from uuid import UUID

from tests import process_tree
from tests.test_rust_legacy_review import ORACLE, PYTHON, REFERENCE

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(
    os.environ.get(
        "CATABOLIC_LEGACY_BINARY", ROOT / "target/debug/catabolic-legacy-native"
    )
).resolve()
STAMP = "2026-01-01 00:00:00"
JOB_IDS = [f"{number:032x}" for number in (10, 11, 12, 0)]
MUTABLE_WORKER_TABLES = {"processor_jobs", "processor_attempts", "fallback_epoch"}

# Prepare one catalog with fixed input/job IDs, then copy it. No generated
# destination, receipt, output, watcher, or publication work is created.
SEED = r"""
import hashlib,json,pathlib,sys
from catabolic.app import Application
from catabolic.operations import Operations
from catabolic.outputs import Outputs
from catabolic.processors import Processors
from catabolic.rules import Rules
from catabolic.store import Store,encode
path,source,endpoint=sys.argv[1:]
with Store(path,writable=True) as store:
    app=Application(store)
    app.bind('source','fixture',source)
    app.add_profile('other')
    with store.transaction() as db:
        db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors,finished_at) VALUES ('seed','default','fixture',1,4,'[]','2026-01-01 00:00:00')")
        for n in range(4):
            file=pathlib.Path(source)/f'{n}.txt'
            stat=file.stat()
            db.execute("INSERT INTO files(id,location,path) VALUES (?,'fixture',?)",(f'file-{n}',file.name))
            db.execute("INSERT INTO observations(profile,file_id,size,mtime_ns,device,inode,status,scan_id) VALUES ('default',?,?,?,?,?,'present','seed')",(f'file-{n}',stat.st_size,stat.st_mtime_ns,stat.st_dev,stat.st_ino))
            db.execute("INSERT INTO items(id,kind,metadata) VALUES (?,'book',?)",(f'item-{n}',encode({'title':f'Fixture {n}'})))
            db.execute("INSERT INTO item_files(id,file_id,item_id,role,origin) VALUES (?,?,?,'primary','explicit')",(f'association-{n}',f'file-{n}',f'item-{n}'))
    processors=Processors(app)
    processors.put('fixture',{'endpoint':endpoint,'capacity':4,'timeout':10})
    Processors(Application(store,'other')).put('fixture',{'endpoint':endpoint,'capacity':4,'timeout':10})
    output=Outputs(app).define('fixture-output',{'purpose':'custom'})
    options={'true':True,'one':1,'one_float':1.0,'null':None,'tiny':1e-7,'text':'Café 雪','created_at':'user-owned','lease_until':0}
    operation=Operations(app).put('fixture-operation',{'kind':'external','operation':'fixture','options':options,'output_definition_id':output['id']})
    analysis=Operations(app).put('analysis-operation',{'kind':'analysis','operation':'hash'})
    selection={'language':'sql','query':"SELECT file_id FROM catalog_files WHERE file_id IN ('file-0','file-1','file-2') ORDER BY file_id"}
    rules=Rules(app)
    rule=rules.put('fixture-rule',operation['id'],None,selection)
    empty=rules.put('empty-rule',operation['id'],None,{'language':'sql','query':'SELECT file_id FROM catalog_files WHERE 0'})
    nonexternal=rules.put('analysis-rule',analysis['id'],None,selection)
    for n,number in enumerate((10,11,12,0)):
        request,digest=processors.prepare('fixture',f'file-{n}',f'item-{n}',output['id'],options,operation_id=operation['id'])
        identifier=f'{number:032x}'
        with store.transaction() as db:
            db.execute("INSERT INTO processor_jobs(id,profile,processor,request,digest,operation_id,created_at,updated_at) VALUES (?,'default','fixture',?,?,?,'2026-01-01 00:00:00','2026-01-01 00:00:00')",(identifier,encode(request),digest,operation['id']))
            if n<3:
                db.execute("INSERT INTO rule_processor_jobs(rule_id,job_id,file_id,item_id,created_at) VALUES (?,?,?,?,'2026-01-01 00:00:00')",(rule['id'],identifier,f'file-{n}',f'item-{n}'))
    with store.transaction() as db:
        db.execute("INSERT INTO processor_jobs(id,profile,processor,request,digest,state,generation,worker,lease_token,lease_until,created_at,updated_at) VALUES ('other-profile-job','other','fixture','{}','other-digest','leased',1,'other-worker',?,4102444800,'2026-01-01 00:00:00','2026-01-01 00:00:00')",('f'*64,))
        db.execute("UPDATE processing_recipes SET created_at='2026-01-01 00:00:00'")
        db.execute("UPDATE processing_rules SET created_at='2026-01-01 00:00:00'")
    print(json.dumps({'rule':rule['id'],'empty':empty['id'],'analysis':nonexternal['id'],'operation':operation['id'],'output':output['id']}))
"""


def typed(value):
    """Unlike ordinary ==, distinguish bool/int/float and signed floating zero."""
    if isinstance(value, dict):
        return {key: typed(child) for key, child in value.items()}
    if isinstance(value, list):
        return [typed(child) for child in value]
    if isinstance(value, tuple):
        return ("tuple", tuple(typed(child) for child in value))
    return (type(value).__name__, value.hex() if type(value) is float else value)


class Gate:
    def __init__(self, result=None):
        self.entered = threading.Event()
        self.release = threading.Event()
        self.result = {"state": "running"} if result is None else result


class LoopbackProcessor:
    def __init__(self):
        self.responses = []
        self.requests = []
        self.errors = []
        self.gates = []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_PUT(self):
                self.respond()

            def do_GET(self):
                self.respond()

            def respond(self):
                try:
                    length = int(self.headers.get("Content-Length", "0"))
                    raw = self.rfile.read(length)
                    body = json.loads(raw) if raw else None
                    owner.requests.append((self.command, self.path, body))
                    response = (
                        owner.responses.pop(0)
                        if owner.responses
                        else {"state": "running"}
                    )
                    if isinstance(response, Gate):
                        response.entered.set()
                        if not response.release.wait(20):
                            raise AssertionError("loopback response was never released")
                        response = response.result
                    data = (
                        response
                        if isinstance(response, bytes)
                        else json.dumps(response).encode()
                    )
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                except (BrokenPipeError, ConnectionResetError):
                    # Expected when the supervised worker is interrupted.
                    pass
                except BaseException as exc:
                    owner.errors.append(repr(exc))

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.endpoint = f"http://127.0.0.1:{self.server.server_port}"

    def reset(self, responses):
        self.responses = list(responses)
        self.requests = []
        self.gates.extend(r for r in self.responses if isinstance(r, Gate))

    def close(self):
        for gate in self.gates:
            gate.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(5)


class NativeLegacyExternalRuleTest(unittest.TestCase):
    def setUp(self):
        for path in (PYTHON, REFERENCE / "catabolic/rule_cli.py", BINARY):
            self.assertTrue(path.is_file(), str(path))
        temporary = tempfile.TemporaryDirectory(prefix="external-rule-ordinary-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.source = self.root / "source"
        self.source.mkdir()
        for n in range(4):
            (self.source / f"{n}.txt").write_text(f"Disposable external fixture {n}\n")
        self.source_before = {p.name: p.read_bytes() for p in self.source.iterdir()}
        self.db = {True: self.root / "frozen.db", False: self.root / "native.db"}
        self.env = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith("CATABOLIC_")
            and key not in ("PYTHONPATH", "PYTHONHOME")
            and not key.lower().endswith("_proxy")
        }
        self.env.update(
            HOME=str(self.root / "home"),
            COLUMNS="80",
            NO_COLOR="1",
            TERM="dumb",
            PYTHONUTF8="1",
            PYTHONHASHSEED="0",
            NO_PROXY="127.0.0.1",
            no_proxy="127.0.0.1",
        )
        self.server = LoopbackProcessor()
        self.addCleanup(self.server.close)
        result = self.run_cli(True, ["init"])
        self.assertEqual(result.returncode, 0, result.stderr)
        result = self.python(
            SEED, str(self.db[True]), str(self.source), self.server.endpoint
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.ids = json.loads(result.stdout)
        shutil.copyfile(self.db[True], self.db[False])
        self.seed_state = self.snapshot(True)
        self.recorded_volatile = {True: {}, False: {}}
        self.started = int(time.time())
        self.addCleanup(self.assert_fixture_clean)

    def oracle_env(self):
        # The frozen HTTP child uses -m; force it to use the same frozen tree.
        return {**self.env, "PYTHONPATH": str(REFERENCE)}

    def python(self, program, *args):
        return subprocess.run(
            [
                str(PYTHON),
                "-c",
                "import sys;sys.path.insert(0,sys.argv.pop(1));" + program,
                str(REFERENCE),
                *args,
            ],
            env=self.oracle_env(),
            text=True,
            capture_output=True,
            timeout=30,
        )

    def command(self, oracle, args):
        prefix = (
            [str(PYTHON), "-c", ORACLE, str(REFERENCE)] if oracle else [str(BINARY)]
        )
        return prefix + ["--db", str(self.db[oracle])] + args

    def run_cli(self, oracle, args):
        return subprocess.run(
            self.command(oracle, args),
            text=True,
            capture_output=True,
            env=self.oracle_env() if oracle else self.env,
            timeout=30,
        )

    def snapshot(self, oracle):
        with closing(sqlite3.connect(self.db[oracle])) as db:
            db.row_factory = sqlite3.Row
            tables = [
                r[0]
                for r in db.execute(
                    "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"
                )
            ]
            return {
                table: [
                    dict(row)
                    for row in db.execute(
                        'SELECT * FROM "' + table + '" ORDER BY rowid'
                    )
                ]
                for table in tables
            }

    def mutate(self, sql, params=()):
        for oracle in (True, False):
            with closing(sqlite3.connect(self.db[oracle])) as db:
                db.execute(sql, params)
                db.commit()

    def run_args(
        self, *, rule="rule", batch=1, limit=100, worker="worker-雪", mode="--json"
    ):
        return [
            mode,
            "rule",
            "run",
            self.ids[rule],
            "--worker",
            worker,
            "--batch",
            str(batch),
            "--limit",
            str(limit),
        ]

    def normalize_text(self, value):
        for path in self.db.values():
            value = value.replace(str(path), "<DB>")
        return value

    def record(self, oracle, table, row, column, category):
        value = row[column]
        if value is None:
            return
        identity = row.get("id", (row.get("job_id"), row.get("generation")))
        if category == "operation-id":
            self.assertIs(type(value), str)
            self.assertEqual(str(UUID(value)), value)
        elif category == "token":
            self.assertIs(type(value), str)
            self.assertRegex(value, r"^[0-9a-f]{64}$")
        elif category == "timestamp":
            self.assertIs(type(value), str)
            epoch = int(
                datetime.strptime(value, "%Y-%m-%d %H:%M:%S")
                .replace(tzinfo=timezone.utc)
                .timestamp()
            )
            self.assertLessEqual(self.started - 2, epoch)
            self.assertLessEqual(epoch, int(time.time()) + 2)
        else:
            self.assertIs(type(value), int)
            offset = 300 if column == "lease_until" else 0
            self.assertLessEqual(self.started - 2 + offset, value)
            self.assertLessEqual(value, int(time.time()) + 2 + offset)
        self.recorded_volatile[oracle][(table, identity, column)] = (value, category)

    def collect_volatile(self, oracle, state):
        seed_operations = {row["id"] for row in self.seed_state["processing_recipes"]}
        for row in state["processing_recipes"]:
            if row["id"] not in seed_operations:
                self.record(oracle, "processing_recipes", row, "id", "operation-id")
                self.record(
                    oracle, "processing_recipes", row, "created_at", "timestamp"
                )
        for row in state["processor_jobs"]:
            if row["id"] not in JOB_IDS:
                continue
            if row["lease_token"] is not None and row["lease_token"] != "e" * 64:
                self.record(oracle, "processor_jobs", row, "lease_token", "token")
            if row["updated_at"] != STAMP:
                self.record(oracle, "processor_jobs", row, "updated_at", "timestamp")
            if row["lease_until"] not in (None, 0, 4102444800):
                self.record(oracle, "processor_jobs", row, "lease_until", "epoch")
        for row in state["processor_attempts"]:
            for column in ("started_at", "finished_at"):
                if row[column] not in (None, 0):
                    self.record(oracle, "processor_attempts", row, column, "epoch")

    def normalized_row(self, oracle, table, row):
        identity = row.get("id", (row.get("job_id"), row.get("generation")))
        result = typed(row)
        for column in row:
            recorded = self.recorded_volatile[oracle].get((table, identity, column))
            if recorded is not None and row[column] == recorded[0]:
                category = recorded[1]
                marker = (
                    ("operation", row["name"], row["revision"])
                    if category == "operation-id"
                    else category
                )
                result[column] = (type(row[column]).__name__, marker)
        return result

    def normalized_output(self, oracle, value):
        if isinstance(value, dict):
            # Recognize owner rows, never normalize same-named option fields.
            if {"id", "request", "generation", "lease_until"} <= value.keys():
                return self.normalized_row(oracle, "processor_jobs", value)
            if {"id", "name", "revision", "digest", "operation_kind"} <= value.keys():
                return self.normalized_row(oracle, "processing_recipes", value)
            return {
                key: self.normalized_output(oracle, child)
                for key, child in value.items()
            }
        if isinstance(value, list):
            return [self.normalized_output(oracle, child) for child in value]
        return typed(value)

    def assert_states(self, before, *, allowed):
        after = {flag: self.snapshot(flag) for flag in (True, False)}
        self.assertEqual(set(after[True]), set(after[False]))
        for flag in (True, False):
            self.collect_volatile(flag, after[flag])
            for table in after[flag]:
                if table not in allowed:
                    self.assertEqual(
                        typed(after[flag][table]),
                        typed(before[flag][table]),
                        (flag, table),
                    )
            with closing(sqlite3.connect(self.db[flag])) as db:
                self.assertEqual(db.execute("PRAGMA foreign_key_check").fetchall(), [])
                self.assertEqual(
                    db.execute("PRAGMA integrity_check").fetchall(), [("ok",)]
                )
        for table in after[True]:
            self.assertEqual(
                [self.normalized_row(False, table, row) for row in after[False][table]],
                [self.normalized_row(True, table, row) for row in after[True][table]],
                table,
            )
        return after

    def assert_results(self, results):
        old, new = results[True], results[False]
        self.assertEqual(new.returncode, old.returncode, (old, new))
        for attribute in ("stdout", "stderr"):
            first, second = (
                self.normalize_text(getattr(results[flag], attribute))
                for flag in (True, False)
            )
            try:
                expected, actual = json.loads(first), json.loads(second)
            except (ValueError, TypeError):
                self.assertEqual(second, first)
            else:
                self.assertEqual(
                    self.normalized_output(False, actual),
                    self.normalized_output(True, expected),
                )

    def pair(self, args, *, responses=(), allowed=MUTABLE_WORKER_TABLES):
        before = {flag: self.snapshot(flag) for flag in (True, False)}
        results, requests = {}, {}
        for flag in (True, False):
            self.server.reset(responses)
            results[flag] = self.run_cli(flag, args)
            requests[flag] = list(self.server.requests)
        self.assert_states(before, allowed=allowed)
        self.assert_results(results)
        self.assertEqual(typed(requests[False]), typed(requests[True]))
        self.assertEqual(self.server.errors, [])
        return results, requests[False]

    def report(self, result):
        value = json.loads(result.stdout)
        return (
            value["data"] if "data" in value and "interface_version" in value else value
        )

    def assert_fixture_clean(self):
        self.assertEqual(
            {p.name: p.read_bytes() for p in self.source.iterdir()}, self.source_before
        )
        self.assertEqual(self.server.errors, [])
        for flag in (True, False):
            state = self.snapshot(flag)
            for table in (
                "external_receipts",
                "receipt_outputs",
                "media_outputs",
                "processing_artifacts",
                "journal",
            ):
                self.assertEqual(state[table], [], table)
            with open(str(self.db[flag]) + ".lock", "a") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def put_definition(self, value, *, name="new-external", allowed=None):
        path = self.root / "operation.json"
        path.write_text(json.dumps(value, ensure_ascii=False))
        return self.pair(
            ["--json", "operation", "put", name, "--definition", str(path)],
            allowed={"processing_recipes", "fallback_epoch"}
            if allowed is None
            else allowed,
        )

    def external_definition(self, **overrides):
        return {
            "kind": "external",
            "operation": "fixture",
            "output_definition_id": self.ids["output"],
            **overrides,
        }

    def test_external_definition_revisions_idempotency_and_typed_options(self):
        options = {
            "bool": True,
            "integer": 1,
            "float": 1.0,
            "negative_zero": -0.0,
            "tiny": 1e-7,
            "wide": 1099511627776,
            "none": None,
            "unicode": "雪 Café",
            "created_at": STAMP,
            "lease_until": 0,
        }
        first, requests = self.put_definition(self.external_definition(options=options))
        self.assertEqual(requests, [])
        for flag in (True, False):
            self.assertEqual(first[flag].returncode, 0, first[flag].stderr)
            self.assertEqual(
                typed(json.loads(first[flag].stdout)["definition"]["options"]),
                typed(options),
            )
        repeated, _ = self.put_definition(
            self.external_definition(options=options), allowed=set()
        )
        revised, _ = self.put_definition(
            self.external_definition(options={**options, "integer": 2})
        )
        for flag in (True, False):
            self.assertEqual(
                json.loads(first[flag].stdout)["id"],
                json.loads(repeated[flag].stdout)["id"],
            )
            self.assertEqual(json.loads(revised[flag].stdout)["revision"], 2)
            rows = [
                r
                for r in self.snapshot(flag)["processing_recipes"]
                if r["name"] == "new-external"
            ]
            self.assertEqual(len(rows), 2)
            self.assertEqual(
                typed(json.loads(rows[0]["definition"])["options"]), typed(options)
            )

    def test_external_definition_validation_has_no_catalog_effects(self):
        cases = [
            None,
            [],
            {},
            self.external_definition(version=True),
            self.external_definition(version=1.0),
            self.external_definition(extra=1),
            self.external_definition(operation=1),
            self.external_definition(options=[]),
            self.external_definition(operation="absent"),
            self.external_definition(output_definition_id="absent"),
            {"kind": "external", "operation": "fixture"},
            self.external_definition(options={"text": "é" * 40000}),
        ]
        for value in cases:
            with self.subTest(value=str(value)[:160]):
                results, requests = self.put_definition(value, allowed=set())
                self.assertEqual(results[False].returncode, 2)
                self.assertEqual(requests, [])

    def test_external_configuration_utf8_byte_boundary(self):
        options = {"x": "é" * 32763 + "a"}
        self.assertEqual(
            len(json.dumps(options, sort_keys=True, ensure_ascii=False).encode()), 65536
        )
        results, requests = self.put_definition(
            self.external_definition(options=options)
        )
        self.assertEqual(results[False].returncode, 0, results[False].stderr)
        self.assertEqual(requests, [])
        results, _ = self.put_definition(
            self.external_definition(options={"x": options["x"] + "a"}), allowed=set()
        )
        self.assertEqual(results[False].returncode, 2)
        self.assertIn(
            "external configuration must be an object bounded to 64 KiB",
            results[False].stderr,
        )

    def test_empty_selection_is_complete_without_claiming_or_writes(self):
        for mode in ("--json", "--machine"):
            results, requests = self.pair(
                self.run_args(rule="empty", mode=mode), allowed=set()
            )
            report = self.report(results[False])
            self.assertTrue(report["complete"])
            self.assertEqual(report["execution"]["steps"], [])
            self.assertEqual(requests, [])

    def test_matching_jobs_must_be_registered_with_this_rule(self):
        self.mutate("DELETE FROM rule_processor_jobs")
        results, requests = self.pair(self.run_args(batch=3), allowed=set())
        report = self.report(results[False])
        self.assertEqual(report["after"]["counts"]["queued"], 3)
        self.assertEqual(report["execution"]["steps"], [])
        self.assertFalse(report["complete"])
        self.assertEqual(requests, [])

    def test_batch_and_details_are_independent_and_unselected_jobs_untouched(self):
        results, requests = self.pair(
            self.run_args(batch=2, limit=1),
            responses=[{"state": "queued"}, {"state": "running"}],
        )
        report = self.report(results[False])
        self.assertEqual(len(requests), 2)
        self.assertEqual(
            [r[1] for r in requests], [f"/jobs/{i}/1" for i in JOB_IDS[:2]]
        )
        self.assertEqual(report["after"]["matched_inputs"], 3)
        self.assertEqual(len(report["after"]["matches"]), 1)
        self.assertTrue(report["after"]["details_truncated"])
        self.assertEqual(report["after"]["counts"]["running"], 2)
        self.assertEqual(report["after"]["counts"]["queued"], 1)
        self.assertTrue(report["execution"]["complete"])
        self.assertFalse(report["complete"])
        for flag in (True, False):
            jobs = {row["id"]: row for row in self.snapshot(flag)["processor_jobs"]}
            original = {row["id"]: row for row in self.seed_state["processor_jobs"]}
            for identifier in (JOB_IDS[2], JOB_IDS[3], "other-profile-job"):
                self.assertEqual(typed(jobs[identifier]), typed(original[identifier]))

    @staticmethod
    def after_lease_second():
        """Wait for a strictly later wall-clock second than any lease written
        so far. Both engines store lease expiries in whole seconds, and a
        same-worker renewal changes the row (bumping fallback_epoch) only if it
        lands in a later second. This makes that change deterministic for
        both engines, without loosening any state comparison."""
        claimed = int(time.time())
        while int(time.time()) <= claimed:
            time.sleep(0.01)

    def test_pending_same_worker_polls_once_without_new_generation(self):
        self.pair(self.run_args(), responses=[{"state": "running"}])
        self.after_lease_second()
        results, requests = self.pair(self.run_args(), responses=[{"state": "queued"}])
        self.assertEqual(requests, [("GET", f"/jobs/{JOB_IDS[0]}/1", None)])
        result = self.report(results[False])["execution"]["steps"][0]["result"]
        self.assertEqual(result["generation"], 1)
        self.assertEqual(result["state"], "submitted")
        self.assertNotIn("lease_token", result)
        self.assertEqual(len(self.snapshot(False)["processor_attempts"]), 1)

    def test_partial_batch_stops_at_first_dispatch_error_and_preserves_evidence(self):
        results, requests = self.pair(
            self.run_args(batch=3), responses=[{"state": "running"}, b'{"state":']
        )
        report = self.report(results[False])
        self.assertEqual(len(requests), 2)
        self.assertFalse(report["execution"]["complete"])
        self.assertFalse(report["complete"])
        self.assertEqual(
            report["execution"]["errors"],
            [{"job_id": JOB_IDS[1], "error": "processor returned invalid JSON"}],
        )
        jobs = {row["id"]: row for row in self.snapshot(False)["processor_jobs"]}
        self.assertEqual(
            [jobs[i]["state"] for i in JOB_IDS[:3]], ["submitted", "leased", "queued"]
        )
        self.assertEqual(len(self.snapshot(False)["processor_attempts"]), 2)

    def test_failed_response_is_a_durable_step_and_does_not_stop_later_jobs(self):
        results, requests = self.pair(
            self.run_args(batch=2),
            responses=[{"state": "failed"}, {"state": "running"}],
        )
        report = self.report(results[False])
        self.assertEqual(len(requests), 2)
        self.assertEqual(report["execution"]["errors"], [])
        self.assertEqual(report["execution"]["completed"], [])
        self.assertEqual(report["after"]["counts"]["failed"], 1)
        self.assertTrue(report["execution"]["complete"])
        self.assertFalse(report["complete"])

    def test_capacity_reports_no_claim_and_never_steals_unrelated_lease(self):
        self.mutate(
            "UPDATE processors SET definition=json_set(definition,'$.capacity',1) WHERE profile='default'"
        )
        self.mutate(
            "UPDATE processor_jobs SET state='leased',generation=1,worker='other-worker',lease_token=?,lease_until=4102444800 WHERE id=?",
            ("e" * 64, JOB_IDS[3]),
        )
        results, requests = self.pair(self.run_args(batch=3), allowed=set())
        self.assertEqual(requests, [])
        report = self.report(results[False])
        self.assertEqual(
            report["execution"]["steps"],
            [{"claimed": False, "reason": "processor capacity is leased"}],
        )
        self.assertFalse(report["complete"])

    def test_expired_registered_lease_is_reclaimed_and_old_attempt_retained(self):
        self.mutate(
            "UPDATE processor_jobs SET state='submitted',generation=1,worker='expired',lease_token=?,lease_until=0 WHERE id=?",
            ("e" * 64, JOB_IDS[0]),
        )
        self.mutate(
            "INSERT INTO processor_attempts VALUES (?,1,'expired','submitted',0,NULL)",
            (JOB_IDS[0],),
        )
        _, requests = self.pair(self.run_args(), responses=[{"state": "running"}])
        self.assertEqual(requests[0][:2], ("PUT", f"/jobs/{JOB_IDS[0]}/2"))
        attempts = self.snapshot(False)["processor_attempts"]
        self.assertEqual(
            [(r["generation"], r["state"]) for r in attempts],
            [(1, "expired"), (2, "submitted")],
        )
        self.assertIs(type(attempts[0]["finished_at"]), int)

    def test_worker_admission_errors_never_dispatch_or_write(self):
        cases = [
            self.run_args(batch=0),
            self.run_args(batch=1001),
            self.run_args(limit=0),
            self.run_args(limit=1001),
            self.run_args(worker="x" * 256),
            self.run_args(rule="analysis"),
        ]
        cases.append(["--json", "rule", "run", "absent-rule", "--worker", "fixture"])
        for args in cases:
            with self.subTest(args=args):
                results, requests = self.pair(args, allowed=set())
                self.assertEqual(results[False].returncode, 2)
                self.assertEqual(requests, [])

    def test_empty_worker_uses_normal_pending_report_without_network(self):
        results, requests = self.pair(self.run_args(worker="", batch=3), allowed=set())
        report = self.report(results[False])
        self.assertEqual(requests, [])
        self.assertEqual(report["execution"]["pending_external"], 3)
        self.assertEqual(
            report["execution"]["executor"],
            "processor tick or leased processor dispatch",
        )
        self.assertNotIn("steps", report["execution"])
        self.assertFalse(report["complete"])

    def test_missing_credential_is_durable_dispatch_error_without_network(self):
        self.mutate(
            "UPDATE processors SET definition=json_set(definition,'$.credential_env','EXTERNAL_RULE_MISSING_CREDENTIAL') WHERE profile='default'"
        )
        self.env.pop("EXTERNAL_RULE_MISSING_CREDENTIAL", None)
        results, requests = self.pair(self.run_args(batch=3))
        self.assertEqual(requests, [])
        report = self.report(results[False])
        self.assertEqual(
            report["execution"]["errors"],
            [
                {
                    "job_id": JOB_IDS[0],
                    "error": "set credential environment variable EXTERNAL_RULE_MISSING_CREDENTIAL",
                }
            ],
        )
        self.assertEqual(len(self.snapshot(False)["processor_attempts"]), 1)

    def test_invalid_state_and_receipt_refuse_before_any_publication(self):
        for response, message in (
            (["running"], "processor returned an invalid job state"),
            ({"state": True}, "processor returned an invalid job state"),
            (
                {"state": "complete", "receipt": {}},
                "processor receipt does not match the leased request",
            ),
        ):
            with self.subTest(response=response):
                # Each subcase re-polls the job the previous one leased: see
                # after_lease_second.
                self.after_lease_second()
                results, requests = self.pair(self.run_args(), responses=[response])
                self.assertEqual(len(requests), 1)
                report = self.report(results[False])
                self.assertEqual(
                    report["execution"]["errors"],
                    [{"job_id": JOB_IDS[0], "error": message}],
                )
                self.assertEqual(report["execution"]["completed"], [])

    def start_blocked(self, oracle, args):
        gate = Gate()
        self.server.reset([gate])
        child = subprocess.Popen(
            self.command(oracle, args),
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=self.oracle_env() if oracle else self.env,
        )
        self.addCleanup(self.retire, child, gate)
        self.assertTrue(gate.entered.wait(15), "CLI did not reach loopback HTTP")
        return child, gate

    @staticmethod
    def retire(child, gate):
        gate.release.set()
        if child.poll() is None:
            child.kill()
        child.communicate(timeout=15)

    def finish(self, child, gate):
        gate.release.set()
        stdout, stderr = child.communicate(timeout=15)
        return subprocess.CompletedProcess(child.args, child.returncode, stdout, stderr)

    def assert_writer_available(self, oracle):
        with open(str(self.db[oracle]) + ".lock", "a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with closing(sqlite3.connect(self.db[oracle], timeout=0.1)) as db:
                db.execute("BEGIN IMMEDIATE")
                db.rollback()

    def test_network_wait_releases_application_and_sqlite_writer_locks(self):
        before = {flag: self.snapshot(flag) for flag in (True, False)}
        results = {}
        for flag in (True, False):
            child, gate = self.start_blocked(flag, self.run_args())
            self.assert_writer_available(flag)
            results[flag] = self.finish(child, gate)
        self.assert_states(before, allowed=MUTABLE_WORKER_TABLES)
        self.assert_results(results)

    def test_cancellation_during_network_wait_fences_late_response(self):
        before = {flag: self.snapshot(flag) for flag in (True, False)}
        results = {}
        for flag in (True, False):
            child, gate = self.start_blocked(flag, self.run_args())
            self.assert_writer_available(flag)
            # The already-qualified owner cancels each catalog identically;
            # this test concerns the orchestration's late-response fence.
            cancelled = self.python(
                "from catabolic.store import Store;from catabolic.app import Application;from catabolic.processors import Processors\nwith Store(sys.argv[1],writable=True) as store: Processors(Application(store)).cancel(sys.argv[2])",
                str(self.db[flag]),
                JOB_IDS[0],
            )
            self.assertEqual(cancelled.returncode, 0, cancelled.stderr)
            results[flag] = self.finish(child, gate)
        self.assert_states(before, allowed=MUTABLE_WORKER_TABLES)
        self.assert_results(results)
        report = self.report(results[False])
        self.assertEqual(
            report["execution"]["errors"],
            [
                {
                    "job_id": JOB_IDS[0],
                    "error": "worker lease is expired, replaced or no longer active",
                }
            ],
        )
        self.assertEqual(
            self.snapshot(False)["processor_attempts"][0]["state"], "cancelled"
        )

    @staticmethod
    def descendants(pid):
        # Portable kernel process table (Linux procps and macOS ps), covering
        # both Python's and the native worker thread's HTTP children.
        return process_tree.descendants(pid)

    def test_interrupt_reaps_http_child_and_leaves_reclaimable_durable_lease(self):
        before = {flag: self.snapshot(flag) for flag in (True, False)}
        results = {}
        for flag in (True, False):
            child, gate = self.start_blocked(flag, self.run_args(mode="--machine"))
            descendants = self.descendants(child.pid)
            self.assertTrue(descendants, "no bounded HTTP subprocess was observed")
            child.send_signal(signal.SIGINT)
            # Keep the fake endpoint blocked until supervision has finished.
            stdout, stderr = child.communicate(timeout=15)
            results[flag] = subprocess.CompletedProcess(
                child.args, child.returncode, stdout, stderr
            )
            self.assertEqual(child.returncode, 130, (stdout, stderr))
            deadline = time.monotonic() + 5
            while process_tree.present(descendants) and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertFalse(
                process_tree.present(descendants),
                "HTTP descendants were not reaped",
            )
            self.assert_writer_available(flag)
            gate.release.set()
        self.assert_states(before, allowed=MUTABLE_WORKER_TABLES)
        self.assert_results(results)
        job = next(
            r for r in self.snapshot(False)["processor_jobs"] if r["id"] == JOB_IDS[0]
        )
        self.assertEqual((job["state"], job["generation"]), ("leased", 1))
        self.assertEqual(
            self.snapshot(False)["processor_attempts"][0]["state"], "leased"
        )


if __name__ == "__main__":
    unittest.main()
