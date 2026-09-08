# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Native preset contracts and actual source-preserving generated outputs."""

import json
import shutil
import subprocess
import unittest
from unittest.mock import patch

from catabolic.domain import CatabolicError
from catabolic.rendering import capabilities, definition, input_error
from tests import test_artifacts


class NativeDefinitionTest(unittest.TestCase):
    def test_option_bounds_and_legacy_digest_inputs(self):
        self.assertEqual(
            definition("h264-720p", {}),
            {
                "version": 1,
                "preset": "h264-720p",
                "timeout": 3600,
                "max_output_bytes": 20 * 1024**3,
                "reserve_bytes": 64 * 1024**2,
            },
        )
        for preset, options in (
            ("av1-720p", {"crf": 64}),
            ("av1-720p", {"encoder_speed": "8 -y"}),
            ("audio-opus", {"audio_bitrate_kbps": True}),
            ("waveform", {"width": 4097}),
            ("waveform", {"duration_seconds": 61}),
            ("audio-normalize", {"integrated_lufs": -4}),
            ("audio-normalize", {"true_peak_db": float("nan")}),
            ("hdr-sdr-1080p", {"peak_nits": 0}),
        ):
            with (
                self.subTest(preset=preset, options=options),
                self.assertRaises(CatabolicError),
            ):
                definition(preset, options)
        self.assertEqual(definition("av1-720p", {"crf": 63})["crf"], 63)

    def test_tone_mapping_requires_complete_selected_hdr_tags(self):
        hdr = {
            "codec_type": "video",
            "color_transfer": "smpte2084",
            "color_primaries": "bt2020",
            "color_space": "bt2020nc",
            "color_range": "tv",
        }
        recipe = definition("hdr-sdr-1080p", {})
        self.assertIsNone(input_error(recipe, {"streams": [hdr]}))
        for key in ("color_transfer", "color_primaries", "color_space", "color_range"):
            with self.subTest(key=key):
                self.assertIsNotNone(
                    input_error(
                        recipe,
                        {"streams": [{k: v for k, v in hdr.items() if k != key}]},
                    )
                )
        data = {"streams": [{"codec_type": "video", "color_transfer": "bt709"}, hdr]}
        self.assertIsNone(input_error(definition("av1-720p", {}), data))
        self.assertIn(
            "HDR", input_error(definition("av1-720p", {"video_stream": 1}), data)
        )
        self.assertIsNone(
            input_error(definition("hdr-sdr-1080p", {"video_stream": 1}), data)
        )
        self.assertIsNotNone(input_error(definition("waveform", {}), data))

    def test_missing_filters_are_reported(self):
        from catabolic import rendering

        real = rendering.command_output

        def without_zscale(argv, **kwargs):
            raw = real(argv, **kwargs)
            return b"\n".join(
                line for line in raw.splitlines() if b"zscale" not in line
            )

        if not shutil.which("ffmpeg") or not shutil.which("ffprobe"):
            self.skipTest("FFmpeg required")
        with patch.object(rendering, "command_output", side_effect=without_zscale):
            found = {p["name"]: p for p in capabilities()["presets"]}
        self.assertFalse(found["hdr-sdr-1080p"]["available"])
        self.assertIn("filters:zscale", found["hdr-sdr-1080p"]["missing"])


@unittest.skipUnless(
    shutil.which("ffmpeg") and shutil.which("ffprobe"), "FFmpeg required"
)
class NativeRenderTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        test_artifacts.ArtifactTest.setUpClass.__func__(cls)
        cls.available = {p["name"]: p for p in capabilities()["presets"]}

    @classmethod
    def tearDownClass(cls):
        test_artifacts.ArtifactTest.tearDownClass.__func__(cls)

    setUp = test_artifacts.ArtifactTest.setUp
    enqueue = test_artifacts.ArtifactTest.enqueue

    def render(self, preset, options=None):
        if not self.available[preset]["available"]:
            self.skipTest(str(self.available[preset]["missing"]))
        recipe, job = self.enqueue(preset, options)
        result = self.a.run()
        self.assertTrue(result["complete"], result)
        artifact = self.a.get(result["completed"][0]["artifact_id"])
        path = self.generated / artifact["path"]
        data = json.loads(
            subprocess.check_output(
                ["ffprobe", "-v", "error", "-show_streams", "-of", "json", str(path)]
            )
        )
        subprocess.run(
            ["ffmpeg", "-v", "error", "-i", str(path), "-f", "null", "-"],
            check=True,
            capture_output=True,
        )
        self.assertEqual(self.source_file.read_bytes(), self.payload)
        self.assertEqual(
            self.a.enqueue(self.file_id, recipe["id"], "generated", self.item_id)[
                "job_id"
            ],
            job["job_id"],
        )
        return data["streams"], path, artifact

    def test_av1_and_optional_audio(self):
        streams, _, _ = self.render("av1-720p", {"encoder_speed": "veryfast"})
        self.assertEqual([s["codec_name"] for s in streams], ["av1", "opus"])
        self.assertLessEqual(streams[0]["height"], 720)
        streams, _, _ = self.render(
            "av1-720p", {"encoder_speed": "veryfast", "audio_stream": None}
        )
        self.assertEqual([s["codec_name"] for s in streams], ["av1"])

    def test_opus_extraction(self):
        streams, _, _ = self.render("audio-opus")
        self.assertEqual([s["codec_name"] for s in streams], ["opus"])
        self.assertEqual(streams[0]["sample_rate"], "48000")
        self.assertEqual(streams[0]["channels"], 2)

    def test_waveform_image(self):
        streams, _, _ = self.render(
            "waveform", {"width": 320, "height": 80, "duration_seconds": 1}
        )
        self.assertEqual([s["codec_name"] for s in streams], ["png"])
        self.assertEqual((streams[0]["width"], streams[0]["height"]), (320, 80))

    def test_normalized_audio(self):
        fixture = self.root / "long-audio.mkv"
        subprocess.run(
            [
                "ffmpeg",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=6",
                "-c:a",
                "pcm_s16le",
                "-metadata",
                "REPLAYGAIN_TRACK_GAIN=+8 dB",
                "-metadata:s:a",
                "R128_TRACK_GAIN=2048",
                str(fixture),
            ],
            check=True,
            capture_output=True,
        )
        self.payload = fixture.read_bytes()
        self.source_file.write_bytes(self.payload)
        self.app.scan()
        streams, path, _ = self.render("audio-normalize")
        self.assertEqual([s["codec_name"] for s in streams], ["flac"])
        self.assertEqual(streams[0]["sample_rate"], "48000")
        metadata = json.loads(
            subprocess.check_output(
                ["ffprobe", "-v", "error", "-show_format", "-of", "json", str(path)]
            )
        )
        for tags in (metadata["format"].get("tags", {}), streams[0].get("tags", {})):
            self.assertFalse(
                any(key.upper().startswith(("REPLAYGAIN_", "R128_")) for key in tags)
            )
        measured = subprocess.run(
            [
                "ffmpeg",
                "-hide_banner",
                "-i",
                str(path),
                "-af",
                "loudnorm=print_format=json",
                "-f",
                "null",
                "-",
            ],
            check=True,
            capture_output=True,
            text=True,
        ).stderr
        stats, _ = json.JSONDecoder().raw_decode(measured[measured.rindex("{") :])
        self.assertAlmostEqual(float(stats["input_i"]), -16, delta=0.5)
        self.assertLessEqual(float(stats["input_tp"]), -1.5)

    def test_tone_mapping_missing_dependency_rejects_enqueue(self):
        if not self.available["hdr-sdr-1080p"]["available"]:
            with self.assertRaisesRegex(CatabolicError, "filters:"):
                self.enqueue("hdr-sdr-1080p")

    def test_actual_hdr_output(self):
        if not self.available["hdr-sdr-1080p"]["available"]:
            self.skipTest(str(self.available["hdr-sdr-1080p"]["missing"]))
        hdr = self.source / "hdr.mkv"
        subprocess.run(
            [
                "ffmpeg",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=160x120:rate=10:duration=2",
                "-c:v",
                "ffv1",
                "-pix_fmt",
                "yuv420p10le",
                "-color_primaries",
                "bt2020",
                "-color_trc",
                "smpte2084",
                "-colorspace",
                "bt2020nc",
                "-color_range",
                "tv",
                "-threads",
                "1",
                str(hdr),
            ],
            check=True,
            capture_output=True,
        )
        self.source_file.write_bytes(hdr.read_bytes())
        self.payload = self.source_file.read_bytes()
        self.app.scan()
        streams, _, _ = self.render("hdr-sdr-1080p")
        self.assertEqual(streams[0]["color_transfer"], "bt709")
        self.assertEqual(streams[0]["color_primaries"], "bt709")
        self.assertEqual(streams[0]["color_space"], "bt709")
        self.assertEqual(streams[0]["color_range"], "tv")
