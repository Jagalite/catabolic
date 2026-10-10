# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Populated component evidence, language normalization and cursor parity."""

import json
import subprocess
import unittest

from catabolic.graphql_query import execute_graphql
from catabolic.sql_query import execute_sql
from tests import test_components as fixtures
from tests import test_rust_queries as runner
from tests.test_rust_migration import BINARY


class NativeComponentQueryTest(unittest.TestCase):
    file = fixtures.ComponentsTest.file
    probe = fixtures.ComponentsTest.probe
    native = runner.NativeQueryTest.native
    close_worker = runner.NativeQueryTest.close_worker

    def setUp(self):
        fixtures.ComponentsTest.setUp(self)
        file = self.file("movie.mkv")
        self.probe(
            file,
            [
                {
                    "index": 0,
                    "codec_type": "video",
                    "codec_name": "h264",
                    "width": 1920,
                    "height": 1080,
                },
                {
                    "index": 1,
                    "codec_type": "audio",
                    "codec_name": "aac",
                    "tags": {"language": "eng"},
                },
                {
                    "index": 2,
                    "codec_type": "subtitle",
                    "codec_name": "subrip",
                    "tags": {"language": "fra"},
                    "disposition": {"forced": 1},
                },
            ],
        )
        self.file(
            "movie.en.srt", role="subtitle", metadata={"language": "en", "forced": True}
        )
        self.store.close()
        self.path = self.database
        self.worker = subprocess.Popen(
            [str(BINARY), "--stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(self.close_worker)

    def test_component_views_and_language_filter(self):
        before = self.path.read_bytes()
        for view in (
            "catalog_components",
            "catalog_component_occurrences",
            "catalog_component_lineage",
        ):
            sql = f"SELECT * FROM {view} ORDER BY 1,2"
            self.assertEqual(self.native("query", sql), execute_sql(self.path, sql))
        for language in ("eng", "en", "FRA", "und"):
            query = f'{{ componentOccurrences(language:"{language}",first:1) {{ nodes {{ id fileId itemId kind language forced current technicallyVerified observed asserted effective conflicts }} pageInfo {{ hasNextPage endCursor }} }} }}'
            expected = json.loads(json.dumps(execute_graphql(self.path, query)))
            actual = self.native("graphql", query)
            self.assertEqual(actual, expected, language)
        self.assertEqual(before, self.path.read_bytes())
