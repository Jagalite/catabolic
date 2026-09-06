import copy
import hashlib
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import catabolic.layouts as layout_module
from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.layouts import PRESETS, Layouts, render_path, validate_layout
from catabolic.reconcile import Reconciler
from catabolic.store import Store
from tests.test_media_model import populate_media


def layout(path, **rule):
    return {"version": 1, "rules": [{"name": "files", "path": path, **rule}]}


class LayoutTest(unittest.TestCase):
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
        (self.root / "custom").mkdir()
        self.app.bind("output", "custom", str(self.root / "custom"))
        self.layouts = Layouts(self.app)
        self.layouts.put("flat", copy.deepcopy(PRESETS["flat"]))

    def test_preview_readonly_apply_sync_repeated_and_two_outputs(self):
        before = self.path.read_bytes()
        sources = {
            p.name: (p.stat().st_ino, hashlib.sha256(p.read_bytes()).hexdigest())
            for p in (self.root / "media").iterdir()
        }
        with Store(self.path) as reader:
            plan = Layouts(Application(reader)).run("flat", "custom")
        self.assertTrue(plan["safe"])
        self.assertEqual(plan["desired_count"], 14)
        self.assertEqual(self.path.read_bytes(), before)
        self.assertEqual(list((self.root / "custom").iterdir()), [])
        result = self.layouts.run("flat", "custom", apply=True, limit=1)
        self.assertTrue(result["applied"])
        self.assertTrue(result["details_truncated"])
        self.assertEqual(result["change_count"], 14)
        self.assertEqual(list((self.root / "custom").iterdir()), [])
        reconcile = Reconciler(self.app)
        self.assertTrue(reconcile.apply(None)["healthy"])
        self.assertTrue(reconcile.verify(None)["healthy"])
        links = {
            str(p): (p.lstat().st_ino, os.readlink(p))
            for p in self.root.rglob("*")
            if p.is_symlink()
        }
        self.assertEqual(len(links), 28)
        self.assertEqual(self.layouts.run("flat", "custom", apply=True)["changes"], [])
        self.assertEqual(reconcile.apply(None)["applied"], [])
        self.assertEqual(
            links,
            {
                str(p): (p.lstat().st_ino, os.readlink(p))
                for p in self.root.rglob("*")
                if p.is_symlink()
            },
        )
        self.assertEqual(
            sources,
            {
                p.name: (p.stat().st_ino, hashlib.sha256(p.read_bytes()).hexdigest())
                for p in (self.root / "media").iterdir()
            },
        )

    def test_rename_disables_only_owned_and_preserves_history(self):
        manual = self.app.put_mapping(
            "custom", self.files["movie.mkv"], "movie", "Handpicked/Movie.mkv"
        )
        self.layouts.run("flat", "custom", apply=True)
        Reconciler(self.app).apply("custom")
        self.layouts.put("flat", layout("Changed/{item.id}/{file.name}"))
        result = self.layouts.run("flat", "custom", apply=True)
        self.assertEqual(result["change_count"], 28)
        self.assertEqual(
            self.store.rows("SELECT active FROM mappings WHERE id=?", (manual["id"],))[
                0
            ]["active"],
            1,
        )
        self.assertEqual(
            self.store.rows(
                "SELECT count(*) n FROM mappings WHERE catalog='custom' AND active=0"
            )[0]["n"],
            14,
        )
        self.assertTrue(Reconciler(self.app).apply("custom")["healthy"])
        self.assertTrue((self.root / "custom/Handpicked/Movie.mkv").is_symlink())
        self.assertFalse((self.root / "custom/movie/movie/movie.mkv").exists())
        self.layouts.put("flat", PRESETS["flat"])
        self.assertTrue(self.layouts.run("flat", "custom", apply=True)["applied"])
        self.assertEqual(
            self.store.rows("SELECT count(*) n FROM mappings WHERE catalog='custom'")[
                0
            ]["n"],
            29,
        )

    def test_collision_and_invalid_metadata_block_entire_apply(self):
        for template in ("Same/output", "{item.metadata.missing}/{file.name}"):
            with self.subTest(template=template):
                self.layouts.put("broken", layout(template))
                before = self.store.rows("SELECT * FROM mappings")
                result = self.layouts.run("broken", "custom", apply=True)
                self.assertFalse(result["safe"])
                self.assertFalse(result["applied"])
                self.assertGreater(result["blocker_count"], 0)
                self.assertEqual(before, self.store.rows("SELECT * FROM mappings"))
                self.assertIsNone(self.layouts._read("layout-catalog:custom"))

    def test_explicit_collision_including_exact_alias_and_parent(self):
        for destination in (
            "movie/movie/movie.mkv",
            "MOVIE/movie/movie.mkv",
            "movie/movie",
            "movie/movie/movie.mkv/child",
        ):
            with self.subTest(destination=destination):
                mapping = self.app.put_mapping(
                    "custom", self.files["movie.mkv"], "movie", destination
                )
                result = self.layouts.run("flat", "custom", apply=True)
                self.assertFalse(result["safe"])
                self.app.disable_mapping(mapping["id"])
        # Even a disabled exact manual mapping is not silently adopted.
        self.assertFalse(self.layouts.run("flat", "custom")["safe"])

    def test_replace_layout_is_explicit(self):
        self.layouts.run("flat", "custom", apply=True)
        self.layouts.put("alternate", layout("Other/{file.name}"))
        with self.assertRaisesRegex(CatabolicError, "replace-layout"):
            self.layouts.run("alternate", "custom", apply=True)
        self.assertTrue(
            self.layouts.run("alternate", "custom", apply=True, replace_layout=True)[
                "applied"
            ]
        )

    def test_preset_relationships_missing_fields_and_ambiguity(self):
        self.layouts.put("plex", PRESETS["plex"])
        self.assertFalse(self.layouts.run("plex", "custom")["safe"])
        self.app.put_item("movie", {}, {"title": "A: Movie", "year": 2024}, "movie")
        self.app.media.associate(
            self.files["movie.srt"],
            "movie",
            role="subtitle",
            metadata={"language": "en"},
        )
        plan = self.layouts.run("plex", "custom")
        self.assertTrue(plan["safe"], plan)
        paths = {change["path"] for change in plan["changes"]}
        self.assertIn("Movies/A_ Movie (2024)/A_ Movie (2024).en.srt", paths)
        self.assertIn("Music/artist/album/01 - track1.flac", paths)
        self.assertEqual(plan["desired_count"], 5)
        self.app.put_item("artist", {}, {"title": "Other"}, "artist2")
        self.app.media.relate("album", "artist2", "performed_by")
        self.assertFalse(self.layouts.run("plex", "custom", apply=True)["applied"])

    def test_episode_multihop_and_order(self):
        for identifier, kind in (
            ("show", "series"),
            ("season", "season"),
            ("episode", "episode"),
        ):
            self.app.put_item(kind, {}, {"title": identifier}, identifier)
        self.app.media.relate("season", "show", "part_of", position=2)
        self.app.media.relate("episode", "season", "part_of", position=3)
        self.app.media.associate(self.files["movie.mkv"], "episode")
        self.layouts.put("tv", {"version": 1, "rules": [PRESETS["plex"]["rules"][2]]})
        plan = self.layouts.run("tv", "custom")
        self.assertTrue(plan["safe"])
        self.assertEqual(
            plan["changes"][0]["path"], "TV/show/Season 02/show - S02E03.mkv"
        )

    def test_first_matching_rule_and_filters(self):
        self.layouts.put(
            "filtered",
            {
                "version": 1,
                "rules": [
                    {
                        "name": "book",
                        "when": {
                            "kinds": ["book_edition"],
                            "has": ["title"],
                            "metadata": {"title": "edition"},
                        },
                        "path": "Books/{file.name}",
                    },
                    {"name": "fallback", "path": "Other/{file.name}"},
                ],
            },
        )
        plan = self.layouts.run("filtered", "custom")
        self.assertTrue(plan["safe"])
        self.assertEqual(
            sum(c["path"].startswith("Books/") for c in plan["changes"]), 2
        )

    def test_filesystem_conflict_is_left_for_safe_sync(self):
        self.layouts.run("flat", "custom", apply=True)
        destination = self.root / "custom/movie/movie/movie.mkv"
        destination.parent.mkdir(parents=True)
        destination.write_text("external file")
        result = Reconciler(self.app).apply("custom")
        self.assertFalse(result["safe"])
        self.assertEqual(destination.read_text(), "external file")

    def test_failed_mapping_write_rolls_back_disable_and_ownership(self):
        self.layouts.run("flat", "custom", apply=True)
        before = self.store.rows("SELECT * FROM mappings ORDER BY id")
        owner = self.layouts._read("layout-catalog:custom")
        self.layouts.put("flat", layout("Changed/{file.name}"))
        with self.store.transaction() as db:
            db.execute(
                "CREATE TRIGGER fail_layout BEFORE INSERT ON mappings BEGIN SELECT RAISE(ABORT, 'injected write failure'); END"
            )
        with self.assertRaisesRegex(sqlite3.IntegrityError, "injected"):
            self.layouts.run("flat", "custom", apply=True)
        self.assertEqual(before, self.store.rows("SELECT * FROM mappings ORDER BY id"))
        self.assertEqual(owner, self.layouts._read("layout-catalog:custom"))

    def test_pending_journal_blocks_planning_and_application(self):
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO journal VALUES ('pending','default','global','pending','create','target',NULL)"
            )
        for apply in (False, True):
            with self.assertRaisesRegex(CatabolicError, "pending"):
                self.layouts.run("flat", "custom", apply=apply)

    def test_incoming_chains_kind_filter_inactive_edges_and_error_alias(self):
        self.app.media.associate(self.files["notes.txt"], "artist")
        self.app.media.relate("album", "artist", "custom:points")
        self.app.media.relate("photo", "artist", "custom:points")
        self.app.put_item("album", {}, {"title": "inactive"}, "inactive")
        self.app.media.relate("inactive", "artist", "custom:points")
        with self.store.transaction() as db:
            db.execute(
                "UPDATE item_relationships SET active=0 WHERE source_id IN ('inactive','track2')"
            )
        definition = layout(
            "{error.title}/{track.title}/{track.position:02d}/{back.title}/{file.name}",
            relations=[
                {
                    "alias": "error",
                    "kind": "custom:points",
                    "direction": "incoming",
                    "target_kind": "album",
                },
                {
                    "alias": "track",
                    "from": "error",
                    "kind": "part_of",
                    "direction": "incoming",
                    "target_kind": "track",
                },
                {"alias": "back", "from": "error", "kind": "performed_by"},
            ],
        )
        definition["selection"] = {
            "language": "sql",
            "query": "SELECT item_id FROM catalog_items WHERE item_id='artist'",
        }
        self.layouts.put("incoming", definition)
        with patch.object(layout_module, "_LAYOUT_BATCH_SIZE", 1):
            plan = self.layouts.run("incoming", "custom", apply=True)
        self.assertTrue(plan["applied"], plan)
        self.assertEqual(plan["changes"][0]["path"], "album/track1/01/artist/notes.txt")

    def test_batch_boundaries_preserve_order_and_global_conflict_checks(self):
        expected = self.layouts.run("flat", "custom")
        with patch.object(layout_module, "_LAYOUT_BATCH_SIZE", 1):
            self.assertEqual(self.layouts.run("flat", "custom"), expected)
            self.assertTrue(self.layouts.run("flat", "custom", apply=True)["applied"])
            self.layouts.put("flat", layout("Same/output"))
            before = self.store.rows("SELECT * FROM mappings ORDER BY id")
            owner = self.layouts._read("layout-catalog:custom")
            plan = self.layouts.run("flat", "custom", apply=True)
            self.assertFalse(plan["applied"])
            self.assertEqual(plan["blocker_count"], 13)
            self.assertEqual(
                before, self.store.rows("SELECT * FROM mappings ORDER BY id")
            )
            self.assertEqual(owner, self.layouts._read("layout-catalog:custom"))

    def test_selection_loads_only_required_items_and_skips_unmatched_relations(self):
        definition = {
            "version": 1,
            "rules": [
                {
                    "name": "ignored",
                    "when": {"kinds": ["movie"]},
                    "relations": [{"alias": "missing", "kind": "custom:absent"}],
                    "path": "{missing.title}",
                },
                PRESETS["plex"]["rules"][3],
            ],
            "selection": {
                "language": "sql",
                "query": "SELECT item_id FROM catalog_items WHERE item_id='track1'",
            },
        }
        self.layouts.put("scoped", definition)
        with patch.object(
            layout_module, "_load_items", wraps=layout_module._load_items
        ) as loads:
            plan = self.layouts.run("scoped", "custom")
        self.assertTrue(plan["safe"])
        self.assertEqual(plan["desired_count"], 1)
        requested = {
            identifier for call in loads.call_args_list for identifier in call.args[1]
        }
        self.assertEqual(requested, {"track1", "album", "artist"})

    def test_high_degree_ambiguity_counts_without_loading_targets(self):
        with self.store.transaction() as db:
            db.executemany(
                "INSERT INTO items VALUES (?, 'collection', '{}')",
                [(f"many-{i}",) for i in range(2000)],
            )
            db.executemany(
                "INSERT INTO item_relationships(id,source_id,target_id,kind,metadata,active) VALUES (?, 'track1', ?, 'custom:many', '{}', 1)",
                [(f"edge-{i}", f"many-{i}") for i in range(2000)],
            )
        definition = layout(
            "{group.title}/{file.name}",
            relations=[{"alias": "group", "kind": "custom:many"}],
        )
        definition["selection"] = {
            "language": "sql",
            "query": "SELECT item_id FROM catalog_items WHERE item_id='track1'",
        }
        self.layouts.put("many", definition)
        with patch.object(
            layout_module, "_load_items", wraps=layout_module._load_items
        ) as loads:
            plan = self.layouts.run("many", "custom", apply=True)
        self.assertFalse(plan["applied"])
        self.assertEqual(
            plan["blockers"][0]["reason"],
            "relation group must match exactly one item; found 2000",
        )
        requested = {
            identifier for call in loads.call_args_list for identifier in call.args[1]
        }
        self.assertEqual(requested, {"track1"})

    def test_scope_cap_checked_before_loading_graph(self):
        original = self.store.rows

        def oversized(sql, args=()):
            if "LIMIT 100001" in sql:
                return [None] * 100001
            return original(sql, args)

        with (
            patch.object(self.store, "rows", side_effect=oversized),
            patch.object(layout_module, "_load_items") as loads,
        ):
            with self.assertRaisesRegex(CatabolicError, "at most 100000"):
                self.layouts.run("flat", "custom")
            loads.assert_not_called()

    def test_cli_definition_preview_and_apply(self):
        definition = self.root / "layout.json"
        definition.write_text(json.dumps(layout("CLI/{file.name}")))
        prefix = [
            sys.executable,
            "-m",
            "catabolic",
            "--db",
            str(self.path),
            "--json",
            "layout",
        ]
        # Close the writer because child mutations intentionally acquire its lock.
        self.store.close()
        for args in (
            ("put", "cli", "--file", str(definition)),
            ("preview", "cli", "--catalog", "custom"),
            ("apply", "cli", "--catalog", "custom"),
        ):
            result = subprocess.run([*prefix, *args], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIsInstance(json.loads(result.stdout), dict)


class TemplateTest(unittest.TestCase):
    def test_no_expressions_traversal_or_reserved_paths(self):
        for template in (
            "../{item.id}",
            "/{item.id}",
            ".catabolic-owner/{item.id}",
            "{item[id]}",
            "{item.id!r}",
            "{item.id.__class__()}",
            "{item.id:100000d}",
            "{item.id:{item.kind}}",
            "bad\\name/{item.id}",
            "bad\nname/{item.id}",
            "{unknown.id}",
        ):
            with self.subTest(template=template), self.assertRaises(CatabolicError):
                validate_layout(layout(template))

    def test_dict_only_normalization_extensionless_and_subpaths(self):
        self.assertEqual(
            render_path(
                "{item.title}/{file.stem}{file.extension}",
                {
                    "item": {"title": "Cafe\u0301 / Test"},
                    "file": {"stem": "LICENSE", "extension": ""},
                },
            ),
            "Café _ Test/LICENSE",
        )
        self.assertEqual(
            render_path("Archive/{file.path}", {"file": {"path": "a/b.txt"}}),
            "Archive/a/b.txt",
        )
        self.assertEqual(
            render_path("{{literal}}/{item.id}", {"item": {"id": "ok"}}), "{literal}/ok"
        )
        for value in ("..", ".catabolic-owner", ["x"], None, "x" * 256):
            with self.subTest(value=value), self.assertRaises(CatabolicError):
                render_path("{item.title}", {"item": {"title": value}})
