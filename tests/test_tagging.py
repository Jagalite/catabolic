import contextlib
import copy
import io
import json
import tempfile
import unittest
from pathlib import Path

from jsonschema import Draft202012Validator

from catabolic.app import Application
from catabolic.cli import main
from catabolic.domain import CatabolicError
from catabolic.graphql_query import execute_graphql
from catabolic.interchange import specification
from catabolic.interchange.validation import (
    decode_document,
    document_value,
    encode_document,
    validate_document,
)
from catabolic.layouts import PRESETS, Layouts
from catabolic.manifest import Manifest, digest
from catabolic.migration import SCHEMA_VERSION, load_migrations, upgrade_database
from catabolic.reconcile import Reconciler
from catabolic.sql_query import execute_sql
from catabolic.store import Store
from tests.test_media_model import populate_media
from tests.test_migrations import contents, create_legacy


class TaggingTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.path = self.root / "catalog.sqlite3"
        Store.initialize(self.path)
        self.store = Store(self.path, writable=True)
        self.addCleanup(self.store.close)
        self.app = Application(self.store)
        self.files, _ = populate_media(self.app, self.root)
        for tag in (
            "genre:fiction",
            "genre:science-fiction",
            "genre:space-opera",
            "mood:hopeful",
            "workflow:review",
        ):
            self.app.tags.put(tag)
        self.app.tags.parent("genre:science-fiction", "genre:fiction")
        self.app.tags.parent("genre:space-opera", "genre:science-fiction")
        self.app.tags.alias("genre:science-fiction", "genre:sci-fi")

    def items(self, **kwargs):
        return [row["id"] for row in self.app.queries.items(**kwargs)["items"]]

    def test_normalized_aliases_rename_and_reserved_names(self):
        first = self.app.tags.put(" Subject: CAFÉ ", description="A subject")
        self.assertEqual(first["name"], "subject:café")
        self.assertEqual(self.app.tags.put("subject:cafe\u0301")["id"], first["id"])
        self.app.tags.alias("subject:café", "coffee")
        self.assertEqual(self.app.tags.put("COFFEE")["id"], first["id"])
        self.app.tags.assign(["coffee"], items=["track1"])
        renamed = self.app.tags.rename("coffee", "subject:coffee")
        self.assertEqual(first["id"], renamed["id"])
        self.assertEqual(self.items(tags=["subject:café"]), ["track1"])
        self.app.tags.alias("coffee", "coffee", remove=True)
        with self.assertRaisesRegex(CatabolicError, "unknown tag"):
            self.items(tags=["coffee"])
        with self.assertRaisesRegex(CatabolicError, "canonical"):
            self.app.tags.alias("subject:coffee", "subject:coffee", remove=True)
        for action in (self.app.tags.alias, self.app.tags.rename):
            with self.assertRaisesRegex(CatabolicError, "another tag"):
                action("genre:fiction", "subject:café")

    def test_dag_cycles_and_parent_removal(self):
        self.app.tags.parent("genre:space-opera", "mood:hopeful")
        before = self.store.rows("SELECT * FROM tag_parents")
        for child, parent in (
            ("genre:fiction", "genre:space-opera"),
            ("genre:sci-fi", "genre:science-fiction"),
        ):
            with self.assertRaisesRegex(CatabolicError, "cycle"):
                self.app.tags.parent(child, parent)
        self.assertEqual(self.store.rows("SELECT * FROM tag_parents"), before)
        self.assertEqual(len(self.app.queries.tags(parent="genre:sci-fi")["tags"]), 1)
        self.app.tags.parent("genre:space-opera", "mood:hopeful", remove=True)
        self.assertEqual(self.app.queries.tags(parent="mood:hopeful")["tags"], [])

    def test_provenance_withdrawal_reactivation_and_idempotence(self):
        human = self.app.tags.assign(
            ["genre:sci-fi"], items=["track1"], note="Reviewed"
        )
        before = self.store.rows("SELECT * FROM item_tags")
        self.assertEqual(
            self.app.tags.assign(["genre:sci-fi"], items=["track1"], note="Reviewed"),
            human,
        )
        self.assertEqual(self.store.rows("SELECT * FROM item_tags"), before)
        agent = self.app.tags.assign(
            ["genre:sci-fi"],
            items=["track1"],
            source="agent:curator",
            confidence=0.8,
            note="Evidence",
        )
        self.app.tags.assign(
            ["genre:sci-fi"], items=["track1"], source="agent:curator", remove=True
        )
        self.assertEqual(self.items(tags=["genre:sci-fi"]), ["track1"])
        all_rows = self.app.queries.taggings(item="track1", active="all")["taggings"]
        self.assertEqual(len(all_rows), 2)
        self.assertEqual(
            next(r for r in all_rows if r["source"] == "agent:curator")["note"],
            "Evidence",
        )
        self.app.tags.assign(["genre:sci-fi"], items=["track1"], remove=True)
        self.assertEqual(self.items(tags=["genre:sci-fi"]), [])
        new = self.app.tags.assign(
            ["genre:sci-fi"], items=["track1"], source="agent:curator"
        )
        self.assertEqual(new["assignments"][0]["id"], agent["assignments"][0]["id"])

    def test_atomic_batches_and_invalid_inputs(self):
        for kwargs in (
            {"items": ["track1", "z-unknown"]},
            {"items": ["track1"], "files": ["unknown"]},
        ):
            with self.assertRaises(CatabolicError):
                self.app.tags.assign(["workflow:review"], **kwargs)
            self.assertEqual(self.store.rows("SELECT * FROM item_tags"), [])
        for name in ("", "genre:", "bad space:tag", "line\nfeed", "x" * 256):
            with self.assertRaises(CatabolicError):
                self.app.tags.put(name)
        for confidence in (True, -0.1, 1.1, float("nan"), float("inf")):
            with self.assertRaises(CatabolicError):
                self.app.tags.assign(
                    ["workflow:review"], items=["track1"], confidence=confidence
                )
        with self.assertRaises(CatabolicError):
            self.app.tags.assign(["workflow:review"] * 1001, items=["track1"])
        with self.assertRaises(CatabolicError):
            self.app.tags.assign(["unknown"], items=["track1"])

    def test_boolean_descendant_filters_and_no_implicit_media_inheritance(self):
        self.app.tags.assign(["genre:space-opera", "mood:hopeful"], items=["track1"])
        self.app.tags.assign(["genre:sci-fi"], items=["track2"])
        self.app.tags.assign(["workflow:review"], files=[self.files["track01.flac"]])
        self.assertEqual(self.items(tags=["genre:fiction"]), [])
        self.assertEqual(
            self.items(tags=["genre:fiction"], descendants=True), ["track1", "track2"]
        )
        self.assertEqual(
            self.items(tags=["genre:fiction", "mood:hopeful"], descendants=True),
            ["track1"],
        )
        self.assertEqual(
            self.items(any_tags=["mood:hopeful", "genre:sci-fi"]), ["track1", "track2"]
        )
        self.assertEqual(
            self.items(kind="track", not_tags=["genre:sci-fi"], descendants=True), []
        )
        self.assertEqual(self.items(tags=["workflow:review"]), [])
        self.assertEqual(
            len(self.app.queries.files(tags=["workflow:review"])["files"]), 1
        )
        self.assertEqual(
            self.app.queries.files(tags=["genre:sci-fi"], descendants=True)["files"], []
        )
        self.app.add_profile("offline")
        self.assertEqual(
            Application(self.store, "offline").queries.items(tags=["genre:sci-fi"])[
                "items"
            ][0]["id"],
            "track2",
        )

    def test_paginated_queries_and_cursor_scope(self):
        self.app.tags.assign(["workflow:review"], items=["track1", "track2"])
        first = self.app.queries.items(tags=["workflow:review"], limit=1)
        second = self.app.queries.items(
            tags=["workflow:review"], limit=1, cursor=first["next_cursor"]
        )
        self.assertEqual(second["items"][0]["id"], "track2")
        with self.assertRaisesRegex(CatabolicError, "another query"):
            self.app.queries.items(
                tags=["workflow:review"], descendants=True, cursor=first["next_cursor"]
            )
        first = self.app.queries.tags(namespace="genre", limit=1)
        self.assertIsNotNone(first["next_cursor"])
        second = self.app.queries.tags(
            namespace="genre", limit=1, cursor=first["next_cursor"]
        )
        self.assertNotEqual(first["tags"][0]["id"], second["tags"][0]["id"])
        self.assertEqual(len(self.app.queries.tags(search="sci-fi")["tags"]), 1)
        first = self.app.queries.taggings(limit=1)
        second = self.app.queries.taggings(limit=1, cursor=first["next_cursor"])
        self.assertNotEqual(first["taggings"][0]["id"], second["taggings"][0]["id"])

    def test_sql_graphql_and_readonly(self):
        self.app.tags.assign(
            ["genre:sci-fi"], items=["track1"], source="agent:test", confidence=0.9
        )
        self.app.tags.assign(["workflow:review"], files=[self.files["track01.flac"]])
        before = self.path.read_bytes()
        sql = execute_sql(
            self.path,
            "SELECT subject_id FROM catalog_taggings WHERE active=1 AND subject_type='item' AND tag_name=:tag",
            params='{"tag":"genre:science-fiction"}',
        )
        self.assertEqual(sql["rows"], [["track1"]])
        result = execute_graphql(
            self.path,
            """{ items(tags:["genre:fiction"], descendants:true) { nodes { id taggings { nodes { tagName source confidence active } } } }
          files(tags:["workflow:review"]) { nodes { id taggings { nodes { subjectType } } } }
          tags(search:"sci-fi") { nodes { name aliases parentIds } }
          taggings(source:"agent:test") { nodes { subjectId } } }""",
        )
        self.assertNotIn("errors", result, result)
        self.assertEqual(
            result["data"]["items"]["nodes"][0]["taggings"]["nodes"][0]["source"],
            "agent:test",
        )
        self.assertEqual(result["data"]["taggings"]["nodes"], [{"subjectId": "track1"}])
        self.assertEqual(self.path.read_bytes(), before)
        bad = execute_graphql(self.path, '{items(tags:["unknown"]) {nodes {id}}}')
        self.assertIn("errors", bad)

    def test_tag_query_folders_refresh_and_preserve_sources(self):
        self.app.tags.assign(["workflow:review"], items=["track1", "track2"])
        (self.root / "review").mkdir()
        self.app.bind("output", "review", self.root / "review")
        layouts = Layouts(self.app)
        for language, query in (
            (
                "sql",
                "SELECT DISTINCT subject_id AS item_id FROM catalog_taggings WHERE active=1 AND subject_type='item' AND tag_name='workflow:review'",
            ),
            (
                "graphql",
                'query($after: String) { items(tags:["workflow:review"], after:$after) {nodes {id} pageInfo {hasNextPage endCursor}} }',
            ),
        ):
            definition = copy.deepcopy(PRESETS["flat"])
            definition["selection"] = {"language": language, "query": query}
            layouts.put("review", definition)
            self.assertEqual(layouts.run("review", "review")["desired_count"], 2)
        source = self.root / "media/track01.flac"
        before = (source.read_bytes(), source.stat().st_mtime_ns, source.stat().st_ino)
        layouts.run("review", "review", apply=True)
        self.assertTrue(Reconciler(self.app).apply("review")["healthy"])
        link = self.root / "review/track/track1/track01.flac"
        self.assertTrue(link.is_symlink())
        self.app.tags.assign(["workflow:review"], items=["track1"], remove=True)
        layouts.run("review", "review", apply=True)
        self.assertTrue(Reconciler(self.app).apply("review")["healthy"])
        self.assertFalse(link.is_symlink())
        self.assertEqual(
            (source.read_bytes(), source.stat().st_mtime_ns, source.stat().st_ino),
            before,
        )

    def manifest(self):
        self.app.tags.assign(["genre:space-opera"], items=["track1"])
        self.app.tags.assign(["workflow:review"], files=[self.files["track01.flac"]])
        self.app.tags.assign(
            ["workflow:review"], files=[self.files["track01.flac"]], remove=True
        )
        layouts = Layouts(self.app)
        layouts.put("flat", PRESETS["flat"])
        layouts.run("flat", apply=True)
        with self.store.transaction():
            return Manifest(self.app).build()

    def test_manifest_v2_tag_closure_withdrawals_and_extensions(self):
        value = self.manifest()
        self.assertEqual(value["format_version"], 3)
        content = value["content"]
        self.assertEqual(
            {t["name"] for t in content["tags"]},
            {
                "genre:space-opera",
                "genre:science-fiction",
                "genre:fiction",
                "workflow:review",
            },
        )
        self.assertEqual(len(content["taggings"]), 2)
        self.assertEqual(len(content["tag_parents"]), 2)
        self.assertIn("genre:sci-fi", {row["name"] for row in content["tag_names"]})
        content["taggings"][0]["vendor:evidence"] = {"keep": [1, None]}
        value["content_sha256"] = digest(content)
        self.assertEqual(document_value(validate_document(value)), value)
        Draft202012Validator(specification.schema()).validate(value)

    def test_manifest_rejects_dangling_duplicate_cyclic_and_invalid_tags(self):
        original = self.manifest()
        for mutation in (
            lambda c: c["taggings"][0].update(subject_id="absent"),
            lambda c: c["taggings"][0].update(confidence=2),
            lambda c: c["tag_names"][0].update(tag_id="absent"),
            lambda c: c["tag_names"][0].update(name="UNNORMALIZED"),
            lambda c: c["tag_parents"][0].update(
                parent_id=c["tag_parents"][0]["child_id"]
            ),
            lambda c: c["taggings"][1].update(**c["taggings"][0]),
        ):
            value = copy.deepcopy(original)
            mutation(value["content"])
            value["content_sha256"] = digest(value["content"])
            with self.assertRaises(CatabolicError):
                validate_document(value)

    def test_v2_numeric_confidence_and_extensions_roundtrip_without_coercion(self):
        value = self.manifest()
        for confidence in (None, 0, 1, 0.0, 1.0, 0.123):
            value["content"]["taggings"][0]["confidence"] = confidence
            value["content_sha256"] = digest(value["content"])
            raw = encode_document(validate_document(value))
            decoded = document_value(decode_document(raw))
            actual = decoded["content"]["taggings"][0]["confidence"]
            self.assertIs(type(actual), type(confidence))
            self.assertEqual(decoded["content_sha256"], value["content_sha256"])

    def test_alias_limits_and_full_atomic_batch(self):
        for index in range(100):
            self.app.tags.alias("workflow:review", f"review-{index}")
        with self.assertRaisesRegex(CatabolicError, "100 aliases"):
            self.app.tags.alias("workflow:review", "one-too-many")
        with self.assertRaisesRegex(CatabolicError, "100 aliases"):
            self.app.tags.rename("workflow:review", "workflow:new")
        # Promoting an existing alias does not allocate another name.
        self.app.tags.rename("workflow:review", "review-0")
        ids = [f"bulk-{index}" for index in range(1000)]
        with self.store.transaction() as db:
            db.executemany(
                "INSERT INTO items VALUES (?,'custom:fixture','{}')",
                [(key,) for key in ids],
            )
        batch = self.app.tags.assign(["workflow:review"], items=ids)
        self.assertEqual(batch["count"], 1000)
        result = self.app.queries.items(tags=["workflow:review"], limit=1000)
        self.assertEqual(len(result["items"]), 1000)
        self.assertIsNone(result["next_cursor"])

    def test_cli_json_mutations_filters_and_errors(self):
        # CLI obtains its own writer lock and snapshots.
        self.store.close()

        def run(*args, code=0):
            out, err = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                status = main(["--db", str(self.path), "--json", *args])
            self.assertEqual(status, code, err.getvalue())
            return json.loads(out.getvalue() if code == 0 else err.getvalue())

        run("tag", "put", "collection:favorites")
        run("tag", "alias", "collection:favorites", "favorite")
        run(
            "tag",
            "add",
            "favorite",
            "--item",
            "track1",
            "--item",
            "track2",
            "--source",
            "agent:test",
            "--confidence",
            "0.7",
        )
        self.assertEqual(len(run("item", "list", "--tag", "favorite")["items"]), 2)
        run("tag", "rename", "favorite", "collection:best")
        self.assertEqual(
            len(run("tag", "assignments", "--tag", "favorite")["taggings"]), 2
        )
        run("tag", "remove", "favorite", "--item", "track1", "--source", "agent:test")
        self.assertEqual(len(run("item", "list", "--tag", "favorite")["items"]), 1)
        run("tag", "add", "favorite", "--item", "track1", "--confidence", "nan", code=2)
        self.assertEqual(
            len(run("tag", "list", "--namespace", "collection")["tags"]), 1
        )


class TagMigrationTest(unittest.TestCase):
    def test_v3_upgrade_rehearses_and_preserves_every_old_row_and_link(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = create_legacy(root)
            upgrade_database(path, migrations=load_migrations()[:3])
            before = contents(path)
            original = path.read_bytes()
            self.assertEqual(upgrade_database(path, dry_run=True)["schema"], 3)
            self.assertEqual(path.read_bytes(), original)
            result = upgrade_database(path)
            self.assertEqual(
                [step["version"] for step in result["applied"]],
                list(range(4, SCHEMA_VERSION + 1)),
            )
            self.assertEqual(contents(Path(result["backup"])), before)
            self.assertEqual(
                {key: value for key, value in contents(path).items() if key in before},
                before,
            )
            with Store(path, writable=True) as store:
                app = Application(store)
                app.tags.put("workflow:review")
                app.tags.assign(["workflow:review"], items=["item-a"])
                self.assertEqual(
                    len(app.queries.items(tags=["workflow:review"])["items"]), 1
                )
            self.assertTrue((root / "plex/Movies/Test.mkv").is_symlink())
