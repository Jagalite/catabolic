# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Preserved TMDB intake against isolated frozen Python; loopback/fake tokens only."""

import fcntl
import json
import re
import select
import signal
import sqlite3
import subprocess
import threading
import time
import unittest
import uuid
from contextlib import closing
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from tests import test_rust_legacy_cli as legacy
from tests.test_rust_legacy_cli import BINARY
from tests.test_rust_legacy_parser import PYTHON, REFERENCE

TOKEN = "FAKE_IDENTIFY_FIXTURE_ONLY"
ENDPOINT = "https://api.themoviedb.org/3/search/movie"
ORACLE = r"""
import os,pathlib,sys
sys.path.insert(0,sys.argv.pop(1))
from catabolic import network_adapters as network
from catabolic.cli import main
assert pathlib.Path(network.__file__).resolve() == pathlib.Path(sys.path[0]) / 'catabolic/network_adapters.py'
original = network.request
def request(url, **kwargs):
    assert url.startswith('https://api.themoviedb.org/3/search/movie?')
    endpoint=os.environ['CATABOLIC_TMDB_FIXTURE_ENDPOINT']
    assert endpoint.startswith('http://127.0.0.1:')
    return original(endpoint + '/3/search/movie?' + url.split('?',1)[1], **kwargs)
network.request=request
raise SystemExit(main(sys.argv[1:]))
"""


class IdentifyTest(unittest.TestCase):
    setUp = legacy.NativeLegacyCliTest.setUp
    normalized = legacy.NativeLegacyCliTest.normalized
    compare = legacy.NativeLegacyCliTest.compare

    def command(self, reference):
        return (
            [str(PYTHON), "-c", ORACLE, str(REFERENCE / "src")]
            if reference
            else [str(BINARY)]
        )

    def run_process(self, reference, args, stdin=""):
        return subprocess.run(
            self.command(reference) + args,
            input=stdin,
            capture_output=True,
            text=True,
            env=self.env,
            timeout=35,
        )

    def sql(self, statement, parameters=()):
        for path in (self.reference, self.native):
            with closing(sqlite3.connect(path)) as db, db:
                db.execute(statement, parameters)

    def seed(self):
        self.sql("INSERT INTO locations VALUES ('fixture')")
        self.sql(
            "INSERT INTO files(id,location,path) VALUES ('file','fixture','movie.mkv')"
        )

    def server(self, response=None):
        self.body = json.dumps(
            response if response is not None else {"results": []}
        ).encode()
        self.status = 200
        self.calls = []
        self.delay = 0
        self.entered = threading.Event()
        self.release = threading.Event()
        self.closed = threading.Event()
        self.hook = None
        self.active_path = None
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                fixture.calls.append(
                    (
                        self.path,
                        self.headers.get("Authorization"),
                        self.headers.get("Accept"),
                    )
                )
                if fixture.hook:
                    fixture.hook()
                fixture.entered.set()
                if fixture.delay:
                    deadline = time.monotonic() + fixture.delay
                    while time.monotonic() < deadline and not fixture.release.is_set():
                        readable, _, _ = select.select([self.connection], [], [], 0.05)
                        if readable and self.connection.recv(1) == b"":
                            fixture.closed.set()
                            return
                try:
                    self.send_response(fixture.status)
                    if fixture.status == 302:
                        self.send_header("Location", "/must-not-follow")
                    self.send_header("Content-Length", str(len(fixture.body)))
                    self.end_headers()
                    self.wfile.write(fixture.body)
                except (BrokenPipeError, ConnectionResetError):
                    pass

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(thread.join, 5)
        self.addCleanup(server.shutdown)
        self.addCleanup(self.release.set)
        self.env.update(
            CATABOLIC_TMDB_FIXTURE="1",
            CATABOLIC_TMDB_FIXTURE_ENDPOINT=f"http://127.0.0.1:{server.server_port}",
            CATABOLIC_IDENTIFY_TOKEN=TOKEN,
        )

    def args(self, mode="--machine", **options):
        result = ([mode] if mode else []) + [
            "identify",
            "--file-id",
            options.pop("file_id", "file"),
            "--query",
            options.pop("query", "Example Movie"),
            "--credential-env",
            "CATABOLIC_IDENTIFY_TOKEN",
        ]
        for key, value in options.items():
            result.append("--" + key.replace("_", "-"))
            if value is not True:
                result.append(str(value))
        return result

    def scrub(self, output):
        output = self.normalized(output)
        return re.sub(
            r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{6}\+00:00", "<FETCHED>", output
        )

    def rows(self, path):
        with closing(sqlite3.connect(path)) as db:
            db.row_factory = sqlite3.Row
            return [
                dict(row)
                for row in db.execute("SELECT * FROM proposals ORDER BY rowid")
            ]

    def checked_rows(self, path):
        rows = self.rows(path)
        with closing(sqlite3.connect(path)) as db:
            identity = db.execute(
                "SELECT value FROM meta WHERE key='database_id'"
            ).fetchone()[0]
        for row in rows:
            payload, evidence, snapshot = [
                json.loads(row[k]) for k in ("payload", "evidence", "snapshot")
            ]
            encoded = json.dumps(
                [
                    row["profile"],
                    row["file_id"],
                    payload,
                    row["source"],
                    evidence,
                    snapshot,
                ],
                ensure_ascii=False,
                sort_keys=True,
            )
            self.assertEqual(row["id"], str(uuid.uuid5(uuid.UUID(identity), encoded)))
            self.assertNotIn(TOKEN, json.dumps(row))
            row["id"] = "<PROPOSAL>"
            row["created_at"] = "<CREATED>"
            row["evidence"] = self.scrub(row["evidence"])
        return rows

    def pair(self, mode="--machine", expected=0, **options):
        results = []
        for reference, path in ((True, self.reference), (False, self.native)):
            self.active_path = path
            result = self.run_process(
                reference, ["--db", str(path)] + self.args(mode, **options)
            )
            self.assertEqual(
                result.returncode, expected, (reference, result.stdout, result.stderr)
            )
            stdout = self.scrub(result.stdout)
            for row in self.rows(path):
                stdout = stdout.replace(row["id"], "<PROPOSAL>")
            results.append((stdout, self.scrub(result.stderr)))
        self.assertEqual(results[0], results[1])
        self.assertEqual(
            self.checked_rows(self.reference), self.checked_rows(self.native)
        )
        self.assertNotIn(TOKEN, "".join(results[1]))
        return results[1]

    def test_admission_precedes_network_and_file_validation(self):
        self.server()
        for mode in ("", "--json", "--machine"):
            for options in (
                {"query": " "},
                {"query": "é" * 257},
                {"query": "\x1c"},
                {"year": 0},
                {"year": 10000},
            ):
                self.pair(mode, expected=2, **options)
            self.compare("identify-missing-db", self.args(mode), database=False)
            self.compare(
                "identify-profile-first",
                ["--profile", "missing"] + self.args(mode, query=""),
            )
        self.assertEqual(self.calls, [])
        # Preview never validates the supplied file ID, even with no such file.
        self.pair(file_id="absent")
        self.assertEqual(len(self.calls), 2)

    def test_credential_validation_and_network_errors_exact(self):
        self.server()
        for credential in ("missing", "BAD-NAME", "CATABOLIC_IDENTIFY_TOKEN"):
            self.env["CATABOLIC_IDENTIFY_TOKEN"] = "bad\nvalue"
            self.compare(
                "identify-token", self.args() + ["--credential-env", credential]
            )
        self.assertEqual(self.calls, [])
        self.env["CATABOLIC_IDENTIFY_TOKEN"] = "\x01" * 50000
        self.pair(expected=2)
        self.assertEqual(self.calls, [])
        self.env["CATABOLIC_IDENTIFY_TOKEN"] = TOKEN
        for status in (401, 403, 404, 429, 500, 302):
            self.status = status
            self.pair(expected=2)
        self.assertEqual(len(self.calls), 12)
        self.assertTrue(
            all(call[0].startswith("/3/search/movie?") for call in self.calls)
        )
        self.status = 200
        self.body = b"x" * (1024 * 1024 + 1)
        self.pair(expected=2)

    def test_response_shape_empty_page_and_paging(self):
        self.server()
        for payload in ([], {}, {"results": {}}, {"results": None}):
            self.body = json.dumps(payload).encode()
            self.pair(expected=2)
        for pages in (0, 1, 2, True, False, -2, 1.5, 10**30):
            self.body = json.dumps({"results": [], "total_pages": pages}).encode()
            self.pair(expected=3 if pages > 1 else 0)

    def test_ranking_unicode_filtering_first_twenty_exact_modes(self):
        results = [
            None,
            [],
            {"id": True, "title": "Bad"},
            {"id": 1.0, "title": "Bad"},
            {"id": 1, "title": 7},
            {"id": 2, "title": "Other"},
            {"id": 3, "title": "STRASSE Ⅷ café", "release_date": "٢٠٢٦-01-01"},
            {"id": -8, "title": "café", "release_date": "wrong"},
            {"id": 10**40, "title": "Straße Ⅷ café", "release_date": "2026-"},
            {"id": 4, "title": "Straße Ⅷ café", "release_date": "0000-"},
        ]
        results += [{"id": n + 10, "title": "tail"} for n in range(20)]
        self.server({"results": results, "total_pages": 2})
        for mode in ("", "--json", "--machine"):
            self.pair(mode, expected=3, query="Straße Ⅷ café", year=2026)
        self.assertEqual(len(self.calls), 6)
        self.assertEqual(
            self.calls[0],
            (
                "/3/search/movie?query=Stra%C3%9Fe+%E2%85%A7+caf%C3%A9&include_adult=false&page=1&year=2026",
                "Bearer " + TOKEN,
                "application/json",
            ),
        )
        for payload in (
            {"results": [{"id": 1, "title": "!!!"}]},
            {"results": [{"id": 1, "title": "a\u0301_²"}]},
        ):
            self.body = json.dumps(payload).encode("utf-16")
            self.pair(query="!!!")

    def test_apply_uses_existing_proposal_ids_snapshots_and_partial_transactions(self):
        self.seed()
        self.server(
            {
                "results": [
                    {"id": 2, "title": "Other"},
                    {"id": 1, "title": "Example Movie"},
                ]
            }
        )
        for mode in ("", "--json", "--machine"):
            self.pair(mode, apply=True)
        self.assertEqual(len(self.rows(self.native)), 6)
        with closing(sqlite3.connect(self.native)) as db:
            for table in ("items", "identities", "item_files", "mappings", "journal"):
                self.assertEqual(
                    db.execute(f"SELECT count(*) FROM {table}").fetchone()[0], 0
                )
        self.body = json.dumps(
            {
                "results": [
                    {"id": 3, "title": "First persists"},
                    {"id": 4, "title": "x" * 270000},
                ]
            }
        ).encode()
        self.pair(expected=2, apply=True)
        self.assertEqual(len(self.rows(self.native)), 7)

    def test_apply_file_validation_only_for_valid_candidates(self):
        self.server({"results": [{"id": True, "title": "Skipped"}]})
        self.pair(apply=True, file_id="absent")
        self.body = json.dumps({"results": [{"id": 1, "title": "Candidate"}]}).encode()
        self.pair(expected=2, apply=True, file_id="absent")
        self.assertEqual(self.rows(self.native), [])

    def test_native_detaches_writer_and_fences_replaced_catalog(self):
        self.seed()
        self.server({"results": [{"id": 1, "title": "Candidate"}]})

        def hook():
            with open(str(self.native) + ".lock", "a+b") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                with closing(sqlite3.connect(self.native, timeout=0.1)) as db, db:
                    db.execute("BEGIN IMMEDIATE")
                    db.execute(
                        "UPDATE meta SET value=? WHERE key='database_id'",
                        (str(uuid.uuid4()),),
                    )
                fcntl.flock(lock, fcntl.LOCK_UN)

        self.hook = hook
        result = self.run_process(
            False, ["--db", str(self.native)] + self.args(apply=True)
        )
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn("catalog replaced during detached execution", result.stdout)
        self.assertEqual(self.rows(self.native), [])

    def test_cancel_reaps_http_worker_discards_late_response_and_releases_writer(self):
        self.seed()
        self.server({"results": [{"id": 1, "title": "Candidate"}]})
        self.delay = 30
        for reference, path in ((True, self.reference), (False, self.native)):
            self.entered.clear()
            self.closed.clear()
            process = subprocess.Popen(
                self.command(reference) + ["--db", str(path)] + self.args(apply=True),
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                env=self.env,
            )
            self.addCleanup(lambda p=process: p.poll() is None and p.kill())
            self.assertTrue(self.entered.wait(5))
            process.send_signal(signal.SIGINT)
            out, err = process.communicate(timeout=5)
            self.assertEqual(process.returncode, 130, out + err)
            self.assertTrue(
                self.closed.wait(5), "owned HTTP worker must close the request"
            )
            self.assertEqual(self.rows(path), [])
            with open(str(path) + ".lock", "a+b") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                fcntl.flock(lock, fcntl.LOCK_UN)
            if reference:
                expected = (out, err)
            else:
                self.assertEqual((out, err), expected)
        self.release.set()
        time.sleep(0.1)
        self.assertEqual(self.rows(self.native), [])

    def test_writer_contention_keeps_frozen_machine_error_type(self):
        self.server()
        locks = []
        for path in (self.reference, self.native):
            lock = open(str(path) + ".lock", "a+b")
            self.addCleanup(lock.close)
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            locks.append(lock)
        self.compare("identify-contended", self.args(apply=True, query=""))
        self.assertEqual(self.calls, [])

    def test_pending_recovery_is_checked_by_existing_proposal_owner(self):
        self.seed()
        self.server({"results": [{"id": 1, "title": "Candidate"}]})
        self.sql(
            "INSERT INTO journal(id,profile,catalog,path,kind) VALUES ('pending','default','global','unused','create')"
        )
        self.pair()
        self.pair(expected=2, apply=True)
        self.assertEqual(len(self.calls), 4)
        self.assertEqual(self.rows(self.native), [])

    def test_duplicate_candidates_reuse_one_proposal_within_the_page(self):
        self.seed()
        self.server({"results": [{"id": 1, "title": "Candidate"}] * 2})
        self.pair(apply=True)
        self.assertEqual(len(self.rows(self.native)), 1)

    def test_whole_request_timeout_has_no_late_proposal(self):
        self.seed()
        self.server({"results": [{"id": 1, "title": "Candidate"}]})
        self.delay = 30
        self.pair(expected=2, apply=True)
        self.assertEqual(self.rows(self.native), [])

    def test_fixture_endpoint_does_not_admit_named_or_external_hosts(self):
        self.server()
        for endpoint in (
            "http://localhost:1234",
            "http://192.0.2.1:1234",
            "https://127.0.0.1:1234",
            "http://127.0.0.1:0",
            "http://127.0.0.1:1234/path",
        ):
            self.env["CATABOLIC_TMDB_FIXTURE_ENDPOINT"] = endpoint
            result = self.run_process(False, ["--db", str(self.native)] + self.args())
            self.assertEqual(result.returncode, 2)
            self.assertIn("numeric-loopback", result.stdout)
        self.assertEqual(self.calls, [])

    def test_malformed_json_is_bounded_explicit_native_error(self):
        self.server()
        self.body = b"{bad JSON"
        result = self.run_process(False, ["--db", str(self.native)] + self.args())
        self.assertEqual(result.returncode, 2)
        self.assertIn("invalid TMDB JSON response", result.stdout)
        self.assertEqual(self.rows(self.native), [])


if __name__ == "__main__":
    unittest.main()
