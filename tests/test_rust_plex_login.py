# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Frozen Plex login scenarios and native subprocess-only loopback boundaries.

No request can reach Plex: native commands always select the numeric loopback
fixture and reference calls are intercepted. All tokens and files are disposable.
The opt-in companion CLI is exercised here; legacy --machine routing remains a
separate migration surface.
"""

import fcntl
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch
from urllib.parse import parse_qs, urlsplit

from catabolic import plex_login
from catabolic.app import Application
from catabolic.consumer_adapters import ConsumerError
from catabolic.store import Store
from tests.test_consumers import ProtocolServer
from tests.test_rust_migration import BINARY


class NativePlexLoginTest(unittest.TestCase):
    def setUp(self):
        self.assertTrue(BINARY.is_file(), "build catabolic-cli before qualification")
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.folder = self.root / "credentials"
        self.database = self.root / "catalog.sqlite3"
        self.authorized = False
        self.secret = "PRIVATE_SYNTHETIC_PLEX_TOKEN"
        self.calls = []
        self.fixture_failures = []
        self.responses = {}
        self.statuses = {}
        self.user_callback = None
        self.block_route = None
        self.entered = threading.Event()
        self.release = threading.Event()
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            # Exercise the frozen HTTP/1.0 fixture and connection-close behavior.
            def log_message(self, *_):
                pass

            def do_GET(self):
                self.respond()

            def do_POST(self):
                self.respond()

            def respond(self):
                route = urlsplit(self.path)
                try:
                    if self.command == "POST":
                        fixture.assertEqual(self.headers.get("Content-Length"), "0")
                        fixture.assertIsNone(self.headers.get("Transfer-Encoding"))
                    body = fixture.protocol(route, self.command, self.headers)
                    if fixture.block_route == route.path:
                        fixture.entered.set()
                        fixture.release.wait(25)
                    if route.path == "/api/v2/user" and fixture.user_callback:
                        callback, fixture.user_callback = fixture.user_callback, None
                        callback()
                    status = fixture.statuses.get(route.path, 200)
                except Exception as exc:
                    fixture.fixture_failures.append(repr(exc))
                    status, body = 500, b"fixture failure"
                self.send_response(status)
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Retry-After", "120")
                if status == 302:
                    self.send_header("Location", fixture.endpoint + "/must-not-follow")
                self.end_headers()
                try:
                    self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError):
                    pass

        self.remote = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.endpoint = f"http://127.0.0.1:{self.remote.server_port}"
        self.thread = threading.Thread(target=self.remote.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.close)

    def close(self):
        self.release.set()
        self.remote.shutdown()
        self.remote.server_close()
        self.thread.join()
        self.assertEqual(self.fixture_failures, [])

    def protocol(self, route, method, headers):
        self.assertEqual(headers["X-Plex-Product"], "Catabolic")
        self.assertEqual(headers["Accept"], "application/json")
        self.assertNotIn(self.secret, route.geturl())
        self.calls.append((route.path, method, parse_qs(route.query), headers))
        if route.path == "/api/v2/pins":
            self.assertEqual(method, "POST")
            self.assertEqual(parse_qs(route.query), {"strong": ["true"]})
            default = {"id": 123, "code": "strong-code", "expiresIn": 600}
        elif route.path == "/api/v2/pins/123":
            self.assertEqual(method, "GET")
            self.assertEqual(parse_qs(route.query), {"code": ["strong-code"]})
            default = {
                "id": 123,
                "code": "strong-code",
                "authToken": self.secret if self.authorized else None,
            }
        else:
            self.assertEqual(route.path, "/api/v2/user")
            self.assertEqual(method, "GET")
            self.assertEqual(headers["X-Plex-Token"], self.secret)
            default = {"id": 42}
        response = self.responses.get(route.path, default)
        return (
            response if isinstance(response, bytes) else json.dumps(response).encode()
        )

    def reference_request(self, url, **kwargs):
        route = urlsplit(url)
        self.assertEqual((route.scheme, route.netloc), ("https", "plex.tv"))
        self.assertEqual(
            (kwargs["timeout"], kwargs["maximum"], kwargs["structured_errors"]),
            (15, 65536, True),
        )
        status = self.statuses.get(route.path, 200)
        if status != 200:
            code = {
                401: "unauthorized",
                429: "rate_limited",
                503: "temporarily_failed",
            }.get(status, "temporarily_failed")
            raise ConsumerError(code, retry_after=120)
        return self.protocol(route, kwargs["method"], kwargs["headers"])

    def command(self, operation, **request):
        return [
            str(BINARY),
            "--db",
            str(self.database),
            "operation",
            "plex-login",
            operation,
            "--options",
            json.dumps(
                {
                    "credential_dir": str(self.folder),
                    "fixture_endpoint": self.endpoint,
                    **request,
                }
            ),
        ]

    def environment(self):
        return {**os.environ, "CATABOLIC_PLEX_LOGIN_FIXTURE": "1"}

    def native(self, operation, *, error=None, **request):
        run = subprocess.run(
            self.command(operation, **request),
            env=self.environment(),
            capture_output=True,
            text=True,
            timeout=25,
        )
        self.assertNotIn(self.secret, run.stdout + run.stderr)
        self.assertEqual(run.returncode, 2 if error else 0, run.stdout + run.stderr)
        if error:
            self.assertIn("consumer " + error + ";", run.stderr)
            return run.stderr
        return json.loads(run.stdout)

    def completed(self):
        started = self.native("start")
        self.authorized = True
        return self.native("complete", identifier=started["login_id"])

    def state_file(self, started):
        return self.folder / (started["login_id"] + ".json")

    def rewrite(self, filename, **changes):
        value = json.loads(filename.read_text())
        value.update(changes)
        filename.write_text(json.dumps(value))

    def test_pending_authorized_repeat_and_stable_client(self):
        result = self.native("start")
        with patch("catabolic.plex_login.request", side_effect=self.reference_request):
            reference = plex_login.start(self.folder)
        # Reusing the same stable identity makes URL and all non-random fields exact.
        self.assertEqual(
            {k: v for k, v in result.items() if k != "login_id"},
            {k: v for k, v in reference.items() if k != "login_id"},
        )
        pending = self.native("complete", identifier=result["login_id"])
        with patch("catabolic.plex_login.request", side_effect=self.reference_request):
            self.assertEqual(
                pending, plex_login.complete(self.folder, result["login_id"])
            )
        self.authorized = True
        done = self.native("complete", identifier=result["login_id"])
        count = len(self.calls)
        self.assertEqual(done, self.native("complete", identifier=result["login_id"]))
        with patch("catabolic.plex_login.request", side_effect=self.reference_request):
            self.assertEqual(done, plex_login.complete(self.folder, result["login_id"]))
        self.assertEqual(count, len(self.calls))
        value = plex_login.read_private(done["credential_file"])
        self.assertEqual(set(value), {"client_id", "token"})
        self.assertEqual(value["token"], self.secret)
        self.native("start")
        self.assertEqual(
            {call[3]["X-Plex-Client-Identifier"] for call in self.calls},
            {value["client_id"]},
        )
        self.assertEqual(self.folder.stat().st_mode & 0o777, 0o700)
        self.assertEqual(Path(done["credential_file"]).stat().st_mode & 0o777, 0o600)
        self.assertFalse(self.database.exists())

    def test_expiry_and_invalid_id_do_not_poll(self):
        result = self.native("start")
        self.rewrite(self.state_file(result), expires_at=1)
        expired = self.native("complete", identifier=result["login_id"])
        self.assertEqual(expired, plex_login.complete(self.folder, result["login_id"]))
        for identifier in ("../client", result["login_id"].upper(), None, 1):
            self.native("complete", identifier=identifier, error="invalid_login_id")
        self.assertEqual(len(self.calls), 1)

    def test_private_storage_rejects_permissions_and_symlinks(self):
        done = self.completed()
        filename = Path(done["credential_file"])
        identifier = filename.stem
        filename.chmod(0o644)
        self.native(
            "complete", identifier=identifier, error="unsafe_credential_storage"
        )
        filename.chmod(0o600)
        backup = filename.with_suffix(".saved")
        filename.rename(backup)
        filename.symlink_to(backup)
        self.native(
            "complete", identifier=identifier, error="credential_storage_unavailable"
        )
        filename.unlink()
        backup.rename(filename)
        self.folder.chmod(0o755)
        self.native("start", error="unsafe_credential_storage")
        self.folder.chmod(0o700)

    def test_malformed_and_failed_auth_are_redacted_and_retry_preserves_pin(self):
        result = self.native("start")
        self.authorized = True
        self.responses["/api/v2/pins/123"] = b"not json PRIVATE_SYNTHETIC_PLEX_TOKEN"
        self.native(
            "complete", identifier=result["login_id"], error="plex_login_failed"
        )
        self.assertIn("code", plex_login.read_private(self.state_file(result)))
        self.responses.clear()
        self.assertTrue(
            self.native("complete", identifier=result["login_id"])["complete"]
        )

    def test_rate_limit_and_unauthorized_do_not_save_token(self):
        result = self.native("start")
        for status, code in (
            (429, "rate_limited"),
            (401, "unauthorized"),
            (503, "temporarily_failed"),
        ):
            self.statuses["/api/v2/pins/123"] = status
            self.native("complete", identifier=result["login_id"], error=code)
            with patch(
                "catabolic.plex_login.request", side_effect=self.reference_request
            ):
                with self.assertRaises(ConsumerError) as raised:
                    plex_login.complete(self.folder, result["login_id"])
            self.assertEqual(raised.exception.code, code)
        self.assertNotIn("token", plex_login.read_private(self.state_file(result)))

    def test_failed_write_leaves_resumable_pin_and_no_temporary_token(self):
        result = self.native("start")
        self.authorized = True
        filename = self.state_file(result)
        backup = filename.with_suffix(".saved")

        def fail_rename():
            filename.rename(backup)
            filename.mkdir(mode=0o700)

        self.user_callback = fail_rename
        self.native(
            "complete",
            identifier=result["login_id"],
            error="credential_storage_unavailable",
        )
        self.assertEqual(list(self.folder.glob(".plex-*")), [])
        self.assertIn("code", plex_login.read_private(backup))
        filename.rmdir()
        backup.rename(filename)
        self.assertTrue(
            self.native("complete", identifier=result["login_id"])["complete"]
        )

    def test_mismatched_pin_and_failed_user_validation_never_persist_token(self):
        result = self.native("start")
        self.responses["/api/v2/pins/123"] = {
            "id": 999,
            "code": "wrong",
            "authToken": self.secret,
        }
        self.native("complete", identifier=result["login_id"], error="invalid_response")
        self.responses.clear()
        self.authorized = True
        self.statuses["/api/v2/user"] = 401
        self.native("complete", identifier=result["login_id"], error="unauthorized")
        self.assertNotIn("token", plex_login.read_private(self.state_file(result)))

    def test_machine_flow_without_database_never_outputs_token(self):
        # Frozen scenario, using the opt-in companion JSON output. Legacy
        # --machine envelope/exit behavior is deliberately not claimed here.
        result = self.native("start")
        self.authorized = True
        self.assertTrue(
            self.native("complete", identifier=result["login_id"])["complete"]
        )
        self.assertFalse(self.database.exists())
        self.assertEqual({p.name for p in self.root.iterdir()}, {"credentials"})

    def consumer(self, operation, request, *, error=None, surface="consumers"):
        run = subprocess.run(
            [
                str(BINARY),
                "--db",
                str(self.database),
                "operation",
                surface,
                operation,
                "--options",
                json.dumps(request),
            ],
            capture_output=True,
            text=True,
            timeout=25,
        )
        self.assertNotIn(self.secret, run.stdout + run.stderr)
        self.assertEqual(run.returncode, 2 if error else 0, run.stdout + run.stderr)
        if error:
            self.assertIn(error, run.stderr)
            return
        return json.loads(run.stdout)

    def test_login_credential_cannot_be_sent_to_jellyfin(self):
        done = self.completed()
        Store.initialize(self.database)
        self.consumer(
            "connection-put",
            {
                "id": "wrong",
                "application": "jellyfin",
                "endpoint": self.endpoint,
                "credential_env": "file:" + done["credential_file"],
                "apply": True,
            },
            error="plex_credential_requires_plex_connection",
        )
        self.assertEqual(len(self.calls), 3)

    def test_file_credential_connects_and_scans_without_environment_token(self):
        done = self.completed()
        Store.initialize(self.database)
        output = self.root / "output"
        output.mkdir()
        with Store(self.database, writable=True) as store:
            Application(store).bind("output", "global", str(output))
        remote = ProtocolServer(self.database)
        self.addCleanup(remote.close)
        reference = "file:" + done["credential_file"]
        self.consumer(
            "connection-put",
            {
                "id": "plex",
                "application": "plex",
                "endpoint": remote.url,
                "credential_env": reference,
                "apply": True,
            },
        )
        self.assertFalse(remote.scans)
        self.consumer("apply", {}, surface="outputs")
        self.consumer(
            "bind",
            {
                "id": "movies",
                "connection": "plex",
                "catalog": "global",
                "subtree": "Movies",
                "remote_root": "/plex/Movies",
                "library_id": "1",
                "type": "movie",
                "apply": True,
                "initial_scan": True,
                "automatic": True,
            },
        )
        self.assertEqual(remote.scans, ["/library/sections/1/refresh"])
        self.assertFalse(remote.lock_failures)
        with Store(self.database) as store:
            rows = store.rows("SELECT * FROM consumer_connections")
        self.assertEqual(rows[0]["credential_env"], reference)
        self.assertNotIn(self.secret, json.dumps(rows))
        self.assertNotIn(self.secret.encode(), self.database.read_bytes())

    def test_response_and_private_file_bounds(self):
        for body, error in (
            (b"[]", "plex_login_failed"),
            (b"x" * 65537, "temporarily_failed"),
            ({"id": True, "code": "ok", "expiresIn": 600}, "invalid_response"),
            ({"id": 1, "code": "ok", "expiresIn": 3601}, "invalid_response"),
            ({"id": 1, "code": "x" * 513, "expiresIn": 600}, "invalid_configuration"),
        ):
            self.responses["/api/v2/pins"] = body
            self.native("start", error=error)
        self.responses.clear()
        started = self.native("start")
        filename = self.state_file(started)
        filename.write_bytes(b"x" * 16385)
        self.native(
            "complete",
            identifier=started["login_id"],
            error="credential_storage_unavailable",
        )
        filename.write_bytes(b'{"code":"\xff"}')
        self.native(
            "complete",
            identifier=started["login_id"],
            error="credential_storage_unavailable",
        )

    def test_interruption_preserves_pin_and_allows_resume(self):
        started = self.native("start")
        self.authorized = True
        self.block_route = "/api/v2/user"
        process = subprocess.Popen(
            self.command("complete", identifier=started["login_id"]),
            env=self.environment(),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            self.assertTrue(self.entered.wait(5))
            process.send_signal(signal.SIGINT)
            # The owner joins the current bounded request; it must not detach
            # a writer. Prompt network cancellation remains a documented gap.
            time.sleep(0.1)
            self.assertIsNone(process.poll())
            self.release.set()
            stdout, stderr = process.communicate(timeout=5)
            self.assertEqual(process.returncode, 130)
            self.assertNotIn(self.secret, stdout + stderr)
            self.assertNotIn("token", plex_login.read_private(self.state_file(started)))
            self.assertEqual(list(self.folder.glob(".plex-*")), [])
        finally:
            self.release.set()
            if process.poll() is None:
                process.kill()
                process.wait()
        self.block_route = None
        self.assertTrue(
            self.native("complete", identifier=started["login_id"])["complete"]
        )

    def test_stable_identity_parallel_and_network_lock_release(self):
        self.block_route = "/api/v2/pins"
        first = subprocess.Popen(
            self.command("start"),
            env=self.environment(),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            self.assertTrue(self.entered.wait(5))
            self.block_route = None
            second = self.native("start")
            self.release.set()
            stdout, stderr = first.communicate(timeout=5)
            self.assertEqual(first.returncode, 0, stderr)
            self.assertNotEqual(json.loads(stdout)["login_id"], second["login_id"])
            self.assertEqual(
                len({c[3]["X-Plex-Client-Identifier"] for c in self.calls}), 1
            )
        finally:
            self.release.set()
            if first.poll() is None:
                first.kill()
                first.wait()

    def test_redirect_and_fixture_gate_refused_without_fallback(self):
        self.statuses["/api/v2/pins"] = 302
        self.native("start", error="temporarily_failed")
        self.assertEqual(len(self.calls), 1)
        for endpoint in (
            "https://plex.tv",
            "http://localhost:80",
            "http://192.0.2.1:80",
            "http://127.0.0.1:80/path",
            "http://127.0.0.1:80@plex.tv",
        ):
            self.native(
                "start", fixture_endpoint=endpoint, error="invalid_configuration"
            )
        env = self.environment()
        env.pop("CATABOLIC_PLEX_LOGIN_FIXTURE")
        run = subprocess.run(
            self.command("start"), env=env, capture_output=True, text=True, timeout=5
        )
        self.assertEqual(run.returncode, 2)
        self.assertIn("invalid_configuration", run.stderr)
        self.assertEqual(len(self.calls), 1)

    def test_hardlink_and_nonregular_state_are_refused(self):
        started = self.native("start")
        filename = self.state_file(started)
        link = self.folder / "alias.json"
        os.link(filename, link)
        self.native(
            "complete",
            identifier=started["login_id"],
            error="unsafe_credential_storage",
        )
        link.unlink()
        filename.unlink()
        os.mkfifo(filename, 0o600)
        self.native(
            "complete",
            identifier=started["login_id"],
            error="unsafe_credential_storage",
        )

    def test_whole_request_timeout_preserves_pin(self):
        started = self.native("start")
        self.block_route = "/api/v2/pins/123"
        start = time.monotonic()
        self.native(
            "complete", identifier=started["login_id"], error="plex_login_failed"
        )
        self.assertLess(time.monotonic() - start, 20)
        self.assertNotIn("token", plex_login.read_private(self.state_file(started)))
        self.release.set()

    def test_protocol_numeric_equality_and_json_byte_encodings(self):
        started = self.native("start")
        responses = [
            {"id": value, "code": "strong-code"}
            for value in (123, 123.0, True, None, "123", 124)
        ]
        for encoding in (
            "utf-8-sig",
            "utf-16",
            "utf-32",
            "utf-16-be",
            "utf-16-le",
            "utf-32-be",
            "utf-32-le",
        ):
            responses.append(
                json.dumps({"id": 123, "code": "strong-code"}).encode(encoding)
            )
        for response in responses:
            with self.subTest(response_kind=type(response).__name__):
                self.responses["/api/v2/pins/123"] = response
                with patch(
                    "catabolic.plex_login.request", side_effect=self.reference_request
                ):
                    try:
                        expected = plex_login.complete(self.folder, started["login_id"])
                    except ConsumerError as exc:
                        self.native(
                            "complete", identifier=started["login_id"], error=exc.code
                        )
                    else:
                        self.assertEqual(
                            self.native("complete", identifier=started["login_id"]),
                            expected,
                        )

    @unittest.skipUnless(
        sys.platform == "linux" and shutil.which("cc"),
        "Linux test-only fsync interposition",
    )
    def test_publication_interrupt_cleanup_cli_and_python(self):
        source = Path(__file__).parent / "fixtures/plex_login_interrupt_fsync.c"
        library = self.root / "interrupt_fsync.so"
        subprocess.run(
            ["cc", "-shared", "-fPIC", str(source), "-ldl", "-o", str(library)],
            check=True,
            capture_output=True,
        )
        extension = Path(
            os.environ.get(
                "CATABOLIC_NATIVE_EXTENSION", BINARY.parent / "lib_native.so"
            )
        )
        self.assertTrue(
            extension.is_file(),
            "build catabolic-python before login cancellation qualification",
        )
        program = """import importlib.util,json,sys,signal
interrupted = KeyboardInterrupt('synthetic interruption')
def stop(signum, frame):
    raise interrupted
signal.signal(signal.SIGINT, stop)
spec=importlib.util.spec_from_file_location('_native',sys.argv[1])
module=importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
try:
    module.execute_operation(sys.argv[2],'plex-login','complete',sys.argv[3],'default')
except BaseException as exc:
    print(json.dumps({'type':type(exc).__name__, 'same_instance':exc is interrupted}))
else:
    raise AssertionError('expected KeyboardInterrupt')
"""
        for python in (False, True):
            for eintr in (False, True):
                for committed in (False, True):
                    with self.subTest(python=python, eintr=eintr, committed=committed):
                        started = self.native("start")
                        self.authorized = True
                        env = self.environment()
                        env["LD_PRELOAD"] = str(library)
                        env["CATABOLIC_TEST_INTERRUPT_SYNC"] = "2" if committed else "1"
                        if eintr:
                            env["CATABOLIC_TEST_EINTR"] = "1"
                        command = self.command(
                            "complete", identifier=started["login_id"]
                        )
                        if python:
                            request = json.dumps(
                                {
                                    "credential_dir": str(self.folder),
                                    "fixture_endpoint": self.endpoint,
                                    "identifier": started["login_id"],
                                }
                            )
                            command = [
                                sys.executable,
                                "-c",
                                program,
                                str(extension),
                                str(self.database),
                                request,
                            ]
                        process = subprocess.run(
                            command, env=env, capture_output=True, text=True, timeout=5
                        )
                        self.assertNotIn(self.secret, process.stdout + process.stderr)
                        self.assertEqual(
                            process.returncode,
                            0 if python else 130,
                            process.stdout + process.stderr,
                        )
                        if python:
                            self.assertEqual(
                                json.loads(process.stdout),
                                {"type": "KeyboardInterrupt", "same_instance": True},
                            )
                        self.assertEqual(list(self.folder.glob(".plex-*")), [])
                        state = plex_login.read_private(self.state_file(started))
                        self.assertEqual("token" in state, committed)
                        self.assertEqual("code" in state, not committed)
                        before = len(self.calls)
                        result = self.native("complete", identifier=started["login_id"])
                        self.assertTrue(result["complete"])
                        self.assertEqual(
                            len(self.calls) - before, 0 if committed else 2
                        )
                        before = len(self.calls)
                        self.assertEqual(
                            result,
                            self.native("complete", identifier=started["login_id"]),
                        )
                        self.assertEqual(len(self.calls), before)
                        self.assertFalse(self.database.exists())

    def test_identity_lock_cancellation_is_bounded_without_network(self):
        self.folder.mkdir(mode=0o700)
        fd = os.open(self.folder, os.O_RDONLY)
        process = None
        try:
            fcntl.flock(fd, fcntl.LOCK_EX)
            process = subprocess.Popen(
                self.command("start"),
                env=self.environment(),
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            time.sleep(0.15)
            self.assertIsNone(process.poll())
            process.send_signal(signal.SIGINT)
            stdout, stderr = process.communicate(timeout=5)
            self.assertEqual(process.returncode, 130, stdout + stderr)
            self.assertEqual(self.calls, [])
            self.assertEqual(list(self.folder.iterdir()), [])
        finally:
            os.close(fd)
            if process is not None and process.poll() is None:
                process.kill()
                process.wait()
        self.assertFalse(self.native("start")["complete"])

    def test_cancelled_network_joins_within_request_bound_without_publication(self):
        started = self.native("start")
        self.authorized = True
        self.block_route = "/api/v2/pins/123"
        process = subprocess.Popen(
            self.command("complete", identifier=started["login_id"]),
            env=self.environment(),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            self.assertTrue(self.entered.wait(5))
            before = len(self.calls)
            beginning = time.monotonic()
            process.send_signal(signal.SIGINT)
            time.sleep(0.1)
            self.assertIsNone(
                process.poll(), "joined network ownership must remain explicit"
            )
            # Cancellation must not leave the private directory locked while
            # the in-flight HTTP request reaches its existing deadline.
            fd = os.open(self.folder, os.O_RDONLY)
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            finally:
                os.close(fd)
            stdout, stderr = process.communicate(timeout=20)
            elapsed = time.monotonic() - beginning
            self.assertGreater(
                elapsed, 10, "fixture did not exercise the joined request deadline"
            )
            self.assertLess(elapsed, 20)
            self.assertEqual(process.returncode, 130, stdout + stderr)
            self.assertNotIn(self.secret, stdout + stderr)
            self.assertEqual(len(self.calls), before)
            self.assertEqual(list(self.folder.glob(".plex-*")), [])
            self.assertIn("code", plex_login.read_private(self.state_file(started)))
            self.assertFalse(self.database.exists())
        finally:
            self.release.set()
            if process.poll() is None:
                process.kill()
                process.wait()
        self.block_route = None
        self.assertTrue(
            self.native("complete", identifier=started["login_id"])["complete"]
        )
