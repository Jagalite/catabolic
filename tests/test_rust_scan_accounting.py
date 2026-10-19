# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Ordinary disposable scan byte/record boundaries against the frozen owner.

Recompose expected batches from the real durable file/scope payloads using the
frozen ordinary schedule. Reports and batch counters are never replaced.
"""

import json
import sqlite3
import unittest
from contextlib import closing

from tests import test_rust_maintenance_cli as maintenance


def encoded_bytes(value):
    return len(json.dumps(value, sort_keys=True, ensure_ascii=False).encode())


class ScanAccountingCliTest(unittest.TestCase):
    maxDiff = None
    setUp = maintenance.NativeMaintenanceCliTest.setUp
    setup = maintenance.NativeMaintenanceCliTest.setup
    run_cli = maintenance.NativeMaintenanceCliTest.run_cli

    def inspect(self, path, report):
        evidence = report["observations"][0]
        scan = report["scans"][0]
        self.assertTrue(report["complete"])
        self.assertEqual(evidence["report"], scan)
        with closing(sqlite3.connect(path)) as db:
            job = evidence["id"]
            batches = db.execute(
                "SELECT sequence,observed,bytes FROM observation_batches WHERE job_id=? ORDER BY sequence",
                (job,),
            ).fetchall()
            scope_rows = {
                name: (state, json.loads(detail))
                for name, state, detail in db.execute(
                    "SELECT path,state,detail FROM observation_scopes WHERE job_id=?",
                    (job,),
                )
            }
            entries = {}
            for name, size, mtime, device, inode, status, reason, read in db.execute(
                "SELECT f.path,o.size,o.mtime_ns,o.device,o.inode,h.status,h.reason,h.bytes_read "
                "FROM observations o JOIN files f ON f.id=o.file_id "
                "JOIN media_header_checks h ON h.file_id=f.id AND h.profile=o.profile WHERE o.scan_id=?",
                (scan["scan_id"],),
            ):
                entries[name] = dict(
                    path=name,
                    size=size,
                    mtime_ns=mtime,
                    device=device,
                    inode=inode,
                    media_header=dict(status=status, reason=reason, bytes_read=read),
                )
            limits = scan["budgets"]
            # Keep wall-clock flushes out of this bounded byte/record matrix.
            self.assertEqual(limits["batch_seconds"], 120)
            self.assertLess(scan["progress"]["elapsed_seconds"], 120)
            expected, scopes, entry_bytes, scope_bytes = [], [], {}, {}
            buffered_scopes, buffered_entries = {}, []
            size = 0

            def flush():
                nonlocal size
                sequence = len(expected) + 1
                expected.append((sequence, len(buffered_entries), size))
                entry_bytes[sequence] = sum(encoded_bytes(e) for e in buffered_entries)
                scope_bytes[sequence] = size - entry_bytes[sequence]
                buffered_entries.clear()
                buffered_scopes.clear()
                size = 0

            def checkpoint():
                if (
                    len(buffered_entries) + len(buffered_scopes)
                    >= limits["batch_records"]
                    or size >= limits["batch_bytes"]
                ):
                    flush()

            def directory(name, state, detail):
                nonlocal size
                scopes.append((len(expected) + 1, name, state, detail))
                buffered_scopes[name] = (state, detail)
                size += len(name.encode()) + encoded_bytes(detail)
                if state == "running":
                    flush()
                else:
                    checkpoint()

            queue = [""]
            for name in queue:
                state, detail = scope_rows[name]
                self.assertEqual(state, "complete")
                directory(
                    name,
                    "running",
                    {k: detail[k] for k in ("identity", "mtime_ns", "ctime_ns")},
                )
                for child in (self.source / name).iterdir():
                    relative = child.relative_to(self.source).as_posix()
                    if scope_rows.get(relative, (None,))[0] == "excluded":
                        directory(relative, *scope_rows[relative])
                    elif child.is_dir():
                        directory(
                            relative,
                            "pending",
                            {"identity": scope_rows[relative][1]["identity"]},
                        )
                        queue.append(relative)
                    elif relative in entries:
                        buffered_entries.append(entries[relative])
                        size += encoded_bytes(entries[relative])
                        checkpoint()
                directory(name, state, detail)
                flush()
            flush()
            self.assertEqual(batches, expected)
            self.assertEqual(scan["batches"], len(batches))
            self.assertEqual(scan["progress"]["batches"], len(batches))
            self.assertEqual(
                scan["progress"]["committed_bytes"], sum(row[2] for row in batches)
            )
            self.assertEqual(scan["progress"]["buffered_bytes"], 0)
            self.assertEqual(
                json.loads(
                    db.execute(
                        "SELECT report FROM observation_jobs WHERE id=?", (job,)
                    ).fetchone()[0]
                ),
                scan,
            )
            for table in ("journal", "mappings", "processing_jobs", "watch_revisions"):
                self.assertEqual(
                    db.execute(f"SELECT count(*) FROM {table}").fetchone()[0], 0
                )
        self.assertEqual(batches[-1][1:], (0, 0))
        return batches, scopes, entry_bytes, scope_bytes

    def pair(self, budgets=None, extra=()):
        budget_file = self.root / "budgets.json"
        budget_file.write_text(json.dumps({"batch_seconds": 120, **(budgets or {})}))
        before = {
            p.relative_to(self.source): p.read_bytes()
            for p in self.source.rglob("*")
            if p.is_file()
        }
        values = []
        for reference, path in ((True, self.ref), (False, self.native)):
            result = self.run_cli(
                reference, ["--json", "scan", "--budgets", str(budget_file), *extra]
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stderr, "")
            report = json.loads(result.stdout)
            values.append(self.inspect(path, report))
            evidence = report["observations"][0]
            if reference:
                self.assertEqual(
                    evidence["guarantees"]["minimum_start"],
                    evidence["guarantees"]["after"],
                )
            else:
                self.assertEqual(
                    evidence["guarantees"]["minimum_start"], evidence["started_at"]
                )
                self.assertGreater(
                    evidence["guarantees"]["minimum_start"],
                    evidence["guarantees"]["after"],
                )
        self.assertEqual(
            before,
            {
                p.relative_to(self.source): p.read_bytes()
                for p in self.source.rglob("*")
                if p.is_file()
            },
        )
        left, right = values
        self.assertEqual([row[:2] for row in left[0]], [row[:2] for row in right[0]])
        self.assertEqual(left[2], right[2])
        self.assertEqual([row[:3] for row in left[1]], [row[:3] for row in right[1]])
        for a, b in zip(left[1], right[1], strict=True):
            self.assertEqual(set(a[3]), set(b[3]))
            for key in a[3]:
                if key == "seconds":
                    self.assertGreaterEqual(a[3][key], 0)
                    self.assertGreaterEqual(b[3][key], 0)
                else:
                    self.assertEqual(a[3][key], b[3][key], (a[:3], key))
        # Real elapsed times remain in the payloads. Their encoded sizes can
        # differ, and the actual batch byte difference must be exactly that.
        for a, b in zip(left[0], right[0], strict=True):
            self.assertEqual(a[2] - b[2], left[3].get(a[0], 0) - right[3].get(b[0], 0))
        return values

    def test_empty_bound_source_has_three_actual_batches(self):
        self.setup(files=0)
        values = self.pair()
        self.assertEqual([row[1] for row in values[0][0]], [0, 0, 0])

    def test_one_file_scope_bytes_and_final_empty_batch(self):
        self.setup()
        values = self.pair()
        self.assertEqual([row[1] for row in values[0][0]], [0, 1, 0])
        self.assertGreater(
            sum(row[2] for row in values[1][0]), sum(values[1][2].values())
        )

    def test_nested_unicode_and_excluded_scopes_are_accounted(self):
        self.setup(files=0)
        child = self.source / "dír-猫"
        child.mkdir()
        (child / "entrée.txt").write_text("ordinary unicode fixture")
        (self.source / "skip").mkdir()
        (self.source / "skip/hidden.txt").write_text("excluded fixture")
        values = self.pair(extra=("--exclude", "skip"))
        self.assertEqual([row[1] for row in values[0][0]], [0, 0, 0, 1, 0])

    def test_filtered_entries_do_not_consume_record_or_byte_budget(self):
        self.setup(files=0)
        (self.source / "ignored.unrecognized-extension").write_text("filtered fixture")
        values = self.pair({"batch_records": 2})
        self.assertEqual([row[1] for row in values[0][0]], [0, 0, 0])

    def test_entry_and_scope_share_record_budget(self):
        for limit, count in ((2, 4), (3, 4), (4, 3)):
            with self.subTest(limit=limit):
                self.doCleanups()
                self.setUp()
                self.setup()
                (self.source / "skip").mkdir()
                values = self.pair(
                    {"batch_records": limit}, extra=("--exclude", "skip")
                )
                self.assertEqual(len(values[0][0]), count)
                self.assertEqual(sum(row[1] for row in values[0][0]), 1)

    def test_total_entry_stop_keeps_only_committed_batches(self):
        self.setup(files=2)
        budget_file = self.root / "budgets.json"
        budget_file.write_text(
            json.dumps({"total_entries": 1, "batch_records": 1, "batch_seconds": 120})
        )
        ledgers = []
        for reference, path in ((True, self.ref), (False, self.native)):
            result = self.run_cli(
                reference, ["--json", "scan", "--budgets", str(budget_file)]
            )
            self.assertEqual(result.returncode, 3, result.stderr)
            report = json.loads(result.stdout)
            self.assertFalse(report["complete"])
            scan, evidence = report["scans"][0], report["observations"][0]
            self.assertEqual(scan["blocker"], "total_entry_budget")
            self.assertEqual(scan["progress"]["buffered_bytes"], 0)
            with closing(sqlite3.connect(path)) as db:
                batches = db.execute(
                    "SELECT sequence,observed,bytes FROM observation_batches WHERE job_id=? ORDER BY sequence",
                    (evidence["id"],),
                ).fetchall()
                self.assertEqual([row[1] for row in batches], [0, 1])
                self.assertTrue(all(row[2] > 0 for row in batches))
                self.assertEqual(scan["batches"], 2)
                self.assertEqual(
                    scan["progress"]["committed_bytes"], sum(row[2] for row in batches)
                )
                self.assertEqual(
                    db.execute(
                        "SELECT state FROM observation_scopes WHERE job_id=? AND path=''",
                        (evidence["id"],),
                    ).fetchone()[0],
                    "running",
                )
            ledgers.append(batches)
        self.assertEqual(*ledgers)

    def test_scope_record_boundary_is_inclusive(self):
        for limit, count in ((1, 5), (2, 4), (3, 3)):
            with self.subTest(limit=limit):
                self.doCleanups()
                self.setUp()
                self.setup(files=0)
                (self.source / "skip").mkdir()
                values = self.pair(
                    {"batch_records": limit}, extra=("--exclude", "skip")
                )
                self.assertEqual(len(values[0][0]), count)

    def test_scope_byte_boundary_uses_utf8_and_is_inclusive(self):
        name = "sauté-猫"
        threshold = len(name.encode()) + encoded_bytes(
            {"reason": "configured_exclusion"}
        )
        for limit, count in ((threshold - 1, 5), (threshold, 5), (threshold + 1, 4)):
            with self.subTest(limit=limit):
                self.doCleanups()
                self.setUp()
                self.setup(files=0)
                (self.source / name).mkdir()
                values = self.pair({"batch_bytes": limit}, extra=("--exclude", name))
                self.assertEqual(len(values[0][0]), count)
                if limit <= threshold:
                    self.assertEqual(values[0][0][1], (2, 0, threshold))
                    self.assertEqual(values[1][0][1], (2, 0, threshold))


if __name__ == "__main__":
    unittest.main()
