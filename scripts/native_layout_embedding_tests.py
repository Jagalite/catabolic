#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Fixed nine-row Layouts/plans/PRESETS installed artifact gate.

Only the ordinary persistent adapters are qualified here. No definition,
filesystem, publication, aggregate, default or release safety gate is closed.
"""

import argparse
import ast
import copy
import enum
import functools
import hashlib
import importlib
import importlib.util
import inspect
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import unittest
import zipfile
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
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--wheel", type=Path, help="Exact installed paired release wheel")
    mode.add_argument(
        "--dev-wheel",
        type=Path,
        help="Exact installed helperless development wheel; never release qualification",
    )
    parser.add_argument(
        "--source-root", type=Path, default=Path(__file__).resolve().parents[1]
    )
    parser.add_argument("--inventory", type=Path)
    parser.add_argument("--build-manifest", type=Path)
    parser.add_argument("--reference-root", type=Path)
    parser.add_argument("--reference-python", type=Path)
    parser.add_argument("--reference-commit")
    parser.add_argument("--report", type=Path)
    parser.add_argument("--scenario", choices=("reference", "native"))
    parser.add_argument("--root", type=Path)
    return parser


# CLI preflight precedes project imports and optimized-mode refusal.
# Library imports never inspect caller arguments or invalidate their receipts.
if __name__ == "__main__":
    clear_stale_report(parser=argument_parser())
if not __debug__ or os.environ.get("PYTHONOPTIMIZE"):
    raise SystemExit(
        "layout qualification requires assertions; do not run with -O or -OO or PYTHONOPTIMIZE"
    )

PREFIX = "python:catabolic."
CONTRACTS = {
    PREFIX + "layouts.Layouts" + suffix: dict(tested=tested, gap=gap)
    for suffix, tested, gap in (
        (
            "",
            "Installed public class and frozen signatures",
            "Opt-in namespace; foreign Python Store ownership is unsupported",
        ),
        (
            ".__init__",
            "App and persistent Store identity",
            "No new runtime or Store is created",
        ),
        (
            ".get",
            "Native definitions owner, errors, persistent and readonly values",
            "Exhaustive invalid definitions and SQLite exception categories remain unqualified",
        ),
        (
            ".list",
            "Sorted values, empty and uncommitted state",
            "Corrupt stored JSON categories remain unqualified",
        ),
        (
            ".put",
            "Native validation order, values, replacement, rollback",
            "Non-JSON Python inputs can fail serialization before semantic validation",
        ),
        (
            ".effective_definition",
            "Shared native overlay, profile isolation and mismatch order",
            "Historical schema breadth remains unqualified",
        ),
        (
            ".run",
            "Same-owner preview/apply, limit order, rollback, readonly, signal exceptions and reuse",
            "Native application has a private nested rollback scope; all existing filesystem gates remain unchanged; exhaustive layout semantics and cancellation timings remain unqualified",
        ),
    )
}
CONTRACTS[PREFIX + "plans.projection_digest"] = dict(
    tested="Native projection hash, fixed IDs, changed binding and lexical rollback",
    gap="Hash owner forwarded unchanged; exhaustive historical/corrupt configurations unqualified",
)
CONTRACTS[PREFIX + "layouts.PRESETS"] = dict(
    tested="Exact frozen preset values from native generator-owned resource",
    gap="Mutable import value is not a live mutation of the embedded native validator resource",
)
EXPECTED_GROUPS = (
    "presets",
    "attributes",
    "empty",
    "put",
    "get",
    "list",
    "invalid_names",
    "invalid_definitions",
    "invalid_limits",
    "lookup_order",
    "effective",
    "preview",
    "truncated",
    "apply",
    "state",
    "repeat",
    "nested_rollback",
    "outer_rollback",
    "digest_missing",
    "overlay",
    "overlay_mismatch",
    "alternate_profile",
    "digest",
    "digest_binding_change",
    "digest_rollback",
    "readonly",
    "option_representation",
)


def sibling(name):
    spec = importlib.util.spec_from_file_location(
        name, Path(__file__).with_name(name + ".py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def api(engine):
    namespace = "catabolic" if engine == "reference" else "catabolic_native"
    return tuple(
        importlib.import_module(namespace + "." + name)
        for name in ("store", "app", "layouts", "plans")
    )


def trace():
    _, _, layouts, plans = api("native")
    calls = {PREFIX + "layouts.PRESETS": 1}

    def wrap(fn, key):
        @functools.wraps(fn)
        def counted(*args, **kwargs):
            calls[key] = calls.get(key, 0) + 1
            return fn(*args, **kwargs)

        return counted

    for name in ("__init__", "get", "list", "put", "effective_definition", "run"):
        setattr(
            layouts.Layouts,
            name,
            wrap(getattr(layouts.Layouts, name), PREFIX + "layouts.Layouts." + name),
        )
    plans.projection_digest = wrap(
        plans.projection_digest, PREFIX + "plans.projection_digest"
    )
    return calls


def fixture(store):
    with store.transaction() as db:
        db.execute("INSERT INTO profiles VALUES ('alternate')")
        db.execute("INSERT INTO locations VALUES ('media')")
        db.execute("INSERT INTO catalogs VALUES ('readonly')")
        for i in range(3):
            db.execute(
                "INSERT INTO files VALUES (?,?,?)",
                (f"file{i}", "media", f"book{i}.txt"),
            )
            db.execute(
                "INSERT INTO items(id,kind,metadata) VALUES (?,?,?)",
                (f"item{i}", "book", "{}"),
            )
            db.execute(
                "INSERT INTO item_files(id,file_id,item_id,role,origin) VALUES (?,?,?,?,?)",
                (f"association{i}", f"file{i}", f"item{i}", "primary", "explicit"),
            )


def bind_fixture(store):
    with store.transaction() as db:
        db.execute(
            "INSERT INTO saved_queries(id,profile,name,revision,definition,digest) VALUES ('query','default','query',1,'{}','digest')"
        )
        db.execute(
            "INSERT INTO fallback_policies(id,profile,name,revision,definition,digest) VALUES ('policy','default','policy',1,'{}','digest')"
        )
        db.execute(
            "INSERT INTO projection_bindings VALUES ('default','global','query','flat')"
        )
        db.execute(
            "INSERT INTO fallback_bindings(profile,catalog,policy_id,query_id,layout) VALUES ('default','global','policy','query','flat')"
        )
        db.execute(
            "INSERT INTO bindings VALUES ('default','output','global','/synthetic-layout-output',1,2)"
        )


def failure(fn):
    try:
        fn()
    except Exception as error:
        return str(error)
    raise AssertionError("expected failure")


def state(store):
    values = {"mappings": store.rows("SELECT * FROM mappings ORDER BY id"), "meta": []}
    for row in store.rows(
        "SELECT key,value FROM meta WHERE key LIKE 'layout-catalog:%' OR key LIKE 'statistics:%' ORDER BY key"
    ):
        value = json.loads(row["value"])
        value.pop("evaluated_at", None)
        values["meta"].append(dict(key=row["key"], value=value))
    return values


class IntegerLimit(int):
    pass


class EnumLimit(enum.IntEnum):
    ONE = 1


class TruthFlag:
    def __init__(self, label, calls, value=False, error=None):
        self.label, self.calls, self.value, self.error = label, calls, value, error

    def __bool__(self):
        self.calls.append(self.label)
        if self.error is not None:
            raise self.error
        return self.value


def option_representation(layouts, store, presets):
    calls, result = [], {}
    for name, limit in (
        ("intenum", EnumLimit.ONE),
        ("subclass", IntegerLimit(1)),
        ("object", object()),
        ("zero", 0),
        ("huge", 10**5000),
    ):
        result[name] = failure(
            lambda limit=limit: layouts.run(
                "missing",
                limit=limit,
                apply=TruthFlag(
                    "unexpected",
                    calls,
                    error=AssertionError("must validate limit first"),
                ),
            )
        )
    assert calls == []
    result["unknown"] = failure(
        lambda: layouts.run(
            "missing",
            apply=TruthFlag("apply", calls),
            replace_layout=TruthFlag(
                "unused-replace", calls, error=AssertionError("unused")
            ),
            allow_empty=TruthFlag(
                "unused-empty", calls, error=AssertionError("unused")
            ),
        )
    )
    result["unknown_calls"] = calls[:]
    calls.clear()
    result["unchanged"] = layouts.run(
        "flat",
        apply=TruthFlag("apply", calls),
        replace_layout=TruthFlag(
            "unused-replace", calls, error=AssertionError("unused")
        ),
        allow_empty=TruthFlag("unused-empty", calls, error=AssertionError("unused")),
    )
    result["unchanged_calls"] = calls[:]
    calls.clear()
    try:
        with store.transaction():
            layouts.put("other", presets["flat"])
            result["replace"] = failure(
                lambda: layouts.run(
                    "other",
                    apply=TruthFlag("apply", calls),
                    replace_layout=TruthFlag("replace", calls),
                )
            )
            result["replace_calls"] = calls[:]
            calls.clear()
            definition = copy.deepcopy(presets["flat"])
            definition["selection"] = {
                "language": "sql",
                "query": "SELECT item_id FROM catalog_items WHERE 0",
            }
            layouts.put("flat", definition)
            result["empty"] = layouts.run(
                "flat",
                apply=TruthFlag("apply", calls),
                allow_empty=TruthFlag("empty", calls),
            )
            result["empty_calls"] = calls[:]
            calls.clear()
            result["allowed"] = layouts.run(
                "flat",
                apply=TruthFlag("apply", calls),
                allow_empty=TruthFlag("empty", calls, True),
            )
            result["allowed_calls"] = calls[:]
            raise RuntimeError("rollback option fixtures")
    except RuntimeError:
        pass
    return result


def scenario(root, engine):
    store_module, app_module, module, plans = api(engine)
    values = {"presets": copy.deepcopy(module.PRESETS)}
    with store_module.Store(root / (engine + ".db"), writable=True) as store:
        fixture(store)
        app = app_module.Application(store)
        layouts = module.Layouts(app)
        values["attributes"] = [layouts.app is app, layouts.store is store]
        values["empty"] = layouts.list()
        values["put"] = layouts.put("flat", copy.deepcopy(module.PRESETS["flat"]))
        values["get"] = layouts.get("flat")
        layouts.put("z", module.PRESETS["catabolic"])
        layouts.put(
            "blocked", {"version": 1, "rules": [{"name": "all", "path": "same.txt"}]}
        )
        values["list"] = layouts.list()
        values["invalid_names"] = [
            failure(lambda n=n: layouts.put(n, {})) for n in ("", "bad name", "x/y")
        ]
        values["invalid_definitions"] = [
            failure(lambda v=v: layouts.put("valid", v))
            for v in ({}, {"version": 1, "rules": []}, {"version": True, "rules": []})
        ]
        values["invalid_limits"] = [
            failure(lambda n=n: layouts.run("missing", "missing", apply=True, limit=n))
            for n in (False, True, 0, 1001, 1.0, "5", None)
        ]
        values["lookup_order"] = [
            failure(lambda: layouts.run("missing", "absent")),
            failure(lambda: layouts.run("flat", "absent")),
        ]
        values["effective"] = layouts.effective_definition("flat", "absent")
        values["preview"] = layouts.run("flat")
        values["truncated"] = layouts.run("flat", limit=1)
        values["apply"] = layouts.run("flat", apply=True)
        values["state"] = state(store)
        values["repeat"] = layouts.run("flat", apply=True)
        values["option_representation"] = option_representation(
            layouts, store, module.PRESETS
        )
        before = state(store)
        try:
            with store.transaction() as db:
                db.execute("UPDATE files SET path='outer.txt' WHERE id='file0'")
                try:
                    with store.transaction() as inner:
                        inner.execute(
                            "UPDATE files SET path='nested.txt' WHERE id='file1'"
                        )
                        values["nested_rollback"] = layouts.run("flat", apply=True)
                        raise RuntimeError("rollback nested")
                except RuntimeError:
                    pass
                values["outer_rollback"] = layouts.run("flat", apply=True)
                raise RuntimeError("rollback outer")
        except RuntimeError:
            pass
        assert state(store) == before
        values["digest_missing"] = failure(
            lambda: plans.projection_digest(app, "global")
        )
        bind_fixture(store)
        values["overlay"] = layouts.effective_definition("flat", "global")
        values["overlay_mismatch"] = failure(
            lambda: layouts.effective_definition("z", "global")
        )
        values["alternate_profile"] = module.Layouts(
            app_module.Application(store, "alternate")
        ).effective_definition("flat", "global")
        values["digest"] = plans.projection_digest(app, "global")
        try:
            with store.transaction() as db:
                db.execute(
                    "UPDATE bindings SET root='/synthetic-layout-changed' WHERE kind='output'"
                )
                values["digest_binding_change"] = plans.projection_digest(app, "global")
                assert values["digest_binding_change"] != values["digest"]
                raise RuntimeError("rollback binding")
        except RuntimeError:
            pass
        values["digest_rollback"] = plans.projection_digest(app, "global")
        assert values["digest_rollback"] == values["digest"]
    with store_module.Store(root / (engine + ".db")) as store:
        app = app_module.Application(store)
        layouts = module.Layouts(app)
        values["readonly"] = [
            layouts.get("flat"),
            layouts.list(),
            layouts.effective_definition("flat", "global"),
            plans.projection_digest(app, "global"),
            failure(lambda: layouts.put("flat", module.PRESETS["flat"])),
        ]
        values["readonly"].extend(
            [
                failure(lambda: layouts.run("missing", apply=True)),
                failure(lambda: layouts.run("flat", "absent", apply=True)),
                layouts.run("blocked", "readonly", apply=True),
            ]
        )

    assert set(values) == set(EXPECTED_GROUPS)
    return values


class InstalledLayouts(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.store_module, self.app_module, self.module, self.plans = api("native")
        self.path = self.root / "db"
        self.store_module.Store.initialize(self.path)
        self.store = self.store_module.Store(self.path, writable=True)
        self.addCleanup(self.store.close)
        fixture(self.store)
        self.app = self.app_module.Application(self.store)
        self.layouts = self.module.Layouts(self.app)
        self.layouts.put("flat", self.module.PRESETS["flat"])

    def test_values_and_mutations(self):
        self.assertIs(self.layouts.app, self.app)
        self.assertIs(self.layouts.store, self.store)
        self.assertEqual(
            self.layouts.get("flat")["definition"], self.module.PRESETS["flat"]
        )
        self.assertEqual(len(self.layouts.list()["layouts"]), 1)
        self.assertEqual(
            self.layouts.effective_definition("flat", "global"),
            self.module.PRESETS["flat"],
        )
        self.assertFalse(self.layouts.run("flat")["applied"])
        self.assertTrue(self.layouts.run("flat", apply=True)["applied"])
        self.assertEqual(self.layouts.run("flat", apply=True)["changes"], [])
        self.assertEqual(len(state(self.store)["mappings"]), 3)

    def test_limits_and_errors(self):
        for n in (False, True, 0, 1001, 1.0, "5", None):
            self.assertEqual(
                failure(lambda n=n: self.layouts.run("missing", apply=True, limit=n)),
                "limit must be between 1 and 1000",
            )
        self.assertIn("unknown layout", failure(lambda: self.layouts.get("missing")))
        self.assertEqual(self.layouts.run("flat", limit=1)["change_count"], 3)
        self.assertEqual(len(self.layouts.run("flat", limit=1)["changes"]), 1)

    def test_uncommitted_preview_apply_and_nested_rollback(self):
        with self.store.transaction() as outer:
            outer.execute("UPDATE files SET path='outer.txt' WHERE id='file0'")
            with self.assertRaisesRegex(RuntimeError, "nested"):
                with self.store.transaction() as inner:
                    inner.execute("UPDATE files SET path='nested.txt' WHERE id='file1'")
                    self.assertTrue(self.layouts.run("flat", apply=True)["applied"])
                    self.assertTrue(
                        any(
                            r["path"].endswith("nested.txt")
                            for r in state(self.store)["mappings"]
                        )
                    )
                    raise RuntimeError("nested")
            self.assertEqual(state(self.store)["mappings"], [])
            self.assertTrue(
                any(
                    r["path"].endswith("outer.txt")
                    for r in self.layouts.run("flat")["changes"]
                )
            )
        self.assertTrue(self.layouts.run("flat", apply=True)["applied"])

    def test_outer_rollback_including_definitions_statistics(self):
        before = state(self.store)
        with self.assertRaisesRegex(RuntimeError, "outer"):
            with self.store.transaction():
                self.layouts.put("new", self.module.PRESETS["flat"])
                self.layouts.run("new", apply=True)
                raise RuntimeError("outer")
        self.assertEqual(state(self.store), before)
        self.assertIn("unknown layout", failure(lambda: self.layouts.get("new")))
        self.assertTrue(self.layouts.run("flat", apply=True)["applied"])

    def test_binding_and_digest_are_same_owner(self):
        self.assertIn(
            "authoritative fallback",
            failure(lambda: self.plans.projection_digest(self.app, "global")),
        )
        bind_fixture(self.store)
        self.assertEqual(
            self.layouts.effective_definition("flat", "global")["selection"],
            {"query_id": "query", "profile": "default"},
        )
        digest = self.plans.projection_digest(self.app, "global")
        with self.assertRaises(RuntimeError):
            with self.store.transaction() as db:
                db.execute("UPDATE bindings SET root='/changed'")
                self.assertNotEqual(
                    self.plans.projection_digest(self.app, "global"), digest
                )
                raise RuntimeError("rollback")
        self.assertEqual(self.plans.projection_digest(self.app, "global"), digest)

    def test_readonly_and_closed_refusals(self):
        self.store.close()
        with self.store_module.Store(self.path) as store:
            layouts = self.module.Layouts(self.app_module.Application(store))
            self.assertFalse(layouts.run("flat")["applied"])
            self.assertIn("read-only", failure(lambda: layouts.run("flat", apply=True)))
            self.assertIn(
                "read-only",
                failure(lambda: layouts.put("other", self.module.PRESETS["flat"])),
            )
        self.assertIn("closed", failure(self.layouts.list))
        self.assertIn("closed", failure(lambda: self.layouts.run("flat")))

    def test_detached_refusal_and_reconnect(self):
        with self.store.detached():
            self.assertIn("detached", failure(self.layouts.list))
        self.assertEqual(self.layouts.get("flat")["name"], "flat")

    def test_replaced_store_refuses_stale_facade(self):
        replacement = self.root / "replacement"
        self.store_module.Store.initialize(replacement)
        with self.assertRaisesRegex(
            self.store_module.CatabolicError, "catalog replaced"
        ):
            with self.store.detached():
                os.replace(replacement, self.path)
        self.assertIn("closed", failure(self.layouts.list))
        self.assertIn("closed", failure(lambda: self.layouts.run("flat", apply=True)))

    def test_reconnect_callback_exception_identity(self):
        class Marker(Exception):
            pass

        marker = Marker("reconnect")

        def callback():
            raise marker

        self.store.after_reconnect["layout"] = callback
        try:
            with self.store.detached():
                pass
        except Marker as error:
            self.assertIs(error, marker)
        else:
            self.fail("callback exception was lost")
        self.assertIn("closed", failure(self.layouts.list))

    def test_signal_callback_identity_and_rollback_reuse(self):
        class Marker(BaseException):
            pass

        marker = Marker("layout signal")

        def callback(*_):
            raise marker

        definition = copy.deepcopy(self.module.PRESETS["flat"])
        definition["selection"] = {
            "language": "sql",
            "query": "WITH RECURSIVE x(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM x WHERE n<100000000) SELECT 'item0' AS item_id FROM x WHERE n=100000000",
            "timeout_ms": 60000,
        }
        self.layouts.put("slow", definition)
        old = signal.signal(signal.SIGALRM, callback)
        try:
            with self.assertRaises(Marker) as caught:
                with self.store.transaction() as db:
                    db.execute("INSERT INTO profiles VALUES ('rolled-back')")
                    signal.setitimer(signal.ITIMER_REAL, 0.03)
                    self.layouts.run("slow", apply=True)
            self.assertIs(caught.exception, marker)
        finally:
            signal.setitimer(signal.ITIMER_REAL, 0)
            signal.signal(signal.SIGALRM, old)
        self.assertEqual(state(self.store)["mappings"], [])
        self.assertEqual(
            self.store.rows("SELECT * FROM profiles WHERE id='rolled-back'"), []
        )
        self.assertTrue(self.layouts.run("flat", apply=True)["applied"])

    def test_typed_limit_and_deferred_flag_callbacks(self):
        self.layouts.run("flat", apply=True)
        result = option_representation(self.layouts, self.store, self.module.PRESETS)
        self.assertEqual(result["unknown_calls"], ["apply"])
        self.assertEqual(result["unchanged_calls"], ["apply"])
        self.assertEqual(result["replace_calls"], ["apply", "replace"])
        self.assertEqual(result["empty_calls"], ["apply", "empty"])
        self.assertEqual(result["allowed_calls"], ["apply", "empty"])
        self.assertFalse(result["empty"]["safe"])
        self.assertTrue(result["allowed"]["safe"])

    def test_flag_exception_identity_and_rollback(self):
        class Marker(BaseException):
            pass

        marker, calls = Marker("truth callback"), []
        for options in ({"apply": TruthFlag("apply", calls, error=marker)},):
            with self.assertRaises(Marker) as caught:
                self.layouts.run("missing", **options)
            self.assertIs(caught.exception, marker)
        self.layouts.run("flat", apply=True)
        self.layouts.put("other", self.module.PRESETS["flat"])
        before = state(self.store)
        with self.assertRaises(Marker) as caught:
            self.layouts.run(
                "other",
                apply=True,
                replace_layout=TruthFlag("replace", calls, error=marker),
            )
        self.assertIs(caught.exception, marker)
        self.assertEqual(state(self.store), before)
        definition = copy.deepcopy(self.module.PRESETS["flat"])
        definition["selection"] = {
            "language": "sql",
            "query": "SELECT item_id FROM catalog_items WHERE 0",
        }
        self.layouts.put("flat", definition)
        with self.assertRaises(Marker) as caught:
            self.layouts.run(
                "flat", apply=True, allow_empty=TruthFlag("empty", calls, error=marker)
            )
        self.assertIs(caught.exception, marker)
        self.assertEqual(state(self.store), before)
        self.layouts.put("flat", self.module.PRESETS["flat"])
        self.assertTrue(self.layouts.run("flat", apply=True)["applied"])
        self.assertEqual(calls, ["apply", "replace", "empty"])

    def test_no_helper_or_python_engine_forwarding(self):
        from unittest.mock import patch

        import catabolic_native

        with patch.object(
            catabolic_native,
            "execute_operation",
            side_effect=AssertionError("path reopen"),
        ):
            self.layouts.list()
            self.layouts.run("flat", apply=True)
            self.assertIn(
                "authoritative",
                failure(lambda: self.plans.projection_digest(self.app, "global")),
            )
        self.assertFalse(
            any(n == "catabolic" or n.startswith("catabolic.") for n in sys.modules)
        )


EXPECTED_TESTS = (
    "test_values_and_mutations",
    "test_limits_and_errors",
    "test_uncommitted_preview_apply_and_nested_rollback",
    "test_outer_rollback_including_definitions_statistics",
    "test_binding_and_digest_are_same_owner",
    "test_readonly_and_closed_refusals",
    "test_detached_refusal_and_reconnect",
    "test_replaced_store_refuses_stale_facade",
    "test_reconnect_callback_exception_identity",
    "test_signal_callback_identity_and_rollback_reuse",
    "test_no_helper_or_python_engine_forwarding",
    "test_typed_limit_and_deferred_flag_callbacks",
    "test_flag_exception_identity_and_rollback",
)


def installed_artifact(wheel):
    import catabolic_native

    package = Path(catabolic_native.__file__).resolve().parent
    checkout = Path(__file__).resolve().parents[1]
    assert not package.is_relative_to(checkout), (
        "installed qualification must run outside checkout"
    )
    with zipfile.ZipFile(wheel) as archive:
        assert "catabolic_native/native-helper.json" not in archive.namelist(), (
            "development wheel must be helperless"
        )
        assert "catabolic_native/bin/catabolic-native" not in archive.namelist(), (
            "development wheel must be helperless"
        )
        expected = {
            name.removeprefix("catabolic_native/"): hashlib.sha256(
                archive.read(name)
            ).hexdigest()
            for name in archive.namelist()
            if name.startswith("catabolic_native/") and not name.endswith("/")
        }
    actual = {
        str(p.relative_to(package)): hashlib.sha256(p.read_bytes()).hexdigest()
        for p in package.rglob("*")
        if p.is_file() and "__pycache__" not in p.parts
    }
    assert expected and actual == expected, (
        "installed package differs from exact development wheel"
    )
    return {
        "wheel_sha256": hashlib.sha256(wheel.read_bytes()).hexdigest(),
        "package_sha256": actual,
        "package_path": str(package),
    }


def build_identity(manifest_path, artifact):
    assert manifest_path is not None, "qualification requires --build-manifest"
    root = Path(__file__).resolve().parents[1]
    source = sibling("build_native_wheel").source_digest(root)
    manifest = json.loads(manifest_path.read_text())
    extensions = {
        name: digest
        for name, digest in artifact["package_sha256"].items()
        if name.startswith("_native.") and name.endswith((".so", ".pyd"))
    }
    assert len(extensions) == 1, "expected exactly one installed native extension"
    expected = {
        "profile": "development",
        "source_sha256": source,
        "source_sha256_after": source,
        "wheel_sha256": artifact["wheel_sha256"],
        "extension_sha256": next(iter(extensions.values())),
    }
    for key, value in expected.items():
        assert manifest.get(key) == value, ("build identity mismatch", key)
    return {
        **expected,
        "build_manifest_sha256": hashlib.sha256(manifest_path.read_bytes()).hexdigest(),
    }


def installed_signatures(rows):
    def arguments(signature):
        return ast.dump(ast.parse("def preserved" + signature + ": pass").body[0].args)

    verified = {}
    for row in rows:
        module, *attributes = row["id"].removeprefix(PREFIX).split(".")
        value = importlib.import_module("catabolic_native." + module)
        for name in attributes:
            value = getattr(value, name)
        kind = row["signature"]["kind"]
        if kind == "module-or-value-or-reexport":
            assert isinstance(value, dict), row["id"]
            verified[row["id"]] = "value"
        elif kind == "class":
            assert inspect.isclass(value), row["id"]
            verified[row["id"]] = "class"
        else:
            actual = str(inspect.signature(value))
            expected = "(" + row["signature"]["arguments"] + ")"
            assert arguments(actual) == arguments(expected), row["id"]
            verified[row["id"]] = actual
    return verified


def source_manifest():
    root = Path(__file__).resolve().parents[1]
    paths = (
        "scripts/native_layout_embedding_tests.py",
        "scripts/native_qualification_results.py",
        "scripts/native_facade_qualification.py",
        "scripts/native_component_policy_embedding_tests.py",
        "scripts/qualify_native_embedding.py",
        "scripts/build_native_wheel.py",
        "scripts/qualify_native_wheel.py",
        "scripts/migration_rust_resources.py",
        "crates/catabolic-store/src/public_layouts.rs",
        "crates/catabolic-store/src/layout.rs",
        "crates/catabolic-store/src/layout_mutation.rs",
        "crates/catabolic-store/src/definitions.rs",
        "crates/catabolic-store/src/plans.rs",
        "crates/catabolic-store/src/embedding.rs",
        "crates/catabolic-store/src/lib.rs",
        "crates/catabolic-store/resources/layouts.json",
        "crates/catabolic-store/tests/public_layouts_facade.rs",
        "tests/test_native_layout_embedding_contract.py",
        "crates/catabolic-python/src/embedding.rs",
        "crates/catabolic-python/src/public_layouts.rs",
        "crates/catabolic-python/src/lib.rs",
        "crates/catabolic-python/python/catabolic_native/layouts.py",
        "crates/catabolic-python/python/catabolic_native/plans.py",
        "crates/catabolic-python/python/catabolic_native/store.py",
    )
    return {
        name: hashlib.sha256((root / name).read_bytes()).hexdigest() for name in paths
    }


def main():
    parser = argument_parser()
    clear_stale_report(parser=parser)
    if not __debug__:
        raise SystemExit(
            "layout qualification requires assertions; do not run with -O or -OO"
        )
    qualification = sibling("native_facade_qualification")
    args = parser.parse_args()
    for name, value in vars(args).items():
        if isinstance(value, Path):
            # Keep interpreter symlinks to preserve the selected environment.
            setattr(args, name, value.absolute())
    provenance = sibling("qualify_native_embedding")
    if args.scenario:
        oracle, calls = None, {}
        if args.scenario == "reference":
            assert list(sys.version_info[:3]) == qualification.REFERENCE_VERSION, (
                "oracle requires Python 3.14.6"
            )
            assert args.reference_commit == qualification.FROZEN_COMMIT
            oracle = provenance.reference_provenance(
                args.reference_root, args.reference_commit
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
            json.dumps(dict(result=result, calls=calls, oracle=oracle, imports=imports))
        )
        return
    assert (
        (args.dev_wheel or args.wheel)
        and args.inventory
        and args.reference_root
        and args.reference_python
        and args.report
    )
    assert args.build_manifest is not None, "qualification requires --build-manifest"
    source_before = source_manifest()
    identity = qualification.NativeIdentity(args.source_root)
    fence = qualification.file_fence(__file__, args.inventory, args.build_manifest)
    runtime = qualification.reference_runtime(args.reference_python)
    if args.wheel:
        artifact = qualification.strict_artifact(
            args.wheel, args.build_manifest, args.source_root
        )
        build = artifact["build"]
    else:
        artifact = installed_artifact(args.dev_wheel)
        build = build_identity(args.build_manifest, artifact)
    inventory = json.loads(args.inventory.read_text())
    rows = qualification.inventory_rows(inventory, CONTRACTS, 9)
    signatures = installed_signatures(rows)
    oracle = provenance.reference_provenance(
        args.reference_root, inventory["reference_commit"]
    )
    attempt = args.report.with_suffix(".attempt.json")
    attempt.write_text(
        json.dumps(
            dict(
                development_only=bool(args.dev_wheel),
                artifact=artifact,
                build=build,
                installed_signatures=signatures,
                source_sha256=source_before,
                oracle=oracle,
                inventory_sha256=hashlib.sha256(
                    args.inventory.read_bytes()
                ).hexdigest(),
            ),
            indent=2,
        )
        + "\n"
    )
    calls = trace()
    result = sibling("native_qualification_results").run_required_suite(
        unittest.defaultTestLoader.loadTestsFromTestCase(InstalledLayouts),
        {InstalledLayouts: EXPECTED_TESTS},
        expected_count=13,
    )
    installed_calls = calls.copy()
    from catabolic_native import Store

    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        Store.initialize(root / "seed.db")
        outputs = {}
        for engine, python in (
            ("reference", args.reference_python),
            ("native", Path(sys.executable)),
        ):
            shutil.copyfile(root / "seed.db", root / (engine + ".db"))
            command = [
                str(python),
                "-I",
                "-B",
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
            env = dict(
                os.environ,
                CATABOLIC_PYTHON_REFERENCE=str(args.reference_root / "src"),
                PYTHONPATH=str(args.reference_root / "src"),
            )
            completed = subprocess.run(
                command,
                cwd=directory,
                env=env,
                capture_output=True,
                text=True,
                timeout=120,
            )
            assert completed.returncode == 0, (
                engine,
                completed.stdout,
                completed.stderr,
            )
            outputs[engine] = json.loads(completed.stdout)
        reference, native = outputs["reference"]["result"], outputs["native"]["result"]
        qualification.paired_groups(reference, native, EXPECTED_GROUPS, 27)
        assert outputs["reference"]["oracle"] == oracle
        paired_calls = outputs["native"]["calls"]
        oracle["imported_modules"] = outputs["reference"]["imports"]
    coverage = []
    for row in rows:
        key = row["id"] + (".__init__" if row["signature"]["kind"] == "class" else "")
        assert installed_calls.get(key, 0) > 0 and paired_calls.get(key, 0) > 0, row[
            "id"
        ]
        coverage.append(
            dict(
                id=row["id"],
                classification=row["classification"],
                qualification="selected-contracts-tested-with-gaps",
                installed_calls=installed_calls[key],
                paired_calls=paired_calls[key],
                **CONTRACTS[row["id"]],
            )
        )
    identity.verify()
    if args.wheel:
        assert artifact == qualification.strict_artifact(
            args.wheel, args.build_manifest, args.source_root
        )
    else:
        assert artifact == installed_artifact(args.dev_wheel)
        assert build == build_identity(args.build_manifest, artifact)
    assert fence == qualification.file_fence(
        __file__, args.inventory, args.build_manifest
    ), "qualification files changed"
    assert runtime == qualification.reference_runtime(args.reference_python), (
        "oracle interpreter changed"
    )
    assert signatures == installed_signatures(rows)
    assert provenance.reference_provenance(
        args.reference_root, inventory["reference_commit"]
    ) == {k: v for k, v in oracle.items() if k != "imported_modules"}
    assert source_before == source_manifest(), (
        "qualification sources changed during execution"
    )
    report = dict(
        format=1,
        scope="Separate Layouts/plans/PRESETS selected-contract checkpoint; no aggregate, default, publication or release gate closure",
        development_only=bool(args.dev_wheel),
        passed=True,
        reference_runtime=runtime,
        qualification_scripts_sha256=fence["qualification_scripts_sha256"],
        artifact=artifact,
        build=build,
        installed_signatures=signatures,
        acceptance_tests=result.testsRun,
        successful_scenarios=result.successful_scenarios,
        paired_groups=len(native),
        paired_group_names=list(EXPECTED_GROUPS),
        inventory_sha256=hashlib.sha256(args.inventory.read_bytes()).hexdigest(),
        oracle=oracle,
        coverage=coverage,
        source_sha256=source_before,
    )
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(
        json.dumps(
            dict(
                tests=result.testsRun,
                paired_groups=len(native),
                inventory_rows=len(rows),
                development_only=bool(args.dev_wheel),
                report=str(args.report),
            )
        )
    )


if __name__ == "__main__":
    main()
