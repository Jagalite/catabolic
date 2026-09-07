# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Rendition semantics, provenance and migration tests without optional media tools."""

import json
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.graphql_query import execute_graphql
from catabolic.migration import load_migrations, upgrade_database, validate_preservation
from catabolic.outputs import Outputs
from catabolic.rendering import definition as recipe_definition
from catabolic.sql_query import execute_sql
from catabolic.store import Store, encode
from tests.test_migrations import contents, create_legacy


class OutputTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name).resolve()
        source = self.root / "media"
        source.mkdir()
        for filename in ("source.mkv", "external.mkv", "third.mkv"):
            (source / filename).write_bytes(filename.encode())
        self.path = self.root / "catalog.sqlite3"
        Store.initialize(self.path)
        self.store = Store(self.path, writable=True)
        self.addCleanup(self.store.close)
        self.app = Application(self.store)
        self.app.bind("source", "media", str(source))
        self.app.scan()
        self.files = {
            r["path"]: r["id"] for r in self.store.rows("SELECT * FROM files")
        }
        self.item = self.app.put_item(
            "movie", {"fixture": "original"}, {"title": "Original"}
        )["id"]
        self.app.media.associate(self.files["source.mkv"], self.item)
        self.outputs = Outputs(self.app)

    def define(self, **changes):
        return self.outputs.define(
            "custom", {"file_metadata": {"variant": "mobile"}, **changes}
        )

    def register(self, definition, **kwargs):
        return self.outputs.register(
            self.files["external.mkv"],
            self.files["source.mkv"],
            self.item,
            definition["id"],
            **kwargs,
        )

    def test_external_registration_is_idempotent_and_preserves_files_and_selection(
        self,
    ):
        value = self.define()
        before = {p.name: p.read_bytes() for p in (self.root / "media").iterdir()}
        result = self.register(value, metadata={"encoder": "user-tool"})
        self.assertEqual(
            self.register(value, metadata={"encoder": "user-tool"})["id"], result["id"]
        )
        self.assertEqual(
            (result["origin"], result["artifact_id"], result["item_id"]),
            ("registered", None, self.item),
        )
        self.assertEqual(self.store.rows("SELECT * FROM processing_jobs"), [])
        self.assertEqual(self.store.rows("SELECT * FROM processing_artifacts"), [])
        association = self.store.rows(
            "SELECT * FROM item_files WHERE file_id=?", (result["file_id"],)
        )[0]
        self.assertEqual(association["active"], 0)
        self.assertEqual(json.loads(association["metadata"]), result["metadata"])
        self.assertEqual(
            before, {p.name: p.read_bytes() for p in (self.root / "media").iterdir()}
        )
        with self.assertRaisesRegex(CatabolicError, "different output registration"):
            self.register(value, metadata={"encoder": "changed-claim"})

    def test_definition_revisions_and_validation(self):
        first = self.define()
        self.assertEqual(self.define()["id"], first["id"])
        second = self.define(role="custom:mobile")
        self.assertEqual(second["revision"], 2)
        self.assertEqual(
            self.outputs.get_definition(first["id"])["definition"]["role"], "primary"
        )
        for value in (
            [],
            {"version": True},
            {"mode": "surprise"},
            {"command": "rm"},
            {"role": "invalid"},
            {"item_metadata": {}},
            {"mode": "new_item"},
            {"file_metadata": []},
            {"file_metadata": {"x": float("nan")}},
            {"file_metadata": {"x": "x" * 65536}},
        ):
            with self.subTest(value=str(value)[:80]), self.assertRaises(CatabolicError):
                self.outputs.define("invalid", value)

    def test_distinct_item_creation_is_atomic_and_does_not_copy_provider_ids(self):
        value = self.define(
            mode="new_item",
            item_kind="movie",
            relationship="edition_of",
            item_metadata={"title": "Custom edit", "edition": "fan edit"},
        )
        original = self.app.media.relate

        def fail_after_relationship(*args, **kwargs):
            original(*args, **kwargs)
            raise RuntimeError("injected before registration commit")

        with (
            patch.object(self.app.media, "relate", side_effect=fail_after_relationship),
            self.assertRaises(RuntimeError),
        ):
            self.register(value)
        self.assertEqual(len(self.store.rows("SELECT * FROM items")), 1)
        self.assertEqual(self.store.rows("SELECT * FROM item_relationships"), [])
        self.assertEqual(self.store.rows("SELECT * FROM media_outputs"), [])
        result = self.register(value)
        self.assertNotEqual(result["item_id"], self.item)
        self.assertEqual(
            self.store.rows(
                "SELECT * FROM identities WHERE item_id=?", (result["item_id"],)
            ),
            [],
        )
        relation = self.store.rows("SELECT * FROM item_relationships")[0]
        self.assertEqual(
            (relation["source_id"], relation["target_id"], relation["kind"]),
            (result["item_id"], self.item, "edition_of"),
        )
        self.assertEqual(
            json.loads(
                self.store.rows("SELECT metadata FROM items WHERE id=?", (self.item,))[
                    0
                ]["metadata"]
            ),
            {"title": "Original"},
        )
        self.assertEqual(self.store.rows("PRAGMA foreign_key_check"), [])

    def test_custom_nonvideo_kind_and_invalid_edition_endpoints(self):
        invalid = self.define(
            mode="new_item", item_kind="photo", relationship="edition_of"
        )
        with self.assertRaisesRegex(CatabolicError, "invalid relationship"):
            self.register(invalid)
        valid = self.define(
            mode="new_item", item_kind="custom:contact_sheet", role="custom:analysis"
        )
        row = self.register(valid)
        self.assertEqual(
            self.store.rows("SELECT kind FROM items WHERE id=?", (row["item_id"],))[0][
                "kind"
            ],
            "custom:contact_sheet",
        )

    def test_existing_curated_association_is_preserved(self):
        self.app.media.associate(
            self.files["external.mkv"], self.item, metadata={"curated": True}
        )
        self.register(self.define())
        association = self.store.rows(
            "SELECT * FROM item_files WHERE file_id=?", (self.files["external.mkv"],)
        )[0]
        self.assertEqual(
            (association["active"], json.loads(association["metadata"])),
            (1, {"curated": True}),
        )

    def test_cycles_missing_files_and_unidentified_inputs_are_rejected(self):
        value = self.define()
        with self.assertRaisesRegex(CatabolicError, "cycle"):
            self.outputs.register(
                self.files["source.mkv"],
                self.files["source.mkv"],
                self.item,
                value["id"],
            )
        self.register(value)
        self.app.media.associate(self.files["external.mkv"], self.item)
        with self.assertRaisesRegex(CatabolicError, "cycle"):
            self.outputs.register(
                self.files["source.mkv"],
                self.files["external.mkv"],
                self.item,
                value["id"],
            )
        with self.assertRaisesRegex(CatabolicError, "active association"):
            self.outputs.register(
                self.files["source.mkv"],
                self.files["third.mkv"],
                self.item,
                value["id"],
            )
        (self.root / "media/third.mkv").unlink()
        with self.assertRaises((CatabolicError, OSError)):
            self.outputs.register(
                self.files["third.mkv"],
                self.files["source.mkv"],
                self.item,
                value["id"],
            )

    def test_cli_queries_paging_and_profile_isolation(self):
        value = self.define()
        first = self.register(value)
        self.outputs.register(
            self.files["third.mkv"], self.files["source.mkv"], self.item, value["id"]
        )
        page = self.outputs.list(limit=1)
        self.assertTrue(page["next_after"])
        self.assertNotEqual(
            page["outputs"][0]["id"],
            self.outputs.list(limit=1, after=page["next_after"])["outputs"][0]["id"],
        )
        sql = execute_sql(
            self.path,
            "SELECT origin,count(*) FROM catalog_renditions GROUP BY origin",
            _store=self.store,
        )
        self.assertEqual(sql["rows"], [["registered", 2]])
        result = execute_graphql(
            self.path,
            "{ renditions(first: 1) { nodes pageInfo { hasNextPage endCursor } } outputDefinitions { nodes } }",
            _store=self.store,
        )
        self.assertNotIn("errors", result)
        self.assertTrue(result["data"]["renditions"]["pageInfo"]["hasNextPage"])
        self.assertEqual(len(result["data"]["outputDefinitions"]["nodes"]), 1)
        self.store.db.execute("INSERT INTO profiles VALUES ('other')")
        self.assertEqual(
            Outputs(Application(self.store, "other")).list()["outputs"], []
        )
        with self.assertRaisesRegex(CatabolicError, "unknown media output"):
            Outputs(Application(self.store, "other")).get(first["id"])
        self.store.close()
        proc = subprocess.run(
            [
                sys.executable,
                "-m",
                "catabolic",
                "--db",
                str(self.path),
                "--json",
                "rendition",
                "define",
                "via-stdin",
                "--file",
                "-",
            ],
            input='{"role":"custom:external"}',
            capture_output=True,
            text=True,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(
            json.loads(proc.stdout)["definition"]["role"], "custom:external"
        )

    def test_structured_recipe_options_reject_command_fragments(self):
        for preset, options in (
            ("remux-mkv", {"stream_indices": [0, 0]}),
            ("remux-mkv", {"stream_indices": [True]}),
            ("remux-mkv", {"stream_indices": "0:v"}),
            ("h264-720p", {"crf": 52}),
            ("h264-720p", {"audio_stream": -1}),
            ("h264-720p", {"encoder_speed": "medium -y"}),
            ("audio-aac", {"audio_bitrate_kbps": 0}),
        ):
            with (
                self.subTest(preset=preset, options=options),
                self.assertRaises(CatabolicError),
            ):
                recipe_definition(preset, options)


class OutputMigrationTest(unittest.TestCase):
    def test_schema_eight_upgrade_preserves_immutable_recipes_and_backup(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = create_legacy(Path(temporary))
            upgrade_database(path, migrations=load_migrations()[:8])
            with sqlite3.connect(path) as db:
                db.execute(
                    "INSERT INTO processing_recipes(id,name,revision,preset,definition,digest) VALUES ('old','old',1,'remux-mkv',?,'original-digest')",
                    (encode(recipe_definition("remux-mkv", {})),),
                )
            db.close()
            before = contents(path)
            result = upgrade_database(path)
            self.assertEqual(contents(Path(result["backup"])), before)
            with Store(path) as store:
                validate_preservation(store.db, before)
                self.assertIsNone(
                    store.rows(
                        "SELECT output_definition_id FROM processing_recipes WHERE id='old'"
                    )[0]["output_definition_id"]
                )
                self.assertEqual(store.rows("SELECT * FROM media_outputs"), [])
                self.assertEqual(store.rows("PRAGMA foreign_key_check"), [])
