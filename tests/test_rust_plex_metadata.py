# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Frozen metadata scenarios against separate catalogs and fake remote states.

Run with pinned 3.14 and the frozen reference's src first on PYTHONPATH. No
credentials, catalogs or Plex services outside disposable loopback fixtures are
used. Only event UUIDs and event creation timestamps are normalized.
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
from catabolic.plex_metadata import run
from catabolic.store import Store
from tests import test_plex_matching as matching
from tests import test_plex_metadata as frozen
from tests.test_rust_migration import BINARY


class NativeFixture:
    def protocol(self, route, method="GET", params=None):
        body = frozen.PlexMetadataTest.protocol(self, route, method, params)
        transform = getattr(self, "wire_transform", None)
        return transform(route, body) if transform and method == "GET" else body

    def start_remote(self):
        self.assertTrue(BINARY.is_file(), "build catabolic-cli before qualification")
        self.compare_reference = True
        self.fixture_failures = []
        self.native_calls = []
        self.native_headers = []
        self.block_route = None
        self.block_method = "GET"
        self.block_after_effect = False
        self.entered = threading.Event()
        self.release = threading.Event()
        self.on_native = None
        self.detail_body = None
        self.wire_transform = None
        self.lose_response = False
        credential = patch.dict(os.environ, {"PLEX_TEST_TOKEN": "FAKE_METADATA_ONLY"})
        credential.start()
        self.addCleanup(credential.stop)
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.0"

            def log_message(self, *_):
                pass

            def do_GET(self):
                self.respond()

            def do_PUT(self):
                self.respond()

            def respond(self):
                route = urlsplit(self.path)
                params = {
                    k: v[0]
                    for k, v in parse_qs(route.query, keep_blank_values=True).items()
                }
                for key in list(params):
                    if key in (
                        "includeGuids",
                        "X-Plex-Container-Start",
                        "X-Plex-Container-Size",
                        "type",
                    ) or key.endswith(".locked"):
                        params[key] = int(params[key])
                    elif key == "year.value" and params[key]:
                        params[key] = int(params[key])
                fixture.native_calls.append((route.path, self.command, params or None))
                fixture.native_headers.append(
                    {k.lower(): v for k, v in self.headers.items()}
                )
                blocked = (
                    fixture.block_route == route.path
                    and fixture.block_method == self.command
                )
                try:
                    if fixture.on_native:
                        fixture.on_native(route.path, self.command)
                    if blocked and not fixture.block_after_effect:
                        fixture.entered.set()
                        if not fixture.release.wait(12):
                            raise RuntimeError("fixture was not released")
                    body = fixture.protocol(route.path, self.command, params or None)
                    if (
                        route.path.startswith("/library/metadata/")
                        and fixture.detail_body is not None
                    ):
                        body = fixture.detail_body
                    if blocked and fixture.block_after_effect:
                        fixture.entered.set()
                        if not fixture.release.wait(12):
                            raise RuntimeError("fixture was not released")
                    if self.command == "PUT" and fixture.lose_response:
                        self.close_connection = True
                        return
                    status = 200
                except ConsumerError as exc:
                    status = {
                        "unauthorized": 401,
                        "rate_limited": 429,
                        "unavailable": 404,
                    }.get(exc.code, 500)
                    body = b"fixture response"
                except BaseException as exc:
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
        with Store(self.database, writable=True) as store, store.transaction() as db:
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

    def command(self, **kwargs):
        return [
            str(BINARY),
            "--db",
            str(self.database),
            "operation",
            "plex-metadata",
            "run",
            "--options",
            json.dumps({"connection": "home", "library_id": "7", **kwargs}),
        ]

    @staticmethod
    def normalized(value):
        if isinstance(value, dict):
            return {
                k: NativeFixture.normalized(v)
                for k, v in value.items()
                if k != "event_id"
            }
        if isinstance(value, list):
            return [NativeFixture.normalized(v) for v in value]
        return value

    @staticmethod
    def contents(database):
        with Store(database) as store:
            result = {}
            for table in (
                "items",
                "identities",
                "item_files",
                "mappings",
                "owned_links",
                "proposals",
                "decision_events",
                "consumer_events",
            ):
                result[table] = store.rows("SELECT * FROM " + table + " ORDER BY rowid")
                if table == "consumer_events":
                    result[table] = [
                        {k: v for k, v in row.items() if k not in ("id", "created_at")}
                        for row in result[table]
                    ]
            return result

    def reference(self, kwargs):
        original = self.database
        reference = self.root / "metadata-reference.db"
        shutil.copyfile(original, reference)
        saved = {
            name: getattr(self, name)
            for name in (
                "nodes",
                "node",
                "calls",
                "detail_reads",
                "put_error",
                "put_ignored",
                "after_put",
                "on_read",
                "server",
                "library_uuid",
                "library_type",
            )
        }
        self.database = reference
        self.nodes = copy.deepcopy(self.nodes)
        self.node = self.nodes[0]
        self.calls = []
        expected = error = None
        try:
            try:
                expected = run(reference, "default", "home", "7", **kwargs)
            except ConsumerError as exc:
                error = exc.code
            state = [ET.tostring(node) for node in self.nodes]
            puts = self.writes()
            contents = self.contents(reference)
            return expected, error, state, puts, contents
        finally:
            self.database = original
            for name, value in saved.items():
                setattr(self, name, value)

    def invoke(self, **kwargs):
        before = self.database.read_bytes()
        compared = self.reference(kwargs) if self.compare_reference else None
        start = len(self.calls)
        process = subprocess.run(
            self.command(**kwargs), capture_output=True, text=True, timeout=40
        )
        self.assertNotIn("FAKE_METADATA_ONLY", process.stdout + process.stderr)
        if not kwargs.get("apply") and self.on_read is None and self.on_native is None:
            self.assertEqual(before, self.database.read_bytes())
        if process.returncode:
            self.assertEqual(process.returncode, 2, process.stderr)
            code = re.search(r"consumer ([a-z_]+)", process.stderr)
            if compared:
                self.assertIsNotNone(
                    compared[1],
                    "native rejected successful reference: " + process.stderr,
                )
                self.assertIn(compared[1], process.stderr)
                self.assertEqual(self.contents(self.database), compared[4])
            raise ConsumerError(code.group(1) if code else process.stderr)
        actual = json.loads(process.stdout)
        if compared:
            expected, error, state, puts, contents = compared
            self.assertIsNone(error, "native accepted rejected reference")
            self.assertEqual(self.normalized(actual), self.normalized(expected))
            self.assertEqual([ET.tostring(n) for n in self.nodes], state)
            self.assertEqual([c for c in self.calls[start:] if c[1] == "PUT"], puts)
            self.assertEqual(self.contents(self.database), contents)
        return actual

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


class NativePlexMetadataTest(NativeFixture, frozen.PlexMetadataTest):
    def setUp(self):
        super().setUp()
        self.start_remote()

    def preview(self, **kwargs):
        kwargs.setdefault("fields", ["title", "summary", "year"])
        kwargs.setdefault("item_ids", [self.item_id])
        return self.invoke(**kwargs)

    def test_cli_preview_apply_exit_codes_and_events(self):
        preview = self.preview(fields=["title"])
        result = subprocess.run(
            self.command(item_ids=[self.item_id], fields=["title"], apply=True),
            capture_output=True,
            text=True,
            timeout=20,
        )
        self.assertEqual(result.returncode, 2)
        self.put_ignored = True
        self.assertFalse(self.apply(preview, fields=["title"])["complete"])
        self.put_ignored = False
        self.assertTrue(self.apply(preview, fields=["title"])["complete"])

    def test_process_interruption_leaves_started_intent(self):
        preview = self.preview()
        self.compare_reference = False
        self.block_route, self.block_method = "/library/sections/7/all", "PUT"
        self.block_after_effect = True
        process = self.native_process(
            item_ids=[self.item_id],
            fields=["title", "summary", "year"],
            apply=True,
            expected_plan=preview["plan_id"],
        )
        self.assertTrue(self.entered.wait(10))
        self.assertEqual(self.events()[0]["state"], "started")
        process.send_signal(signal.SIGINT)
        time.sleep(0.1)
        count = len(self.native_calls)
        self.release.set()
        out, err = process.communicate(timeout=20)
        self.assertEqual(process.returncode, 130, out + err)
        self.assertEqual(len(self.native_calls), count)
        self.assertEqual(self.events()[0]["state"], "started")
        self.assertEqual(self.node.get("title"), "Curated title")
        self.assertEqual(len(self.writes()), 1)

    def test_put_encodes_values_and_keeps_credentials_in_headers(self):
        self.curate(title="Été & title.locked=0 / ?", summary="Line one\nLine two")
        self.apply(fields=["title", "summary"], lock_fields=True)
        call = next(c for c in self.native_calls if c[1] == "PUT")
        self.assertEqual(call[2]["title.value"], "Été & title.locked=0 / ?")
        self.assertEqual(call[2]["summary.value"], "Line one\nLine two")
        for headers in self.native_headers:
            self.assertEqual(headers["x-plex-token"], "FAKE_METADATA_ONLY")
        self.assertNotIn("FAKE_METADATA_ONLY", json.dumps(self.native_calls))

    def test_lost_response_after_effect_is_uncertain_without_replay(self):
        preview = self.preview()
        self.compare_reference = False
        self.lose_response = True
        result = self.apply(preview)
        self.assertFalse(result["complete"])
        self.assertEqual(result["results"][0]["state"], "uncertain")
        self.assertEqual(self.events()[0]["state"], "uncertain")
        self.assertEqual(len(self.writes()), 1)
        self.assertEqual(self.node.get("title"), "Curated title")
        self.lose_response = False
        self.assertEqual(self.apply()["results"][0]["state"], "unchanged")
        self.assertEqual(len(self.writes()), 1)

    def test_cancelled_preview_issues_no_following_request(self):
        self.block_route = "/"
        process = self.native_process(item_ids=[self.item_id], fields=["title"])
        self.assertTrue(self.entered.wait(10))
        process.send_signal(signal.SIGTERM)
        time.sleep(0.1)
        self.release.set()
        out, err = process.communicate(timeout=20)
        self.assertEqual(process.returncode, 130, out + err)
        self.assertEqual([c[0] for c in self.native_calls], ["/"])
        self.assertEqual(self.events(), [])

    def test_publication_serialization_does_not_hold_store_writer(self):
        preview = self.preview()
        self.compare_reference = False
        self.block_route, self.block_method = "/library/sections/7/all", "PUT"
        self.block_after_effect = True
        args = dict(
            item_ids=[self.item_id],
            fields=["title", "summary", "year"],
            apply=True,
            expected_plan=preview["plan_id"],
        )
        process = self.native_process(**args)
        self.assertTrue(self.entered.wait(10))
        # Normal writers remain independent; another publisher must refuse.
        with Store(self.database, writable=True) as store, store.transaction() as db:
            db.execute(
                "INSERT INTO meta(key,value) VALUES ('fixture.writer-progress','ok')"
            )
        blocked = subprocess.run(
            self.command(**args), capture_output=True, text=True, timeout=5
        )
        self.assertEqual(blocked.returncode, 2)
        self.assertIn("metadata_writer_busy_preview_again", blocked.stderr)
        self.release.set()
        out, err = process.communicate(timeout=20)
        self.assertEqual(process.returncode, 0, out + err)
        self.assertTrue(json.loads(out)["complete"])
        self.assertEqual(len(self.writes()), 1)

    def test_request_and_plan_byte_budgets(self):
        self.curate(title="x" * 8192, summary="y" * 8192)
        with self.assertRaisesRegex(ConsumerError, "request_too_large"):
            self.preview(fields=["title", "summary"])
        self.curate(title="A valid short title")
        with Store(self.database, writable=True) as store, store.transaction() as db:
            row = store.rows("SELECT id,evidence FROM proposals")[0]
            evidence = json.loads(row["evidence"])
            evidence["fixture_padding"] = "x" * (4 * 1024 * 1024)
            db.execute(
                "UPDATE proposals SET evidence=? WHERE id=?",
                (json.dumps(evidence), row["id"]),
            )
        with self.assertRaisesRegex(ConsumerError, "plan_too_large"):
            self.preview(fields=["title"])
        self.assertEqual(self.writes(), [])

    def test_year_zero_date_and_nonboolean_options_refused(self):
        self.curate(release_date="0000-01-01")
        with self.assertRaisesRegex(ConsumerError, "invalid_release_date"):
            self.preview(fields=["release_date"])
        for option in (
            {"lock_fields": 1},
            {"unlock_fields": "false"},
            {"apply": None},
            {"lock_fields": True, "unlock_fields": True},
        ):
            with (
                self.subTest(option=option),
                self.assertRaisesRegex(ConsumerError, "invalid_options"),
            ):
                self.preview(fields=["title"], **option)
        self.assertEqual(self.writes(), [])

    def test_namespaced_detail_attributes_and_field_locks(self):
        self.node.set("{urn:fixture}ratingKey", "forged")
        self.node.set("{urn:fixture}title", "unselected")
        self.preview(fields=["title"])
        original = self.node.attrib.pop("ratingKey")
        with self.assertRaisesRegex(ConsumerError, "remote_item_mismatch"):
            self.preview(fields=["title"])
        self.node.set("ratingKey", original)
        ET.SubElement(self.node, "Field", name="unselected", locked="2")
        with self.assertRaisesRegex(ConsumerError, "invalid_field_locks"):
            self.preview(fields=["title"])
        self.node.remove(self.node.find("Field"))
        ET.SubElement(self.node, "Field", name="title", locked="0")
        ET.SubElement(self.node, "Field", name="title", locked="1")
        with self.assertRaisesRegex(ConsumerError, "invalid_field_locks"):
            self.preview(fields=["title"])
        self.assertEqual(self.writes(), [])

    def test_remote_selected_field_and_year_bounds(self):
        self.node.set("title", "x" * 65537)
        with self.assertRaisesRegex(ConsumerError, "remote_field_too_large"):
            self.preview(fields=["title"])
        self.node.set("title", "Plex title")
        for raw in ("1.5", "10000", "-1", "１"):
            self.node.set("year", raw)
            with (
                self.subTest(raw=raw),
                self.assertRaisesRegex(ConsumerError, "invalid_remote_year"),
            ):
                self.preview(fields=["year"])
        self.assertEqual(self.writes(), [])

    def test_last_detached_read_local_change_blocks_before_put(self):
        # Explicit safety tightening: frozen code could PUT and only then notice
        # a local edit during this last detached detail read. Native fences again.
        preview = self.preview()
        self.compare_reference = False
        self.detail_reads = 0

        def edit():
            if self.detail_reads == 2:
                self.curate(
                    title="Concurrent local edit", summary="Curated summary", year=2025
                )

        self.on_read = edit
        result = self.apply(preview)
        self.assertFalse(result["complete"])
        self.assertEqual(result["results"][0]["state"], "blocked")
        self.assertEqual(self.events(), [])
        self.assertEqual(self.writes(), [])

    def test_post_effect_identity_change_is_uncertain(self):
        preview = self.preview()
        self.compare_reference = False
        self.after_put = lambda: setattr(self, "server", "replacement")
        result = self.apply(preview)
        self.assertFalse(result["complete"])
        self.assertEqual(result["results"][0]["state"], "uncertain")
        self.assertEqual(
            result["results"][0]["error"]["code"], "server_identity_changed"
        )
        self.assertEqual(self.events()[0]["state"], "uncertain")
        self.assertEqual(len(self.writes()), 1)

    def test_catalog_replacement_after_effect_retains_unacknowledged_intent(self):
        preview = self.preview()
        self.compare_reference = False

        def replace():
            with (
                Store(self.database, writable=True) as store,
                store.transaction() as db,
            ):
                db.execute(
                    "UPDATE meta SET value='00000000-0000-4000-8000-000000000001' WHERE key='database_id'"
                )

        self.after_put = replace
        result = subprocess.run(
            self.command(
                item_ids=[self.item_id],
                fields=["title", "summary", "year"],
                apply=True,
                expected_plan=preview["plan_id"],
            ),
            capture_output=True,
            text=True,
            timeout=20,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("catalog replaced during detached execution", result.stderr)
        self.assertEqual(self.events()[0]["state"], "started")
        self.assertEqual(len(self.writes()), 1)

    def test_discovery_namespace_identity_and_library_fences(self):
        def transform(route, body):
            root = ET.fromstring(body)
            if route == "/":
                root.set(
                    "{urn:fixture}machineIdentifier",
                    root.attrib.pop("machineIdentifier"),
                )
            return ET.tostring(root)

        self.wire_transform = transform
        with self.assertRaisesRegex(ConsumerError, "invalid_configuration"):
            self.preview(fields=["title"])

        def transform(route, body):
            root = ET.fromstring(body)
            if route == "/library/sections":
                root[0].tag = "{urn:fixture}Directory"
            return ET.tostring(root)

        self.wire_transform = transform
        with self.assertRaisesRegex(ConsumerError, "unknown_library"):
            self.preview(fields=["title"])

        def transform(route, body):
            root = ET.fromstring(body)
            if route == "/library/sections":
                root[0].set("{urn:fixture}uuid", root[0].attrib.pop("uuid"))
            return ET.tostring(root)

        self.wire_transform = transform
        with self.assertRaisesRegex(ConsumerError, "invalid_configuration"):
            self.preview(fields=["title"])
        self.assertEqual(self.writes(), [])

    def test_discovery_foreign_attributes_cannot_shadow_real_values(self):
        def transform(route, body):
            root = ET.fromstring(body)
            if route == "/":
                real = dict(root.attrib)
                root.attrib.clear()
                root.set("{urn:fixture}machineIdentifier", "forged")
                root.attrib.update(real)
            if route == "/library/sections":
                real = dict(root[0].attrib)
                root[0].attrib.clear()
                root[0].set("{urn:fixture}uuid", "forged")
                root[0].attrib.update(real)
                foreign = ET.Element("{urn:fixture}Directory", **real)
                root.insert(0, foreign)
            return ET.tostring(root)

        self.wire_transform = transform
        self.assertTrue(self.preview(fields=["title"])["complete"])

    def test_utf16_adapter_preview_matches_frozen_reference(self):
        # The historical metadata checkpoint recorded this as a known gap.
        # Shared XML decoding now has separate frozen-oracle wire evidence.
        for encoding in ("utf-16", "utf-16-le", "utf-16-be"):
            with self.subTest(encoding=encoding):
                self.wire_transform = lambda route, body, encoding=encoding: (
                    body.decode("utf-8").encode(encoding)
                )
                self.assertTrue(self.preview(fields=["title"])["complete"])
        self.assertEqual(self.writes(), [])
        self.assertEqual(self.events(), [])


class NativeMatchedMetadataTest(NativeFixture, matching.PlexMatchingTest):
    def setUp(self):
        super().setUp()
        self.start_remote()

    def preview(self, match_id, **kwargs):
        kwargs.setdefault("fields", ["title", "summary"])
        return self.invoke(item_ids=[], match_ids=[match_id], **kwargs)

    def test_concurrent_catabolic_writer_blocked(self):
        # The frozen test holds the real database's metadata lock; an isolated
        # oracle copy would intentionally use a distinct lock.
        self.compare_reference = False
        super().test_concurrent_catabolic_writer_blocked()

    def test_duplicate_matches_cannot_update_one_remote_item_twice(self):
        from catabolic.app import Application

        first = self.save_match()
        with Store(self.database, writable=True) as store:
            Application(store).media.associate(
                self.file_id,
                self.item_id,
                metadata={"title": "Edited", "summary": "Summary"},
            )
        second = self.save_match()
        self.assertNotEqual(first, second)
        with self.assertRaisesRegex(ConsumerError, "duplicate_remote_item"):
            self.invoke(item_ids=[], match_ids=[first, second], fields=["title"])

    def test_metadata_match_cli_and_listing(self):
        first = self.save_match()
        preview = self.preview(first, fields=["title"])
        result = self.preview(
            first, fields=["title"], apply=True, expected_plan=preview["plan_id"]
        )
        self.assertTrue(result["complete"])
        self.assertEqual(result["results"][0]["association_id"], self.association["id"])

    def test_remote_rating_key_cannot_select_multiple_items(self):
        first = self.save_match()
        for key in ("1,2", 123):
            with (
                Store(self.database, writable=True) as store,
                store.transaction() as db,
            ):
                row = store.rows("SELECT evidence FROM proposals WHERE id=?", (first,))[
                    0
                ]
                evidence = json.loads(row["evidence"])
                evidence["rating_key"] = key
                db.execute(
                    "UPDATE proposals SET evidence=? WHERE id=?",
                    (json.dumps(evidence), first),
                )
            with (
                self.subTest(key=key),
                self.assertRaisesRegex(ConsumerError, "invalid_rating_key"),
            ):
                self.preview(first)
        self.assertFalse(
            any(c[0].startswith("/library/metadata/") for c in self.native_calls)
        )

    def test_match_selection_accepts_null_item_list(self):
        first = self.save_match()
        self.assertTrue(
            self.invoke(item_ids=None, match_ids=[first], fields=["title"])["complete"]
        )
