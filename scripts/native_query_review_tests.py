#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Independent installed query-adapter regressions; run with isolated Python -I.

This supplemental suite does not change either preserved-inventory gate or
qualify a helperless development wheel for release.
"""

import tempfile
import unittest
from pathlib import Path

from catabolic_native import Application, CatabolicError, Queries, Store, encode
from catabolic_native.evaluation import EvaluationSession
from catabolic_native.graphql_query import execute_graphql
from catabolic_native.sql_query import execute_sql


class QueryAdapterReview(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.TemporaryDirectory()
        self.addCleanup(self.root.cleanup)
        self.path = Path(self.root.name) / "query-review.db"
        Store.initialize(self.path)
        self.store = Store(self.path, writable=True)
        self.addCleanup(self.store.close)
        self.queries = Queries(self.store)

    def query(self, name, sql, mode="selection"):
        return self.queries.put(
            name,
            {"mode": mode, "selection": {"language": "sql", "query": sql}},
        )["id"]

    def test_reused_cache_preserves_cumulative_id_budget(self):
        first = self.query("first", "SELECT 'a' AS item_id")
        second = self.query("second", "SELECT 'b' AS item_id")
        session = EvaluationSession(self.store, max_ids=1)
        selected = self.queries.select(first, session=session)
        self.assertEqual(selected[1], {"a"})
        self.assertEqual(self.queries.select(first, session=session), selected)
        with self.assertRaisesRegex(CatabolicError, "exceeds its ID budget"):
            self.queries.select(second, session=session)
        self.assertEqual(self.queries.select(first, session=session), selected)
        self.assertEqual(
            execute_sql(None, "SELECT 7", _store=self.store)["rows"], [[7]]
        )

    def test_rows_use_exact_cumulative_byte_budget(self):
        identifier = self.query("report", "SELECT 7 AS answer", "rows")
        result = self.queries.run(identifier)
        size = len(encode(result).encode())
        session = EvaluationSession(self.store, max_bytes=size * 2)
        self.assertEqual(self.queries.run(identifier, session=session), result)
        self.assertEqual(self.queries.run(identifier, session=session), result)
        with self.assertRaisesRegex(CatabolicError, "evaluation_result_byte_budget"):
            self.queries.run(identifier, session=session)
        self.assertEqual(self.queries.run(identifier), result)

    def test_cached_selection_is_not_charged_twice(self):
        identifier = self.query("selection", "SELECT 'a' AS item_id")
        entity, identifiers, report = self.queries.select(identifier)
        size = len(encode([entity, sorted(identifiers), report]).encode())
        session = EvaluationSession(self.store, max_bytes=size)
        expected = (entity, identifiers, report)
        self.assertEqual(self.queries.select(identifier, session=session), expected)
        self.assertEqual(self.queries.select(identifier, session=session), expected)
        with self.assertRaisesRegex(CatabolicError, "evaluation_result_byte_budget"):
            session.account(None)

    def test_account_error_preserves_budget_and_session_lifecycle(self):
        session = EvaluationSession(self.store, max_bytes=2)
        with self.assertRaisesRegex(CatabolicError, "evaluation_result_byte_budget"):
            session.account("too large")
        session.account(0)
        session.account(0)
        with self.assertRaisesRegex(CatabolicError, "evaluation_result_byte_budget"):
            session.account(0)
        session.close()
        session.close()
        with self.assertRaisesRegex(CatabolicError, "identity_mismatch"):
            session.check(self.store, "default")
        fresh = EvaluationSession(self.store, max_bytes=2)
        fresh.account(0)
        fresh.check(self.store, "default")

    def test_detached_external_mutation_invalidates_cached_epoch(self):
        identifier = self.query("selection", "SELECT 'a' AS item_id")
        session = EvaluationSession(self.store)
        self.queries.select(identifier, session=session)
        with self.store.detached():
            with Store(self.path, writable=True) as other:
                Application(other).put_item("movie", {}, {}, "external")
        for action in (
            lambda: session.check(self.store, "default"),
            lambda: self.queries.select(identifier, session=session),
            lambda: self.queries.run(identifier, session=session),
        ):
            with self.assertRaisesRegex(CatabolicError, "stale_evaluation_snapshot"):
                action()
        fresh = EvaluationSession(self.store)
        self.assertEqual(self.queries.select(identifier, session=fresh)[1], {"a"})

    def test_graphql_error_and_sql_timeout_preserve_enclosing_scope(self):
        with self.store.transaction():
            Application(self.store).add_profile("outer")
            with self.assertRaisesRegex(CatabolicError, "timed out"):
                execute_sql(
                    "ignored-path",
                    "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) FROM n",
                    timeout_ms=1,
                    _store=self.store,
                )
            self.assertTrue(self.store.db.in_transaction)
            result = execute_graphql(
                "ignored-path", "{items(first:0){nodes{id}}}", _store=self.store
            )
            self.assertIn("errors", result)
            self.assertTrue(self.store.db.in_transaction)
            self.assertEqual(
                execute_graphql("ignored-path", "{profile}", _store=self.store)["data"],
                {"profile": "default"},
            )
            Application(self.store).add_profile("after")
        self.assertEqual(
            self.store.rows(
                "SELECT id FROM profiles WHERE id IN ('outer','after') ORDER BY id"
            ),
            [{"id": "after"}, {"id": "outer"}],
        )

    def test_explicit_cancellation_can_be_cleared_and_expiration_extended(self):
        identifier = self.query("selection", "SELECT 'a' AS item_id")
        session = EvaluationSession(self.store)
        session.cancelled = True
        with self.assertRaisesRegex(CatabolicError, "evaluation_cancelled"):
            self.queries.select(identifier, session=session)
        session.cancelled = False
        self.assertEqual(self.queries.select(identifier, session=session)[1], {"a"})
        session.pause_for_io(-60)
        with self.assertRaisesRegex(CatabolicError, "timed out"):
            session.check(self.store, "default")
        session.pause_for_io(60)
        self.assertEqual(self.queries.select(identifier, session=session)[1], {"a"})

    def test_http_session_rejects_local_graphql_and_recovers_owner(self):
        identifier = self.queries.put(
            "document",
            {
                "mode": "document",
                "selection": {"language": "graphql", "query": "{profile}"},
            },
        )["id"]
        session = EvaluationSession(self.store, http=True)
        with self.assertRaisesRegex(CatabolicError, "requires authorization context"):
            self.queries.run(identifier, session=session)
        Application(self.store).add_profile("after-http-refusal")
        self.assertEqual(self.queries.run(identifier)["data"], {"profile": "default"})


if __name__ == "__main__":
    unittest.main(verbosity=2)
