# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Actual loopback native HTTP transport against frozen HTTP assertions."""

import json
import os
import socket
import subprocess
import time
import unittest
from pathlib import Path

import httpx

from tests import test_http_backend as reference

BINARY = Path(
    os.environ.get("CATABOLIC_HTTP_BINARY", "target/debug/catabolic-http-experimental")
).resolve()


class NativeHTTPTransportTest(unittest.TestCase):
    def setUp(self):
        self.assertTrue(BINARY.is_file(), "build catabolic-http-experimental")
        reference.HTTPTest.setUp(self)
        self.client.close()
        self.start_native_server()

    def start_native_server(self, env=None, read_only=False, arguments=None):
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        self.base = f"http://127.0.0.1:{port}"
        self.log = (self.root / "native-http.log").open("wb")
        self.addCleanup(self.log.close)
        if arguments is None:
            command = [
                str(BINARY),
                str(self.database),
                str(port),
                "read-only" if read_only else "writable",
            ]
        else:
            native = Path(
                os.environ.get(
                    "CATABOLIC_NATIVE_BINARY", "target/debug/catabolic-native"
                )
            ).resolve()
            command = [
                str(native),
                "--db",
                str(self.database),
                "api",
                "serve",
                "--experimental-native-http",
                "--port",
                str(port),
                *([] if read_only else ["--enable-processing"]),
                *arguments,
            ]
        self.server = subprocess.Popen(
            command, stdout=self.log, stderr=self.log, env=env
        )
        self.addCleanup(self.stop_server)
        self.client = httpx.Client(base_url=self.base, timeout=15, trust_env=False)
        self.addCleanup(self.client.close)
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if self.server.poll() is not None:
                self.fail((self.root / "native-http.log").read_text())
            try:
                if self.client.get("/v1/me").status_code == 401:
                    break
            except httpx.TransportError:
                pass
            time.sleep(0.03)
        else:
            self.fail("native loopback listener did not start")

    def stop_server(self):
        if self.server.poll() is None:
            self.server.terminate()
            self.server.wait(timeout=10)

    def test_frozen_contract_exact_and_route_inventory(self):
        path = (
            Path(__file__).resolve().parents[1]
            / "src/catabolic/http/releases/1.7.0/openapi.json"
        )
        expected = json.loads(path.read_text())
        actual = self.client.get("/v1/openapi.json", headers=self.headers)
        self.assertEqual(actual.status_code, 200, actual.text)
        self.assertEqual(actual.json(), expected)
        self.assertEqual(sum(len(v) for v in expected["paths"].values()), 62)
        for path, methods in expected["paths"].items():
            for method in methods:
                target = (
                    path.replace("{identifier}", "unknown")
                    .replace("{name}", "unknown")
                    .replace("{resource}", "queries")
                    .replace("{revision_id}", "unknown")
                    .replace("{segment}", "segment000.ts")
                )
                if target.endswith("/content") and "playback" not in target:
                    target += "?revision=unknown"
                response = self.client.request(
                    method, target, json={} if method in ("post", "put") else None
                )
                self.assertIn(
                    response.status_code,
                    (401, 422),
                    (method, target, response.status_code, response.text),
                )

    def test_stream_disconnect_releases_writer_and_descriptor(self):
        from catabolic.store import Store

        for _ in range(15):
            with self.client.stream(
                "GET", self.content, headers=self.headers
            ) as response:
                self.assertEqual(response.status_code, 200)
                next(response.iter_raw())
        deadline = time.monotonic() + 3
        while True:
            response = self.client.get(self.content, headers=self.headers)
            if response.status_code == 200:
                self.assertEqual(response.content, self.payload)
                break
            self.assertLess(time.monotonic(), deadline, response.text)
            time.sleep(0.02)
        with Store(self.database, writable=True):
            pass

    def test_rejected_stream_and_head_do_not_debit_ticket(self):
        from contextlib import ExitStack

        from catabolic.store import Store

        ticket = self.client.post(
            "/v1/content-access",
            headers=self.headers,
            json={
                "file_id": self.file,
                "revision": self.revision,
                "max_bytes": 1048576,
            },
        ).json()
        with ExitStack() as stack:
            iterators = []
            for _ in range(2):
                stream = stack.enter_context(
                    self.client.stream(
                        "GET", "/v1/events?follow=true", headers=self.headers
                    )
                )
                self.assertEqual(stream.status_code, 200)
                iterator = stream.iter_raw()
                iterators.append(iterator)
                next(iterator)
            with Store(self.database) as store:
                before = store.rows(
                    "SELECT remaining_bytes FROM api_tickets WHERE id=?",
                    (ticket["ticket_id"],),
                )[0]["remaining_bytes"]
            head = self.client.head(ticket["content_path"])
            self.assertEqual(head.status_code, 200, head.text)
            rejected = self.client.get(ticket["content_path"])
            self.assertEqual(rejected.status_code, 429, rejected.text)
            self.assertEqual(rejected.json()["code"], "stream_limit")
            with Store(self.database) as store:
                self.assertEqual(
                    store.rows(
                        "SELECT remaining_bytes FROM api_tickets WHERE id=?",
                        (ticket["ticket_id"],),
                    )[0]["remaining_bytes"],
                    before,
                )

    def test_blocked_transfer_rechecks_live_authorization(self):
        from catabolic.access import grant_put
        from catabolic.app import Application
        from catabolic.content_access import revision_of
        from catabolic.store import Store

        path = self.source / "allowed.bin"
        path.write_bytes(b"x" * (16 * 1024 * 1024))
        with Store(self.database, writable=True) as store:
            Application(store).scan()
            revision = revision_of(store, "default", self.file)
        endpoint = f"/v1/files/{self.file}/content?revision={revision}"
        for change in ("grant", "disabled", "expired", "revoked"):
            with self.subTest(change=change):
                with Store(self.database, writable=True) as store:
                    grant_put(store, "site", self.definition, self.grant)
                    with store.transaction() as db:
                        db.execute(
                            "UPDATE api_principals SET enabled=1 WHERE id='site'"
                        )
                        db.execute(
                            "UPDATE api_tokens SET revoked=0,expires=? WHERE id=?",
                            (time.time() + 3600, self.credential["token_id"]),
                        )
                with self.client.stream(
                    "GET", endpoint, headers=self.headers
                ) as response:
                    self.assertEqual(response.status_code, 200)
                    time.sleep(0.2)
                    with Store(self.database, writable=True) as store:
                        if change == "grant":
                            grant_put(
                                store,
                                "site",
                                {**self.definition, "file_ids": []},
                                self.grant,
                            )
                        else:
                            with store.transaction() as db:
                                if change == "disabled":
                                    db.execute(
                                        "UPDATE api_principals SET enabled=0 WHERE id='site'"
                                    )
                                elif change == "expired":
                                    db.execute(
                                        "UPDATE api_tokens SET expires=0 WHERE id=?",
                                        (self.credential["token_id"],),
                                    )
                                else:
                                    db.execute(
                                        "UPDATE api_tokens SET revoked=1 WHERE id=?",
                                        (self.credential["token_id"],),
                                    )
                    with self.assertRaises(httpx.RemoteProtocolError):
                        response.read()
                with Store(self.database, writable=True):
                    pass


for name in (
    "test_authentication_and_private_fields",
    "test_graphql_authorizes_before_search_and_nested_results",
    "test_sql_operator_only_and_security_aliases_denied",
    "test_exact_full_ranges_head_and_revision",
    "test_ranges_conditionals_and_forbidden_alternate",
    "test_tickets_multiple_ranges_budget_and_revoke",
    "test_revocation_applies_to_existing_ticket",
    "test_source_exposure_and_changes_fail_closed",
    "test_cross_principal_cursors",
    "test_dynamic_grant_rechecks_membership",
    "test_snapshot_does_not_override_later_revocation",
    "test_empty_file_head_and_unsatisfied_range",
    "test_private_catalog_bytes_and_registered_credential_are_never_served",
    "test_expiry_and_revision_pins_apply_to_metadata_and_tickets",
    "test_operator_preview_rolls_back_and_stale_plan_is_rejected",
    "test_worker_and_integration_leases_are_private_to_http_sql",
    "test_event_cursor_expiry_and_principal_binding",
    "test_bounded_body_returns_problem_details",
    "test_head_range_unknown_units_and_weak_conditional_comparison",
):
    setattr(NativeHTTPTransportTest, name, getattr(reference.HTTPTest, name))
