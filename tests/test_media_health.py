# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import os
import unittest
from unittest.mock import patch

from catabolic.curation import occurrence
from catabolic.domain import CatabolicError
from catabolic.fallback_probe import check as fallback_check
from catabolic.media_health import check_at, inspect_header
from catabolic.processing import Processing, current_fact
from catabolic.sql_query import execute_sql
from tests import test_application as fixtures


class MediaHeaderTest(unittest.TestCase):
    setUp = fixtures.CatalogTest.setUp

    def test_scan_records_corruption_without_claiming_absence(self):
        self.media.write_bytes(bytes(4096))
        report = self.app.scan("media")
        self.assertTrue(report["complete"], report)
        checks = report["scans"][0]["media_headers"]
        self.assertEqual(checks["counts"], {"invalid": 1})
        self.assertEqual(checks["invalid"][0]["reason"], "zero_filled_header")
        row = self.app.files()["files"][0]
        self.assertEqual(row["status"], "present")
        self.assertEqual(row["media_header_status"], "invalid")
        result = self.reconciler.apply()
        self.assertIn("zero_filled_header", str(result))
        self.assertFalse(result["healthy"])
        self.assertFalse(self.link.exists())

    def test_existing_link_verification_rechecks_bytes_without_a_rescan(self):
        self.reconciler.apply()
        before = self.media.stat()
        self.media.write_bytes(bytes(before.st_size))
        os.utime(self.media, ns=(before.st_atime_ns, before.st_mtime_ns))
        result = self.reconciler.verify()
        self.assertFalse(result["healthy"], result)
        self.assertIn("zero_filled_header", str(result))

    def test_repair_clears_invalid_and_missing_makes_evidence_stale(self):
        self.media.write_bytes(bytes(64))
        self.app.scan()
        self.media.write_bytes(b"\x1aE\xdf\xa3" + bytes(60))
        self.app.scan()
        row = self.app.files()["files"][0]
        self.assertEqual(row["media_header_status"], "not_detected")
        self.assertIsNone(row["media_header_reason"])
        self.media.unlink()
        self.app.scan()
        row = self.app.files()["files"][0]
        self.assertEqual(row["status"], "missing")
        self.assertEqual(row["media_header_status"], "unknown")
        rows = execute_sql(
            self.database,
            "SELECT current FROM catalog_media_headers",
            _store=self.store,
        )["rows"]
        self.assertEqual(rows[0][0], 0)

    def test_header_read_is_bounded_and_raw_formats_are_unknown(self):
        self.media.write_bytes(b"\x1aE\xdf\xa3" + bytes(4096))
        with self.media.open("rb") as stream:
            result = inspect_header(stream.fileno(), "video.MKV", 4100)
            self.assertEqual(result["bytes_read"], 64)
            self.assertEqual(result["status"], "not_detected")
            with patch("catabolic.media_health.os.pread") as read:
                result = inspect_header(stream.fileno(), "disk.img", 4100)
                self.assertEqual(result["status"], "unknown")
                read.assert_not_called()

    def test_replacement_during_header_read_is_rejected(self):
        expected = self.media.stat()
        pread = os.pread

        def replace(fd, count, offset):
            result = pread(fd, count, offset)
            self.media.unlink()
            self.media.write_bytes(bytes(expected.st_size))
            return result

        parent = os.open(self.source, os.O_RDONLY)
        try:
            with patch("catabolic.media_health.os.pread", replace):
                with self.assertRaisesRegex(CatabolicError, "source changed"):
                    check_at(parent, self.media.name, self.media.name, expected)
        finally:
            os.close(parent)

    def test_header_read_failure_is_unknown_and_scan_incomplete(self):
        with patch(
            "catabolic.media_health.os.pread", side_effect=PermissionError("denied")
        ):
            report = self.app.scan()
        self.assertFalse(report["complete"], report)
        row = self.app.files()["files"][0]
        self.assertEqual(row["status"], "present")
        self.assertEqual(row["media_header_status"], "unknown")

    def test_corruption_invalidates_cached_facts_even_with_same_stat_metadata(self):
        processing = Processing(self.app)
        processing.enqueue("sniff", file_ids=[self.file_id])
        self.assertTrue(processing.run()["complete"])
        self.assertTrue(
            current_fact(self.store, "default", self.file_id, "sniff")["current"]
        )
        before = self.media.stat()
        self.media.write_bytes(bytes(before.st_size))
        os.utime(self.media, ns=(before.st_atime_ns, before.st_mtime_ns))
        self.app.scan()
        fact = current_fact(self.store, "default", self.file_id, "sniff")
        self.assertEqual(fact["status"], "invalidated")
        self.assertFalse(fact["current"])

    def test_fallback_probe_rejects_corrupt_container(self):
        self.media.write_bytes(bytes(64))
        self.app.scan()
        snapshot = occurrence(self.store, "default", self.file_id)
        result = fallback_check([snapshot])
        self.assertEqual(result, {"usable": False, "reason": "zero_filled_header"})

    def test_empty_file_is_invalid_without_requiring_read_permission(self):
        self.media.write_bytes(b"")
        with patch(
            "catabolic.media_health.os.open", side_effect=PermissionError("denied")
        ):
            result = check_at(-1, self.media.name, self.media.name, self.media.stat())
        self.assertEqual(
            result, {"status": "invalid", "reason": "empty", "bytes_read": 0}
        )

    def test_verification_rejects_changed_revision_before_reading_header(self):
        self.reconciler.apply()
        self.media.write_bytes(b"new file revision")
        with patch("catabolic.media_health.os.pread") as read:
            result = self.reconciler.verify()
        read.assert_not_called()
        self.assertFalse(result["healthy"])
        self.assertIn("source changed since scan", str(result))

    def test_invalidated_completed_analysis_is_requeued(self):
        self.media.write_bytes(bytes(64))
        self.app.scan()
        processing = Processing(self.app)
        job = processing.enqueue("sniff", file_ids=[self.file_id])["queued"][0]
        self.assertTrue(processing.run()["complete"])
        self.assertEqual(
            processing.enqueue("sniff", file_ids=[self.file_id])["cached"], [job]
        )
        self.app.scan()
        result = processing.enqueue("sniff", file_ids=[self.file_id])
        self.assertEqual(result["cached"], [])
        self.assertEqual(len(result["queued"]), 1)
        self.assertNotEqual(result["queued"][0], job)


if __name__ == "__main__":
    unittest.main()
