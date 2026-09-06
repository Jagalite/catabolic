# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import hashlib
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.media import KINDS
from catabolic.migration import (
    SCHEMA_VERSION,
    data_snapshot,
    inspect_database,
    load_migrations,
    upgrade_database,
    validate_preservation,
)
from catabolic.reconcile import Reconciler
from catabolic.sql_query import execute_sql
from catabolic.store import Store
from tests.test_migrations import create_legacy


def populate_media(app, root):
    """Small fake library exercising file formats, ordered groups, and attachments."""
    source = root / "media"
    source.mkdir()
    output = root / "library"
    output.mkdir()
    filenames = [
        "movie.mkv",
        "movie.srt",
        "track01.flac",
        "track02.flac",
        "cover.jpg",
        "book.epub",
        "book.pdf",
        "chapter01.m4b",
        "chapter02.m4b",
        "photo.raw",
        "photo.jpg",
        "podcast.mp3",
        "issue.cbz",
        "notes.txt",
    ]
    for filename in filenames:
        (source / filename).write_bytes(("synthetic media: " + filename).encode())
    app.bind("source", "media", str(source))
    app.bind("output", "global", str(output))
    app.scan()
    files = {row["path"]: row["id"] for row in app.files()["files"]}
    kinds = {
        "movie": "movie",
        "album": "album",
        "track1": "track",
        "track2": "track",
        "artist": "artist",
        "book": "book",
        "edition": "book_edition",
        "audio": "audiobook",
        "chapter1": "audiobook_chapter",
        "chapter2": "audiobook_chapter",
        "narrator": "person",
        "photos": "photo_album",
        "photo": "photo",
        "derivative": "photo",
        "podcast": "podcast",
        "podcast_ep": "podcast_episode",
        "comics": "comic_series",
        "issue": "comic_issue",
        "notes": "custom:research_notes",
    }
    for identifier, kind in kinds.items():
        app.put_item(
            kind,
            {},
            {"title": identifier, "unknown": {"retain": [True, None, "Café"]}},
            identifier,
        )
    for child, parent, kind, position in (
        ("track1", "album", "part_of", 1),
        ("track2", "album", "part_of", 2),
        ("album", "artist", "performed_by", None),
        ("edition", "book", "edition_of", None),
        ("audio", "book", "edition_of", None),
        ("audio", "narrator", "narrated_by", None),
        ("chapter1", "audio", "part_of", 1),
        ("chapter2", "audio", "part_of", 2),
        ("photo", "photos", "part_of", 1),
        ("derivative", "photo", "derived_from", None),
        ("podcast_ep", "podcast", "part_of", 1),
        ("issue", "comics", "part_of", 1),
    ):
        app.media.relate(child, parent, kind, position=position)
    entries = [
        ("movie.mkv", "movie", "primary", None, "Movies/Movie/Movie.mkv"),
        ("movie.srt", "movie", "subtitle", None, "Movies/Movie/Movie.en.srt"),
        ("track01.flac", "track1", "primary", None, "Music/Artist/Album/01.flac"),
        ("track02.flac", "track2", "primary", None, "Music/Artist/Album/02.flac"),
        ("cover.jpg", "album", "cover", None, "Music/Artist/Album/cover.jpg"),
        ("book.epub", "edition", "primary", None, "Books/Book/Book.epub"),
        ("book.pdf", "edition", "primary", None, "Books/Book/Book.pdf"),
        ("chapter01.m4b", "chapter1", "primary", None, "Audiobooks/Book/01.m4b"),
        ("chapter02.m4b", "chapter2", "primary", None, "Audiobooks/Book/02.m4b"),
        ("photo.raw", "photo", "primary", None, "Photos/Album/Original.raw"),
        ("photo.jpg", "derivative", "primary", None, "Photos/Album/Export.jpg"),
        ("podcast.mp3", "podcast_ep", "primary", None, "Podcasts/Show/01.mp3"),
        ("issue.cbz", "issue", "primary", None, "Comics/Series/01.cbz"),
        ("notes.txt", "notes", "primary", None, "Documents/Notes.txt"),
    ]
    for filename, item, role, part, destination in entries:
        app.media.associate(
            files[filename],
            item,
            role=role,
            part=part,
            metadata={"format": Path(filename).suffix[1:]},
        )
        app.put_mapping("global", files[filename], item, destination)
    return files, entries


class MediaModelTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.path = self.root / "catalog.sqlite3"
        Store.initialize(self.path)
        self.store = Store(self.path, writable=True)
        self.addCleanup(self.store.close)
        self.app = Application(self.store)
        self.files, self.entries = populate_media(self.app, self.root)

    def test_mixed_library_projects_all_formats_without_touching_sources(self):
        before = {
            p.name: (
                p.stat().st_ino,
                p.stat().st_mtime_ns,
                hashlib.sha256(p.read_bytes()).hexdigest(),
            )
            for p in (self.root / "media").iterdir()
        }
        reconciler = Reconciler(self.app)
        self.assertTrue(reconciler.apply()["healthy"])
        self.assertEqual(
            reconciler.verify()["catalogs"][0]["verified_links"], len(self.entries)
        )
        inodes = {}
        for filename, _, _, _, destination in self.entries:
            link = self.root / "library" / destination
            self.assertTrue(link.is_symlink())
            self.assertFalse(os.path.isabs(os.readlink(link)))
            self.assertEqual(link.resolve(), self.root / "media" / filename)
            inodes[destination] = link.lstat().st_ino
        self.assertEqual(reconciler.apply()["applied"], [])
        self.assertEqual(
            inodes,
            {
                destination: (self.root / "library" / destination).lstat().st_ino
                for destination in inodes
            },
        )
        self.assertEqual(
            before,
            {
                p.name: (
                    p.stat().st_ino,
                    p.stat().st_mtime_ns,
                    hashlib.sha256(p.read_bytes()).hexdigest(),
                )
                for p in (self.root / "media").iterdir()
            },
        )

    def test_identification_survives_disabling_placement_and_can_exist_without_it(self):
        mapping = self.app.queries.mappings(item="edition")["mappings"][0]
        self.app.disable_mapping(mapping["id"])
        self.assertEqual(len(self.app.files(item="edition")["files"]), 2)
        self.assertNotIn(
            mapping["file_id"],
            [r["id"] for r in self.app.files(unidentified=True)["files"]],
        )
        self.assertIn(
            mapping["file_id"],
            [r["id"] for r in self.app.files(unmapped=True)["files"]],
        )
        self.app.put_item("document", {}, {"title": "Unplaced"}, "unplaced")
        self.app.media.associate(self.files["notes.txt"], "unplaced")
        detail = self.app.queries.item("unplaced")
        self.assertEqual(detail["occurrences"], [])
        self.assertEqual(len(detail["file_associations"]["associations"]), 1)

    def test_last_identification_cannot_be_disabled_under_an_active_mapping(self):
        association = self.app.queries.associations(item="movie", role="primary")[
            "associations"
        ][0]
        with self.assertRaisesRegex(CatabolicError, "last identification"):
            self.app.media.disable_association(association["id"])
        for mapping in self.app.queries.mappings(item="movie")["mappings"]:
            self.app.disable_mapping(mapping["id"])
        self.app.media.disable_association(association["id"])
        self.assertEqual(
            len(
                self.app.queries.associations(item="movie", active="disabled")[
                    "associations"
                ]
            ),
            1,
        )
        self.assertEqual(
            self.app.media.associate(self.files["movie.mkv"], "movie")["id"],
            association["id"],
        )

    def test_movie_subtitle_and_album_cover_keep_their_roles(self):
        self.assertEqual(
            [
                r["role"]
                for r in self.app.queries.associations(item="movie", role="subtitle")[
                    "associations"
                ]
            ],
            ["subtitle"],
        )
        self.assertEqual(
            self.app.queries.associations(item="album")["associations"][0]["role"],
            "cover",
        )
        self.assertEqual(
            self.store.db.execute("SELECT count(*) FROM item_files").fetchone()[0],
            len(self.entries),
        )

    def test_album_and_audiobook_relationships_are_ordered_and_paginated(self):
        for parent, children in (
            ("album", ["track1", "track2"]),
            ("audio", ["chapter1", "chapter2"]),
        ):
            first = self.app.queries.relationships(
                item=parent, direction="incoming", kind="part_of", limit=1
            )
            second = self.app.queries.relationships(
                item=parent,
                direction="incoming",
                kind="part_of",
                limit=1,
                cursor=first["next_cursor"],
            )
            self.assertEqual(
                [
                    first["relationships"][0]["source_id"],
                    second["relationships"][0]["source_id"],
                ],
                children,
            )
            self.assertIsNone(second["next_cursor"])
            with self.assertRaisesRegex(CatabolicError, "cursor"):
                self.app.queries.relationships(item="book", cursor=first["next_cursor"])

    def test_alternate_formats_parts_and_order_conflicts(self):
        self.assertEqual(
            len(self.app.queries.associations(item="edition")["associations"]), 2
        )
        for number, filename in enumerate(("chapter01.m4b", "chapter02.m4b"), 1):
            self.app.media.associate(self.files[filename], "audio", part=number)
        page = self.app.queries.associations(item="audio", limit=1)
        self.assertEqual(page["associations"][0]["part"], 1)
        self.assertEqual(
            self.app.queries.associations(item="audio", cursor=page["next_cursor"])[
                "associations"
            ][0]["part"],
            2,
        )
        with self.assertRaisesRegex(CatabolicError, "part number"):
            self.app.media.associate(self.files["notes.txt"], "audio", part=1)
        with self.assertRaisesRegex(CatabolicError, "position"):
            self.app.media.relate("track2", "album", "part_of", position=1)

    def test_invalid_relationships_self_links_and_cross_kind_cycles_are_rejected(self):
        for args in (
            ("movie", "album", "part_of"),
            ("album", "track1", "part_of"),
            ("book", "narrator", "narrated_by"),
            ("photo", "photo", "derived_from"),
        ):
            with self.subTest(args=args), self.assertRaises(CatabolicError):
                self.app.media.relate(*args)
        for item in ("group1", "group2", "group3"):
            self.app.put_item("collection", {}, {}, item)
        self.app.media.relate("group1", "group2", "part_of")
        self.app.media.relate("group2", "group3", "derived_from")
        with self.assertRaisesRegex(CatabolicError, "cycle"):
            self.app.media.relate("group3", "group1", "part_of")

    def test_relationship_retries_disable_and_reenable_preserve_ids(self):
        first = self.app.media.relate(
            "track1",
            "album",
            "part_of",
            position=1,
            metadata={"evidence": "manual", "unknown": [None, True]},
        )
        second = self.app.media.relate("track1", "album", "part_of", position=1)
        self.assertEqual(first, second)
        self.app.media.disable_relationship(first["id"])
        self.assertEqual(
            self.app.queries.relationships(item="track1")["relationships"], []
        )
        self.assertEqual(
            self.app.media.relate("track1", "album", "part_of", position=1)["id"],
            first["id"],
        )

    def test_all_builtin_and_custom_types_roundtrip(self):
        for kind in (*KINDS, "custom:sheet_music"):
            result = self.app.put_item(
                kind, {}, {"opaque": {"a": [1, None]}}, "type-" + kind
            )
            self.assertEqual(result["kind"], kind)
            self.assertIn(
                result["id"],
                [r["id"] for r in self.app.queries.items(kind=kind)["items"]],
            )
        for kind in ("madeup", "custom:", "custom:UPPER", "custom:bad name"):
            with self.assertRaises(CatabolicError):
                self.app.put_item(kind, {}, {}, "bad")
        custom = self.app.media.associate(
            self.files["notes.txt"], "notes", role="custom:annotation"
        )
        self.assertEqual(custom["role"], "custom:annotation")
        self.app.media.relate("notes", "book", "custom:references")

    def test_order_updates_preserve_omitted_values_and_require_explicit_clearing(self):
        first = self.app.media.associate(self.files["chapter01.m4b"], "audio", part=1)
        self.assertEqual(
            self.app.media.associate(self.files["chapter01.m4b"], "audio")["part"], 1
        )
        cleared = self.app.media.associate(
            self.files["chapter01.m4b"], "audio", clear_part=True
        )
        self.assertEqual(first["id"], cleared["id"])
        self.assertIsNone(cleared["part"])
        self.assertEqual(
            self.app.media.relate("track1", "album", "part_of")["position"], 1
        )
        self.assertIsNone(
            self.app.media.relate("track1", "album", "part_of", clear_position=True)[
                "position"
            ]
        )
        for options in (
            {"part": 0},
            {"part": -1},
            {"part": True},
            {"part": 1, "clear_part": True},
            {"metadata": []},
            {"role": "invalid"},
        ):
            with self.subTest(options=options), self.assertRaises(CatabolicError):
                self.app.media.associate(
                    self.files["chapter01.m4b"], "audio", **options
                )

    def test_tv_recordings_and_contributors_use_typed_relationships(self):
        for identifier, kind in (
            ("show", "series"),
            ("season", "season"),
            ("episode", "episode"),
            ("recording", "recording"),
        ):
            self.app.put_item(kind, {}, {}, identifier)
        self.app.media.relate("season", "show", "part_of", position=1)
        self.app.media.relate("episode", "season", "part_of", position=1)
        self.app.media.relate("track1", "recording", "recording_of")
        self.app.media.relate("book", "narrator", "created_by")
        with self.assertRaises(CatabolicError):
            self.app.media.relate("book", "narrator", "created_by", position=1)

    def test_profile_specific_association_queries_are_database_only(self):
        self.app.add_profile("laptop")
        (self.root / "media").rename(self.root / "offline")
        rows = Application(self.store, "laptop").queries.associations(item="edition")[
            "associations"
        ]
        self.assertTrue(
            all(r["status"] == "unknown" and r["source_path"] is None for r in rows)
        )
        self.assertTrue(
            all(
                r["status"] == "present"
                for r in self.app.queries.associations(item="edition")["associations"]
            )
        )

    def test_new_mutations_refuse_pending_link_journal(self):
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO journal VALUES ('pending','default','global','pending','create','target',NULL)"
            )
        with self.assertRaisesRegex(CatabolicError, "pending"):
            self.app.media.associate(self.files["notes.txt"], "book")
        with self.assertRaisesRegex(CatabolicError, "pending"):
            self.app.media.relate("notes", "book", "custom:references")

    def test_sql_exposes_independent_associations_and_ordered_relationships(self):
        result = execute_sql(
            self.path,
            """SELECT r.position,a.source_relative_path FROM catalog_relationships r
            JOIN catalog_item_files a ON a.item_id=r.source_id
            WHERE r.target_id=:album AND r.kind='part_of' AND r.active=1 AND a.active=1 AND a.profile=:profile
            ORDER BY r.position""",
            params='{"album":"album"}',
        )
        self.assertEqual(result["rows"], [[1, "track01.flac"], [2, "track02.flac"]])
        self.assertEqual(
            execute_sql(
                self.path,
                "SELECT count(*) FROM catalog_item_files WHERE item_id='edition' AND profile=:profile",
            )["rows"],
            [[2]],
        )

    def test_cli_agents_discover_and_modify_model_without_sql_writes(self):
        def run(*args):
            result = subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "catabolic",
                    "--json",
                    "--db",
                    str(self.path),
                    *args,
                ],
                capture_output=True,
                text=True,
                timeout=20,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            return json.loads(result.stdout)

        self.store.close()
        self.assertIn("podcast_episode", run("item", "types")["kinds"])
        run(
            "item",
            "put",
            "--id",
            "newbook",
            "--kind",
            "book",
            "--metadata",
            '{"title":"New Book"}',
        )
        association = run(
            "association",
            "put",
            "--file",
            self.files["book.epub"],
            "--item",
            "newbook",
            "--metadata",
            '{"format":"epub"}',
        )
        self.assertEqual(
            run("association", "list", "--item", "newbook")["associations"][0]["id"],
            association["id"],
        )
        relation = run(
            "relationship",
            "put",
            "--source",
            "newbook",
            "--target",
            "narrator",
            "--kind",
            "created_by",
        )
        self.assertEqual(
            run("relationship", "list", "--item", "newbook")["relationships"][0]["id"],
            relation["id"],
        )
        self.assertEqual(
            len(run("item", "show", "newbook")["file_associations"]["associations"]), 1
        )
        run("relationship", "disable", relation["id"])
        run("association", "disable", association["id"])


class MediaMigrationTest(unittest.TestCase):
    def test_schema_one_and_two_backfill_preserves_disabled_mapping_evidence(self):
        for version in (1, 2):
            with (
                self.subTest(version=version),
                tempfile.TemporaryDirectory() as temporary,
            ):
                root = Path(temporary).resolve()
                path = create_legacy(root)
                if version == 2:
                    upgrade_database(path, migrations=load_migrations()[:2])
                db = sqlite3.connect(path)
                before = data_snapshot(db)
                db.close()
                link = root / "plex/Movies/Test.mkv"
                inode = link.lstat().st_ino
                result = upgrade_database(path)
                self.assertEqual(result["from_schema"], version)
                self.assertEqual(result["schema"], SCHEMA_VERSION)
                with Store(path) as store:
                    validate_preservation(store.db, before)
                    self.assertEqual(
                        store.rows(
                            "SELECT file_id,item_id,role,origin,active FROM item_files ORDER BY file_id"
                        ),
                        [
                            {
                                "file_id": "file-a",
                                "item_id": "item-a",
                                "role": "primary",
                                "origin": "migration",
                                "active": 1,
                            },
                            {
                                "file_id": "file-b",
                                "item_id": "item-a",
                                "role": "primary",
                                "origin": "migration",
                                "active": 1,
                            },
                        ],
                    )
                    app = Application(store)
                    self.assertEqual(len(app.files(item="item-a")["files"]), 2)
                    self.assertEqual(
                        len(
                            app.queries.item("item-a")["file_associations"][
                                "associations"
                            ]
                        ),
                        2,
                    )
                self.assertEqual(link.lstat().st_ino, inode)
                self.assertEqual(
                    inspect_database(Path(result["backup"]))["schema"], version
                )
                self.assertFalse(upgrade_database(path)["upgraded"])

    def test_schema_two_interruption_rolls_back_new_tables_and_backfill(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = create_legacy(Path(temporary).resolve())
            upgrade_database(path, migrations=load_migrations()[:2])
            db = sqlite3.connect(path)
            before = data_snapshot(db)
            db.close()
            worker = """
import os,sys
import catabolic.migration as m
def stop(stage,db,version):
    if stage=='upgrade:after_migration' and version==3: os._exit(86)
m._checkpoint=stop
m.upgrade_database(sys.argv[1])
"""
            child = subprocess.run(
                [sys.executable, "-c", worker, str(path)],
                capture_output=True,
                text=True,
                timeout=30,
            )
            self.assertEqual(child.returncode, 86, child.stderr)
            db = sqlite3.connect(path)
            self.assertEqual(db.execute("PRAGMA user_version").fetchone()[0], 2)
            self.assertEqual(data_snapshot(db), before)
            db.close()
            self.assertEqual(upgrade_database(path)["schema"], SCHEMA_VERSION)
