# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import hashlib
import json
import shutil
import sys
import unittest
from types import SimpleNamespace
from unittest.mock import patch

from catabolic.domain import CatabolicError
from catabolic.fallback_probe import check
from catabolic.media_health import recorded_reason
from catabolic.probe_health import failure_health, probe_health
from catabolic.processing import ProcessingFailure, command_output, operation_config
from catabolic.reconcile import Reconciler
from catabolic.store import encode
from tests import test_enrichment


class ClassificationTest(unittest.TestCase):
    def test_failures_require_specific_corruption_evidence(self):
        for message, reason in (
            ("EBML header parsing failed", "container_parse_error"),
            ("moov atom not found", "missing_container_index"),
            ("Invalid NAL unit size", "corrupt_packet"),
            ("Error while decoding stream #0:0", "decode_corruption"),
        ):
            result = failure_health("decode", "failed", message)
            self.assertEqual((result["status"], result["reason"]), ("invalid", reason))
        for state, message in (
            ("timeout", "Error while decoding stream"),
            ("partial", "EBML header parsing failed"),
            ("failed", "Invalid data found when processing input"),
            ("failed", "Input/output error; EBML header parsing failed"),
            ("failed", "Permission denied"),
        ):
            self.assertEqual(
                failure_health("probe", state, message)["status"], "unknown"
            )
        self.assertEqual(
            failure_health(
                "probe", "failed", "Format not on whitelist; moov atom not found"
            )["status"],
            "unsupported",
        )

    def test_stream_policy_and_warnings_are_not_corruption(self):
        data = {
            "streams": [
                {"codec_type": "audio"},
                {"codec_type": "video", "disposition": {"attached_pic": 1}},
            ]
        }
        self.assertEqual(probe_health(data)["status"], "not_detected")
        result = probe_health(data, ["video"])
        self.assertEqual(result["status"], "content_mismatch")
        self.assertEqual(result["missing_streams"], ["video"])
        self.assertIn("duration_missing_or_nonpositive", result["warnings"])
        self.assertIn("codec_unidentified", result["warnings"])
        for value in ("video", ["subtitle"], [1], ["audio", "video", "audio"]):
            with self.assertRaises(CatabolicError):
                operation_config("probe", {"required_streams": value})

    def test_runner_retains_separate_bounded_diagnostics(self):
        output = command_output(
            [
                sys.executable,
                "-c",
                "import sys; print('ok'); print('warning',file=sys.stderr)",
            ],
            diagnostics=True,
        )
        self.assertEqual(output, {"stdout": b"ok\n", "stderr": b"warning\n"})
        with self.assertRaises(ProcessingFailure) as caught:
            command_output(
                [
                    sys.executable,
                    "-c",
                    "import sys; print('error',file=sys.stderr); sys.exit(1)",
                ]
            )
        self.assertEqual(caught.exception.stderr, b"error\n")
        self.assertEqual(caught.exception.returncode, 1)

    def test_schema_seven_does_not_query_missing_attempt_results(self):
        store = SimpleNamespace(schema_version=7)
        self.assertIsNone(recorded_reason(store, "default", "fixture", None))


class ProbePublicationTest(unittest.TestCase):
    def setUp(self):
        test_enrichment.EnrichmentTest.setUp(self)
        operation_config("probe")
        operation_config("decode")

    def run_probe(self, **options):
        self.p.enqueue("probe", file_ids=[self.file_id], options=options, refresh=True)
        self.p.run(_refresh=False)
        return self.p.list()["jobs"]

    @unittest.skipUnless(shutil.which("ffprobe"), "ffprobe optional")
    def test_real_audio_and_explicit_video_requirement(self):
        jobs = self.run_probe(required_streams=["video"])
        self.assertEqual(jobs[0]["state"], "complete")
        self.assertEqual(
            jobs[0]["result"]["media_health"]["status"], "content_mismatch"
        )
        self.assertIsNone(
            recorded_reason(self.store, "default", self.file_id, self.file.stat())
        )

    @unittest.skipUnless(shutil.which("ffprobe"), "ffprobe optional")
    def test_confirmed_defect_invalidates_facts_and_blocks_publication(self):
        self.p.enqueue("hash", file_ids=[self.file_id])
        self.p.run(_refresh=False)
        item = self.app.put_item("track", {"fixture": "health"}, {"title": "Health"})[
            "id"
        ]
        self.app.media.associate(self.file_id, item)
        self.app.put_mapping("global", self.file_id, item, "Health.wav")
        failure = ProcessingFailure(
            "failed", "container failure", stderr=b"EBML header parsing failed"
        )
        with patch("catabolic.processing.command_output", side_effect=failure):
            self.run_probe()
        self.assertEqual(
            self.store.rows("SELECT status FROM file_facts")[0]["status"], "invalidated"
        )
        self.assertEqual(
            recorded_reason(self.store, "default", self.file_id, self.file.stat()),
            "container_parse_error",
        )
        result = Reconciler(self.app).apply()
        self.assertFalse(result["verification"]["healthy"])
        self.assertFalse((self.output / "Health.wav").exists())
        from catabolic.curation import occurrence

        self.assertFalse(
            check([occurrence(self.store, "default", self.file_id)], self.store)[
                "usable"
            ]
        )
        with patch(
            "catabolic.processing.command_output",
            side_effect=ProcessingFailure("timeout", "deadline"),
        ):
            self.run_probe()
        self.assertEqual(
            recorded_reason(self.store, "default", self.file_id, self.file.stat()),
            "container_parse_error",
        )
        self.run_probe()
        self.assertIsNone(
            recorded_reason(self.store, "default", self.file_id, self.file.stat())
        )
        self.assertTrue(Reconciler(self.app).apply()["verification"]["healthy"])

    @unittest.skipUnless(shutil.which("ffprobe"), "ffprobe optional")
    def test_real_invalid_container_and_revision_fence(self):
        self.file.write_bytes(
            b"\x00\x00\x00\x18ftypisom\x00\x00\x00\x00isomiso2"
            + b"\x00\x00\x00\x10mdat"
            + b"\0" * 8
        )
        self.app.scan()
        jobs = self.run_probe()
        self.assertEqual(jobs[0]["result"]["media_health"]["status"], "invalid")
        self.assertIsNotNone(
            recorded_reason(self.store, "default", self.file_id, self.file.stat())
        )
        self.file.write_bytes(b"repaired content with a new revision")
        self.assertIsNone(
            recorded_reason(self.store, "default", self.file_id, self.file.stat())
        )

    @unittest.skipUnless(shutil.which("ffprobe"), "ffprobe optional")
    def test_successful_probe_diagnostics_are_classified(self):
        with patch(
            "catabolic.processing.command_output",
            return_value={
                "stdout": json.dumps({"streams": [], "format": {}}).encode(),
                "stderr": b"Packet corrupt",
            },
        ):
            jobs = self.run_probe()
        self.assertEqual(jobs[0]["state"], "failed")
        self.assertEqual(jobs[0]["result"]["media_health"]["status"], "invalid")

    @unittest.skipUnless(shutil.which("ffprobe"), "ffprobe optional")
    def test_smaller_successful_probe_does_not_clear_larger_defect(self):
        with patch(
            "catabolic.processing.command_output",
            side_effect=ProcessingFailure(
                "failed", "packet failure", stderr=b"Packet corrupt"
            ),
        ):
            self.run_probe(analysis_bytes=2000000, analysis_us=2000000)
        self.run_probe(analysis_bytes=1000000, analysis_us=1000000)
        self.assertEqual(
            recorded_reason(self.store, "default", self.file_id, self.file.stat()),
            "corrupt_packet",
        )
        self.run_probe(analysis_bytes=2000000, analysis_us=2000000)
        self.assertIsNone(
            recorded_reason(self.store, "default", self.file_id, self.file.stat())
        )

    @unittest.skipUnless(shutil.which("ffprobe"), "ffprobe optional")
    def test_successful_unsupported_probe_does_not_clear_corruption(self):
        with patch(
            "catabolic.processing.command_output",
            side_effect=ProcessingFailure(
                "failed", "container failure", stderr=b"EBML header parsing failed"
            ),
        ):
            self.run_probe()
        with patch(
            "catabolic.processing.command_output",
            return_value={
                "stdout": b'{"streams":[],"format":{}}',
                "stderr": b"Decoder (codec av1) not found for input stream 0",
            },
        ):
            self.run_probe()
        job = self.store.rows(
            "SELECT result FROM processing_attempts ORDER BY rowid DESC LIMIT 1"
        )[0]
        self.assertEqual(
            json.loads(job["result"])["media_health"]["status"], "unsupported"
        )
        self.assertEqual(
            recorded_reason(self.store, "default", self.file_id, self.file.stat()),
            "container_parse_error",
        )

    @unittest.skipUnless(shutil.which("ffprobe"), "ffprobe optional")
    def test_malformed_probe_output_is_unknown_without_crashing_runner(self):
        for payload in (
            [],
            {"format": None},
            {"streams": [None]},
            {"streams": [{"disposition": None}]},
        ):
            with self.subTest(payload=payload):
                with patch(
                    "catabolic.processing.command_output",
                    return_value={
                        "stdout": json.dumps(payload).encode(),
                        "stderr": b"",
                    },
                ):
                    self.run_probe()
                job = self.store.rows(
                    "SELECT result FROM processing_attempts ORDER BY rowid DESC LIMIT 1"
                )[0]
                self.assertEqual(
                    json.loads(job["result"])["media_health"]["status"], "unknown"
                )

    @unittest.skipUnless(shutil.which("ffprobe"), "ffprobe optional")
    def test_old_adapter_success_requires_requalification(self):
        with patch(
            "catabolic.processing.command_output",
            side_effect=ProcessingFailure(
                "failed", "packet failure", stderr=b"Packet corrupt"
            ),
        ):
            self.run_probe()
        self.run_probe()
        with self.store.transaction() as db:
            for row in self.store.rows("SELECT id FROM processing_jobs"):
                db.execute(
                    "UPDATE processing_jobs SET options=json_set(options,'$.extractor.adapter','probe-v2') WHERE id=?",
                    (row["id"],),
                )
                old = self.p.get(row["id"])
                cache_key = hashlib.sha256(
                    encode([old["snapshot"], "probe", old["options"]]).encode()
                ).hexdigest()
                db.execute(
                    "UPDATE processing_jobs SET cache_key=? WHERE id=?",
                    (cache_key, row["id"]),
                )
        self.assertEqual(
            recorded_reason(self.store, "default", self.file_id, self.file.stat()),
            "corrupt_packet",
        )
        result = self.p.enqueue("probe", file_ids=[self.file_id])
        self.assertEqual(len(result["queued"]), 1)
        self.p.run(_refresh=False)
        self.assertIsNone(
            recorded_reason(self.store, "default", self.file_id, self.file.stat())
        )

    @unittest.skipUnless(shutil.which("ffprobe"), "ffprobe optional")
    def test_successful_io_diagnostics_cannot_clear_corruption(self):
        with patch(
            "catabolic.processing.command_output",
            side_effect=ProcessingFailure(
                "failed", "packet failure", stderr=b"Packet corrupt"
            ),
        ):
            self.run_probe()
        with patch(
            "catabolic.processing.command_output",
            return_value={
                "stdout": b'{"streams":[],"format":{}}',
                "stderr": b"Input/output error",
            },
        ):
            self.run_probe()
        row = self.store.rows(
            "SELECT result FROM processing_attempts ORDER BY rowid DESC LIMIT 1"
        )[0]
        self.assertEqual(json.loads(row["result"])["media_health"]["status"], "unknown")
        self.assertEqual(
            recorded_reason(self.store, "default", self.file_id, self.file.stat()),
            "corrupt_packet",
        )

    @unittest.skipUnless(
        shutil.which("ffmpeg") and shutil.which("ffprobe"), "FFmpeg optional"
    )
    def test_probe_success_cannot_clear_decode_corruption(self):
        self.p.enqueue("decode", file_ids=[self.file_id])
        with patch(
            "catabolic.processing.command_output",
            side_effect=ProcessingFailure(
                "failed", "decode error", stderr=b"Error while decoding stream #0:0"
            ),
        ):
            self.p.run(_refresh=False)
        self.run_probe()
        self.assertEqual(
            recorded_reason(self.store, "default", self.file_id, self.file.stat()),
            "decode_corruption",
        )
        self.p.enqueue("decode", file_ids=[self.file_id], refresh=True)
        self.p.run(_refresh=False)
        self.assertIsNone(
            recorded_reason(self.store, "default", self.file_id, self.file.stat())
        )

    @unittest.skipUnless(shutil.which("ffprobe"), "ffprobe optional")
    def test_changed_source_discards_failed_corruption_evidence(self):
        def changed(*args, **kwargs):
            self.file.write_bytes(b"changed while probing")
            raise ProcessingFailure(
                "failed", "container failure", stderr=b"EBML header parsing failed"
            )

        with patch("catabolic.processing.command_output", side_effect=changed):
            jobs = self.run_probe()
        self.assertEqual(jobs[0]["state"], "changed")
        self.assertIsNone(
            recorded_reason(self.store, "default", self.file_id, self.file.stat())
        )

    @unittest.skipUnless(shutil.which("ffmpeg"), "ffmpeg optional")
    def test_real_truncated_audio_decode(self):
        self.file.write_bytes(self.file.read_bytes()[:-1])
        self.app.scan()
        self.p.enqueue("decode", file_ids=[self.file_id])
        self.p.run(_refresh=False)
        job = self.p.list()["jobs"][0]
        self.assertEqual(job["state"], "failed")
        self.assertEqual(job["result"]["media_health"]["reason"], "corrupt_packet")
        self.assertEqual(
            recorded_reason(self.store, "default", self.file_id, self.file.stat()),
            "corrupt_packet",
        )

    @unittest.skipUnless(shutil.which("ffprobe"), "ffprobe optional")
    def test_zero_header_is_invalid_without_running_tool(self):
        self.file.write_bytes(b"\0" * 100)
        self.app.scan()
        self.p.enqueue("probe", file_ids=[self.file_id])
        with patch("catabolic.processing.command_output") as command:
            self.p.run(_refresh=False)
        command.assert_not_called()
        self.assertEqual(
            self.p.list()["jobs"][0]["result"]["media_health"]["reason"],
            "zero_filled_header",
        )
