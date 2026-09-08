# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Loopback Plex/Jellyfin protocol, actual links, lock release and crash fencing."""

import contextlib
import io
import json
import os
import sqlite3
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
from catabolic.cli import main
from catabolic.consumer_adapters import ConsumerError, Plex, path, translate
from catabolic.consumer_setup import (
    bind,
    creation,
    discover,
    put_connection,
    verify_indexing,
)
from catabolic.consumers import acknowledge, claim, deliver, drain, status
from catabolic.migration import (
    data_snapshot,
    load_migrations,
    upgrade_database,
    validate_preservation,
)
from catabolic.network_adapters import Refresh
from catabolic.reconcile import Reconciler
from catabolic.store import Store, encode
from tests.test_migrations import create_legacy


class ProtocolServer:
    def __init__(self, database):
        self.database = database
        self.server_id = "fixture-server"
        self.libraries = [
            self.library("1", "/plex/Movies"),
            self.library("2", "/different"),
        ]
        self.requests, self.scans = [], []
        self.mode, self.busy, self.on_scan = 200, False, None
        self.lock_failures = []
        self.indexed = []
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                self.respond()

            def do_POST(self):
                self.respond()

            def respond(self):
                route = urlsplit(self.path)
                params = parse_qs(route.query)
                fixture.requests.append((self.command, route.path, params))
                try:
                    with Store(fixture.database, writable=True):
                        pass
                except Exception as exc:
                    fixture.lock_failures.append(str(exc))
                code = fixture.mode
                body = b""
                if code != 200:
                    body = b"DO_NOT_LEAK_TOKEN_OR_URL"
                elif route.path == "/":
                    body = ET.tostring(
                        ET.Element(
                            "MediaContainer",
                            machineIdentifier=fixture.server_id,
                            version="fixture-1",
                            readOnlyLibraries="0",
                        )
                    )
                elif route.path == "/library/sections" and self.command == "GET":
                    root = ET.Element("MediaContainer")
                    for row in fixture.libraries:
                        node = ET.SubElement(
                            root,
                            "Directory",
                            key=row["id"],
                            uuid=row["uuid"],
                            title=row["name"],
                            type=row["type"],
                            scanner=row["scanner"],
                            agent=row["agent"],
                            createdAt=row["created_at"],
                            refreshing=str(int(fixture.busy)),
                        )
                        for r in row["roots"]:
                            ET.SubElement(node, "Location", path=r)
                    body = ET.tostring(root)
                elif route.path == "/library/sections" and self.command == "POST":
                    row = fixture.library(
                        str(len(fixture.libraries) + 1), params["location"][0]
                    )
                    row.update(
                        name=params["name"][0],
                        scanner=params["scanner"][0],
                        agent=params["agent"][0],
                    )
                    fixture.libraries.append(row)
                elif route.path.startswith("/system/scanners/"):
                    body = b'<MediaContainer><Scanner name="Fixture Scanner"/></MediaContainer>'
                elif route.path == "/system/agents":
                    body = b'<MediaContainer><Agent identifier="fixture.agent"/></MediaContainer>'
                elif route.path.endswith("/refresh"):
                    fixture.scans.append(route.path)
                    if fixture.on_scan:
                        callback, fixture.on_scan = fixture.on_scan, None
                        callback()
                elif route.path.endswith("/all"):
                    root = ET.Element("MediaContainer")
                    for p in fixture.indexed:
                        ET.SubElement(
                            ET.SubElement(ET.SubElement(root, "Video"), "Media"),
                            "Part",
                            file=p,
                        )
                    body = ET.tostring(root)
                elif route.path == "/System/Info":
                    body = json.dumps(
                        {"Id": fixture.server_id, "Version": "fixture-jellyfin"}
                    ).encode()
                elif route.path == "/Library/VirtualFolders":
                    body = json.dumps(
                        [
                            {
                                "ItemId": r["id"],
                                "Name": r["name"],
                                "CollectionType": "movies",
                                "Locations": r["roots"],
                            }
                            for r in fixture.libraries
                        ]
                    ).encode()
                elif route.path.endswith("/Refresh"):
                    fixture.scans.append(route.path)
                else:
                    code = 404
                self.send_response(code)
                if code == 429:
                    self.send_header("Retry-After", "120")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.url = f"http://127.0.0.1:{self.server.server_port}"

    @staticmethod
    def library(identifier, root):
        return {
            "id": identifier,
            "uuid": "uuid-" + identifier,
            "name": "Duplicate title",
            "type": "movie",
            "roots": [root],
            "scanner": "Fixture Scanner",
            "agent": "fixture.agent",
            "created_at": "100",
        }

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


class ConsumerTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name).resolve()
        self.database = self.root / "catalog.sqlite3"
        self.source = self.root / "source"
        self.output = self.root / "output"
        self.source.mkdir()
        self.output.mkdir()
        Store.initialize(self.database)
        with Store(self.database, writable=True) as store:
            app = Application(store)
            app.bind("source", "media", str(self.source))
            app.bind("output", "global", str(self.output))
        self.remote = ProtocolServer(self.database)
        self.addCleanup(self.remote.close)
        environment = patch.dict(
            os.environ, {"CATABOLIC_TEST_PLEX_TOKEN": "DO_NOT_LEAK_TOKEN_OR_URL"}
        )
        environment.start()
        self.addCleanup(environment.stop)
        put_connection(
            self.database,
            "default",
            "plex",
            "plex",
            self.remote.url,
            "CATABOLIC_TEST_PLEX_TOKEN",
            apply=True,
        )

    def bind(self, **kwargs):
        values = dict(
            identifier="movies",
            connection_id="plex",
            catalog="global",
            subtree="Movies",
            remote_root="/plex/Movies",
            library_id="1",
            kind="movie",
            apply=True,
        )
        values.update(kwargs)
        return bind(self.database, "default", **values)

    def add(self, name="One", subtree="Movies"):
        (self.source / (name + ".mkv")).write_bytes(b"test media " + name.encode())
        with Store(self.database, writable=True) as store:
            app = Application(store)
            app.scan()
            file = next(
                r["id"] for r in app.files()["files"] if r["path"] == name + ".mkv"
            )
            item = app.put_item("movie", {"fixture": name}, {"title": name})["id"]
            app.media.associate(file, item)
            app.put_mapping("global", file, item, subtree + "/" + name + ".mkv")

    def sync(self, **kwargs):
        with Store(self.database, writable=True) as store:
            result = Reconciler(Application(store)).apply(**kwargs)
        return result

    def status(self):
        with Store(self.database) as store:
            return status(Application(store))

    def force_due(self):
        with Store(self.database, writable=True) as store, store.transaction() as db:
            db.execute("UPDATE consumer_deliveries SET due_at=0")
            db.execute("UPDATE consumer_bindings SET due_at=0")

    def test_discovery_duplicate_names_and_readonly_preview(self):
        report = discover(self.database, "default", "plex", "movie")
        self.assertEqual([r["id"] for r in report["libraries"]], ["1", "2"])
        self.assertEqual(report["creation_choices"]["scanners"], ["Fixture Scanner"])
        self.bind(apply=False)
        self.assertEqual(self.status()["bindings"], [])
        self.assertEqual(self.remote.scans, [])
        self.assertTrue(all(method == "GET" for method, _, _ in self.remote.requests))
        self.bind()
        self.assertEqual(self.status()["bindings"][0]["library"]["uuid"], "uuid-1")
        self.assertEqual(self.remote.lock_failures, [])

    def test_automatic_after_store_close_and_unchanged_repeat(self):
        self.bind(automatic=True)
        self.add()
        with Store(self.database, writable=True) as store:
            result = Reconciler(Application(store), notify_consumers=False).apply()
            self.assertEqual(self.remote.scans, [])
        self.assertTrue(result["healthy"])
        self.assertTrue(result["consumers"]["complete"])
        self.assertEqual(self.remote.scans, ["/library/sections/1/refresh"])
        self.sync()
        self.assertEqual(len(self.remote.scans), 1)
        self.assertEqual(self.remote.lock_failures, [])
        self.assertTrue(
            all(
                "force" not in q and "trash" not in p.lower() and m != "DELETE"
                for m, p, q in self.remote.requests
            )
        )

    def test_explicit_initial_scan_is_idempotent(self):
        self.add()
        self.sync()
        self.bind(automatic=True)
        self.assertEqual(self.remote.scans, [])
        self.bind(
            identifier="initial",
            library_id="2",
            remote_root="/different",
            automatic=True,
            initial_scan=True,
        )
        self.assertEqual(len(self.remote.scans), 1)
        self.bind(
            identifier="initial",
            library_id="2",
            remote_root="/different",
            automatic=True,
            initial_scan=True,
        )
        self.assertEqual(len(self.remote.scans), 1)

    def test_subtrees_coalesce_and_multiple_consumers(self):
        self.remote.libraries[0]["roots"] = ["/plex"]
        self.bind()
        self.bind(identifier="tv", subtree="TV", remote_root="/plex/TV")
        self.bind(identifier="other", library_id="2", remote_root="/different")
        self.add()
        self.add("Two", "TV")
        self.add("Three")
        self.sync()
        report = drain(self.database)
        self.assertEqual(report["processed"], 2)
        with Store(self.database) as store:
            self.assertEqual(
                len(
                    store.rows(
                        "SELECT * FROM consumer_events WHERE event='projection_updated'"
                    )
                ),
                1,
            )
        self.assertEqual(
            sorted(self.remote.scans),
            ["/library/sections/1/refresh", "/library/sections/2/refresh"],
        )
        with self.assertRaises(ConsumerError):
            self.bind(identifier="overlap", remote_root="/plex/Movies/Child")

    def test_changes_during_request_remain_pending(self):
        self.bind()
        self.add()
        self.sync()

        def change():
            self.add("Two")
            self.sync()

        self.remote.on_scan = change
        report = drain(self.database, limit=1)
        self.assertFalse(report["complete"])
        b = self.status()["bindings"][0]
        self.assertGreater(b["generation"], b["acknowledged"])
        self.assertTrue(drain(self.database, limit=1)["complete"])
        self.assertEqual(len(self.remote.scans), 2)

    def test_change_during_discovery_defers_without_repair_or_attempt_penalty(self):
        self.bind()
        self.add()
        self.sync()
        original = Plex.libraries

        def newer(adapter):
            libraries = original(adapter)
            self.add("Two")
            self.sync()
            return libraries

        with patch.object(Plex, "libraries", newer):
            result = drain(self.database, limit=1)
        self.assertEqual(result["deliveries"][0]["state"], "pending")
        self.assertEqual(
            result["deliveries"][0]["error"]["code"], "publication_changed"
        )
        self.assertEqual(self.status()["bindings"][0]["attempts"], 0)
        self.assertEqual(self.remote.scans, [])
        self.force_due()
        self.assertTrue(drain(self.database)["complete"])
        self.assertEqual(len(self.remote.scans), 1)

    def test_busy_scan_defers_without_acknowledgement(self):
        self.bind()
        self.add()
        self.sync()
        self.remote.busy = True
        self.assertFalse(drain(self.database)["complete"])
        self.assertEqual(self.status()["bindings"][0]["acknowledged"], 0)
        self.remote.busy = False
        self.force_due()
        self.assertTrue(drain(self.database)["complete"])

    def test_storage_outage_and_unverified_shared_binding_hold_library(self):
        self.remote.libraries[0]["roots"] = ["/plex"]
        self.bind()
        self.bind(identifier="tv", subtree="TV", remote_root="/plex/TV")
        self.add()
        self.sync()
        self.add("Two", "TV")
        (self.source / "Two.mkv").unlink()
        result = self.sync()
        self.assertFalse(result["healthy"])
        self.assertEqual(drain(self.database)["processed"], 0)
        self.assertEqual(self.remote.scans, [])
        self.output.rename(self.root / "offline")
        self.force_due()
        self.assertEqual(drain(self.database)["processed"], 0)

    def test_crash_after_filesystem_recovery_uses_shared_boundary(self):
        self.bind()
        self.add()

        def crash(operation):
            if operation["kind"] == "create":
                raise RuntimeError("crash")

        with self.assertRaises(RuntimeError):
            self.sync(after_filesystem=crash)
        self.assertEqual(self.remote.scans, [])
        with Store(self.database, writable=True) as store:
            Reconciler(Application(store), notify_consumers=False).recover()
        self.assertTrue(drain(self.database)["complete"])
        self.assertEqual(len(self.remote.scans), 1)

    def test_crash_after_ownership_before_eligibility_worker_recovers(self):
        self.bind()
        self.add()
        with patch("catabolic.consumers.published", side_effect=RuntimeError("crash")):
            with self.assertRaises(RuntimeError):
                self.sync()
        self.assertEqual(self.status()["bindings"][0]["state"], "unverified")
        self.assertTrue(drain(self.database)["complete"])

    def test_lease_contention_expiry_and_lost_acknowledgement(self):
        self.bind()
        self.add()
        result = self.sync()
        self.assertTrue(result["healthy"])
        self.assertFalse(result["consumers"]["complete"])
        first = claim(self.database, "default")
        self.assertIsNone(claim(self.database, "default"))
        Plex(first["connection"]).scan(first["library"])
        with Store(self.database, writable=True) as store, store.transaction() as db:
            db.execute("UPDATE consumer_deliveries SET lease_until=0")
        second = claim(self.database, "default")
        self.assertFalse(acknowledge(self.database, "default", first)["complete"])
        self.assertTrue(deliver(self.database, "default", second)["complete"])
        self.assertEqual(len(self.remote.scans), 2)

    def test_errors_rate_limits_exhaustion_and_zero_due_is_not_success(self):
        self.bind()
        self.add()
        self.sync()
        self.remote.mode = 429
        self.assertFalse(drain(self.database)["complete"])
        with Store(self.database) as store:
            row = store.rows("SELECT * FROM consumer_deliveries")[0]
            self.assertGreaterEqual(row["due_at"], time.time() + 100)
        self.assertEqual(drain(self.database)["processed"], 0)
        self.remote.mode = 503
        for _ in range(4):
            self.force_due()
            drain(self.database, limit=1)
        self.assertEqual(self.status()["bindings"][0]["state"], "exhausted")
        self.assertFalse(drain(self.database)["complete"])

    def test_auth_and_server_or_library_replacement_require_repair(self):
        for change in ("auth", "server", "uuid"):
            with self.subTest(change=change):
                if not self.status()["bindings"]:
                    self.bind()
                    self.add()
                    self.sync()
                self.remote.mode = 401 if change == "auth" else 200
                self.remote.server_id = (
                    "replacement" if change == "server" else "fixture-server"
                )
                self.remote.libraries[0]["uuid"] = (
                    "replacement" if change == "uuid" else "uuid-1"
                )
                with (
                    Store(self.database, writable=True) as store,
                    store.transaction() as db,
                ):
                    db.execute(
                        "UPDATE consumer_deliveries SET state='pending',due_at=0,attempts=0"
                    )
                self.assertFalse(drain(self.database)["complete"])
                self.assertEqual(self.status()["bindings"][0]["state"], "repair")
        self.assertEqual(self.remote.scans, [])
        self.assertNotIn("DO_NOT_LEAK", encode(self.status()))

    def test_disable_during_send_cannot_ack_or_delete_remote_library(self):
        self.bind()
        self.add()
        self.sync()

        def disable():
            with (
                Store(self.database, writable=True) as store,
                store.transaction() as db,
            ):
                db.execute("UPDATE consumer_bindings SET enabled=0,revision=revision+1")

        self.remote.on_scan = disable
        report = drain(self.database, limit=1)
        self.assertEqual(report["deliveries"][0]["state"], "stale_acknowledgement")
        self.assertEqual(self.status()["bindings"][0]["state"], "disabled")
        self.assertEqual(len(self.remote.libraries), 2)

    def test_creation_preview_apply_repeat_and_uncertain_crash(self):
        spec = {
            "name": "New library",
            "type": "movie",
            "root": "/new",
            "scanner": "Fixture Scanner",
            "agent": "fixture.agent",
            "language": "en-US",
        }
        self.assertFalse(
            creation(self.database, "default", "create", "plex", spec)["applied"]
        )
        self.assertEqual(len(self.remote.libraries), 2)
        original = Plex.create

        def lost(obj, value):
            original(obj, value)
            raise RuntimeError("lost ack")

        with patch.object(Plex, "create", lost), self.assertRaises(RuntimeError):
            creation(self.database, "default", "create", "plex", spec, apply=True)
        self.assertEqual(len(self.remote.libraries), 3)
        result = creation(self.database, "default", "create", "plex", spec, apply=True)
        self.assertTrue(result["complete"])
        self.assertTrue(
            creation(self.database, "default", "create", "plex", spec, apply=True)[
                "reused"
            ]
        )
        self.assertEqual(len(self.remote.libraries), 3)
        self.remote.libraries[-1]["uuid"] = "new-identity"
        with self.assertRaises(ConsumerError):
            creation(self.database, "default", "create", "plex", spec, apply=True)

    def test_uncertain_creation_without_candidate_is_never_reposted(self):
        spec = {
            "name": "Absent",
            "type": "movie",
            "root": "/new",
            "scanner": "Fixture Scanner",
            "agent": "fixture.agent",
            "language": "en-US",
        }
        with patch.object(
            Plex, "create", side_effect=ConsumerError("temporarily_failed")
        ) as post:
            self.assertFalse(
                creation(self.database, "default", "absent", "plex", spec, apply=True)[
                    "complete"
                ]
            )
            self.assertFalse(
                creation(self.database, "default", "absent", "plex", spec, apply=True)[
                    "complete"
                ]
            )
            self.assertEqual(post.call_count, 1)

    def test_indexing_requires_paths_and_remains_separate_from_acceptance(self):
        self.bind()
        self.add()
        self.sync()
        drain(self.database)
        self.assertFalse(
            verify_indexing(self.database, "default", "movies")["complete"]
        )
        self.remote.indexed = ["/plex/Movies/One.mkv"]
        self.assertTrue(verify_indexing(self.database, "default", "movies")["complete"])

    def test_unsupported_operation_and_malformed_creation_have_stable_errors(self):
        self.bind()
        self.add()
        self.sync()
        self.remote.mode = 405
        report = drain(self.database)
        self.assertEqual(report["deliveries"][0]["error"]["code"], "unsupported")
        self.assertEqual(report["deliveries"][0]["state"], "repair")
        self.remote.mode = 200
        spec = self.root / "invalid.json"
        spec.write_text("[]")
        with contextlib.redirect_stdout(io.StringIO()) as output:
            code = main(
                [
                    "--db",
                    str(self.database),
                    "--machine",
                    "consumer",
                    "create",
                    "bad",
                    "--connection",
                    "plex",
                    "--catalog",
                    "global",
                    "--subtree",
                    "Movies",
                    "--remote-root",
                    "/plex/Movies",
                    "--type",
                    "movie",
                    "--spec",
                    str(spec),
                    "--apply",
                ]
            )
        self.assertEqual(code, 2)
        self.assertEqual(
            json.loads(output.getvalue())["data"]["error"]["code"],
            "invalid_creation_spec",
        )
        self.assertEqual(self.remote.scans, [])

    def test_creation_rejects_occupied_binding_id_before_remote_mutation(self):
        self.bind()
        spec = self.root / "creation.json"
        spec.write_text(
            json.dumps(
                {
                    "name": "Another",
                    "type": "movie",
                    "root": "/plex/Movies",
                    "scanner": "Fixture Scanner",
                    "agent": "fixture.agent",
                    "language": "en-US",
                }
            )
        )
        with contextlib.redirect_stdout(io.StringIO()) as output:
            code = main(
                [
                    "--db",
                    str(self.database),
                    "--machine",
                    "consumer",
                    "create",
                    "movies",
                    "--connection",
                    "plex",
                    "--catalog",
                    "global",
                    "--subtree",
                    "Movies",
                    "--remote-root",
                    "/plex/Movies",
                    "--type",
                    "movie",
                    "--spec",
                    str(spec),
                    "--apply",
                ]
            )
        self.assertEqual(code, 2)
        self.assertEqual(
            json.loads(output.getvalue())["data"]["error"]["code"],
            "binding_id_already_exists",
        )
        self.assertTrue(all(method == "GET" for method, _, _ in self.remote.requests))
        self.assertEqual(len(self.remote.libraries), 2)

    def test_component_paths_unicode_and_outside_scope(self):
        b = {"subtree": "Movies/日本", "remote_root": "/srv/映画"}
        self.assertEqual(translate(b, "Movies/日本/é.mkv"), "/srv/映画/é.mkv")
        for value in (
            "../escape",
            "Movies/../x",
            "Movies//x",
            "/absolute",
            "Movies\\x",
        ):
            with self.subTest(value=value), self.assertRaises(ConsumerError):
                path(value, relative=True)
        with self.assertRaises(ConsumerError):
            translate(b, "Movies/日本語/x")

    def test_jellyfin_shared_adapter_and_legacy_refresh(self):
        put_connection(
            self.database,
            "default",
            "jellyfin",
            "jellyfin",
            self.remote.url,
            "CATABOLIC_TEST_PLEX_TOKEN",
            apply=True,
        )
        self.bind(connection_id="jellyfin", kind="movies")
        self.add()
        self.sync()
        self.assertTrue(drain(self.database)["complete"])
        self.assertEqual(self.remote.scans, ["/Items/1/Refresh"])
        with Store(self.database, writable=True) as store:
            Refresh(Application(store)).configure(
                "global", self.remote.url, "CATABOLIC_TEST_PLEX_TOKEN"
            )
        self.add("Two")
        self.sync()
        with Store(self.database, writable=True) as store:
            result = Refresh(Application(store)).run()
        self.assertTrue(result["complete"])
        self.assertIn("/Library/Refresh", self.remote.scans)
        self.assertEqual(self.remote.lock_failures, [])

    def test_machine_error_codes_and_import_separation(self):
        with contextlib.redirect_stdout(io.StringIO()) as output:
            code = main(
                [
                    "--db",
                    str(self.database),
                    "--machine",
                    "consumer",
                    "discover",
                    "missing",
                ]
            )
        result = json.loads(output.getvalue())
        self.assertEqual(code, 2)
        self.assertEqual(result["interface_version"], 1)
        self.assertEqual(result["data"]["error"]["code"], "unknown_connection")

    def test_rename_removal_and_hardlink_publication(self):
        with Store(self.database, writable=True) as store:
            Application(store).bind(
                "output", "global", str(self.output), link_mode="hardlink"
            )
        self.bind()
        self.add()
        self.sync()
        self.assertTrue(drain(self.database)["complete"])
        self.assertFalse((self.output / "Movies/One.mkv").is_symlink())
        with Store(self.database, writable=True) as store, store.transaction() as db:
            db.execute(
                "UPDATE mappings SET path='Movies/Renamed.mkv' WHERE catalog='global'"
            )
        self.sync(max_removals=1)
        self.assertTrue(drain(self.database)["complete"])
        self.assertFalse((self.output / "Movies/One.mkv").exists())
        with Store(self.database, writable=True) as store, store.transaction() as db:
            db.execute("UPDATE mappings SET active=0 WHERE catalog='global'")
        self.sync(max_removals=1)
        self.assertTrue(drain(self.database)["complete"])
        self.assertEqual(len(self.remote.scans), 3)
        self.assertTrue((self.source / "One.mkv").is_file())
        self.sync()
        self.assertEqual(drain(self.database)["processed"], 0)

    def test_rescanned_source_change_keeps_link_but_schedules_verified_scan(self):
        self.bind()
        self.add()
        self.sync()
        self.assertTrue(drain(self.database)["complete"])
        link = self.output / "Movies/One.mkv"
        inode = link.lstat().st_ino
        (self.source / "One.mkv").write_bytes(
            b"externally changed source fixture bytes"
        )
        with Store(self.database, writable=True) as store:
            Application(store).scan()
        # No further sync is needed to retain intent after the observation commit.
        self.assertEqual(self.status()["bindings"][0]["state"], "unverified")
        self.assertTrue(drain(self.database)["complete"])
        self.assertEqual(len(self.remote.scans), 2)
        self.assertEqual(link.lstat().st_ino, inode)
        with Store(self.database, writable=True) as store:
            Application(store).scan()
        self.assertEqual(drain(self.database)["processed"], 0)
        self.assertEqual(link.lstat().st_ino, inode)

    def test_binding_edit_invalidates_claim_and_never_redirects_old_work(self):
        self.bind()
        self.add()
        self.sync()
        old = claim(self.database, "default")
        with self.assertRaises(ConsumerError):
            self.bind(library_id="2", remote_root="/different")
        self.bind(library_id="2", remote_root="/different", rebind=True)
        result = deliver(self.database, "default", old)
        self.assertEqual(result["state"], "stale_acknowledgement")
        self.assertEqual(self.remote.scans, [])
        self.assertEqual(drain(self.database)["processed"], 0)

    def test_disable_enable_catches_changes_missed_while_disabled(self):
        self.bind()
        self.add()
        self.sync()
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(
                main(["--db", str(self.database), "consumer", "disable", "movies"]), 0
            )
        self.add("Two")
        self.sync()
        self.assertTrue(drain(self.database)["complete"])
        self.assertEqual(self.remote.scans, [])
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(
                main(["--db", str(self.database), "consumer", "enable", "movies"]), 0
            )
        self.assertTrue(drain(self.database)["complete"])
        self.assertEqual(len(self.remote.scans), 1)

    def test_shared_library_requires_consistent_identity_evidence(self):
        self.remote.libraries[0]["roots"] = ["/plex"]
        self.bind()
        self.bind(identifier="tv", subtree="TV", remote_root="/plex/TV")
        self.add("One", "TV")
        self.sync()
        self.remote.libraries[0]["roots"] = ["/plex", "/additional"]
        self.bind(rebind=True)
        self.assertFalse(drain(self.database)["complete"])
        self.assertEqual(self.remote.scans, [])
        self.assertEqual(
            self.status()["bindings"][1]["delivery_error"],
            "library_binding_evidence_mismatch",
        )

    def test_transaction_rollback_keeps_generation_and_profiles_isolated(self):
        from catabolic.consumers import record_change

        self.bind()
        with Store(self.database, writable=True) as store:
            with self.assertRaises(RuntimeError), store.transaction() as db:
                record_change(db, "default", "global", "Movies/One.mkv")
                raise RuntimeError("rollback")
            with store.transaction() as db:
                record_change(db, "unrelated-profile", "global", "Movies/One.mkv")
        self.assertEqual(self.status()["bindings"][0]["generation"], 0)

    def test_offline_and_timeout_are_independent_from_publication(self):
        from catabolic.domain import CatabolicError

        self.bind()
        self.add()
        result = self.sync()
        self.assertTrue(result["healthy"])
        for message in ("connection refused PRIVATE", "deadline exceeded PRIVATE"):
            with patch(
                "catabolic.consumer_adapters.request",
                side_effect=CatabolicError(message),
            ):
                self.force_due()
                report = drain(self.database)
            self.assertFalse(report["complete"])
            self.assertNotIn("PRIVATE", encode(report))
            self.assertEqual(
                report["deliveries"][0]["error"]["code"], "temporarily_failed"
            )
        self.assertTrue((self.output / "Movies/One.mkv").is_symlink())

    def test_notification_failure_does_not_repeat_accepted_scan(self):
        from catabolic.notifications import configure

        with Store(self.database, writable=True) as store:
            configure(
                Application(store),
                "human",
                "ABSENT_FIXTURE_NOTIFICATION",
                ["projection_updated", "scan_requested"],
                [],
                "info",
                apply=True,
            )
        self.bind(automatic=True)
        self.add()
        result = self.sync()
        self.assertTrue(result["healthy"])
        self.assertTrue(result["consumers"]["complete"])
        self.assertFalse(result["notifications"]["complete"])
        self.assertEqual(len(self.remote.scans), 1)
        self.assertTrue(drain(self.database)["complete"])
        self.assertEqual(len(self.remote.scans), 1)


class ConsumerMigrationTest(unittest.TestCase):
    def test_populated_schema16_preserves_legacy_targets_events_attempts(self):
        with tempfile.TemporaryDirectory() as tmp:
            database = create_legacy(Path(tmp))
            upgrade_database(database, migrations=load_migrations()[:16])
            with contextlib.closing(sqlite3.connect(database)) as db, db:
                target = {
                    "profile": "default",
                    "catalog": "global",
                    "application": "jellyfin",
                    "endpoint": "http://localhost:8096",
                    "credential_env": "JF_TOKEN",
                }
                db.execute(
                    "INSERT INTO refresh_targets VALUES ('default','global','jellyfin','http://localhost:8096','JF_TOKEN')"
                )
                db.execute(
                    "INSERT INTO refresh_dirty VALUES ('default','global',?)",
                    (encode(target),),
                )
                db.execute(
                    "INSERT INTO refresh_events(profile,catalog,target,state,attempts,error) VALUES ('default','global',?,'failed',2,'offline')",
                    (encode(target),),
                )
                before = data_snapshot(db)
            upgrade_database(database)
            with Store(database) as store:
                validate_preservation(store.db, before)
                self.assertEqual(
                    store.rows("SELECT attempts,state FROM refresh_events"),
                    [{"attempts": 2, "state": "failed"}],
                )
                self.assertEqual(store.rows("SELECT * FROM consumer_connections"), [])
                self.assertEqual(store.rows("SELECT * FROM consumer_bindings"), [])
                self.assertEqual(
                    store.rows("SELECT * FROM notification_destinations"), []
                )
