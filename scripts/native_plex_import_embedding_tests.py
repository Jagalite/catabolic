#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Installed-only PyO3 Plex import cancellation, independent of workflow proof.

Run outside the checkout with -I after installing the candidate wheel. Every
HTTP request goes to one ephemeral loopback server with a fake credential.
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
from urllib.parse import urlsplit

if not __debug__:
    raise RuntimeError("Plex import embedding qualification requires Python assertions")


class InstalledPlexImportCancellation(unittest.TestCase):
    def setUp(self):
        import catabolic_native as native

        self.native = native
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.path = self.root / "catalog.db"
        source = self.root / "source"
        source.mkdir()
        (source / "movie.mkv").write_bytes(b"disposable Plex import fixture")
        native.Store.initialize(self.path)
        with native.Store(self.path, writable=True) as store:
            app = native.Application(store)
            app.bind("source", "media", str(source))
            app.scan()
        self.entered = threading.Event()
        self.release = threading.Event()
        self.block = False
        self.errors = []
        self.requests = []
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_):
                pass

            def do_GET(self):
                route = urlsplit(self.path).path
                fixture.requests.append(route)
                # The native import must release its writer during remote reads.
                try:
                    with native.Store(fixture.path, writable=True):
                        pass
                except Exception as exc:
                    fixture.errors.append(f"writer unavailable: {exc}")
                if route == "/":
                    body = b'<MediaContainer machineIdentifier="fixture-server" version="fixture" />'
                elif route == "/library/sections":
                    body = b'<MediaContainer><Directory key="7" uuid="fixture-library" type="movie" title="Movies" createdAt="1"><Location path="/media" /></Directory></MediaContainer>'
                elif route == "/library/sections/7/all":
                    body = b'<MediaContainer totalSize="1" offset="0" size="1"><Video ratingKey="1" type="movie" title="Fixture" year="2026" guid="plex://movie/fixture"><Guid id="imdb://tt1234567" /><Media><Part file="/media/movie.mkv" /></Media></Video></MediaContainer>'
                    if fixture.block:
                        fixture.entered.set()
                        if not fixture.release.wait(10):
                            fixture.errors.append(
                                "blocked fixture response was not released"
                            )
                else:
                    fixture.errors.append(f"unexpected request: {route}")
                    body = b"<MediaContainer />"
                self.send_response(200)
                self.send_header("Content-Type", "application/xml")
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

    def close_server(self):
        self.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.server_thread.join(5)
        self.assertFalse(self.server_thread.is_alive())
        self.assertEqual(self.errors, [])

    def run_import(self, **options):
        result = self.native.execute_operation(
            self.path,
            "plex-import",
            "run",
            connection="fixture",
            library_id="7",
            mappings=[{"remote_root": "/media", "location": "media"}],
            **options,
        )
        self.assertNotIn("FAKE_ONLY_NO_REAL_ACCOUNT", json.dumps(result))
        return result

    def interrupt_blocked_apply(self, exception):
        preview = self.run_import()
        self.assertEqual(len(preview["candidates"]), 1)
        before = self.path.read_bytes()
        self.block = True
        signalled = threading.Event()

        def interrupt():
            if not self.entered.wait(10):
                self.errors.append("import did not enter the blocked HTTP read")
                self.release.set()
                return
            os.kill(os.getpid(), signal.SIGINT)
            signalled.set()
            # Allow the 25ms Python poll to capture the signal before response.
            time.sleep(0.15)
            self.release.set()

        thread = threading.Thread(target=interrupt)
        thread.start()
        try:
            with self.assertRaises(exception) as raised:
                self.run_import(apply=True, expected_plan=preview["plan_id"])
        finally:
            self.release.set()
            thread.join(12)
        self.assertFalse(thread.is_alive())
        self.assertTrue(signalled.is_set())
        self.assertEqual(self.path.read_bytes(), before)
        self.block = False
        with self.native.Store(self.path, writable=True) as store:
            self.assertEqual(store.rows("SELECT * FROM items"), [])
            self.assertEqual(store.rows("SELECT * FROM proposals"), [])
        # A successful retry establishes cleanup, joined HTTP work and no sticky
        # cancellation; another reviewed apply must not create duplicates.
        preview = self.run_import()
        applied = self.run_import(apply=True, expected_plan=preview["plan_id"])
        self.assertEqual(len(applied["imported"]), 1)
        preview = self.run_import()
        self.assertEqual(
            self.run_import(apply=True, expected_plan=preview["plan_id"])["imported"],
            [],
        )
        with self.native.Store(self.path) as store:
            self.assertEqual(len(store.rows("SELECT * FROM items")), 1)
            self.assertEqual(len(store.rows("SELECT * FROM proposals")), 1)
        return raised.exception

    def test_keyboard_interrupt_rolls_back_and_retry_is_idempotent(self):
        self.interrupt_blocked_apply(KeyboardInterrupt)

    def test_custom_python_signal_exception_is_preserved(self):
        class FixtureSignal(Exception):
            pass

        expected = FixtureSignal("exact Python signal exception")

        def handler(_signal, _frame):
            raise expected

        previous = signal.signal(signal.SIGINT, handler)
        try:
            self.assertIs(self.interrupt_blocked_apply(FixtureSignal), expected)
        finally:
            signal.signal(signal.SIGINT, previous)


def wheel_verifier():
    path = Path(__file__).resolve().with_name("qualify_native_wheel.py")
    spec = importlib.util.spec_from_file_location("_plex_wheel_verifier", path)
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
    "test_keyboard_interrupt_rolls_back_and_retry_is_idempotent",
    "test_custom_python_signal_exception_is_preserved",
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
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(
        InstalledPlexImportCancellation
    )
    result = result_verifier()(
        suite, {InstalledPlexImportCancellation: EXPECTED_TESTS}, expected_count=2
    )
    if args.wheel:
        assert wheel_verifier()(args.wheel) == verification, "installed wheel changed"
    report = {
        "format": 1,
        "scope": "installed PyO3 plex-import/run cancellation only",
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
        "wheel_verifier_sha256": hashlib.sha256(
            Path(__file__).resolve().with_name("qualify_native_wheel.py").read_bytes()
        ).hexdigest(),
        "result_verifier_sha256": hashlib.sha256(
            Path(__file__).with_name("native_qualification_results.py").read_bytes()
        ).hexdigest(),
        "files": {
            str(path.relative_to(package.parent)): hashlib.sha256(
                path.read_bytes()
            ).hexdigest()
            for path in sorted(package.parent.rglob("*"))
            if path.is_file() and "__pycache__" not in path.parts
        },
        "qualified": [
            "SIGINT during a managed HTTP read raises KeyboardInterrupt after cleanup",
            "A custom Python signal exception retains its exact exception class",
            "Interrupted apply leaves catalog bytes unchanged and no partial proposal",
            "Writer access is available during HTTP and after interrupted cleanup",
            "Retry applies once; repeating the reviewed import is idempotent",
        ],
        "gaps": [
            "No claim of exhaustive signal timing or all page/batch cancellation schedules",
            "Dev-wheel evidence without --wheel does not bind a final helper-paired wheel",
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
