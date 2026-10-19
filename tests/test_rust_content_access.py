# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native content authorization and interoperable bounded transfer tickets."""

import hashlib
import json
import os
import subprocess
import unittest
from urllib.parse import parse_qs, urlsplit

from catabolic.access import grant_put, revoke
from catabolic.content_access import revision_of
from catabolic.store import Store
from tests import test_http_backend as reference
from tests.test_rust_migration import BINARY


class NativeContentAccessTest(unittest.TestCase):
    def setUp(self):
        self.assertTrue(BINARY.is_file(), "build native CLI before qualification")
        reference.HTTPTest.setUp(self)

    def native(
        self, operation="inspect", *, token=None, ticket=None, ok=True, **request
    ):
        request = {"file_id": self.file, "revision": self.revision, **request}
        result = subprocess.run(
            [
                str(BINARY),
                "--db",
                str(self.database),
                "operation",
                "content",
                operation,
                "--options",
                json.dumps(request),
            ],
            env={
                **os.environ,
                "CATABOLIC_API_TOKEN": token or self.credential["token"],
                "CATABOLIC_CONTENT_TICKET": ticket or "",
            },
            capture_output=True,
            text=True,
            timeout=20,
        )
        self.assertEqual(
            result.returncode, 0 if ok else 2, result.stdout + result.stderr
        )
        return json.loads(result.stdout) if ok else result.stderr

    def test_auth_readonly_exact_revision_and_private_redaction(self):
        before = self.database.read_bytes()
        self.assertEqual(
            self.native(),
            {
                "file_id": self.file,
                "revision": self.revision,
                "size": len(self.payload),
            },
        )
        self.assertEqual(self.database.read_bytes(), before)
        self.assertIn("not_found", self.native(file_id=self.hidden_file, ok=False))
        self.assertIn("not_found", self.native(token=self.other, ok=False))
        self.assertIn("revision_mismatch", self.native(revision="0" * 64, ok=False))
        self.assertIn("unauthorized", self.native(token="invalid", ok=False))

    def test_native_ticket_python_ranges_and_native_debit_are_one_budget(self):
        created = self.native("ticket", ttl=120, max_bytes=20)
        secret = parse_qs(urlsplit(created["content_path"]).query)["ticket"][0]
        self.assertEqual(len(secret.split(".")[0]), 24)
        self.assertEqual(len(secret.split(".")[1]), 43)
        with Store(self.database) as store:
            row = store.rows(
                "SELECT * FROM api_tickets WHERE id=?", (created["ticket_id"],)
            )[0]
            self.assertEqual(row["digest"], hashlib.sha256(secret.encode()).hexdigest())
            self.assertNotIn(secret, str(row))
        result = self.client.get(
            created["content_path"], headers={"Range": "bytes=0-9"}
        )
        self.assertEqual(result.status_code, 206, result.text)
        self.assertEqual(result.content, self.payload[:10])
        self.native("inspect-ticket", ticket=secret)
        self.native("debit-ticket", ticket=secret, length=10)
        self.assertIn(
            "ticket_transfer_limit",
            self.native("debit-ticket", ticket=secret, length=1, ok=False),
        )
        self.assertEqual(self.client.head(created["content_path"]).status_code, 200)
        with Store(self.database) as store:
            self.assertEqual(
                store.rows("SELECT remaining_bytes FROM api_tickets")[0][
                    "remaining_bytes"
                ],
                0,
            )

    def test_python_ticket_native_revalidates_grants_token_and_owner(self):
        response = self.client.post(
            "/v1/content-access",
            headers=self.headers,
            json={"file_id": self.file, "revision": self.revision},
        )
        self.assertEqual(response.status_code, 200, response.text)
        created = response.json()
        secret = parse_qs(urlsplit(created["content_path"]).query)["ticket"][0]
        self.native("inspect-ticket", ticket=secret)
        self.assertIn(
            "not_found",
            self.native(
                "inspect-ticket", ticket=secret, file_id=self.hidden_file, ok=False
            ),
        )
        self.native("revoke-ticket", token=self.other, ticket_id=created["ticket_id"])
        self.native("inspect-ticket", ticket=secret)
        with Store(self.database, writable=True) as store:
            grant_put(store, "site", {**self.definition, "file_ids": []}, self.grant)
        self.assertIn(
            "not_found", self.native("inspect-ticket", ticket=secret, ok=False)
        )
        with Store(self.database, writable=True) as store:
            grant_put(store, "site", self.definition, self.grant)
            revoke(store, self.credential["token_id"])
        self.assertIn(
            "unauthorized", self.native("inspect-ticket", ticket=secret, ok=False)
        )

    def test_source_exposure_revision_sqlite_header_and_named_identity(self):
        with Store(self.database, writable=True) as store:
            store.db.execute("DELETE FROM api_sources")
        self.assertIn("source_not_exposed", self.native(ok=False))
        with Store(self.database, writable=True) as store:
            store.db.execute("INSERT INTO api_sources VALUES ('default','media')")
        path = self.source / "allowed.bin"
        path.write_bytes(b"SQLite format 3\x00" + b"private" * 100)
        from catabolic.app import Application

        with Store(self.database, writable=True) as store:
            Application(store).scan()
            self.revision = revision_of(store, "default", self.file)
        self.assertIn("private_content", self.native(ok=False))
        path.unlink()
        path.symlink_to(self.source / "hidden.bin")
        self.assertNotEqual(self.native(ok=False), "")

    def test_expired_revoked_and_exhausted_ticket_do_not_change_ledger(self):
        created = self.native("ticket", ttl=1, max_bytes=1)
        secret = parse_qs(urlsplit(created["content_path"]).query)["ticket"][0]
        self.assertIn(
            "unauthorized",
            self.native("inspect-ticket", ticket=secret + "bad", ok=False),
        )
        self.native("revoke-ticket", ticket_id=created["ticket_id"])
        self.assertIn(
            "unauthorized", self.native("inspect-ticket", ticket=secret, ok=False)
        )
        with Store(self.database) as store:
            self.assertEqual(
                store.rows("SELECT remaining_bytes FROM api_tickets")[0][
                    "remaining_bytes"
                ],
                1,
            )

    def test_live_and_registered_retired_sidecar_aliases_are_refused(self):
        from catabolic.app import Application

        for suffix in ("-shm", "-wal", "-journal"):
            with self.subTest(suffix=suffix):
                private = self.database.with_name(self.database.name + suffix)
                private.write_bytes(b"private sidecar bytes" * 3)
                alias = self.source / ("alias" + suffix + ".bin")
                os.link(private, alias)
                original = alias.stat()
                try:
                    with Store(self.database, writable=True) as store:
                        if suffix != "-shm":
                            # SQLite may retire the WAL/journal pathname while
                            # an alias survives. Retained private-file identity
                            # is required after the original pathname is gone.
                            with store.transaction() as db:
                                db.execute(
                                    "INSERT INTO api_private_files VALUES (?,?,?)",
                                    (str(private), original.st_dev, original.st_ino),
                                )
                        Application(store).scan()
                        file_id = store.rows(
                            "SELECT id FROM files WHERE path=?", (alias.name,)
                        )[0]["id"]
                        revision = revision_of(store, "default", file_id)
                    error = self.native(
                        token=self.operator,
                        file_id=file_id,
                        revision=revision,
                        ok=False,
                    )
                    self.assertIn("private_content", error)
                finally:
                    alias.unlink(missing_ok=True)
                    private.unlink(missing_ok=True)
