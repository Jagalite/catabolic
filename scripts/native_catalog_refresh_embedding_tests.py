#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Fixed eight-row CatalogRefresh paired-wheel or explicit development checkpoint.

All 16 installed scenarios, including three legacy CLI cases, and 43 paired
groups are required. This is selected disposable/no-publication evidence only.
"""

import argparse
import functools
import hashlib
import importlib.metadata
import importlib.util
import inspect
import json
import os
import selectors
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

if not __debug__ or os.environ.get("PYTHONOPTIMIZE"):
    raise RuntimeError(
        "catalog refresh qualification requires Python assertions; do not use -O, -OO or PYTHONOPTIMIZE"
    )

PREFIX = "python:catabolic.catalog_refresh"
CONTRACTS = {
    PREFIX: "Preserved module binding and native class export",
    PREFIX
    + ".enqueue": "Scope tokens, rollback, profile, coalescing and observation suppression",
    PREFIX
    + ".finish": "Same result object, untouched arbitrary values, error truncation and cancellation escape",
    PREFIX + ".CatalogRefresh": "Persistent Application/Store/profile identity",
    PREFIX
    + ".CatalogRefresh.__init__": "Persistent Application/Store/profile identity",
    PREFIX
    + ".CatalogRefresh.configure": "Immediate drain, saved-layout validation, disable, integer enabled coercion, nested rollback",
    PREFIX
    + ".CatalogRefresh.pending": "Same-session pages, ordering, truncation and limits",
    PREFIX
    + ".CatalogRefresh.run": "Writer/transaction checks, empty runs, force, durable backoff and watcher-owned skip",
}


EXPECTED_GROUPS = (
    "identity",
    "empty",
    "disabled",
    "enqueue_return",
    "uncommitted",
    "watchers_suppressed",
    "coalesced",
    "other_profile",
    "inner_rollback",
    "outer_rollback",
    "missing_layout_run",
    "backoff_run",
    "force_zero",
    "force_one",
    "retry_attempts",
    "finish_report",
    "configure_bool_False",
    "configure_bool_False_stored",
    "configure_bool_True",
    "configure_bool_True_stored",
    "configure_int_0",
    "configure_int_0_stored",
    "configure_int_1",
    "configure_int_1_stored",
    "configure_str_0",
    "configure_str_0_stored",
    "configure_str_1",
    "configure_str_1_stored",
    "enabled_two_constraint",
    "invalid_enabled",
    "invalid_removals",
    "unknown_catalog",
    "invalid_limit",
    "run_limit",
    "invalid_force",
    "active_transaction",
    "finish_identity",
    "finish_disabled",
    "configure_metadata_rejections",
    "configure_coercion_preconditions",
    "malformed_retry_batch",
    "malformed_retry_attempts",
    "readonly_run",
)


def sibling(name):
    spec = importlib.util.spec_from_file_location(
        name, Path(__file__).with_name(name + ".py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def trace():
    import catabolic_native.catalog_refresh as module

    calls = {PREFIX: 1}
    targets = [(module, "enqueue"), (module, "finish")]
    targets += [
        (module.CatalogRefresh, name)
        for name in ("__init__", "configure", "pending", "run")
    ]
    for owner, name in targets:
        original = getattr(owner, name)
        key = (
            PREFIX + (".CatalogRefresh" if inspect.isclass(owner) else "") + "." + name
        )

        def wrapped(*args, _original=original, _key=key, **kwargs):
            calls[_key] = calls.get(_key, 0) + 1
            return _original(*args, **kwargs)

        setattr(owner, name, functools.wraps(original)(wrapped))
    return calls


def seed(store):
    """Only metadata/queue rows. No output roots, media, credentials or services."""
    with store.transaction() as db:
        db.execute("INSERT INTO profiles VALUES ('other')")
        for catalog in ("second", "third"):
            db.execute("INSERT INTO catalogs(id) VALUES (?)", (catalog,))
        for name in ("active", "other", "disabled", "noevents"):
            definition = json.dumps({"events": name != "noevents"})
            db.execute(
                "INSERT INTO watcher_definitions VALUES (?,?,?,?,?,?,?)",
                ("d-" + name, "default", name, 1, definition, "digest-" + name, 0),
            )
            db.execute(
                "INSERT INTO watchers(profile,name,definition_id,enabled,active_run) VALUES(?,?,?,?,?)",
                (
                    "default",
                    name,
                    "d-" + name,
                    int(name != "disabled"),
                    "r-active" if name == "active" else None,
                ),
            )
        db.execute(
            "INSERT INTO watcher_runs(id,profile,watcher,definition_id,generation,trigger,scheduled_at,started_at,state) VALUES('r-active','default','active','d-active',1,'event',0,0,'observing')"
        )
        db.execute(
            "INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at) VALUES('obs','default','media','{}','[]',0,1,'running',0)"
        )
        db.execute(
            "INSERT INTO observation_requests(id,profile,source,requester,guarantees,job_id,created_at) VALUES('req','default','media','watcher:d-active:1','{}','obs',0)"
        )
        db.execute(
            "INSERT INTO meta(key,value) VALUES('layout-catalog:global','{\"layout\":\"absent\"}')"
        )


def configure_queue(store, *, owned=False):
    with store.transaction() as db:
        db.execute(
            "INSERT OR REPLACE INTO catalog_refresh_settings VALUES('default','global',1,0)"
        )
        if owned:
            db.execute(
                "INSERT OR REPLACE INTO watcher_owners VALUES('default','global','active')"
            )


def malformed_batch(store):
    with store.transaction() as db:
        db.execute("DELETE FROM catalog_refresh_queue WHERE profile='default'")
        for catalog in ("global", "second", "third"):
            db.execute(
                "INSERT OR REPLACE INTO catalog_refresh_settings VALUES('default',?,1,0)",
                (catalog,),
            )
            db.execute(
                "INSERT INTO catalog_refresh_queue(profile,catalog) VALUES('default',?)",
                (catalog,),
            )
        db.execute(
            "INSERT OR REPLACE INTO watcher_owners VALUES('default','global','active')"
        )
        for catalog in ("global", "second"):
            db.execute(
                "INSERT OR REPLACE INTO meta(key,value) VALUES(?,?)",
                ("layout-catalog:" + catalog, "{"),
            )


DIAGNOSTICS = {}


def configuration_rejections(store, refresh):
    """Configuration rejects unusable owner metadata before changing queue state."""
    with store.transaction() as db:
        db.execute(
            "INSERT OR REPLACE INTO catalog_refresh_settings VALUES('default','global',0,7)"
        )
        db.execute(
            "INSERT OR REPLACE INTO catalog_refresh_queue(profile,catalog,generation,attempts,next_attempt,error) VALUES('default','global',9,3,2000000000,'keep')"
        )
    before = [
        store.rows("SELECT * FROM catalog_refresh_settings ORDER BY profile,catalog"),
        store.rows("SELECT * FROM catalog_refresh_queue ORDER BY profile,catalog"),
    ]
    results = []
    for value in ("{}", "null", "false", "[]", '""', "0", "0.0", "{", b"{"):
        label = value if isinstance(value, str) else "blob_" + value.hex()
        with store.transaction() as db:
            db.execute(
                "INSERT OR REPLACE INTO meta(key,value) VALUES('layout-catalog:global',?)",
                (value,),
            )
        try:
            refresh.configure("global", enabled=True)
        except Exception as error:
            diagnostic = [type(error).__name__, str(error)]
        else:
            raise AssertionError("unusable owner metadata enabled refresh: " + label)
        after = [
            store.rows(
                "SELECT * FROM catalog_refresh_settings ORDER BY profile,catalog"
            ),
            store.rows("SELECT * FROM catalog_refresh_queue ORDER BY profile,catalog"),
        ]
        assert after == before, (value, before, after)
        if value in ("{", b"{"):
            DIAGNOSTICS["configure_malformed_metadata_" + label] = diagnostic
            diagnostic = ["rejected malformed metadata before mutation"]
        results.append({"value": label, "error": diagnostic, "unchanged": True})
    refresh.configure("global", enabled=False)
    return results


def configure_coercion_preconditions(refresh):
    class ThrowingEnabled:
        def __bool__(self):
            raise LookupError("enabled bool called")

        def __int__(self):
            raise LookupError("enabled int called")

    results = []
    for catalog, removals in (("global", -1), ("missing", 0)):
        try:
            refresh.configure(catalog, enabled=ThrowingEnabled(), max_removals=removals)
        except Exception as error:
            results.append([type(error).__name__, str(error)])
        else:
            raise AssertionError("invalid configuration was accepted")
    assert results == [
        ["CatabolicError", "max removals must be a nonnegative integer"],
        ["CatabolicError", "unknown catalog"],
    ], results
    return results


def clean(value):
    if isinstance(value, dict):
        return {
            k: clean(v)
            for k, v in value.items()
            if k not in ("updated_at", "event_first", "event_last", "next_attempt")
        }
    if isinstance(value, list):
        return [clean(v) for v in value]
    return value


def scenario(root, engine):
    if engine == "reference":
        from catabolic.app import Application
        from catabolic.catalog_refresh import CatalogRefresh, enqueue, finish
        from catabolic.store import Store
    else:
        from catabolic_native import Application, Store
        from catabolic_native.catalog_refresh import CatalogRefresh, enqueue, finish
    root = Path(root)
    path = root / (engine + ".db")
    Store.initialize(path)
    out = {}
    with Store(path, writable=True) as store:
        seed(store)
        app = Application(store)
        refresh = CatalogRefresh(app)
        out["identity"] = [refresh.app is app, refresh.store is store, refresh.profile]
        out["empty"] = refresh.run()
        out["disabled"] = refresh.configure("global", enabled=False)
        configure_queue(store)
        with store.transaction() as db:
            out["enqueue_return"] = enqueue(db, "default", observation_job="obs")
            out["uncommitted"] = clean(refresh.pending())
            out["watchers_suppressed"] = store.rows(
                "SELECT name,pending_generation FROM watchers ORDER BY name"
            )
            enqueue(db, "default")
        out["coalesced"] = clean(refresh.pending())
        with store.transaction() as db:
            db.execute(
                "INSERT INTO catalog_refresh_settings VALUES('other','global',1,0)"
            )
            enqueue(db, "other")
        out["other_profile"] = clean(
            CatalogRefresh(Application(store, "other")).pending()
        )
        before = clean(refresh.pending())
        try:
            with store.transaction() as db:
                enqueue(db, "default")
                try:
                    with store.transaction() as inner:
                        enqueue(inner, "default")
                        raise ValueError("inner")
                except ValueError:
                    pass
                out["inner_rollback"] = clean(refresh.pending())
                raise ValueError("outer")
        except ValueError:
            pass
        assert clean(refresh.pending()) == before
        out["outer_rollback"] = clean(refresh.pending())
        with store.transaction() as db:
            db.execute("DELETE FROM meta WHERE key='layout-catalog:global'")
        out["missing_layout_run"] = refresh.run()
        out["backoff_run"] = refresh.run()
        out["force_zero"] = refresh.run(force="0")
        out["force_one"] = refresh.run(force="1")
        out["retry_attempts"] = store.rows(
            "SELECT attempts FROM catalog_refresh_queue WHERE profile='default'"
        )
        out["finish_report"] = finish(app, {"original": "kept"})
        with store.transaction() as db:
            db.execute(
                "INSERT INTO meta(key,value) VALUES('layout-catalog:global','{\"layout\":\"absent\"}')"
            )
            db.execute("INSERT INTO watcher_owners VALUES('default','global','active')")
        for value in (False, True, 0, 1, "0", "1"):
            key = "configure_" + type(value).__name__ + "_" + str(value)
            out[key] = refresh.configure("global", enabled=value)
            out[key + "_stored"] = store.rows(
                "SELECT enabled FROM catalog_refresh_settings WHERE profile='default'"
            )
        try:
            refresh.configure("global", enabled=2)
        except Exception as error:
            out["enabled_two_constraint"] = str(error)
        else:
            raise AssertionError("enabled=2 bypassed the frozen CHECK constraint")
        for label, action in (
            ("invalid_enabled", lambda: refresh.configure("global", enabled="no")),
            (
                "invalid_removals",
                lambda: refresh.configure("global", enabled=True, max_removals=True),
            ),
            ("unknown_catalog", lambda: refresh.configure("missing", enabled=False)),
            ("invalid_limit", lambda: refresh.pending(limit=True)),
            ("run_limit", lambda: refresh.run(limit=1001)),
            ("invalid_force", lambda: refresh.run(force="no")),
        ):
            try:
                action()
            except Exception as error:
                out[label] = [type(error).__name__, str(error)]
            else:
                raise AssertionError(label + " did not raise")
        with store.transaction():
            try:
                refresh.run()
            except Exception as error:
                out["active_transaction"] = str(error)
        refresh.configure("global", enabled=False)
        payload = object()
        existing = object()
        original = {"payload": payload, "catalog_refresh": existing}
        out["finish_identity"] = [
            finish(app, original) is original,
            original["payload"] is payload,
            original["catalog_refresh"] is existing,
        ]
        out["finish_disabled"] = finish(app, original, enabled=False) is original
        out["configure_metadata_rejections"] = configuration_rejections(store, refresh)
        out["configure_coercion_preconditions"] = configure_coercion_preconditions(
            refresh
        )
        malformed_batch(store)
        malformed = refresh.run(force=True)
        DIAGNOSTICS["malformed_layout"] = malformed["errors"]
        out["malformed_retry_batch"] = {
            key: value for key, value in malformed.items() if key != "errors"
        } | {"error_catalogs": [row["catalog"] for row in malformed["errors"]]}
        out["malformed_retry_attempts"] = store.rows(
            "SELECT catalog,attempts FROM catalog_refresh_queue WHERE profile='default' ORDER BY catalog"
        )
    with Store(path) as store:
        try:
            CatalogRefresh(Application(store)).run()
        except Exception as error:
            out["readonly_run"] = str(error)
    return clean(out)


class InstalledCatalogRefresh(unittest.TestCase):
    def setUp(self):
        import catabolic_native as native
        import catabolic_native.catalog_refresh as module

        self.native, self.module = native, module
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.path = Path(temporary.name) / "catalog.db"
        native.Store.initialize(self.path)
        self.store = native.Store(self.path, writable=True)
        self.addCleanup(self.store.close)
        seed(self.store)
        self.app = native.Application(self.store)
        self.refresh = module.CatalogRefresh(self.app)

    def test_owner_identity_and_no_path_reopen(self):
        self.assertIs(self.native.CatalogRefresh, self.module.CatalogRefresh)
        owner = self.store._owner
        with patch.object(
            self.native, "execute_operation", side_effect=AssertionError("path reopen")
        ):
            self.assertEqual(self.refresh.run()["processed"], 0)
            self.refresh.configure("global", enabled=False)
            self.assertEqual(
                self.refresh.pending(), {"pending": [], "truncated": False}
            )
        self.assertIs(self.store._owner, owner)
        self.assertIs(self.refresh.app, self.app)
        self.assertIs(self.refresh.store, self.store)

    def test_enqueue_nested_scope_tokens_and_cross_thread(self):
        configure_queue(self.store)
        with self.store.transaction() as outer:
            self.module.enqueue(outer, "default")
            with self.assertRaisesRegex(ValueError, "inner"):
                with self.store.transaction() as inner:
                    self.module.enqueue(inner, "default")
                    raise ValueError("inner")
            self.assertEqual(self.refresh.pending()["pending"][0]["generation"], 1)
            with self.assertRaisesRegex(self.native.CatabolicError, "no longer active"):
                self.module.enqueue(inner, "default")
            errors = []

            def other_thread():
                try:
                    self.module.enqueue(outer, "default")
                except Exception as error:
                    errors.append(str(error))

            thread = threading.Thread(target=other_thread)
            thread.start()
            thread.join(5)
            self.assertFalse(thread.is_alive())
            self.assertEqual(len(errors), 1)
            self.assertIn("another thread", errors[0])
        with self.assertRaisesRegex(self.native.CatabolicError, "no longer active"):
            self.module.enqueue(outer, "default")
        with self.assertRaises(self.native.CatabolicError):
            self.module.enqueue(object(), "default")

    def test_observation_suppression_profile_and_watcher_owned(self):
        configure_queue(self.store, owned=True)
        with self.store.transaction() as db:
            self.module.enqueue(db, "default", observation_job="obs")
        self.assertEqual(self.refresh.pending()["pending"], [])
        self.assertEqual(
            self.store.rows(
                "SELECT name,pending_generation FROM watchers ORDER BY name"
            ),
            [
                {"name": "active", "pending_generation": 0},
                {"name": "disabled", "pending_generation": 0},
                {"name": "noevents", "pending_generation": 0},
                {"name": "other", "pending_generation": 1},
            ],
        )
        with self.store.transaction() as db:
            self.module.enqueue(db, "other")
        self.assertEqual(
            self.store.rows("SELECT sum(pending_generation) AS n FROM watchers")[0][
                "n"
            ],
            1,
        )
        report = self.refresh.configure("global", enabled=True)["catalog_refresh"]
        self.assertEqual(
            report,
            {
                "processed": 1,
                "refreshed": [],
                "errors": [],
                "pending": 1,
                "complete": False,
            },
        )

    def test_configure_coercion_and_nested_rollback(self):
        configure_queue(self.store, owned=True)
        for enabled, stored in (("0", 0), ("1", 1), (False, 0), (True, 1)):
            result = self.refresh.configure("global", enabled=enabled)
            self.assertIs(result["enabled"], enabled)
            self.assertEqual(
                self.store.rows("SELECT enabled FROM catalog_refresh_settings")[0][
                    "enabled"
                ],
                stored,
            )
        with self.assertRaisesRegex(
            self.native.CatabolicError, "CHECK constraint failed: enabled IN"
        ):
            self.refresh.configure("global", enabled=2)
        with self.assertRaises(ValueError):
            self.refresh.configure("global", enabled="invalid")
        with self.assertRaisesRegex(ValueError, "rollback"):
            with self.store.transaction():
                self.refresh.configure("global", enabled=False)
                with self.assertRaisesRegex(
                    self.native.CatabolicError, "outside an active transaction"
                ):
                    self.refresh.configure("global", enabled=True)
                self.assertEqual(self.refresh.pending()["pending"][0]["generation"], 1)
                raise ValueError("rollback")
        self.assertEqual(
            self.store.rows("SELECT enabled FROM catalog_refresh_settings")[0][
                "enabled"
            ],
            1,
        )

    def test_pending_order_limits_and_same_session(self):
        with self.store.transaction() as db:
            for catalog, due in (("global", 3), ("second", 1), ("third", 2)):
                db.execute(
                    "INSERT INTO catalog_refresh_settings VALUES('default',?,1,0)",
                    (catalog,),
                )
                db.execute(
                    "INSERT INTO catalog_refresh_queue(profile,catalog,next_attempt) VALUES('default',?,?)",
                    (catalog, due),
                )
            report = self.refresh.pending(limit=2)
            self.assertEqual(
                [r["catalog"] for r in report["pending"]], ["second", "third"]
            )
            self.assertTrue(report["truncated"])
        for limit in (True, 0, -1, 1001, 1.5, "1"):
            with self.assertRaisesRegex(
                self.native.CatabolicError, "limit must be between 1 and 1000"
            ):
                self.refresh.pending(limit=limit)

    def test_configure_rejects_metadata_and_preserves_coercion_preconditions(self):
        self.assertEqual(len(configuration_rejections(self.store, self.refresh)), 9)
        self.assertEqual(len(configure_coercion_preconditions(self.refresh)), 2)

    def test_retry_backoff_force_and_fail_closed(self):
        configure_queue(self.store)
        with self.store.transaction() as db:
            db.execute("DELETE FROM meta WHERE key='layout-catalog:global'")
            self.module.enqueue(db, "default")
        before = int(time.time())
        result = self.refresh.run()
        self.assertEqual(
            result["errors"],
            [{"catalog": "global", "error": "catalog no longer has a saved layout"}],
        )
        row = self.refresh.pending()["pending"][0]
        self.assertEqual(row["attempts"], 1)
        self.assertGreaterEqual(row["next_attempt"], before + 5)
        self.assertEqual(self.refresh.run(force="0")["processed"], 0)
        self.assertEqual(self.refresh.run(force="1")["processed"], 1)
        self.assertEqual(self.refresh.pending()["pending"][0]["attempts"], 2)
        # A nonexistent saved definition cannot clear queued intent or write output.
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO meta(key,value) VALUES('layout-catalog:global','{\"layout\":\"absent\"}')"
            )
        self.assertFalse(self.refresh.run(force=True)["complete"])
        self.assertEqual(self.store.rows("SELECT * FROM journal"), [])

    def test_corrupt_layout_skips_owner_and_continues_retry_batch(self):
        malformed_batch(self.store)
        report = self.refresh.run(force=True)
        self.assertEqual(report["processed"], 3)
        self.assertEqual(report["refreshed"], [])
        self.assertEqual(report["pending"], 3)
        self.assertFalse(report["complete"])
        self.assertEqual(
            [row["catalog"] for row in report["errors"]], ["second", "third"]
        )
        self.assertEqual(
            self.store.rows(
                "SELECT catalog,attempts FROM catalog_refresh_queue ORDER BY catalog"
            ),
            [
                {"catalog": "global", "attempts": 0},
                {"catalog": "second", "attempts": 1},
                {"catalog": "third", "attempts": 1},
            ],
        )
        self.assertEqual(self.refresh.run()["processed"], 1)
        self.assertEqual(self.store.rows("SELECT * FROM journal"), [])

    def test_public_refresh_never_opts_in_fallback_publication(self):
        configure_queue(self.store)
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO fallback_policies VALUES('fixture-policy','default','fixture',1,'{}','fixture-digest')"
            )
            db.execute(
                "INSERT INTO saved_queries(id,profile,name,revision,definition,digest) VALUES('fixture-query','default','fixture',1,'{}','fixture-query-digest')"
            )
            db.execute(
                "INSERT INTO fallback_bindings(profile,catalog,policy_id,query_id,layout) VALUES('default','global','fixture-policy','fixture-query','fixture-layout')"
            )
            self.module.enqueue(db, "default")
        report = self.refresh.run(force=True)
        self.assertFalse(report["complete"])
        self.assertEqual(report["refreshed"], [])
        self.assertEqual(report["pending"], 1)
        self.assertIn(
            "restricted to disposable debug fixtures", report["errors"][0]["error"]
        )
        self.assertEqual(self.store.rows("SELECT * FROM journal"), [])
        # The public adapter discards private companion fixture options too.
        report = self.store._call(
            "catalog_refresh",
            "run",
            {"force": 1, "disposable_fixture": True},
            "default",
        )
        self.assertIn(
            "restricted to disposable debug fixtures", report["errors"][0]["error"]
        )
        self.assertEqual(self.store.rows("SELECT * FROM journal"), [])

    def test_finish_identity_error_truncation_and_exception_escape(self):
        payload, existing = object(), object()
        result = {"opaque": payload, "catalog_refresh": existing}
        self.assertIs(self.module.finish(self.app, result), result)
        self.assertIs(result["catalog_refresh"], existing)
        self.assertIs(result["opaque"], payload)
        with patch.object(
            self.module.CatalogRefresh,
            "run",
            side_effect=AssertionError("must not run"),
        ):
            self.assertIs(self.module.finish(self.app, result, enabled=False), result)
            self.store.schema_version = 13
            self.assertIs(self.module.finish(self.app, result), result)
        self.store.schema_version = 32
        with patch.object(
            self.module.CatalogRefresh, "run", side_effect=ValueError("é" * 4500)
        ):
            self.assertIs(self.module.finish(self.app, result), result)
        self.assertEqual(result["catalog_refresh"]["errors"], [{"error": "é" * 4000}])
        from sqlite3 import OperationalError

        with patch.object(
            self.module.CatalogRefresh,
            "run",
            side_effect=OperationalError("sqlite exception only"),
        ):
            self.assertIs(self.module.finish(self.app, result), result)
        self.assertEqual(
            result["catalog_refresh"]["errors"], [{"error": "sqlite exception only"}]
        )
        cancellation = KeyboardInterrupt("exact")
        with patch.object(self.module.CatalogRefresh, "run", side_effect=cancellation):
            with self.assertRaises(KeyboardInterrupt) as caught:
                self.module.finish(self.app, result)
        self.assertIs(caught.exception, cancellation)

    def test_readonly_closed_detached_and_reconnect(self):
        self.store.close()
        with self.assertRaises(self.native.CatabolicError):
            self.refresh.pending()
        self.store = self.native.Store(self.path)
        self.addCleanup(self.store.close)
        self.refresh = self.module.CatalogRefresh(self.native.Application(self.store))
        with self.assertRaisesRegex(
            self.native.CatabolicError, "outside an active transaction"
        ):
            self.refresh.run()
        with self.assertRaises(self.native.CatabolicError):
            self.module.enqueue(self.store.db, "default")
        self.store.close()
        self.store = self.native.Store(self.path, writable=True)
        self.addCleanup(self.store.close)
        self.refresh = self.module.CatalogRefresh(self.native.Application(self.store))
        with self.store.detached():
            with self.assertRaises(self.native.CatabolicError):
                self.refresh.pending()
        self.assertEqual(self.refresh.pending()["pending"], [])

    def interrupt_operation(self, table, operation, *, custom, nested):
        self.store.db.execute(
            f"CREATE TRIGGER slow_refresh BEFORE INSERT ON {table} BEGIN SELECT (WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<100000000) SELECT sum(x) FROM n); END"
        )
        number = signal.SIGUSR1 if custom else signal.SIGINT
        original = signal.getsignal(number)
        cancellation = (
            RuntimeError("exact custom refresh cancellation")
            if custom
            else KeyboardInterrupt("exact refresh cancellation")
        )

        def handler(*_):
            raise cancellation

        signal.signal(number, handler)
        self.addCleanup(signal.signal, number, original)
        thread = threading.Thread(
            target=lambda: (time.sleep(0.15), os.kill(os.getpid(), number))
        )
        thread.start()
        try:
            with self.assertRaises(type(cancellation)) as caught:
                if nested:
                    with self.store.transaction():
                        with self.store.transaction() as db:
                            operation(db)
                else:
                    operation(None)
            self.assertIs(caught.exception, cancellation)
        finally:
            thread.join(5)
        self.assertFalse(thread.is_alive())
        self.assertFalse(self.store.db.in_transaction)
        self.store.db.execute("DROP TRIGGER slow_refresh")
        self.assertEqual(self.refresh.pending()["pending"], [])
        self.assertEqual(self.refresh.run()["processed"], 0)

    def test_enqueue_cancellation_retires_scope_and_keeps_exception(self):
        configure_queue(self.store)
        self.interrupt_operation(
            "catalog_refresh_queue",
            lambda db: self.module.enqueue(db, "default"),
            custom=False,
            nested=True,
        )

    def test_configure_custom_cancellation_keeps_exception_and_owner(self):
        self.interrupt_operation(
            "catalog_refresh_settings",
            lambda _: self.refresh.configure("global", enabled=False),
            custom=True,
            nested=False,
        )


EXPECTED_TESTS = (
    "test_owner_identity_and_no_path_reopen",
    "test_enqueue_nested_scope_tokens_and_cross_thread",
    "test_observation_suppression_profile_and_watcher_owned",
    "test_configure_coercion_and_nested_rollback",
    "test_configure_rejects_metadata_and_preserves_coercion_preconditions",
    "test_pending_order_limits_and_same_session",
    "test_retry_backoff_force_and_fail_closed",
    "test_corrupt_layout_skips_owner_and_continues_retry_batch",
    "test_public_refresh_never_opts_in_fallback_publication",
    "test_finish_identity_error_truncation_and_exception_escape",
    "test_readonly_closed_detached_and_reconnect",
    "test_enqueue_cancellation_retires_scope_and_keeps_exception",
    "test_configure_custom_cancellation_keeps_exception_and_owner",
)


CLI_EVIDENCE = {}
OPTIONS = None


class CatalogRefreshCLI(unittest.TestCase):
    def setUp(self):
        from catabolic_native import Store

        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.seed = self.root / "seed.db"
        Store.initialize(self.seed)
        with Store(self.seed, writable=True) as store:
            seed(store)
            configure_queue(store, owned=True)
        self.paths = {}
        for engine in ("reference", "native"):
            self.paths[engine] = self.root / (engine + ".db")
            shutil.copyfile(self.seed, self.paths[engine])

    def command(self, engine, arguments, *, machine=False):
        command = (
            [
                str(OPTIONS.reference_python),
                "-I",
                "-B",
                str(Path(__file__).with_name("native_facade_qualification.py")),
                "--reference-root",
                str(OPTIONS.reference_root),
                "--oracle-manifest",
                str(OPTIONS.oracle_manifest),
                "--oracle-sha256",
                OPTIONS.oracle_manifest_sha256,
                "--",
            ]
            if engine == "reference"
            else [str(OPTIONS.legacy_binary)]
        )
        return [
            *command,
            "--db",
            str(self.paths[engine]),
            *(["--machine"] if machine else ["--json"]),
            "catalog-refresh",
            *arguments,
        ]

    def environment(self, engine):
        env = dict(os.environ)
        env.pop("CATABOLIC_DB", None)
        env.pop("CATABOLIC_PROFILE", None)
        env["PYTHONPATH"] = (
            str(OPTIONS.reference_root / "src") if engine == "reference" else ""
        )
        env["CATABOLIC_PYTHON_REFERENCE"] = str(OPTIONS.reference_root / "src")
        return env

    def test_paired_cli_leaves_and_invalid_arguments(self):
        observations = {}
        operations = [
            ["pending"],
            ["enable", "--catalog", "global"],
            ["pending", "--limit", "1"],
            ["run", "--catalog", "global", "--force"],
            ["disable", "--catalog", "global"],
            ["run"],
            ["pending", "--limit", "0"],
            ["watch", "--interval", "0"],
            ["watch", "--limit", "0"],
        ]
        for engine in ("reference", "native"):
            values = []
            for operation in operations:
                completed = subprocess.run(
                    self.command(engine, operation, machine=True),
                    cwd=self.root,
                    env=self.environment(engine),
                    capture_output=True,
                    text=True,
                    timeout=15,
                )
                values.append(
                    {
                        "arguments": operation,
                        "code": completed.returncode,
                        "stdout": clean(json.loads(completed.stdout)),
                        "stderr": completed.stderr,
                    }
                )
            observations[engine] = values
        self.assertEqual(observations["native"], observations["reference"])
        CLI_EVIDENCE["leaves"] = observations["native"]

    def test_paired_watch_ticks_release_writer_and_interrupt(self):
        from catabolic_native import Store

        observations = {}
        for engine in ("reference", "native"):
            with (
                Store(self.paths[engine], writable=True) as store,
                store.transaction() as db,
            ):
                db.execute(
                    "INSERT INTO catalog_refresh_queue(profile,catalog) VALUES('default','global')"
                )
            process = subprocess.Popen(
                self.command(engine, ["watch", "--interval", "1"]),
                cwd=self.root,
                env=self.environment(engine),
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                bufsize=1,
            )
            selector = selectors.DefaultSelector()
            selector.register(process.stdout, selectors.EVENT_READ)
            lines = []
            try:
                for _ in range(2):
                    self.assertTrue(
                        selector.select(10), "watch did not emit a bounded tick"
                    )
                    line = process.stdout.readline()
                    self.assertTrue(line, "watch exited before second tick")
                    lines.append(line.rstrip("\n"))
                    with Store(self.paths[engine], writable=True) as store:
                        self.assertEqual(
                            store.rows("SELECT generation FROM catalog_refresh_queue")[
                                0
                            ]["generation"],
                            1,
                        )
                process.send_signal(signal.SIGINT)
                stdout, stderr = process.communicate(timeout=10)
                self.assertEqual(process.returncode, 130)
                observations[engine] = {
                    "lines": lines,
                    "remaining_stdout": stdout,
                    "stderr": stderr,
                    "code": process.returncode,
                }
            finally:
                selector.close()
                if process.poll() is None:
                    process.kill()
                    process.communicate(timeout=10)
        self.assertEqual(observations["native"], observations["reference"])
        CLI_EVIDENCE["watch_ticks"] = observations["native"]

    def test_paired_watch_idle_silent_and_error_continuation(self):
        observations = {}
        for engine in ("reference", "native"):
            process = subprocess.Popen(
                self.command(engine, ["watch", "--interval", "1"]),
                cwd=self.root,
                env=self.environment(engine),
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                bufsize=1,
            )
            selector = selectors.DefaultSelector()
            selector.register(process.stdout, selectors.EVENT_READ)
            try:
                self.assertFalse(
                    selector.select(1.2), "empty watch produced idle output"
                )
                process.send_signal(signal.SIGINT)
                stdout, stderr = process.communicate(timeout=10)
                self.assertEqual(process.returncode, 130)
                observations[engine] = [stdout, stderr, process.returncode]
            finally:
                selector.close()
                if process.poll() is None:
                    process.kill()
                    process.communicate(timeout=10)
        self.assertEqual(observations["native"], observations["reference"])
        # Opening failure is caught per tick, rather than a false completion.
        errors = {}
        for engine in ("reference", "native"):
            self.paths[engine] = self.root / "missing.db"
            process = subprocess.Popen(
                self.command(engine, ["watch", "--interval", "1"]),
                cwd=self.root,
                env=self.environment(engine),
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                bufsize=1,
            )
            selector = selectors.DefaultSelector()
            selector.register(process.stdout, selectors.EVENT_READ)
            try:
                results = []
                for _ in range(2):
                    self.assertTrue(selector.select(10))
                    result = json.loads(process.stdout.readline())
                    self.assertEqual(set(result), {"complete", "errors"})
                    self.assertFalse(result["complete"])
                    self.assertEqual(len(result["errors"]), 1)
                    self.assertTrue(result["errors"][0]["error"])
                    results.append(result)
                process.send_signal(signal.SIGINT)
                process.communicate(timeout=10)
                self.assertEqual(process.returncode, 130)
                errors[engine] = results
            finally:
                selector.close()
                if process.poll() is None:
                    process.kill()
                    process.communicate(timeout=10)
        # OS/Store open diagnostics are recorded independently, not normalized
        # into a misleading exact value-parity claim.
        CLI_EVIDENCE["idle"] = observations["native"]
        CLI_EVIDENCE["opening_errors_unpaired"] = errors


CLI_TESTS = (
    "test_paired_cli_leaves_and_invalid_arguments",
    "test_paired_watch_ticks_release_writer_and_interrupt",
    "test_paired_watch_idle_silent_and_error_continuation",
)


def verify_development_wheel(path):
    """Explicit helperless development bytes, never the strict release verifier."""
    import catabolic_native

    distribution = importlib.metadata.distribution("catabolic-native")
    installed = Path(distribution.locate_file("")).resolve()
    package = Path(catabolic_native.__file__).resolve().parent
    hashes = {}
    with zipfile.ZipFile(path) as archive:
        assert "catabolic_native/native-helper.json" not in archive.namelist(), (
            "development wheel must be helperless"
        )
        assert "catabolic_native/bin/catabolic-native" not in archive.namelist(), (
            "development wheel must be helperless"
        )
        members = archive.infolist()
        assert 0 < len(members) < 10000
        assert sum(member.file_size for member in members) <= 256 * 1024 * 1024
        assert len({member.filename for member in members}) == len(members)
        for member in members:
            if member.is_dir() or member.filename.endswith(".dist-info/RECORD"):
                continue
            relative = Path(member.filename)
            assert not relative.is_absolute() and ".." not in relative.parts
            assert not any(part.endswith(".data") for part in relative.parts)
            target = installed / relative
            assert (
                target.resolve().is_relative_to(installed) and not target.is_symlink()
            )
            hashes[member.filename] = hashlib.sha256(archive.read(member)).hexdigest()
            assert digest(target) == hashes[member.filename]
    for target in package.rglob("*"):
        if target.is_file() and "__pycache__" not in target.parts:
            assert not target.is_symlink()
            assert str(target.relative_to(installed)) in hashes
    assert (
        str(Path(catabolic_native._native.__file__).resolve().relative_to(installed))
        in hashes
    )
    assert (
        str(Path(catabolic_native.__file__).resolve().relative_to(installed)) in hashes
    )
    return hashes


def clear_stale_report():
    # Invalidate a normal attempt before loading even the shared gate helper.
    # Optimized-mode refusal above deliberately leaves all files untouched.
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--report", type=Path)
    args, _ = parser.parse_known_args()
    if args.report:
        args.report.unlink(missing_ok=True)
        args.report.with_suffix(".attempt.json").unlink(missing_ok=True)


def main():
    global OPTIONS
    clear_stale_report()
    qualification = sibling("native_facade_qualification")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--scenario", choices=("native", "reference"))
    parser.add_argument("--root", type=Path)
    parser.add_argument("--reference-root", type=Path, required=True)
    parser.add_argument("--reference-python", type=Path)
    parser.add_argument("--reference-commit")
    parser.add_argument("--inventory", type=Path)
    parser.add_argument("--source-root", type=Path)
    parser.add_argument("--wheel", type=Path)
    parser.add_argument(
        "--dev",
        action="store_true",
        help="Explicit development artifact; never release qualification",
    )
    parser.add_argument("--native-binary", type=Path)
    parser.add_argument("--legacy-binary", type=Path)
    parser.add_argument("--build-manifest", type=Path)
    parser.add_argument("--report", type=Path)
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
            json.dumps(
                {
                    "result": result,
                    "oracle": oracle,
                    "calls": calls,
                    "imports": imports,
                    "diagnostics": DIAGNOSTICS,
                }
            )
        )
        return
    assert all(
        (
            args.reference_python,
            args.inventory,
            args.source_root,
            args.wheel,
            args.native_binary,
            args.legacy_binary,
            args.build_manifest,
            args.report,
        )
    )
    assert not Path.cwd().resolve().is_relative_to(args.source_root.resolve()), (
        "run installed qualification outside the checkout"
    )
    OPTIONS = args
    inventory = json.loads(args.inventory.read_text())
    rows = qualification.inventory_rows(inventory, CONTRACTS, 8)
    identity = qualification.NativeIdentity(args.source_root)
    fence = qualification.file_fence(__file__, args.inventory, args.build_manifest)
    runtime = qualification.reference_runtime(args.reference_python)
    source = sibling("build_native_wheel").source_digest(args.source_root)
    manifest = json.loads(args.build_manifest.read_text())
    expected_manifest = {
        "profile": "development" if args.dev else "release",
        "source_sha256": source,
        "wheel_sha256": digest(args.wheel),
        "native_binary_sha256": digest(args.native_binary),
        "legacy_binary_sha256": digest(args.legacy_binary),
    }
    assert manifest["source_sha256_after"] == source, (
        "build source fence was not stable"
    )
    for key, value in expected_manifest.items():
        assert manifest[key] == value, ("build identity mismatch", key)
    script_names = (
        Path(__file__).name,
        "native_qualification_results.py",
        "qualify_native_embedding.py",
        "build_native_wheel.py",
        "native_facade_qualification.py",
        "qualify_native_wheel.py",
    )
    script_hashes = {
        name: digest(Path(__file__).with_name(name)) for name in script_names
    }
    inventory_hash = digest(args.inventory)
    manifest_hash = digest(args.build_manifest)
    installed = (
        verify_development_wheel(args.wheel)
        if args.dev
        else qualification.strict_artifact(
            args.wheel,
            args.build_manifest,
            args.source_root,
            native_binary=args.native_binary,
            legacy_binary=args.legacy_binary,
        )
    )
    oracle = provenance.reference_provenance(
        args.reference_root, inventory["reference_commit"]
    )
    os.environ["CATABOLIC_NATIVE_BINARY"] = str(args.native_binary)
    os.environ["CATABOLIC_PYTHON_REFERENCE"] = str(args.reference_root / "src")
    attempt = args.report.with_suffix(".attempt.json")
    attempt.write_text(
        json.dumps(
            dict(
                development_only=args.dev,
                build=manifest,
                installed=installed,
                oracle=oracle,
                reference_runtime=runtime,
                **fence,
            ),
            indent=2,
        )
        + "\n"
    )
    args.oracle_manifest = attempt
    args.oracle_manifest_sha256 = digest(attempt)
    calls = trace()
    suite = unittest.TestSuite(
        [
            unittest.defaultTestLoader.loadTestsFromTestCase(InstalledCatalogRefresh),
            unittest.defaultTestLoader.loadTestsFromTestCase(CatalogRefreshCLI),
        ]
    )
    result = sibling("native_qualification_results").run_required_suite(
        suite,
        {InstalledCatalogRefresh: EXPECTED_TESTS, CatalogRefreshCLI: CLI_TESTS},
        16,
    )
    with tempfile.TemporaryDirectory() as directory:
        outputs = {}
        for engine, interpreter in (
            ("reference", args.reference_python),
            ("native", Path(sys.executable)),
        ):
            command = [
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
                "--reference-commit",
                inventory["reference_commit"],
            ]
            completed = subprocess.run(
                command, cwd=directory, capture_output=True, text=True, timeout=60
            )
            assert completed.returncode == 0, (
                engine,
                completed.stdout,
                completed.stderr,
            )
            outputs[engine] = json.loads(completed.stdout)
        reference, native = outputs["reference"]["result"], outputs["native"]["result"]
        qualification.paired_groups(reference, native, EXPECTED_GROUPS, 43)
        assert outputs["reference"]["oracle"] == oracle
    paired_calls = outputs["native"]["calls"]
    report = {
        "format": 1,
        "scope": "Eight-row CatalogRefresh public facade and five CLI leaves, selected disposable/no-publication contracts only",
        "development_only": args.dev,
        "passed": True,
        "reference_runtime": runtime,
        "publication_qualification": "Not established; default/release destructive and fallback publication gates remain intact",
        "tests": result.testsRun,
        "successful_scenarios": result.successful_scenarios,
        "paired_groups": len(native),
        "paired_group_names": list(EXPECTED_GROUPS),
        "cli_evidence": CLI_EVIDENCE,
        "build": manifest,
        "build_manifest_sha256": manifest_hash,
        "artifact": installed,
        "installed_payloads_sha256": installed
        if args.dev
        else installed["installed_wheel"]["installed_payloads_sha256"],
        "inventory_sha256": inventory_hash,
        "oracle": oracle,
        "oracle_imports": outputs["reference"]["imports"],
        "unpaired_diagnostics": {
            engine: output["diagnostics"] for engine, output in outputs.items()
        },
        "qualification_scripts_sha256": script_hashes,
        "coverage": [],
    }
    for row in rows:
        key = row["id"] + (".__init__" if row["signature"]["kind"] == "class" else "")
        assert calls.get(key, 0) > 0 and paired_calls.get(key, 0) > 0, (
            "uncalled row",
            key,
        )
        report["coverage"].append(
            {
                "id": row["id"],
                "installed_calls": calls[key],
                "paired_calls": paired_calls[key],
                "selected_contracts": CONTRACTS[row["id"]],
                "qualification": "selected-contracts-tested-with-gaps",
                "gap": "Publication/recovery effects, all coercion types, real platforms and full frozen producer suites remain unqualified; enabled=2 preserves CHECK failure and message but native Store maps sqlite3.IntegrityError to CatabolicError",
            }
        )
    assert sibling("build_native_wheel").source_digest(args.source_root) == source
    identity.verify()
    assert (
        verify_development_wheel(args.wheel)
        if args.dev
        else qualification.strict_artifact(
            args.wheel,
            args.build_manifest,
            args.source_root,
            native_binary=args.native_binary,
            legacy_binary=args.legacy_binary,
        )
    ) == installed
    assert fence == qualification.file_fence(
        __file__, args.inventory, args.build_manifest
    ), "qualification files changed"
    assert runtime == qualification.reference_runtime(args.reference_python), (
        "oracle interpreter changed"
    )
    assert (
        provenance.reference_provenance(
            args.reference_root, inventory["reference_commit"]
        )
        == oracle
    )
    assert digest(args.native_binary) == manifest["native_binary_sha256"]
    assert digest(args.legacy_binary) == manifest["legacy_binary_sha256"]
    assert {
        name: digest(Path(__file__).with_name(name)) for name in script_names
    } == script_hashes
    assert digest(args.inventory) == inventory_hash
    assert digest(args.build_manifest) == manifest_hash
    assert digest(attempt) == args.oracle_manifest_sha256, "CLI oracle snapshot changed"
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(
        json.dumps(
            {
                "tests": result.testsRun,
                "paired_groups": len(native),
                "inventory_rows": len(rows),
                "development_only": args.dev,
                "report": str(args.report),
            }
        )
    )


if __name__ == "__main__":
    main()
