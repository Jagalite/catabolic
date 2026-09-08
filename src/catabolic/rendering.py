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
    "av1-720p": ("mkv", "matroska", "primary", "video", "libsvtav1"),
    "audio-opus": ("opus", "opus", "custom:audio", "audio", "libopus"),
    "hdr-sdr-1080p": ("mp4", "mp4", "primary", "video", "libx264"),
    "waveform": ("png", "image2", "custom:waveform", "audio", "png"),
    "audio-normalize": ("flac", "flac", "custom:audio", "audio", "flac"),
}
VIDEO_TRANSCODES = ("preview", "h264-720p", "h264-1080p", "av1-720p", "hdr-sdr-1080p")
AUDIO_PRESETS = ("audio-flac", "audio-aac", "audio-opus", "audio-normalize")
IMAGE_PRESETS = ("thumbnail", "waveform")


def definition(preset, options):
    if preset not in PRESETS or not isinstance(options, dict):
        raise CatabolicError("unknown rendering preset or invalid options")
    allowed = {
        "timeout",
        "max_output_bytes",
        "reserve_bytes",
        "max_size_ratio",
        "require_duration",
    }
    if preset in ("thumbnail", "preview"):
        allowed.add("start_seconds")
    if preset == "preview":
        allowed.add("duration_seconds")
    if preset in (*AUDIO_PRESETS, "subtitle-srt", "waveform"):
        allowed.add("stream")
    if preset == "remux-mkv":
        allowed.add("stream_indices")
    if preset in ("thumbnail", *VIDEO_TRANSCODES):
        allowed.add("video_stream")
    if preset in VIDEO_TRANSCODES:
        allowed.update(("audio_stream", "crf", "encoder_speed", "audio_bitrate_kbps"))
    if preset in ("audio-aac", "audio-opus"):
        allowed.add("audio_bitrate_kbps")
    if preset == "waveform":
        allowed.update(("width", "height", "duration_seconds", "start_seconds"))
    if preset == "audio-normalize":
        allowed.update(("integrated_lufs", "true_peak_db", "loudness_range"))
    if preset == "hdr-sdr-1080p":
        allowed.add("peak_nits")
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
    if preset == "waveform":
        value.update(width=1280, height=240, duration_seconds=60)
    if preset == "audio-normalize":
        value.update(integrated_lufs=-16, true_peak_db=-2, loudness_range=7)
    if preset == "hdr-sdr-1080p":
        value["peak_nits"] = 1000
    # Keep omitted settings omitted so old recipe definitions retain their digest.
    value.update(options)
    if "require_duration" in value and type(value["require_duration"]) is not bool:
        raise CatabolicError("require_duration must be boolean")
    if "max_size_ratio" in value and (
        type(value["max_size_ratio"]) not in (int, float)
        or not math.isfinite(value["max_size_ratio"])
        or not 0 < value["max_size_ratio"] <= 100
    ):
        raise CatabolicError("max_size_ratio must be a finite positive ratio up to 100")
    for key, lower, upper in (
        ("timeout", 1, 86400),
        ("max_output_bytes", 1024, 1024**4),
        ("reserve_bytes", 0, 1024**4),
        ("start_seconds", 0, 86400),
        ("duration_seconds", 1, 60),
        ("stream", 0, 255),
        ("video_stream", 0, 255),
        ("crf", 0, 63 if preset == "av1-720p" else 51),
        ("audio_bitrate_kbps", 8, 512),
        ("width", 64, 4096),
        ("height", 32, 2048),
        ("integrated_lufs", -70, -5),
        ("true_peak_db", -9, 0),
        ("loudness_range", 1, 50),
        ("peak_nits", 100, 10000),
    ):
        if key in value and (
            type(value[key]) is not int or not lower <= value[key] <= upper
        ):
            raise CatabolicError(f"invalid recipe {key}")
    if (
        "audio_stream" in value
        and value["audio_stream"] is not None
        and (
            type(value["audio_stream"]) is not int
            or not 0 <= value["audio_stream"] <= 255
        )
    ):
        raise CatabolicError(
            "audio_stream must be null or an integer between 0 and 255"
        )
    if "encoder_speed" in value and value["encoder_speed"] not in (
        "veryfast",
        "medium",
        "slow",
    ):
        raise CatabolicError("encoder_speed must be veryfast, medium, or slow")
    if "stream_indices" in value:
        indices = value["stream_indices"]
        if (
            not isinstance(indices, list)
            or not 1 <= len(indices) <= 256
            or any(type(i) is not int or not 0 <= i <= 255 for i in indices)
            or len(set(indices)) != len(indices)
        ):
            raise CatabolicError(
                "stream_indices must be a nonempty list of unique stream indices between 0 and 255"
            )
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
    requirements = {}
    for key, spec in PRESETS.items():
        needed = {
            "muxers": {spec[1]},
            "encoders": {spec[4]} if spec[4] else set(),
            "filters": set(),
        }
        if key in ("thumbnail", *VIDEO_TRANSCODES):
            needed["filters"].add("scale")
        if key in VIDEO_TRANSCODES:
            needed["encoders"].add("libopus" if key == "av1-720p" else "aac")
        if key == "hdr-sdr-1080p":
            needed["filters"].update(("zscale", "format", "tonemap", "sidedata"))
        if key == "waveform":
            needed["filters"].update(("showwavespic", "aformat", "atrim", "asetpts"))
        if key == "audio-normalize":
            needed["filters"].add("loudnorm")
        requirements[key] = [
            f"{category}:{name}"
            for category, names in needed.items()
            for name in sorted(names - supported.get(category, set()))
        ]
    return {
        "tools": identity,
        "presets": [
            {
                "name": key,
                "extension": spec[0],
                "role": spec[2],
                "available": available and not requirements[key],
                "missing": requirements[key],
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


def input_error(recipe, data):
    """The same recorded/live input requirements apply to planning and execution."""
    preset = recipe["preset"]
    kind = PRESETS[preset][3]
    candidates = streams(data, kind) if kind else data["streams"]
    index = (
        recipe.get("video_stream", 0) if kind == "video" else recipe.get("stream", 0)
    )
    if len(candidates) <= index:
        return "input lacks the requested stream"
    if preset in VIDEO_TRANSCODES:
        video = candidates[index]
        if preset == "hdr-sdr-1080p":
            if (
                video.get("color_transfer") not in ("smpte2084", "arib-std-b67")
                or video.get("color_primaries") != "bt2020"
                or video.get("color_space") != "bt2020nc"
                or video.get("color_range") not in ("tv", "pc")
            ):
                return "tone mapping requires tagged BT.2020 PQ/HLG input with a known range"
        elif video.get("color_transfer") in ("smpte2084", "arib-std-b67"):
            return "HDR conversion requires a tone-mapping recipe; this preset supports SDR"
        audio_index = recipe.get("audio_stream")
        if audio_index is not None and audio_index >= len(streams(data, "audio")):
            return "input lacks the requested audio stream"
    return None


def render(input_fd, output_fd, recipe, identity, poll):
    preset = recipe["preset"]
    before = probe(input_fd, identity)
    if error := input_error(recipe, before):
        raise CatabolicError(error)
    expected = PRESETS[preset][3]
    selected = streams(before, expected) if expected else before["streams"]
    if not selected or recipe.get("stream", 0) >= len(selected):
        raise CatabolicError("input lacks the requested stream")
    video_index = recipe.get("video_stream", 0)
    if expected == "video" and video_index >= len(selected):
        raise CatabolicError("input lacks the requested video stream")
    if preset == "remux-mkv" and "stream_indices" in recipe:
        indexed = {s["index"]: s for s in before["streams"]}
        if any(i not in indexed for i in recipe["stream_indices"]):
            raise CatabolicError("input lacks a requested remux stream index")
        selected = [indexed[i] for i in recipe["stream_indices"]]
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
        for stream in selected:
            command += ["-map", f"0:{stream['index']}"]
        command += ["-c", "copy"]
    elif preset == "thumbnail":
        command += [
            "-ss",
            str(recipe["start_seconds"]),
            "-map",
            f"0:{streams(before, 'video')[video_index]['index']}",
            "-frames:v",
            "1",
            "-vf",
            "scale=w='min(640,iw)':h=-2",
            "-c:v",
            "png",
            "-update",
            "1",
        ]
    elif preset == "waveform":
        command += [
            "-filter_complex",
            f"[0:{selected[recipe['stream']]['index']}]atrim=start={recipe['start_seconds']}:duration={recipe['duration_seconds']},asetpts=PTS-STARTPTS,aformat=sample_rates=48000:channel_layouts=mono,showwavespic=s={recipe['width']}x{recipe['height']}:colors=white[wave]",
            "-map",
            "[wave]",
            "-frames:v",
            "1",
            "-c:v",
            "png",
            "-update",
            "1",
        ]
    elif preset in (*AUDIO_PRESETS, "subtitle-srt"):
        command += [
            "-map",
            f"0:{selected[recipe['stream']]['index']}",
            "-c",
            PRESETS[preset][4],
        ]
        if preset == "audio-aac" and "audio_bitrate_kbps" in recipe:
            command += ["-b:a", f"{recipe['audio_bitrate_kbps']}k"]
        if preset == "audio-opus":
            command += [
                "-b:a",
                f"{recipe.get('audio_bitrate_kbps', 96)}k",
                "-ar",
                "48000",
                "-ac",
                "2",
            ]
        if preset == "audio-normalize":
            command += [
                "-af",
                f"loudnorm=I={recipe['integrated_lufs']}:TP={recipe['true_peak_db']}:LRA={recipe['loudness_range']}:linear=false",
                "-ar",
                "48000",
                "-sample_fmt",
                "s16",
            ]
            # Gains measured on the input would cause players to adjust the new
            # normalized samples a second time. Keep other descriptive tags.
            for key in (
                "REPLAYGAIN_TRACK_GAIN",
                "REPLAYGAIN_TRACK_PEAK",
                "REPLAYGAIN_ALBUM_GAIN",
                "REPLAYGAIN_ALBUM_PEAK",
                "R128_TRACK_GAIN",
                "R128_ALBUM_GAIN",
            ):
                command += ["-metadata", key + "=", "-metadata:s:a", key + "="]
    else:
        height = 1080 if preset in ("h264-1080p", "hdr-sdr-1080p") else 720
        filters = f"scale=w=-2:h='trunc(min({height},ih)/2)*2'"
        if preset == "hdr-sdr-1080p":
            filters = (
                "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,"
                f"tonemap=hable:desat=2:peak={recipe['peak_nits'] / 100:g},"
                "zscale=t=bt709:m=bt709:r=tv,format=yuv420p,"
                "sidedata=mode=delete," + filters
            )
        command += [
            "-map",
            f"0:{streams(before, 'video')[video_index]['index']}",
            "-vf",
            filters,
            "-c:v",
            PRESETS[preset][4],
            "-preset",
            str(
                {"veryfast": 12, "medium": 8, "slow": 4}[
                    recipe.get("encoder_speed", "medium")
                ]
            )
            if preset == "av1-720p"
            else recipe.get("encoder_speed", "medium"),
            "-crf",
            str(recipe.get("crf", 32 if preset == "av1-720p" else 23)),
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "libopus" if preset == "av1-720p" else "aac",
            "-b:a",
            f"{recipe.get('audio_bitrate_kbps', 96 if preset == 'av1-720p' else 160)}k",
        ]
        if preset == "av1-720p":
            command += ["-svtav1-params", "lp=1", "-ar", "48000", "-ac", "2"]
        if preset == "hdr-sdr-1080p":
            command += [
                "-color_primaries",
                "bt709",
                "-color_trc",
                "bt709",
                "-colorspace",
                "bt709",
                "-color_range",
                "tv",
            ]
        audio_index = recipe.get("audio_stream", 0)
        audio = streams(before, "audio")
        if audio_index is not None:
            if "audio_stream" in recipe and audio_index >= len(audio):
                raise CatabolicError("input lacks the requested audio stream")
            if audio:
                command += ["-map", f"0:{audio[audio_index]['index']}"]
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
        "-filter_complex_threads",
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
    if (
        "max_size_ratio" in recipe
        and size > os.fstat(input_fd).st_size * recipe["max_size_ratio"]
    ):
        raise CatabolicError(
            "output exceeds the recipe's maximum input/output size ratio"
        )
    output_kind = "video" if preset == "waveform" else expected
    if output_kind and not streams(after, output_kind):
        raise CatabolicError("output is missing its expected stream")
    if preset == "remux-mkv":
        fields = ("codec_type", "codec_name")
        if [tuple(s.get(k) for k in fields) for s in selected] != [
            tuple(s.get(k) for k in fields) for s in after["streams"]
        ]:
            raise CatabolicError(
                "remux did not preserve the selected streams and codecs"
            )
    if preset in VIDEO_TRANSCODES:
        if streams(after, "video")[0].get("codec_name") != (
            "av1" if preset == "av1-720p" else "h264"
        ):
            raise CatabolicError("output codec does not match the recipe")
        expected_audio = recipe.get("audio_stream", 0) is not None and bool(
            streams(before, "audio")
        )
        output_audio = streams(after, "audio")
        if len(output_audio) != int(expected_audio) or any(
            s.get("codec_name") != ("opus" if preset == "av1-720p" else "aac")
            for s in output_audio
        ):
            raise CatabolicError("output audio does not match the recipe")
    if preset == "hdr-sdr-1080p":
        output_video = streams(after, "video")[0]
        if (
            any(
                output_video.get(k) != "bt709"
                for k in ("color_transfer", "color_primaries", "color_space")
            )
            or output_video.get("color_range") != "tv"
        ):
            raise CatabolicError("output lacks the required BT.709 SDR color evidence")
    if preset in (*AUDIO_PRESETS, "subtitle-srt"):
        wanted_codec = {
            "audio-flac": "flac",
            "audio-aac": "aac",
            "subtitle-srt": "subrip",
            "audio-opus": "opus",
            "audio-normalize": "flac",
        }[preset]
        if (
            len(after["streams"]) != 1
            or after["streams"][0].get("codec_name") != wanted_codec
        ):
            raise CatabolicError("extracted stream codec does not match the recipe")
    if (
        preset in IMAGE_PRESETS
        and streams(after, "video")[0].get("codec_name") != "png"
    ):
        raise CatabolicError("output is not a PNG image")
    if preset == "waveform" and any(
        streams(after, "video")[0].get(k) != recipe[k] for k in ("width", "height")
    ):
        raise CatabolicError("waveform dimensions do not match the recipe")
    wanted = duration(before)
    if recipe.get("require_duration") and (not wanted or not duration(after)):
        raise CatabolicError("required duration evidence is unknown")
    if preset == "preview":
        wanted = min(
            recipe["duration_seconds"], max(0, wanted - recipe["start_seconds"])
        )
    if (
        preset not in (*IMAGE_PRESETS, "subtitle-srt")
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
        **(
            {
                "waveform_window": {
                    "start_seconds": recipe["start_seconds"],
                    "duration_seconds": recipe["duration_seconds"],
                }
            }
            if preset == "waveform"
            else {}
        ),
        **(
            {
                "normalization": {
                    "mode": "single-pass dynamic",
                    "integrated_lufs": recipe["integrated_lufs"],
                    "true_peak_db": recipe["true_peak_db"],
                    "loudness_range": recipe["loudness_range"],
                    "coverage": "configured targets; achieved loudness is not independently measured",
                }
            }
            if preset == "audio-normalize"
            else {}
        ),
        **(
            {
                "tone_mapping": {
                    "algorithm": "hable",
                    "peak_nits": recipe["peak_nits"],
                    "coverage": "explicit peak assumption and tagged input; not display certification",
                }
            }
            if preset == "hdr-sdr-1080p"
            else {}
        ),
    }
