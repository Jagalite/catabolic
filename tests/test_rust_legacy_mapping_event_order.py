# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Exact frozen/native mapping-event bytes from disposable catalog subjects.

No normalization, service, credentials, real destination or mutation owner.
Scalar values and native malformed-input rules stay under the existing owner.
"""

import json
import os
import random
import shutil
import signal
import sqlite3
import subprocess
import time
import unittest
from contextlib import closing
from pathlib import Path

import tests.test_rust_legacy_mapping_history as history


class NativeLegacyMappingEventOrderTest(unittest.TestCase):
    setUp = history.NativeLegacyMappingHistoryTest.setUp
    command = history.NativeLegacyMappingHistoryTest.command
    run_cli = history.NativeLegacyMappingHistoryTest.run_cli
    snapshot = history.NativeLegacyMappingHistoryTest.snapshot
    seed = history.NativeLegacyMappingHistoryTest.seed
    assert_unchanged = history.NativeLegacyMappingHistoryTest.assert_unchanged

    def subject(self, raw, identifier="default-publication-0001"):
        for flag in (True, False):
            with closing(sqlite3.connect(self.db[flag])) as db:
                db.execute(
                    "UPDATE consumer_events SET subject=? WHERE id=?", (raw, identifier)
                )
                db.commit()

    def exact(self, args, code=0):
        results = [
            subprocess.run(
                self.command(flag, args),
                env=self.env,
                capture_output=True,
                timeout=30,
            )
            for flag in (True, False)
        ]
        old, new = results
        self.assertEqual(old.returncode, code, (old.stdout, old.stderr))
        self.assertEqual(
            (new.returncode, new.stdout, new.stderr),
            (old.returncode, old.stdout, old.stderr),
        )
        return old, new

    def exact_event(self, identifier="default-publication-0001", code=0):
        for mode in ([], ["--json"], ["--machine"]):
            with self.subTest(mode=mode):
                self.exact(
                    mode + ["projection", "mapping-events", "--event", identifier], code
                )

    def test_nested_permutations_report_like_objects_and_scalar_values(self):
        self.seed(http=1, publication=1)
        original = {
            "version": 1,
            "owner": "custom",
            "key": "k",
            "state": "uncertain",
            "plan_id": "p",
            "before": {
                "type": "KeyboardInterrupt",
                "message": "opaque",
                "next_cursor": None,
                "definition": {"z": False, "a": True, "revision": 7},
                "stage": "preflight",
            },
            "after": [
                {"z": 2**130, "a": -(2**130), "empty": {}, "list": []},
                {"id": "nested", "state": "old", "é雪😀": [None, True, False]},
                {'z"\\\n': 1.0, "a": -0.0, "small": 5e-324, "float": 1e-7},
                {"$serde_json::private::Number": "123", "a": "literal"},
                [],
                {},
            ],
        }

        def permute(value, rng):
            if isinstance(value, dict):
                fields = list(value.items())
                rng.shuffle(fields)
                return {key: permute(child, rng) for key, child in fields}
            if isinstance(value, list):
                return [permute(child, rng) for child in value]
            return value

        for seed in range(8):
            with self.subTest(seed=seed):
                raw = json.dumps(
                    permute(original, random.Random(seed)), ensure_ascii=bool(seed % 2)
                )
                self.subject(raw)
                before = {flag: self.snapshot(flag) for flag in (True, False)}
                self.exact_event()
                self.assert_unchanged(before)

    def test_duplicate_keys_keep_first_position_and_last_value(self):
        self.seed(http=0, publication=1)
        cases = [
            r'{"z":0,"a":1,"z":{"later":2,"early":3}}',
            r'{"z":{"later":2,"early":3},"a":1,"\u007a":0}',
            r'{"z":{"b":0,"a":1,"\u0062":[{"z":2,"a":3}]},"tail":null}',
            r'{"z":[{"$serde_json::private::Number":"123"},{"z":0,"a":1,"z":2}]}',
        ]
        for raw in cases:
            with self.subTest(raw=raw):
                self.subject(raw)
                before = {flag: self.snapshot(flag) for flag in (True, False)}
                self.exact_event()
                self.assert_unchanged(before)

    def test_http_shaped_publication_uses_actual_source_and_first_aggregate_match(self):
        self.seed(http=1, publication=0, collision=True)
        # Exactly eight result keys mimic a joined HTTP row. This remains an
        # opaque publication subject, and is selected ahead of its HTTP duplicate.
        raw = json.dumps(
            {
                "version": 3,
                "error": {"z": 1, "a": 2},
                "attempts": 2,
                "state": "verified",
                "plan_id": "p",
                "key": "k",
                "owner": "collision",
            }
        )
        self.subject(raw, "default-http-0001")
        before = {flag: self.snapshot(flag) for flag in (True, False)}
        self.exact_event("default-http-0001")
        for mode in ([], ["--json"], ["--machine"]):
            self.exact(mode + ["projection", "mapping-events"])
            self.exact(mode + ["projection", "mapping-recover", "default-http-0001"], 2)
        self.assert_unchanged(before)

    def test_exit_code_and_empty_containers_follow_authoritative_value(self):
        self.seed(http=0, publication=1)
        for raw, code in [
            ("{}", 0),
            ('{"complete":false,"z":[],"a":{}}', 3),
            ('{"healthy":false,"safe":true,"z":{}}', 3),
        ]:
            with self.subTest(raw=raw):
                self.subject(raw)
                before = {flag: self.snapshot(flag) for flag in (True, False)}
                self.exact_event(code=code)
                self.assert_unchanged(before)

    def test_numeric_token_normalization_uses_owner_values(self):
        self.seed(http=0, publication=1)
        for token in ["1.00", "1e0", "-0", "-0.000", "1e-7", "5e-324", "1e+30"]:
            with self.subTest(token=token):
                self.subject('{"z":' + token + ',"a":1}')
                before = {flag: self.snapshot(flag) for flag in (True, False)}
                self.exact_event()
                self.assert_unchanged(before)

    @unittest.skipUnless(
        Path("/proc/self/fd").is_dir(), "requires Linux process descriptors"
    )
    def test_real_sigint_discards_selected_sidecar_and_releases_snapshot(self):
        self.seed(http=0, publication=1)
        with closing(sqlite3.connect(self.db[True])) as db:
            db.execute(
                "WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<99999) "
                "INSERT INTO consumer_events(id,profile,event,severity,subject) "
                "SELECT 'slow-'||x,'default','mapping_publish','info',? FROM n",
                ('{"z":1,"a":2}',),
            )
            db.commit()
        shutil.copyfile(self.db[True], self.db[False])
        before = {flag: self.snapshot(flag) for flag in (True, False)}
        for mode in ([], ["--json"], ["--machine"]):
            outputs = []
            for flag in (True, False):
                process = subprocess.Popen(
                    self.command(
                        flag,
                        mode
                        + [
                            "projection",
                            "mapping-events",
                            "--event",
                            "default-publication-0001",
                        ],
                    ),
                    env=self.env,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                )
                self.addCleanup(
                    lambda p=process: p.kill() if p.poll() is None else None
                )
                deadline = time.monotonic() + 10
                opened = False
                while process.poll() is None and time.monotonic() < deadline:
                    for fd in Path(f"/proc/{process.pid}/fd").glob("*"):
                        try:
                            opened |= os.readlink(fd) == str(self.db[flag])
                        except FileNotFoundError:
                            pass
                    if opened:
                        break
                    time.sleep(0.001)
                self.assertTrue(opened, "catalog read was never observed")
                process.send_signal(signal.SIGINT)
                out, err = process.communicate(timeout=10)
                self.assertEqual(process.returncode, 130, (out, err))
                outputs.append((out, err))
                with closing(sqlite3.connect(self.db[flag], timeout=1)) as db:
                    db.execute("BEGIN EXCLUSIVE")
                    db.rollback()
            self.assertEqual(outputs[1], outputs[0])
        self.assert_unchanged(before)


if __name__ == "__main__":
    unittest.main()
