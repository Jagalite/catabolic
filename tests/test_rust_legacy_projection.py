# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Narrow HTTP-v3 preserved projection transport; disposable catalogs only.

The pinned Python process and native process never share a writable catalog.
No receiver runs, no delivery worker is invoked, and no credentials are used.
This does not qualify destination/publication owners or release gates.
"""

import array
import fcntl
import json
import os
import shutil
import signal
import sqlite3
import subprocess
import tempfile
import termios
import time
import unittest
from contextlib import closing
from pathlib import Path

from tests.test_rust_legacy_review import BINARY, ORACLE, PYTHON, REFERENCE

SEED = """
from catabolic.store import Store
from catabolic.saved_queries import Queries
from catabolic.openapi_operations import put
import json,sys
spec={"openapi":"3.1.0","info":{"title":"Fixture","version":"1"},"paths":{"/notify":{"post":{"operationId":"notify","requestBody":{"required":True,"content":{"application/json":{"schema":{"type":"object","properties":{"id":{"type":"string"},"title":{"type":"string"}},"required":["id","title"],"additionalProperties":False}}}},"responses":{"204":{"description":"Queued"}}}}}}
with Store(sys.argv[1],writable=True) as store:
    query=Queries(store).put("fixture",{"mode":"rows","selection":{"language":"sql","query":"SELECT 'b' AS id, 'Ångström' AS title UNION ALL SELECT 'a', 'Alpha'"}})["id"]
    operation=put(store,"default","fixture",spec,"notify","http://127.0.0.1:9",None)["id"]
print(json.dumps({"version":3,"id":"fixture","query":query,"operation":operation,"key":{"column":"id"},"body":{"id":{"column":"id"},"title":{"column":"title"}}}))
"""


class NativeLegacyProjectionTest(unittest.TestCase):
    def setUp(self):
        for path in (PYTHON, REFERENCE / "catabolic/cli.py", BINARY):
            self.assertTrue(path.is_file(), str(path))
        tmp = tempfile.TemporaryDirectory(prefix="projection-cli-")
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.db = {True: self.root / "python.db", False: self.root / "rust.db"}
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
        seeded = subprocess.run(
            [
                str(PYTHON),
                "-c",
                "import sys;sys.path.insert(0,sys.argv.pop(1));" + SEED,
                str(REFERENCE),
                str(self.db[True]),
            ],
            env=self.env,
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(seeded.returncode, 0, seeded.stderr)
        self.definition = json.loads(seeded.stdout)
        self.definition_file = self.root / "definition.json"
        self.definition_file.write_text(json.dumps(self.definition, sort_keys=True))
        shutil.copyfile(self.db[True], self.db[False])

    def command(self, oracle, args, database=True):
        command = (
            [str(PYTHON), "-c", ORACLE, str(REFERENCE)] if oracle else [str(BINARY)]
        )
        return command + (["--db", str(self.db[oracle])] if database else []) + args

    def run_cli(self, oracle, args, *, database=True, stdin=None):
        return subprocess.run(
            self.command(oracle, args, database),
            input=stdin,
            env=self.env,
            capture_output=True,
            text=True,
            timeout=30,
        )

    def compare(self, args, **kwargs):
        old, new = (self.run_cli(flag, args, **kwargs) for flag in (True, False))

        def normalize(text):
            return text.replace(str(self.db[True]), "<DB>").replace(
                str(self.db[False]), "<DB>"
            )

        self.assertEqual(
            new.returncode,
            old.returncode,
            (old.stdout, old.stderr, new.stdout, new.stderr),
        )
        self.assertEqual(normalize(new.stderr), normalize(old.stderr))
        self.assertEqual(normalize(new.stdout), normalize(old.stdout))
        return old, new

    def args(self, operation="mapping-preview", **options):
        out = ["projection", operation, "--definition", str(self.definition_file)]
        for key, value in options.items():
            out += ["--" + key.replace("_", "-"), str(value)]
        return out

    def snapshot(self, flag):
        with closing(sqlite3.connect(self.db[flag])) as db:
            return list(db.iterdump())

    def test_preview_channels_and_read_only_effects(self):
        before = {flag: self.snapshot(flag) for flag in (True, False)}
        for mode in ([], ["--json"], ["--machine"]):
            with self.subTest(mode=mode):
                self.compare(mode + self.args())
        for flag in (True, False):
            self.assertEqual(before[flag], self.snapshot(flag))
            self.assertFalse(Path(str(self.db[flag]) + ".plex-metadata.lock").exists())

    def test_stdin_definition_matches_file_transport(self):
        for mode in ([], ["--json"], ["--machine"]):
            self.compare(
                mode + ["projection", "mapping-preview", "--definition", "-"],
                stdin=self.definition_file.read_text(),
            )

    def test_open_stdin_interrupts_both_leaves_before_eof(self):
        before = {flag: self.snapshot(flag) for flag in (True, False)}
        for operation in ("mapping-preview", "mapping-apply"):
            for mode in ([], ["--json"], ["--machine"]):
                outputs = []
                for flag in (True, False):
                    args = mode + ["projection", operation, "--definition", "-"]
                    if operation == "mapping-apply":
                        args += ["--expected-plan", "stale"]
                    reader, writer = os.pipe()
                    original_flags = fcntl.fcntl(reader, fcntl.F_GETFL)
                    process = subprocess.Popen(
                        self.command(flag, args),
                        stdin=reader,
                        stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE,
                        env=self.env,
                        text=True,
                    )
                    try:
                        os.write(writer, b'{"version":3}')
                        deadline = time.monotonic() + 15
                        consumed = False
                        while time.monotonic() < deadline and process.poll() is None:
                            waiting = array.array("i", [0])
                            fcntl.ioctl(reader, termios.FIONREAD, waiting, True)
                            if waiting[0] == 0:
                                consumed = True
                                break
                            time.sleep(0.01)
                        self.assertTrue(
                            consumed,
                            "child must consume the pipe bytes before signalling",
                        )
                        time.sleep(1)
                        self.assertIsNone(process.poll(), "input must still await EOF")
                        process.send_signal(signal.SIGINT)
                        # wait() does not close stdin. In particular, do not use
                        # communicate(input=...) to manufacture EOF before exit.
                        process.wait(timeout=2)
                        out, err = process.communicate(timeout=2)
                        self.assertEqual(process.returncode, 130, (out, err))
                        self.assertEqual(
                            fcntl.fcntl(reader, fcntl.F_GETFL), original_flags
                        )
                        outputs.append((out, err))
                    finally:
                        if process.poll() is None:
                            process.kill()
                        process.communicate(timeout=5)
                        os.close(writer)
                        os.close(reader)
                    self.assertEqual(before[flag], self.snapshot(flag))
                    self.assertFalse(
                        Path(str(self.db[flag]) + ".plex-metadata.lock").exists()
                    )
                self.assertEqual(outputs[1], outputs[0])

    def test_both_leaf_file_stdin_completion_and_malformed_controls(self):
        for operation in ("mapping-preview", "mapping-apply"):
            extra = ["--expected-plan", "stale"] if operation == "mapping-apply" else []
            for mode in ([], ["--json"], ["--machine"]):
                self.definition_file.write_text(
                    json.dumps(self.definition, sort_keys=True)
                )
                self.compare(
                    mode + ["projection", operation, "--definition", "-"] + extra,
                    stdin=self.definition_file.read_text(),
                )
                self.definition_file.write_text("{")
                for flag in (True, False):
                    from_file = self.run_cli(flag, mode + self.args(operation) + extra)
                    from_stdin = self.run_cli(
                        flag,
                        mode + ["projection", operation, "--definition", "-"] + extra,
                        stdin="{",
                    )
                    self.assertEqual(from_stdin.returncode, 2)
                    self.assertEqual(
                        (from_stdin.stdout, from_stdin.stderr),
                        (from_file.stdout, from_file.stderr),
                    )
                    payload = from_stdin.stdout + from_stdin.stderr
                    if mode:
                        # Preserve each existing engine's diagnostic provenance;
                        # the documented malformed-JSON parity gap remains open.
                        self.assertIn(
                            "JSONDecodeError" if flag else "CatabolicError", payload
                        )
                    if not flag:
                        self.assertIn("invalid JSON", payload)

    def test_apply_exact_result_and_isolated_queue(self):
        preview = json.loads(self.run_cli(True, ["--json"] + self.args()).stdout)
        before = {flag: self.snapshot(flag) for flag in (True, False)}
        self.compare(
            ["--machine"]
            + self.args(
                "mapping-apply", expected_plan=preview["plan_id"], max_removals=-17
            )
        )
        ids = []
        for flag in (True, False):
            self.assertNotEqual(before[flag], self.snapshot(flag))
            with closing(sqlite3.connect(self.db[flag])) as db:
                queue = db.execute(
                    "SELECT event_id,state,attempts FROM notification_deliveries ORDER BY event_id"
                ).fetchall()
                self.assertEqual(len(queue), 2)
                self.assertTrue(
                    all(
                        state == "pending" and attempts == 0
                        for _, state, attempts in queue
                    ),
                    queue,
                )
                ids.append({row[0] for row in queue})
        self.assertTrue(ids[0].isdisjoint(ids[1]))

    def test_stale_plan_and_empty_expected_channels(self):
        before = {flag: self.snapshot(flag) for flag in (True, False)}
        for expected in ("stale", ""):
            for mode in ([], ["--json"], ["--machine"]):
                with self.subTest(expected=expected, mode=mode):
                    self.compare(
                        mode + self.args("mapping-apply", expected_plan=expected)
                    )
        for flag in (True, False):
            self.assertEqual(before[flag], self.snapshot(flag))

    def test_native_owner_admission_errors(self):
        for change in (
            {"version": 3.0},
            {"key": {"column": "absent"}},
            {"body": {"constant": 3}},
            {"operation": "absent"},
            {"query": "absent"},
            {"id": ""},
            {"unexpected": True},
        ):
            self.definition_file.write_text(
                json.dumps({**self.definition, **change}, sort_keys=True)
            )
            for mode in ([], ["--json"], ["--machine"]):
                with self.subTest(change=change, mode=mode):
                    self.compare(mode + self.args())

    def test_metadata_lock_contention_then_release(self):
        locks = []
        for flag in (True, False):
            lock = open(str(self.db[flag]) + ".plex-metadata.lock", "w")
            self.addCleanup(lock.close)
            locks.append(lock)
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        self.compare(["--machine"] + self.args("mapping-apply", expected_plan=""))
        self.compare(["--machine"] + self.args())
        for lock in locks:
            lock.close()
        self.compare(["--machine"] + self.args("mapping-apply", expected_plan=""))

    def test_catalog_writer_contention_releases_metadata_lock(self):
        locks = []
        for flag in (True, False):
            lock = open(str(self.db[flag]) + ".lock", "w")
            self.addCleanup(lock.close)
            locks.append(lock)
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        self.compare(["--machine"] + self.args("mapping-apply", expected_plan="stale"))
        self.compare(["--machine"] + self.args())
        for flag in (True, False):
            with open(str(self.db[flag]) + ".plex-metadata.lock", "r+") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        for lock in locks:
            lock.close()
        self.compare(["--machine"] + self.args("mapping-apply", expected_plan="stale"))

    def test_parse_surface_and_missing_database(self):
        for args in (
            ["projection", "mapping-apply", "--definition", "-"],
            ["projection", "mapping-recover", "event", "--resolution", "invalid"],
            ["projection", "mapping-preview", "--help"],
        ):
            self.compare(args, database=False)
        self.compare(["--machine"] + self.args(), database=False)

    def test_interruption_channels_release_both_locks_without_enqueue(self):
        sql = "WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT 'a' AS id, CAST(sum(x) AS TEXT) AS title FROM n"
        code = "import sys,json;sys.path.insert(0,sys.argv.pop(1));from catabolic.store import Store;from catabolic.saved_queries import Queries\nwith Store(sys.argv[1],writable=True) as store: print(Queries(store).put('slow',{'mode':'rows','selection':{'language':'sql','query':sys.argv[2]}})['id'])"
        result = subprocess.run(
            [str(PYTHON), "-c", code, str(REFERENCE), str(self.db[True]), sql],
            env=self.env,
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        shutil.copyfile(self.db[True], self.db[False])
        definition = {**self.definition, "query": result.stdout.strip()}
        self.definition_file.write_text(json.dumps(definition, sort_keys=True))
        before = {flag: self.snapshot(flag) for flag in (True, False)}
        for mode in ([], ["--machine"]):
            outputs = []
            for flag in (True, False):
                process = subprocess.Popen(
                    self.command(
                        flag, mode + self.args("mapping-apply", expected_plan="stale")
                    ),
                    env=self.env,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                )
                self.addCleanup(
                    lambda p=process: p.kill() if p.poll() is None else None
                )
                deadline = time.monotonic() + 10
                held = False
                while time.monotonic() < deadline and process.poll() is None:
                    path = Path(str(self.db[flag]) + ".plex-metadata.lock")
                    if path.exists():
                        with path.open("r+") as lock:
                            try:
                                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                                fcntl.flock(lock, fcntl.LOCK_UN)
                            except BlockingIOError:
                                held = True
                                break
                    time.sleep(0.01)
                self.assertTrue(held, "metadata lock must be held before interruption")
                process.send_signal(signal.SIGINT)
                out, err = process.communicate(timeout=10)
                self.assertEqual(process.returncode, 130, (out, err))
                outputs.append((out, err))
                self.assertEqual(before[flag], self.snapshot(flag))
                for suffix in (".lock", ".plex-metadata.lock"):
                    with open(str(self.db[flag]) + suffix, "a+") as lock:
                        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            self.assertEqual(outputs[1], outputs[0])

    def test_arbitrary_definition_order_is_explicitly_unqualified(self):
        self.definition_file.write_text(json.dumps(self.definition))
        old, new = (
            self.run_cli(flag, ["--json"] + self.args()) for flag in (True, False)
        )
        self.assertEqual(old.returncode, 0, old.stderr)
        self.assertEqual(new.returncode, 0, new.stderr)
        self.assertEqual(json.loads(old.stdout), json.loads(new.stdout))
        # The native owner's sorted Value loses the source dictionary order.
        # This named, visible gap prevents an exhaustive byte-parity claim.
        self.assertNotEqual(old.stdout, new.stdout)

    def test_unimplemented_owners_stay_closed(self):
        cases = []
        for version in (2,):
            path = self.root / f"v{version}.json"
            path.write_text(json.dumps({"version": version}))
            cases.append(
                [
                    "projection",
                    "mapping-apply",
                    "--definition",
                    str(path),
                    "--expected-plan",
                    "anything",
                ]
            )
        before = self.snapshot(False)
        # Version-two folder apply is open (track C4): an invalid definition is
        # refused by the same admission in both engines.
        for args in cases:
            old, new = self.compare(["--machine"] + args)
            self.assertEqual(new.returncode, 2)
            self.assertIn("invalid_publication_definition", new.stdout)
        # Ordinary projection preview now has a rollback-only owner; execute
        # retains an earlier explicit publication gate. Neither publishes.
        for operation, diagnostic in [
            ("preview", "unknown catalog"),
            ("execute", "native projection execute is blocked"),
        ]:
            result = self.run_cli(
                False, ["--machine", "projection", operation, "missing"]
            )
            self.assertEqual(result.returncode, 2)
            self.assertIn(diagnostic, result.stdout)
        self.assertEqual(before, self.snapshot(False))


if __name__ == "__main__":
    unittest.main()
