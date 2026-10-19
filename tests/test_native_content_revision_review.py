# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Independent pure-revision adapter checks on disposable synthetic catalogs.

The installed checks require an explicit installed wheel interpreter. The fake
owner tests are source-adapter checks only, never wheel qualification evidence.
No source paths, credentials, authorization policy, or output owners are used.
"""

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch

from tests.test_rust_legacy_parser import PYTHON, REFERENCE, environment

ROOT = Path(__file__).resolve().parents[1]
FACADE = ROOT / "crates/catabolic-python/python/catabolic_native"


class RevisionForwardingReview(unittest.TestCase):
    def setUp(self):
        bytecode = patch.object(sys, "dont_write_bytecode", True)
        bytecode.start()
        self.addCleanup(bytecode.stop)
        namespace = "_revision_adapter_review"
        package = types.ModuleType(namespace)
        package.__path__ = [str(FACADE)]
        transport = types.ModuleType(namespace + ".store")

        class CatabolicError(RuntimeError):
            pass

        transport.CatabolicError = CatabolicError
        loaded = {namespace: package, namespace + ".store": transport}
        self.patched = patch.dict(sys.modules, loaded)
        self.patched.start()
        self.addCleanup(self.patched.stop)
        for name in ("access", "content_access"):
            spec = importlib.util.spec_from_file_location(
                namespace + "." + name, FACADE / (name + ".py")
            )
            module = importlib.util.module_from_spec(spec)
            sys.modules[spec.name] = module
            spec.loader.exec_module(module)
            setattr(self, name, module)
        self.CatabolicError = CatabolicError

    def test_exact_forwarding_and_result_identity(self):
        calls = []
        sentinel = object()

        class Owner:
            def _call(self, *args, **kwargs):
                calls.append((args, kwargs))
                return sentinel

            @property
            def path(self):
                raise AssertionError("adapter must not inspect or reopen a path")

        value = self.content_access.revision_of(Owner(), 'prof-雪"', "file-雪\n")
        self.assertIs(value, sentinel)
        self.assertEqual(
            calls,
            [
                (
                    (
                        "content_access",
                        "revision_of",
                        {"file_id": "file-雪\n"},
                        'prof-雪"',
                    ),
                    {},
                )
            ],
        )

    def test_only_exact_not_found_is_translated(self):
        for error in (
            self.CatabolicError("not_found"),
            self.CatabolicError("not_found: database unavailable"),
            self.CatabolicError("native store is closed"),
            self.CatabolicError("native store is detached"),
            KeyboardInterrupt("cancelled review call"),
            ValueError("not_found"),
        ):
            with self.subTest(error=repr(error)):

                class Owner:
                    def _call(self, *_, failure=error):
                        raise failure

                try:
                    self.content_access.revision_of(Owner(), "default", "missing")
                except BaseException as result:
                    if type(error) is self.CatabolicError and str(error) == "not_found":
                        self.assertIsInstance(result, self.access.AccessError)
                        self.assertEqual(
                            (result.args, result.code, result.status),
                            (("not_found",), "not_found", 404),
                        )
                        self.assertIsNone(result.__cause__)
                        self.assertTrue(result.__suppress_context__)
                    else:
                        self.assertIs(result, error)
                else:
                    self.fail("adapter swallowed an exception")


OWNER_PROBE = r"""
import importlib, json, os, sys, tempfile, threading
from pathlib import Path
if not __debug__ or os.environ.get("PYTHONOPTIMIZE"):
    raise SystemExit("revision owner checks require assertions; do not run with -O or -OO")
namespace = sys.argv[1]
if namespace == "catabolic":
    assert sys.version_info[:3] == (3, 14, 6)
    sys.path.insert(0, sys.argv[2])
Store = importlib.import_module(namespace + ".store").Store
CatabolicError = importlib.import_module(namespace + ".store").CatabolicError
revision_of = importlib.import_module(namespace + ".content_access").revision_of
AccessError = importlib.import_module(namespace + ".access").AccessError
paired, native = {}, []
with tempfile.TemporaryDirectory() as root:
    path = Path(root) / "catalog.db"
    Store.initialize(path)
    with Store(path, writable=True) as store:
        with store.transaction() as db:
            db.execute("INSERT INTO locations VALUES ('media')")
            db.execute("INSERT INTO files VALUES (?,?,?)", ("file-雪\n", "media", "quote-\"-雪"))
        assert not store.db.in_transaction
        first = revision_of(store, "default", "file-雪\n")
        assert not store.db.in_transaction
        try:
            revision_of(store, "default", "absent")
        except AccessError as error:
            assert (error.args, error.code, error.status) == (("not_found",), "not_found", 404)
        else:
            raise AssertionError("missing file succeeded")
        assert not store.db.in_transaction
        paired["writable-autocommit"] = first
        paired["unknown-string-profile"] = revision_of(store, "absent-雪", "file-雪\n")
        with store.transaction() as outer:
            outer.execute("UPDATE files SET path='outer' WHERE id=?", ("file-雪\n",))
            with store.transaction() as inner:
                inner.execute("UPDATE files SET path='inner-commit' WHERE id=?", ("file-雪\n",))
                nested = revision_of(store, "default", "file-雪\n")
            assert store.db.in_transaction
            try:
                revision_of(store, "default", "absent")
            except AccessError:
                pass
            else:
                raise AssertionError("missing file succeeded in a transaction")
            assert store.db.in_transaction
            assert revision_of(store, "default", "file-雪\n") == nested
        assert not store.db.in_transaction
        paired["nested-commit-and-error-reuse"] = revision_of(store, "default", "file-雪\n")
        assert paired["nested-commit-and-error-reuse"] == nested != first
        if namespace == "catabolic_native":
            with store.transaction():
                errors = []
                def wrong_thread():
                    try:
                        revision_of(store, "default", "absent")
                    except BaseException as error:
                        errors.append(error)
                worker = threading.Thread(target=wrong_thread)
                worker.start(); worker.join(10)
                assert not worker.is_alive() and len(errors) == 1
                assert isinstance(errors[0], CatabolicError) and not isinstance(errors[0], AccessError)
                assert "another thread" in str(errors[0])
                assert revision_of(store, "default", "file-雪\n") == nested
            native.append("transaction-thread-before-missing-lookup")
            try:
                with store.transaction() as db:
                    try:
                        db.execute("INSERT OR ROLLBACK INTO profiles VALUES ('default')")
                    except CatabolicError:
                        pass
                    else:
                        raise AssertionError("duplicate insert succeeded")
                    try:
                        revision_of(store, "default", "absent")
                    except CatabolicError as error:
                        assert not isinstance(error, AccessError)
                        assert "transaction was aborted" in str(error)
                    else:
                        raise AssertionError("revision ignored implicitly rolled-back scope")
                    raise LookupError("unwind poisoned scope")
            except LookupError:
                pass
            assert not store.db.in_transaction
            assert revision_of(store, "default", "file-雪\n") == nested
            native.append("implicit-rollback-before-missing-lookup-and-reuse")
    with Store(path) as reader:
        assert reader.db.in_transaction
        paired["readonly-transaction-preserved"] = revision_of(reader, "default", "file-雪\n")
        assert reader.db.in_transaction
print(json.dumps({"paired": paired, "native": native}, sort_keys=True))
"""


class RevisionProbeExecutionReview(unittest.TestCase):
    def test_optimized_owner_probe_is_refused_before_import(self):
        for flag in ("-O", "-OO"):
            run = subprocess.run(
                [sys.executable, flag, "-c", OWNER_PROBE, "catabolic_native"],
                capture_output=True,
                text=True,
                env=environment(overrides={"PYTHONDONTWRITEBYTECODE": "1"}),
                timeout=10,
            )
            self.assertNotEqual(run.returncode, 0)
            self.assertIn("do not run with -O or -OO", run.stderr)


class RevisionInstalledOwnerReview(unittest.TestCase):
    def test_disposable_owner_boundaries(self):
        native_python = Path(os.environ.get("CATABOLIC_NATIVE_PYTHON", sys.executable))
        results = {}
        with tempfile.TemporaryDirectory() as directory:
            env = environment(
                overrides={"HOME": directory, "PYTHONDONTWRITEBYTECODE": "1"}
            )
            for namespace, interpreter in (
                ("catabolic", PYTHON),
                ("catabolic_native", native_python),
            ):
                run = subprocess.run(
                    [
                        str(interpreter),
                        "-c",
                        OWNER_PROBE,
                        namespace,
                        str(REFERENCE / "src"),
                    ],
                    cwd=directory,
                    env=env,
                    capture_output=True,
                    text=True,
                    timeout=90,
                )
                self.assertEqual(run.returncode, 0, (namespace, run.stdout, run.stderr))
                self.assertEqual(run.stderr, "")
                results[namespace] = json.loads(run.stdout)
        self.assertEqual(
            results["catabolic"]["paired"], results["catabolic_native"]["paired"]
        )
        self.assertEqual(
            results["catabolic_native"]["native"],
            [
                "transaction-thread-before-missing-lookup",
                "implicit-rollback-before-missing-lookup-and-reuse",
            ],
        )


if __name__ == "__main__":
    unittest.main()
