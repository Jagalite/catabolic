# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import contextlib
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
from catabolic.plex_import import run as import_plex
from catabolic.plex_metadata import run
from catabolic.store import Store


class PlexMetadataTest(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.database = self.root / "catalog.db"
        self.source = self.root / "source"
        self.source.mkdir()
        (self.source / "movie.mkv").write_bytes(b"original")
        Store.initialize(self.database)
        with Store(self.database, writable=True) as store:
            app = Application(store)
            app.bind("source", "seed", str(self.source))
            app.scan()
        self.server = "server-one"
        self.library_uuid = "library-one"
        self.library_type = "movie"
        self.node = ET.Element(
            "Video",
            ratingKey="1",
            librarySectionID="7",
            type="movie",
            title="Plex title",
            year="2024",
            summary="Plex summary",
            guid="plex://movie/example",
        )
        media = ET.SubElement(self.node, "Media")
        ET.SubElement(media, "Part", file="/media/movie.mkv")
        self.nodes = [self.node]
        self.calls = []
        self.put_error = None
        self.put_ignored = False
        self.after_put = None
        self.on_read = None
        self.detail_reads = 0
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
        mapping = [{"remote_root": "/media", "location": "seed"}]
        args = (self.database, "default", "home", "7", mapping)
        preview = import_plex(*args)
        imported = import_plex(*args, apply=True, expected_plan=preview["plan_id"])
        self.item_id = imported["imported"][0]["item_id"]
        self.curate(title="Curated title", summary="Curated summary", year=2025)
        self.calls.clear()

    def curate(self, **metadata):
        with Store(self.database, writable=True) as store:
            Application(store).put_item(
                self.node.get("type"), {}, metadata, self.item_id
            )

    def protocol(self, route, method="GET", params=None):
        # Every read and write must occur outside a local Store writer lifetime.
        with Store(self.database, writable=True):
            pass
        self.calls.append((route, method, params))
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
        if method == "PUT":
            self.assertEqual(route, "/library/sections/7/all")
            node = next(n for n in self.nodes if n.get("ratingKey") == params["id"])
            self.assertEqual(
                params["type"],
                {"movie": 1, "episode": 4, "track": 10, "photo": 13}[node.get("type")],
            )
            if self.put_error:
                raise self.put_error
            if not self.put_ignored:
                for name, val in params.items():
                    if name.endswith(".value"):
                        node.set(name[:-6], str(val))
                    if name.endswith(".locked"):
                        key = name[:-7]
                        for f in list(node.findall("Field")):
                            if f.get("name") == key:
                                node.remove(f)
                        ET.SubElement(node, "Field", name=key, locked=str(val))
            if self.after_put:
                self.after_put()
            return b""
        self.assertEqual(method, "GET")
        root = ET.Element("MediaContainer", size="1")
        if route == "/library/sections/7/all":
            root.set("totalSize", str(len(self.nodes)))
            root.set("offset", str(params["X-Plex-Container-Start"]))
        else:
            self.assertTrue(route.startswith("/library/metadata/"))
            self.assertEqual(params, {"includeGuids": 1})
            self.detail_reads += 1
            if self.on_read:
                self.on_read()
        if route == "/library/sections/7/all":
            offset = params["X-Plex-Container-Start"]
            nodes = self.nodes[offset : offset + params["X-Plex-Container-Size"]]
            root.extend(nodes)
            root.set("size", str(len(nodes)))
        else:
            key = route.rsplit("/", 1)[1]
            root.append(self.nodes[int(key) - 1])
        return ET.tostring(root)

    def preview(self, **kwargs):
        kwargs.setdefault("fields", ["title", "summary", "year"])
        kwargs.setdefault("item_ids", [self.item_id])
        return run(self.database, "default", "home", "7", **kwargs)

    def apply(self, preview=None, **kwargs):
        preview = preview or self.preview(**kwargs)
        return self.preview(apply=True, expected_plan=preview["plan_id"], **kwargs)

    def writes(self):
        return [c for c in self.calls if c[1] == "PUT"]

    def events(self):
        with Store(self.database) as store:
            return [
                json.loads(r["subject"])
                for r in store.rows(
                    "SELECT subject FROM consumer_events WHERE event='metadata_publish'"
                )
            ]

    def test_preview_apply_verified_and_noop_repeat(self):
        before = self.database.read_bytes()
        preview = self.preview()
        self.assertEqual(self.database.read_bytes(), before)
        self.assertEqual(self.writes(), [])
        self.assertEqual(
            preview["items"][0]["changes"]["title"],
            {
                "before": {"value": "Plex title", "locked": False},
                "after": {"value": "Curated title", "locked": False},
            },
        )
        result = self.apply(preview)
        self.assertTrue(result["complete"])
        self.assertEqual(result["results"][0]["state"], "verified")
        self.assertEqual(self.node.get("title"), "Curated title")
        self.assertEqual(self.node.get("year"), "2025")
        self.assertEqual(self.events()[0]["state"], "verified")
        self.assertEqual(self.apply()["results"][0]["state"], "unchanged")
        self.assertEqual(len(self.writes()), 1)
        self.assertEqual(len(self.events()), 1)
        self.assertEqual((self.source / "movie.mkv").read_bytes(), b"original")
        with Store(self.database) as store:
            self.assertEqual(
                json.loads(store.rows("SELECT metadata FROM items")[0]["metadata"])[
                    "title"
                ],
                "Curated title",
            )

    def test_selected_fields_only_and_preserve_locks(self):
        ET.SubElement(self.node, "Field", name="title", locked="1")
        self.apply(fields=["title"])
        self.assertEqual(
            self.writes()[0][2],
            {"id": "1", "type": 1, "title.value": "Curated title", "title.locked": 1},
        )
        self.assertEqual(self.node.get("summary"), "Plex summary")
        self.assertEqual(self.node.get("year"), "2024")

    def test_lock_only_change_and_preview_options_pinned(self):
        self.curate(title="Plex title")
        preview = self.preview(fields=["title"])
        with self.assertRaisesRegex(ConsumerError, "plan_changed"):
            self.apply(preview, fields=["title"], lock_fields=True)
        self.assertEqual(self.writes(), [])
        locked = self.preview(fields=["title"], lock_fields=True)
        self.assertTrue(locked["items"][0]["changes"]["title"]["after"]["locked"])
        self.apply(locked, fields=["title"], lock_fields=True)
        self.assertEqual(self.writes()[0][2]["title.locked"], 1)

    def test_missing_or_changed_plan_never_writes(self):
        with self.assertRaisesRegex(ConsumerError, "requires_expected_plan"):
            self.preview(apply=True)
        preview = self.preview()
        self.node.set("title", "Edited in Plex")
        with self.assertRaisesRegex(ConsumerError, "plan_changed"):
            self.apply(preview)
        preview = self.preview()
        self.curate(title="New local edit", summary="Curated summary", year=2025)
        with self.assertRaisesRegex(ConsumerError, "plan_changed"):
            self.apply(preview)
        preview = self.preview()
        ET.SubElement(self.node, "Field", name="summary", locked="1")
        with self.assertRaisesRegex(ConsumerError, "plan_changed"):
            self.apply(preview)
        self.assertEqual(self.writes(), [])

    def test_identity_and_path_checks(self):
        mutations = [
            ("ratingKey", "2"),
            ("librarySectionID", "8"),
            ("type", "episode"),
            ("guid", "plex://movie/replaced"),
        ]
        for name, new in mutations:
            with self.subTest(name=name):
                old = self.node.get(name)
                self.node.set(name, new)
                with self.assertRaises(ConsumerError):
                    self.preview()
                self.node.set(name, old)
        self.node.find("Media/Part").set("file", "/media/other.mkv")
        with self.assertRaisesRegex(ConsumerError, "identity_changed"):
            self.preview()
        self.assertEqual(self.writes(), [])

    def test_server_and_library_replacement(self):
        preview = self.preview()
        self.server = "replacement"
        with self.assertRaisesRegex(ConsumerError, "server_identity_changed"):
            self.apply(preview)
        self.server = "server-one"
        self.library_uuid = "replacement"
        with self.assertRaisesRegex(ConsumerError, "imported_plex_identity"):
            self.apply(preview)
        self.assertEqual(self.writes(), [])

    def test_no_provenance_or_inactive_association_rejected(self):
        with Store(self.database, writable=True) as store, store.transaction() as db:
            db.execute("UPDATE item_files SET active=0")
        with self.assertRaisesRegex(ConsumerError, "requires_import_provenance"):
            self.preview()
        with Store(self.database, writable=True) as store, store.transaction() as db:
            db.execute("UPDATE item_files SET active=1")
            db.execute("UPDATE proposals SET source='manual'")
        with self.assertRaisesRegex(ConsumerError, "requires_import_provenance"):
            self.preview()
        self.assertEqual(self.writes(), [])

    def test_invalid_or_missing_fields_and_bounded_requests(self):
        for fields in ([], ["arbitrary"], ["studio"]):
            with self.subTest(fields=fields), self.assertRaises(ConsumerError):
                self.preview(fields=fields)
        for metadata in (
            {"title": ""},
            {"title": None},
            {"title": "bad\x00"},
            {"title": "x" * 8193},
            {"year": True},
            {"year": 10000},
            {"release_date": "2025-02-30"},
        ):
            with self.subTest(metadata=metadata):
                self.curate(**metadata)
                with self.assertRaises(ConsumerError):
                    self.preview(fields=list(metadata))
        for ids in ([], ["x"] * 101):
            with self.assertRaises(ConsumerError):
                self.preview(item_ids=ids)
        self.assertEqual(self.writes(), [])

    def test_explicit_empty_summary_and_unicode(self):
        self.curate(title="Été & 冬", summary="")
        self.apply(fields=["title", "summary"])
        self.assertEqual(self.node.get("title"), "Été & 冬")
        self.assertEqual(self.node.get("summary"), "")

    def test_unverified_write_is_not_success_or_retried(self):
        self.put_ignored = True
        result = self.apply()
        self.assertFalse(result["complete"])
        self.assertEqual(result["results"][0]["state"], "uncertain")
        self.assertIn("verification_failed", result["results"][0]["error"]["code"])
        self.assertEqual(self.events()[0]["state"], "uncertain")
        self.assertEqual(len(self.writes()), 1)

    def test_write_errors_and_lost_response_require_new_preview(self):
        for code in ("unauthorized", "rate_limited", "temporarily_failed"):
            with self.subTest(code=code):
                self.put_error = ConsumerError(code)
                result = self.apply()
                self.assertFalse(result["complete"])
                self.assertEqual(result["results"][0]["state"], "uncertain")
                self.assertEqual(result["results"][0]["error"]["code"], code)
        self.put_error = None
        self.after_put = lambda: (_ for _ in ()).throw(
            ConsumerError("temporarily_failed")
        )
        result = self.apply()
        self.assertFalse(result["complete"])
        self.after_put = None
        self.assertEqual(self.preview()["items"][0]["changes"], {})
        count = len(self.writes())
        self.assertTrue(self.apply()["complete"])
        self.assertEqual(len(self.writes()), count)

    def test_process_interruption_leaves_started_intent(self):
        self.after_put = lambda: (_ for _ in ()).throw(KeyboardInterrupt())
        with self.assertRaises(KeyboardInterrupt):
            self.apply()
        self.assertEqual(self.events()[0]["state"], "started")
        self.assertEqual(self.node.get("title"), "Curated title")

    def test_remote_edit_just_before_put_blocks(self):
        preview = self.preview()
        self.detail_reads = 0

        def change():
            if self.detail_reads == 2:
                self.node.set("title", "Concurrent edit")

        self.on_read = change
        result = self.apply(preview)
        self.assertFalse(result["complete"])
        self.assertEqual(result["results"][0]["state"], "blocked")
        self.assertEqual(self.writes(), [])

    def test_local_edit_during_publish_not_claimed_verified(self):
        self.after_put = lambda: self.curate(
            title="Changed again", summary="Curated summary", year=2025
        )
        result = self.apply()
        self.assertFalse(result["complete"])
        self.assertEqual(
            result["results"][0]["error"]["code"],
            "metadata_local_changed_during_publish",
        )

    def add_item(self, key):
        import copy

        (self.source / f"movie{key}.mkv").write_bytes(b"another original")
        with Store(self.database, writable=True) as store:
            Application(store).scan()
        node = copy.deepcopy(self.node)
        node.set("ratingKey", key)
        node.set("guid", f"plex://movie/{key}")
        node.find("Media/Part").set("file", f"/media/movie{key}.mkv")
        self.nodes.append(node)
        args = (
            self.database,
            "default",
            "home",
            "7",
            [{"remote_root": "/media", "location": "seed"}],
        )
        preview = import_plex(*args)
        result = import_plex(*args, apply=True, expected_plan=preview["plan_id"])
        item_id = result["imported"][-1]["item_id"]
        with Store(self.database, writable=True) as store:
            Application(store).put_item(
                "movie", {}, {"title": f"Curated {key}"}, item_id
            )
        return item_id

    def test_batch_stops_on_uncertain_write_and_reports_remaining(self):
        ids = sorted([self.item_id, self.add_item("2"), self.add_item("3")])
        preview = self.preview(item_ids=ids, fields=["title"])

        def fail_next():
            self.put_error = ConsumerError("unauthorized")

        self.after_put = fail_next
        result = self.apply(preview, item_ids=ids, fields=["title"])
        self.assertFalse(result["complete"])
        self.assertEqual(
            [r["state"] for r in result["results"]], ["verified", "uncertain"]
        )
        self.assertEqual(result["remaining_item_ids"], ids[2:])
        self.assertEqual(len(self.writes()), 2)
        self.assertFalse(result["results"][1]["error"]["safe_to_retry"])

    def test_entire_batch_validated_before_writes(self):
        second = self.add_item("2")
        with self.assertRaisesRegex(ConsumerError, "selected_field_missing"):
            self.apply(item_ids=[self.item_id, second], fields=["summary"])
        self.assertEqual(self.writes(), [])

    def test_local_change_during_remote_preview_rejected(self):
        self.on_read = lambda: self.curate(
            title="Concurrent", summary="Curated summary", year=2025
        )
        with self.assertRaisesRegex(ConsumerError, "local_changed"):
            self.preview()
        self.assertEqual(self.writes(), [])

    def test_item_kind_specific_fields(self):
        # Use an accepted import for each supported kind, retaining its file identity.
        for kind, library, fields in (
            ("episode", "show", ["title", "summary"]),
            ("track", "artist", ["title"]),
            ("photo", "photo", ["title", "summary"]),
        ):
            with self.subTest(kind=kind):
                self.node.set("type", kind)
                self.library_type = library
                with (
                    Store(self.database, writable=True) as store,
                    store.transaction() as db,
                ):
                    db.execute(
                        "UPDATE items SET kind=? WHERE id=?", (kind, self.item_id)
                    )
                self.curate(title="New " + kind, summary="Description")
                self.apply(fields=fields)
                self.assertEqual(self.node.get("title"), "New " + kind)
                with self.assertRaisesRegex(ConsumerError, "unsupported_for_item_kind"):
                    self.preview(fields=["studio"])

    def test_cli_preview_apply_exit_codes_and_events(self):
        args = [
            "--db",
            str(self.database),
            "--json",
            "consumer",
            "metadata",
            "home",
            "--library-id",
            "7",
            "--item",
            self.item_id,
            "--field",
            "title",
        ]

        def cli(extra):
            output, error = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(output), contextlib.redirect_stderr(error):
                code = main(args + extra)
            return code, json.loads(output.getvalue()) if output.getvalue() else None

        code, preview = cli([])
        self.assertEqual(code, 0)
        self.assertEqual(cli(["--apply"])[0], 2)
        self.put_ignored = True
        code, result = cli(["--apply", "--expected-plan", preview["plan_id"]])
        self.assertEqual(code, 3)
        self.assertFalse(result["complete"])
        self.put_ignored = False
        code, result = cli(["--apply", "--expected-plan", preview["plan_id"]])
        self.assertEqual(code, 0)
        self.assertTrue(result["complete"])


class PlexMetadataWireTest(unittest.TestCase):
    def test_put_encodes_values_and_keeps_credentials_in_headers(self):
        from urllib.parse import parse_qs, urlsplit

        from catabolic.plex_metadata import parameters

        row = {
            "rating_key": "42",
            "kind": "movie",
            "changes": {
                "title": {
                    "after": {"value": "Été & title.locked=0 / ?", "locked": True}
                },
                "summary": {"after": {"value": "Line one\nLine two", "locked": False}},
            },
        }
        connection = {
            "application": "plex",
            "endpoint": "https://plex.test:32400",
            "credential_env": "PLEX_TEST_TOKEN",
        }
        with (
            patch.dict("os.environ", {"PLEX_TEST_TOKEN": "private-secret"}),
            patch("catabolic.consumer_adapters.request", return_value=b"") as request,
        ):
            Plex(connection).call("/library/sections/7/all", "PUT", parameters(row))
        args, kwargs = request.call_args
        url = urlsplit(args[0])
        self.assertEqual(url.path, "/library/sections/7/all")
        self.assertEqual(
            parse_qs(url.query),
            {
                "id": ["42"],
                "type": ["1"],
                "title.value": ["Été & title.locked=0 / ?"],
                "title.locked": ["1"],
                "summary.value": ["Line one\nLine two"],
                "summary.locked": ["0"],
            },
        )
        self.assertEqual(kwargs["method"], "PUT")
        self.assertEqual(kwargs["headers"]["X-Plex-Token"], "private-secret")
        self.assertNotIn("private-secret", args[0])
        self.assertEqual(kwargs["timeout"], 15)


if __name__ == "__main__":
    unittest.main()
