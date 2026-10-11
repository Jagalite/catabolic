#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Installed-only Plex metadata signal cleanup and unknown-effect qualification.

Run outside the checkout with -I. All catalogs, media, credentials and remote
states are disposable fixtures; every HTTP request reaches numeric loopback.
"""

import argparse
import hashlib
import importlib.metadata
import importlib.util
import json
import os
import signal
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch
from urllib.parse import parse_qs, urlsplit
from xml.etree import ElementTree as ET

if not __debug__:
    raise RuntimeError("Metadata qualification requires Python assertions")


class InstalledPlexMetadata(unittest.TestCase):
    def setUp(self):
        import catabolic_native as native

        self.native = native
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.path = self.root / "catalog.db"
        source = self.root / "source"
        source.mkdir()
        (source / "movie.mkv").write_bytes(b"disposable metadata source")
        native.Store.initialize(self.path)
        with native.Store(self.path, writable=True) as store:
            app = native.Application(store)
            app.bind("source", "media", str(source))
            app.scan()
        self.entered, self.release = threading.Event(), threading.Event()
        self.block, self.lost = None, False
        self.errors, self.requests = [], []
        self.node = ET.fromstring(
            '<Video ratingKey="1" librarySectionID="7" type="movie" title="Fixture" year="2026" guid="plex://movie/fixture"><Media><Part file="/media/movie.mkv" /></Media></Video>'
        )
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
                url = urlsplit(self.path)
                route = url.path
                fixture.requests.append((self.command, route))
                try:
                    with native.Store(fixture.path, writable=True):
                        pass
                    if self.headers["X-Plex-Token"] != "FAKE_ONLY_NO_REAL_ACCOUNT":
                        raise AssertionError("wrong credential header")
                    if "FAKE_ONLY_NO_REAL_ACCOUNT" in self.path:
                        raise AssertionError("credential leaked into URL")
                    if self.command == "PUT":
                        if route != "/library/sections/7/all":
                            raise AssertionError("wrong PUT route")
                        for key, values in parse_qs(
                            url.query, keep_blank_values=True
                        ).items():
                            if key.endswith(".value"):
                                fixture.node.set(key[:-6], values[0])
                            elif key.endswith(".locked"):
                                for field in list(fixture.node.findall("Field")):
                                    if field.get("name") == key[:-7]:
                                        fixture.node.remove(field)
                                ET.SubElement(
                                    fixture.node,
                                    "Field",
                                    name=key[:-7],
                                    locked=values[0],
                                )
                        body = b""
                    elif route == "/":
                        body = b'<MediaContainer machineIdentifier="fixture-server" version="fixture" />'
                    elif route == "/library/sections":
                        body = b'<MediaContainer><Directory key="7" uuid="fixture-library" type="movie" title="Movies" createdAt="1"><Location path="/media" /></Directory></MediaContainer>'
                    elif route in ("/library/sections/7/all", "/library/metadata/1"):
                        container = ET.Element(
                            "MediaContainer", totalSize="1", offset="0", size="1"
                        )
                        container.append(fixture.node)
                        body = ET.tostring(container)
                    else:
                        raise AssertionError("unexpected request " + route)
                    if fixture.block == (self.command, route):
                        fixture.entered.set()
                        if not fixture.release.wait(10):
                            raise AssertionError("response not released")
                    if fixture.lost and self.command == "PUT":
                        self.close_connection = True
                        return
                except Exception as exc:
                    fixture.errors.append(repr(exc))
                    body = b"fixture error"
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                try:
                    self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError):
                    pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server_thread = threading.Thread(
            target=self.server.serve_forever, daemon=True
        )
        self.server_thread.start()
        self.addCleanup(self.close_server)
        credential = patch.dict(
            os.environ, {"CATABOLIC_PLEX_FIXTURE_TOKEN": "FAKE_ONLY_NO_REAL_ACCOUNT"}
        )
        credential.start()
        self.addCleanup(credential.stop)
        native.execute_operation(
            self.path,
            "consumers",
            "connection-put",
            id="fixture",
            application="plex",
            endpoint=f"http://127.0.0.1:{self.server.server_port}",
            credential_env="CATABOLIC_PLEX_FIXTURE_TOKEN",
            apply=True,
        )
        options = dict(
            connection="fixture",
            library_id="7",
            mappings=[{"remote_root": "/media", "location": "media"}],
        )
        preview = native.execute_operation(self.path, "plex-import", "run", **options)
        imported = native.execute_operation(
            self.path,
            "plex-import",
            "run",
            **options,
            apply=True,
            expected_plan=preview["plan_id"],
        )
        self.item = imported["imported"][0]["item_id"]
        with native.Store(self.path, writable=True) as store:
            native.Application(store).put_item(
                "movie", {}, {"title": "Curated title"}, self.item
            )
        self.requests.clear()

    def close_server(self):
        self.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.server_thread.join(5)
        self.assertFalse(self.server_thread.is_alive())
        self.assertEqual(self.errors, [])

    def metadata(self, **options):
        result = self.native.execute_operation(
            self.path,
            "plex-metadata",
            "run",
            connection="fixture",
            library_id="7",
            item_ids=[self.item],
            fields=["title"],
            **options,
        )
        self.assertNotIn("FAKE_ONLY_NO_REAL_ACCOUNT", json.dumps(result))
        return result

    def events(self):
        with self.native.Store(self.path) as store:
            return [
                json.loads(r["subject"])
                for r in store.rows(
                    "SELECT subject FROM consumer_events WHERE event='metadata_publish'"
                )
            ]

    def interrupt(self, after_effect, exception):
        preview = self.metadata()
        before = self.path.read_bytes()
        self.requests.clear()
        self.block = (
            ("PUT", "/library/sections/7/all")
            if after_effect
            else ("GET", "/library/metadata/1")
        )
        signalled = threading.Event()

        def stop():
            if not self.entered.wait(10):
                self.errors.append("HTTP did not block")
                self.release.set()
                return
            os.kill(os.getpid(), signal.SIGINT)
            signalled.set()
            time.sleep(0.15)
            self.release.set()

        thread = threading.Thread(target=stop)
        thread.start()
        try:
            with self.assertRaises(exception) as raised:
                self.metadata(apply=True, expected_plan=preview["plan_id"])
        finally:
            self.release.set()
            thread.join(12)
        self.assertTrue(signalled.is_set())
        self.assertFalse(thread.is_alive())
        self.assertEqual(self.requests[-1], self.block)
        self.assertEqual(
            self.events()[0]["state"] if after_effect else self.events(),
            "started" if after_effect else [],
        )
        if not after_effect:
            self.assertEqual(self.path.read_bytes(), before)
        self.block = None
        # The same owner is reusable, the remote worker has joined, and an
        # already-applied uncertain item produces no second PUT on new review.
        count = sum(method == "PUT" for method, _ in self.requests)
        preview = self.metadata()
        result = self.metadata(apply=True, expected_plan=preview["plan_id"])
        self.assertTrue(result["complete"])
        self.assertEqual(
            result["results"][0]["state"], "unchanged" if after_effect else "verified"
        )
        self.assertEqual(
            sum(method == "PUT" for method, _ in self.requests),
            count + (0 if after_effect else 1),
        )
        with self.native.Store(self.path, writable=True):
            pass
        return raised.exception

    def test_preview_interrupt_has_no_effect_and_retry_verifies(self):
        self.interrupt(False, KeyboardInterrupt)

    def test_post_effect_interrupt_retains_started_without_replay(self):
        self.interrupt(True, KeyboardInterrupt)

    def test_post_effect_custom_exception_instance_is_preserved(self):
        class FixtureSignal(Exception):
            pass

        expected = FixtureSignal("exact metadata signal instance")

        def handler(_signal, _frame):
            raise expected

        previous = signal.signal(signal.SIGINT, handler)
        try:
            self.assertIs(self.interrupt(True, FixtureSignal), expected)
        finally:
            signal.signal(signal.SIGINT, previous)

    def test_lost_response_is_uncertain_and_new_review_is_unchanged(self):
        preview = self.metadata()
        self.lost = True
        result = self.metadata(apply=True, expected_plan=preview["plan_id"])
        self.assertFalse(result["complete"])
        self.assertEqual(result["results"][0]["state"], "uncertain")
        self.assertFalse(result["results"][0]["error"]["safe_to_retry"])
        self.assertEqual(self.events()[0]["state"], "uncertain")
        self.assertEqual(sum(method == "PUT" for method, _ in self.requests), 1)
        self.lost = False
        preview = self.metadata()
        self.assertEqual(
            self.metadata(apply=True, expected_plan=preview["plan_id"])["results"][0][
                "state"
            ],
            "unchanged",
        )
        self.assertEqual(sum(method == "PUT" for method, _ in self.requests), 1)


def wheel_verifier():
    path = Path(__file__).resolve().with_name("qualify_native_wheel.py")
    spec = importlib.util.spec_from_file_location("_metadata_wheel_verifier", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.verify_installed_wheel


def result_verifier():
    path = Path(__file__).resolve().with_name("native_qualification_results.py")
    spec = importlib.util.spec_from_file_location("_required_results", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.run_required_suite


EXPECTED_TESTS = (
    "test_preview_interrupt_has_no_effect_and_retry_verifies",
    "test_post_effect_interrupt_retains_started_without_replay",
    "test_post_effect_custom_exception_instance_is_preserved",
    "test_lost_response_is_uncertain_and_new_review_is_unchanged",
)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wheel", type=Path)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    if args.report:
        args.report.unlink(missing_ok=True)
    import catabolic_native as native

    package = Path(native.__file__).resolve()
    dist = importlib.metadata.distribution("catabolic-native")
    assert package.is_relative_to(Path(dist.locate_file("")).resolve()), (
        "not an installed wheel"
    )
    verification = wheel_verifier()(args.wheel) if args.wheel else None
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(InstalledPlexMetadata)
    result = result_verifier()(
        suite, {InstalledPlexMetadata: EXPECTED_TESTS}, expected_count=4
    )
    if args.wheel:
        assert wheel_verifier()(args.wheel) == verification, "installed wheel changed"
    report = {
        "format": 1,
        "scope": "installed PyO3 plex-metadata/run cancellation and unknown-effect qualification",
        "tests": result.testsRun,
        "successful_scenarios": result.successful_scenarios,
        "python": sys.version,
        "installed_package": str(package),
        "wheel_sha256": hashlib.sha256(args.wheel.read_bytes()).hexdigest()
        if args.wheel
        else None,
        "installed_wheel_verification": verification,
        "qualification_script_sha256": hashlib.sha256(
            Path(__file__).read_bytes()
        ).hexdigest(),
        "result_verifier_sha256": hashlib.sha256(
            Path(__file__).with_name("native_qualification_results.py").read_bytes()
        ).hexdigest(),
        "files": {
            str(p.relative_to(package.parent)): hashlib.sha256(
                p.read_bytes()
            ).hexdigest()
            for p in sorted(package.parent.rglob("*"))
            if p.is_file() and "__pycache__" not in p.parts
        },
        "qualified": [
            "No remote write before interrupted preview returns",
            "Joined post-effect cancellation preserves started event and exact Python exception instance",
            "Lost PUT response records uncertain and is never replayed",
            "New review after observed effect is unchanged, with no duplicate PUT",
            "Store writer is available during all HTTP requests and after cleanup",
        ],
        "gaps": [
            "No exhaustive signal scheduling claim",
            "Dev-wheel evidence is not final paired-helper/platform/release qualification",
        ],
    }
    if args.report:
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(
        json.dumps(
            {
                "tests": result.testsRun,
                "report": str(args.report) if args.report else None,
            }
        )
    )


if __name__ == "__main__":
    main()
