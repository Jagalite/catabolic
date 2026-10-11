# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native workflow mutations against the frozen Python owner on cloned catalogs."""

import json
import select
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.item_workflow import ItemWorkflow
from catabolic.processing import Processing
from catabolic.store import Store
from tests.test_rust_migration import BINARY


@unittest.skipUnless(BINARY.is_file(), "build catabolic-cli before qualification")
class NativeItemWorkflowTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.source = self.root / "source"
        self.source.mkdir()
        (self.source / "book.txt").write_text("A book.\n")
        self.path = self.root / "native.sqlite3"
        self.oracle = self.root / "oracle.sqlite3"
        Store.initialize(self.path)
        with Store(self.path, writable=True) as store:
            app = Application(store)
            app.bind("source", "media", str(self.source))
            app.scan()
            self.file = store.rows("SELECT id FROM files")[0]["id"]
            app.put_item("book", {}, {"title": "Book"}, "book")
            app.media.associate(self.file, "book")
        self.worker = subprocess.Popen(
            [str(BINARY), "--stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(self.close_worker)

    def close_worker(self):
        self.worker.stdin.close()
        try:
            self.worker.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.worker.kill()
            self.worker.wait()
        self.worker.stdout.close()
        self.worker.stderr.close()

    def native(self, action, args, *, ok=True, profile="default"):
        self.worker.stdin.write(
            json.dumps(
                [
                    "--db",
                    str(self.path),
                    "--profile",
                    profile,
                    "workflow",
                    action,
                    json.dumps(args),
                ]
            )
            + "\n"
        )
        self.worker.stdin.flush()
        self.assertTrue(select.select([self.worker.stdout], [], [], 120)[0])
        response = json.loads(self.worker.stdout.readline())
        self.assertEqual(response["status"], 0 if ok else 2, response)
        return (
            response["result"] if ok else response["error"].removeprefix("catabolic: ")
        )

    def normalized(self, value):
        if isinstance(value, list):
            return [self.normalized(v) for v in value]
        if isinstance(value, dict):
            return {
                k: self.normalized(v)
                for k, v in value.items()
                if k not in ("created_at", "updated_at")
            }
        return value

    def state(self, path):
        with Store(path) as store:
            return self.normalized(
                {
                    table: store.rows(f"SELECT * FROM {table} ORDER BY rowid")
                    for table in ("item_workflow", "item_worklog", "item_requirements")
                }
            )

    def differential(self, action, args, call, *, ok=True, profile="default"):
        shutil.copyfile(self.path, self.oracle)
        actual = self.native(action, args, ok=ok, profile=profile)
        with Store(self.oracle, writable=True) as store:
            workflow = ItemWorkflow(Application(store, profile=profile))
            if ok:
                identifier = actual.get("requirement_id", "unused")
                with patch("catabolic.item_workflow.uuid4", return_value=identifier):
                    expected = call(workflow)
                self.assertEqual(self.normalized(actual), self.normalized(expected))
            else:
                with self.assertRaises(CatabolicError) as caught:
                    call(workflow)
                self.assertEqual(actual, str(caught.exception))
        self.assertEqual(self.state(self.path), self.state(self.oracle))
        return actual

    def test_defaults_read_pages_and_worklog_revision_history(self):
        for action, call in (
            ("get", lambda w: w.get("book")),
            ("checks", lambda w: w.checks("book")),
            ("log", lambda w: w.log("book")),
        ):
            self.differential(action, {"item_id": "book"}, call)
        body = "First line\nSecond line — 中文"
        self.differential(
            "note",
            {
                "item_id": "book",
                "body": body,
                "actor": "agent:cataloger",
                "expected_revision": 0,
            },
            lambda w: w.note(
                "book", body, actor="agent:cataloger", expected_revision=0
            ),
        )
        self.differential(
            "status",
            {
                "item_id": "book",
                "status": "deferred",
                "note": "Edition pending",
                "expected_revision": 1,
            },
            lambda w: w.set_status(
                "book", "deferred", note="Edition pending", expected_revision=1
            ),
        )
        self.differential(
            "note",
            {"item_id": "book", "body": "stale", "expected_revision": 0},
            lambda w: w.note("book", "stale", expected_revision=0),
            ok=False,
        )
        page = self.differential(
            "log", {"item_id": "book", "limit": 1}, lambda w: w.log("book", limit=1)
        )
        self.differential(
            "log",
            {"item_id": "book", "after": page["next_after"]},
            lambda w: w.log("book", after=page["next_after"]),
        )

    def test_manual_review_waiver_reopen_and_atomic_rejection(self):
        added = self.differential(
            "require",
            {"item_id": "book", "kind": "review", "label": "Confirm edition"},
            lambda w: w.require("book", "review", "Confirm edition"),
        )
        requirement = added["requirement_id"]
        self.differential(
            "status",
            {"item_id": "book", "status": "complete"},
            lambda w: w.set_status("book", "complete"),
            ok=False,
        )
        for state in ("waived", "pending", "complete"):
            self.differential(
                "resolve",
                {
                    "requirement_id": requirement,
                    "state": state,
                    "note": "Checked evidence",
                },
                lambda w, state=state: w.resolve(
                    requirement, state, note="Checked evidence"
                ),
            )
        self.differential(
            "status",
            {"item_id": "book", "status": "complete"},
            lambda w: w.set_status("book", "complete"),
        )

    def test_required_job_current_evidence_and_source_change(self):
        with Store(self.path, writable=True) as store:
            app = Application(store)
            job = Processing(app).enqueue("hash", file_ids=[self.file])["queued"][0]
        added = self.differential(
            "require",
            {"item_id": "book", "kind": "job", "label": "Checksum", "target": job},
            lambda w: w.require("book", "job", "Checksum", target=job),
        )
        self.differential(
            "resolve",
            {
                "requirement_id": added["requirement_id"],
                "state": "complete",
                "note": "Pretend",
            },
            lambda w: w.resolve(added["requirement_id"], "complete", note="Pretend"),
            ok=False,
        )
        self.differential(
            "status",
            {"item_id": "book", "status": "complete"},
            lambda w: w.set_status("book", "complete"),
            ok=False,
        )
        with Store(self.path, writable=True) as store:
            self.assertTrue(Processing(Application(store)).run()["complete"])
        self.differential(
            "status",
            {"item_id": "book", "status": "complete"},
            lambda w: w.set_status("book", "complete"),
        )
        before = self.native("log", {"item_id": "book"})
        (self.source / "book.txt").write_text("Changed book and size.\n")
        with Store(self.path, writable=True) as store:
            Application(store).scan()
        after = self.differential("get", {"item_id": "book"}, lambda w: w.get("book"))
        self.assertEqual(after["status"], "needs_attention")
        self.assertEqual(before, self.native("log", {"item_id": "book"}))
        self.differential("checks", {"item_id": "book"}, lambda w: w.checks("book"))

    def test_invalid_targets_and_text_roll_back(self):
        cases = [
            ("note", {"item_id": "book", "body": ""}, lambda w: w.note("book", "")),
            (
                "note",
                {"item_id": "book", "body": "x", "actor": ""},
                lambda w: w.note("book", "x", actor=""),
            ),
            (
                "note",
                {"item_id": "book", "body": "x", "expected_revision": True},
                lambda w: w.note("book", "x", expected_revision=True),
            ),
            (
                "status",
                {"item_id": "book", "status": "needs_attention"},
                lambda w: w.set_status("book", "needs_attention"),
            ),
            (
                "require",
                {
                    "item_id": "book",
                    "kind": "review",
                    "label": "Read",
                    "target": "unexpected",
                },
                lambda w: w.require("book", "review", "Read", target="unexpected"),
            ),
            (
                "require",
                {
                    "item_id": "book",
                    "kind": "job",
                    "label": "Read",
                    "target": "missing",
                },
                lambda w: w.require("book", "job", "Read", target="missing"),
            ),
            (
                "log",
                {"item_id": "book", "after": True},
                lambda w: w.log("book", after=True),
            ),
        ]
        for action, args, call in cases:
            with self.subTest(action=action, args=args):
                self.differential(action, args, call, ok=False)
        self.assertEqual(self.native("log", {"item_id": "book"})["entries"], [])

    def test_title_and_primary_absence_block_completion(self):
        with Store(self.path, writable=True) as store:
            app = Application(store)
            app.put_item("book", {}, {}, "untitled")
        self.differential(
            "status",
            {"item_id": "untitled", "status": "complete"},
            lambda w: w.set_status("untitled", "complete"),
            ok=False,
        )
        (self.source / "book.txt").unlink()
        with Store(self.path, writable=True) as store:
            Application(store).scan()
        self.differential(
            "status",
            {"item_id": "book", "status": "complete"},
            lambda w: w.set_status("book", "complete"),
            ok=False,
        )

    def test_profile_readiness_global_revision_and_requirement_limit(self):
        with Store(self.path, writable=True) as store:
            Application(store).add_profile("offline")
        self.differential(
            "status",
            {"item_id": "book", "status": "complete"},
            lambda w: w.set_status("book", "complete"),
        )
        other = self.differential(
            "get", {"item_id": "book"}, lambda w: w.get("book"), profile="offline"
        )
        self.assertEqual(other["status"], "needs_attention")
        self.differential(
            "note",
            {"item_id": "book", "body": "Global history", "expected_revision": 1},
            lambda w: w.note("book", "Global history", expected_revision=1),
            profile="offline",
        )
        self.assertEqual(self.native("get", {"item_id": "book"})["revision"], 2)
        with Store(self.path, writable=True) as store:
            with store.transaction() as db:
                db.executemany(
                    "INSERT INTO item_requirements(id,item_id,profile,kind,label,state) VALUES (?,'book','default','review','Check','pending')",
                    [(f"requirement-{i}",) for i in range(1000)],
                )
        self.differential(
            "require",
            {"item_id": "book", "kind": "review", "label": "Too many"},
            lambda w: w.require("book", "review", "Too many"),
            ok=False,
        )

    def test_requirement_and_worklog_rollback_on_invalid_actor(self):
        self.differential(
            "require",
            {"item_id": "book", "kind": "review", "label": "Review", "actor": ""},
            lambda w: w.require("book", "review", "Review", actor=""),
            ok=False,
        )
        self.assertEqual(self.native("log", {"item_id": "book"})["entries"], [])

    def test_large_expected_revision_and_control_whitespace_match_python(self):
        for expected in (2**100, 1.5, -1):
            self.differential(
                "note",
                {"item_id": "book", "body": "x", "expected_revision": expected},
                lambda w, expected=expected: w.note(
                    "book", "x", expected_revision=expected
                ),
                ok=False,
            )
        self.differential(
            "note",
            {"item_id": "book", "body": "\x1c"},
            lambda w: w.note("book", "\x1c"),
            ok=False,
        )
        self.differential(
            "note",
            {"item_id": "book", "body": "x", "kind": None},
            lambda w: w.note("book", "x", kind=None),
            ok=False,
        )

    def test_multibyte_note_limit_and_event_page_bounds(self):
        body = "界" * 21846
        self.differential(
            "note",
            {"item_id": "book", "body": body},
            lambda w: w.note("book", body),
            ok=False,
        )
        for limit in (0, True, 1001):
            self.differential(
                "checks",
                {"item_id": "book", "limit": limit},
                lambda w, limit=limit: w.checks("book", limit=limit),
                ok=False,
            )

    def test_worklog_result_budget_keeps_continuation_cursor(self):
        body = "界" * 21000
        with Store(self.path, writable=True) as store, store.transaction() as db:
            db.executemany(
                "INSERT INTO item_worklog(item_id,profile,revision,kind,body,actor,data) VALUES ('book','default',?,'note',?,'user','{}')",
                [(revision, body) for revision in range(1, 71)],
            )
            db.execute(
                "INSERT INTO item_workflow(item_id,status,revision,updated_by) VALUES ('book','pending',70,'user')"
            )
        first = self.differential(
            "log",
            {"item_id": "book", "limit": 1000},
            lambda w: w.log("book", limit=1000),
        )
        self.assertLess(len(first["entries"]), 70)
        self.assertEqual(first["next_after"], first["entries"][-1]["id"])
        second = self.differential(
            "log",
            {"item_id": "book", "limit": 1000, "after": first["next_after"]},
            lambda w: w.log("book", limit=1000, after=first["next_after"]),
        )
        self.assertIsNone(second["next_after"])
        self.assertEqual(len(first["entries"]) + len(second["entries"]), 70)
