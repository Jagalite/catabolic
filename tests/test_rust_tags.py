# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Populated tag hierarchy, aliases, association provenance and filters."""

import json
import subprocess
import unittest

from catabolic.app import Application
from catabolic.graphql_query import execute_graphql
from catabolic.store import Store
from tests import test_rust_queries as runner
from tests import test_tagging as fixtures
from tests.test_rust_migration import BINARY


class NativeTagQueryTest(unittest.TestCase):
    native = runner.NativeQueryTest.native
    close_worker = runner.NativeQueryTest.close_worker

    def setUp(self):
        fixtures.TaggingTest.setUp(self)
        self.app.tags.assign(
            ["genre:space-opera", "mood:hopeful"],
            items=["movie"],
            source="fixture",
            confidence=0.75,
            note="retained",
        )
        self.app.tags.assign(["workflow:review"], files=[self.files["movie.mkv"]])
        self.store.close()
        self.worker = subprocess.Popen(
            [str(BINARY), "--stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(self.close_worker)

    def test_catalog_tag_pages_filters_and_status(self):
        with Store(self.path) as store:
            query = Application(store).queries
            for entity, options in (
                ("tags", {"limit": 2}),
                ("tags", {"search": "sci-fi"}),
                ("taggings", {"item": "movie", "limit": 1}),
                ("items", {"tags": ["genre:fiction"], "descendants": True}),
                ("items", {"any_tags": ["mood:hopeful", "genre:sci-fi"]}),
                ("files", {"tags": ["workflow:review"]}),
            ):
                with self.subTest(entity=entity, options=options):
                    expected = getattr(query, entity)(**options)
                    self.assertEqual(
                        self.native(
                            "catalog", entity, "--options", json.dumps(options)
                        ),
                        expected,
                    )
            self.assertEqual(
                self.native("catalog", "status"), Application(store).status()
            )

    def test_graphql_nested_tags(self):
        document = '{ tags(first:2) { nodes { id name aliases description parentIds } pageInfo { hasNextPage endCursor } } taggings { nodes { id subjectType subjectId active confidence source note tagId tagName } } item(id:"movie") { id taggings { nodes { id tagId tagName } } } }'
        expected = json.loads(json.dumps(execute_graphql(self.path, document)))
        self.assertEqual(self.native("graphql", document), expected)

    def test_tag_filter_rejects_unicode_other_and_expanded_names(self):
        from catabolic.domain import CatabolicError

        for tag in ("zero\u200bwidth", "private\ue000", "unassigned\u0378", "ß" * 255):
            with Store(self.path) as store, self.assertRaises(CatabolicError) as error:
                Application(store).queries.items(tags=[tag])
            self.assertEqual(
                self.native(
                    "catalog",
                    "items",
                    "--options",
                    json.dumps({"tags": [tag]}),
                    ok=False,
                ),
                "catabolic: " + str(error.exception),
            )
