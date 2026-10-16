# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Credential CLI checks use disposable catalogs and synthetic principals only."""

import hashlib
import json
import os
import pwd
import re
import sqlite3
import time
import unittest
from contextlib import closing
from pathlib import Path

from tests import test_rust_legacy_access_admin as fixtures


class LegacyPrivateSecret(unittest.TestCase):
    setUpClass = classmethod(fixtures.LegacyAccessAdmin.setUpClass.__func__)
    tearDownClass = classmethod(fixtures.LegacyAccessAdmin.tearDownClass.__func__)
    setUp = fixtures.LegacyAccessAdmin.setUp
    run_cli = fixtures.LegacyAccessAdmin.run_cli
    state = fixtures.LegacyAccessAdmin.state
    compare = fixtures.LegacyAccessAdmin.compare

    def prepare(self, engine):
        result = self.run_cli(
            engine,
            [
                "api",
                "grant-put",
                "fixture",
                "--file",
                str(self.grant),
                "--id",
                "fake-grant",
            ],
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return self.root / ("reference-output" if engine else "native-output")

    def invoke(self, engine, operation, subject, output, *extra):
        return self.run_cli(
            engine, ["api", operation, subject, "--output", str(output), *extra]
        )

    def verify_saved(self, engine, result, output):
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        summary = json.loads(result.stdout)["data"]
        raw = output.read_bytes()
        value = json.loads(raw)
        self.assertEqual(list(value), ["token_id", "token", "expires"])
        self.assertEqual(raw, (json.dumps(value) + "\n").encode())
        self.assertEqual(output.stat().st_mode & 0o777, 0o600)
        self.assertEqual(set(summary), {"token_id", "expires", "credential_file"})
        self.assertEqual(
            summary,
            {
                "token_id": value["token_id"],
                "expires": value["expires"],
                "credential_file": str(output),
            },
        )
        self.assertRegex(value["token_id"], r"^[0-9a-f]{24}$")
        self.assertTrue(
            re.fullmatch(value["token_id"] + r"\.[A-Za-z0-9_-]{43}", value["token"])
        )
        self.assertNotIn(value["token"], result.stdout + result.stderr)
        self.assertLess(abs(value["expires"] - time.time() - 3600), 30)
        with closing(sqlite3.connect(self.paths[engine])) as db:
            row = db.execute(
                "SELECT principal,digest,grants,expires,revoked FROM api_tokens WHERE id=?",
                (value["token_id"],),
            ).fetchone()
            self.assertEqual(
                row,
                (
                    "fixture",
                    hashlib.sha256(value["token"].encode()).hexdigest(),
                    '["fake-grant"]',
                    value["expires"],
                    0,
                ),
            )
            identity = db.execute(
                "SELECT device,inode FROM api_private_files WHERE path=?",
                (str(output),),
            ).fetchone()
            stat = output.stat()
            self.assertEqual(identity, (stat.st_dev, stat.st_ino))
        return value["token_id"]

    def test_token_create_rotate_and_exclusive_failure_order(self):
        snapshots = []
        for engine in (True, False):
            folder = self.prepare(engine)
            folder.mkdir()
            first = folder / "FAKE-first.json"
            old = self.verify_saved(
                engine,
                self.invoke(
                    engine, "token-create", "fixture", first, "--grant", "fake-grant"
                ),
                first,
            )
            second = folder / "FAKE-rotated.json"
            new = self.verify_saved(
                engine, self.invoke(engine, "token-rotate", old, second), second
            )
            before = second.read_bytes()
            failed = self.invoke(engine, "token-rotate", new, second)
            self.assertNotEqual(failed.returncode, 0)
            self.assertEqual(second.read_bytes(), before)
            self.assertNotIn(json.loads(before)["token"], failed.stdout + failed.stderr)
            with closing(sqlite3.connect(self.paths[engine])) as db:
                snapshots.append(
                    {
                        "old_revoked": db.execute(
                            "SELECT revoked FROM api_tokens WHERE id=?", (old,)
                        ).fetchone()[0],
                        "new_revoked": db.execute(
                            "SELECT revoked FROM api_tokens WHERE id=?", (new,)
                        ).fetchone()[0],
                        "token_count": db.execute(
                            "SELECT count(*) FROM api_tokens"
                        ).fetchone()[0],
                        "private_count": db.execute(
                            "SELECT count(*) FROM api_private_files"
                        ).fetchone()[0],
                        "audit": db.execute(
                            "SELECT action FROM api_audit ORDER BY id"
                        ).fetchall(),
                    }
                )
        self.assertEqual(snapshots[0], snapshots[1])
        self.assertEqual(snapshots[0]["old_revoked"], 1)
        self.assertEqual(snapshots[0]["new_revoked"], 0)
        self.assertEqual(snapshots[0]["token_count"], 5)  # two seeded + three issued
        self.assertEqual(snapshots[0]["private_count"], 2)

    def test_home_expansion_normalizes_separators_before_create_and_rotate(self):
        home = Path(self.env["HOME"])
        home.mkdir()
        for engine in (True, False):
            folder = self.prepare(engine)
            folder.mkdir()
            for variant in ("ordinary", "double", "repeated", "leading-dot"):
                old = None
                for operation in ("token-create", "token-rotate"):
                    with self.subTest(
                        engine=engine, variant=variant, operation=operation
                    ):
                        name = f"FAKE-{variant}-{operation}.json"
                        # Both the expected HOME path and the former erroneous
                        # absolute destination remain inside this disposable root.
                        alternate = folder / name
                        suffix = str(alternate).lstrip("/")
                        if variant == "ordinary":
                            argument = f"~/control/{folder.name}/{name}"
                            expected = home / "control" / folder.name / name
                        else:
                            argument = {
                                "double": "~//" + suffix,
                                "repeated": "~///" + suffix.replace("/", "//"),
                                "leading-dot": "./~//" + suffix,
                            }[variant]
                            expected = home / suffix
                            if variant == "leading-dot":
                                alternate = self.root / "~" / suffix
                        self.assertTrue(expected.is_relative_to(self.root))
                        self.assertTrue(alternate.is_relative_to(self.root))
                        expected.parent.mkdir(parents=True, exist_ok=True)
                        alternate.parent.mkdir(parents=True, exist_ok=True)
                        extra = ("--grant", "fake-grant") if old is None else ()
                        result = self.invoke(
                            engine, operation, old or "fixture", argument, *extra
                        )
                        self.assertEqual(
                            result.returncode, 0, result.stdout + result.stderr
                        )
                        summary = json.loads(result.stdout)["data"]
                        previous, old = old, summary["token_id"]
                        self.assertEqual(summary["credential_file"], str(expected))
                        identifier = self.verify_saved(engine, result, expected)
                        self.assertFalse(alternate.exists())
                        if previous is not None:
                            with closing(sqlite3.connect(self.paths[engine])) as db:
                                self.assertEqual(
                                    db.execute(
                                        "SELECT revoked FROM api_tokens WHERE id=?",
                                        (previous,),
                                    ).fetchone(),
                                    (1,),
                                )
                        old = identifier

    def run_with_env(self, env, engine, arguments, prefix=None):
        saved, self.env = self.env, env
        try:
            return self.run_cli(engine, arguments, prefix=prefix)
        finally:
            self.env = saved

    def test_named_user_home_unset_and_unusual_home_expansion(self):
        # Frozen pathlib expands `~user` through passwd, falls back to the
        # passwd home when HOME is unset, maps an empty HOME to `/`, and
        # keeps `..` unresolved. `..` steps keep every write in this fixture.
        account = pwd.getpwuid(os.getuid())
        climb = "../" * len(Path(account.pw_dir).parts)
        unset = {k: v for k, v in self.env.items() if k != "HOME"}
        (self.root / "relative-home").mkdir()
        for engine in (True, False):
            folder = self.prepare(engine)
            folder.mkdir()
            inside = str(folder).lstrip("/")
            cases = {
                "named-user": (
                    self.env,
                    f"~{account.pw_name}/{climb}{inside}/FAKE-named.json",
                    f"{account.pw_dir.rstrip('/')}/{climb}{inside}/FAKE-named.json",
                ),
                "home-unset": (
                    unset,
                    f"~/{climb}{inside}/FAKE-unset.json",
                    f"{account.pw_dir.rstrip('/')}/{climb}{inside}/FAKE-unset.json",
                ),
                "home-empty": (
                    dict(self.env, HOME=""),
                    f"~/{inside}/FAKE-empty.json",
                    f"/{inside}/FAKE-empty.json",
                ),
                "home-relative": (
                    dict(self.env, HOME="relative-home"),
                    f"~/FAKE-relative-{int(engine)}.json",
                    f"{self.root}/relative-home/FAKE-relative-{int(engine)}.json",
                ),
            }
            old = None
            for name, (env, argument, expected) in cases.items():
                for operation in ("token-create", "token-rotate"):
                    with self.subTest(engine=engine, case=name, operation=operation):
                        target = Path(expected)
                        if operation == "token-rotate":
                            argument = argument.replace(".json", "-rotated.json")
                            target = Path(expected.replace(".json", "-rotated.json"))
                        extra = ("--grant", "fake-grant") if old is None else ()
                        result = self.run_with_env(
                            env,
                            engine,
                            ["api", operation, old or "fixture", "--output", argument]
                            + list(extra),
                        )
                        self.assertEqual(
                            json.loads(result.stdout)["data"]["credential_file"],
                            expected
                            if operation == "token-create"
                            else expected.replace(".json", "-rotated.json"),
                        )
                        old = self.verify_saved(engine, result, target)
                old = None

    def test_unexpandable_home_raises_runtime_error_after_issue(self):
        missing = "~catabolic-no-such-user-fixture/FAKE.json"
        cases = (
            ("unknown-user", self.env, missing),
            ("tilde-home", dict(self.env, HOME="~fixture"), "~/FAKE.json"),
        )
        snapshots = {True: [], False: []}
        for engine in (True, False):
            self.prepare(engine)
            first = self.root / f"FAKE-first-{int(engine)}.json"
            old = self.verify_saved(
                engine,
                self.invoke(
                    engine, "token-create", "fixture", first, "--grant", "fake-grant"
                ),
                first,
            )
            for name, env, argument in cases:
                for operation, subject, extra in (
                    ("token-create", "fixture", ("--grant", "fake-grant")),
                    ("token-rotate", old, ()),
                ):
                    for prefix in (["--machine"], ["--json"], []):
                        with self.subTest(
                            engine=engine, case=name, operation=operation, prefix=prefix
                        ):
                            result = self.run_with_env(
                                env,
                                engine,
                                ["api", operation, subject, "--output", argument]
                                + list(extra),
                                prefix=prefix,
                            )
                            self.assertEqual(result.returncode, 1, result.stderr)
                            message = "Could not determine home directory."
                            if engine:
                                # Frozen Python leaves pathlib's exception
                                # unhandled; its traceback text is the class
                                # and message the native envelope preserves.
                                self.assertEqual(result.stdout, "")
                                self.assertTrue(
                                    result.stderr.endswith(
                                        f"RuntimeError: {message}\n"
                                    ),
                                    result.stderr,
                                )
                            elif prefix == ["--machine"]:
                                envelope = json.loads(result.stdout)
                                self.assertEqual(result.stderr, "")
                                self.assertEqual(envelope["exit_code"], 1)
                                self.assertEqual(envelope["outcome"], "error")
                                self.assertEqual(
                                    envelope["data"],
                                    {
                                        "error": {
                                            "type": "RuntimeError",
                                            "message": message,
                                        }
                                    },
                                )
                            elif prefix == ["--json"]:
                                self.assertEqual(result.stdout, "")
                                self.assertEqual(
                                    json.loads(result.stderr),
                                    {
                                        "error": {
                                            "type": "RuntimeError",
                                            "message": message,
                                        }
                                    },
                                )
                            else:
                                self.assertEqual(result.stdout, "")
                                self.assertEqual(result.stderr, f"error: {message}\n")
                            with closing(sqlite3.connect(self.paths[engine])) as db:
                                snapshots[engine].append(
                                    {
                                        "tokens": db.execute(
                                            "SELECT principal,grants,revoked FROM api_tokens ORDER BY rowid"
                                        ).fetchall(),
                                        "private": db.execute(
                                            "SELECT count(*) FROM api_private_files"
                                        ).fetchone()[0],
                                        "audit": db.execute(
                                            "SELECT action FROM api_audit ORDER BY id"
                                        ).fetchall(),
                                    }
                                )
            self.assertFalse((self.root / "~catabolic-no-such-user-fixture").exists())
            self.assertFalse((self.root / "~").exists())
        self.assertEqual(snapshots[True], snapshots[False])
        # Each refusal follows token issuance and never revokes the old token.
        self.assertEqual(len(snapshots[True][-1]["tokens"]), 2 + 1 + 12)
        self.assertTrue(all(row[2] == 0 for row in snapshots[True][-1]["tokens"]))
        self.assertEqual(snapshots[True][-1]["private"], 1)

    def test_unknown_rotation_and_invalid_configuration_have_no_file(self):
        destination = self.root / "FAKE-never-written.json"
        for args in (
            ["api", "token-rotate", "missing", "--output", str(destination)],
            [
                "api",
                "token-create",
                "fixture",
                "--grant",
                "missing",
                "--ttl",
                "0",
                "--output",
                str(destination),
            ],
        ):
            self.compare(args)
            self.assertFalse(destination.exists())

    def test_revoke_failure_keeps_new_file_and_old_token(self):
        snapshots = []
        for engine in (True, False):
            folder = self.prepare(engine)
            folder.mkdir()
            oldfile = folder / "FAKE-old.json"
            old = self.verify_saved(
                engine,
                self.invoke(
                    engine, "token-create", "fixture", oldfile, "--grant", "fake-grant"
                ),
                oldfile,
            )
            with closing(sqlite3.connect(self.paths[engine])) as db:
                db.execute(
                    "UPDATE sqlite_sequence SET seq=9223372036854775806 WHERE name='api_audit'"
                )
                db.commit()
            output = folder / "FAKE-revoke-failed.json"
            failed = self.invoke(engine, "token-rotate", old, output)
            self.assertNotEqual(failed.returncode, 0)
            self.assertIn("database or disk is full", failed.stdout + failed.stderr)
            new = json.loads(output.read_bytes())
            self.assertNotIn(new["token"], failed.stdout + failed.stderr)
            with closing(sqlite3.connect(self.paths[engine])) as db:
                snapshots.append(
                    {
                        "old_revoked": db.execute(
                            "SELECT revoked FROM api_tokens WHERE id=?", (old,)
                        ).fetchone()[0],
                        "new_revoked": db.execute(
                            "SELECT revoked FROM api_tokens WHERE id=?",
                            (new["token_id"],),
                        ).fetchone()[0],
                        "private_count": db.execute(
                            "SELECT count(*) FROM api_private_files"
                        ).fetchone()[0],
                        "last_audit": db.execute(
                            "SELECT id,action FROM api_audit ORDER BY id DESC LIMIT 1"
                        ).fetchone(),
                    }
                )
        self.assertEqual(snapshots[0], snapshots[1])
        self.assertEqual(snapshots[0]["old_revoked"], 0)
        self.assertEqual(snapshots[0]["new_revoked"], 0)
        self.assertEqual(snapshots[0]["private_count"], 2)
