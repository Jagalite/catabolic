# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Plex authorization contracts; all accounts and tokens are synthetic."""

import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from urllib.parse import parse_qs, urlsplit

from catabolic import plex_login
from catabolic.cli import main
from catabolic.consumer_adapters import ConsumerError, Plex
from catabolic.consumer_setup import put_connection
from catabolic.store import Store
from tests.test_consumers import ProtocolServer


class PlexLoginTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name).resolve()
        self.folder = self.root / "credentials"
        self.authorized = False
        self.calls = []
        self.secret = "PRIVATE_SYNTHETIC_PLEX_TOKEN"
        self.mock = patch("catabolic.plex_login.request", side_effect=self.protocol)
        self.mock.start()
        self.addCleanup(self.mock.stop)

    def protocol(self, url, **kwargs):
        self.calls.append((url, kwargs))
        route = urlsplit(url)
        self.assertEqual(route.scheme, "https")
        self.assertEqual(route.netloc, "plex.tv")
        self.assertEqual(kwargs["headers"]["X-Plex-Product"], "Catabolic")
        self.assertEqual(kwargs["headers"]["Accept"], "application/json")
        self.assertEqual(kwargs["timeout"], 15)
        self.assertEqual(kwargs["maximum"], 65536)
        self.assertTrue(kwargs["structured_errors"])
        if route.path == "/api/v2/pins":
            self.assertEqual(kwargs["method"], "POST")
            self.assertEqual(parse_qs(route.query), {"strong": ["true"]})
            return json.dumps(
                {"id": 123, "code": "strong-code", "expiresIn": 600}
            ).encode()
        if route.path == "/api/v2/pins/123":
            self.assertEqual(kwargs["method"], "GET")
            self.assertEqual(parse_qs(route.query), {"code": ["strong-code"]})
            return json.dumps(
                {
                    "id": 123,
                    "code": "strong-code",
                    "authToken": self.secret if self.authorized else None,
                }
            ).encode()
        self.assertEqual(route.path, "/api/v2/user")
        self.assertEqual(kwargs["method"], "GET")
        self.assertEqual(kwargs["headers"]["X-Plex-Token"], self.secret)
        self.assertNotIn(self.secret, url)
        return b'{"id": 42}'

    def login(self):
        start = plex_login.start(self.folder)
        self.authorized = True
        return plex_login.complete(self.folder, start["login_id"])

    def test_pending_authorized_repeat_and_stable_client(self):
        result = plex_login.start(self.folder)
        self.assertFalse(result["complete"])
        url = urlsplit(result["authorization_url"])
        self.assertEqual(
            (url.scheme, url.netloc, url.path), ("https", "app.plex.tv", "/auth")
        )
        query = parse_qs(url.fragment.lstrip("?"))
        self.assertEqual(query["code"], ["strong-code"])
        self.assertEqual(query["context[device][product]"], ["Catabolic"])
        self.assertEqual(
            plex_login.complete(self.folder, result["login_id"])["state"],
            "awaiting_authorization",
        )
        self.authorized = True
        done = plex_login.complete(self.folder, result["login_id"])
        count = len(self.calls)
        self.assertEqual(done, plex_login.complete(self.folder, result["login_id"]))
        self.assertEqual(count, len(self.calls))
        self.assertNotIn(self.secret, json.dumps(done))
        value = plex_login.read_private(done["credential_file"])
        self.assertNotIn("code", value)
        self.assertEqual(value["token"], self.secret)
        plex_login.start(self.folder)
        self.assertEqual(
            {x[1]["headers"]["X-Plex-Client-Identifier"] for x in self.calls},
            {value["client_id"]},
        )
        self.assertEqual(self.folder.stat().st_mode & 0o777, 0o700)
        self.assertEqual(Path(done["credential_file"]).stat().st_mode & 0o777, 0o600)

    def test_expiry_and_invalid_id_do_not_poll(self):
        result = plex_login.start(self.folder)
        with patch("catabolic.plex_login.time.time", return_value=10**12):
            self.assertEqual(
                plex_login.complete(self.folder, result["login_id"])["state"], "expired"
            )
        with self.assertRaises(ConsumerError):
            plex_login.complete(self.folder, "../client")
        self.assertEqual(len(self.calls), 1)

    def test_private_storage_rejects_permissions_and_symlinks(self):
        done = self.login()
        filename = Path(done["credential_file"])
        filename.chmod(0o644)
        with self.assertRaises(ConsumerError):
            plex_login.credential("file:" + str(filename))
        filename.chmod(0o600)
        link = self.folder / "link.json"
        link.symlink_to(filename)
        with self.assertRaises(ConsumerError):
            plex_login.credential("file:" + str(link))
        self.folder.chmod(0o755)
        with self.assertRaises(ConsumerError):
            plex_login.start(self.folder)

    def test_malformed_and_failed_auth_are_redacted_and_retry_preserves_pin(self):
        result = plex_login.start(self.folder)
        self.authorized = True
        with patch(
            "catabolic.plex_login.request",
            return_value=b"not json PRIVATE_SYNTHETIC_PLEX_TOKEN",
        ):
            with self.assertRaises(ConsumerError) as error:
                plex_login.complete(self.folder, result["login_id"])
            self.assertNotIn(self.secret, str(error.exception))
        self.assertIn(
            "code",
            plex_login.read_private(self.folder / (result["login_id"] + ".json")),
        )
        self.assertTrue(
            plex_login.complete(self.folder, result["login_id"])["complete"]
        )

    def test_rate_limit_and_unauthorized_do_not_save_token(self):
        result = plex_login.start(self.folder)
        for code in ("rate_limited", "unauthorized", "temporarily_failed"):
            with patch(
                "catabolic.plex_login.request",
                side_effect=ConsumerError(code, retry_after=120),
            ):
                with self.assertRaises(ConsumerError) as error:
                    plex_login.complete(self.folder, result["login_id"])
                self.assertEqual(error.exception.code, code)
        self.assertNotIn(
            "token",
            plex_login.read_private(self.folder / (result["login_id"] + ".json")),
        )

    def test_failed_write_leaves_resumable_pin_and_no_temporary_token(self):
        result = plex_login.start(self.folder)
        self.authorized = True
        with patch(
            "catabolic.plex_login.os.replace", side_effect=OSError("private failure")
        ):
            with self.assertRaises(ConsumerError):
                plex_login.complete(self.folder, result["login_id"])
        self.assertEqual(list(self.folder.glob(".plex-*")), [])
        self.assertIn(
            "code",
            plex_login.read_private(self.folder / (result["login_id"] + ".json")),
        )
        self.assertTrue(
            plex_login.complete(self.folder, result["login_id"])["complete"]
        )

    def test_mismatched_pin_and_failed_user_validation_never_persist_token(self):
        result = plex_login.start(self.folder)
        filename = self.folder / (result["login_id"] + ".json")
        with patch(
            "catabolic.plex_login.request",
            return_value=json.dumps(
                {
                    "id": 999,
                    "code": "wrong",
                    "authToken": self.secret,
                }
            ).encode(),
        ):
            with self.assertRaises(ConsumerError):
                plex_login.complete(self.folder, result["login_id"])
        self.authorized = True
        original = self.protocol

        def unauthorized_user(url, **kwargs):
            if urlsplit(url).path.endswith("/user"):
                raise ConsumerError("unauthorized")
            return original(url, **kwargs)

        with patch("catabolic.plex_login.request", side_effect=unauthorized_user):
            with self.assertRaises(ConsumerError):
                plex_login.complete(self.folder, result["login_id"])
        self.assertNotIn("token", plex_login.read_private(filename))

    def test_machine_flow_without_database_never_outputs_token(self):
        def cli(*args):
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                code = main(
                    [
                        "--machine",
                        "consumer",
                        *args,
                        "--credential-dir",
                        str(self.folder),
                    ]
                )
            self.assertIn(code, (0, 3))
            self.assertNotIn(self.secret, output.getvalue())
            result = json.loads(output.getvalue())
            self.assertEqual(result["interface_version"], 1)
            return result["data"]

        result = cli("plex-login")
        self.authorized = True
        self.assertTrue(cli("plex-login-complete", result["login_id"])["complete"])

    def test_login_credential_cannot_be_sent_to_jellyfin(self):
        done = self.login()
        with self.assertRaises(ConsumerError) as error:
            put_connection(
                self.root / "unused.db",
                "default",
                "wrong",
                "jellyfin",
                "https://example.invalid",
                "file:" + done["credential_file"],
                apply=True,
            )
        self.assertEqual(
            error.exception.code, "plex_credential_requires_plex_connection"
        )

    def test_file_credential_connects_and_scans_without_environment_token(self):
        done = self.login()
        database = self.root / "catalog.sqlite3"
        Store.initialize(database)
        remote = ProtocolServer(database)
        self.addCleanup(remote.close)
        ref = "file:" + done["credential_file"]
        result = put_connection(
            database, "default", "plex", "plex", remote.url, ref, apply=True
        )
        self.assertFalse(remote.scans)
        Plex(result["connection"]).scan({"id": "1"})
        self.assertEqual(remote.scans, ["/library/sections/1/refresh"])
        self.assertFalse(remote.lock_failures)
        with Store(database) as store:
            rows = store.rows("SELECT * FROM consumer_connections")
        self.assertNotIn(self.secret, json.dumps(rows))
        self.assertEqual(rows[0]["credential_env"], ref)
        self.assertNotIn(self.secret.encode(), database.read_bytes())
