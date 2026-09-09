# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import contextlib
import copy
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from xml.etree import ElementTree as ET

from catabolic.app import Application
from catabolic.cli import main
from catabolic.consumer_adapters import ConsumerError, Plex
from catabolic.consumer_setup import put_connection
from catabolic.plex_import import mappings, run
from catabolic.store import Store


class PlexImportTest(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.database = self.root / "catalog.db"
        self.source = self.root / "source"
        self.source.mkdir()
        (self.source / "movie.mkv").write_bytes(b"original media")
        Store.initialize(self.database)
        with Store(self.database, writable=True) as store:
            app = Application(store)
            app.bind("source", "seed", str(self.source))
            app.scan()
        self.server = "server-one"
        self.library_uuid = "library-one"
        self.library_type = "movie"
        self.items = [self.item("1", "/media/movie.mkv")]
        self.calls = []
        self.bad_page = {}
        mock = patch.object(Plex, "call", side_effect=self.protocol)
        mock.start()
        self.addCleanup(mock.stop)
        put_connection(
            self.database,
            "default",
            "home",
            "plex",
            "https://plex.test:32400",
            "PLEX_TEST_TOKEN",
            apply=True,
        )
        self.mapping = [{"remote_root": "/media", "location": "seed"}]

    def item(self, key, filename, kind="movie"):
        node = ET.Element(
            "Video",
            ratingKey=key,
            type=kind,
            title="Example",
            year="2024",
            guid="plex://movie/example",
        )
        ET.SubElement(node, "Guid", id="imdb://tt1234567")
        media = ET.SubElement(node, "Media")
        ET.SubElement(media, "Part", file=filename)
        return node

    def protocol(self, route, method="GET", params=None):
        self.assertEqual(method, "GET")
        # The read must not be inside a Store writer lifetime.
        with Store(self.database, writable=True):
            pass
        self.calls.append((route, params))
        if route == "/":
            return ET.tostring(
                ET.Element(
                    "MediaContainer", machineIdentifier=self.server, version="fixture"
                )
            )
        if route == "/library/sections":
            root = ET.Element("MediaContainer")
            lib = ET.SubElement(
                root,
                "Directory",
                key="7",
                uuid=self.library_uuid,
                type=self.library_type,
                title="Cinema",
                createdAt="1",
            )
            ET.SubElement(lib, "Location", path="/media")
            return ET.tostring(root)
        self.assertEqual(route, "/library/sections/7/all")
        self.assertEqual(params["includeGuids"], 1)
        self.assertEqual(params["sort"], "id:asc")
        offset, limit = (
            params["X-Plex-Container-Start"],
            params["X-Plex-Container-Size"],
        )
        rows = self.items[offset : offset + limit]
        root = ET.Element(
            "MediaContainer",
            totalSize=str(len(self.items)),
            offset=str(offset),
            size=str(len(rows)),
        )
        root.attrib.update(self.bad_page)
        root.extend(rows)
        return ET.tostring(root)

    def preview(self, **kwargs):
        return run(self.database, "default", "home", "7", self.mapping, **kwargs)

    def apply(self, preview=None, **kwargs):
        preview = preview or self.preview(**kwargs)
        return self.preview(apply=True, expected_plan=preview["plan_id"], **kwargs)

    def rows(self, table):
        with Store(self.database) as store:
            return store.rows("SELECT * FROM " + table)

    def test_preview_apply_repeat_and_provenance(self):
        before = self.database.read_bytes()
        preview = self.preview()
        self.assertEqual(self.database.read_bytes(), before)
        self.assertEqual(len(preview["candidates"]), 1)
        self.assertEqual(self.rows("items"), [])
        done = self.apply(preview)
        self.assertTrue(done["complete"])
        self.assertEqual(len(done["imported"]), 1)
        self.assertEqual(len(self.rows("items")), 1)
        self.assertEqual(len(self.rows("item_files")), 1)
        proposal = self.rows("proposals")[0]
        self.assertEqual(proposal["state"], "accepted")
        self.assertEqual(
            json.loads(proposal["evidence"])["guids"], ["imdb://tt1234567"]
        )
        repeat = self.apply()
        self.assertEqual(repeat["imported"], [])
        self.assertEqual(len(self.rows("proposals")), 1)
        self.assertEqual((self.source / "movie.mkv").read_bytes(), b"original media")
        self.assertEqual(self.rows("consumer_bindings"), [])
        self.assertEqual(self.rows("mappings"), [])

    def test_requires_reviewed_plan_and_detects_remote_changes(self):
        with self.assertRaisesRegex(ConsumerError, "requires_expected_plan"):
            self.preview(apply=True)
        preview = self.preview()
        self.items[0].set("title", "Changed")
        with self.assertRaisesRegex(ConsumerError, "plan_changed"):
            self.apply(preview)
        self.assertEqual(self.rows("items"), [])

    def test_detects_local_change_and_keeps_existing_association(self):
        preview = self.preview()
        with Store(self.database, writable=True) as store:
            app = Application(store)
            file_id = store.rows("SELECT id FROM files")[0]["id"]
            item_id = app.put_item(
                "movie", {"manual": "original"}, {"title": "My title"}
            )["id"]
            app.media.associate(file_id, item_id)
        with self.assertRaisesRegex(ConsumerError, "plan_changed"):
            self.apply(preview)
        result = self.apply()
        self.assertFalse(result["complete"])
        self.assertIn("existing_association_conflict", result["deferred"][0]["reason"])
        self.assertEqual(len(self.rows("items")), 1)

    def test_preserves_edited_metadata_and_adds_missing_fields(self):
        self.apply()
        with Store(self.database, writable=True) as store:
            item = store.rows("SELECT * FROM items")[0]
            Application(store).put_item(
                "movie", {}, {"title": "Curated title", "year": 2024}, item["id"]
            )
        self.items[0].set("summary", "Plex summary")
        preview = self.preview()
        self.assertEqual(preview["candidates"][0]["preserved_fields"], ["title"])
        self.apply(preview)
        metadata = json.loads(self.rows("items")[0]["metadata"])
        self.assertEqual(metadata["title"], "Curated title")
        self.assertEqual(metadata["summary"], "Plex summary")
        self.assertEqual(self.apply()["imported"], [])

    def test_pagination_and_resume(self):
        (self.source / "second.mkv").write_bytes(b"second")
        with Store(self.database, writable=True) as store:
            Application(store).scan()
        self.items.append(self.item("2", "/media/second.mkv"))
        first = self.apply(limit=1)
        self.assertEqual(first["next_offset"], 1)
        self.assertFalse(first["complete"])
        self.assertTrue(first["page_complete"])
        second = self.apply(offset=1, limit=1)
        self.assertTrue(second["complete"])
        self.assertEqual(len(self.rows("items")), 2)
        self.assertEqual(self.apply(limit=1)["imported"], [])

    def test_unmapped_missing_and_duplicate_paths(self):
        for filename in ("/elsewhere/movie.mkv", "/media/absent.mkv"):
            with self.subTest(filename=filename):
                self.items = [self.item("1", filename)]
                result = self.apply()
                self.assertFalse(result["complete"])
                self.assertEqual(result["imported"], [])
        self.items = [
            self.item("1", "/media/movie.mkv"),
            self.item("2", "/media/movie.mkv"),
        ]
        result = self.apply()
        self.assertEqual(result["candidates"], [])
        self.assertEqual(len(result["deferred"]), 2)
        self.assertEqual(self.rows("items"), [])

    def test_changed_source_deferred(self):
        preview = self.preview()
        (self.source / "movie.mkv").write_bytes(b"changed media bytes")
        with self.assertRaisesRegex(ConsumerError, "plan_changed"):
            self.apply(preview)
        self.assertFalse(self.preview()["complete"])
        self.assertEqual(self.rows("items"), [])

    def test_server_and_library_identity_changes(self):
        preview = self.preview()
        self.server = "replacement"
        with self.assertRaisesRegex(ConsumerError, "server_identity_changed"):
            self.apply(preview)
        self.server = "server-one"
        self.library_uuid = "replacement"
        with self.assertRaisesRegex(ConsumerError, "plan_changed"):
            self.apply(preview)

    def test_bad_pagination_and_duplicate_ids_rejected(self):
        for bad in (
            {"offset": "1"},
            {"size": "2"},
            {"totalSize": "-1"},
            {"totalSize": "0"},
        ):
            with self.subTest(bad=bad):
                self.bad_page = bad
                with self.assertRaises(ConsumerError):
                    self.preview()
        self.bad_page = {}
        self.items.append(copy.deepcopy(self.items[0]))
        with self.assertRaisesRegex(ConsumerError, "duplicate_import_rating_key"):
            self.preview()

    def test_path_mapping_validation(self):
        for mapping in (
            [{"remote_root": "/media/../secret", "location": "seed"}],
            [
                {"remote_root": "/media", "location": "seed"},
                {"remote_root": "/media/sub", "location": "seed"},
            ],
            [{"remote_root": "/media", "location": "seed", "subtree": "../bad"}],
        ):
            with self.subTest(mapping=mapping), self.assertRaises(ConsumerError):
                mappings(mapping)

    def test_subtree_mapping(self):
        nested = self.source / "nested"
        nested.mkdir()
        (nested / "other.mkv").write_bytes(b"nested")
        with Store(self.database, writable=True) as store:
            Application(store).scan()
        self.mapping[0]["subtree"] = "nested"
        self.items = [self.item("3", "/media/other.mkv")]
        self.assertEqual(self.apply()["candidates"][0]["path"], "nested/other.mkv")

    def test_multipart_and_editions_stay_distinct(self):
        (self.source / "part2.mkv").write_bytes(b"part2")
        (self.source / "edition.mkv").write_bytes(b"edition")
        with Store(self.database, writable=True) as store:
            Application(store).scan()
        ET.SubElement(self.items[0].find("Media"), "Part", file="/media/part2.mkv")
        self.items.append(self.item("2", "/media/edition.mkv"))
        self.items[1].set("editionTitle", "Extended")
        done = self.apply()
        self.assertEqual(done["errors"], [])
        self.assertEqual(len(done["imported"]), 3)
        self.assertEqual(len(self.rows("items")), 2)
        self.assertEqual(
            sorted(a["part"] for a in self.rows("item_files") if a["part"] is not None),
            [1, 2],
        )
        self.assertEqual(self.apply()["imported"], [])

    def test_episode_track_and_photo_metadata(self):
        for library_type, kind, extra, expected in (
            (
                "show",
                "episode",
                {"grandparentTitle": "Series", "parentIndex": "0", "index": "1"},
                {"series": "Series", "season": 0, "episode": 1},
            ),
            (
                "artist",
                "track",
                {"grandparentTitle": "Artist", "parentTitle": "Album", "index": "2"},
                {"artist": "Artist", "album": "Album", "track": 2},
            ),
            ("photo", "photo", {}, {}),
        ):
            with self.subTest(kind=kind):
                self.library_type = library_type
                self.items = [self.item("1", "/media/movie.mkv", kind)]
                self.items[0].attrib.update(extra)
                result = self.preview()
                metadata = result["candidates"][0]["payload"]["item"]["metadata"]
                for key, value in expected.items():
                    self.assertEqual(metadata[key], value)

    def test_unsupported_and_unavailable_items_are_explicit(self):
        self.items[0].set("type", "clip")
        result = self.preview()
        self.assertFalse(result["complete"])
        self.assertEqual(
            result["deferred"][0]["reason"], "unsupported_or_incomplete_item"
        )
        self.library_type = "unsupported"
        with self.assertRaisesRegex(ConsumerError, "unsupported_import_library_type"):
            self.preview()

    def test_cli_preview_and_apply(self):
        mapping_file = self.root / "map.json"
        mapping_file.write_text(json.dumps(self.mapping))
        argv = [
            "--db",
            str(self.database),
            "--json",
            "consumer",
            "import",
            "home",
            "--library-id",
            "7",
            "--map",
            str(mapping_file),
        ]
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            self.assertEqual(main(argv), 0)
        preview = json.loads(output.getvalue())
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            self.assertEqual(
                main(argv + ["--apply", "--expected-plan", preview["plan_id"]]), 0
            )
        result = json.loads(output.getvalue())
        self.assertEqual(len(result["imported"]), 1)
        self.assertNotIn("PLEX_TEST_TOKEN", output.getvalue())
        self.assertNotIn("credential_env", output.getvalue())

    def test_library_changes_during_read_rejected(self):
        original = self.protocol

        def replaced(route, method="GET", params=None):
            result = original(route, method, params)
            if route.endswith("/all"):
                self.library_uuid = "replaced-during-read"
            return result

        with patch.object(Plex, "call", side_effect=replaced):
            with self.assertRaisesRegex(ConsumerError, "library_identity_changed"):
                self.preview()
        self.assertEqual(self.rows("items"), [])

    def test_guid_rematch_requires_review(self):
        self.apply()
        self.items[0].set("guid", "plex://movie/different")
        result = self.apply()
        self.assertFalse(result["complete"])
        self.assertIn("plex_item_guid_changed", result["deferred"][0]["reason"])
        self.assertEqual(len(self.rows("items")), 1)

    def test_interrupted_batch_resumes_without_duplicate_first_item(self):
        from catabolic.curation import Curation

        (self.source / "second.mkv").write_bytes(b"second")
        with Store(self.database, writable=True) as store:
            Application(store).scan()
        self.items.append(self.item("2", "/media/second.mkv"))
        decide = Curation.decide
        calls = []

        def interrupt(curator, identifier, **kwargs):
            calls.append(identifier)
            if len(calls) == 2:
                raise KeyboardInterrupt()
            return decide(curator, identifier, **kwargs)

        preview = self.preview()
        with (
            patch.object(Curation, "decide", interrupt),
            self.assertRaises(KeyboardInterrupt),
        ):
            self.apply(preview)
        self.assertEqual(len(self.rows("items")), 1)
        done = self.apply()
        self.assertEqual(len(done["imported"]), 1)
        self.assertEqual(len(self.rows("items")), 2)
        self.assertEqual(len(self.rows("proposals")), 2)

    def test_empty_library_and_invalid_page_bounds(self):
        self.items = []
        self.assertTrue(self.apply()["complete"])
        self.assertEqual(self.rows("items"), [])
        for kwargs in ({"limit": 101}, {"limit": 0}, {"offset": -1}):
            with self.subTest(kwargs=kwargs), self.assertRaises(ConsumerError):
                self.preview(**kwargs)
