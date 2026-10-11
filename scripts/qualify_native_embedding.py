#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Installed facade acceptance, optionally paired with the frozen Python oracle.

Run with the interpreter where the wheel is installed, from outside the checkout:
  python -I qualify_native_embedding.py --inventory python-api.json --report proof.json
  ... --reference-root FROZEN_CHECKOUT --reference-python FROZEN_VENV/bin/python
No tests are skipped. Unsupported native contracts remain explicit coverage gaps.
"""

import argparse
import functools
import hashlib
import importlib.metadata
import importlib.util
import inspect
import json
import os
import shutil
import signal
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

if not __debug__:
    raise RuntimeError("embedding qualification requires Python assertions")


def scenario(root, engine, reference=None):
    if reference:
        sys.path.insert(0, str(Path(reference) / "src"))
    if engine == "reference":
        from catabolic.app import Application
        from catabolic.store import Store
    else:
        from catabolic_native import Application, Store
    root = Path(root)
    with Store(root / f"{engine}.db", writable=True) as store:
        app = Application(store)
        result = {
            "profile": app.add_profile("alternate"),
            "recovered": app.require_recovered(),
        }
        app.bind("source", "media", str(root / "source"))
        result["bind_output"] = app.bind(
            "output", "global", str(root / "output"), link_mode="hardlink"
        )
        result["bind_existing"] = app.bind("output", "global")
        result["mode"] = app.link_mode("global")
        binding = app.binding("source", "media")
        result["binding"] = [
            dict(binding),
            binding.volume_uuid,
            binding.policy,
            binding.trust_path,
            binding.evidence,
        ]
        with store.transaction() as db:
            db.execute(
                "INSERT INTO files(id,location,path) VALUES(?,?,?)",
                ("file-a", "media", "example.mkv"),
            )
            result["item"] = app.put_item(
                "movie",
                {"fixture": "one"},
                {"title": "Éxample", "year": 2026},
                "item-a",
                _db=db,
            )
            result["other"] = app.put_item(
                "movie", {"fixture": "two"}, {"title": "Second"}, "item-b"
            )
            result["associate"] = app.media.associate_in_transaction(
                db, "file-a", "item-a", role="primary", metadata={"note": "native"}
            )
            result["relate"] = app.media.relate(
                "item-b", "item-a", "edition_of", metadata={"x": 1}, _db=db
            )
        result["mapping"] = app.put_mapping(
            "global", "file-a", "item-a", "Movies/Example.mkv"
        )
        result["mapping_id"] = app.mapping_id(
            "global", "file-a", "item-a", "Movies/Example.mkv"
        )
        result["files"] = app.files()
        result["items"] = app.queries.items(sort="title", limit=1)
        result["page2"] = app.queries.items(
            sort="title", limit=1, cursor=result["items"]["next_cursor"]
        )
        result["item_get"] = app.queries.item("item-a")
        result["associations"] = app.queries.associations(item="item-a")
        result["relationships"] = app.queries.relationships(item="item-a")
        result["mappings"] = app.queries.mappings()
        tag = app.tags.put("genre:test", description="Native parity")
        result["tag"] = {k: v for k, v in tag.items() if k != "id"}
        app.tags.rename("genre:test", "genre:checked")
        app.tags.alias("genre:checked", "genre:alias")
        app.tags.put("group:all")
        app.tags.parent("genre:checked", "group:all")
        app.tags.assign(
            ["genre:alias"], items=["item-a"], confidence=0.9, note="parity"
        )
        tags = app.queries.tags()
        id_names = {row["id"]: row["name"] for row in tags["tags"]}

        def stable(value):
            if isinstance(value, dict):
                return {
                    k: stable(v)
                    for k, v in value.items()
                    if k not in ("created_at", "updated_at")
                }
            if isinstance(value, list):
                return [stable(v) for v in value]
            return id_names.get(value, value) if isinstance(value, str) else value

        result["tags"] = stable(tags)
        assignments = app.queries.taggings(item="item-a")
        for row in assignments["taggings"]:
            row.pop("id")
        result["taggings"] = stable(assignments)
        result["tag_filter"] = app.queries.items(tags=["genre:alias"])
        result["disable_mapping"] = app.disable_mapping(result["mapping"]["id"])
        result["disable_association"] = app.media.disable_association(
            result["associate"]
        )
        result["disable_relationship"] = app.media.disable_relationship(
            result["relate"]["id"]
        )
        try:
            with store.transaction() as db:
                db.execute("INSERT INTO profiles VALUES ('outer')")
                try:
                    with store.transaction() as nested:
                        nested.execute("INSERT INTO profiles VALUES ('inner')")
                        raise ValueError("rollback inner")
                except ValueError:
                    pass
                result["nested"] = store.rows(
                    "SELECT id FROM profiles WHERE id IN ('outer','inner') ORDER BY id"
                )
                raise ValueError("rollback outer")
        except ValueError:
            pass
        result["outer"] = store.rows(
            "SELECT id FROM profiles WHERE id IN ('outer','inner')"
        )
        result["typed"] = [
            list(row)
            for row in store.db.execute(
                "SELECT ? AS n, ? AS t, ? AS nullable",
                (9223372036854775807, "日本語", None),
            )
        ]
        result["blob"] = (
            store.db.execute("SELECT ? AS blob", (b"\x00\xff",)).fetchone()[0].hex()
        )
        callbacks = []
        store.after_reconnect["query"] = lambda: callbacks.append(
            store.rows("SELECT id FROM profiles WHERE id='default'")
        )
        with store.detached():
            assert store.db is None
        result["reconnect"] = callbacks
        result.update(
            sibling_module("native_workflow_embedding_tests").paired_workflows(
                app, root, engine
            )
        )
        status = app.status()
        status["database"] = "DATABASE"
        result["status"] = status
        store.after_close["once"] = lambda: callbacks.append("closed")
        store.close()
        store.close()
        result["close"] = callbacks[-1:]
    return result


class InstalledFacade(unittest.TestCase):
    def setUp(self):
        import catabolic_native as native

        self.native = native
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.path = self.root / "catalog.db"
        native.Store.initialize(self.path)

    def writer(self):
        store = self.native.Store(self.path, writable=True)
        self.addCleanup(store.close)
        return store

    def test_public_owner_transaction_and_types(self):
        store = self.writer()
        app = self.native.Application(store)
        self.assertNotIsInstance(store.db, sqlite3.Connection)
        self.assertEqual(type(store.db).__name__, "NativeCommandHandle")
        with store.transaction() as db:
            app.add_profile("outer")
            try:
                with store.transaction():
                    app.add_profile("inner")
                    raise KeyboardInterrupt()
            except KeyboardInterrupt:
                pass
            self.assertEqual(store.rows("SELECT id FROM profiles WHERE id='inner'"), [])
            row = db.execute(
                "SELECT :value AS value, :blob AS blob",
                {"value": 2**63 - 1, "blob": b"\x00\xff"},
            ).fetchone()
            self.assertEqual(row[0], 2**63 - 1)
            self.assertEqual(dict(row)["blob"], b"\x00\xff")
        with self.assertRaisesRegex(self.native.CatabolicError, "no longer active"):
            db.execute("SELECT 1")
        for sql in (
            "BEGIN",
            "COMMIT",
            "ROLLBACK",
            "SAVEPOINT bypass",
            "ATTACH ':memory:' AS bypass",
        ):
            with self.assertRaises(self.native.CatabolicError):
                store.db.execute(sql)
        with self.assertRaises(self.native.CatabolicError):
            store.db.execute("SELECT 1; SELECT 2")
        self.assertEqual(app.status()["profile"], "default")

    def test_readonly_snapshot_and_rejections(self):
        store = self.writer()
        store.db.execute("PRAGMA journal_mode=WAL")
        with self.native.Store(self.path) as reader:
            before = self.native.Application(reader).status()
            self.native.Application(store).put_item("movie", {}, {}, "later")
            self.assertEqual(self.native.Application(reader).status(), before)
            with self.assertRaisesRegex(self.native.CatabolicError, "read-only"):
                with reader.transaction():
                    pass
            with self.assertRaisesRegex(
                self.native.CatabolicError, "active transaction"
            ):
                with reader.detached():
                    pass
            with self.assertRaises(self.native.CatabolicError):
                self.native.Application(reader).add_profile("forbidden")
        with self.native.Store(self.path) as latest:
            self.assertEqual(
                self.native.Application(latest).status()["counts"]["items"], 1
            )

    def test_callbacks_detach_errors_and_identity(self):
        store = self.writer()
        called = []
        store.after_reconnect["read"] = lambda: called.extend(
            store.rows("SELECT id FROM profiles")
        )
        with self.assertRaisesRegex(ValueError, "body"):
            with store.detached():
                with self.native.Store(self.path, writable=True) as other:
                    self.native.Application(other).add_profile("other")
                raise ValueError("body")
        self.assertEqual({v["id"] for v in called}, {"default", "other"})
        replacement = self.root / "replacement.db"
        self.native.Store.initialize(replacement)
        with self.assertRaisesRegex(self.native.CatabolicError, "replaced"):
            with store.detached():
                os.replace(replacement, self.path)
        self.assertIsNone(store.db)
        with self.assertRaisesRegex(self.native.CatabolicError, "closed"):
            store.rows("SELECT 1")
        store.after_close["once"] = lambda: called.append("closed")
        store.close()
        store.close()
        self.assertEqual(called.count("closed"), 1)

    def test_failed_callback_and_reentrant_close(self):
        store = self.writer()

        def fail():
            self.assertEqual(store.rows("SELECT 1 AS n"), [{"n": 1}])
            raise ValueError("callback failed")

        store.after_reconnect["fail"] = fail
        with self.assertRaisesRegex(ValueError, "callback failed"):
            with store.detached():
                pass
        self.assertIsNone(store.db)
        store.after_close["reentrant"] = store.close
        store.close()

    def test_thread_affinity_busy_signal_and_gil_release(self):
        store = self.writer()
        errors = []
        with store.transaction():
            thread = threading.Thread(
                target=lambda: self._capture(errors, lambda: store.rows("SELECT 1"))
            )
            thread.start()
            thread.join(5)
            self.assertFalse(thread.is_alive())
            self.assertIn("another thread", str(errors.pop()))
        ticks = []

        def interrupt():
            time.sleep(0.08)
            self._capture(errors, lambda: store.rows("SELECT 1"))
            ticks.append(True)
            os.kill(os.getpid(), signal.SIGINT)

        thread = threading.Thread(target=interrupt)
        thread.start()
        with self.assertRaises(KeyboardInterrupt):
            store.rows(
                "WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<100000000) SELECT sum(x) FROM n"
            )
        thread.join(5)
        self.assertTrue(ticks, "SQL retained the GIL")
        self.assertIn("already in use", str(errors.pop()))
        self.assertEqual(store.rows("SELECT 1 AS n"), [{"n": 1}])
        # A Python signal handler can re-enter without a Rust-lock deadlock.
        handler_errors = []
        previous = signal.signal(
            signal.SIGALRM,
            lambda *_: self._capture(handler_errors, lambda: store.rows("SELECT 1")),
        )
        try:
            signal.setitimer(signal.ITIMER_REAL, 0.01)
            store.rows(
                "WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<100000) SELECT sum(x) FROM n"
            )
        finally:
            signal.setitimer(signal.ITIMER_REAL, 0)
            signal.signal(signal.SIGALRM, previous)
        self.assertTrue(handler_errors)
        self.assertIn("already in use", str(handler_errors[0]))

    @staticmethod
    def _capture(errors, action):
        try:
            action()
        except BaseException as error:
            errors.append(error)

    def test_native_services_scan_and_errors_restore_owner(self):
        store = self.writer()
        app = self.native.Application(store)
        source = self.root / "source"
        source.mkdir()
        (source / "test.mkv").write_bytes(b"fixture source\n")
        app.bind("source", "media", str(source))
        result = app.scan()
        self.assertTrue(result["complete"], result)
        self.assertEqual(result["unstarted_sources"], [])
        file = app.files()["files"][0]["id"]
        app.put_item("movie", {}, {"title": "Native"}, "item")
        association = app.media.associate(file, "item")
        app.tags.put("genre:movie")
        app.tags.assign(["genre:movie"], items=["item"])
        self.assertEqual(
            app.queries.items(tags=["genre:movie"])["items"][0]["id"], "item"
        )
        for action in (
            lambda: app.put_item("invalid", {}, {}, "bad"),
            lambda: app.queries.item("missing"),
            lambda: app.tags.assign(["missing"], items=["item"]),
        ):
            with self.assertRaises(self.native.CatabolicError):
                action()
            self.assertEqual(app.status()["profile"], "default")
        app.media.disable_association(association["id"])
        with self.assertRaisesRegex(self.native.CatabolicError, "omitted output root"):
            app.bind("output", "new-output")
        self.assertFalse((self.root / "new-output").exists())


def sibling_module(name):
    path = Path(__file__).resolve().with_name(name + ".py")
    spec = importlib.util.spec_from_file_location("_embedding_" + name, path)
    assert spec and spec.loader, f"missing qualification module: {path}"
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def trace_native_facade():
    """Count actual public facade calls without replacing native business logic."""
    import catabolic_native as native

    calls = {}

    def wrap(function, identifier):
        @functools.wraps(function)
        def traced(*args, **kwargs):
            calls[identifier] = calls.get(identifier, 0) + 1
            return function(*args, **kwargs)

        return traced

    for module, cls in (
        ("store", native.Store),
        ("app", native.Application),
        ("query", native.CatalogQuery),
        ("media_catalog", native.MediaCatalog),
        ("tagging", native.Tagging),
        ("item_workflow", native.ItemWorkflow),
        ("curation", native.Curation),
    ):
        for name, descriptor in list(vars(cls).items()):
            if name.startswith("_") and name not in (
                "__init__",
                "__enter__",
                "__exit__",
            ):
                continue
            classmethod_ = isinstance(descriptor, classmethod)
            function = descriptor.__func__ if classmethod_ else descriptor
            if not inspect.isfunction(function):
                continue
            identifier = f"python:catabolic.{module}.{cls.__name__}.{name}"

            wrapped = wrap(function, identifier)
            setattr(cls, name, classmethod(wrapped) if classmethod_ else wrapped)
    from catabolic_native import curation

    curation.occurrence = wrap(
        curation.occurrence, "python:catabolic.curation.occurrence"
    )
    return calls


def reference_provenance(root, expected):
    """Reject containing-repository inference, modified bytes and extra code."""
    root = root.resolve()

    def git(*args, input=None):
        return subprocess.check_output(
            ["git", "--no-optional-locks", "-C", str(root), *args], input=input
        )

    actual_root = Path(
        os.fsdecode(git("rev-parse", "--show-toplevel")).strip()
    ).resolve()
    assert actual_root == root, "frozen oracle must have its own Git root"
    head = git("rev-parse", "HEAD").decode().strip()
    assert head == expected, (
        f"frozen oracle HEAD {head} differs from inventory {expected}"
    )
    assert not git("status", "--porcelain", "--untracked-files=no"), (
        "frozen oracle tracked files are dirty"
    )
    tracked = {}
    for entry in git(
        "ls-tree", "-rz", "--full-tree", "HEAD", "--", "src/catabolic"
    ).split(b"\0"):
        if not entry:
            continue
        metadata, path = entry.split(b"\t", 1)
        mode, kind, digest = metadata.split()
        assert kind == b"blob" and mode in (b"100644", b"100755"), (
            "unsupported frozen oracle source entry"
        )
        relative = os.fsdecode(path)
        target = root / relative
        assert not target.is_symlink() and target.is_file(), (
            f"missing/linked frozen source: {relative}"
        )
        # Compare actual bytes to the committed object, even with assume-unchanged.
        data = target.read_bytes()
        assert data == git("cat-file", "blob", digest.decode()), (
            f"frozen source bytes changed: {relative}"
        )
        tracked[relative] = hashlib.sha256(data).hexdigest()
    assert "src/catabolic/app.py" in tracked and "src/catabolic/store.py" in tracked
    package_paths = list((root / "src/catabolic").rglob("*"))
    assert not any(path.is_symlink() for path in package_paths), (
        "frozen package contains symlink payloads"
    )
    actual = {
        str(path.relative_to(root))
        for path in package_paths
        if path.is_file() and "__pycache__" not in path.parts
    }
    assert actual == set(tracked), (
        "frozen oracle contains untracked/ignored package payloads"
    )
    return {"root": str(root), "head": head, "tracked_source_sha256": tracked}


def reference_imports(root, tracked):
    assert "catabolic_native" not in sys.modules, "frozen oracle imported native engine"
    imported = {}
    for name, module in sorted(sys.modules.items()):
        if name != "catabolic" and not name.startswith("catabolic."):
            continue
        path = Path(module.__file__).resolve()
        assert path.is_relative_to(root.resolve()), (
            f"oracle module escaped frozen root: {name}"
        )
        relative = str(path.relative_to(root.resolve()))
        assert relative in tracked, (
            f"oracle module is not frozen tracked source: {name}"
        )
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        assert digest == tracked[relative], f"oracle import bytes changed: {name}"
        imported[name] = {"file": relative, "sha256": digest}
    assert "catabolic.app" in imported and "catabolic.store" in imported
    return imported


EXPECTED_TESTS = {
    "InstalledFacade": (
        "test_public_owner_transaction_and_types",
        "test_readonly_snapshot_and_rejections",
        "test_callbacks_detach_errors_and_identity",
        "test_failed_callback_and_reentrant_close",
        "test_thread_affinity_busy_signal_and_gil_release",
        "test_native_services_scan_and_errors_restore_owner",
    ),
    "ReviewLifecycle": (
        "test_write_sigint_preserves_keyboard_interrupt",
        "test_nested_write_sigint_preserves_keyboard_interrupt",
        "test_nested_read_sigint_rolls_back_only_inner_scope",
        "test_caught_sqlite_rollback_retires_handle",
        "test_caught_sqlite_rollback_blocks_service_write",
        "test_swallowed_nested_rollback_cannot_commit_outer",
        "test_ordinary_statement_error_does_not_retire_valid_scope",
    ),
    "WorkflowLifecycle": (
        "test_transaction_rollback_and_failure_restores_owner",
        "test_read_snapshot_and_readonly_mutations",
        "test_closed_and_detached_services_reject",
        "test_lost_transaction_blocks_workflow_and_curation",
        "test_workflow_and_curation_signals_restore_same_owner",
        "test_workflow_and_curation_reject_cross_thread_scope",
    ),
}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--wheel", type=Path)
    parser.add_argument("--inventory", type=Path)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--reference-root", type=Path)
    parser.add_argument("--reference-python", type=Path)
    parser.add_argument("--reference-commit")
    parser.add_argument("--scenario", choices=("native", "reference"))
    parser.add_argument("--root", type=Path)
    args = parser.parse_args()
    if args.report:
        args.report.unlink(missing_ok=True)
    if args.scenario:
        before = (
            reference_provenance(args.reference_root, args.reference_commit)
            if args.scenario == "reference"
            else None
        )
        calls = trace_native_facade() if args.scenario == "native" else {}
        output = scenario(args.root, args.scenario, args.reference_root)
        imports = (
            reference_imports(args.reference_root, before["tracked_source_sha256"])
            if before
            else None
        )
        if before:
            assert (
                reference_provenance(args.reference_root, args.reference_commit)
                == before
            )
        print(
            json.dumps(
                {
                    "result": output,
                    "facade_calls": calls,
                    "oracle": before,
                    "oracle_imports": imports,
                },
                ensure_ascii=False,
                sort_keys=True,
            )
        )
        return
    import catabolic_native as native

    package = Path(native.__file__).resolve()
    dist = importlib.metadata.distribution("catabolic-native")
    assert package.is_relative_to(Path(dist.locate_file("")).resolve()), (
        "not an installed wheel"
    )
    wheel_hash = None
    installed = None
    if args.wheel:
        installed = sibling_module("qualify_native_wheel").verify_installed_wheel(
            args.wheel
        )
        wheel_hash = hashlib.sha256(args.wheel.read_bytes()).hexdigest()
    inventory = json.loads(args.inventory.read_text()) if args.inventory else None
    oracle = None
    if args.reference_root:
        if not args.reference_python or not inventory:
            parser.error(
                "--reference-python and --inventory are required with --reference-root"
            )
        oracle = reference_provenance(
            args.reference_root, inventory["reference_commit"]
        )
    calls = trace_native_facade()
    classes = (
        InstalledFacade,
        sibling_module("native_embedding_lifecycle_tests").ReviewLifecycle,
        sibling_module("native_workflow_embedding_tests").WorkflowLifecycle,
    )
    suite = unittest.TestSuite(
        unittest.defaultTestLoader.loadTestsFromTestCase(case) for case in classes
    )
    result = sibling_module("native_qualification_results").run_required_suite(
        suite,
        {case: EXPECTED_TESTS[case.__name__] for case in classes},
        expected_count=19,
    )
    installed_calls = calls.copy()
    paired_calls = {}
    parity = None
    if args.reference_root:
        if not args.reference_python:
            parser.error("--reference-python is required with --reference-root")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            native.Store.initialize(root / "seed.db")
            for engine in ("reference", "native"):
                shutil.copyfile(root / "seed.db", root / f"{engine}.db")
            (root / "source").mkdir()
            (root / "output").mkdir()
            outputs = {}
            for engine, python in (
                ("reference", args.reference_python),
                ("native", Path(sys.executable)),
            ):
                command = [
                    str(python),
                    "-I",
                    "-B",
                    "-X",
                    f"pycache_prefix={root / (engine + '-empty-pycache')}",
                    str(Path(__file__).resolve()),
                    "--scenario",
                    engine,
                    "--root",
                    str(root),
                ]
                if engine == "reference":
                    command += [
                        "--reference-root",
                        str(args.reference_root.resolve()),
                        "--reference-commit",
                        inventory["reference_commit"],
                    ]
                completed = subprocess.run(
                    command, cwd=directory, capture_output=True, text=True, timeout=90
                )
                assert completed.returncode == 0, (
                    engine,
                    completed.stdout,
                    completed.stderr,
                )
                payload = json.loads(completed.stdout)
                outputs[engine] = payload["result"]
                if engine == "reference":
                    assert payload["oracle"] == oracle, (
                        "oracle provenance changed in subprocess"
                    )
                    oracle["imported_modules"] = payload["oracle_imports"]
                else:
                    paired_calls = payload["facade_calls"]
                    for identifier, count in paired_calls.items():
                        calls[identifier] = calls.get(identifier, 0) + count
            assert outputs["reference"] == outputs["native"], json.dumps(
                {
                    key: [outputs["reference"].get(key), outputs["native"].get(key)]
                    for key in outputs["reference"]
                    if outputs["reference"][key] != outputs["native"].get(key)
                },
                indent=2,
            )
            assert reference_provenance(
                args.reference_root, inventory["reference_commit"]
            ) == {k: v for k, v in oracle.items() if k != "imported_modules"}
            parity = {
                "paired_scenario": "passed",
                "checked_results": len(outputs["native"]),
                "verified_oracle": oracle,
            }
    if args.wheel:
        assert (
            sibling_module("qualify_native_wheel").verify_installed_wheel(args.wheel)
            == installed
        ), "installed wheel changed during qualification"
    report = {
        "format": 1,
        "wheel_sha256": wheel_hash,
        "installed_wheel_verification": installed,
        "facade_calls": calls,
        "installed_acceptance_calls": installed_calls,
        "paired_scenario_calls": paired_calls,
        "installed_package": str(package),
        "python": sys.version,
        "acceptance_tests": result.testsRun,
        "successful_scenarios": result.successful_scenarios,
        "parity": parity,
        "files": {
            str(path.relative_to(package.parent)): hashlib.sha256(
                path.read_bytes()
            ).hexdigest()
            for path in sorted(package.parent.rglob("*"))
            if path.is_file() and "__pycache__" not in str(path)
        },
        "coverage": [],
    }
    if args.inventory:
        inventory = json.loads(args.inventory.read_text())
        report["inventory_sha256"] = hashlib.sha256(
            args.inventory.read_bytes()
        ).hexdigest()
        report["inventory_reference_commit"] = inventory["reference_commit"]
        report["verified_reference_commit"] = oracle["head"] if oracle else None
        for row in inventory["rows"]:
            if row["classification"] == "preserve-embedding-facade" or row[
                "id"
            ].startswith(
                (
                    "python:catabolic.query.CatalogQuery",
                    "python:catabolic.media_catalog.MediaCatalog",
                    "python:catabolic.tagging.Tagging",
                    "python:catabolic.item_workflow.ItemWorkflow",
                    "python:catabolic.curation.Curation",
                    "python:catabolic.curation.occurrence",
                )
            ):
                gap = (
                    "Creating a previously unbound omitted output root is explicitly unsupported; existing omitted roots and explicit roots are supported"
                    if row["id"].endswith("Application.bind")
                    else None
                )
                if row["id"].endswith("Application.scan"):
                    gap = "Fresh single-source scan is smoke-qualified; continuation, multi-source barriers, and scan cancellation remain unqualified at the persistent facade boundary"
                elif row["id"].endswith("Application.binding"):
                    gap = "Value, volume, policy, trust_path, and evidence attributes match; arbitrary legacy Python filesystem consumers are not qualified"
                elif row["id"].endswith(
                    ("Application.put_item", "MediaCatalog.relate")
                ):
                    gap = "Explicit _db accepts only a same-owner native handle; helper creates a nested native savepoint, unlike legacy _db's direct writes on caught errors"
                elif row["id"].endswith("Store.detached"):
                    gap = "Database identity is validated before reconnect callbacks; legacy invoked callbacks before its final replacement check"
                workflow_contract = sibling_module(
                    "native_workflow_embedding_tests"
                ).CONTRACTS.get(row["id"])
                if workflow_contract:
                    gap = workflow_contract["gap"]
                identifier = (
                    row["id"]
                    if row["signature"]["kind"] != "class"
                    else row["id"] + ".__init__"
                )
                count = calls.get(identifier, 0)
                report["coverage"].append(
                    {
                        "id": row["id"],
                        "classification": row["classification"],
                        "native_owner": "catabolic_store::embedding::Session / shared native service",
                        "facade": row["id"].replace(
                            "python:catabolic.", "catabolic_native."
                        ),
                        "qualification": "untested"
                        if not count
                        else "partial"
                        if gap
                        else "selected-contracts-tested",
                        "executed_calls": count,
                        "installed_acceptance_calls": installed_calls.get(
                            identifier, 0
                        ),
                        "paired_scenario_calls": paired_calls.get(identifier, 0),
                        "selected_contracts": workflow_contract["tested"]
                        if workflow_contract and paired_calls.get(identifier)
                        else "Installed lifecycle calls only; no frozen method-value comparison"
                        if workflow_contract and installed_calls.get(identifier)
                        else None,
                        "evidence": "not executed"
                        if not count
                        else "installed acceptance plus frozen/native scenario"
                        if installed_calls.get(identifier)
                        and paired_calls.get(identifier)
                        else "frozen/native scenario only"
                        if paired_calls.get(identifier)
                        else "installed acceptance only",
                        "gap": gap,
                    }
                )
    report["qualification_scripts_sha256"] = {
        name: hashlib.sha256(
            Path(__file__).resolve().with_name(name).read_bytes()
        ).hexdigest()
        for name in (
            "qualify_native_embedding.py",
            "qualify_native_wheel.py",
            "native_embedding_lifecycle_tests.py",
            "native_workflow_embedding_tests.py",
            "native_qualification_results.py",
        )
    }
    if args.report:
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(
        json.dumps(
            {
                "tests": result.testsRun,
                "paired_results": parity["checked_results"] if parity else None,
                "verified_reference_commit": oracle["head"] if oracle else None,
                "inventory_rows": len(report["coverage"]),
                "report": str(args.report) if args.report else None,
            }
        )
    )


if __name__ == "__main__":
    main()
