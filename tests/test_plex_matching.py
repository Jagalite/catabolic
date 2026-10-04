# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import copy
import json
import os
import unittest
from pathlib import Path

from catabolic.app import Application
from catabolic.consumer_adapters import ConsumerError
from catabolic.consumer_setup import bind
from catabolic.database_io import acquire_writer_lock
from catabolic.plex_matching import match
from catabolic.plex_metadata import run
from catabolic.reconcile import Reconciler
from catabolic.store import Store
from tests import test_plex_metadata as fixtures


class PlexMatchingTest(unittest.TestCase):
    protocol = fixtures.PlexMetadataTest.protocol
    curate = fixtures.PlexMetadataTest.curate
    writes = fixtures.PlexMetadataTest.writes

    def setUp(self):
        fixtures.PlexMetadataTest.setUp(self)
        self.output = self.root / "output"
        self.output.mkdir()
        with Store(self.database, writable=True) as s:
            app = Application(s)
            app.media.disable_association(s.rows("SELECT id FROM item_files")[0]["id"])
            self.item_id = app.put_item(
                "series",
                {"local.series": "show"},
                {"title": "Series title must not overwrite episode"},
            )["id"]
            self.file_id = s.rows("SELECT id FROM files")[0]["id"]
            self.association = app.media.associate(
                self.file_id,
                self.item_id,
                metadata={
                    "title": "Individual episode title",
                    "summary": "Episode summary",
                    "episode": 1,
                },
            )
            app.bind("output", "plex", str(self.output))
            self.mapping = app.put_mapping(
                "plex", self.file_id, self.item_id, "movie.mkv"
            )
            Reconciler(app).apply("plex")
        bind(
            self.database,
            "default",
            "fixture",
            "home",
            "plex",
            "",
            "/media",
            "7",
            "movie",
            apply=True,
        )
        self.calls.clear()

    def preview_match(self, **kw):
        return match(self.database, "default", "fixture", self.mapping["id"], **kw)

    def save_match(self, **kw):
        preview = self.preview_match(**kw)
        return self.preview_match(**kw, apply=True, expected_plan=preview["plan_id"])[
            "match_id"
        ]

    def preview(self, match_id, **kw):
        kw.setdefault("fields", ["title", "summary"])
        return run(
            self.database, "default", "home", "7", None, match_ids=[match_id], **kw
        )

    def test_match_existing_preserves_catalog_and_publishes_file_metadata(self):
        with Store(self.database) as s:
            before = {
                t: s.rows("SELECT * FROM " + t)
                for t in (
                    "items",
                    "identities",
                    "item_files",
                    "mappings",
                    "owned_links",
                )
            }
        raw = self.database.read_bytes()
        preview = self.preview_match()
        self.assertEqual(raw, self.database.read_bytes())
        self.assertEqual(preview["item_kind"], "series")
        mid = self.save_match()
        self.assertEqual(mid, self.save_match())
        with Store(self.database) as s:
            self.assertEqual(before, {t: s.rows("SELECT * FROM " + t) for t in before})
        preview = self.preview(mid)
        self.assertEqual(
            preview["items"][0]["changes"]["title"]["after"]["value"],
            "Individual episode title",
        )
        result = self.preview(mid, apply=True, expected_plan=preview["plan_id"])
        self.assertTrue(result["complete"])
        self.assertEqual(self.node.get("title"), "Individual episode title")
        self.assertEqual(self.node.get("summary"), "Episode summary")
        with Store(self.database) as s:
            self.assertEqual(before, {t: s.rows("SELECT * FROM " + t) for t in before})

    def test_mismatched_item_source_rejected(self):
        with self.assertRaisesRegex(ConsumerError, "source_kind_mismatch"):
            self.preview_match(metadata_source="item")
        with Store(self.database, writable=True) as s:
            Application(s).media.associate(
                self.file_id, self.item_id, metadata={"episode": 1}
            )
        mid = self.save_match()
        with self.assertRaisesRegex(ConsumerError, "selected_field_missing"):
            self.preview(mid)
        self.assertEqual(self.writes(), [])

    def test_matching_kind_can_explicitly_use_item_metadata(self):
        with Store(self.database, writable=True) as s, s.transaction() as db:
            db.execute("UPDATE items SET kind='movie' WHERE id=?", (self.item_id,))
        mid = self.save_match(metadata_source="item")
        preview = self.preview(mid, fields=["title"])
        self.assertEqual(
            preview["items"][0]["changes"]["title"]["after"]["value"],
            "Series title must not overwrite episode",
        )

    def test_missing_ambiguous_and_over_budget_remote_paths(self):
        with self.assertRaisesRegex(ConsumerError, "exceeds_limit"):
            extra = copy.deepcopy(self.node)
            extra.set("ratingKey", "2")
            self.nodes.append(extra)
            self.preview_match(max_items=1)
        with self.assertRaisesRegex(ConsumerError, "ambiguous"):
            self.preview_match()
        self.nodes.pop()
        self.node.find("Media/Part").set("file", "/media/elsewhere.mkv")
        with self.assertRaisesRegex(ConsumerError, "missing_or_ambiguous"):
            self.preview_match()
        self.assertEqual(self.writes(), [])

    def test_changed_plan_and_published_link_rejected(self):
        with self.assertRaisesRegex(ConsumerError, "requires_expected_plan"):
            self.preview_match(apply=True)
        preview = self.preview_match()
        self.node.set("guid", "plex://movie/changed")
        with self.assertRaisesRegex(ConsumerError, "plan_changed"):
            self.preview_match(apply=True, expected_plan=preview["plan_id"])
        mid = self.save_match()
        (self.output / "movie.mkv").unlink()
        (self.output / "movie.mkv").symlink_to("/not/the/source")
        with self.assertRaisesRegex(ConsumerError, "published_link_changed"):
            self.preview(mid)

    def test_mapping_and_binding_changes_invalidate_saved_match(self):
        mid = self.save_match()
        with Store(self.database, writable=True) as s, s.transaction() as db:
            db.execute(
                "UPDATE consumer_bindings SET revision=revision+1 WHERE id='fixture'"
            )
        with self.assertRaisesRegex(ConsumerError, "publication_changed"):
            self.preview(mid)
        self.assertEqual(self.writes(), [])

    def test_remote_rematch_and_changed_local_values_invalidate_preview(self):
        mid = self.save_match()
        preview = self.preview(mid)
        with Store(self.database, writable=True) as s:
            Application(s).media.associate(
                self.file_id,
                self.item_id,
                metadata={"title": "New edit", "summary": "New summary"},
            )
        with self.assertRaisesRegex(ConsumerError, "plan_changed"):
            self.preview(mid, apply=True, expected_plan=preview["plan_id"])
        self.node.set("guid", "reassigned")
        with self.assertRaisesRegex(ConsumerError, "identity_changed"):
            self.preview(mid)
        self.assertEqual(self.writes(), [])

    def test_locked_fields_can_be_restored_and_dates_explicitly_cleared(self):
        mid = self.save_match()
        preview = self.preview(mid, fields=["title"], lock_fields=True)
        self.assertTrue(
            self.preview(
                mid,
                fields=["title"],
                lock_fields=True,
                apply=True,
                expected_plan=preview["plan_id"],
            )["complete"]
        )
        preview = self.preview(mid, fields=["title"], unlock_fields=True)
        self.assertFalse(preview["items"][0]["changes"]["title"]["after"]["locked"])
        self.assertTrue(
            self.preview(
                mid,
                fields=["title"],
                unlock_fields=True,
                apply=True,
                expected_plan=preview["plan_id"],
            )["complete"]
        )
        self.node.set("originallyAvailableAt", "2024-01-01")
        preview = self.preview(
            mid, fields=["year", "release_date"], clear_fields=["year", "release_date"]
        )
        self.assertTrue(
            self.preview(
                mid,
                fields=["year", "release_date"],
                clear_fields=["year", "release_date"],
                apply=True,
                expected_plan=preview["plan_id"],
            )["complete"]
        )
        self.assertEqual(self.node.get("originallyAvailableAt"), "")
        self.assertEqual(self.node.get("year"), "")
        with self.assertRaisesRegex(ConsumerError, "invalid_clear"):
            self.preview(mid, fields=["title"], clear_fields=["title"])

    def test_concurrent_catabolic_writer_blocked(self):
        mid = self.save_match()
        preview = self.preview(mid)
        lock = acquire_writer_lock(Path(str(self.database) + ".plex-metadata"))
        try:
            with self.assertRaisesRegex(ConsumerError, "writer_busy"):
                self.preview(mid, apply=True, expected_plan=preview["plan_id"])
            self.assertEqual(self.writes(), [])
        finally:
            os.close(lock)

    def test_rescanned_replacement_invalidates_saved_match(self):
        mid = self.save_match()
        (self.source / "movie.mkv").write_bytes(b"different media bytes")
        with Store(self.database, writable=True) as store:
            Application(store).scan()
        with self.assertRaisesRegex(ConsumerError, "publication_changed"):
            self.preview(mid)
        self.assertEqual(self.writes(), [])

    def test_structural_association_change_invalidates_saved_match(self):
        mid = self.save_match()
        with Store(self.database, writable=True) as store:
            Application(store).media.associate(self.file_id, self.item_id, part=2)
        with self.assertRaisesRegex(ConsumerError, "publication_changed"):
            self.preview(mid)
        self.assertEqual(self.writes(), [])

    def test_metadata_match_cli_and_listing(self):
        import contextlib
        import io

        from catabolic.cli import main

        def call(args):
            out, err = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                code = main(["--db", str(self.database), "--json", "consumer"] + args)
            self.assertEqual(code, 0, err.getvalue())
            return json.loads(out.getvalue())

        args = ["metadata-match", "fixture", "--mapping", self.mapping["id"]]
        preview = call(args)
        applied = call(args + ["--apply", "--expected-plan", preview["plan_id"]])
        listing = call(["metadata-matches", "--binding", "fixture"])
        self.assertEqual(listing["matches"][0]["id"], applied["match_id"])
        args = [
            "metadata",
            "home",
            "--library-id",
            "7",
            "--match",
            applied["match_id"],
            "--field",
            "title",
        ]
        preview = call(args)
        applied = call(args + ["--apply", "--expected-plan", preview["plan_id"]])
        self.assertTrue(applied["complete"])
        self.assertEqual(
            applied["results"][0]["association_id"], self.association["id"]
        )

    def test_remote_rating_key_cannot_select_multiple_items(self):
        from catabolic.plex_metadata import remote_snapshot

        with self.assertRaisesRegex(ConsumerError, "invalid_rating_key"):
            remote_snapshot(None, {}, {"rating_key": "1,2"}, [])

    def test_duplicate_matches_cannot_update_one_remote_item_twice(self):
        mid = self.save_match()
        # The same target deduplicates by ID, and conflicting match revisions reject.
        with Store(self.database, writable=True) as s:
            Application(s).media.associate(
                self.file_id,
                self.item_id,
                metadata={"title": "Edited", "summary": "Summary"},
            )
        other = self.save_match()
        self.assertNotEqual(mid, other)
        with self.assertRaisesRegex(ConsumerError, "duplicate_remote_item"):
            run(
                self.database,
                "default",
                "home",
                "7",
                None,
                ["title"],
                match_ids=[mid, other],
            )
