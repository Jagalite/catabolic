# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native matching creation and frozen downstream provenance qualification.

The fourteen frozen scenarios are inherited unchanged except the legacy CLI
case, which uses companion run/list. Metadata publication remains the frozen
Python owner, operating only against a fake service. This is deliberately not
evidence of a native metadata publication implementation.
"""

import copy
import json
import os
import re
import shutil
import signal
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from unittest.mock import patch
from urllib.parse import parse_qs, urlsplit
from xml.etree import ElementTree as ET

from catabolic.consumer_adapters import ConsumerError
from catabolic.plex_matching import match
from catabolic.store import Store
from tests import test_plex_matching as frozen
from tests.test_rust_migration import BINARY


class NativePlexMatchingTest(frozen.PlexMatchingTest):
    def setUp(self):
        self.assertTrue(BINARY.is_file(), "build catabolic-cli before qualification")
        super().setUp()
        self.compare_reference = True
        self.fixture_failures = []
        self.block_route = None
        self.entered = threading.Event()
        self.release = threading.Event()
        self.native_calls = []
        self.detail_body = None
        self.page_body = None
        self.on_native = None
        credential = patch.dict(os.environ, {"PLEX_TEST_TOKEN": "FAKE_MATCH_ONLY"})
        credential.start()
        self.addCleanup(credential.stop)
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.0"

            def log_message(self, *_):
                pass

            def do_GET(self):
                route = urlsplit(self.path)
                params = {k: v[0] for k, v in parse_qs(route.query).items()}
                for key in (
                    "includeGuids",
                    "X-Plex-Container-Start",
                    "X-Plex-Container-Size",
                ):
                    if key in params:
                        params[key] = int(params[key])
                fixture.native_calls.append(route.path)
                try:
                    if fixture.on_native:
                        fixture.on_native(route.path)
                    if fixture.block_route == route.path:
                        fixture.entered.set()
                        fixture.release.wait(15)
                    body = fixture.protocol(route.path, params=params or None)
                    if (
                        route.path.startswith("/library/metadata/")
                        and fixture.detail_body is not None
                    ):
                        body = fixture.detail_body
                    if route.path.endswith("/all") and fixture.page_body is not None:
                        body = fixture.page_body
                    status = 200
                except Exception as exc:
                    fixture.fixture_failures.append(repr(exc))
                    body, status = b"fixture failure", 500
                self.send_response(status)
                self.send_header("Content-Type", "application/xml")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                try:
                    self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError):
                    pass

        self.remote = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.remote.serve_forever, daemon=True).start()
        self.addCleanup(self.close_server)
        with Store(self.database, writable=True) as s, s.transaction() as db:
            db.execute(
                "UPDATE consumer_connections SET endpoint=?",
                (f"http://127.0.0.1:{self.remote.server_port}",),
            )
        self.calls.clear()

    def close_server(self):
        self.release.set()
        self.remote.shutdown()
        self.remote.server_close()
        self.assertEqual(self.fixture_failures, [])

    def command(self, operation="run", **kwargs):
        request = dict(binding_id="fixture", mapping_id=self.mapping["id"], **kwargs)
        return [
            str(BINARY),
            "--db",
            str(self.database),
            "operation",
            "plex-match",
            operation,
            "--options",
            json.dumps(request),
        ]

    def native_process(self, **kwargs):
        process = subprocess.Popen(
            self.command(**kwargs),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(self.reap, process)
        return process

    @staticmethod
    def reap(process):
        if process.poll() is None:
            process.kill()
        process.communicate(timeout=20)

    def preview_match(self, **kwargs):
        before = self.database.read_bytes()
        expected = None
        reference_error = None
        applied = kwargs.get("apply", False)
        if self.compare_reference:
            reference = self.database
            if applied:
                reference = self.root / "reference.db"
                shutil.copyfile(self.database, reference)
            try:
                expected = match(
                    reference, "default", "fixture", self.mapping["id"], **kwargs
                )
            except ConsumerError as exc:
                reference_error = exc.code
            if not applied:
                self.assertEqual(before, self.database.read_bytes())
        result = subprocess.run(
            self.command(**kwargs), capture_output=True, text=True, timeout=40
        )
        if not applied and self.on_native is None:
            self.assertEqual(before, self.database.read_bytes())
        self.assertNotIn("FAKE_MATCH_ONLY", result.stdout + result.stderr)
        if result.returncode:
            self.assertEqual(result.returncode, 2, result.stderr)
            if reference_error:
                self.assertIn(reference_error, result.stderr)
            elif expected is not None:
                self.fail("native rejected successful reference: " + result.stderr)
            code = re.search(r"consumer ([a-z_]+)", result.stderr)
            raise ConsumerError(code.group(1) if code else result.stderr)
        self.assertIsNone(reference_error, "native accepted rejected reference")
        actual = json.loads(result.stdout)
        if expected is not None:
            self.assertEqual(actual, expected)
        if applied and expected is not None:
            with (
                Store(self.root / "reference.db") as reference,
                Store(self.database) as native,
            ):
                for table in (
                    "items",
                    "identities",
                    "item_files",
                    "mappings",
                    "owned_links",
                    "proposals",
                    "decision_events",
                ):

                    def stable(store, table=table):
                        return [
                            {k: v for k, v in row.items() if k != "created_at"}
                            for row in store.rows("SELECT * FROM " + table)
                        ]

                    self.assertEqual(stable(reference), stable(native), table)
        return actual

    def test_metadata_match_cli_and_listing(self):
        preview = self.preview_match()
        applied = self.preview_match(apply=True, expected_plan=preview["plan_id"])
        listing = subprocess.run(
            self.command("list"), capture_output=True, text=True, timeout=20
        )
        self.assertEqual(listing.returncode, 0, listing.stderr)
        self.assertEqual(
            json.loads(listing.stdout)["matches"][0]["id"], applied["match_id"]
        )
        preview = self.preview(applied["match_id"], fields=["title"])
        result = self.preview(
            applied["match_id"],
            fields=["title"],
            apply=True,
            expected_plan=preview["plan_id"],
        )
        self.assertTrue(result["complete"])
        self.assertEqual(result["results"][0]["association_id"], self.association["id"])

    def test_full_library_scan_and_boundary_match(self):
        self.nodes = []
        for index in range(101):
            node = copy.deepcopy(self.node)
            node.set("ratingKey", str(index + 1))
            node.find("Media/Part").set("file", f"/media/other-{index}.mkv")
            self.nodes.append(node)
        self.nodes[-1].find("Media/Part").set("file", "/media/movie.mkv")
        result = self.preview_match(max_items=101)
        self.assertEqual(result["match"]["rating_key"], "101")
        self.assertEqual(self.native_calls.count("/library/sections/7/all"), 2)

    def test_invalid_options_are_local(self):
        for options in (
            {"max_items": True},
            {"max_items": 0},
            {"max_items": 10001},
            {"max_items": 1.5},
            {"metadata_source": None},
            {"metadata_source": "other"},
        ):
            with (
                self.subTest(options=options),
                self.assertRaisesRegex(ConsumerError, "invalid_match_options"),
            ):
                self.preview_match(**options)
        self.assertEqual(self.native_calls, [])

    def test_detached_writer_and_changed_connection_refuse(self):
        self.compare_reference = False
        changed = False

        def change(route):
            nonlocal changed
            if not changed and route.endswith("/all"):
                with Store(self.database, writable=True) as s, s.transaction() as db:
                    db.execute("UPDATE consumer_connections SET revision=revision+1")
                changed = True

        self.on_native = change
        with self.assertRaisesRegex(ConsumerError, "local_changed"):
            self.preview_match()
        self.assertTrue(changed)
        with Store(self.database) as s:
            self.assertFalse(
                s.rows("SELECT id FROM proposals WHERE source='plex-match'")
            )

    def test_detail_identity_and_namespace_checks(self):
        self.compare_reference = False
        cases = [
            b'<MediaContainer xmlns:x="urn:test"><Video x:ratingKey="1" librarySectionID="7" type="movie"/></MediaContainer>',
            b'<MediaContainer><Video ratingKey="1" librarySectionID="7" type="movie" guid="changed"><Media><Part file="/media/movie.mkv"/></Media></Video></MediaContainer>',
            b'<MediaContainer><Video ratingKey="1" librarySectionID="7" type="movie" guid="plex://movie/example"><Media><Part file="/media/movie.mkv"/></Media><Field name="title" locked="maybe"/></Video></MediaContainer>',
        ]
        for body in cases:
            self.detail_body = body
            with self.subTest(body=body), self.assertRaises(ConsumerError):
                self.preview_match()

    def test_sigint_during_read_does_not_admit_proposal(self):
        preview = self.preview_match()
        self.block_route = "/library/metadata/1"
        self.native_calls.clear()
        before = self.database.read_bytes()
        process = self.native_process(apply=True, expected_plan=preview["plan_id"])
        self.assertTrue(self.entered.wait(10))
        with Store(self.database, writable=True):
            pass
        process.send_signal(signal.SIGINT)
        time.sleep(0.1)
        self.release.set()
        stdout, stderr = process.communicate(timeout=20)
        self.assertEqual(process.returncode, 130, stdout + stderr)
        self.assertEqual(before, self.database.read_bytes())
        self.assertEqual(self.native_calls[-1], "/library/metadata/1")

    def test_sigint_final_identity_does_not_request_library(self):
        self.compare_reference = False
        count = 0

        def block_second_identity(route):
            nonlocal count
            if route == "/":
                count += 1
                if count == 2:
                    self.block_route = "/"

        self.on_native = block_second_identity
        process = self.native_process()
        self.assertTrue(self.entered.wait(10))
        process.send_signal(signal.SIGINT)
        time.sleep(0.1)
        self.release.set()
        stdout, stderr = process.communicate(timeout=20)
        self.assertEqual(process.returncode, 130, stdout + stderr)
        self.assertEqual(self.native_calls[-1], "/")
        self.assertEqual(self.native_calls.count("/library/sections"), 1)

    def test_page_part_record_and_response_budgets(self):
        self.compare_reference = False
        original = copy.deepcopy(self.node)
        media = self.node.find("Media")
        for index in range(101):
            ET.SubElement(media, "Part", file=f"/media/part-{index}.mkv")
        with self.assertRaisesRegex(ConsumerError, "import_part_limit"):
            self.preview_match()
        self.node = original
        self.nodes = [self.node]
        self.node.set("summary", "é" * 65537)
        with self.assertRaisesRegex(ConsumerError, "invalid_configuration"):
            self.preview_match()
        self.node.attrib.pop("summary")
        self.page_body = (
            b"<MediaContainer>" + b" " * (8 * 1024 * 1024) + b"</MediaContainer>"
        )
        with self.assertRaisesRegex(ConsumerError, "response_limit"):
            self.preview_match()

    def test_malformed_and_inconsistent_pages(self):
        self.compare_reference = False
        for body in (
            b'<!DOCTYPE MediaContainer><MediaContainer totalSize="0" size="0" offset="0"/>',
            b'<MediaContainer totalSize="1" size="0" offset="0"/>',
            b'<MediaContainer totalSize="0" size="0" offset="1"/>',
            b'<MediaContainer totalSize="999999999999999999999999999999999999" size="0" offset="0"/>',
        ):
            self.page_body = body
            with self.subTest(body=body), self.assertRaises(ConsumerError):
                self.preview_match()
        self.assertFalse(
            any(route.startswith("/library/metadata/") for route in self.native_calls)
        )

    def test_native_rejects_changed_link_disabled_binding_and_mapping(self):
        self.compare_reference = False
        with Store(self.database, writable=True) as s, s.transaction() as db:
            db.execute("UPDATE consumer_bindings SET enabled=0")
        with self.assertRaisesRegex(ConsumerError, "binding_disabled"):
            self.preview_match()
        with Store(self.database, writable=True) as s, s.transaction() as db:
            db.execute("UPDATE consumer_bindings SET enabled=1")
            db.execute("UPDATE mappings SET active=0")
        with self.assertRaisesRegex(ConsumerError, "mapping_not_active"):
            self.preview_match()
        with Store(self.database, writable=True) as s, s.transaction() as db:
            db.execute("UPDATE mappings SET active=1")
        (self.output / "movie.mkv").unlink()
        (self.output / "movie.mkv").symlink_to("elsewhere")
        with self.assertRaisesRegex(ConsumerError, "published_link_changed"):
            self.preview_match()
        self.assertEqual(self.native_calls, [])

    def test_catalog_replacement_while_detached_refuses(self):
        self.compare_reference = False
        replacement = self.root / "replacement.db"
        Store.initialize(replacement)
        changed = False

        def replace(route):
            nonlocal changed
            if not changed and route.endswith("/all"):
                os.replace(replacement, self.database)
                changed = True

        self.on_native = replace
        result = subprocess.run(
            self.command(), capture_output=True, text=True, timeout=30
        )
        self.assertTrue(changed)
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn("catalog", result.stderr)
        with Store(self.database) as s:
            self.assertEqual(s.rows("SELECT * FROM proposals"), [])

    def test_detail_foreign_attributes_do_not_shadow_plain_identity(self):
        root = ET.Element("MediaContainer", size="1")
        node = copy.deepcopy(self.node)
        values = dict(node.attrib)
        node.attrib.clear()
        node.set("{urn:foreign}ratingKey", "wrong")
        node.set("{urn:foreign}guid", "wrong")
        node.attrib.update(values)
        root.append(node)
        self.detail_body = ET.tostring(root)
        self.preview_match()

    def test_invalid_rating_key_is_rejected_by_native_before_detail_read(self):
        self.node.set("ratingKey", "1,2")
        with self.assertRaisesRegex(ConsumerError, "invalid_rating_key"):
            self.preview_match()
        self.assertFalse(
            any(route.startswith("/library/metadata/") for route in self.native_calls)
        )
