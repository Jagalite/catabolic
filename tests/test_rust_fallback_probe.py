# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Actual isolated native helper checks against frozen source-probe evidence."""

import json
import os
import subprocess
import unittest
from pathlib import Path

from catabolic.curation import occurrence
from catabolic.fallback_probe import check
from catabolic.fallback_resolution import Resolver
from tests import test_fallback_resolution as reference

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(
    os.environ.get("CATABOLIC_NATIVE_BINARY", ROOT / "target/debug/catabolic-native")
)


class NativeFallbackProbeTest(unittest.TestCase):
    setUp = reference.ResolverTest.setUp
    policy = reference.ResolverTest.policy

    def helper(self, snapshots, *, database_id=None, code=0):
        request = {
            "database": str(self.database),
            "database_id": database_id or self.store.database_id,
            "profile": "default",
            "snapshots": snapshots,
        }
        run = subprocess.run(
            [str(BINARY), "fallback-probe", "check"],
            input=json.dumps(request),
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertEqual(run.returncode, code, run.stdout + run.stderr)
        return json.loads(run.stdout) if code == 0 else run.stderr

    def test_actual_helper_accepts_current_source_and_refuses_stale_snapshot(self):
        snapshot = occurrence(self.store, "default", self.files["main.bin"])
        for snapshots in ([snapshot], [{**snapshot, "size": snapshot["size"] + 1}]):
            expected = check(snapshots, self.store, "default")
            self.assertEqual(self.helper(snapshots), expected)
        self.assertEqual(self.helper([snapshot])["usable"], True)
        (self.source / "main.bin").write_bytes(b"changed source contents")
        self.assertEqual(
            self.helper([snapshot]), check([snapshot], self.store, "default")
        )
        self.assertFalse(self.helper([snapshot])["usable"])

    def test_private_sqlite_magic_is_refused_with_current_file_evidence(self):
        (self.source / "main.bin").write_bytes(b"SQLite format 3\x00" + bytes(100))
        self.app.scan()
        snapshot = occurrence(self.store, "default", self.files["main.bin"])
        self.assertEqual(
            self.helper([snapshot]), check([snapshot], self.store, "default")
        )
        self.assertEqual(
            self.helper([snapshot]),
            {"usable": False, "reason": "source_unavailable_or_changed"},
        )

    def test_live_pids_hold_all_slots_across_native_resolution(self):
        policy = self.policy()
        with self.store.transaction() as db:
            db.execute(
                "UPDATE fallback_probe_slots SET owner='test-live',pid=?",
                (os.getpid(),),
            )
        before = self.store.rows("SELECT * FROM fallback_probe_slots ORDER BY id")
        expected = Resolver(self.app).resolve([{"item_id": self.item}], policy)
        with self.store.detached():
            run = subprocess.run(
                [
                    str(BINARY),
                    "--db",
                    str(self.database),
                    "--json",
                    "operation",
                    "fallback",
                    "resolve",
                    "--options",
                    json.dumps(
                        {"policy_id": policy, "entries": [{"item_id": self.item}]}
                    ),
                ],
                capture_output=True,
                text=True,
                timeout=10,
            )
        self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
        self.assertEqual(json.loads(run.stdout), expected)
        self.assertEqual(
            self.store.rows("SELECT * FROM fallback_probe_slots ORDER BY id"), before
        )
        self.assertEqual(expected["decisions"][0]["state"], "unresolved")

    def test_helper_refuses_replaced_catalog_identity(self):
        snapshot = occurrence(self.store, "default", self.files["main.bin"])
        error = self.helper([snapshot], database_id="different-catalog", code=2)
        self.assertIn("catalog replaced during fallback probe", error)
