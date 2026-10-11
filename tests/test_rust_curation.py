# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native identification, relationship and explicit completeness mutation parity."""

import json
import select
import shutil
import subprocess
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path
from unittest.mock import patch

from catabolic.app import Application
from catabolic.component_sql import OCCURRENCES_SQL
from catabolic.components import revision
from catabolic.curation import Curation
from catabolic.domain import CatabolicError
from catabolic.store import Store
from tests.test_media_model import populate_media
from tests.test_rust_migration import BINARY


@unittest.skipUnless(BINARY.is_file(), "build catabolic-cli before qualification")
class NativeCurationTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.path = self.root / "native.sqlite3"
        self.oracle = self.root / "oracle.sqlite3"
        Store.initialize(self.path)
        with Store(self.path, writable=True) as store:
            self.files, _ = populate_media(Application(store), self.root)
        self.worker = subprocess.Popen(
            [str(BINARY), "--stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(self.close_worker)

    def close_worker(self):
        self.worker.stdin.close()
        try:
            self.worker.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.worker.kill()
            self.worker.wait()
        self.worker.stdout.close()
        self.worker.stderr.close()

    def native(self, action, args, *, ok=True):
        self.worker.stdin.write(
            json.dumps(["--db", str(self.path), "curation", action, json.dumps(args)])
            + "\n"
        )
        self.worker.stdin.flush()
        self.assertTrue(
            select.select([self.worker.stdout], [], [], 120)[0], "native timeout"
        )
        result = json.loads(self.worker.stdout.readline())
        self.assertEqual(result["status"], 0 if ok else 2, result)
        return result["result"] if ok else result["error"].removeprefix("catabolic: ")

    def normalized(self, value):
        if isinstance(value, list):
            return [self.normalized(v) for v in value]
        if not isinstance(value, dict):
            return value
        value = value.copy()
        if "created_at" in value:
            stamp = datetime.strptime(value.pop("created_at"), "%Y-%m-%d %H:%M:%S")
            self.assertLess(
                abs(
                    (
                        datetime.now(timezone.utc).replace(tzinfo=None) - stamp
                    ).total_seconds()
                ),
                120,
            )
        return {k: self.normalized(v) for k, v in value.items()}

    def differential(self, action, args, call, *, random_id=False):
        shutil.copyfile(self.path, self.oracle)
        actual = self.native(action, args)
        with Store(self.oracle, writable=True) as store:
            app = Application(store)
            if random_id:
                with patch("catabolic.app.uuid4", return_value=actual["id"]):
                    expected = call(app)
            else:
                expected = call(app)
        self.assertEqual(self.normalized(actual), self.normalized(expected))
        with Store(self.path) as native, Store(self.oracle) as oracle:
            for table in (
                "items",
                "identities",
                "item_files",
                "item_relationships",
                "expected_sets",
                "media_components",
                "component_occurrences",
                "component_assertions",
                "proposals",
                "decision_events",
            ):
                self.assertEqual(
                    self.normalized(native.rows(f"SELECT * FROM {table} ORDER BY 1")),
                    self.normalized(oracle.rows(f"SELECT * FROM {table} ORDER BY 1")),
                    table,
                )
            self.assertEqual(native.rows("PRAGMA foreign_key_check"), [])
        return actual

    def error(self, action, args, call):
        before = self.path.read_bytes()
        with Store(self.path, writable=True) as store:
            with self.assertRaises(CatabolicError) as expected:
                call(Application(store))
        self.assertEqual(self.native(action, args, ok=False), str(expected.exception))
        self.assertEqual(before, self.path.read_bytes())

    def test_item_identity_merge_conflicts_and_metadata_replacement(self):
        args = {
            "kind": "movie",
            "identities": {"imdb": "tt-new", "tmdb.movie": "900"},
            "metadata": {
                "title": "Café",
                "vendor": {"$serde_json::private::Number": "123", "n": 2**80},
            },
        }
        first = self.differential(
            "item-put",
            args,
            lambda app: app.put_item(
                args["kind"], args["identities"], args["metadata"]
            ),
            random_id=True,
        )
        update = {**args, "metadata": {"title": "New"}}
        self.differential(
            "item-put",
            update,
            lambda app: app.put_item(
                update["kind"], update["identities"], update["metadata"]
            ),
        )
        self.error(
            "item-put",
            {**update, "kind": "book"},
            lambda app: app.put_item("book", update["identities"], update["metadata"]),
        )
        self.error(
            "item-put",
            {**update, "id": "different"},
            lambda app: app.put_item(
                update["kind"], update["identities"], update["metadata"], "different"
            ),
        )
        self.assertTrue(first["id"])

    def test_association_deterministic_revision_metadata_parts_and_reindex(self):
        args = {
            "file_id": self.files["movie.srt"],
            "item_id": "book",
            "role": "subtitle",
            "part": 1,
            "metadata": {"language": "de", "forced": True},
        }
        self.differential("associate", args, lambda app: app.media.associate(**args))
        preserve = {"file_id": args["file_id"], "item_id": "book", "role": "subtitle"}
        kept = self.differential(
            "associate", preserve, lambda app: app.media.associate(**preserve)
        )
        self.assertEqual(kept["part"], 1)
        cleared = {**preserve, "clear_part": True}
        value = self.differential(
            "associate", cleared, lambda app: app.media.associate(**cleared)
        )
        self.assertIsNone(value["part"])
        self.differential(
            "association-disable",
            {"id": value["id"]},
            lambda app: app.media.disable_association(value["id"]),
        )
        self.differential(
            "associate", preserve, lambda app: app.media.associate(**preserve)
        )
        for changes in (
            {"role": "wrong"},
            {"part": True},
            {"part": 0},
            {"metadata": []},
            {"part": 1, "clear_part": True},
            {"file_id": "absent"},
        ):
            invalid = {**preserve, **changes}
            self.error(
                "associate",
                invalid,
                lambda app, invalid=invalid: app.media.associate(**invalid),
            )

    def test_active_mapping_keeps_last_identification(self):
        with Store(self.path) as store:
            row = store.rows(
                "SELECT id FROM item_files WHERE item_id='movie' AND role='primary'"
            )[0]
        self.error(
            "association-disable",
            row,
            lambda app: app.media.disable_association(row["id"]),
        )

    def test_relationship_ordering_cycle_and_metadata_idempotence(self):
        args = {
            "source": "movie",
            "target": "photos",
            "kind": "derived_from",
            "metadata": {"proof": "fixture"},
        }
        first = self.differential("relate", args, lambda app: app.media.relate(**args))
        preserved = {k: v for k, v in args.items() if k != "metadata"}
        self.differential(
            "relate", preserved, lambda app: app.media.relate(**preserved)
        )
        cycle = {"source": "photos", "target": "movie", "kind": "derived_from"}
        self.error("relate", cycle, lambda app: app.media.relate(**cycle))
        self.differential(
            "relationship-disable",
            {"id": first["id"]},
            lambda app: app.media.disable_relationship(first["id"]),
        )
        self.differential("relate", cycle, lambda app: app.media.relate(**cycle))
        for invalid in (
            {"source": "movie", "target": "movie", "kind": "derived_from"},
            {"source": "movie", "target": "artist", "kind": "part_of"},
            {"source": "track1", "target": "album", "kind": "part_of", "position": 2},
            {
                "source": "movie",
                "target": "artist",
                "kind": "created_by",
                "position": 1,
            },
        ):
            self.error(
                "relate",
                invalid,
                lambda app, invalid=invalid: app.media.relate(**invalid),
            )

    def test_expected_set_identity_and_completeness_states_pagination(self):
        with Store(self.path, writable=True) as store:
            app = Application(store)
            app.put_item("movie", {"imdb": "missing-file"}, {}, "unidentified")
            app.put_item("movie", {}, {}, "unavailable")
            app.media.associate(self.files["notes.txt"], "unavailable")
            with store.transaction() as db:
                db.execute(
                    "UPDATE observations SET status='missing' WHERE file_id=?",
                    (self.files["notes.txt"],),
                )
        value = {
            "source": "fixture",
            "edition": "first",
            "ordering": "publisher",
            "complete": True,
            "members": [
                {"key": "1", "item_id": "movie", "title": "Present"},
                {"key": "2", "identity": {"imdb": "missing-file"}},
                {"key": "3", "item_id": "unavailable"},
                {"key": "4", "item_id": "absent"},
                {"key": "5", "identity": {"imdb": "absent"}},
            ],
        }
        args = {"collection": "album", "value": value}
        actual = self.differential(
            "expected-put", args, lambda app: Curation(app).expected_put("album", value)
        )
        self.assertEqual(actual, self.native("expected-put", args))
        with Store(self.path) as store:
            for after in (0, 2, 4, 100):
                self.assertEqual(
                    self.native(
                        "completeness", {"id": actual["id"], "limit": 2, "after": after}
                    ),
                    Curation(Application(store)).completeness(
                        actual["id"], limit=2, after=after
                    ),
                )
        for change in (
            {"complete": 1},
            {"extra": True},
            {"members": [{"key": "x"}]},
            {"members": [{"key": "x", "item_id": "a"}, {"key": "x", "item_id": "b"}]},
            {"members": [{"key": "x", "identity": {"a": "b", "c": "d"}}]},
        ):
            invalid = {**value, **change}
            self.error(
                "expected-put",
                {"collection": "album", "value": invalid},
                lambda app, invalid=invalid: Curation(app).expected_put(
                    "album", invalid
                ),
            )

    def test_reviewed_proposals_preserve_metadata_and_record_decisions(self):
        payload = {
            "item": {
                "id": "movie",
                "kind": "movie",
                "identities": {},
                "metadata": {"title": "Proposed title", "new": "added"},
            }
        }
        args = {
            "file_id": self.files["movie.mkv"],
            "payload": payload,
            "source": "manual",
            "evidence": {"provider": "test"},
        }
        proposal = self.differential(
            "proposal-put", args, lambda app: Curation(app).put(**args)
        )
        self.assertEqual(proposal, self.native("proposal-put", args))
        decision = {"id": proposal["id"], "accept": True, "actor": "reviewer"}
        accepted = self.differential(
            "proposal-decide",
            decision,
            lambda app: Curation(app).decide(
                proposal["id"], accept=True, actor="reviewer"
            ),
        )
        self.assertIn("title", accepted["result"]["preserved_fields"])
        self.assertEqual(accepted, self.native("proposal-decide", decision))
        with Store(self.path) as store:
            curation = Curation(Application(store))
            self.assertEqual(
                self.native("proposal-history", {"limit": 1}), curation.history(limit=1)
            )
            self.assertEqual(
                self.native("proposal-list", {"state": "accepted", "limit": 1}),
                curation.list(state="accepted", limit=1),
            )
        self.error(
            "proposal-decide",
            {**decision, "accept": False},
            lambda app: Curation(app).decide(
                proposal["id"], accept=False, actor="reviewer"
            ),
        )

    def test_stale_proposal_refusal_rejection_and_live_source_admission(self):
        args = {"file_id": self.files["movie.mkv"], "payload": {"item_id": "movie"}}
        proposal = self.differential(
            "proposal-put", args, lambda app: Curation(app).put(**args)
        )
        with Store(self.path, writable=True) as store:
            Application(store).media.associate(
                self.files["movie.mkv"], "movie", metadata={"changed": True}
            )
        decision = {"id": proposal["id"], "accept": True, "actor": "reviewer"}
        self.error(
            "proposal-decide",
            decision,
            lambda app: Curation(app).decide(
                proposal["id"], accept=True, actor="reviewer"
            ),
        )
        self.differential(
            "proposal-decide",
            {**decision, "accept": False},
            lambda app: Curation(app).decide(
                proposal["id"], accept=False, actor="reviewer"
            ),
        )
        current = self.differential(
            "proposal-put", args, lambda app: Curation(app).put(**args)
        )
        with Store(self.path) as store:
            binding = Application(store).binding("source", "media")
        (Path(binding["root"]) / "movie.mkv").write_bytes(b"replacement revision")
        self.error(
            "proposal-decide",
            {**decision, "id": current["id"]},
            lambda app: Curation(app).decide(
                current["id"], accept=True, actor="reviewer"
            ),
        )

    def test_proposal_validation_has_no_mutation_on_invalid_payload(self):
        for payload in (
            None,
            [],
            {},
            {"item": {}, "item_id": "movie"},
            {"item_id": "absent"},
            {"item_id": "movie", "unknown": True},
            {"item_id": "movie", "role": "bad"},
            {"item_id": "movie", "metadata": []},
            {"item_id": "movie", "overwrite_fields": [1]},
            {"item": {"id": "movie", "kind": "wrong"}},
            {"item": {"kind": "movie"}},
        ):
            args = {"file_id": self.files["movie.mkv"], "payload": payload}
            self.error(
                "proposal-put", args, lambda app, args=args: Curation(app).put(**args)
            )

    def test_existing_association_evidence_preserves_association_and_records_event(
        self,
    ):
        args = {
            "file_id": self.files["movie.mkv"],
            "payload": {"item_id": "movie", "role": "primary", "part": None},
        }
        proposal = self.differential(
            "proposal-put", args, lambda app: Curation(app).put(**args)
        )
        decision = {"id": proposal["id"], "actor": "reviewer"}
        accepted = self.differential(
            "proposal-accept-existing",
            decision,
            lambda app: Curation(app).accept_existing_association_evidence(
                proposal["id"], actor="reviewer"
            ),
        )
        self.assertEqual(accepted, self.native("proposal-accept-existing", decision))
        with Store(self.path) as store:
            event = Curation(Application(store)).history()["events"][-1]
        self.assertEqual(event["before_value"], event["after_value"])
        invalid = {**args, "payload": {**args["payload"], "metadata": {}}}
        proposal = self.native("proposal-put", invalid)
        self.error(
            "proposal-accept-existing",
            {**decision, "id": proposal["id"]},
            lambda app: Curation(app).accept_existing_association_evidence(
                proposal["id"], actor="reviewer"
            ),
        )

    def test_sidecar_discovery_dry_run_apply_and_pagination(self):
        for args in (
            {},
            {"limit": 2},
            {"location": "media"},
            {"location": "absent"},
            {"apply": True},
        ):
            with self.subTest(args=args):
                self.differential(
                    "sidecars",
                    args,
                    lambda app, args=args: Curation(app).sidecars(**args),
                )
        with Store(self.path) as store:
            expected = Curation(Application(store)).sidecars()
        self.assertTrue(expected["candidates"])

    def test_component_claim_snapshots_assertion_supersession_and_stale_guards(self):
        with Store(self.path) as store:
            row = store.rows(
                "SELECT * FROM (" + OCCURRENCES_SQL + ") WHERE file_id=?",
                (self.files["movie.srt"],),
            )[0]
            from catabolic.curation import occurrence

            video_revision = revision(
                occurrence(store, "default", self.files["movie.mkv"])
            )
        claim = {
            "occurrence_id": row["occurrence_id"],
            "attributes": {"language": "en", "forced": True},
            "compatibility": {
                "edition_id": "movie",
                "video_file_id": self.files["movie.mkv"],
                "video_revision": video_revision,
                "part": None,
                "timeline_id": "theatrical",
                "coverage": "full",
                "offset_seconds": 0,
                "synchronization": "declared",
            },
            "dependencies_complete": True,
        }
        args = {
            "file_id": self.files["movie.srt"],
            "payload": {"item_id": "movie", "component": claim},
            "evidence": {"review": "fixture"},
        }
        proposal = self.differential(
            "proposal-put", args, lambda app: Curation(app).put(**args)
        )
        decision = {"id": proposal["id"], "accept": True, "actor": "reviewer"}
        self.differential(
            "proposal-decide",
            decision,
            lambda app: Curation(app).decide(
                proposal["id"], accept=True, actor="reviewer"
            ),
        )
        updated = {
            **args,
            "payload": {
                "item_id": "movie",
                "component": {
                    "occurrence_id": row["occurrence_id"],
                    "attributes": {"language": "fr"},
                    "supersedes": [proposal["id"]],
                },
            },
        }
        proposal = self.differential(
            "proposal-put", updated, lambda app: Curation(app).put(**updated)
        )
        self.differential(
            "proposal-decide",
            {**decision, "id": proposal["id"]},
            lambda app: Curation(app).decide(
                proposal["id"], accept=True, actor="reviewer"
            ),
        )
        invalid = {**args, "file_id": self.files["movie.mkv"]}
        self.error("proposal-put", invalid, lambda app: Curation(app).put(**invalid))
