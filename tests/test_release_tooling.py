# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Recovery compatibility and fail-closed release dependency checks."""

import io
import json
import sqlite3
import tempfile
import unittest
from contextlib import closing
from pathlib import Path
from unittest.mock import patch

from catabolic.app import Application
from catabolic.migration import load_migrations, upgrade_database
from catabolic.reconcile import Reconciler
from catabolic.store import RECOVERABLE_SCHEMAS, Store
from scripts.audit_dependencies import audit
from tests.test_migrations import create_legacy


class RecoveryCompatibilityTest(unittest.TestCase):
    def test_recovery_of_pending_symlink_at_each_supported_schema(self):
        migrations = load_migrations()
        for version in sorted(RECOVERABLE_SCHEMAS):
            with (
                self.subTest(version=version),
                tempfile.TemporaryDirectory() as temporary,
            ):
                root = Path(temporary).resolve()
                path = create_legacy(root)
                if version > 1:
                    upgrade_database(path, migrations=migrations[:version])
                link = root / "plex/Movies/Test.mkv"
                target = str(link.readlink())
                link.unlink()
                with closing(sqlite3.connect(path)) as db:
                    db.execute(
                        "DELETE FROM owned_links WHERE profile='default' AND catalog='global' AND path='Movies/Test.mkv'"
                    )
                    db.execute(
                        "INSERT INTO journal VALUES ('pending','default','global','Movies/Test.mkv','create',?,NULL)",
                        (target,),
                    )
                    db.commit()
                with Store(path, writable=True, for_recovery=True) as store:
                    result = Reconciler(Application(store)).recover()
                self.assertEqual(result["recovered"], ["pending"])
                self.assertEqual(str(link.readlink()), target)


class DependencyAuditTest(unittest.TestCase):
    def response(self, request, **kwargs):
        count = len(json.loads(request.data)["queries"])
        return io.BytesIO(json.dumps({"results": [{} for _ in range(count)]}).encode())

    def test_clean_response_covers_all_pins(self):
        with patch("urllib.request.urlopen", side_effect=self.response):
            result = audit()
        self.assertTrue(result["safe"])
        self.assertEqual(len(result["packages"]), 16)

    def test_advisories_and_incomplete_responses_do_not_pass(self):
        def vulnerable(request, **kwargs):
            result = json.load(self.response(request))
            result["results"][0] = {"vulns": [{"id": "fixture-advisory"}]}
            return io.BytesIO(json.dumps(result).encode())

        with patch("urllib.request.urlopen", side_effect=vulnerable):
            self.assertFalse(audit()["safe"])
        with (
            patch("urllib.request.urlopen", return_value=io.BytesIO(b'{"results":[]}')),
            self.assertRaises(ValueError),
        ):
            audit()
