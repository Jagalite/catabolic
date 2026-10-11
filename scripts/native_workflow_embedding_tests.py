#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Persistent workflow/curation contracts, with a separate frozen-oracle scenario.

Only disposable databases and files are used. Timestamp and generated requirement
UUID normalization is explicit; deterministic proposals/cursors remain exact.
"""

import os
import signal
import tempfile
import threading
import time
import unittest
from pathlib import Path
from uuid import UUID


def seed(app, root):
    """Seed identical recorded evidence without relying on scan implementations."""
    source = Path(root) / "source"
    source.mkdir(exist_ok=True)
    for name in ("workflow.mkv", "workflow.en.srt"):
        path = source / name
        if not path.exists():
            path.write_bytes(b"disposable workflow fixture\n")
    app.bind("source", "media", str(source))
    app.put_item("movie", {"fixture": "workflow"}, {"title": "Workflow"}, "workflow")
    app.put_item("collection", {}, {"title": "Collection"}, "collection")
    with app.store.transaction() as db:
        db.execute(
            "INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('workflow-scan','default','media',1,2,'[]')"
        )
        for identifier, name in (
            ("workflow-file", "workflow.mkv"),
            ("0-workflow-sub", "workflow.en.srt"),
        ):
            stat = (source / name).stat()
            db.execute(
                "INSERT INTO files(id,location,path) VALUES (?,'media',?)",
                (identifier, name),
            )
            db.execute(
                "INSERT INTO observations(profile,file_id,size,mtime_ns,device,inode,status,scan_id) VALUES ('default',?,?,?,?,?,'present','workflow-scan')",
                (identifier, stat.st_size, stat.st_mtime_ns, stat.st_dev, stat.st_ino),
            )
    app.media.associate("workflow-file", "workflow", role="primary")


def paired_workflows(app, root, engine):
    if engine == "reference":
        from catabolic.curation import Curation, occurrence
        from catabolic.domain import CatabolicError
        from catabolic.item_workflow import ItemWorkflow
    else:
        from catabolic_native import CatabolicError, Curation, ItemWorkflow
        from catabolic_native.curation import occurrence
    seed(app, root)
    store = app.store
    workflow, curation = ItemWorkflow(app), Curation(app)
    assert (
        workflow.app is app
        and workflow.store is store
        and workflow.profile == app.profile
    )
    assert (
        curation.app is app
        and curation.store is store
        and curation.profile == app.profile
    )
    result = {}
    generated = {}

    def error(name, action):
        try:
            action()
        except CatabolicError as exc:
            result[name] = str(exc)
        else:
            raise AssertionError(f"{name} did not reject invalid input")

    result["workflow_initial"] = workflow.get("workflow")
    result["workflow_note"] = workflow.note(
        "workflow",
        "First line\n中文",
        kind="progress",
        actor="paired-reviewer",
        expected_revision=0,
    )
    result["workflow_status"] = workflow.set_status(
        "workflow",
        "in_progress",
        note="Reviewing",
        actor="paired-reviewer",
        expected_revision=1,
    )
    requirement = workflow.require(
        "workflow",
        "review",
        "Review title",
        note="Requested",
        actor="paired-reviewer",
        expected_revision=2,
    )
    assert UUID(requirement["requirement_id"]).version == 4
    generated[requirement["requirement_id"]] = "REVIEW_REQUIREMENT"
    result["workflow_require"] = requirement
    file_requirement = workflow.require(
        "workflow", "file", "Primary file", target="workflow-file"
    )
    assert UUID(file_requirement["requirement_id"]).version == 4
    generated[file_requirement["requirement_id"]] = "FILE_REQUIREMENT"
    result["workflow_require_file"] = file_requirement
    checks = workflow.checks("workflow", limit=1)
    # UUID order is nondeterministic, but cursor must refer to the emitted row.
    assert checks["next_after"] == checks["checks"][-1]["check_id"]
    next_checks = workflow.checks("workflow", limit=1, after=checks["next_after"])
    assert next_checks["next_after"] is None
    result["workflow_checks"] = sorted(
        checks["checks"] + next_checks["checks"], key=lambda row: row["label"]
    )
    result["workflow_checks_evidence"] = checks["evidence"]
    error(
        "workflow_completion_blocked",
        lambda: workflow.set_status("workflow", "complete"),
    )
    error(
        "workflow_stale_revision",
        lambda: workflow.note("workflow", "stale", expected_revision=1),
    )
    error(
        "workflow_file_cannot_resolve",
        lambda: workflow.resolve(
            file_requirement["requirement_id"], "complete", note="Not allowed"
        ),
    )
    result["workflow_resolve"] = workflow.resolve(
        requirement["requirement_id"],
        "complete",
        note="Reviewed",
        actor="paired-reviewer",
        expected_revision=4,
    )
    result["workflow_complete"] = workflow.set_status(
        "workflow", "complete", expected_revision=5
    )
    first_log = workflow.log("workflow", limit=2)
    assert type(first_log["next_after"]) is int
    result["workflow_log_first"] = first_log
    result["workflow_log_after"] = workflow.log(
        "workflow", after=first_log["next_after"]
    )
    for name, action in (
        ("workflow_unknown", lambda: workflow.get("absent")),
        ("workflow_bool_limit", lambda: workflow.checks("workflow", limit=True)),
        ("workflow_bool_after", lambda: workflow.log("workflow", after=True)),
        ("workflow_bad_note", lambda: workflow.note("workflow", "\0")),
        (
            "workflow_bad_revision",
            lambda: workflow.note("workflow", "bad", expected_revision=True),
        ),
    ):
        error(name, action)
    result["occurrence"] = occurrence(store, app.profile, "workflow-file")
    payload = {"item_id": "workflow", "role": "primary", "part": None}
    result["curation_snapshot"] = curation.snapshot("workflow-file", payload)
    proposal = curation.put(
        "workflow-file",
        payload,
        source="paired-review",
        evidence={"review": "✓", "score": 0.5},
    )
    result["curation_put"] = proposal
    assert (
        curation.put(
            "workflow-file",
            payload,
            source="paired-review",
            evidence={"review": "✓", "score": 0.5},
        )
        == proposal
    )
    result["curation_get"] = curation.get(proposal["id"])
    result["curation_accept_existing"] = curation.accept_existing_association_evidence(
        proposal["id"], actor="paired-reviewer"
    )
    assert result["curation_accept_existing"]["payload"] == payload
    assert (
        curation.accept_existing_association_evidence(
            proposal["id"], actor="paired-reviewer"
        )
        == result["curation_accept_existing"]
    )
    rejected = curation.put(
        "0-workflow-sub",
        {"item_id": "workflow", "role": "subtitle"},
        source="reject-test",
    )
    result["curation_rejected"] = curation.decide(
        rejected["id"], accept=False, actor="paired-reviewer"
    )
    assert (
        curation.decide(rejected["id"], accept=False, actor="paired-reviewer")
        == result["curation_rejected"]
    )
    error(
        "curation_decided",
        lambda: curation.decide(rejected["id"], accept=True, actor="paired-reviewer"),
    )
    result["curation_sidecars_scalar_cursors"] = [
        curation.sidecars(after=cursor)
        for cursor in (99999, 1.5, True, None, 0, False, "", "99999", [], {})
    ]
    result["curation_sidecars_preview"] = curation.sidecars(
        location="media", limit=100, apply=False
    )
    result["curation_sidecars_apply"] = curation.sidecars(
        location="media", limit=100, apply=True
    )
    sidecar = result["curation_sidecars_apply"]["candidates"][0]["proposal"]
    result["curation_accepted"] = curation.decide(
        sidecar, accept=True, actor="paired-reviewer"
    )
    first_proposals = curation.list(limit=1)
    assert type(first_proposals["next_after"]) is str
    result["curation_list_first"] = first_proposals
    result["curation_list_after"] = curation.list(after=first_proposals["next_after"])
    result["curation_list_state"] = curation.list(state="rejected")
    with store.transaction() as db:
        db.execute(
            "INSERT INTO proposals(id,profile,file_id,kind,payload,evidence,source,snapshot) "
            "SELECT '00000000-0000-0000-0000-000000000000',profile,file_id,kind,payload,evidence,source,snapshot "
            "FROM proposals WHERE id=?",
            (proposal["id"],),
        )
        result["curation_list_scalar_cursor"] = [
            curation.list(after=cursor)
            for cursor in (99999, 1.5, True, None, 0, False, "", "99999", [], {})
        ]
        db.execute(
            "DELETE FROM proposals WHERE id='00000000-0000-0000-0000-000000000000'"
        )
    first_history = curation.history(limit=1)
    assert type(first_history["next_after"]) is int
    result["curation_history_first"] = first_history
    result["curation_history_after"] = curation.history(
        after=first_history["next_after"]
    )
    # Frozen history also accepts SQLite scalar cursor values; preserve that boundary.
    result["curation_history_text_cursor"] = curation.history(
        after=str(first_history["next_after"])
    )
    expected = curation.expected_put(
        "collection",
        {
            "source": "paired",
            "edition": "2026",
            "ordering": "declared",
            "complete": True,
            "members": [
                {"key": "first", "identity": {"fixture": "workflow"}},
                {"key": "missing", "item_id": "missing"},
            ],
        },
    )
    result["curation_expected"] = expected
    first_members = curation.completeness(expected["id"], limit=1)
    assert type(first_members["next_after"]) is int
    result["curation_completeness_first"] = first_members
    result["curation_completeness_after"] = curation.completeness(
        expected["id"], after=first_members["next_after"]
    )
    for name, action in (
        ("occurrence_unknown", lambda: occurrence(store, app.profile, "absent")),
        ("curation_unknown", lambda: curation.get("absent")),
        ("curation_bad_state", lambda: curation.list(state="invalid")),
        ("curation_bad_limit", lambda: curation.history(limit=True)),
        (
            "curation_bad_offset",
            lambda: curation.completeness(expected["id"], after=True),
        ),
        (
            "curation_bad_evidence",
            lambda: curation.put("workflow-file", payload, evidence=[]),
        ),
        (
            "curation_bad_source",
            lambda: curation.put("workflow-file", payload, source=""),
        ),
    ):
        error(name, action)
    before = workflow.get("workflow")
    count = len(curation.list()["proposals"])
    try:
        with store.transaction():
            workflow.note("workflow", "outer rollback")
            pending = curation.put("workflow-file", payload, source="rollback")
            assert curation.get(pending["id"])["state"] == "pending"
            try:
                with store.transaction():
                    workflow.note("workflow", "inner rollback")
                    raise ValueError("inner")
            except ValueError:
                pass
            assert workflow.get("workflow")["revision"] == before["revision"] + 1
            raise ValueError("outer")
    except ValueError:
        pass
    assert workflow.get("workflow") == before
    assert len(curation.list()["proposals"]) == count
    result["workflow_nested_rollback"] = workflow.get("workflow")
    result["curation_nested_rollback"] = curation.list()

    def stable(value):
        if isinstance(value, dict):
            return {
                key: stable(value)
                for key, value in value.items()
                if key not in ("created_at", "updated_at")
            }
        if isinstance(value, list):
            return [stable(value) for value in value]
        return generated.get(value, value) if isinstance(value, str) else value

    return stable(result)


class WorkflowLifecycle(unittest.TestCase):
    def setUp(self):
        import catabolic_native as native

        self.native = native
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.path = self.root / "workflow.db"
        native.Store.initialize(self.path)
        self.store = native.Store(self.path, writable=True)
        self.addCleanup(self.store.close)
        self.app = native.Application(self.store)
        seed(self.app, self.root)
        self.workflow = native.ItemWorkflow(self.app)
        self.curation = native.Curation(self.app)

    def test_transaction_rollback_and_failure_restores_owner(self):
        payload = {"item_id": "workflow", "role": "primary", "part": None}
        with self.assertRaisesRegex(ValueError, "outer"):
            with self.store.transaction():
                self.workflow.note("workflow", "outer")
                self.curation.put("workflow-file", payload)
                with self.assertRaises(ValueError):
                    with self.store.transaction():
                        self.workflow.note("workflow", "inner")
                        raise ValueError("inner")
                self.assertEqual(self.workflow.get("workflow")["revision"], 1)
                with self.assertRaises(self.native.CatabolicError):
                    self.workflow.note("workflow", "stale", expected_revision=0)
                self.assertEqual(self.workflow.get("workflow")["revision"], 1)
                raise ValueError("outer")
        self.assertEqual(self.workflow.get("workflow")["revision"], 0)
        self.assertEqual(self.curation.list()["proposals"], [])

        # Requirement creation and resolution must share the same lexical
        # transaction scopes as notes, including the worklog/revision changes.
        with self.assertRaisesRegex(ValueError, "requirements outer"):
            with self.store.transaction():
                requirement = self.workflow.require(
                    "workflow", "review", "Disposable review", expected_revision=0
                )
                identifier = requirement["requirement_id"]
                with self.assertRaisesRegex(ValueError, "requirements inner"):
                    with self.store.transaction():
                        self.workflow.resolve(
                            identifier,
                            "complete",
                            note="Inner resolution",
                            expected_revision=1,
                        )
                        self.assertEqual(self.workflow.get("workflow")["revision"], 2)
                        raise ValueError("requirements inner")
                self.assertEqual(self.workflow.get("workflow")["revision"], 1)
                checks = self.workflow.checks("workflow")["checks"]
                self.assertEqual(len(checks), 1)
                self.assertEqual(checks[0]["check_id"], identifier)
                self.assertEqual(checks[0]["state"], "pending")
                self.assertEqual(len(self.workflow.log("workflow")["entries"]), 1)
                raise ValueError("requirements outer")
        self.assertEqual(self.workflow.get("workflow")["revision"], 0)
        self.assertEqual(self.workflow.checks("workflow")["checks"], [])
        self.assertEqual(self.workflow.log("workflow")["entries"], [])

    def test_read_snapshot_and_readonly_mutations(self):
        self.store.db.execute("PRAGMA journal_mode=WAL")
        with self.native.Store(self.path) as reader:
            app = self.native.Application(reader)
            workflow, curation = (
                self.native.ItemWorkflow(app),
                self.native.Curation(app),
            )
            before = workflow.get("workflow")
            snapshot = curation.snapshot("workflow-file", {"item_id": "workflow"})
            self.workflow.note("workflow", "new write")
            self.curation.put("workflow-file", {"item_id": "workflow"})
            self.assertEqual(workflow.get("workflow"), before)
            self.assertEqual(
                curation.snapshot("workflow-file", {"item_id": "workflow"}), snapshot
            )
            self.assertEqual(curation.list()["proposals"], [])
            for action in (
                lambda: workflow.note("workflow", "forbidden"),
                lambda: curation.put("workflow-file", {"item_id": "workflow"}),
            ):
                with self.assertRaises(self.native.CatabolicError):
                    action()
        self.assertEqual(self.workflow.get("workflow")["revision"], 1)

    def test_closed_and_detached_services_reject(self):
        from catabolic_native.curation import occurrence

        actions = (
            lambda: self.workflow.get("workflow"),
            lambda: self.workflow.note("workflow", "forbidden"),
            lambda: self.curation.list(),
            lambda: self.curation.put("workflow-file", {"item_id": "workflow"}),
            lambda: occurrence(self.store, "default", "workflow-file"),
        )
        with self.store.detached():
            for action in actions:
                with self.assertRaises(self.native.CatabolicError):
                    action()
        self.assertEqual(self.workflow.get("workflow")["revision"], 0)
        self.store.close()
        for action in actions:
            with self.assertRaises(self.native.CatabolicError):
                action()

    def test_lost_transaction_blocks_workflow_and_curation(self):
        with self.assertRaisesRegex(ValueError, "outer"):
            with self.store.transaction() as db:
                with self.assertRaises(self.native.CatabolicError):
                    db.execute("INSERT OR ROLLBACK INTO profiles VALUES ('default')")
                for action in (
                    lambda: self.workflow.note("workflow", "forbidden"),
                    lambda: self.curation.put("workflow-file", {"item_id": "workflow"}),
                ):
                    with self.assertRaises(self.native.CatabolicError):
                        action()
                raise ValueError("outer")
        self.assertEqual(self.workflow.get("workflow")["revision"], 0)
        self.assertEqual(self.curation.list()["proposals"], [])

    def test_workflow_and_curation_signals_restore_same_owner(self):
        for table, action in (
            ("item_worklog", lambda: self.workflow.note("workflow", "interrupted")),
            (
                "proposals",
                lambda: self.curation.put("workflow-file", {"item_id": "workflow"}),
            ),
        ):
            with self.subTest(table=table):
                self.store.db.execute(
                    f"CREATE TRIGGER interrupt_workflow BEFORE INSERT ON {table} BEGIN "
                    "SELECT sum(x) FROM (WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL "
                    "SELECT x+1 FROM n WHERE x<100000000) SELECT x FROM n); END"
                )
                interrupted = threading.Event()

                def interrupt(event=interrupted):
                    time.sleep(0.1)
                    os.kill(os.getpid(), signal.SIGINT)
                    event.set()

                thread = threading.Thread(target=interrupt)
                thread.start()
                try:
                    with self.assertRaises(KeyboardInterrupt):
                        with self.store.transaction():
                            action()
                finally:
                    thread.join(5)
                self.assertTrue(interrupted.is_set())
                self.assertFalse(self.store.db.in_transaction)
                self.assertEqual(self.workflow.get("workflow")["revision"], 0)
                self.assertEqual(self.curation.list()["proposals"], [])
                self.store.db.execute("DROP TRIGGER interrupt_workflow")
        self.assertEqual(self.workflow.note("workflow", "usable")["revision"], 1)
        self.assertEqual(
            self.curation.put("workflow-file", {"item_id": "workflow"})["state"],
            "pending",
        )

    def test_workflow_and_curation_reject_cross_thread_scope(self):
        errors = []

        def worker():
            for action in (
                lambda: self.workflow.get("workflow"),
                lambda: self.workflow.note("workflow", "wrong thread"),
                lambda: self.curation.list(),
                lambda: self.curation.put("workflow-file", {"item_id": "workflow"}),
            ):
                try:
                    action()
                except self.native.CatabolicError as exc:
                    errors.append(str(exc))

        with self.store.transaction():
            thread = threading.Thread(target=worker)
            thread.start()
            thread.join(5)
            self.assertFalse(thread.is_alive())
            self.assertEqual(len(errors), 4)
            self.assertTrue(all("thread" in error for error in errors))
            self.assertEqual(self.workflow.get("workflow")["revision"], 0)
        self.assertEqual(self.curation.list()["proposals"], [])


# Each new preserved inventory row is reported separately. Selected-contract
# evidence never upgrades the frozen inventory to an exhaustive parity claim.
CONTRACTS = {}
for _class, _module in (("ItemWorkflow", "item_workflow"), ("Curation", "curation")):
    for _suffix in ("", ".__init__"):
        CONTRACTS[f"python:catabolic.{_module}.{_class}{_suffix}"] = {
            "tested": "Construction retains app, Store and profile; service calls reuse its native session",
            "gap": "Arbitrary legacy Application/Store duck types are not native session owners",
        }
for _method, _tested, _gap in (
    (
        "get",
        "Initial and revised status; outer/nested rollback; readonly snapshot; unknown item",
        "All processing/rule/artifact evidence combinations remain unqualified at this facade boundary",
    ),
    (
        "checks",
        "Review/file requirements; string cursor pagination; boolean limit rejection",
        "4 MiB page budget, 1000-row boundary, and every rule/job/artifact evidence combination remain unqualified",
    ),
    (
        "note",
        "Multiline Unicode; actor/kind/revision; stale and invalid revisions; rollback",
        "65536-byte note/255-byte actor exact limits and every error-order combination remain unqualified",
    ),
    (
        "set_status",
        "in_progress/complete; blockers; expected revision; recorded file/review evidence",
        "All statuses, recovery gates, eleven-blocker truncation, and rule/job/artifact completion combinations remain unqualified",
    ),
    (
        "log",
        "Decoded data; integer pagination; boolean cursor rejection",
        "4 MiB page budget and SQLite integer overflow categories remain unqualified",
    ),
    (
        "require",
        "Review/file targets; actor/note/optimistic revision; generated ID shape and rollback",
        "Job/artifact/proposal targets and exact 1000-requirement boundary remain unqualified",
    ),
    (
        "resolve",
        "Review completion; file manual-completion rejection; actor/note/revision",
        "Waived/pending transitions, rule requirements and all cross-profile cases remain unqualified",
    ),
):
    CONTRACTS[f"python:catabolic.item_workflow.ItemWorkflow.{_method}"] = {
        "tested": _tested,
        "gap": _gap,
    }
for _method, _tested, _gap in (
    (
        "snapshot",
        "Recorded file/association/item/identity values and readonly snapshot",
        "Component assertion snapshots and metadata/1000-association budget boundaries remain unqualified",
    ),
    (
        "put",
        "Decoded proposal/source/evidence; idempotent deterministic ID; invalid evidence/source; nested rollback",
        "Identify/component payloads, stale-source races and exact payload/snapshot budgets remain unqualified",
    ),
    (
        "get",
        "Decoded payload/evidence/snapshot/result; unknown proposal",
        "All historical schema/result variants and profile isolation remain unqualified",
    ),
    (
        "accept_existing_association_evidence",
        "Existing exact association; decoded accepted result; idempotence",
        "Staleness races, multiple matching associations and all source-refusal paths remain unqualified",
    ),
    (
        "list",
        "State filtering; string pagination; SQLite int/float/bool/text and falsy cursors; invalid state; readonly snapshot",
        "4 MiB boundary remains unqualified; native JSON transport does not accept blob/memoryview cursors, and SQLite error classes differ",
    ),
    (
        "decide",
        "Accept sidecar/reject; decoded results; idempotence; opposite-decision refusal",
        "Identify metadata merging, component assertions, staleness races and source-refusal paths remain unqualified",
    ),
    (
        "history",
        "Decoded before/after; integer pagination; legacy SQLite text cursor; invalid limit",
        "All SQLite scalar cursor types and overflow categories remain unqualified",
    ),
    (
        "sidecars",
        "Exact-stem subtitle preview/apply; location filtering; language evidence; SQLite int/float/bool/text and falsy cursors",
        "Ambiguous candidates, covers, 1000-primary scope bound and partial-page continuation remain unqualified; JSON transport excludes blob/memoryview cursors",
    ),
    (
        "expected_put",
        "Collection/definition result; item/identity members; deterministic snapshot ID",
        "Every malformed member, duplicate and 10000-member boundary remain unqualified",
    ),
    (
        "completeness",
        "Present/missing members; integer pagination; boolean offset rejection",
        "Ambiguous identities, unavailable/unknown/unidentified states and oversized offsets remain unqualified",
    ),
):
    CONTRACTS[f"python:catabolic.curation.Curation.{_method}"] = {
        "tested": _tested,
        "gap": _gap,
    }
CONTRACTS["python:catabolic.curation.occurrence"] = {
    "tested": "Recorded occurrence including source policy/volume; unknown file; closed/detached owner",
    "gap": "Native Store supports the current schema; pre-schema-20 legacy behavior and all source policy overrides remain unqualified",
}


if __name__ == "__main__":
    unittest.main(verbosity=2)
