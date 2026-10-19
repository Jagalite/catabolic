#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Separate installed persistent-query gate. No contribution to the 69/19/82 gate."""

import argparse
import functools
import hashlib
import importlib.util
import inspect
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from uuid import UUID

PREFIX = "python:catabolic."
CONTRACTS = {
    PREFIX + "saved_queries.Queries" + suffix: {
        "tested": tested,
        "gap": gap,
    }
    for suffix, tested, gap in (
        (
            "",
            "Native Store/profile identity and all six methods",
            "Selected contracts only; foreign Store objects are unsupported",
        ),
        (
            ".__init__",
            "Store/profile attributes and alternate profile",
            "No legacy Store/connection sharing",
        ),
        (
            ".get",
            "Decoded definitions, missing revision, profile isolation",
            "Exhaustive database corruption behavior is not qualified",
        ),
        (
            ".list",
            "Pagination, scalar cursors, defaults and invalid limits",
            "SQLite blob/memoryview cursors and every byte boundary are unqualified",
        ),
        (
            ".validate",
            "Selection/rows/document definitions, budgets, composition and invalid bodies",
            "Every invalid field combination and all size boundaries are unqualified",
        ),
        (
            ".put",
            "Revision/digest idempotence, composition dependencies, nested rollback",
            "All competing writers and injected SQLite failures are unqualified",
        ),
        (
            ".select",
            "SQL/GraphQL/composition, tuple/set results, shared native cache/budgets, epoch and cancellation",
            "Foreign Python EvaluationSession/Access contexts and mutable context dictionaries are unsupported; all composition bounds are unqualified",
        ),
        (
            ".run",
            "Selection truncation, rows/document output, native session byte budget and errors",
            "External scoped Access authority is unsupported by this facade; all budget combinations are unqualified",
        ),
    )
}
CONTRACTS[PREFIX + "sql_query.execute_sql"] = {
    "tested": "Same-owner uncommitted reads, snapshot, rollback, options, errors, HTTP/stable restrictions and signal cleanup",
    "gap": "Foreign SQLite Stores are unsupported; exhaustive sqlite exception classes and every result-byte boundary are unqualified",
}
CONTRACTS[PREFIX + "graphql_query.execute_graphql"] = {
    "tested": "Same-owner uncommitted reads, snapshot, rollback, variables/text, operations, validation errors and owner recovery",
    "gap": "Python _context_factory resolver/access callbacks are rejected; no public native scoped GraphQL context adapter is exposed; all schema/budget combinations are unqualified",
}


def sibling(name):
    spec = importlib.util.spec_from_file_location(
        name, Path(__file__).with_name(name + ".py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def api(engine):
    if engine == "reference":
        from catabolic.app import Application
        from catabolic.domain import CatabolicError
        from catabolic.evaluation import EvaluationSession
        from catabolic.graphql_query import execute_graphql
        from catabolic.saved_queries import Queries
        from catabolic.sql_query import execute_sql
        from catabolic.store import Store, encode
    else:
        from catabolic_native import Application, CatabolicError, Store, encode
        from catabolic_native.evaluation import EvaluationSession
        from catabolic_native.graphql_query import execute_graphql
        from catabolic_native.saved_queries import Queries
        from catabolic_native.sql_query import execute_sql
    return (
        Application,
        Store,
        Queries,
        execute_sql,
        execute_graphql,
        EvaluationSession,
        CatabolicError,
        encode,
    )


def trace():
    from catabolic_native import graphql_query, saved_queries, sql_query

    calls = {}

    def wrap(fn, key):
        @functools.wraps(fn)
        def counted(*args, **kwargs):
            calls[key] = calls.get(key, 0) + 1
            return fn(*args, **kwargs)

        return counted

    for name, fn in vars(saved_queries.Queries).copy().items():
        if inspect.isfunction(fn) and (not name.startswith("_") or name == "__init__"):
            setattr(
                saved_queries.Queries,
                name,
                wrap(fn, PREFIX + "saved_queries.Queries." + name),
            )
    for module, name in (
        (sql_query, "execute_sql"),
        (graphql_query, "execute_graphql"),
    ):
        setattr(
            module,
            name,
            wrap(
                getattr(module, name),
                PREFIX + module.__name__.split(".")[-1] + "." + name,
            ),
        )
    return calls


def definitions():
    return {
        "left": {
            "selection": {
                "language": "sql",
                "query": "SELECT item_id FROM catalog_items WHERE item_id IN ('a','b')",
            }
        },
        "right": {
            "selection": {
                "language": "sql",
                "query": "SELECT item_id FROM catalog_items WHERE item_id IN ('b','c')",
            }
        },
        "graphql": {
            "selection": {
                "language": "graphql",
                "query": "query($after:String){items(first:1,after:$after){nodes{id} pageInfo{endCursor hasNextPage}}}",
            }
        },
        "rows": {
            "mode": "rows",
            "selection": {
                "language": "sql",
                "query": "SELECT item_id,title FROM catalog_items ORDER BY item_id",
            },
        },
        "document": {
            "mode": "document",
            "selection": {
                "language": "graphql",
                "query": "{profile items(first:2){nodes{id title} pageInfo{endCursor hasNextPage}}}",
            },
        },
    }


def scenario(root, engine):
    Application, Store, Queries, sql, graphql, EvaluationSession, Error, encode = api(
        engine
    )
    output, ids = {}, {}
    with Store(Path(root) / (engine + ".db"), writable=True) as store:
        app = Application(store)
        app.add_profile("alternate")
        for identifier in ("a", "b", "c"):
            app.put_item("movie", {}, {"title": identifier.upper()}, identifier)
        queries = Queries(store)
        assert queries.store is store and queries.profile == "default"

        def error(key, action):
            try:
                action()
            except Error as exc:
                output[key] = str(exc)
            else:
                raise AssertionError(key + " did not fail")

        records = {}
        for name, definition in definitions().items():
            output["validate_" + name] = queries.validate(definition)
            record = queries.put(name, definition)
            assert UUID(record["id"]).version == 4
            assert (
                record["digest"]
                == hashlib.sha256(encode(record["definition"]).encode()).hexdigest()
            )
            assert queries.put(name, definition) == record
            ids[record["id"]] = name.upper() + "_ID"
            records[name] = record
            output["put_" + name] = record
            output["get_" + name] = queries.get(record["id"])
            output["run_" + name] = queries.run(record["id"], limit=1)
        output["select_sql"] = queries.select(records["left"]["id"])
        assert isinstance(output["select_sql"], tuple) and isinstance(
            output["select_sql"][1], set
        )
        output["select_graphql"] = queries.select(records["graphql"]["id"])
        for combine in ("union", "intersection", "difference"):
            record = queries.put(
                combine,
                {
                    "combine": combine,
                    "queries": [records["left"]["id"], records["right"]["id"]],
                },
            )
            ids[record["id"]] = combine.upper() + "_ID"
            output["composition_" + combine] = queries.select(record["id"])
            assert (
                record["digest"]
                == hashlib.sha256(encode(record["definition"]).encode()).hexdigest()
            )
            output["dependencies_" + combine] = sorted(
                ids[row["dependency_id"]]
                for row in store.rows(
                    "SELECT dependency_id FROM query_dependencies WHERE query_id=?",
                    (record["id"],),
                )
            )
        revised = queries.put("left", definitions()["right"])
        ids[revised["id"]] = "LEFT_REVISION_TWO"
        assert revised["revision"] == 2
        output["revision"] = revised
        all_rows, after = [], ""
        while True:
            page = queries.list(2, after)
            all_rows.extend(page["queries"])
            if page["next_after"] is None:
                break
            assert page["next_after"] == page["queries"][-1]["id"]
            after = page["next_after"]
        output["list_names"] = sorted(
            (row["name"], row["revision"], row["definition_omitted"])
            for row in all_rows
        )
        output["list_scalar_binding"] = []
        for cursor in (None, 1, 1.5, True, False, "zzzz"):
            actual = [row["id"] for row in queries.list(after=cursor)["queries"]]
            expected = [
                row["id"]
                for row in store.rows(
                    "SELECT id FROM saved_queries WHERE profile='default' AND id>? ORDER BY id",
                    (cursor,),
                )
            ]
            # Random UUIDs can sort on either side of the numeric cursor text.
            # Verify the exact bound-SQL selection independently in each clone.
            assert actual == expected
            output["list_scalar_binding"].append(actual == expected)
        output["alternate_empty"] = Queries(store, "alternate").list()
        for key, action in (
            ("missing", lambda: queries.get("missing")),
            (
                "profile_isolation",
                lambda: Queries(store, "alternate").get(records["left"]["id"]),
            ),
            ("limit_bool", lambda: queries.list(True)),
            ("run_limit", lambda: queries.run(records["left"]["id"], 0)),
            ("invalid_fields", lambda: queries.validate({"unexpected": 1})),
            ("invalid_version", lambda: queries.validate({"version": True})),
            (
                "invalid_budget",
                lambda: queries.validate({**definitions()["left"], "max_ids": True}),
            ),
            (
                "cross_profile_body",
                lambda: queries.validate(
                    {
                        "selection": {
                            "language": "sql",
                            "query": "SELECT 1",
                            "profile": "alternate",
                        }
                    }
                ),
            ),
            ("select_rows_error", lambda: queries.select(records["rows"]["id"])),
            (
                "stable_sql",
                lambda: sql(store.path, "SELECT random()", _store=store, _stable=True),
            ),
            (
                "http_sql",
                lambda: sql(
                    store.path, "SELECT * FROM watchers", _store=store, _http=True
                ),
            ),
            (
                "sql_bool_limit",
                lambda: sql(store.path, "SELECT 1", _store=store, max_rows=True),
            ),
            (
                "graphql_bool_timeout",
                lambda: graphql(store.path, "{profile}", _store=store, timeout_ms=True),
            ),
            (
                "graphql_variables",
                lambda: graphql(store.path, "{profile}", _store=store, variables=[]),
            ),
        ):
            error(key, action)
        output["standalone_sql"] = sql(store.path, "SELECT 42 AS answer")
        output["standalone_graphql"] = graphql(store.path, "{profile schemaVersion}")
        output["sql_options"] = sql(
            "ignored-path",
            "SELECT :n,:profile,9223372036854775807",
            profile="alternate",
            params='{"n":3}',
            _store=store,
            _http=True,
        )
        output["graphql_variables_text"] = graphql(
            "ignored-path",
            "query Q($id:ID!){item(id:$id){id title}}",
            variables='{"id":"a"}',
            operation_name="Q",
            _store=store,
        )
        output["graphql_validation"] = graphql(
            store.path, "{absentField}", _store=store
        )
        output["graphql_operations"] = graphql(
            store.path,
            "query A{profile} query B{schemaVersion}",
            operation_name="B",
            _store=store,
        )
        # These are independently executed against each cloned engine. The
        # frozen order is limit -> query lookup -> native session identity.
        profile_session = EvaluationSession(store, "alternate")
        with Store(store.path) as other_store:
            owner_session = EvaluationSession(other_store)
            for label, mismatch in (
                ("profile", profile_session),
                ("owner", owner_session),
            ):
                for suffix, action in (
                    (
                        "select_missing",
                        lambda mismatch=mismatch: queries.select(
                            "missing", session=mismatch
                        ),
                    ),
                    (
                        "run_missing_bad_limit",
                        lambda mismatch=mismatch: queries.run(
                            "missing", 0, session=mismatch
                        ),
                    ),
                    (
                        "run_missing",
                        lambda mismatch=mismatch: queries.run(
                            "missing", session=mismatch
                        ),
                    ),
                    (
                        "select_existing",
                        lambda mismatch=mismatch: queries.select(
                            records["left"]["id"], session=mismatch
                        ),
                    ),
                    (
                        "run_existing_bad_limit",
                        lambda mismatch=mismatch: queries.run(
                            records["left"]["id"], 0, session=mismatch
                        ),
                    ),
                    (
                        "run_existing",
                        lambda mismatch=mismatch: queries.run(
                            records["left"]["id"], session=mismatch
                        ),
                    ),
                ):
                    error("session_precedence_" + label + "_" + suffix, action)
        shared = EvaluationSession(store)
        output["session_first"] = queries.select(records["left"]["id"], session=shared)
        output["session_cached"] = queries.select(records["left"]["id"], session=shared)
        output["session_rows"] = queries.run(records["rows"]["id"], session=shared)
        output["session_document"] = queries.run(
            records["document"]["id"], session=shared
        )
        shared.cancelled = True
        error(
            "session_cancelled",
            lambda: queries.select(records["left"]["id"], session=shared),
        )
        shared.cancelled = False
        small = EvaluationSession(store, max_bytes=1)
        error(
            "session_byte_budget",
            lambda: queries.run(records["rows"]["id"], session=small),
        )
        bounded = EvaluationSession(store, max_ids=1)
        error(
            "session_id_budget",
            lambda: queries.select(records["left"]["id"], session=bounded),
        )
        try:
            with store.transaction():
                created = queries.put("rolled_back", definitions()["left"])
                assert queries.get(created["id"])["name"] == "rolled_back"
                output["uncommitted_sql"] = sql(
                    store.path,
                    "SELECT name FROM saved_queries WHERE name='rolled_back'",
                    _store=store,
                )
                output["uncommitted_graphql"] = graphql(
                    store.path,
                    "query($id:ID!){savedQuery(id:$id)}",
                    variables={"id": created["id"]},
                    _store=store,
                )
                ids[created["id"]] = "ROLLED_BACK_ID"
                raise ValueError("rollback")
        except ValueError:
            pass
        error("rolled_back_get", lambda: queries.get(created["id"]))
        app.put_item("movie", {}, {"title": "Changed"}, "changed")
        error(
            "session_stale",
            lambda: queries.select(records["left"]["id"], session=shared),
        )
        output["owner_writable_after_queries"] = app.add_profile("still-writable")
        output["query_count"] = store.rows("SELECT count(*) AS n FROM saved_queries")

    def normalize(value):
        if isinstance(value, dict):
            return {
                key: normalize(item)
                for key, item in value.items()
                if key != "created_at"
            }
        if isinstance(value, (tuple, list)):
            return [normalize(item) for item in value]
        if isinstance(value, set):
            return sorted(value)
        return ids.get(value, value) if isinstance(value, str) else value

    return normalize(output)


class InstalledQueries(unittest.TestCase):
    def setUp(self):
        (
            self.Application,
            self.Store,
            self.Queries,
            self.sql,
            self.graphql,
            self.Session,
            self.Error,
            _,
        ) = api("native")
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / "catalog.db"
        self.Store.initialize(self.path)

    def writer(self):
        store = self.Store(self.path, writable=True)
        self.addCleanup(store.close)
        return store

    def test_standalone_defaults_and_validation_precedence(self):
        self.assertEqual(self.sql(self.path, "SELECT 42 AS answer")["rows"], [[42]])
        self.assertEqual(
            self.graphql(self.path, "{profile}")["data"]["profile"], "default"
        )
        absent = self.path.parent / "does-not-exist.db"
        with self.assertRaisesRegex(self.Error, "provide one SQL statement"):
            self.sql(absent)
        self.assertIn("errors", self.graphql(absent, "{absentField}"))
        with self.assertRaisesRegex(self.Error, "max-rows"):
            self.sql(absent, "SELECT 1", max_rows=True)
        with self.assertRaisesRegex(self.Error, "--params"):
            self.sql(absent, "SELECT 1", params="[]")
        self.assertFalse(absent.exists())

    def test_nested_rollback_and_recovery(self):
        store = self.writer()
        queries = self.Queries(store)
        with store.transaction():
            outer = queries.put("outer", definitions()["left"])
            try:
                with store.transaction():
                    inner = queries.put("inner", definitions()["left"])
                    self.graphql(self.path, "{profile}", _store=store)
                    with self.assertRaises(self.Error):
                        self.sql(self.path, "SELECT missing FROM missing", _store=store)
                    raise ValueError("rollback inner")
            except ValueError:
                pass
            self.assertEqual(queries.get(outer["id"])["name"], "outer")
            with self.assertRaises(self.Error):
                queries.get(inner["id"])
            self.Application(store).add_profile("after-errors")
        self.assertEqual(len(queries.list()["queries"]), 1)

    def test_snapshot_closed_detached_readonly(self):
        with self.writer() as setup:
            setup.db.execute("PRAGMA journal_mode=WAL")
        with self.Store(self.path) as reader:
            self.assertEqual(
                self.sql(
                    self.path, "SELECT count(id) FROM saved_queries", _store=reader
                )["rows"],
                [[0]],
            )
            with self.writer() as writer:
                queries = self.Queries(writer)
                queries.put("new", definitions()["left"])
                self.assertEqual(self.Queries(reader).list()["queries"], [])
                self.assertEqual(
                    self.graphql(self.path, "{savedQueries{nodes}}", _store=reader)[
                        "data"
                    ]["savedQueries"]["nodes"],
                    [],
                )
                with self.assertRaises(self.Error):
                    self.Queries(reader).put("no", definitions()["left"])
                with writer.detached():
                    for action in (
                        lambda: queries.list(),
                        lambda: self.sql(self.path, "SELECT 1", _store=writer),
                        lambda: self.graphql(self.path, "{profile}", _store=writer),
                    ):
                        with self.assertRaises(self.Error):
                            action()
                self.assertEqual(len(queries.list()["queries"]), 1)
            self.assertEqual(self.Queries(reader).list()["queries"], [])
        for action in (
            lambda: self.Queries(reader).list(),
            lambda: self.sql(self.path, "SELECT 1", _store=reader),
            lambda: self.graphql(self.path, "{profile}", _store=reader),
        ):
            with self.assertRaises(self.Error):
                action()

    def test_native_session_validation_lookup_precedence(self):
        store = self.writer()
        app, queries = self.Application(store), self.Queries(store)
        app.add_profile("alternate")
        saved = queries.put("left", definitions()["left"])
        profile_session = self.Session(store, "alternate")
        with self.Store(self.path) as other_store:
            owner_session = self.Session(other_store)
            for mismatch in (profile_session, owner_session):
                with self.assertRaisesRegex(self.Error, "unknown query revision"):
                    queries.select("missing", session=mismatch)
                with self.assertRaisesRegex(self.Error, "limit must be between"):
                    queries.run("missing", 0, session=mismatch)
                with self.assertRaisesRegex(self.Error, "unknown query revision"):
                    queries.run("missing", session=mismatch)
                with self.assertRaisesRegex(self.Error, "identity_mismatch"):
                    queries.select(saved["id"], session=mismatch)
                with self.assertRaisesRegex(self.Error, "limit must be between"):
                    queries.run(saved["id"], 0, session=mismatch)
                with self.assertRaisesRegex(self.Error, "identity_mismatch"):
                    queries.run(saved["id"], session=mismatch)
        self.assertEqual(queries.select(saved["id"])[1], set())

    def test_session_identity_budgets_and_detach(self):
        store = self.writer()
        app, queries = self.Application(store), self.Queries(store)
        app.put_item("movie", {}, {}, "a")
        saved = queries.put("left", definitions()["left"])
        session = self.Session(store)
        self.assertEqual(queries.select(saved["id"], session=session)[1], {"a"})
        with self.Store(self.path) as other:
            with self.assertRaisesRegex(self.Error, "identity_mismatch"):
                self.Queries(other).select(saved["id"], session=session)
        with store.detached():
            with self.assertRaises(self.Error):
                session.check(store, "default")
        self.assertEqual(queries.select(saved["id"], session=session)[1], {"a"})
        session.cancelled = True
        with self.assertRaisesRegex(self.Error, "evaluation_cancelled"):
            queries.select(saved["id"], session=session)
        session.cancelled = False
        session.pause_for_io(-60)
        with self.assertRaisesRegex(self.Error, "timed out"):
            queries.select(saved["id"], session=session)

    def test_foreign_contexts_are_explicit(self):
        store = self.writer()
        with self.assertRaisesRegex(self.Error, "_context_factory is unsupported"):
            self.graphql(
                self.path, "{profile}", _store=store, _context_factory=lambda *_: None
            )
        with self.assertRaisesRegex(self.Error, "foreign Python"):
            self.Queries(store).select("unused", session=object())
        with self.assertRaisesRegex(self.Error, "Python Access"):
            self.Session(store, access=object())
        with self.assertRaisesRegex(self.Error, "_store requires"):
            self.sql(self.path, "SELECT 1", _store=object())
        self.Application(store).add_profile("still-writable")

    def test_thread_and_lost_transaction_refusal(self):
        store = self.writer()
        outcomes = []
        with store.transaction():

            def thread():
                try:
                    self.graphql(self.path, "{profile}", _store=store)
                except Exception as exc:
                    outcomes.append(str(exc))

            worker = threading.Thread(target=thread)
            worker.start()
            worker.join()
            self.assertIn("another thread", outcomes[0])
        with self.assertRaises(self.Error):
            with store.transaction() as db:
                try:
                    db.execute("INSERT OR ROLLBACK INTO profiles VALUES ('default')")
                except self.Error:
                    pass
                for action in (
                    lambda: self.Queries(store).list(),
                    lambda: self.sql(self.path, "SELECT 1", _store=store),
                    lambda: self.graphql(self.path, "{profile}", _store=store),
                ):
                    with self.assertRaises(self.Error):
                        action()
        self.Application(store).add_profile("recovered")

    def test_sigint_sql_and_saved_query_restore_owner(self):
        store = self.writer()
        query = "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) FROM n"
        saved = self.Queries(store).put(
            "slow",
            {
                "mode": "rows",
                "timeout_ms": 60000,
                "selection": {"language": "sql", "query": query},
            },
        )
        for action in (
            lambda: self.sql(self.path, query, timeout_ms=60000, _store=store),
            lambda: self.Queries(store).run(saved["id"]),
        ):
            with self.assertRaises(KeyboardInterrupt):
                with store.transaction():
                    self.Application(store).add_profile("rolled-back")
                    timer = threading.Timer(
                        0.08, lambda: os.kill(os.getpid(), signal.SIGINT)
                    )
                    timer.start()
                    try:
                        action()
                    finally:
                        timer.join()
            self.assertEqual(
                store.rows("SELECT id FROM profiles WHERE id='rolled-back'"), []
            )
            self.assertEqual(
                self.sql(self.path, "SELECT 1", _store=store)["rows"], [[1]]
            )
        self.Application(store).add_profile("after-interrupt")

    def test_graphql_signal_restores_owner(self):
        store = self.writer()
        with store.transaction() as db:
            db.execute(
                "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<4000) INSERT INTO items(id,kind,metadata) SELECT printf('%08d',x),'movie',json_object('title',printf('%08d',x),'padding',printf('%2000s','')) FROM n"
            )
        observed = []

        class QuerySignal(Exception):
            pass

        def handler(*_):
            try:
                self.Queries(store).list()
            except self.Error as exc:
                observed.append(str(exc))
            raise QuerySignal("GraphQL signal")

        previous = signal.signal(signal.SIGUSR1, handler)
        self.addCleanup(signal.signal, signal.SIGUSR1, previous)
        with self.assertRaises(QuerySignal):
            with store.transaction():
                self.Application(store).add_profile("graphql-rollback")
                timer = threading.Timer(
                    0.02, lambda: os.kill(os.getpid(), signal.SIGUSR1)
                )
                timer.start()
                try:
                    self.graphql(
                        self.path,
                        "{items(first:1000){nodes{id title metadata identities{namespace value} workflow{profile status revision}}}}",
                        _store=store,
                        timeout_ms=60000,
                    )
                finally:
                    timer.join()
        self.assertTrue(observed and "already in use" in observed[0], observed)
        self.assertEqual(
            store.rows("SELECT id FROM profiles WHERE id='graphql-rollback'"), []
        )
        self.Application(store).add_profile("graphql-after-signal")
        self.assertEqual(
            self.graphql(self.path, "{profile}", _store=store)["data"]["profile"],
            "default",
        )

    def test_graphql_execution_error_restores_owner(self):
        store = self.writer()
        with store.transaction():
            self.Application(store).add_profile("outer")
            with self.assertRaises(self.Error):
                self.graphql(self.path, "{profile}", profile="absent", _store=store)
            self.assertIn(
                "errors",
                self.graphql(self.path, "{items(first:0){nodes{id}}}", _store=store),
            )
            self.assertEqual(
                self.graphql(self.path, "{profile}", _store=store)["data"]["profile"],
                "default",
            )
            self.Application(store).add_profile("after-graphql")
        self.assertEqual(
            len(
                store.rows(
                    "SELECT id FROM profiles WHERE id IN ('outer','after-graphql')"
                )
            ),
            2,
        )


EXPECTED_TESTS = (
    "test_standalone_defaults_and_validation_precedence",
    "test_nested_rollback_and_recovery",
    "test_snapshot_closed_detached_readonly",
    "test_native_session_validation_lookup_precedence",
    "test_session_identity_budgets_and_detach",
    "test_foreign_contexts_are_explicit",
    "test_thread_and_lost_transaction_refusal",
    "test_sigint_sql_and_saved_query_restore_owner",
    "test_graphql_signal_restores_owner",
    "test_graphql_execution_error_restores_owner",
)


def main():
    if not __debug__:
        raise SystemExit(
            "query qualification requires assertions; do not run with -O or -OO"
        )
    parser = argparse.ArgumentParser()
    parser.add_argument("--wheel", type=Path)
    parser.add_argument(
        "--dev",
        action="store_true",
        help="Explicit helperless development evidence; never release/wheel qualification",
    )
    parser.add_argument("--inventory", type=Path)
    parser.add_argument("--reference-root", type=Path)
    parser.add_argument("--reference-python", type=Path)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--scenario", choices=("reference", "native"))
    parser.add_argument("--root", type=Path)
    parser.add_argument("--reference-commit")
    args = parser.parse_args()
    if args.report:
        args.report.unlink(missing_ok=True)
    provenance = sibling("qualify_native_embedding")
    if args.scenario:
        oracle, calls = None, {}
        if args.scenario == "reference":
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
        __debug__
        and (args.wheel or args.dev)
        and args.inventory
        and args.reference_root
        and args.reference_python
    )
    assert not (args.wheel and args.dev), (
        "dev and strict wheel evidence are separate modes"
    )
    installed = (
        sibling("qualify_native_wheel").verify_installed_wheel(args.wheel)
        if args.wheel
        else None
    )
    inventory = json.loads(args.inventory.read_text())
    rows = [row for row in inventory["rows"] if row["id"] in CONTRACTS]
    assert len(rows) == len(CONTRACTS) == 10
    oracle = provenance.reference_provenance(
        args.reference_root, inventory["reference_commit"]
    )
    calls = trace()
    result = sibling("native_qualification_results").run_required_suite(
        unittest.defaultTestLoader.loadTestsFromTestCase(InstalledQueries),
        {InstalledQueries: EXPECTED_TESTS},
        expected_count=10,
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
                "-X",
                f"pycache_prefix={root / (engine + '-pycache')}",
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
                command, cwd=directory, capture_output=True, text=True, timeout=120
            )
            assert completed.returncode == 0, (
                engine,
                completed.stdout,
                completed.stderr,
            )
            outputs[engine] = json.loads(completed.stdout)
        reference, native = outputs["reference"]["result"], outputs["native"]["result"]
        assert reference == native, json.dumps(
            {
                key: [reference.get(key), native.get(key)]
                for key in reference.keys() | native.keys()
                if reference.get(key) != native.get(key)
            },
            indent=2,
        )
        assert outputs["reference"]["oracle"] == oracle
        paired_calls = outputs["native"]["calls"]
        oracle["imported_modules"] = outputs["reference"]["imports"]
    if args.wheel:
        assert (
            sibling("qualify_native_wheel").verify_installed_wheel(args.wheel)
            == installed
        )
    report = dict(
        format=1,
        scope="Separate persistent-query checkpoint; unchanged existing 69 rows / 19 tests / 82 groups",
        acceptance_tests=result.testsRun,
        successful_scenarios=result.successful_scenarios,
        paired_groups=len(native),
        installed_wheel=installed,
        wheel_sha256=hashlib.sha256(args.wheel.read_bytes()).hexdigest()
        if args.wheel
        else None,
        development_only=args.dev,
        inventory_sha256=hashlib.sha256(args.inventory.read_bytes()).hexdigest(),
        oracle=oracle,
        installed_calls=installed_calls,
        paired_calls=paired_calls,
        coverage=[],
    )
    for row in rows:
        key = row["id"] + (".__init__" if row["signature"]["kind"] == "class" else "")
        report["coverage"].append(
            dict(
                id=row["id"],
                classification=row["classification"],
                qualification="selected-contracts-tested-with-gaps"
                if paired_calls.get(key)
                else "unqualified",
                installed_calls=installed_calls.get(key, 0),
                paired_calls=paired_calls.get(key, 0),
                **CONTRACTS[row["id"]],
            )
        )
    import catabolic_native

    package = Path(catabolic_native.__file__).parent
    report["installed_package_sha256"] = {
        str(path.relative_to(package)): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in sorted(package.rglob("*"))
        if path.is_file() and "__pycache__" not in path.parts
    }
    report["script_sha256"] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    report["qualification_scripts_sha256"] = {
        name: hashlib.sha256(Path(__file__).with_name(name).read_bytes()).hexdigest()
        for name in (
            "native_query_embedding_tests.py",
            "qualify_native_embedding.py",
            "qualify_native_wheel.py",
            "native_qualification_results.py",
        )
    }
    assert provenance.reference_provenance(
        args.reference_root, inventory["reference_commit"]
    ) == {key: value for key, value in oracle.items() if key != "imported_modules"}
    if args.report:
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(
        json.dumps(
            dict(
                tests=result.testsRun,
                paired_groups=len(native),
                inventory_rows=len(rows),
                report=str(args.report),
            )
        )
    )


if __name__ == "__main__":
    main()
