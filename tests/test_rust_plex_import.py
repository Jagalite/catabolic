# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native Plex import qualification against the frozen Python behavior.

Run with the frozen reference's src directory first on PYTHONPATH and this
checkout available for tests. Every ordinary preview compares the entire JSON
plan, including its digest, and checks byte-for-byte database immutability.
All protocol traffic goes to an ephemeral HTTP/1.1 loopback server using a fake
credential. The first nineteen cases retain the frozen Plex import scenarios;
additional cases exercise actual process cancellation and concurrency.
"""

import copy
import json
import os
import re
import shutil
import signal
import subprocess
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch
from urllib.parse import parse_qs, urlsplit
from xml.etree import ElementTree as ET

from catabolic.app import Application
from catabolic.consumer_adapters import ConsumerError
from catabolic.consumer_setup import put_connection
from catabolic.plex_import import run
from catabolic.store import Store
from tests.test_rust_migration import BINARY


class NativePlexImportTest(unittest.TestCase):
    def setUp(self):
        self.assertTrue(BINARY.is_file(), "build catabolic-cli before qualification")
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
        self.lock_failures = []
        self.fixture_failures = []
        self.on_page = None
        self.page_body = None
        self.page_status = 200
        self.page_entered = threading.Event()
        self.page_release = threading.Event()
        self.block_page = False
        self.compare_reference = True
        self.mapping = [{"remote_root": "/media", "location": "seed"}]
        credential = patch.dict(
            os.environ, {"PLEX_TEST_TOKEN": "FAKE_ONLY_DO_NOT_LEAK"}
        )
        credential.start()
        self.addCleanup(credential.stop)
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_):
                pass

            def do_GET(self):
                route = urlsplit(self.path)
                params = parse_qs(route.query)
                try:
                    body = fixture.protocol(route.path, params)
                    status = fixture.page_status if route.path.endswith("/all") else 200
                except Exception as exc:
                    fixture.fixture_failures.append(repr(exc))
                    status, body = 500, b"fixture failure"
                self.send_response(status)
                self.send_header("Content-Type", "application/xml")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                try:
                    self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError):
                    # A cancelled native process may have closed its socket.
                    pass

        self.remote = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=self.remote.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(self.close_server)
        put_connection(
            self.database,
            "default",
            "home",
            "plex",
            f"http://127.0.0.1:{self.remote.server_port}",
            "PLEX_TEST_TOKEN",
            apply=True,
        )
        self.calls.clear()

    def close_server(self):
        self.page_release.set()
        self.remote.shutdown()
        self.remote.server_close()
        self.assertEqual(self.lock_failures, [])
        self.assertEqual(self.fixture_failures, [])

    def protocol(self, route, params):
        try:
            with Store(self.database, writable=True):
                pass
        except Exception as exc:
            self.lock_failures.append(str(exc))
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
        self.assertEqual(params["includeGuids"], ["1"])
        self.assertEqual(params["sort"], ["id:asc"])
        expected_type = {"movie": "1", "show": "4", "artist": "10", "photo": "13"}
        self.assertEqual(params["type"], [expected_type[self.library_type]])
        offset = int(params["X-Plex-Container-Start"][0])
        limit = int(params["X-Plex-Container-Size"][0])
        rows = self.items[offset : offset + limit]
        root = ET.Element(
            "MediaContainer",
            totalSize=str(len(self.items)),
            offset=str(offset),
            size=str(len(rows)),
        )
        root.attrib.update(self.bad_page)
        root.extend(rows)
        body = self.page_body if self.page_body is not None else ET.tostring(root)
        if self.on_page:
            self.on_page()
        if self.block_page:
            self.page_entered.set()
            if not self.page_release.wait(12):
                raise RuntimeError("test did not release blocked page")
        return body

    def command(self, **kwargs):
        options = {
            "connection": "home",
            "library_id": "7",
            "mappings": self.mapping,
            **kwargs,
        }
        return [
            str(BINARY),
            "--db",
            str(self.database),
            "operation",
            "plex-import",
            "run",
            "--options",
            json.dumps(options),
        ]

    def native_process(self, **kwargs):
        process = subprocess.Popen(
            self.command(**kwargs),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=dict(os.environ),
        )
        self.addCleanup(self.reap_process, process)
        return process

    @staticmethod
    def reap_process(process):
        if process.poll() is None:
            process.kill()
        process.communicate(timeout=10)

    def preview(self, **kwargs):
        readonly = not kwargs.get("apply", False)
        before = self.database.read_bytes()
        expected = None
        reference_error = None
        if readonly and self.compare_reference:
            try:
                expected = run(
                    self.database, "default", "home", "7", self.mapping, **kwargs
                )
            except ConsumerError as exc:
                reference_error = exc.code
            self.assertEqual(self.database.read_bytes(), before)
        result = subprocess.run(
            self.command(**kwargs),
            capture_output=True,
            text=True,
            timeout=40,
            env=dict(os.environ),
        )
        if readonly:
            self.assertEqual(self.database.read_bytes(), before)
        self.assertNotIn("FAKE_ONLY_DO_NOT_LEAK", result.stdout + result.stderr)
        self.assertNotIn("PLEX_TEST_TOKEN", result.stdout)
        self.assertNotIn("credential_env", result.stdout)
        if result.returncode:
            self.assertEqual(result.returncode, 2, result.stderr or result.stdout)
            if reference_error:
                self.assertIn(reference_error, result.stderr)
            elif expected is not None:
                self.fail(
                    "native rejected successful reference preview: " + result.stderr
                )
            code = re.search(r"consumer ([a-z_]+)", result.stderr)
            raise ConsumerError(code.group(1) if code else result.stderr)
        self.assertIsNone(reference_error, "native accepted rejected Python input")
        actual = json.loads(result.stdout)
        if expected is not None:
            self.assertEqual(actual, expected)
        return actual

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
                self.mapping = mapping
                self.preview()

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
        preview = self.preview()
        result = self.apply(preview)
        self.assertEqual(len(result["imported"]), 1)
        self.assertNotIn("PLEX_TEST_TOKEN", json.dumps(result))
        self.assertNotIn("credential_env", json.dumps(result))

    def test_library_changes_during_read_rejected(self):
        self.compare_reference = False

        def replace_library():
            self.library_uuid = "replaced-during-read"

        self.on_page = replace_library
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
        # A real process is interrupted while fetching the next reviewed page.
        # Runtime callback tests separately cover interruption within one batch.
        (self.source / "second.mkv").write_bytes(b"second")
        with Store(self.database, writable=True) as store:
            Application(store).scan()
        self.items.append(self.item("2", "/media/second.mkv"))
        self.apply(limit=1)
        preview = self.preview(offset=1, limit=1)
        before = self.database.read_bytes()
        self.block_page = True
        process = self.native_process(
            offset=1, limit=1, apply=True, expected_plan=preview["plan_id"]
        )
        self.assertTrue(self.page_entered.wait(10), "native did not request next page")
        process.send_signal(signal.SIGINT)
        # Keep the response blocked until the signal is actually delivered.
        time.sleep(0.1)
        self.page_release.set()
        stdout, stderr = process.communicate(timeout=10)
        self.assertEqual(process.returncode, 130, stderr or stdout)
        self.assertEqual(self.database.read_bytes(), before)
        self.assertEqual(len(self.rows("items")), 1)
        self.block_page = False
        done = self.apply(offset=1, limit=1)
        self.assertEqual(len(done["imported"]), 1)
        self.assertEqual(len(self.rows("items")), 2)
        self.assertEqual(len(self.rows("proposals")), 2)
        self.assertEqual(self.apply(offset=1, limit=1)["imported"], [])

    def test_empty_library_and_invalid_page_bounds(self):
        self.items = []
        self.assertTrue(self.apply()["complete"])
        self.assertEqual(self.rows("items"), [])
        for kwargs in ({"limit": 101}, {"limit": 0}, {"offset": -1}):
            with self.subTest(kwargs=kwargs), self.assertRaises(ConsumerError):
                self.preview(**kwargs)

    def test_writer_progresses_while_network_read_waits(self):
        self.block_page = True
        process = self.native_process()
        self.assertTrue(self.page_entered.wait(10), "native did not request page")
        started = time.monotonic()
        with Store(self.database, writable=True) as store:
            Application(store).put_item(
                "movie", {"manual": "parallel-writer"}, {"title": "Parallel writer"}
            )
        self.assertLess(time.monotonic() - started, 2)
        before = self.database.read_bytes()
        self.page_release.set()
        stdout, stderr = process.communicate(timeout=10)
        self.assertEqual(process.returncode, 0, stderr)
        self.assertEqual(len(json.loads(stdout)["candidates"]), 1)
        self.assertEqual(self.database.read_bytes(), before)
        self.assertEqual(len(self.rows("items")), 1)

    def test_connection_change_during_network_read_rejected(self):
        self.compare_reference = False

        def change_connection():
            with Store(self.database, writable=True) as store:
                with store.transaction() as db:
                    db.execute(
                        "UPDATE consumer_connections SET revision=revision+1 "
                        "WHERE profile='default' AND id='home'"
                    )

        self.on_page = change_connection
        process = subprocess.run(
            self.command(), capture_output=True, text=True, timeout=20
        )
        self.assertEqual(process.returncode, 2, process.stdout)
        self.assertIn("import_connection_changed", process.stderr)
        self.assertEqual(self.rows("items"), [])
        self.assertEqual(self.rows("proposals"), [])

    def test_malformed_xml_and_wrong_container_rejected_readonly(self):
        for body in (b"not XML", b"<MediaContainer", b"<Wrong />"):
            with self.subTest(body=body):
                self.page_body = body
                with self.assertRaisesRegex(ConsumerError, "invalid_response"):
                    self.preview()
        self.assertEqual(self.rows("items"), [])
        self.assertEqual(self.rows("proposals"), [])

    def test_count_bounds_and_non_ascii_numeric_fields(self):
        for field, value in (("year", "-1"), ("duration", "1.5"), ("year", "２０２４")):
            with self.subTest(field=field, value=value):
                self.items = [self.item("1", "/media/movie.mkv")]
                self.items[0].set(field, value)
                with self.assertRaisesRegex(ConsumerError, "invalid_import_" + field):
                    self.preview()
        self.items = [self.item("1", "/media/movie.mkv")]
        for _ in range(100):
            ET.SubElement(self.items[0].find("Media"), "Part", file="/media/movie.mkv")
        with self.assertRaisesRegex(ConsumerError, "import_part_limit"):
            self.preview()
        self.items = [self.item(str(index), "/media/movie.mkv") for index in range(11)]
        for item in self.items:
            for _ in range(99):
                ET.SubElement(item.find("Media"), "Part", file="/media/movie.mkv")
        with self.assertRaisesRegex(ConsumerError, "import_page_part_limit"):
            self.preview()
        self.assertEqual(self.rows("proposals"), [])

    def test_http_failure_is_bounded_and_keeps_existing_data(self):
        self.apply()
        for status, error in (
            (401, "unauthorized"),
            (429, "rate_limited"),
            (503, "temporarily_failed"),
        ):
            with self.subTest(status=status):
                self.page_status = status
                self.page_body = b"FAKE_ONLY_DO_NOT_LEAK"
                with self.assertRaisesRegex(ConsumerError, error):
                    self.preview()
        self.assertEqual(len(self.rows("items")), 1)
        self.assertEqual(len(self.rows("proposals")), 1)

    def test_plan_size_is_bounded(self):
        # A small wire response expands into a separately bounded candidate plan.
        self.items = [self.item("1", "/media/movie.mkv")]
        self.items[0].set("summary", "x" * 60000)
        for index in range(99):
            filename = f"part-{index}.mkv"
            (self.source / filename).write_bytes(b"part")
            ET.SubElement(
                self.items[0].find("Media"), "Part", file="/media/" + filename
            )
        with Store(self.database, writable=True) as store:
            Application(store).scan()
        with self.assertRaisesRegex(
            ConsumerError, "import_plan_too_large_reduce_limit"
        ):
            self.preview()

    def test_applied_catalog_and_provenance_match_frozen_reference(self):
        preview = self.preview()
        reference = self.root / "reference.db"
        shutil.copyfile(self.database, reference)
        expected = run(
            reference,
            "default",
            "home",
            "7",
            self.mapping,
            apply=True,
            expected_plan=preview["plan_id"],
        )
        actual = self.apply(preview)
        for result in (actual, expected):
            for imported in result["imported"]:
                imported.pop("proposal_id")
                imported.pop("association_id")
        self.assertEqual(actual, expected)
        for table, columns in (
            ("items", "*"),
            ("identities", "*"),
            ("item_files", "file_id,item_id,role,part,metadata,origin,active"),
            (
                "proposals",
                "profile,file_id,kind,payload,evidence,source,snapshot,state",
            ),
            ("decision_events", "action,actor,before_value"),
        ):
            with self.subTest(table=table), Store(reference) as store:
                self.assertEqual(
                    store_rows(self.database, table, columns),
                    store.rows("SELECT " + columns + " FROM " + table + " ORDER BY 1"),
                )

    def test_import_xml_attributes_require_no_namespace(self):
        # ElementTree's unqualified get() ignores namespaced attributes. The
        # import parser must do likewise for both required and optional fields.
        for selector, attribute, value, error in (
            ("root", "totalSize", "1", "invalid_import_total_size"),
            ("root", "size", "1", "invalid_import_size"),
            ("root", "offset", "9", None),
            ("item", "ratingKey", "1", "invalid_configuration"),
            ("item", "type", "movie", None),
            ("item", "title", "Example", None),
            ("item", "year", "not-a-number", None),
            ("item", "guid", "plex://movie/foreign", None),
            ("part", "file", "/media/movie.mkv", "invalid_configuration"),
            ("guid", "id", "imdb://foreign", "invalid_configuration"),
        ):
            with self.subTest(selector=selector, attribute=attribute):
                item = self.item("1", "/media/movie.mkv")
                root = ET.Element("MediaContainer", totalSize="1", size="1", offset="0")
                root.append(item)
                node = {
                    "root": root,
                    "item": item,
                    "part": item.find("Media/Part"),
                    "guid": item.find("Guid"),
                }[selector]
                node.attrib.pop(attribute, None)
                node.set("{urn:import-review}" + attribute, value)
                self.page_body = ET.tostring(root)
                if error:
                    with self.assertRaisesRegex(ConsumerError, error):
                        self.preview()
                else:
                    self.preview()
        self.assertEqual(self.rows("items"), [])
        self.assertEqual(self.rows("proposals"), [])

    def test_import_xml_ignores_namespaced_attribute_collisions(self):
        # Put the foreign attribute first so a local-name-only lookup cannot
        # accidentally pass by finding the genuine unqualified value first.
        for selector, attribute, foreign in (
            ("root", "totalSize", "0"),
            ("root", "size", "0"),
            ("root", "offset", "9"),
            ("item", "ratingKey", "foreign-key"),
            ("item", "type", "clip"),
            ("item", "title", "Foreign title"),
            ("item", "year", "bad"),
            ("item", "guid", "plex://movie/foreign"),
            ("part", "file", "/media/absent.mkv"),
            ("guid", "id", "imdb://foreign"),
        ):
            with self.subTest(selector=selector, attribute=attribute):
                item = self.item("1", "/media/movie.mkv")
                root = ET.Element("MediaContainer", totalSize="1", size="1", offset="0")
                root.append(item)
                node = {
                    "root": root,
                    "item": item,
                    "part": item.find("Media/Part"),
                    "guid": item.find("Guid"),
                }[selector]
                original = dict(node.attrib)
                node.attrib.clear()
                node.set("{urn:import-review}" + attribute, foreign)
                node.attrib.update(original)
                self.page_body = ET.tostring(root)
                result = self.preview()
                self.assertTrue(result["complete"])
                self.assertEqual(len(result["candidates"]), 1)

    def test_import_xml_child_namespaces_match_frozen_reference(self):
        for selector in ("item", "media", "part", "guid"):
            with self.subTest(selector=selector):
                item = self.item("1", "/media/movie.mkv")
                node = {
                    "item": item,
                    "media": item.find("Media"),
                    "part": item.find("Media/Part"),
                    "guid": item.find("Guid"),
                }[selector]
                node.tag = "{urn:import-review}" + node.tag
                self.items = [item]
                result = self.preview()
                self.assertEqual(result["complete"], selector in ("item", "guid"))

    def test_import_decimal_digit_ceiling_and_large_metadata(self):
        for attribute, value, accepted in (
            ("year", "18446744073709551616001", True),
            ("duration", "0" * 4300, True),
            ("year", "1" * 4300, True),
            ("year", "1" * 4301, False),
            ("year", "0" * 4301, False),
        ):
            with self.subTest(attribute=attribute, digits=len(value)):
                self.items = [self.item("1", "/media/movie.mkv")]
                self.items[0].set(attribute, value)
                if accepted:
                    result = self.preview()
                    key = "duration_ms" if attribute == "duration" else attribute
                    self.assertEqual(
                        result["candidates"][0]["payload"]["item"]["metadata"][key],
                        int(value),
                    )
                else:
                    with self.assertRaisesRegex(
                        ConsumerError, "invalid_import_" + attribute
                    ):
                        self.preview()

    def test_cancellation_between_post_read_validation_requests(self):
        preview = self.preview()
        self.calls.clear()
        before = self.database.read_bytes()
        entered = threading.Event()
        release = threading.Event()
        original_protocol = self.protocol

        def protocol(route, params):
            body = original_protocol(route, params)
            if route == "/" and sum(path == "/" for path, _ in self.calls) == 2:
                entered.set()
                if not release.wait(10):
                    raise RuntimeError(
                        "test did not release post-read identity request"
                    )
            return body

        self.protocol = protocol
        process = self.native_process(apply=True, expected_plan=preview["plan_id"])
        try:
            self.assertTrue(entered.wait(10), "native did not recheck server identity")
            process.send_signal(signal.SIGINT)
            time.sleep(0.1)
            release.set()
            stdout, stderr = process.communicate(timeout=10)
            self.assertEqual(process.returncode, 130, stderr or stdout)
            self.assertEqual(self.database.read_bytes(), before)
            self.assertEqual(self.rows("proposals"), [])
            self.assertEqual(
                sum(path == "/library/sections" for path, _ in self.calls),
                1,
                "cancellation must not start another managed HTTP wait",
            )
        finally:
            release.set()

    def test_catalog_replacement_during_remote_read_refuses_admission(self):
        preview = self.preview()
        self.block_page = True
        process = self.native_process(apply=True, expected_plan=preview["plan_id"])
        self.assertTrue(self.page_entered.wait(10), "native did not request page")
        replacement = self.root / "replacement.db"
        Store.initialize(replacement)
        before = replacement.read_bytes()
        os.replace(replacement, self.database)
        self.page_release.set()
        stdout, stderr = process.communicate(timeout=10)
        self.assertEqual(process.returncode, 2, stderr or stdout)
        self.assertIn("catalog replaced during detached execution", stderr)
        self.assertEqual(self.database.read_bytes(), before)
        self.assertEqual(self.rows("items"), [])
        self.assertEqual(self.rows("proposals"), [])


def store_rows(database, table, columns):
    with Store(database) as store:
        return store.rows("SELECT " + columns + " FROM " + table + " ORDER BY 1")
