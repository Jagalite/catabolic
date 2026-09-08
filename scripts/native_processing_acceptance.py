# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Installed-CLI native render replay; explicitly records unavailable optional tools."""

import hashlib
import json
import subprocess


def exercise(workflow, file_id, item_id):
    cli = workflow.cli
    capabilities = {p["name"]: p for p in cli("artifact", "capabilities")["presets"]}
    report = {}
    for preset, expected_codec in [
        ("av1-720p", "av1"),
        ("audio-opus", "opus"),
        ("waveform", "png"),
        ("audio-normalize", "flac"),
    ]:
        if not capabilities[preset]["available"]:
            report[preset] = {
                "status": "unavailable",
                "missing": capabilities[preset]["missing"],
            }
            continue
        options = {"encoder_speed": "veryfast"} if preset == "av1-720p" else {}
        recipe = cli(
            "artifact",
            "recipe",
            "native-" + preset,
            "--preset",
            preset,
            "--options",
            json.dumps(options),
        )
        enqueue = (
            "artifact",
            "enqueue",
            "--file-id",
            file_id,
            "--item-id",
            item_id,
            "--recipe",
            recipe["id"],
            "--location",
            "generated",
        )
        job = cli(*enqueue)
        result = cli("artifact", "run", "--limit", "1")
        assert result["complete"], result
        artifact = cli("artifact", "show", result["completed"][0]["artifact_id"])
        path = workflow.root / "generated" / artifact["path"]
        assert hashlib.sha256(path.read_bytes()).hexdigest() == artifact["sha256"]
        data = json.loads(
            subprocess.check_output(
                ["ffprobe", "-v", "error", "-show_streams", "-of", "json", str(path)]
            )
        )
        assert data["streams"][0]["codec_name"] == expected_codec, data
        subprocess.run(
            ["ffmpeg", "-v", "error", "-i", str(path), "-f", "null", "-"],
            check=True,
            capture_output=True,
            timeout=60,
        )
        assert cli(*enqueue)["job_id"] == job["job_id"]
        report[preset] = {
            "status": "passed",
            "artifact_id": artifact["id"],
            "sha256": artifact["sha256"],
            "codecs": [s["codec_name"] for s in data["streams"]],
        }
    report["hdr-sdr-1080p"] = {
        "status": "not_run"
        if capabilities["hdr-sdr-1080p"]["available"]
        else "unavailable",
        "missing": capabilities["hdr-sdr-1080p"]["missing"],
    }
    return report
