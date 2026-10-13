# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Assert report facts without normalizing native owner differences away."""

import json
import sqlite3
from contextlib import closing


def assert_completed_report_facts(case, oracle, native):
    """One ordinary scan; check actual batches and retain the freshness gap."""
    left, right = oracle["scans"][0], native["scans"][0]
    a, b = oracle["observations"][0], native["observations"][0]
    case.assertEqual(set(left), set(right))
    case.assertEqual(set(a), set(b))
    case.assertEqual(left["media_headers"], right["media_headers"])
    case.assertEqual(left["suggested_actions"], right["suggested_actions"])
    for key in ("coverage", "reuse", "retry_state"):
        case.assertEqual(a[key], b[key])
    case.assertEqual(b["reuse"], "created")
    case.assertIsNone(b["retry_state"])
    case.assertEqual(set(left["progress"]), set(right["progress"]))
    for key in ("buffered_bytes", "directory_records", "entries_visited"):
        case.assertEqual(left["progress"][key], right["progress"][key])
    case.assertEqual((left["batches"], right["batches"]), (3, 3))
    case.assertEqual(
        (left["progress"]["batches"], right["progress"]["batches"]), (3, 3)
    )
    for path, evidence, report in ((case.ref, a, left), (case.native, b, right)):
        case.assertEqual(evidence["report"], report)
        case.assertGreater(evidence["guarantees"]["after"], 0)
        case.assertLessEqual(evidence["guarantees"]["after"], evidence["started_at"])
        with closing(sqlite3.connect(path)) as db:
            durable = db.execute(
                "SELECT report FROM observation_jobs WHERE id=?", (evidence["id"],)
            ).fetchone()[0]
            case.assertEqual(json.loads(durable), report)
            progress = db.execute(
                "SELECT count(*),sum(bytes),max(committed_at) FROM observation_batches WHERE job_id=?",
                (evidence["id"],),
            ).fetchone()
            case.assertEqual(
                tuple(
                    report["progress"][k]
                    for k in ("batches", "committed_bytes", "last_progress_at")
                ),
                progress,
            )
            # Actual scope payload bytes include elapsed seconds, so exact
            # cross-run totals need not match. Recompose each real ledger.
            batches = db.execute(
                "SELECT observed,bytes FROM observation_batches WHERE job_id=? ORDER BY sequence",
                (evidence["id"],),
            ).fetchall()
            detail = json.loads(
                db.execute(
                    "SELECT detail FROM observation_scopes WHERE job_id=? AND path=''",
                    (evidence["id"],),
                ).fetchone()[0]
            )

            def encoded(value):
                return len(
                    json.dumps(value, sort_keys=True, ensure_ascii=False).encode()
                )

            running = {k: detail[k] for k in ("identity", "mtime_ns", "ctime_ns")}
            entries = db.execute(
                "SELECT f.path,o.size,o.mtime_ns,o.device,o.inode,h.status,h.reason,h.bytes_read "
                "FROM observations o JOIN files f ON f.id=o.file_id JOIN media_header_checks h "
                "ON h.file_id=f.id AND h.profile=o.profile WHERE o.scan_id=?",
                (report["scan_id"],),
            ).fetchall()
            entry_bytes = sum(
                encoded(
                    dict(
                        path=name,
                        size=size,
                        mtime_ns=mtime,
                        device=device,
                        inode=inode,
                        media_header=dict(
                            status=status, reason=reason, bytes_read=read
                        ),
                    )
                )
                for name, size, mtime, device, inode, status, reason, read in entries
            )
            case.assertEqual(
                batches,
                [
                    (0, encoded(running)),
                    (len(entries), entry_bytes + encoded(detail)),
                    (0, 0),
                ],
            )
            guarantee = db.execute(
                "SELECT guarantees FROM observation_requests WHERE id=?",
                (evidence["request_id"],),
            ).fetchone()[0]
            case.assertEqual(json.loads(guarantee), evidence["guarantees"])
        case.assertGreaterEqual(report["progress"]["elapsed_seconds"], 0)
        case.assertGreater(report["progress"]["temporary_bytes"], 0)
        case.assertGreater(report["progress"]["temporary_free_bytes"], 0)
    # The native owner still has its stricter admission-time freshness floor.
    # Recording the public request boundary must not silently relax that policy.
    case.assertEqual(a["guarantees"]["minimum_start"], a["guarantees"]["after"])
    case.assertEqual(b["guarantees"]["minimum_start"], b["started_at"])
    case.assertGreater(b["guarantees"]["minimum_start"], b["guarantees"]["after"])
