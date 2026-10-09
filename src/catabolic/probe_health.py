# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Conservative interpretation of bounded FFmpeg evidence."""

import math
import re


def failure_health(operation, state, diagnostic):
    message = diagnostic.lower()
    # Environmental and policy failures take precedence over parsing messages.
    unsupported = (
        "not on whitelist",
        "not on the whitelist",
        "protocol not found",
        "decoder not found",
        "unknown decoder",
        "unsupported codec",
        "decoding requested, but no decoder found",
        "not implemented",
    )
    unavailable = (
        "permission denied",
        "input/output error",
        "cannot allocate memory",
        "no such file",
        "resource temporarily unavailable",
    )
    reason = state
    status = "unsupported" if state == "unsupported" else "unknown"
    if any(token in message for token in unsupported) or re.search(
        r"decoder(?:[^\n]*) not found", message
    ):
        status, reason = "unsupported", "unsupported_format_or_codec"
    elif any(token in message for token in unavailable):
        reason = "io_error"
    elif state == "failed":
        defects = (
            ("ebml header parsing failed", "container_parse_error"),
            ("moov atom not found", "missing_container_index"),
            ("invalid nal unit size", "corrupt_packet"),
            ("packet corrupt", "corrupt_packet"),
            ("corrupt input packet", "corrupt_packet"),
            ("invalid pcm packet", "corrupt_packet"),
            ("error while decoding stream", "decode_corruption"),
            ("corrupt decoded frame", "decode_corruption"),
        )
        for token, flag in defects:
            if token in message:
                status, reason = "invalid", flag
                break
    return {
        "status": status,
        "reason": reason,
        "warnings": [],
        "coverage": "bounded stream analysis"
        if operation == "probe"
        else "incomplete decode",
    }


def probe_health(data, required_streams=()):
    streams = data.get("streams", [])
    present = {
        s.get("codec_type")
        for s in streams
        if not s.get("disposition", {}).get("attached_pic")
    }
    missing = sorted(set(required_streams) - present)
    warnings = []
    duration = data.get("format", {}).get("duration")
    try:
        if (
            duration is None
            or not math.isfinite(float(duration))
            or float(duration) <= 0
        ):
            warnings.append("duration_missing_or_nonpositive")
    except (ValueError, TypeError):
        warnings.append("duration_missing_or_nonpositive")
    if any(
        s.get("codec_type") in ("audio", "video") and not s.get("codec_name")
        for s in streams
    ):
        warnings.append("codec_unidentified")
    return {
        "status": "content_mismatch" if missing else "not_detected",
        "reason": "missing_required_streams" if missing else None,
        "missing_streams": missing,
        "warnings": warnings,
        "coverage": "bounded stream analysis; not a full decode",
    }
