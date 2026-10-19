#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Strict installed eight-row Reconciler facade qualification.

Only disposable metadata and empty output roots. Never runs reference apply or
recover, and never admits managed-output effects here: apply is called only
while a pending journal makes its plan unsafe. Publication parity (track C3)
is paired separately in tests/test_native_reconciler_publication.py.
"""

import argparse
import ast
import functools
import importlib.util
import json
import os
import signal
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


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
    raise RuntimeError("reconciler qualification requires Python assertions")

PREFIX = "python:catabolic.reconcile.Reconciler"
METHODS = ("__init__", "owner", "catalogs", "preview", "verify", "apply", "recover")
CONTRACTS = {PREFIX: "Persistent public class"}
CONTRACTS.update(
    {
        PREFIX + "." + name: "Pre-effect refusal witness only"
        if name in ("apply", "recover")
        else "Selected ordinary read facade contracts"
        for name in METHODS
    }
)
EXPECTED_GROUPS = (
    "identity",
    "owner",
    "catalogs",
    "unknown_catalogs",
    "unbound_preview",
    "unbound_verify",
    "empty_preview",
    "empty_verify",
    "unknown_preview",
    "unknown_verify",
    "invalid_removals",
    "invalid_percent",
    "invalid_order",
    "uncommitted",
    "inner_rollback",
    "outer_rollback",
    "alternate_owner",
    "alternate_preview",
    "alternate_verify",
    "all_preview",
    "all_verify",
    "readonly_preview",
    "readonly_verify",
    "closed_owner",
    "empty_roots",
)
GATE = "native Reconciler apply/recover is blocked: filesystem publication and recovery are not yet qualified"
WITNESSES = {}


def sibling(name):
    spec = importlib.util.spec_from_file_location(
        name, Path(__file__).with_name(name + ".py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def verify_signatures(rows):
    import catabolic_native.reconcile as module

    selected = {row["id"]: row for row in rows}
    tree = ast.parse(Path(module.__file__).read_text())
    cls = next(
        node
        for node in tree.body
        if isinstance(node, ast.ClassDef) and node.name == "Reconciler"
    )
    methods = [node for node in cls.body if isinstance(node, ast.FunctionDef)]
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
    from catabolic_native import Reconciler

    calls = {}
    for name in METHODS:
        original = getattr(Reconciler, name)
        key = PREFIX + "." + name

        def wrapped(*args, _original=original, _key=key, **kwargs):
            calls[_key] = calls.get(_key, 0) + 1
            return _original(*args, **kwargs)

        setattr(Reconciler, name, functools.wraps(original)(wrapped))
    return calls


def error(call):
    try:
        call()
    except Exception as exc:
        return {"type": type(exc).__name__, "message": str(exc)}
    raise AssertionError("expected an exception")


def owner_value(reconciler):
    value = reconciler.owner("does-not-have-to-exist")
    assert value["database_id"] == reconciler.store.database_id
    return {**value, "database_id": "<caller-database-id>"}


def scenario(root, engine):
    if engine == "reference":
        from catabolic.app import Application
        from catabolic.reconcile import Reconciler
        from catabolic.store import Store
    else:
        from catabolic_native import Application, Reconciler, Store
    database = root / (engine + ".db")
    output = root / (engine + "-empty-output")
    output.mkdir()
    Store.initialize(database)
    notify = object()
    with Store(database, writable=True) as store:
        app = Application(store)
        reconciler = Reconciler(app, notify_consumers=notify)
        result = {
            "identity": [
                reconciler.app is app,
                reconciler.store is store,
                reconciler.profile == app.profile,
                reconciler.notify_consumers is notify,
            ],
            "owner": owner_value(reconciler),
            "catalogs": reconciler.catalogs(None),
        }
        result["unknown_catalogs"] = error(lambda: reconciler.catalogs("missing"))
        result["unbound_preview"] = reconciler.preview()
        result["unbound_verify"] = reconciler.verify()
        app.bind("output", "global", str(output))
        result["empty_preview"] = [
            reconciler.preview(max_removals=0, max_removal_percent=12.5),
            reconciler.preview(max_removals=0, max_removal_percent=25),
        ]
        result["empty_verify"] = reconciler.verify()
        result["unknown_preview"] = error(lambda: reconciler.preview("missing"))
        result["unknown_verify"] = error(lambda: reconciler.verify("missing"))
        result["invalid_removals"] = [
            error(lambda v=v: reconciler.preview(max_removals=v))
            for v in (True, -1, 1.5, "3", [], {})
        ]
        result["invalid_percent"] = [
            error(lambda v=v: reconciler.preview(max_removal_percent=v))
            for v in (True, -1, 101, "3", float("nan"), float("inf"), [])
        ]
        result["invalid_order"] = error(
            lambda: reconciler.preview(
                "missing", max_removals=-1, max_removal_percent=-1
            )
        )

        class Rollback(Exception):
            pass

        try:
            with store.transaction() as db:
                db.execute("INSERT INTO catalogs(id) VALUES('outer')")
                result["uncommitted"] = [
                    reconciler.catalogs(None),
                    reconciler.preview("outer"),
                    reconciler.verify("outer"),
                ]
                db.execute(
                    "INSERT INTO journal(id,profile,catalog,path,kind) VALUES('staged','default','global','','prepare')"
                )
                result["uncommitted"].append(reconciler.preview())
                try:
                    with store.transaction() as nested:
                        nested.execute("INSERT INTO catalogs(id) VALUES('inner')")
                        assert "inner" in reconciler.catalogs(None)
                        raise Rollback()
                except Rollback:
                    pass
                result["inner_rollback"] = reconciler.catalogs(None)
                raise Rollback()
        except Rollback:
            pass
        result["outer_rollback"] = reconciler.catalogs(None)
        app.add_profile("other")
        alternate = Reconciler(Application(store, "other"), notify_consumers=False)
        result["alternate_owner"] = owner_value(alternate)
        result["alternate_preview"] = alternate.preview()
        result["alternate_verify"] = alternate.verify()
        with store.transaction() as db:
            db.execute("INSERT INTO catalogs(id) VALUES('zeta')")
            db.execute("INSERT INTO catalogs(id) VALUES('alpha')")
        result["all_preview"] = reconciler.preview(None)
        result["all_verify"] = reconciler.verify(None)
    result["closed_owner"] = owner_value(reconciler)
    with Store(database) as store:
        readonly = Reconciler(Application(store))
        result["readonly_preview"] = readonly.preview()
        result["readonly_verify"] = readonly.verify()
    result["empty_roots"] = list(output.iterdir())
    assert not result["empty_roots"]
    return result


class InstalledReconciler(unittest.TestCase):
    def setUp(self):
        from catabolic_native import Application, Reconciler, Store

        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.database = self.root / "fixture.db"
        self.output = self.root / "output"
        self.output.mkdir()
        Store.initialize(self.database)
        self.store = Store(self.database, writable=True)
        self.addCleanup(self.store.close)
        self.app = Application(self.store)
        self.app.bind("output", "global", str(self.output))
        self.reconciler = Reconciler(self.app)

    def test_public_reads_and_borrowed_identity(self):
        from catabolic_native.reconcile import Reconciler

        self.assertIsInstance(self.reconciler, Reconciler)
        self.assertIs(self.reconciler.store, self.store)
        self.assertIs(self.reconciler.app, self.app)
        self.assertEqual(
            self.reconciler.owner("global")["database_id"], self.store.database_id
        )
        self.assertEqual(self.reconciler.catalogs(None), ["global"])
        self.assertTrue(self.reconciler.preview()["safe"])
        self.assertFalse(self.reconciler.verify()["healthy"])
        self.assertEqual(list(self.output.iterdir()), [])

    def test_uncommitted_and_rollback_without_reopen(self):
        class Rollback(Exception):
            pass

        with self.assertRaises(Rollback):
            with self.store.transaction() as db:
                db.execute("INSERT INTO catalogs(id) VALUES('pending')")
                self.assertEqual(self.reconciler.catalogs("pending"), ["pending"])
                self.assertFalse(self.reconciler.preview("pending")["safe"])
                self.assertFalse(self.reconciler.verify("pending")["healthy"])
                raise Rollback()
        self.assertEqual(self.reconciler.catalogs(None), ["global"])

    def test_apply_recover_and_private_options_refuse_before_effects(self):
        def callback(*args):
            raise AssertionError("callback must not run")

        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO journal(id,profile,catalog,path,kind) VALUES('pending','default','global','','prepare')"
            )
        before = self.store.rows("SELECT * FROM journal")
        for operation in ("apply", "recover"):
            for request in (
                {},
                {"catalog": None, "disposable_fixture": True},
                {"catalog": "missing", "disposable_fixture": True},
            ):
                observed = error(
                    lambda operation=operation, request=request: self.store._call(
                        "reconcile", operation, request
                    )
                )
                self.assertEqual(observed, {"type": "CatabolicError", "message": GATE})
        # The public method publishes through the shared executor. A pending
        # journal makes the plan unsafe, so nothing is journaled or written
        # and the callback never runs. Recover would execute the pending
        # claim, so it is exercised only by the paired publication tests.
        plan = self.reconciler.apply(after_filesystem=callback)
        self.assertFalse(plan["safe"])
        self.assertEqual(plan["applied"], [])
        self.assertIn(
            "pending operations require recover", json.dumps(plan["blockers"])
        )
        WITNESSES["apply"] = {
            "classification": "publication-routed; unsafe pending-journal plan only",
            "blockers": plan["blockers"],
        }
        WITNESSES["recover"] = {
            "classification": "publication-routed; parity in tests/test_native_reconciler_publication.py",
        }
        self.assertEqual(self.store.rows("SELECT * FROM journal"), before)
        self.assertEqual(list(self.output.iterdir()), [])

    def test_readonly_and_closed_state(self):
        from catabolic_native import Application, Reconciler, Store

        self.store.close()
        self.assertEqual(
            self.reconciler.owner("global")["database_id"], self.store.database_id
        )
        observed = {
            name: error(lambda name=name: getattr(self.reconciler, name)("global"))
            for name in ("catalogs", "preview", "verify", "apply", "recover")
        }
        self.assertTrue(
            all(
                value["type"] == "CatabolicError" and "closed" in value["message"]
                for value in observed.values()
            )
        )
        WITNESSES["closed_native_errors_unpaired"] = observed
        with Store(self.database) as store:
            reconciler = Reconciler(Application(store))
            self.assertTrue(reconciler.preview()["safe"])
            self.assertFalse(reconciler.verify()["healthy"])
            self.assertEqual(reconciler.catalogs(None), ["global"])
            for method in (reconciler.apply, reconciler.recover):
                self.assertIn(
                    "filesystem publication requires a writer",
                    error(method)["message"],
                )

    def test_readonly_snapshot_remains_on_caller_store(self):
        from catabolic_native import Application, Reconciler, Store

        # Disposable metadata fixture: permit a second writer to commit while
        # the readonly caller deliberately keeps its existing snapshot.
        self.store.db.execute("PRAGMA journal_mode=WAL")
        self.store.close()
        with Store(self.database) as reader:
            reconciler = Reconciler(Application(reader))
            self.assertEqual(reconciler.catalogs(None), ["global"])
            with Store(self.database, writable=True) as writer:
                with writer.transaction() as db:
                    db.execute("INSERT INTO catalogs(id) VALUES('later')")
            self.assertEqual(reconciler.catalogs(None), ["global"])
            self.assertTrue(reconciler.preview()["safe"])
            self.assertFalse(reconciler.verify()["healthy"])
            self.assertEqual(reconciler.catalogs(None), ["global"])
        with Store(self.database) as fresh:
            self.assertEqual(
                Reconciler(Application(fresh)).catalogs(None), ["global", "later"]
            )

    def test_native_signal_exception_identity_and_owner_reuse(self):
        with self.store.transaction() as db:
            db.execute(
                "WITH RECURSIVE numbers(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM numbers WHERE n<100000) INSERT INTO catalogs(id) SELECT 'fixture-'||n FROM numbers"
            )
        for operation in ("catalogs", "preview", "verify"):
            expected = KeyboardInterrupt("reconciler-signal-" + operation)

            def handler(*args, expected=expected):
                raise expected

            previous = signal.signal(signal.SIGALRM, handler)
            try:
                signal.setitimer(signal.ITIMER_REAL, 0.001)
                with self.assertRaises(KeyboardInterrupt) as caught:
                    getattr(self.reconciler, operation)(None)
                self.assertIs(caught.exception, expected)
            finally:
                signal.setitimer(signal.ITIMER_REAL, 0)
                signal.signal(signal.SIGALRM, previous)
            self.assertEqual(self.reconciler.catalogs("global"), ["global"])
            self.assertTrue(self.reconciler.preview()["safe"])
        WITNESSES["signal"] = (
            "catalogs/preview/verify preserve the original Python KeyboardInterrupt object and release the same owner for reuse"
        )


EXPECTED_TESTS = (
    "test_public_reads_and_borrowed_identity",
    "test_uncommitted_and_rollback_without_reopen",
    "test_apply_recover_and_private_options_refuse_before_effects",
    "test_readonly_and_closed_state",
    "test_native_signal_exception_identity_and_owner_reuse",
    "test_readonly_snapshot_remains_on_caller_store",
)


def main():
    parser = argument_parser()
    clear_stale_report(parser=parser)
    args = parser.parse_args()
    qualification = sibling("native_facade_qualification")
    provenance = sibling("qualify_native_embedding")
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
    rows = qualification.inventory_rows(inventory, CONTRACTS, 8)
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
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(InstalledReconciler)
    result = sibling("native_qualification_results").run_required_suite(
        suite, {InstalledReconciler: EXPECTED_TESTS}, 6
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
            25,
        )
        assert typed(paired["reference"]["result"]) == typed(
            paired["native"]["result"]
        ), "paired Python scalar kinds differ"
        assert paired["reference"]["oracle"] == oracle
    coverage = []
    for row in rows:
        key = row["id"] + (".__init__" if row["signature"]["kind"] == "class" else "")
        assert calls.get(key, 0) > 0, ("uncalled installed row", key)
        gated = key.endswith((".apply", ".recover"))
        if not gated:
            assert paired["native"]["calls"].get(key, 0) > 0, (
                "uncalled paired row",
                key,
            )
        coverage.append(
            {
                "id": row["id"],
                "installed_calls": calls[key],
                "paired_calls": paired["native"]["calls"].get(key, 0),
                "qualification": "publication-routed; paired separately"
                if gated
                else "selected-read-contracts-with-gaps",
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
        "scope": "ordinary persistent Reconciler read adapter only; no publication/recovery parity",
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
            "apply/recover effect parity is paired in tests/test_native_reconciler_publication.py, not by this installed receipt",
            "Closed read error class remains native CatabolicError versus frozen AttributeError",
            "Fallback/live sources, hardlinks, filesystem safety, managed publication/recovery, all input coercions and platform/Python combinations remain unqualified",
            "No milestone or release qualification is implied by this adapter receipt",
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
