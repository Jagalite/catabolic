# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Small, versioned FFmpeg recipes. No user-provided command fragments or URLs."""

import hashlib
import json
import math
import os
import re

from .domain import CatabolicError
from .process_runner import command_output
from .processing import tool_signature
from .store import encode

FORMATS = "mov,matroska,webm,wav,flac,mp3,ogg,aac,ac3,eac3,aiff,ape,asf,avi,mpeg,mpegts,png_pipe,jpeg_pipe,webp_pipe,gif,bmp_pipe,tiff_pipe"
PRESETS = {
    "thumbnail": ("png", "image2", "thumbnail", "video", "png"),
    "preview": ("mp4", "mp4", "extra", "video", "libx264"),
    "remux-mkv": ("mkv", "matroska", "primary", None, None),
    "audio-flac": ("flac", "flac", "custom:audio", "audio", "flac"),
    "audio-aac": ("m4a", "mp4", "custom:audio", "audio", "aac"),
    "subtitle-srt": ("srt", "srt", "subtitle", "subtitle", "srt"),
    "h264-720p": ("mp4", "mp4", "primary", "video", "libx264"),
    "h264-1080p": ("mp4", "mp4", "primary", "video", "libx264"),
}


def definition(preset, options):
    if preset not in PRESETS or not isinstance(options, dict):
        raise CatabolicError("unknown rendering preset or invalid options")
    allowed = {"timeout", "max_output_bytes", "reserve_bytes"}
    if preset in ("thumbnail", "preview"):
        allowed.add("start_seconds")
    if preset == "preview":
        allowed.add("duration_seconds")
    if preset in ("subtitle-srt", "audio-flac", "audio-aac"):
        allowed.add("stream")
    if set(options) - allowed:
        raise CatabolicError("unsupported recipe option")
    value = {
        "version": 1,
        "preset": preset,
        "timeout": 3600,
        "max_output_bytes": 20 * 1024**3,
        "reserve_bytes": 64 * 1024**2,
    }
    if "start_seconds" in allowed:
        value["start_seconds"] = 0
    if "duration_seconds" in allowed:
        value["duration_seconds"] = 10
    if "stream" in allowed:
        value["stream"] = 0
    value.update(options)
    for key, lower, upper in (
        ("timeout", 1, 86400),
        ("max_output_bytes", 1024, 1024**4),
        ("reserve_bytes", 0, 1024**4),
        ("start_seconds", 0, 86400),
        ("duration_seconds", 1, 60),
        ("stream", 0, 255),
    ):
        if key in value and (
            type(value[key]) is not int or not lower <= value[key] <= upper
        ):
            raise CatabolicError(f"invalid recipe {key}")
    return value


def tools(*, required=True):
    result = {"ffmpeg": tool_signature("decode"), "ffprobe": tool_signature("probe")}
    if required and any(not value.get("available") for value in result.values()):
        raise CatabolicError(
            "rendering requires optional ffmpeg and ffprobe executables"
        )
    return result


def capabilities():
    identity = tools(required=False)
    available = all(value.get("available") for value in identity.values())
    supported = {}
    if available:
        for category in ("encoders", "muxers", "filters"):
            listing = command_output(
                [identity["ffmpeg"]["tool"], "-hide_banner", "-" + category]
            ).decode()
            supported[category] = {
                name
                for line in listing.splitlines()
                if len(parts := line.split()) >= 2
                and re.fullmatch(r"[A-Z.]{1,8}", parts[0])
                for name in parts[1].split(",")
            }
    return {
        "tools": identity,
        "presets": [
            {
                "name": key,
                "extension": spec[0],
                "role": spec[2],
                "available": available
                and spec[1] in supported["muxers"]
                and (spec[4] is None or spec[4] in supported["encoders"])
                and (
                    key not in ("thumbnail", "preview", "h264-720p", "h264-1080p")
                    or "scale" in supported["filters"]
                )
                and (
                    key not in ("preview", "h264-720p", "h264-1080p")
                    or "aac" in supported["encoders"]
                ),
            }
            for key, spec in PRESETS.items()
        ],
    }


def probe(fd, identity, timeout=20):
    os.lseek(fd, 0, os.SEEK_SET)
    raw = command_output(
        [
            identity["ffprobe"]["tool"],
            "-v",
            "error",
            "-protocol_whitelist",
            "file,pipe",
            "-format_whitelist",
            FORMATS + ",srt",
            "-max_alloc",
            "33554432",
            "-probesize",
            "1048576",
            "-analyzeduration",
            "1000000",
            "-show_format",
            "-show_streams",
            "-show_chapters",
            "-of",
            "json",
            f"/dev/fd/{fd}",
        ],
        pass_fds=(fd,),
        timeout=timeout,
    )
    data = json.loads(raw)
    if not isinstance(data.get("streams"), list) or len(data["streams"]) > 256:
        raise CatabolicError("invalid or excessive output streams")
    data.get("format", {}).pop("filename", None)
    video, audio = streams(data, "video"), streams(data, "audio")
    data["summary"] = {
        "width": max((s.get("width", 0) for s in video), default=None),
        "height": max((s.get("height", 0) for s in video), default=None),
        "video_codecs": sorted({s["codec_name"] for s in video if s.get("codec_name")}),
        "audio_codecs": sorted({s["codec_name"] for s in audio if s.get("codec_name")}),
        "languages": sorted(
            {
                s["tags"]["language"]
                for s in data["streams"]
                if s.get("tags", {}).get("language")
            }
        ),
        "duration": data.get("format", {}).get("duration"),
        "format": data.get("format", {}).get("format_name"),
        "hdr": any(
            s.get("color_transfer") in ("smpte2084", "arib-std-b67") for s in video
        )
        if video and all(s.get("color_transfer") for s in video)
        else None,
    }
    return data


def digest(fd):
    os.lseek(fd, 0, os.SEEK_SET)
    result = hashlib.sha256()
    while data := os.read(fd, 1024 * 1024):
        result.update(data)
    return result.hexdigest()


def streams(data, kind):
    return [
        s
        for s in data["streams"]
        if s.get("codec_type") == kind
        and not s.get("disposition", {}).get("attached_pic")
    ]


def duration(data):
    try:
        result = float(data.get("format", {}).get("duration", 0))
    except (ValueError, TypeError):
        return 0
    return result if math.isfinite(result) and result > 0 else 0


def render(input_fd, output_fd, recipe, identity, poll):
    preset = recipe["preset"]
    before = probe(input_fd, identity)
    expected = PRESETS[preset][3]
    selected = streams(before, expected) if expected else before["streams"]
    if not selected or recipe.get("stream", 0) >= len(selected):
        raise CatabolicError("input lacks the requested stream")
    if preset in ("preview", "h264-720p", "h264-1080p") and any(
        s.get("color_transfer") in ("smpte2084", "arib-std-b67")
        for s in streams(before, "video")
    ):
        raise CatabolicError(
            "HDR conversion requires a reviewed tone-mapping recipe; these presets support SDR"
        )
    os.lseek(input_fd, 0, os.SEEK_SET)
    command = [
        identity["ffmpeg"]["tool"],
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-y",
        "-protocol_whitelist",
        "file,pipe",
        "-format_whitelist",
        FORMATS,
        "-max_alloc",
        "33554432",
        "-threads",
        "1",
        "-i",
        f"/dev/fd/{input_fd}",
        "-map_metadata",
        "0",
        "-map_chapters",
        "0",
    ]
    if preset == "remux-mkv":
        command += ["-map", "0", "-c", "copy"]
    elif preset == "thumbnail":
        command += [
            "-ss",
            str(recipe["start_seconds"]),
            "-map",
            f"0:{streams(before, 'video')[0]['index']}",
            "-frames:v",
            "1",
            "-vf",
            "scale=w='min(640,iw)':h=-2",
            "-c:v",
            "png",
            "-update",
            "1",
        ]
    elif preset in ("audio-flac", "audio-aac", "subtitle-srt"):
        command += [
            "-map",
            f"0:{selected[recipe['stream']]['index']}",
            "-c",
            PRESETS[preset][4],
        ]
    else:
        height = 1080 if preset == "h264-1080p" else 720
        command += [
            "-map",
            f"0:{streams(before, 'video')[0]['index']}",
            "-map",
            "0:a:0?",
            "-vf",
            f"scale=w=-2:h='trunc(min({height},ih)/2)*2'",
            "-c:v",
            "libx264",
            "-preset",
            "medium",
            "-crf",
            "23",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "160k",
        ]
        if preset == "preview":
            command += [
                "-ss",
                str(recipe["start_seconds"]),
                "-t",
                str(recipe["duration_seconds"]),
            ]
    # FD output is exclusively created by the caller; never point FFmpeg at a library path.
    command += [
        "-threads",
        "1",
        "-filter_threads",
        "1",
        "-fs",
        str(recipe["max_output_bytes"]),
        "-f",
        PRESETS[preset][1],
        f"/dev/fd/{output_fd}",
    ]
    command_output(
        command, pass_fds=(input_fd, output_fd), timeout=recipe["timeout"], on_poll=poll
    )
    poll()
    os.fsync(output_fd)
    after = probe(output_fd, identity)
    size = os.fstat(output_fd).st_size
    if not 0 < size < recipe["max_output_bytes"]:
        raise CatabolicError("empty output or output reached its byte budget")
    if expected and not streams(after, expected):
        raise CatabolicError("output is missing its expected stream")
    if preset == "remux-mkv":
        fields = ("codec_type", "codec_name")
        if [tuple(s.get(k) for k in fields) for s in before["streams"]] != [
            tuple(s.get(k) for k in fields) for s in after["streams"]
        ]:
            raise CatabolicError("remux did not preserve all streams and codecs")
    if preset in ("preview", "h264-720p", "h264-1080p"):
        if streams(after, "video")[0].get("codec_name") != "h264":
            raise CatabolicError("output codec does not match the recipe")
    if preset == "thumbnail" and streams(after, "video")[0].get("codec_name") != "png":
        raise CatabolicError("output is not a PNG thumbnail")
    wanted = duration(before)
    if preset == "preview":
        wanted = min(
            recipe["duration_seconds"], max(0, wanted - recipe["start_seconds"])
        )
    if (
        preset not in ("thumbnail", "subtitle-srt")
        and wanted
        and abs(duration(after) - wanted) > max(1, wanted * 0.02)
    ):
        raise CatabolicError("output duration does not match the requested content")
    return {
        "version": 1,
        "input": before,
        "output": after,
        "coverage": "bounded stream probe and operation-specific checks; not full decode",
        "recipe_digest": hashlib.sha256(encode(recipe).encode()).hexdigest(),
    }
