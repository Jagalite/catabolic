# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Unchanged tag workflow assertions executed through the native mutation owner.

Python owns disposable fixture creation and independent persisted-state reads.
Every tested tag mutation runs in the native executable with its own writer lock.
"""

import json
import subprocess
import unittest
from datetime import datetime

from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.store import Store
from tests import test_tagging as reference
from tests.test_rust_migration import BINARY


class NativeTags:
    def __init__(self, owner):
        self.owner = owner

    def call(self, operation, **request):
        owner = self.owner
        owner.store.close()
        try:
            run = subprocess.run(
                [
                    str(BINARY),
                    "--db",
                    str(owner.path),
                    "tag",
                    operation,
                    "--options",
                    json.dumps(request),
                ],
                capture_output=True,
                text=True,
                timeout=30,
            )
            if run.returncode:
                owner.assertEqual(run.returncode, 2, run.stderr)
                raise CatabolicError(run.stderr.removeprefix("catabolic: ").strip())
            return json.loads(run.stdout)
        finally:
            owner.store = Store(owner.path, writable=True)
            owner.app = Application(owner.store)
            owner.app.tags = self

    def put(self, name, **options):
        return self.call("put", name=name, **options)

    def rename(self, name, new_name):
        return self.call("rename", name=name, new_name=new_name)

    def alias(self, name, alias, **options):
        return self.call("alias", name=name, alias=alias, **options)

    def parent(self, child, parent, **options):
        return self.call("parent", child=child, parent=parent, **options)

    def assign(self, names, **options):
        return self.call("assign", names=names, **options)


@unittest.skipUnless(BINARY.is_file(), "build catabolic-cli before qualification")
class NativeTagMutationTest(unittest.TestCase):
    items = reference.TaggingTest.items

    def setUp(self):
        reference.TaggingTest.setUp(self)
        self.addCleanup(lambda: self.store.close())
        self.app.tags = NativeTags(self)

    def test_generated_timestamp_and_noop_preservation(self):
        before = datetime.now().astimezone()
        result = self.app.tags.assign(["workflow:review"], items=["track1"])
        after = datetime.now().astimezone()
        row = self.store.rows("SELECT * FROM item_tags")[0]
        self.assertLessEqual(before, datetime.fromisoformat(row["created_at"]))
        self.assertLessEqual(datetime.fromisoformat(row["created_at"]), after)
        self.assertEqual(row["created_at"], row["updated_at"])
        self.assertEqual(result["assignments"][0]["id"], row["id"])
        self.assertEqual(
            self.app.tags.assign(["workflow:review"], items=["track1"]), result
        )
        self.assertEqual(self.store.rows("SELECT * FROM item_tags")[0], row)

    def test_differential_validation_and_existing_tag_updates(self):
        from catabolic.tagging import tag_name

        for value in (
            "",
            "line\nfeed",
            "private\ue000",
            "unassigned\u0378",
            "ß" * 255,
            "wrong namespace:tag",
            "genre:",
            3,
            None,
        ):
            with self.subTest(value=value):
                with self.assertRaises(CatabolicError) as expected:
                    tag_name(value)
                with self.assertRaises(CatabolicError) as actual:
                    self.app.tags.put(value)
                self.assertEqual(str(expected.exception), str(actual.exception))
        row = self.store.rows("SELECT * FROM tags WHERE name='genre:fiction'")[0]
        actual = self.app.tags.put("  GENRE: Fiction  ", description="Reviewed Café")
        self.assertEqual(actual, dict(row, description="Reviewed Café"))


for _name in (
    "test_normalized_aliases_rename_and_reserved_names",
    "test_dag_cycles_and_parent_removal",
    "test_provenance_withdrawal_reactivation_and_idempotence",
    "test_atomic_batches_and_invalid_inputs",
    "test_boolean_descendant_filters_and_no_implicit_media_inheritance",
    "test_paginated_queries_and_cursor_scope",
    "test_sql_graphql_and_readonly",
    "test_alias_limits_and_full_atomic_batch",
):
    setattr(NativeTagMutationTest, _name, getattr(reference.TaggingTest, _name))
