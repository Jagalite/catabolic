# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native source observation and symlink journal qualification on disposable trees.

Python owns fixture setup and the independent reference oracle. Native mutations
run in subprocesses with no Python fallback. The imported assertion methods cover
only the native reconciliation calls; Python fixture scans are not counted as
native scanner coverage. Exact journals are exercised in both recovery directions.
"""

import hashlib
import json
import os
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from catabolic import scan_policy, source_trust
from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.store import Store
from tests import test_application as reference
from tests.test_rust_migration import BINARY


class NativeReconciler:
    def __init__(self, owner):
        self.owner = owner

    def call(self, action, catalog="global", **args):
        with self.owner.store.detached():
            return native(
                self.owner.database, "outputs", action, catalog=catalog, **args
            )

    def preview(self, catalog="global", **args):
        return self.call("preview", catalog, **args)

    def apply(self, catalog="global", **args):
        return self.call("apply", catalog, **args)

    def recover(self, catalog="global", **args):
        return self.call("recover", catalog, **args)

    def verify(self, catalog="global", **args):
        return self.call("verify", catalog, **args)


def native(database, surface, action, expected=0, **args):
    # Release builds deliberately ignore this debug-only safety-gate exception.
    # Every caller here owns a disposable TemporaryDirectory fixture.
    if surface == "outputs":
        args.setdefault("disposable_fixture", True)
    run = subprocess.run(
        [
            str(BINARY),
            "--db",
            str(database),
            *(
                ["operation", "watcher", action]
                if surface == "watcher"
                else [surface, action]
            ),
            "--options",
            json.dumps(args),
        ],
        capture_output=True,
        text=True,
        timeout=60,
    )
    if run.returncode != expected:
        if run.returncode == 2:
            raise CatabolicError(run.stderr.removeprefix("catabolic: ").strip())
        raise AssertionError((run.returncode, run.stdout, run.stderr))
    return json.loads(run.stdout) if run.stdout else None


@unittest.skipUnless(BINARY.is_file(), "build catabolic-cli before qualification")
class NativeReconciliationReferenceTest(unittest.TestCase):
    def setUp(self):
        reference.CatalogTest.setUp(self)
        self.reconciler = NativeReconciler(self)


# Execute unchanged reference assertions, without importing the reference TestCase
# itself into unittest discovery or counting Python-only fixture operations.
for name in (
    "end_to_end_and_repeat_preserves_source_and_link",
    "preview_is_read_only",
    "missing_without_scan_blocks_all_changes",
    "confirmed_missing_removes_link_and_restores_mapping",
    "changed_root_blocks_scan_and_sync",
    "missing_root_blocks_scan_and_sync",
    "source_symlink_is_not_followed",
    "source_changed_requires_rescan",
    "empty_source_is_blocked",
    "unknown_output_is_never_claimed",
    "all_catalogs_preflight_blocks_before_any_mutation",
    "wrong_ownership_blocks",
    "symlink_parent_blocks_without_touching_external_tree",
    "external_regular_file_is_not_replaced_or_removed",
    "external_retargeted_symlink_is_not_removed",
    "incomplete_download_suffix_is_never_projected",
):
    setattr(
        NativeReconciliationReferenceTest,
        "test_" + name,
        getattr(reference.CatalogTest, "test_" + name),
    )


@unittest.skipUnless(BINARY.is_file(), "build catabolic-cli before qualification")
class NativeObservationDifferentialTest(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.source = self.root / "source"
        self.source.mkdir()
        self.database = self.root / "native.sqlite3"
        Store.initialize(self.database)
        with Store(self.database, writable=True) as store:
            Application(store).bind("source", "media", str(self.source))
        self.reference = self.root / "python.sqlite3"
        shutil.copyfile(self.database, self.reference)

    def test_cancelling_one_request_preserves_shared_resume_and_claim_fence(self):
        (self.source / "child").mkdir()
        (self.source / "child/one.txt").write_text("shared inventory bytes")
        native(
            self.database,
            "observe",
            "scan",
            source="media",
            crash_after_scope="",
            expected=86,
        )
        with sqlite3.connect(self.database) as db:
            request, job = db.execute(
                "SELECT id,job_id FROM observation_requests"
            ).fetchone()
            old_claim = db.execute(
                "SELECT claim_token FROM observation_jobs WHERE id=?", (job,)
            ).fetchone()[0]
            sibling = "sibling-request"
            db.execute(
                "INSERT INTO observation_requests(id,profile,source,requester,guarantees,job_id,created_at,state) SELECT ?,profile,source,NULL,guarantees,job_id,created_at,'active' FROM observation_requests WHERE id=?",
                (sibling, request),
            )
        self.assertEqual(
            native(self.database, "observe", "cancel", request_id=request)["state"],
            "cancelled",
        )
        self.assertEqual(
            native(self.database, "observe", "cancel", request_id=request)["state"],
            "cancelled",
        )
        cancelled = native(self.database, "observe", "scan", request_id=request)
        self.assertEqual(cancelled["observations"][0]["state"], "cancelled")
        resumed = native(self.database, "observe", "scan", request_id=sibling)
        self.assertTrue(resumed["complete"], resumed)
        self.assertNotEqual(resumed["observations"][0]["claim_token"], old_claim)
        with sqlite3.connect(self.database) as db:
            self.assertEqual(
                db.execute(
                    "SELECT state FROM observation_requests WHERE id=?", (request,)
                ).fetchone()[0],
                "cancelled",
            )
            self.assertEqual(
                db.execute(
                    "SELECT state FROM observation_requests WHERE id=?", (sibling,)
                ).fetchone()[0],
                "active",
            )
            self.assertEqual(
                db.execute("SELECT status FROM observations").fetchone()[0], "present"
            )

    def both_scan(self, **args):
        with Store(self.reference, writable=True) as store:
            expected = Application(store).scan("media", **args)
        actual = native(self.database, "observe", "scan", source="media", **args)
        self.assertEqual(actual["complete"], expected["complete"])
        for key in (
            "observed",
            "published",
            "excluded",
            "extensions",
            "coverage",
            "deferred",
        ):
            self.assertEqual(actual["scans"][0][key], expected["scans"][0][key], key)
        self.assertEqual(self.snapshot(self.database), self.snapshot(self.reference))
        return actual

    def snapshot(self, path):
        with sqlite3.connect(path) as db:
            return {
                "files": db.execute("SELECT * FROM files ORDER BY id").fetchall(),
                "observations": db.execute(
                    "SELECT profile,file_id,size,mtime_ns,device,inode,status FROM observations ORDER BY profile,file_id"
                ).fetchall(),
                "headers": db.execute(
                    "SELECT profile,file_id,size,mtime_ns,device,inode,status,reason,bytes_read FROM media_header_checks ORDER BY profile,file_id"
                ).fetchall(),
            }

    def test_full_scan_id_and_header_state_matches_python(self):
        (self.source / "unicode café").mkdir()
        (self.source / "unicode café" / "video.mkv").write_bytes(b"media" * 32)
        (self.source / "zero.mkv").write_bytes(bytes(128))
        (self.source / "empty.txt").write_text("")
        (self.source / "unselected.nope").write_text("ignored")
        (self.source / "outside").symlink_to(self.root, target_is_directory=True)
        self.both_scan(budgets={"batch_records": 2})
        self.both_scan(budgets={"batch_records": 2})

    def test_partial_directory_absence_and_removed_subtree_match_python(self):
        for directory in ("costly", "healthy", "removed"):
            (self.source / directory).mkdir()
        for i in range(8):
            (self.source / "costly" / f"{i}.txt").write_text(str(i))
        (self.source / "healthy" / "gone.txt").write_text("old")
        (self.source / "removed" / "gone.txt").write_text("old")
        self.both_scan()
        (self.source / "costly" / "7.txt").unlink()
        (self.source / "healthy" / "gone.txt").unlink()
        (self.source / "healthy" / "new.txt").write_text("new")
        shutil.rmtree(self.source / "removed")
        self.both_scan(budgets={"directory_entries": 3, "batch_records": 2})
        self.both_scan()

    def test_exclusions_and_extension_filters_retain_old_evidence(self):
        (self.source / "retain.txt").write_text("retain")
        (self.source / "gone.mkv").write_bytes(b"video")
        self.both_scan()
        (self.source / "retain.txt").unlink()
        (self.source / "gone.mkv").unlink()
        self.both_scan(exclude=["retain.txt"])
        policy = {"extensions": [".txt"]}
        with Store(self.reference, writable=True) as store:
            expected = scan_policy.configure(
                Application(store), "media", policy, apply=True
            )
        actual = native(
            self.database,
            "observe",
            "scan_policy",
            source="media",
            policy=policy,
            apply=True,
        )
        self.assertEqual(actual, expected)
        self.both_scan()

    def test_source_policy_revision_and_history_matches_python(self):
        for policy, overrides in (
            ("path", {}),
            (None, {"inode": "check"}),
            ("strict", {}),
            ("strict", {}),
        ):
            with Store(self.reference, writable=True) as store:
                expected = source_trust.configure(
                    Application(store), "media", policy, overrides=overrides, apply=True
                )
            actual = native(
                self.database,
                "observe",
                "trust",
                source="media",
                policy=policy,
                overrides=overrides,
                apply=True,
            )
            # New timestamps are independently generated, format/window checked by storage tests.
            for value in (actual, expected):
                for row in value["history"]:
                    row.pop("created_at")
            self.assertEqual(actual, expected)

    def test_continuation_matches_completed_scope_retention_and_barrier(self):
        for folder in ("costly", "healthy"):
            (self.source / folder).mkdir()
        for i in range(8):
            (self.source / "costly" / f"{i}.txt").write_text(str(i))
        (self.source / "healthy" / "one.txt").write_text("one")
        first = self.both_scan(budgets={"directory_entries": 3})
        request = first["observations"][0]
        repeated = native(
            self.database,
            "observe",
            "scan",
            source="media",
            budgets={"directory_entries": 3},
        )
        self.assertEqual(repeated["observations"][0]["id"], request["id"])
        with Store(self.reference, writable=True) as store:
            old = store.rows(
                "SELECT id FROM observation_requests ORDER BY created_at DESC LIMIT 1"
            )[0]["id"]
            expected = Application(store).scan(continue_id=old, extended=True)
        actual = native(
            self.database,
            "observe",
            "scan",
            continue_id=request["request_id"],
            extended=True,
        )
        self.assertTrue(actual["complete"])
        self.assertEqual(
            actual["scans"][0]["coverage"], expected["scans"][0]["coverage"]
        )
        self.assertEqual(actual["observations"][0]["parent_job"], request["id"])
        self.assertEqual(
            actual["observations"][0]["guarantees"]["minimum_start"],
            request["guarantees"]["minimum_start"],
        )
        self.assertEqual(self.snapshot(self.database), self.snapshot(self.reference))

    def test_selected_continuation_preserves_other_deferred_scopes(self):
        for folder in ("one", "two"):
            (self.source / folder).mkdir()
            for i in range(6):
                (self.source / folder / f"{i}.txt").write_text(str(i))
        first = self.both_scan(budgets={"directory_entries": 3})
        with Store(self.reference, writable=True) as store:
            old = store.rows(
                "SELECT id FROM observation_requests ORDER BY created_at DESC LIMIT 1"
            )[0]["id"]
            expected = Application(store).scan(
                continue_id=old, extended=True, scopes=["one"]
            )
        actual = native(
            self.database,
            "observe",
            "scan",
            continue_id=first["observations"][0]["request_id"],
            extended=True,
            scopes=["one"],
        )
        self.assertFalse(actual["complete"])
        self.assertEqual(
            actual["scans"][0]["coverage"], expected["scans"][0]["coverage"]
        )
        self.assertEqual(self.snapshot(self.database), self.snapshot(self.reference))

    def test_dirty_interrupted_scan_readmits_before_publishing_missing(self):
        (self.source / "healthy").mkdir()
        (self.source / "healthy" / "one.txt").write_text("one")
        (self.source / "later").mkdir()
        vanished = self.source / "later" / "old.txt"
        vanished.write_text("old")
        native(self.database, "observe", "scan", source="media")
        vanished.unlink()
        native(self.database, "observe", "scan", source="media", crash_after_scope="healthy", expected=86)
        with sqlite3.connect(self.database) as db:
            request, old_job, old_scan = db.execute("SELECT r.id,j.id,j.scan_id FROM observation_requests r JOIN observation_jobs j ON j.id=r.job_id ORDER BY j.requested_at DESC LIMIT 1").fetchone()
            self.assertEqual(db.execute("SELECT o.status FROM observations o JOIN files f ON f.id=o.file_id WHERE f.path='later/old.txt'").fetchone()[0], "present")
        native(self.database, "observe", "dirty", source="media")
        result = native(self.database, "observe", "scan", request_id=request)
        self.assertTrue(result["complete"], result)
        with sqlite3.connect(self.database) as db:
            new_job, new_scan, dirty = db.execute("SELECT j.id,j.scan_id,j.dirty_generation FROM observation_requests r JOIN observation_jobs j ON j.id=r.job_id WHERE r.id=?", (request,)).fetchone()
            status, recorded_scan = db.execute("SELECT o.status,o.scan_id FROM observations o JOIN files f ON f.id=o.file_id WHERE f.path='later/old.txt'").fetchone()
            current_dirty = db.execute("SELECT dirty_generation FROM observation_sources WHERE profile='default' AND source='media'").fetchone()[0]
        self.assertNotEqual(new_job, old_job)
        self.assertNotEqual(new_scan, old_scan)
        self.assertEqual(dirty, current_dirty)
        self.assertEqual((status, recorded_scan), ("missing", new_scan))

    def test_continuation_refuses_active_parent_without_changing_inventory(self):
        (self.source / "one.txt").write_text("one")
        native(self.database, "observe", "scan", source="media", crash_after_scope="", expected=86)
        with sqlite3.connect(self.database) as db:
            request, job = db.execute("SELECT id,job_id FROM observation_requests").fetchone()
            state = db.execute("SELECT state,claim_token FROM observation_jobs WHERE id=?", (job,)).fetchone()
        self.assertEqual(state[0], "running")
        before = self.snapshot(self.database)
        with self.assertRaisesRegex(CatabolicError, "observation still active"):
            native(self.database, "observe", "scan", continue_id=request, extended=True)
        self.assertEqual(self.snapshot(self.database), before)
        with sqlite3.connect(self.database) as db:
            self.assertEqual(db.execute("SELECT state,claim_token FROM observation_jobs WHERE id=?", (job,)).fetchone(), state)

    def test_continuation_rejects_changed_policy_without_publishing_absence(self):
        for index in range(8):
            (self.source / f"{index}.txt").write_text(str(index))
        first = native(self.database, "observe", "scan", source="media", budgets={"directory_entries": 3})
        native(self.database, "observe", "scan_policy", source="media", policy={"extensions": [".txt"]}, apply=True)
        before = self.snapshot(self.database)
        with self.assertRaisesRegex(CatabolicError, "observation policy or binding changed"):
            native(self.database, "observe", "scan", continue_id=first["observations"][0]["request_id"], extended=True)
        self.assertEqual(self.snapshot(self.database), before)

    def test_continuation_rejects_new_dirty_generation(self):
        for i in range(8):
            (self.source / f"{i}.txt").write_text(str(i))
        first = native(
            self.database,
            "observe",
            "scan",
            source="media",
            budgets={"directory_entries": 3},
        )
        native(self.database, "observe", "dirty", source="media")
        with self.assertRaisesRegex(CatabolicError, "source changed since observation"):
            native(
                self.database,
                "observe",
                "scan",
                continue_id=first["observations"][0]["request_id"],
                extended=True,
            )

    def test_path_trust_continuation_rejects_replaced_root(self):
        native(
            self.database, "observe", "trust", source="media", policy="path", apply=True
        )
        for i in range(8):
            (self.source / f"{i}.txt").write_text(str(i))
        first = native(
            self.database,
            "observe",
            "scan",
            source="media",
            budgets={"directory_entries": 3},
        )
        self.source.rename(self.root / "old-source")
        self.source.mkdir()
        (self.source / "replacement.txt").write_text("replacement")
        before = self.snapshot(self.database)
        result = native(
            self.database,
            "observe",
            "scan",
            continue_id=first["observations"][0]["request_id"],
            extended=True,
        )
        self.assertFalse(result["complete"])
        self.assertEqual(result["scans"][0]["blocker"], "source_root_replaced")
        self.assertEqual(before, self.snapshot(self.database))

    def test_temporary_storage_budget_preserves_prior_inventory(self):
        (self.source / "one.txt").write_text("one")
        self.both_scan()
        (self.source / "one.txt").unlink()
        actual = self.both_scan(budgets={"temporary_bytes": 1})
        self.assertEqual(actual["scans"][0]["blocker"], "temporary_storage_budget")

    def test_read_preview_preserves_database_and_does_not_create_lock(self):
        lock = Path(str(self.database) + ".lock")
        lock.unlink(missing_ok=True)
        before = self.database.read_bytes()
        result = native(
            self.database,
            "observe",
            "trust",
            source="media",
            policy="path",
            apply=False,
        )
        self.assertFalse(result["applied"])
        self.assertEqual(before, self.database.read_bytes())
        self.assertFalse(lock.exists())


@unittest.skipUnless(BINARY.is_file(), "build catabolic-cli before qualification")
class NativeJournalInteropTest(unittest.TestCase):
    def setUp(self):
        reference.CatalogTest.setUp(self)

    def call(self, action, **args):
        with self.store.detached():
            return native(self.database, "outputs", action, **args)

    def test_python_crashed_claim_and_create_recover_natively(self):
        for kind in ("prepare", "create"):
            with self.subTest(kind=kind):

                def interrupt(operation, kind=kind):
                    if operation["kind"] == kind:
                        raise RuntimeError("simulated process loss")

                with self.assertRaisesRegex(RuntimeError, "process loss"):
                    self.reconciler.apply(after_filesystem=interrupt)
                self.call("recover")
        self.assertTrue(self.reconciler.verify()["healthy"])
        self.assertEqual(self.store.rows("SELECT * FROM journal"), [])

    def test_native_process_exit_at_durable_boundaries_recovers_in_python(self):
        original = hashlib.sha256(self.media.read_bytes()).hexdigest()
        # Claim is a separate durable boundary. Recovering it does not invent an
        # unjournaled creation; the next apply finishes the desired tree.
        self.call("apply", failpoint="after_journal", crash=True, expected=86)
        self.reconciler.recover()
        self.reconciler.apply()
        for stage in (
            "after_journal",
            "after_filesystem",
            "before_commit",
            "after_commit",
        ):
            with self.subTest(stage=stage):
                self.link.unlink()
                self.call("apply", failpoint=stage, crash=True, expected=86)
                self.reconciler.recover()
                self.reconciler.apply()
                self.assertTrue(self.reconciler.verify()["healthy"])
                self.assertEqual(self.store.rows("SELECT * FROM journal"), [])
                self.assertEqual(
                    hashlib.sha256(self.media.read_bytes()).hexdigest(), original
                )

    def test_native_removal_journal_recovered_by_python(self):
        self.call("apply")
        self.app.disable_mapping(self.mapping["id"])
        self.call("apply", failpoint="after_filesystem", crash=True, expected=86)
        self.assertFalse(self.link.is_symlink())
        self.reconciler.recover()
        self.assertTrue(self.reconciler.verify()["healthy"])
        self.assertEqual(self.store.rows("SELECT * FROM journal"), [])

    def test_python_resumes_native_guarded_scan_after_process_exit(self):
        (self.source / "healthy").mkdir()
        (self.source / "healthy" / "one.txt").write_text("one")
        (self.source / "later").mkdir()
        (self.source / "later" / "two.txt").write_text("two")
        with self.store.detached():
            native(
                self.database,
                "observe",
                "scan",
                source="media",
                crash_after_scope="healthy",
                expected=86,
            )
        row = self.store.rows(
            "SELECT id,state,scan_id FROM observation_jobs ORDER BY requested_at DESC LIMIT 1"
        )[0]
        self.assertEqual(row["state"], "running")
        request = self.store.rows(
            "SELECT id FROM observation_requests WHERE job_id=?", (row["id"],)
        )[0]["id"]
        result = self.app.scan(request_id=request)
        self.assertTrue(result["complete"], result)
        self.assertEqual(result["observations"][0]["scan_id"], row["scan_id"])
        self.assertEqual(
            {r["path"] for r in self.app.files()["files"]},
            {"example.mkv", "healthy/one.txt", "later/two.txt"},
        )

    def test_native_resumes_python_guarded_scan_after_interruption(self):
        from catabolic.scan_execution import Publisher

        (self.source / "healthy").mkdir()
        (self.source / "healthy" / "one.txt").write_text("one")
        (self.source / "later").mkdir()
        (self.source / "later" / "two.txt").write_text("two")
        original = Publisher._flush

        def interrupt(publisher):
            done = publisher.scopes.get("healthy", (None,))[0] == "complete"
            original(publisher)
            if done:
                raise KeyboardInterrupt()

        with patch.object(Publisher, "_flush", interrupt):
            with self.assertRaises(KeyboardInterrupt):
                self.app.scan("media")
        row = self.store.rows(
            "SELECT id,state,scan_id FROM observation_jobs ORDER BY requested_at DESC LIMIT 1"
        )[0]
        request = self.store.rows(
            "SELECT id FROM observation_requests WHERE job_id=?", (row["id"],)
        )[0]["id"]
        # Closing releases the Python observation guard as after_close requires.
        self.store.close()
        try:
            result = native(self.database, "observe", "scan", request_id=request)
        finally:
            self.store = Store(self.database, writable=True)
            self.app = Application(self.store)
            self.addCleanup(self.store.close)
        self.assertTrue(result["complete"], result)
        self.assertEqual(result["observations"][0]["scan_id"], row["scan_id"])
        self.assertEqual(
            {r["path"] for r in self.app.files()["files"]},
            {"example.mkv", "healthy/one.txt", "later/two.txt"},
        )

    def test_native_resumes_native_scan_with_same_scan_and_fresh_claim(self):
        (self.source / "healthy").mkdir()
        (self.source / "healthy" / "one.txt").write_text("one")
        (self.source / "later").mkdir()
        (self.source / "later" / "two.txt").write_text("two")
        with self.store.detached():
            native(
                self.database,
                "observe",
                "scan",
                source="media",
                crash_after_scope="healthy",
                expected=86,
            )
        row = self.store.rows(
            "SELECT id,scan_id,claim_token FROM observation_jobs ORDER BY requested_at DESC LIMIT 1"
        )[0]
        request = self.store.rows(
            "SELECT id FROM observation_requests WHERE job_id=?", (row["id"],)
        )[0]["id"]
        with self.store.detached():
            result = native(self.database, "observe", "scan", request_id=request)
        self.assertTrue(result["complete"], result)
        self.assertEqual(result["observations"][0]["scan_id"], row["scan_id"])
        self.assertNotEqual(
            result["observations"][0]["claim_token"], row["claim_token"]
        )

    def test_unqualified_destructive_symlink_paths_are_blocked_without_fixture_opt_in(
        self,
    ):
        self.reconciler.apply()
        before = self.link.lstat()
        with self.store.transaction() as db:
            db.execute("UPDATE mappings SET active=0")
        plan = self.call("preview", disposable_fixture=False)
        self.assertFalse(plan["safe"])
        self.assertIn(
            "concurrent external leaf-writer safety", plan["blockers"][0]["reason"]
        )
        result = self.call("apply", disposable_fixture=False)
        self.assertFalse(result["safe"])
        self.assertEqual(self.link.lstat(), before)
        self.assertEqual(self.store.rows("SELECT * FROM journal"), [])
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO journal VALUES('d0fe3e94-f0f5-44e9-87fd-1460e8917b09','default','global',?,'remove',NULL,?)",
                (self.mapping["path"], os.readlink(self.link)),
            )
        with self.assertRaisesRegex(
            CatabolicError, "concurrent external leaf-writer safety"
        ):
            self.call("recover", disposable_fixture=False)
        self.assertEqual(self.link.lstat(), before)
        self.assertEqual(len(self.store.rows("SELECT * FROM journal")), 1)

    def test_python_replacement_temporary_is_recovered_natively(self):
        self.reconciler.apply()
        second = self.source / "second.mkv"
        second.write_bytes(b"replacement source")
        self.app.scan()
        file_id = self.store.rows("SELECT id FROM files WHERE path='second.mkv'")[0][
            "id"
        ]
        with self.store.transaction() as db:
            db.execute(
                "UPDATE mappings SET file_id=? WHERE id=?",
                (file_id, self.mapping["id"]),
            )
        with patch(
            "catabolic.reconcile.os.replace", side_effect=OSError("interrupted rename")
        ):
            with self.assertRaisesRegex(OSError, "interrupted rename"):
                self.reconciler.apply()
        self.call("recover")
        self.assertEqual(self.link.resolve(), second)
        self.assertTrue(self.reconciler.verify()["healthy"])
        self.assertFalse(list(self.link.parent.glob(".catabolic-link-*")))

    def test_forged_journal_identity_cannot_escape_temporary_parent(self):
        self.reconciler.apply()
        outside = self.root / "keep"
        outside.write_text("keep")
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO journal VALUES(?,?,?,?,?,?,?)",
                (
                    "../../keep",
                    "default",
                    "global",
                    self.mapping["path"],
                    "replace",
                    "new",
                    os.readlink(self.link),
                ),
            )
        with self.assertRaisesRegex(
            CatabolicError, "invalid journal operation identity"
        ):
            self.call("recover")
        self.assertEqual(outside.read_text(), "keep")
        self.assertEqual(len(self.store.rows("SELECT * FROM journal")), 1)
