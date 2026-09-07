# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Versioned planning heuristics, never promises about compressed media size."""

import math

from .domain import CatabolicError


def positive(value):
    try:
        value = float(value)
        return value if math.isfinite(value) and 0 < value <= 10**15 else None
    except (ValueError, TypeError, OverflowError):
        return None


def options(value):
    if not isinstance(value, dict) or set(value) - {"video_kbps"}:
        raise CatabolicError("estimate options accept only video_kbps")
    if "video_kbps" in value and (
        type(value["video_kbps"]) is not int or not 1 <= value["video_kbps"] <= 1000000
    ):
        raise CatabolicError("estimated video_kbps must be 1..1000000")
    return dict(value)


def estimate(recipe, observation, probe, assumptions):
    """Return additional logical bytes for one output; originals are retained."""
    preset = recipe["preset"]
    cap = recipe["max_output_bytes"]
    result = {
        "estimator_version": 1,
        "expected_bytes": None,
        "low_bytes": None,
        "high_bytes": None,
        "output_limit_bytes": cap,
        "method": "unknown",
        "assumptions": [],
        "exceeds_output_limit": False,
    }
    if not probe:
        result["assumptions"] = ["Current probe facts are missing; size is unknown."]
        return result
    streams = probe.get("streams", [])
    duration = positive(probe.get("summary", {}).get("duration")) or positive(
        probe.get("format", {}).get("duration")
    )
    video = [s for s in streams if s.get("codec_type") == "video"]
    audio = [s for s in streams if s.get("codec_type") == "audio"]
    expected = low = high = None
    if preset == "thumbnail" and len(video) > recipe.get("video_stream", 0):
        stream = video[recipe.get("video_stream", 0)]
        w, h = positive(stream.get("width")), positive(stream.get("height"))
        if w and h:
            width = min(640, w)
            raw = width * max(2, math.ceil(h * width / w / 2) * 2) * 3
            expected, low, high = raw * 0.6 + 4096, raw * 0.1 + 4096, raw * 1.2 + 65536
            result["method"] = "scaled_rgb_png"
            result["assumptions"] = [
                "PNG size varies with image complexity; RGB storage heuristic at a maximum width of 640."
            ]
    elif preset in ("preview", "h264-720p", "h264-1080p") and duration:
        if preset == "preview":
            duration = min(
                recipe["duration_seconds"], max(0, duration - recipe["start_seconds"])
            )
        kbps = assumptions.get("video_kbps", 5000 if preset == "h264-1080p" else 2500)
        audio_rate = (
            recipe.get("audio_bitrate_kbps", 160)
            if audio and recipe.get("audio_stream", 0) is not None
            else 0
        )
        expected = duration * (kbps + audio_rate) * 1000 / 8 * 1.02
        low = duration * (kbps * 0.4 + audio_rate) * 1000 / 8
        high = duration * (kbps * 3 + audio_rate) * 1000 / 8 * 1.05
        result["method"] = "duration_times_assumed_bitrate"
        result["assumptions"] = [
            f"Assume {kbps} kbps video and {audio_rate} kbps audio; 2% container overhead.",
            "This is a planning assumption, not an encoder bitrate setting. CRF output can fall outside this heuristic range.",
        ]
    elif preset == "remux-mkv":
        size = positive(observation.get("size"))
        if size:
            expected, low, high = size * 1.01, size * 0.95, size * 1.1
            result["method"] = "source_size"
            result["assumptions"] = [
                "Stream copy usually stays near source size; selecting fewer streams can make it much smaller."
            ]
    elif preset == "audio-aac" and duration:
        bitrate = recipe.get("audio_bitrate_kbps", 128)
        expected = duration * bitrate * 1000 / 8 * 1.02
        low, high = expected * 0.85, expected * 1.2
        result["method"] = "audio_bitrate"
        result["assumptions"] = [
            f"Assume {bitrate} kbps AAC; when omitted by the recipe, 128 kbps is an estimate of the encoder default."
        ]
    elif preset == "audio-flac" and duration and len(audio) > recipe.get("stream", 0):
        stream = audio[recipe.get("stream", 0)]
        rate, channels = (
            positive(stream.get("sample_rate")),
            positive(stream.get("channels")),
        )
        bits = positive(stream.get("bits_per_raw_sample")) or 16
        if rate and channels:
            raw = duration * rate * channels * bits / 8
            expected, low, high = raw * 0.65, raw * 0.3, raw * 1.1
            result["method"] = "pcm_compression_ratio"
            result["assumptions"] = [
                f"Assume {bits:g}-bit samples and 65% of raw PCM size; compression varies with content."
            ]
    elif preset == "subtitle-srt" and duration:
        expected, low, high = (
            duration * 20 + 1024,
            duration * 2 + 1024,
            duration * 100 + 1024,
        )
        result["method"] = "subtitle_text_density"
        result["assumptions"] = [
            "Assume 20 bytes of subtitle text/timing per second; dialogue density varies."
        ]
    if expected is not None:
        result.update(
            expected_bytes=math.ceil(expected),
            low_bytes=math.ceil(low),
            high_bytes=math.ceil(high),
            exceeds_output_limit=high > cap,
        )
    else:
        result["assumptions"] = [
            "Probe facts lack the duration or stream properties needed for an estimate."
        ]
    return result


def total(estimates):
    values = list(estimates)
    known = [value for value in values if value["expected_bytes"] is not None]
    unknown = len(values) - len(known)
    return {
        "output_count": len(values),
        "estimated_count": len(known),
        "unknown_count": unknown,
        "expected_bytes": sum(v["expected_bytes"] for v in known)
        if not unknown
        else None,
        "known_expected_bytes": sum(v["expected_bytes"] for v in known),
        "low_bytes": sum(v["low_bytes"] for v in known) if not unknown else None,
        "high_bytes": sum(v["high_bytes"] for v in known) if not unknown else None,
        "planning_bytes": sum(
            v["high_bytes"] if v["high_bytes"] is not None else v["output_limit_bytes"]
            for v in values
        ),
        "configured_output_limits_bytes": sum(v["output_limit_bytes"] for v in values),
        "estimates_exceeding_output_limit": sum(
            v["exceeds_output_limit"] for v in values
        ),
    }


def human_bytes(value):
    if value is None:
        return "unknown"
    return f"{value / 1024**3:.2f} GiB ({value:,} bytes)"
