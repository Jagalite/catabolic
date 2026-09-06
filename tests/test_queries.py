import base64
import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.store import Store


class QueryTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.path = self.root / "catalog.sqlite3"
        Store.initialize(self.path)
        self.store = Store(self.path, writable=True)
        self.addCleanup(self.store.close)
        self.app = Application(self.store)
        self.query = self.app.queries
        for location, filenames in (
            ("drive-a", ["Straße%_Café.mkv", "missing.mkv", "raw.mkv", "disabled.mkv"]),
            ("drive-b", ["copy.mkv", "episode.mkv"]),
        ):
            source = self.root / location
            source.mkdir()
            for filename in filenames:
                (source / filename).write_bytes(b"synthetic media")
            self.app.bind("source", location, str(source))
        for catalog in ("global", "favorites"):
            output = self.root / catalog
            output.mkdir()
            self.app.bind("output", catalog, str(output))
        self.app.scan()
        self.files = {row["path"]: row["id"] for row in self.app.files()["files"]}
        for item_id, kind, metadata, identities in (
            (
                "a",
                "movie",
                {
                    "title": "Straße%_Café",
                    "year": 2021,
                    "genre": "Sci-Fi",
                    "rating": None,
                    "reviewed": True,
                    "extra": {"x": [1, None]},
                },
                {"tmdb.movie": "1", "imdb": "tt1"},
            ),
            (
                "b",
                "movie",
                {"title": "STRASSE%_CAFÉ", "year": 2021},
                {"tmdb.movie": "2"},
            ),
            ("c", "episode", {"title": "Episode", "year": 2022}, {"tvdb.episode": "3"}),
            ("d", "movie", {"title": "Disabled", "year": "2021"}, {}),
            ("orphan", "other", {}, {}),
        ):
            self.app.put_item(kind, identities, metadata, item_id)
        for catalog, filename, item, destination in (
            ("global", "Straße%_Café.mkv", "a", "Movies/A.mkv"),
            ("favorites", "Straße%_Café.mkv", "a", "Movies/A.mkv"),
            ("favorites", "copy.mkv", "a", "Movies/Copy.mkv"),
            ("global", "missing.mkv", "b", "Movies/B.mkv"),
            ("global", "episode.mkv", "c", "TV/Episode.mkv"),
            ("favorites", "Straße%_Café.mkv", "c", "TV/Alternate.mkv"),
        ):
            self.app.put_mapping(catalog, self.files[filename], item, destination)
        disabled = self.app.put_mapping(
            "global", self.files["disabled.mkv"], "d", "Movies/Disabled.mkv"
        )
        self.app.disable_mapping(disabled["id"])
        (self.root / "drive-a/missing.mkv").unlink()
        self.app.scan("drive-a")
        self.app.add_profile("laptop")

    def ids(self, result, key="items"):
        return [row["id"] for row in result[key]]

    def test_literal_unicode_search_and_combined_item_filters(self):
        self.assertEqual(
            self.ids(self.query.items(search="strasse%_café", kind="movie", year=2021)),
            ["a", "b"],
        )
        self.assertEqual(self.ids(self.query.items(search="%_")), ["a", "b"])
        self.assertEqual(self.ids(self.query.items(search="' OR 1=1 --")), [])
        self.assertEqual(self.ids(self.query.items(kind="episode", year=2021)), [])
        self.assertEqual(self.ids(self.query.items(year=2021)), ["a", "b"])

    def test_metadata_exact_matches_preserve_types_null_and_objects(self):
        for filters in (
            ['genre="Sci-Fi"', "year=2021"],
            ["rating=null"],
            ["reviewed=true"],
            ['extra={"x":[1,null]}'],
        ):
            with self.subTest(filters=filters):
                self.assertEqual(self.ids(self.query.items(metadata=filters)), ["a"])
        self.assertEqual(self.ids(self.query.items(metadata=["absent=null"])), [])
        self.assertEqual(self.ids(self.query.items(metadata=["reviewed=1"])), [])
        self.assertEqual(self.ids(self.query.items(metadata=['year="2021"'])), ["d"])

    def test_identity_lookup_returns_all_provider_identifiers(self):
        result = self.query.items(identity="imdb=tt1")
        self.assertEqual(self.ids(result), ["a"])
        self.assertEqual(
            result["items"][0]["identities"],
            [
                {"namespace": "imdb", "value": "tt1"},
                {"namespace": "tmdb.movie", "value": "1"},
            ],
        )
        self.assertEqual(self.ids(self.query.items(identity="imdb=unknown")), [])
        self.assertEqual(self.query.item(identity="tmdb.movie=1")["item"]["id"], "a")

    def test_file_path_location_and_observed_status_filters(self):
        result = self.app.files(
            search="STRASSE%_CAFÉ", location="drive-a", status="present"
        )
        self.assertEqual(self.ids(result, "files"), [self.files["Straße%_Café.mkv"]])
        self.assertIsNotNone(result["files"][0]["observed_at"])
        self.assertEqual(
            self.ids(self.app.files(status="missing"), "files"),
            [self.files["missing.mkv"]],
        )
        self.assertEqual(self.ids(self.app.files(search="' OR 1=1 --"), "files"), [])
        laptop = Application(self.store, "laptop")
        self.assertEqual(laptop.files(status="present")["files"], [])
        unknown = laptop.files(status="unknown")["files"]
        self.assertEqual(len(unknown), 6)
        self.assertTrue(
            all(row["size"] is None and row["observed_at"] is None for row in unknown)
        )

    def test_unmapped_is_catalog_scoped_and_unidentified_is_global(self):
        def paths(result):
            return {row["path"] for row in result["files"]}

        self.assertEqual(paths(self.app.files(unidentified=True)), {"raw.mkv"})
        self.assertEqual(
            paths(self.app.files(unmapped=True)),
            {"raw.mkv", "disabled.mkv", "copy.mkv"},
        )
        self.assertEqual(
            paths(self.app.files(unmapped=True, catalog="favorites")),
            {"raw.mkv", "disabled.mkv", "missing.mkv", "episode.mkv"},
        )

    def test_file_item_filters_match_one_active_association_without_duplicates(self):
        result = self.app.files(
            catalog="favorites", kind="movie", identity="tmdb.movie=1"
        )
        self.assertEqual(
            set(self.ids(result, "files")),
            {self.files["Straße%_Café.mkv"], self.files["copy.mkv"]},
        )
        self.assertEqual(
            self.app.files(
                catalog="favorites", kind="movie", identity="tvdb.episode=3"
            )["files"],
            [],
        )
        self.assertEqual(
            self.ids(self.app.files(item="d"), "files"), [self.files["disabled.mkv"]]
        )
        self.assertEqual(
            self.ids(self.app.files(item="b", year=2021), "files"),
            [self.files["missing.mkv"]],
        )

    def test_items_can_filter_active_catalog_membership(self):
        self.assertEqual(self.ids(self.query.items(catalog="global")), ["a", "b", "c"])
        self.assertEqual(self.ids(self.query.items(catalog="favorites")), ["a", "c"])
        self.assertEqual(
            self.ids(self.query.items(catalog="global", identity="tmdb.movie=1")), ["a"]
        )

    def test_mapping_filters_and_all_catalogs(self):
        self.assertEqual(len(self.query.mappings()["mappings"]), 4)
        self.assertEqual(len(self.query.mappings(catalog=None)["mappings"]), 7)
        self.assertEqual(
            len(self.query.mappings(catalog=None, item="a")["mappings"]), 3
        )
        disabled = self.query.mappings(active="disabled")["mappings"]
        self.assertEqual([row["item_id"] for row in disabled], ["d"])
        self.assertEqual(len(self.query.mappings(active="active")["mappings"]), 3)
        result = self.query.mappings(
            catalog=None, file=self.files["Straße%_Café.mkv"], search="movies/a"
        )
        self.assertEqual(len(result["mappings"]), 2)

    def test_item_detail_combines_files_and_destinations_without_live_probes(self):
        with patch(
            "os.stat", side_effect=AssertionError("queries must not probe drives")
        ):
            result = self.query.item("a")
        self.assertEqual(len(result["occurrences"]), 3)
        self.assertEqual(
            {row["catalog"] for row in result["occurrences"]}, {"global", "favorites"}
        )
        self.assertEqual(
            {row["location"] for row in result["occurrences"]}, {"drive-a", "drive-b"}
        )
        for row in result["occurrences"]:
            self.assertEqual(
                row["source_path"],
                str(self.root / row["location"] / row["source_relative_path"]),
            )
            self.assertEqual(
                row["output_path"], str(self.root / row["catalog"] / row["path"])
            )
            self.assertEqual(row["status"], "present")
            self.assertIsNone(row["recorded_link_target"])
        self.assertEqual(len(self.query.item("a", catalog="global")["occurrences"]), 1)
        self.assertEqual(self.query.item("b")["occurrences"][0]["status"], "missing")

    def test_item_detail_unknown_profile_bindings_and_disabled_history(self):
        result = Application(self.store, "laptop").queries.item("a")
        for row in result["occurrences"]:
            self.assertEqual(row["status"], "unknown")
            self.assertIsNone(row["source_path"])
            self.assertIsNone(row["output_path"])
        self.assertEqual(self.query.item("d")["occurrences"], [])
        self.assertEqual(
            len(self.query.item("d", include_disabled=True)["occurrences"]), 1
        )
        self.assertEqual(self.query.item("orphan")["occurrences"], [])

    def test_pagination_every_sort_both_directions_with_ties_and_nulls(self):
        for query, key, sorts in (
            (self.query.items, "items", ("id", "title", "year")),
            (self.query.files, "files", ("id", "path", "size", "mtime")),
            (self.query.mappings, "mappings", ("id", "path", "catalog")),
        ):
            for sort in sorts:
                for descending in (False, True):
                    with self.subTest(key=key, sort=sort, descending=descending):
                        expected = self.ids(
                            query(sort=sort, descending=descending), key
                        )
                        actual, cursor = [], None
                        for _ in range(10):
                            result = query(
                                sort=sort, descending=descending, limit=2, cursor=cursor
                            )
                            actual.extend(self.ids(result, key))
                            cursor = result["next_cursor"]
                            if cursor is None:
                                break
                        self.assertIsNone(cursor)
                        self.assertEqual(actual, expected)
                        self.assertEqual(len(actual), len(set(actual)))
                        if descending:
                            self.assertEqual(
                                actual, list(reversed(self.ids(query(sort=sort), key)))
                            )
        self.assertEqual(
            self.ids(self.query.items(sort="title")), ["orphan", "d", "c", "a", "b"]
        )
        laptop = Application(self.store, "laptop").queries
        first = laptop.files(sort="size", limit=2)
        self.assertEqual(
            len(
                laptop.files(sort="size", limit=100, cursor=first["next_cursor"])[
                    "files"
                ]
            ),
            4,
        )

    def test_detail_occurrences_are_paginated(self):
        first = self.query.item("a", limit=1)
        second = self.query.item("a", limit=100, cursor=first["next_cursor"])
        self.assertEqual(len(second["occurrences"]), 2)
        self.assertNotIn(first["occurrences"][0]["id"], self.ids(second, "occurrences"))
        self.assertIsNone(second["next_cursor"])
        with self.assertRaisesRegex(CatabolicError, "cursor"):
            self.query.item("b", cursor=first["next_cursor"])

    def test_cursor_is_bound_to_filters_sort_profile_and_entity(self):
        cursor = self.query.items(limit=1)["next_cursor"]
        for args in (
            {"kind": "movie"},
            {"sort": "title"},
            {"descending": True},
            {"metadata": ["year=2021"]},
            {"catalog": "global"},
        ):
            with (
                self.subTest(args=args),
                self.assertRaisesRegex(CatabolicError, "cursor"),
            ):
                self.query.items(cursor=cursor, **args)
        for query in (
            self.query.files,
            self.query.mappings,
            Application(self.store, "laptop").queries.items,
        ):
            with self.assertRaisesRegex(CatabolicError, "cursor"):
                query(cursor=cursor)
        fresh = self.root / "other.sqlite3"
        Store.initialize(fresh)
        with Store(fresh) as other:
            with self.assertRaisesRegex(CatabolicError, "cursor"):
                Application(other).queries.items(cursor=cursor)

    def test_malformed_cursors_and_invalid_filters_fail_cleanly(self):
        for cursor in (
            "!",
            "a",
            "x" * 20000,
            base64.b64encode(b"{}").decode(),
            base64.b64encode(b"null").decode(),
        ):
            with self.subTest(cursor=cursor[:20]), self.assertRaises(CatabolicError):
                self.query.items(cursor=cursor)
        for args in (
            {"limit": 0},
            {"limit": 1001},
            {"sort": "id; DROP TABLE items"},
            {"year": 0},
            {"year": 10000},
            {"kind": "invalid"},
            {"identity": "bad"},
            {"metadata": ["title=plain"]},
            {"metadata": ["=1"]},
            {"metadata": ["rating=NaN"]},
        ):
            with self.subTest(args=args), self.assertRaises(CatabolicError):
                self.query.items(**args)
        for args in (
            {"catalog": "missing"},
            {"location": "missing"},
            {"item": "missing"},
            {"status": "bad"},
        ):
            with self.subTest(args=args), self.assertRaises(CatabolicError):
                self.query.files(**args)
        for args in (
            {},
            {"item_id": "absent"},
            {"identity": "imdb=absent"},
            {"item_id": "a", "identity": "imdb=tt1"},
        ):
            with self.subTest(args=args), self.assertRaises(CatabolicError):
                self.query.item(**args)

    def test_queries_are_database_only_and_create_no_lock_or_backup(self):
        self.store.close()
        Path(str(self.path) + ".lock").unlink()
        # Disconnected sources do not change previously recorded availability.
        (self.root / "drive-a").rename(self.root / "offline")
        before = hashlib.sha256(self.path.read_bytes()).hexdigest()
        entries = set(self.root.rglob("*"))
        with Store(self.path) as store:
            query = Application(store).queries
            query.files(status="present")
            query.items(search="café")
            query.mappings(catalog=None)
            self.assertEqual(query.item("a")["occurrences"][0]["status"], "present")
        self.assertEqual(hashlib.sha256(self.path.read_bytes()).hexdigest(), before)
        self.assertEqual(set(self.root.rglob("*")), entries)

    def test_public_cli_search_show_filters_and_errors(self):
        def run(*arguments, expected=0):
            result = subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "catabolic",
                    "--db",
                    str(self.path),
                    "--json",
                    *arguments,
                ],
                text=True,
                capture_output=True,
                timeout=20,
            )
            self.assertEqual(
                result.returncode, expected, result.stderr or result.stdout
            )
            return json.loads(result.stdout if expected == 0 else result.stderr)

        self.assertEqual(
            self.ids(
                run(
                    "item",
                    "list",
                    "--search",
                    "strasse",
                    "--year",
                    "2021",
                    "--metadata",
                    'genre="Sci-Fi"',
                )
            ),
            ["a"],
        )
        self.assertEqual(
            len(run("item", "show", "--identity", "imdb=tt1")["occurrences"]), 3
        )
        self.assertEqual(len(run("files", "--status", "missing")["files"]), 1)
        self.assertEqual(len(run("files", "--unidentified")["files"]), 1)
        self.assertEqual(
            len(run("mapping", "list", "--all-catalogs", "--item", "a")["mappings"]), 3
        )
        page = run("item", "list", "--sort", "title", "--descending", "--limit", "1")
        self.assertEqual(
            len(
                run(
                    "item",
                    "list",
                    "--sort",
                    "title",
                    "--descending",
                    "--cursor",
                    page["next_cursor"],
                )["items"]
            ),
            4,
        )
        self.assertIn("error", run("item", "show", expected=2))
        self.assertIn("error", run("item", "list", "--metadata", "invalid", expected=2))
