# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Immutable native fallback policies against independent Python normalization."""

import json
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
from pathlib import Path

from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.fallback_policies import Policies
from catabolic.saved_queries import Queries
from catabolic.store import Store
from tests.test_rust_migration import BINARY


class NativeFallbackPolicyTest(unittest.TestCase):
    def setUp(self):
        self.assertTrue(BINARY.is_file(), "build catabolic-cli before qualification")
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.path = self.root / "native.db"
        self.reference = self.root / "reference.db"
        Store.initialize(self.path)
        with Store(self.path, writable=True) as store:
            Application(store).add_profile("other")
            queries = Queries(store)
            self.query = queries.put(
                "sources",
                {
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS file_id FROM files",
                    }
                },
            )["id"]
            self.other_query = Queries(store, "other").put(
                "other",
                {
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS file_id FROM files",
                    }
                },
            )["id"]
        shutil.copyfile(self.path, self.reference)

    def native(self, action, args, ok=True):
        run = subprocess.run(
            [
                str(BINARY),
                "--db",
                str(self.path),
                "--json",
                "operation",
                "fallback-policy",
                action,
                "--options",
                json.dumps(args),
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(run.returncode, 0 if ok else 2, run.stdout + run.stderr)
        return json.loads(run.stdout) if ok else run.stderr

    def test_defaults_rankings_packages_and_immutable_dependencies(self):
        base = {"fallbacks": [{"name": "primary", "query_id": self.query}]}
        variants = [
            base,
            {
                **base,
                "version": True,
                "package": None,
                "within_tier": {"tie_break": None},
            },
            {**base, "version": 1.0, "failback": {"mode": "manual"}},
            {
                **base,
                "within_tier": {
                    "require": {"language": "en"},
                    "prefer": [{"field": "size", "order": "desc", "values": None}],
                    "tie_break": "file_id",
                },
            },
            {
                **base,
                "within_tier": {
                    "prefer": [
                        {"field": "location", "order": None, "values": ["a", "b", None]}
                    ]
                },
            },
            {
                **base,
                "package": {
                    "requirements": [
                        {
                            "name": "audio",
                            "fallbacks": [{"name": "audio", "query_id": self.query}],
                        }
                    ],
                    "operation_id": None,
                    "supported_codecs": None,
                },
            },
            {
                **base,
                "package": {
                    "requirements": [
                        {
                            "name": "subtitle",
                            "required": False,
                            "fallbacks": [{"name": "all", "query_id": self.query}],
                        }
                    ],
                    "publication": "selected_only",
                    "supported_codecs": ["subrip"],
                },
            },
            {
                **base,
                "budgets": {
                    "max_entries": 1,
                    "max_candidates": 100000,
                    "max_checks": 1,
                    "query_timeout_ms": 30000,
                    "probe_timeout_ms": 10,
                },
            },
        ]
        for index, definition in enumerate(variants):
            with self.subTest(index=index):
                name = f"p{index}"
                native = self.native("put", {"name": name, "definition": definition})
                with Store(self.reference, writable=True) as store:
                    python = Policies(store).put(name, definition)
                for key in ("profile", "name", "revision", "definition", "digest"):
                    self.assertEqual(native[key], python[key], key)
                self.assertEqual(
                    self.native("put", {"name": name, "definition": definition}), native
                )
                with sqlite3.connect(self.path) as db:
                    self.assertEqual(
                        db.execute(
                            "SELECT query_id FROM fallback_dependencies WHERE policy_id=?",
                            (native["id"],),
                        ).fetchall(),
                        [(self.query,)],
                    )
        self.assertEqual(
            len(self.native("list", {"limit": 100})["policies"]), len(variants)
        )

    def test_invalid_policy_and_cross_profile_dependencies_leave_no_revisions(self):
        base = {"fallbacks": [{"name": "primary", "query_id": self.query}]}
        invalid = [
            {**base, "extra": True},
            {**base, "budgets": {"max_checks": True}},
            {**base, "fallbacks": []},
            {**base, "fallbacks": base["fallbacks"] * 2},
            {**base, "within_tier": {"prefer": [{"field": "size"}]}},
            {**base, "within_tier": {"require": {"secret": True}}},
            {**base, "fallbacks": [{"name": "other", "query_id": self.other_query}]},
            {
                **base,
                "package": {
                    "requirements": [{"name": "video", "fallbacks": base["fallbacks"]}]
                },
            },
            {
                **base,
                "slot_roles": ["subtitle"],
                "package": {
                    "requirements": [{"name": "audio", "fallbacks": base["fallbacks"]}]
                },
            },
        ]
        for value in invalid:
            with self.subTest(value=value):
                with Store(self.reference, writable=True) as store:
                    with self.assertRaises(CatabolicError):
                        Policies(store).put("invalid", value)
                self.native("put", {"name": "invalid", "definition": value}, ok=False)
        with sqlite3.connect(self.path) as db:
            self.assertEqual(
                db.execute("SELECT count(*) FROM fallback_policies").fetchone(), (0,)
            )
            self.assertEqual(
                db.execute("SELECT count(*) FROM fallback_dependencies").fetchone(),
                (0,),
            )


if __name__ == "__main__":
    unittest.main()
