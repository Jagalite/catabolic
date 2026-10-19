#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Strict installed seven-row Publisher adapter qualification.

Only disposable scan sources and catalogs; no managed-output effects.
Narrow publication transport/transaction checks, not filesystem safety qualification. Gate witnesses are not parity.
"""

import argparse
import ast
import functools
import gc
import importlib.util
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch


def clear_stale_report(argv=None, parser=None):
    """Invalidate every canonical --report destination before fallible preflight.

    Only --report PATH and --report=PATH opt in; abbreviations are rejected by
    the full parser. Stop at -- and use that parser's option lexer to distinguish
    filename values (including version-specific negative numbers) from options.
    No argv parsing or project imports happen here. Bootstrap copies are checked
    against this implementation by the receipt-boundary tests.
    """
    if parser is None:
        parser = argparse.ArgumentParser(add_help=False, allow_abbrev=False)
        parser.add_argument("--report")
    arguments = sys.argv[1:] if argv is None else argv
    first_error = None
    for index, argument in enumerate(arguments):
        if argument == "--":
            break
        if argument == "--report":
            value = arguments[index + 1] if index + 1 < len(arguments) else ""
            if parser._parse_optional(value) is not None:
                continue
        elif argument.startswith("--report="):
            value = argument.partition("=")[2]
        else:
            continue
        if value:
            report = Path(value)
            for suffix in (None, ".attempt.json"):
                try:
                    target = report if suffix is None else report.with_suffix(suffix)
                    target.unlink(missing_ok=True)
                except (OSError, ValueError) as error:
                    if first_error is None:
                        first_error = error
    if first_error is not None:
        raise first_error


def argument_parser():
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument("--scenario", choices=("reference", "native"))
    parser.add_argument("--root", type=Path)
    for name in (
        "reference-root",
        "reference-python",
        "source-root",
        "wheel",
        "build-manifest",
        "inventory",
        "report",
    ):
        parser.add_argument("--" + name, type=Path)
    return parser


# CLI preflight precedes project imports and optimized-mode refusal.
# Library imports never inspect caller arguments or invalidate their receipts.
if __name__ == "__main__":
    clear_stale_report(parser=argument_parser())

if not __debug__ or os.environ.get("PYTHONOPTIMIZE"):
    raise RuntimeError("publisher qualification requires Python assertions")

PREFIX = "python:catabolic.scan_execution.Publisher"
METHODS = ("__init__", "initial_scopes", "directory", "checkpoint", "stop", "flush")
CONTRACTS = {PREFIX: "Persistent native publication owner"}
CONTRACTS.update(
    {
        PREFIX + "." + name: "Selected ordinary publication adapter contracts"
        for name in METHODS
    }
)
EXPECTED_GROUPS = (
    "identity",
    "initial",
    "initial_state",
    "root_unset",
    "filter_options",
    "root_set",
    "filter",
    "record_checkpoint",
    "scope_checkpoint",
    "elapsed_checkpoint",
    "buffered",
    "before_batch",
    "after_state",
    "one_atomic_batch",
    "persisted_scopes",
    "persisted_entry",
    "default_header",
    "snapshot_stable",
    "running_flush",
    "stop",
    "rollback",
    "retry",
    "production_hooks",
    "reusable_store",
)
EXPECTED_TESTS = (
    "test_selected_paired_contracts_execute",
    "test_callback_exception_identity_and_retained_owner",
    "test_reentrant_publisher_is_available_store_is_borrowed",
    "test_callback_schedule_remains_separate_from_ordinary_scan",
    "test_constructor_reads_same_owner_snapshot_and_detached_lifetime",
    "test_factory_failure_preserves_exception_and_releases_claim",
    "test_initial_snapshot_pages_remain_stable_after_rollback",
    "test_wrapped_callback_exception_identity",
    "test_explicit_missing_refuses_before_effects",
    "test_native_flush_failure_stays_native_scan_outcome",
    "test_child_directory_callback_preserves_error_provenance",
)
WITNESSES = {}


def sibling(name):
    spec = importlib.util.spec_from_file_location(
        name, Path(__file__).with_name(name + ".py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def verify_signatures(rows):
    import catabolic_native.scan_execution as module

    selected = {row["id"]: row for row in rows}
    tree = ast.parse(Path(module.__file__).read_text())
    cls = next(
        node
        for node in tree.body
        if isinstance(node, ast.ClassDef) and node.name == "Publisher"
    )
    methods = [node for node in cls.body if isinstance(node, ast.FunctionDef)]
    methods = [node for node in methods if node.name in METHODS]
    assert {node.name for node in methods} == set(METHODS)
    for node in methods:
        assert (
            ast.unparse(node.args)
            == selected[PREFIX + "." + node.name]["signature"]["arguments"]
        ), ("installed signature drift", node.name)


def typed(value):
    """Compare Python-visible scalar kinds as well as JSON-equivalent values."""
    if isinstance(value, dict):
        return {key: typed(item) for key, item in value.items()}
    if isinstance(value, list):
        return [typed(item) for item in value]
    return [type(value).__name__, value]


def trace():
    from catabolic_native.scan_execution import Publisher

    calls = {}
    for name in METHODS:
        original = getattr(Publisher, name)
        getter = isinstance(original, property)
        function = original.fget if getter else original
        key = PREFIX + "." + name

        def wrapped(*args, _original=function, _key=key, **kwargs):
            calls[_key] = calls.get(_key, 0) + 1
            return _original(*args, **kwargs)

        wrapped = functools.wraps(function)(wrapped)
        setattr(Publisher, name, property(wrapped) if getter else wrapped)
    return calls


def api(engine):
    if engine == "reference":
        from catabolic.app import Application
        from catabolic.scan_execution import PublicationStopped, Publisher
        from catabolic.store import Store
    else:
        from catabolic_native import Application, Store
        from catabolic_native.scan_execution import PublicationStopped, Publisher
    return Application, Store, Publisher, PublicationStopped


def interrupted_publisher(app, Publisher, **scan_options):
    held = []
    marker = KeyboardInterrupt("publisher fixture interruption")

    def pause(publisher):
        held.append(publisher)
        raise marker

    with patch.object(Publisher, "_flush", pause):
        try:
            app.scan("media", **scan_options)
        except KeyboardInterrupt as exc:
            assert exc is marker
        else:
            raise AssertionError("production scan did not enter Publisher._flush")
    assert len(held) == 1
    return held[0]


def state(p):
    return json.loads(
        json.dumps(
            dict(
                entries=p.entries,
                scopes=p.scopes,
                bytes=p.bytes,
                sequence=p.sequence,
                blocker=p.blocker,
            )
        )
    )


def scenario(root, engine):
    Application, Store, Publisher, PublicationStopped = api(engine)
    source = root / (engine + "-source")
    source.mkdir()
    (source / "one.txt").write_text("one")
    database = root / (engine + ".db")
    Store.initialize(database)
    result = {}
    with Store(database, writable=True) as store:
        app = Application(store)
        app.bind("source", "media", str(source))
        held = interrupted_publisher(app, Publisher, budgets={"batch_seconds": 60})
        # Seed a live disposable fixture claim after the intentional setup interruption.
        # This is fixture preparation, not a production claim/recovery API.
        with store.transaction() as db:
            db.execute(
                "UPDATE observation_jobs SET state='running',lease_until=? WHERE id=?",
                (time.time() + 3600, held.job["id"]),
            )
        p = Publisher(app, held.job, held.binding)
        result["identity"] = [
            p.app is app,
            p.app.store is store,
            p.job is held.job,
            p.binding is held.binding,
        ]
        result["initial"] = list(p.initial_scopes)
        result["initial_state"] = state(p)
        result["root_unset"] = p.root_identity
        result["filter_options"] = [p.extensions, p.include_regex, p.exclude_regex]
        p.root_identity = (source.stat().st_dev, source.stat().st_ino)
        result["root_set"] = p.root_identity == (
            source.stat().st_dev,
            source.stat().st_ino,
        )
        filter_job = dict(held.job)
        options = json.loads(filter_job["execution"])
        options.update(
            extensions=[".txt"], include_regex=["allowed"], exclude_regex=["deny"]
        )
        options["budgets"]["batch_records"] = 2
        filter_job["execution"] = json.dumps(options)
        filtered = Publisher(app, filter_job, held.binding)
        calls = []
        with patch.object(Publisher, "_flush", lambda owner: calls.append(owner.bytes)):
            for path in (
                "allowed.bin",
                "allowed.txt",
                "allowed-deny.txt",
                "allowed2.txt",
            ):
                filtered(dict(path=path, size=1, mtime_ns=2, device=3, inode=4))
            result["filter"] = state(filtered)
            filtered.checkpoint()
            result["record_checkpoint"] = len(calls)
            filtered.stop("done")
            filtered.directory("one", "pending", {})
            filtered.directory("two", "pending", {})
            result["scope_checkpoint"] = len(calls)
        options["budgets"]["batch_seconds"] = 0.001
        filter_job["execution"] = json.dumps(options)
        timed = Publisher(app, filter_job, held.binding)
        time.sleep(0.02)
        with patch.object(Publisher, "_flush", lambda owner: calls.append(owner.bytes)):
            timed.checkpoint()
        result["elapsed_checkpoint"] = len(calls)
        # Public multi-scope buffers are one owner/transaction, not a loop of batches.
        p.directory("alpha", "pending", {"note": "é"})
        p.directory("beta", "pending", {})
        p.directory("alpha", "pending", {"note": "updated"})
        p(dict(path="fixture.txt", size=7, mtime_ns=8, device=9, inode=10))
        p.checkpoint()
        result["buffered"] = state(p)
        result["before_batch"] = store.rows(
            "SELECT count(*) AS n FROM observation_batches"
        )[0]["n"]
        with store.detached():
            p.flush()
        result["after_state"] = state(p)
        result["one_atomic_batch"] = store.rows(
            "SELECT sequence,observed,bytes FROM observation_batches"
        )
        result["persisted_scopes"] = store.rows(
            "SELECT path,state,detail FROM observation_scopes ORDER BY path"
        )
        result["persisted_entry"] = store.rows(
            "SELECT f.path,o.size,o.mtime_ns,o.device,o.inode,o.status FROM files f JOIN observations o ON o.file_id=f.id"
        )
        result["default_header"] = store.rows(
            "SELECT status,reason,bytes_read FROM media_header_checks"
        )
        result["snapshot_stable"] = list(p.initial_scopes)
        with store.detached():
            p.directory("gamma", "running", {})
        result["running_flush"] = state(p)
        p.directory("zeta", "pending", {})
        p(dict(path="discarded.txt", size=1, mtime_ns=2, device=3, inode=4))
        p.stop("fixture_stop")
        result["stop"] = state(p)
        # Only narrow transaction rollback validation for the new multi-scope operation.
        p.directory("first", "pending", {})
        p.directory("reject", "pending", {})
        before = store.rows("SELECT count(*) AS n FROM observation_batches")[0]["n"]
        p(dict(path="rolled-back.txt", size=1, mtime_ns=2, device=3, inode=4))
        p(dict(path="invalid.txt", size=None, mtime_ns=2, device=3, inode=4))
        with store.detached():
            try:
                p.flush()
            except PublicationStopped:
                pass
            else:
                raise AssertionError("NOT NULL fixture did not roll back")
        result["rollback"] = [
            store.rows("SELECT count(*) AS n FROM observation_batches")[0]["n"]
            == before,
            store.rows(
                "SELECT path FROM observation_scopes WHERE path IN('first','reject')"
            )
            == [],
            sorted(p.scopes) == ["first", "reject"],
            store.rows(
                "SELECT path FROM files WHERE path IN('rolled-back.txt','invalid.txt')"
            )
            == [],
        ]
        p.stop("fixture_retry")
        p.directory("first", "pending", {})
        p.directory("reject", "pending", {})
        with store.detached():
            p.flush()
        result["retry"] = [
            p.sequence == before + 1,
            sorted(
                r["path"]
                for r in store.rows(
                    "SELECT path FROM observation_scopes WHERE path IN('first','reject')"
                )
            ),
        ]
    # Real consumer hooks, independent of the manually constructed Publisher.
    database = root / (engine + "-hooks.db")
    Store.initialize(database)
    with Store(database, writable=True) as store:
        app = Application(store)
        app.bind("source", "media", str(source))
        flushed, visited, identity = [], [], []
        original, directory = Publisher._flush, Publisher.directory

        def publish(p):
            had_entries = bool(p.entries)
            identity.append(p.app is app and p.app.store is store)
            original(p)
            with Store(database) as reader:
                rows = reader.rows(
                    "SELECT observed,complete FROM scans WHERE id=?",
                    (p.job["scan_id"],),
                )
                if had_entries:
                    flushed.append(rows[0]["observed"] > 0 and not rows[0]["complete"])

        def visit(p, path, status, detail):
            if status == "running":
                visited.append(path)
            return directory(p, path, status, detail)

        with (
            patch.object(Publisher, "_flush", publish),
            patch.object(Publisher, "directory", visit),
        ):
            report = app.scan("media", budgets={"batch_records": 1})
        result["production_hooks"] = [
            report["complete"],
            all(identity),
            bool(flushed),
            all(flushed),
            visited,
        ]
        result["reusable_store"] = store.rows("SELECT count(*) AS n FROM files")[0]["n"]
    return result


class InstalledPublisher(unittest.TestCase):
    def setUp(self):
        Application, Store, self.Publisher, _ = api("native")
        self.Store = Store
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.source = self.root / "source"
        self.source.mkdir()
        (self.source / "one.txt").write_text("one")
        self.database = self.root / "fixture.db"
        Store.initialize(self.database)
        self.store = Store(self.database, writable=True)
        self.addCleanup(self.store.close)
        self.app = Application(self.store)
        self.app.bind("source", "media", str(self.source))

    def test_selected_paired_contracts_execute(self):
        with tempfile.TemporaryDirectory() as root:
            result = scenario(Path(root), "native")
        self.assertTrue(all(result["rollback"]))
        self.assertEqual(result["retry"], [True, ["first", "reject"]])
        self.assertTrue(all(result["production_hooks"][:4]))

    def test_callback_exception_identity_and_retained_owner(self):
        for kind in (RuntimeError, KeyboardInterrupt, MemoryError):
            marker = kind("retained callback")
            retained = []
            original = self.Publisher._flush

            def interrupt(p, retained=retained, original=original, marker=marker):
                retained.append(p)
                original(p)
                raise marker

            with patch.object(self.Publisher, "_flush", interrupt):
                try:
                    self.app.scan("media", budgets={"batch_records": 1})
                except BaseException as exc:
                    self.assertIs(exc, marker)
                else:
                    self.fail("callback did not interrupt")
            self.assertIs(retained[0].app, self.app)
            self.assertEqual(retained[0].sequence, 1)
            self.assertEqual(self.store.rows("SELECT 1 AS n"), [{"n": 1}])
            marker.__traceback__ = None
            retained.clear()
            gc.collect()
            self.assertTrue(self.app.scan("media")["complete"])

    def test_reentrant_publisher_is_available_store_is_borrowed(self):
        from catabolic_native import CatabolicError

        original = self.Publisher._flush
        checked = []

        def nested(p):
            self.assertIs(p.app.store, self.store)
            with self.assertRaisesRegex(CatabolicError, "already in use"):
                self.store.rows("SELECT 1")
            # No Publisher lock is held at the Python boundary.
            self.assertIsInstance(p.entries, list)
            self.assertIsInstance(p.scopes, dict)
            original(p)
            checked.append(p.sequence)

        with patch.object(self.Publisher, "_flush", nested):
            self.assertTrue(self.app.scan("media")["complete"])
        self.assertTrue(checked)
        self.assertEqual(self.store.rows("SELECT 1 AS n"), [{"n": 1}])

    def test_callback_schedule_remains_separate_from_ordinary_scan(self):
        for name in ("a", "b"):
            child = self.source / name
            child.mkdir()
            (child / "child.txt").write_text("child")
        # Shared publication owner; callback scheduling remains separately unqualified.
        a = self.app._call("scan", location="media", budgets={"batch_records": 2})
        b = self.app.scan("media", budgets={"batch_records": 2})
        for key in (
            "complete",
            "observed",
            "published",
            "coverage",
            "media_headers",
        ):
            self.assertEqual(a["scans"][0][key], b["scans"][0][key], key)
        # Do not normalize the known callback/ordinary scheduling difference.
        self.assertEqual(a["scans"][0]["batches"], 11)
        self.assertEqual(b["scans"][0]["batches"], 8)
        for result in (a, b):
            evidence = result["observations"][0]
            rows = self.store.rows(
                "SELECT count(*) AS n,sum(bytes) AS bytes FROM observation_batches WHERE job_id=?",
                (evidence["id"],),
            )
            self.assertEqual(result["scans"][0]["batches"], rows[0]["n"])
            self.assertEqual(
                result["scans"][0]["progress"]["committed_bytes"], rows[0]["bytes"]
            )

    def test_constructor_reads_same_owner_snapshot_and_detached_lifetime(self):
        captured = interrupted_publisher(self.app, self.Publisher)

        class Abort(Exception):
            pass

        with self.assertRaises(Abort):
            with self.store.transaction() as db:
                db.execute(
                    "INSERT INTO observation_scopes VALUES (?,?,'running','{}',0)",
                    (captured.job["id"], "uncommitted"),
                )
                publisher = self.Publisher(self.app, captured.job, captured.binding)
                self.assertIn(
                    {"path": "uncommitted", "state": "pending", "detail": {}},
                    list(publisher.initial_scopes),
                )
                raise Abort()
        self.assertIn("uncommitted", [r["path"] for r in publisher.initial_scopes])
        self.assertEqual(
            self.store.rows(
                "SELECT path FROM observation_scopes WHERE path='uncommitted'"
            ),
            [],
        )
        with self.store.transaction() as db:
            db.execute(
                "UPDATE observation_jobs SET state='running',lease_until=? WHERE id=?",
                (time.time() + 3600, captured.job["id"]),
            )
        publisher.root_identity = captured.root_identity
        with self.store.detached():
            publisher.directory("later", "running", {})
        self.assertEqual(publisher.sequence, 1)
        self.assertEqual(
            self.store.rows("SELECT state FROM observation_scopes WHERE path='later'"),
            [{"state": "running"}],
        )

    def test_factory_failure_preserves_exception_and_releases_claim(self):
        marker = MemoryError("factory delivery failed")
        retained = []

        def fail(app, owner, job, binding):
            retained.append(owner)
            raise marker

        with patch.object(self.Publisher, "_from_native", fail):
            try:
                self.app.scan("media")
            except BaseException as exc:
                self.assertIs(exc, marker)
            else:
                self.fail("factory did not fail")
        self.assertEqual(
            self.store.rows("SELECT state,lease_until FROM observation_jobs"),
            [{"state": "failed", "lease_until": 0.0}],
        )
        self.assertEqual(json.loads(retained[0].call("state", "null"))["sequence"], 0)
        self.assertTrue(self.app.scan("media")["complete"])

    def test_initial_snapshot_pages_remain_stable_after_rollback(self):
        captured = interrupted_publisher(self.app, self.Publisher)

        class Abort(Exception):
            pass

        with self.assertRaises(Abort):
            with self.store.transaction() as db:
                for index in range(600):
                    db.execute(
                        "INSERT INTO observation_scopes VALUES (?,?,'running','{}',0)",
                        (captured.job["id"], f"row-{index}"),
                    )
                publisher = self.Publisher(self.app, captured.job, captured.binding)
                raise Abort()
        scopes = list(publisher.initial_scopes)
        self.assertEqual(len(scopes), 601)
        self.assertTrue(all(row["state"] == "pending" for row in scopes))
        self.store.close()
        self.assertEqual(list(publisher.initial_scopes), scopes)

    def test_wrapped_callback_exception_identity(self):
        for kind in (OSError, sqlite3.OperationalError):
            marker = kind("wrapped callback fixture")
            with patch.object(self.Publisher, "_flush", side_effect=marker):
                try:
                    self.app.scan("media")
                except BaseException as exc:
                    self.assertIs(exc, marker)
                else:
                    self.fail("original wrapped callback did not propagate")
            self.assertEqual(
                self.store.rows(
                    "SELECT state,lease_until FROM observation_jobs WHERE state='running'"
                ),
                [],
            )
            self.assertTrue(self.app.scan("media")["complete"])

    def test_explicit_missing_refuses_before_effects(self):
        from catabolic_native import CatabolicError

        gate = "native public Publisher explicit missing entries for non-generated sources are unqualified"
        captured = interrupted_publisher(self.app, self.Publisher)
        publisher = self.Publisher(self.app, captured.job, captured.binding)
        # Both a direct owner and an actual retained scanner owner refuse it.
        for owner in (publisher, captured):
            before = state(owner)
            with self.assertRaises(CatabolicError) as result:
                owner(
                    dict(
                        path="one.txt",
                        status="missing",
                        size=0,
                        mtime_ns=0,
                        device=0,
                        inode=0,
                    )
                )
            self.assertEqual(str(result.exception), gate)
            self.assertEqual(state(owner), before)
        self.assertEqual(self.store.rows("SELECT * FROM observation_batches"), [])
        self.assertEqual(self.store.rows("SELECT * FROM observations"), [])
        WITNESSES["explicit_missing"] = {
            "classification": "refusal-only; not parity",
            "message": gate,
        }

    def test_native_flush_failure_stays_native_scan_outcome(self):
        original = self.Publisher._flush

        def invalid(publisher):
            publisher(
                dict(path="invalid.txt", size=None, mtime_ns=2, device=3, inode=4)
            )
            original(publisher)

        with patch.object(self.Publisher, "_flush", invalid):
            report = self.app.scan("media")
        self.assertFalse(report["complete"])
        self.assertIn("NOT NULL", report["scans"][0]["blocker"])
        self.assertEqual(self.store.rows("SELECT * FROM observations"), [])
        self.assertEqual(self.store.rows("SELECT * FROM observation_batches"), [])
        self.assertTrue(self.app.scan("media")["complete"])

        def stopped(publisher):
            original(publisher)
            publisher.stop("fixture caller stop")

        with patch.object(self.Publisher, "_flush", stopped):
            report = self.app.scan("media")
        self.assertFalse(report["complete"])
        self.assertEqual(report["scans"][0]["blocker"], "fixture caller stop")

    def test_child_directory_callback_preserves_error_provenance(self):
        child = self.source / "child"
        child.mkdir()
        (child / "two.txt").write_text("two")
        original = self.Publisher._flush
        injected = []

        def invalid_child(publisher):
            if "child" in publisher.scopes and not injected:
                injected.append(publisher.scopes["child"][0])
                publisher(
                    dict(path="invalid.txt", size=None, mtime_ns=2, device=3, inode=4)
                )
            original(publisher)

        with patch.object(self.Publisher, "_flush", invalid_child):
            report = self.app.scan("media")["scans"][0]
        self.assertEqual(injected, ["pending"])
        self.assertFalse(report["complete"])
        self.assertIn("NOT NULL", report["blocker"])
        self.assertEqual(report["errors"], [report["blocker"]])
        self.assertNotIn("publisher callback failed", report["blocker"])
        self.assertEqual(
            self.store.rows("SELECT path FROM files WHERE path='invalid.txt'"), []
        )
        self.assertEqual(
            self.store.rows("SELECT state,lease_until FROM observation_jobs"),
            [{"state": "failed", "lease_until": 0.0}],
        )
        self.assertTrue(self.app.scan("media")["complete"])

        for kind in (
            RuntimeError,
            KeyboardInterrupt,
            OSError,
            sqlite3.OperationalError,
        ):
            marker = kind("child callback fixture")
            hits = []

            def interrupt_child(publisher, marker=marker, hits=hits):
                if "child" in publisher.scopes and not hits:
                    hits.append(publisher.scopes["child"][0])
                    raise marker
                original(publisher)

            with patch.object(self.Publisher, "_flush", interrupt_child):
                try:
                    self.app.scan("media")
                except BaseException as exc:
                    self.assertIs(exc, marker)
                else:
                    self.fail("original child callback exception did not propagate")
            self.assertEqual(hits, ["pending"])
            self.assertEqual(
                self.store.rows(
                    "SELECT id FROM observation_jobs WHERE state='running'"
                ),
                [],
            )
            self.assertTrue(self.app.scan("media")["complete"])


def main():
    parser = argument_parser()
    clear_stale_report(parser=parser)
    qualification = sibling("native_facade_qualification")
    provenance = sibling("qualify_native_embedding")
    args = parser.parse_args()
    if args.scenario:
        oracle = None
        calls = None
        if args.scenario == "reference":
            assert list(sys.version_info[:3]) == qualification.REFERENCE_VERSION
            oracle = provenance.reference_provenance(
                args.reference_root, qualification.FROZEN_COMMIT
            )
            sys.path.insert(0, str(args.reference_root / "src"))
        else:
            calls = trace()
        result = scenario(args.root, args.scenario)
        imports = (
            provenance.reference_imports(
                args.reference_root, oracle["tracked_source_sha256"]
            )
            if oracle
            else None
        )
        print(
            json.dumps(
                {"result": result, "oracle": oracle, "imports": imports, "calls": calls}
            )
        )
        return
    assert all(
        (
            args.reference_root,
            args.reference_python,
            args.source_root,
            args.wheel,
            args.build_manifest,
            args.inventory,
            args.report,
        )
    ), "all artifact/oracle/report options are required"
    assert not args.report.exists(), "receipt already exists; choose a fresh path"
    inventory = json.loads(args.inventory.read_text())
    rows = qualification.inventory_rows(inventory, CONTRACTS, 7)
    identity = qualification.NativeIdentity(args.source_root)
    fence = qualification.file_fence(__file__, args.inventory, args.build_manifest)
    runtime = qualification.reference_runtime(args.reference_python)
    artifact = qualification.strict_artifact(
        args.wheel, args.build_manifest, args.source_root
    )
    oracle = provenance.reference_provenance(
        args.reference_root, qualification.FROZEN_COMMIT
    )
    verify_signatures(rows)
    calls = trace()
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(InstalledPublisher)
    result = sibling("native_qualification_results").run_required_suite(
        suite, {InstalledPublisher: EXPECTED_TESTS}, 11
    )
    with tempfile.TemporaryDirectory() as directory:
        paired = {}
        for engine, interpreter in (
            ("reference", args.reference_python),
            ("native", Path(sys.executable)),
        ):
            completed = subprocess.run(
                [
                    str(interpreter),
                    "-I",
                    "-B",
                    str(Path(__file__).resolve()),
                    "--scenario",
                    engine,
                    "--root",
                    directory,
                    "--reference-root",
                    str(args.reference_root),
                ],
                cwd=directory,
                capture_output=True,
                text=True,
                timeout=90,
            )
            assert completed.returncode == 0, (
                engine,
                completed.stdout,
                completed.stderr,
            )
            paired[engine] = json.loads(completed.stdout)
        qualification.paired_groups(
            paired["reference"]["result"],
            paired["native"]["result"],
            EXPECTED_GROUPS,
            24,
        )
        assert typed(paired["reference"]["result"]) == typed(
            paired["native"]["result"]
        ), "paired Python scalar kinds differ"
        assert paired["reference"]["oracle"] == oracle
    coverage = []
    for row in rows:
        key = row["id"] + (".__init__" if row["signature"]["kind"] == "class" else "")
        assert calls.get(key, 0) > 0, ("uncalled installed row", key)
        assert paired["native"]["calls"].get(key, 0) > 0, (
            "uncalled paired row",
            key,
        )
        coverage.append(
            {
                "id": row["id"],
                "installed_calls": calls[key],
                "paired_calls": paired["native"]["calls"].get(key, 0),
                "qualification": "selected-publication-contracts-with-gaps",
            }
        )
    identity.verify()
    assert (
        qualification.strict_artifact(args.wheel, args.build_manifest, args.source_root)
        == artifact
    )
    assert (
        qualification.file_fence(__file__, args.inventory, args.build_manifest) == fence
    )
    assert qualification.reference_runtime(args.reference_python) == runtime
    assert (
        provenance.reference_provenance(
            args.reference_root, qualification.FROZEN_COMMIT
        )
        == oracle
    )
    report = {
        "format": 1,
        "scope": "ordinary persistent Publisher adapter; no broad filesystem safety qualification",
        "passed": True,
        "tests": result.testsRun,
        "successful_scenarios": result.successful_scenarios,
        "paired_groups": len(EXPECTED_GROUPS),
        "paired_group_names": list(EXPECTED_GROUPS),
        "coverage": coverage,
        "gate_and_lifecycle_witnesses": WITNESSES,
        "artifact": artifact,
        "oracle": oracle,
        "oracle_imports": paired["reference"]["imports"],
        "reference_runtime": runtime,
        **fence,
        "gaps": [
            "Explicit missing entries on non-generated sources refuse before buffering; not frozen parity",
            "Callback-backed scans retain their established per-scope batch schedule, separately from ordinary scans; exact full scan-report parity is not claimed",
            "Public buffer attributes are read-only snapshots; direct mutable-list/dict instrumentation is unsupported",
            "Legacy Python sqlite3 Store sharing, snapshot connection access and exhaustive input/error coercions are unqualified",
            "No broad filesystem safety, managed-output, crash-recovery, platform or milestone gate is closed",
        ],
    }
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(
        json.dumps(
            {
                "tests": result.testsRun,
                "paired_groups": len(EXPECTED_GROUPS),
                "inventory_rows": len(rows),
                "report": str(args.report),
            }
        )
    )


if __name__ == "__main__":
    main()
