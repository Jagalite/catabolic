# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import json
import tempfile
import unittest
from pathlib import Path
from uuid import uuid4

from catabolic import components
from catabolic.app import Application
from catabolic.curation import Curation, occurrence
from catabolic.domain import CatabolicError
from catabolic.evaluation import EvaluationSession
from catabolic.graphql_query import execute_graphql
from catabolic.saved_queries import Queries
from catabolic.store import Store, encode


class ComponentsTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name).resolve()
        self.source = self.root / "source"
        self.source.mkdir()
        self.database = self.root / "catalog.db"
        Store.initialize(self.database)
        self.store = Store(self.database, writable=True)
        self.addCleanup(self.store.close)
        self.app = Application(self.store)
        self.app.bind("source", "media", str(self.source))
        self.item = self.app.put_item(
            "movie", {"fixture": "components"}, {"title": "Fixture", "year": 2026}
        )["id"]

    def file(self, name, role="primary", metadata=None):
        (self.source / name).write_bytes(b"fixture")
        self.app.scan()
        identifier = self.store.rows("SELECT id FROM files WHERE path=?", (name,))[0][
            "id"
        ]
        self.app.media.associate(identifier, self.item, role=role, metadata=metadata)
        return identifier

    def probe(self, file, streams):
        job = str(uuid4())
        snap = occurrence(self.store, "default", file)
        data = {"streams": streams}
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state,result) VALUES (?,'default',?,'probe','media',?,'{}',?,'complete',?)",
                (job, file, encode(snap), job, encode(data)),
            )
            db.execute(
                "INSERT INTO file_facts VALUES ('default',?,'probe',?,?,?,'complete') ON CONFLICT(profile,file_id,operation) DO UPDATE SET job_id=excluded.job_id,snapshot=excluded.snapshot,data=excluded.data,status=excluded.status",
                (file, job, encode(snap), encode(data)),
            )
            components.index_file(self.store, "default", file)

    def rows(self, where="1", params=()):
        from catabolic.component_sql import OCCURRENCES_SQL

        return self.store.rows(
            "SELECT * FROM (" + OCCURRENCES_SQL + ") WHERE " + where, params
        )

    def claim(self, row, **value):
        c = Curation(self.app)
        p = c.put(
            row["file_id"],
            {
                "item_id": self.item,
                "component": {"occurrence_id": row["occurrence_id"], **value},
            },
            evidence={"review": "fixture"},
        )
        c.decide(p["id"], accept=True, actor="test")
        return p["id"]

    def test_embedded_external_query_parity_and_unknowns(self):
        video = self.file("movie.mkv")
        self.probe(
            video,
            [
                {"index": 0, "codec_type": "video"},
                {"index": 1, "codec_type": "audio", "tags": {"language": "eng"}},
                {
                    "index": 2,
                    "codec_type": "subtitle",
                    "tags": {"language": "eng"},
                    "disposition": {"forced": 1},
                },
            ],
        )
        self.file("movie.en.srt", "subtitle", {"language": "en", "forced": True})
        query = Queries(self.store).put(
            "english-forced",
            {
                "entity": "occurrence_id",
                "selection": {
                    "language": "sql",
                    "query": "SELECT occurrence_id FROM catalog_component_occurrences WHERE profile=:profile AND kind='subtitle' AND language='en' AND forced=1 AND current=1",
                },
            },
        )
        entity, ids, _ = EvaluationSession(self.store).select(query["id"])
        self.assertEqual(entity, "occurrence_id")
        self.assertEqual(len(ids), 2)
        self.assertEqual(
            {
                r["storage"]
                for r in self.rows(
                    "occurrence_id IN (SELECT value FROM json_each(?))",
                    (encode(sorted(ids)),),
                )
            },
            {"embedded", "external"},
        )
        audio = self.rows("kind='audio'")[0]
        self.assertIsNone(audio["forced"])
        self.assertIsNone(audio["default_flag"])
        external = self.rows("storage='external'")[0]
        self.assertFalse(external["technically_verified"])
        gql = 'query($after:String){componentOccurrences(kind:"subtitle",language:"en",forced:true,current:true,after:$after){nodes{id} pageInfo{hasNextPage endCursor}}}'
        g = Queries(self.store).put(
            "gql",
            {
                "entity": "occurrence_id",
                "selection": {"language": "graphql", "query": gql},
            },
        )
        self.assertEqual(EvaluationSession(self.store).select(g["id"])[1], ids)
        combined = Queries(self.store).put(
            "both-surfaces",
            {
                "entity": "occurrence_id",
                "combine": "intersection",
                "queries": [query["id"], g["id"]],
            },
        )
        self.assertEqual(EvaluationSession(self.store).select(combined["id"])[1], ids)
        with self.assertRaisesRegex(CatabolicError, "budget"):
            EvaluationSession(self.store, max_bytes=1).select(combined["id"])
        page = execute_graphql(
            self.database,
            '{ componentOccurrences(kind:"subtitle",first:1){nodes{id} pageInfo{hasNextPage endCursor}} }',
        )
        first = page["data"]["componentOccurrences"]
        self.assertTrue(first["pageInfo"]["hasNextPage"])
        next_page = execute_graphql(
            self.database,
            'query($cursor:String){componentOccurrences(kind:"subtitle",first:1,after:$cursor){nodes{id} pageInfo{hasNextPage}}}',
            variables={"cursor": first["pageInfo"]["endCursor"]},
        )
        self.assertNotEqual(
            first["nodes"][0]["id"],
            next_page["data"]["componentOccurrences"]["nodes"][0]["id"],
        )

    def test_translations_are_distinct_until_accepted_identity(self):
        self.file("one.srt", "subtitle", {"language": "en"})
        self.file("two.srt", "subtitle", {"language": "en"})
        one, two = self.rows()
        self.assertNotEqual(one["component_id"], two["component_id"])
        self.claim(two, component_id=one["component_id"])
        self.assertEqual(
            components.get(self.store, "default", two["occurrence_id"])["component_id"],
            one["component_id"],
        )

    def test_reordered_streams_do_not_reuse_revision_identity(self):
        file = self.file("movie.mkv")
        self.probe(
            file, [{"index": 0, "codec_type": "audio", "tags": {"language": "eng"}}]
        )
        old = self.rows()[0]
        (self.source / "movie.mkv").write_bytes(b"changed file revision")
        self.app.scan()
        self.probe(
            file,
            [
                {"index": 0, "codec_type": "audio", "tags": {"language": "fra"}},
                {"index": 1, "codec_type": "audio", "tags": {"language": "eng"}},
            ],
        )
        rows = self.rows()
        self.assertEqual(len(rows), 3)
        self.assertFalse(
            components.get(self.store, "default", old["occurrence_id"])["current"]
        )
        fresh = self.rows("current=1 AND language='en'")[0]
        self.assertNotEqual(old["component_id"], fresh["component_id"])
        self.assertEqual(json.loads(fresh["locator"])["index"], 1)
        with self.assertRaises(CatabolicError):
            self.claim(old, attributes={"title": "stale"})

    def test_conflicting_flags_are_not_guessed(self):
        video = self.file("movie.mkv")
        self.probe(
            video,
            [{"index": 0, "codec_type": "subtitle", "disposition": {"forced": 0}}],
        )
        row = self.rows()[0]
        self.claim(row, attributes={"forced": True})
        row = components.get(self.store, "default", row["occurrence_id"])
        self.assertIsNone(row["forced"])
        self.assertEqual(row["conflicts"], ["forced"])
        self.assertFalse(row["observed"]["forced"])
        self.assertTrue(row["asserted"]["forced"])

    def test_components_do_not_expand_to_parent_file_selections(self):
        from catabolic.selection import selected_associations

        self.file("one.srt", "subtitle")
        with self.assertRaisesRegex(CatabolicError, "not file associations"):
            selected_associations(
                self.store,
                {
                    "language": "sql",
                    "query": "SELECT occurrence_id FROM catalog_component_occurrences",
                },
            )

    def test_component_only_grant_cannot_read_parent_or_other_components(self):
        from catabolic.access import authenticate, grant_put, issue, principal_put
        from catabolic.access.graphql import AuthorizedContext
        from catabolic.content_access import authorize

        self.file("one.srt", "subtitle", {"language": "en"})
        self.file("two.srt", "subtitle", {"language": "fr"})
        row = self.rows()[0]
        principal_put(self.store, "default", "viewer")
        grant = grant_put(
            self.store,
            "viewer",
            {
                "actions": ["metadata:read", "component:read", "content:read"],
                "occurrence_ids": [row["occurrence_id"]],
            },
        )
        access = authenticate(
            self.store, issue(self.store, "viewer", [grant["grant_id"]])["token"]
        )
        with self.assertRaises(CatabolicError):
            authorize(access, row["file_id"], row["revision"])
        result = execute_graphql(
            self.database,
            "{componentOccurrences {nodes{id} pageInfo{hasNextPage}}}",
            _store=self.store,
            _context_factory=lambda st, pr, deadline: AuthorizedContext(
                st, pr, deadline, access
            ),
        )
        self.assertFalse(result.get("errors"), result)
        self.assertEqual(
            result["data"]["componentOccurrences"]["nodes"],
            [{"id": row["occurrence_id"]}],
        )


if __name__ == "__main__":
    unittest.main()
