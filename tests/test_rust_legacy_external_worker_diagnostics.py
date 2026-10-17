# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Bounded ordinary worker diagnostics on independent disposable catalogs.

Numeric loopback only; no live service, real credentials, physical output
production or source-policy review. Selected malformed-protocol, truncated-body,
refused-connection and credential-header encoding cases are compared too.
"""

import json
import shutil
import socket
import unittest
from dataclasses import dataclass
from pathlib import Path

import tests.test_rust_legacy_external_rule as fixture

MAXIMUM = 1024 * 1024
REDIRECT = "redirect refused; configure the final endpoint explicitly"
OVERSIZE = "HTTP response exceeds byte limit"
NETWORK = "HTTP request failed; check endpoint, credentials and network"


@dataclass(frozen=True)
class Reply:
    status: int = 200
    body: bytes = b'{"state":"running"}'
    headers: tuple = ()
    chunked: bool = False


@dataclass(frozen=True)
class Raw:
    """Exact response bytes, followed by connection close."""

    data: bytes


class ExternalWorkerDiagnosticsTest(fixture.NativeLegacyExternalRuleTest):
    def setUp(self):
        super().setUp()
        self.seed = self.root / "seed.db"
        shutil.copyfile(self.db[True], self.seed)
        owner = self.server

        def respond(handler):
            try:
                length = int(handler.headers.get("Content-Length", "0"))
                raw = handler.rfile.read(length)
                owner.requests.append(
                    (handler.command, handler.path, json.loads(raw) if raw else None)
                )
                # http.server decodes header bytes as Latin-1.
                owner.authorization.append(handler.headers.get("Authorization"))
                reply = owner.responses.pop(0)
                if isinstance(reply, fixture.Gate):
                    reply.entered.set()
                    if not reply.release.wait(20):
                        raise AssertionError("loopback response was never released")
                    reply = reply.result
                if isinstance(reply, Raw):
                    handler.wfile.write(reply.data)
                    handler.wfile.flush()
                    handler.close_connection = True
                    return
                if not isinstance(reply, Reply):
                    reply = Reply(body=json.dumps(reply).encode())
                handler.send_response(reply.status)
                handler.send_header("Content-Type", "application/json")
                for name, value in reply.headers:
                    handler.send_header(name, value)
                if reply.chunked:
                    handler.send_header("Transfer-Encoding", "chunked")
                else:
                    handler.send_header("Content-Length", str(len(reply.body)))
                handler.send_header("Connection", "close")
                handler.end_headers()
                if reply.chunked:
                    for offset in range(0, len(reply.body), 16384):
                        data = reply.body[offset : offset + 16384]
                        handler.wfile.write(
                            f"{len(data):x}\r\n".encode() + data + b"\r\n"
                        )
                    handler.wfile.write(b"0\r\n\r\n")
                else:
                    handler.wfile.write(reply.body)
            except (BrokenPipeError, ConnectionResetError):
                # A bounded reader may stop before the fixture finishes writing.
                pass
            except BaseException as exc:
                owner.errors.append(repr(exc))

        self.server.authorization = []
        self.server.server.RequestHandlerClass.protocol_version = "HTTP/1.1"
        self.server.server.RequestHandlerClass.respond = respond

    def reset_catalogs(self, method):
        for flag in (True, False):
            shutil.copyfile(self.seed, self.db[flag])
        self.recorded_volatile = {True: {}, False: {}}
        if method == "GET":
            self.pair(self.run_args(), responses=[Reply()])

    def diagnostic_pair(self, reply, expected, *, method="PUT", mode="--json"):
        self.reset_catalogs(method)
        results, requests = self.pair(self.run_args(mode=mode), responses=[reply])
        self.assertEqual(len(requests), 1, "redirect target must never be contacted")
        self.assertEqual(requests[0][:2], (method, f"/jobs/{fixture.JOB_IDS[0]}/1"))
        # Full reports, request values and every durable table were compared by
        # pair(). Also compare diagnostic UTF-8 bytes without text normalization.
        for flag in (True, False):
            result = results[flag]
            self.assertEqual(result.returncode, 3)
            self.assertEqual(result.stderr.encode(), b"")
            errors = self.report(result)["execution"]["errors"]
            self.assertEqual(
                errors,
                []
                if expected is None
                else [{"job_id": fixture.JOB_IDS[0], "error": expected}],
            )
            if expected is not None:
                self.assertEqual(errors[0]["error"].encode(), expected.encode())
                self.assertIn(json.dumps(expected).encode(), result.stdout.encode())
            self.assert_writer_available(flag)
        return results

    def test_redirect_status_and_location_presence_for_put_and_get(self):
        for method in ("PUT", "GET"):
            for status in (301, 302, 303, 307, 308):
                for headers in ((), (("Location", self.server.endpoint + "/never"),)):
                    with self.subTest(method=method, status=status, headers=headers):
                        self.diagnostic_pair(
                            Reply(status=status, body=b"", headers=headers),
                            REDIRECT
                            if headers
                            else f"HTTP request failed with status {status}",
                            method=method,
                        )

    def test_ordinary_location_forms_and_legacy_uri_header(self):
        cases = [
            ((("Location", "/never"),), REDIRECT),
            ((("Location", ""),), REDIRECT),
            ((("Location", "../never"),), REDIRECT),
            ((("URI", "/never"),), REDIRECT),
            (
                (("Location", "/never"), ("URI", "mailto:unused@example.invalid")),
                REDIRECT,
            ),
            (
                (("Location", "mailto:unused@example.invalid"), ("URI", "/never")),
                "HTTP request failed with status 302",
            ),
            ((("Location", "FTP://127.0.0.1/never"),), REDIRECT),
        ]
        for headers, expected in cases:
            with self.subTest(headers=headers):
                self.diagnostic_pair(
                    Reply(status=302, headers=headers), expected, mode="--machine"
                )

    def test_other_statuses_keep_their_diagnostic_and_success_neighbors(self):
        for status in (200, 201, 202, 300, 304, 305, 306, 309, 400, 404, 429, 500, 503):
            for headers in ((), (("Location", "/never"),)):
                with self.subTest(status=status, headers=headers):
                    self.diagnostic_pair(
                        Reply(status=status, headers=headers),
                        None
                        if 200 <= status < 300
                        else f"HTTP request failed with status {status}",
                    )

    def test_exact_response_byte_boundaries_with_length_and_chunked_bodies(self):
        prefix = b'{"state":"running","text":"Caf\xc3\xa9 \xe9\x9b\xaa"}'
        for method in ("PUT", "GET"):
            for chunked in (False, True):
                for size in (MAXIMUM - 1, MAXIMUM, MAXIMUM + 1):
                    with self.subTest(method=method, chunked=chunked, size=size):
                        body = prefix + b" " * (size - len(prefix))
                        self.assertEqual(len(body), size)
                        self.diagnostic_pair(
                            Reply(body=body, chunked=chunked),
                            OVERSIZE if size > MAXIMUM else None,
                            method=method,
                        )

    def test_valid_receipt_after_durable_cancellation_remains_rejected(self):
        before = {flag: self.snapshot(flag) for flag in (True, False)}
        results = {}
        for flag in (True, False):
            child, gate = self.start_blocked(flag, self.run_args())
            self.assert_writer_available(flag)
            row = next(
                r
                for r in self.snapshot(flag)["processor_jobs"]
                if r["id"] == fixture.JOB_IDS[0]
            )
            body = json.loads(row["request"])
            receipt = {
                "version": 1,
                "database_id": body["database_id"],
                "profile": "default",
                "producer": row["processor"],
                "instance": row["worker"],
                "job_id": row["id"],
                "attempt": str(row["generation"]),
                "configuration_digest": body["configuration_digest"],
                "tools": {"fixture": "1"},
                "outcome": "complete",
                "source": body["source"],
                "outputs": [
                    {
                        "location": "fixture",
                        "path": "3.txt",
                        "definition_id": self.ids["output"],
                        "size": len(self.source_before["3.txt"]),
                    }
                ],
            }
            gate.result = Reply(
                body=json.dumps({"state": "complete", "receipt": receipt}).encode()
            )
            cancelled = self.python(
                "from catabolic.store import Store;from catabolic.app import Application;from catabolic.processors import Processors\nwith Store(sys.argv[1],writable=True) as store: Processors(Application(store)).cancel(sys.argv[2])",
                str(self.db[flag]),
                fixture.JOB_IDS[0],
            )
            self.assertEqual(cancelled.returncode, 0, cancelled.stderr)
            results[flag] = self.finish(child, gate)
        self.assert_states(before, allowed=fixture.MUTABLE_WORKER_TABLES)
        self.assert_results(results)
        self.assertEqual(
            self.report(results[False])["execution"]["errors"],
            [
                {
                    "job_id": fixture.JOB_IDS[0],
                    "error": "worker lease is expired, replaced or no longer active",
                }
            ],
        )
        self.assertEqual(
            self.snapshot(False)["processor_attempts"][0]["state"], "cancelled"
        )

    def test_protocol_and_truncation_failures_use_frozen_worker_diagnostic(self):
        invalid = "processor returned invalid JSON"
        head = b"HTTP/1.1 200 OK\r\nConnection: close\r\n"
        cases = {
            "closed-without-response": (Raw(b""), NETWORK),
            "malformed-status-line": (Raw(b"garbage\r\n\r\n"), NETWORK),
            "truncated-chunked-body": (
                Raw(head + b'Transfer-Encoding: chunked\r\n\r\n5\r\n{"st'),
                NETWORK,
            ),
            "unterminated-chunked-body": (
                Raw(head + b"Transfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n"),
                NETWORK,
            ),
            # http.client returns a length-delimited body cut short by EOF.
            "truncated-length-body": (
                Raw(head + b"Content-Length: 100\r\n\r\n{"),
                invalid,
            ),
            "truncated-length-valid-json": (
                Raw(head + b'Content-Length: 100\r\n\r\n{"state":"running"}'),
                None,
            ),
            "advertised-oversize-without-body": (
                Raw(head + b"Content-Length: %d\r\n\r\n" % (MAXIMUM + 1)),
                invalid,
            ),
            "advertised-oversize-partial-over-limit": (
                Raw(
                    head
                    + b"Content-Length: %d\r\n\r\n" % (MAXIMUM + 10)
                    + b" " * (MAXIMUM + 1)
                ),
                OVERSIZE,
            ),
            "close-delimited": (Raw(head + b'\r\n{"state":"running"}'), None),
        }
        for method in ("PUT", "GET"):
            for name, (reply, expected) in cases.items():
                with self.subTest(method=method, case=name):
                    self.diagnostic_pair(reply, expected, method=method)

    def failed_without_request(self, expected):
        self.reset_catalogs("PUT")
        results, requests = self.pair(self.run_args())
        self.assertEqual(requests, [])
        for flag in (True, False):
            self.assertEqual(results[flag].returncode, 3)
            self.assertEqual(
                self.report(results[flag])["execution"]["errors"],
                [{"job_id": fixture.JOB_IDS[0], "error": expected}],
            )
            self.assert_writer_available(flag)

    def test_refused_connection_uses_frozen_worker_diagnostic(self):
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            closed = f"http://127.0.0.1:{probe.getsockname()[1]}"
        self.mutate(
            "UPDATE processors SET definition=json_set(definition,'$.endpoint',?) WHERE profile='default'",
            (closed,),
        )
        shutil.copyfile(self.db[True], self.seed)
        self.failed_without_request(NETWORK)

    def test_credential_header_encoding_and_exact_request_budget(self):
        variable = "EXTERNAL_RULE_ENCODED_CREDENTIAL"
        self.mutate(
            "UPDATE processors SET definition=json_set(definition,'$.credential_env',?) WHERE profile='default'",
            (variable,),
        )
        shutil.copyfile(self.db[True], self.seed)
        # An ASCII credential is sent identically by both workers.
        self.env[variable] = "FAKE-ascii"
        self.server.authorization = []
        self.diagnostic_pair(Reply(), None)
        self.assertEqual(self.server.authorization, ["Bearer FAKE-ascii"] * 2)
        # A wider character fails header encoding before any connection.
        self.env[variable] = "FAKE-\u2603"
        self.failed_without_request(NETWORK)
        # A long credential is bounded by the exact worker envelope, not by a
        # separate credential-length refusal.
        self.env[variable] = "F" * (300 * 1024)
        self.failed_without_request("HTTP request exceeds input limit")

    def test_resumed_poll_cleanup_neighbor(self):
        super().test_pending_same_worker_polls_once_without_new_generation()

    def test_writer_release_cleanup_neighbor(self):
        super().test_network_wait_releases_application_and_sqlite_writer_locks()

    def test_cancel_cleanup_neighbor(self):
        super().test_cancellation_during_network_wait_fences_late_response()

    @unittest.skipUnless(Path("/proc/self/task").exists(), "Linux process-tree witness")
    def test_interrupt_cleanup_neighbor(self):
        super().test_interrupt_reaps_http_child_and_leaves_reclaimable_durable_lease()


def load_tests(_loader, _tests, _pattern):
    # Keep only unique IDs owned here. Inherited broad definition/admission
    # cases belong to the base module and must not be rediscovered here.
    names = sorted(
        n for n in ExternalWorkerDiagnosticsTest.__dict__ if n.startswith("test_")
    )
    return unittest.TestSuite(ExternalWorkerDiagnosticsTest(n) for n in names)


if __name__ == "__main__":
    unittest.main()
