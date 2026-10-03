# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Bounded HLS encoding in the supervised API worker, with no catalog publication."""

import json
import os
import sys
import time

from . import rendering
from .access import AccessError, authenticate
from .domain import CatabolicError
from .playback import permitted
from .playback_cache import (
    cache_size,
    cleanup,
    destination_root,
    directory_name,
    playlist,
)
from .process_runner import CommandFailure, command_output
from .source_access import validated_source
from .store import Store

SEGMENT_SECONDS = 2


def hls_command(input_fd, directory, recipe, tools, before):
    error = rendering.input_error(recipe, before)
    if error:
        raise CatabolicError(error)
    videos = rendering.streams(before, "video")
    video = recipe.get("video_stream", 0)
    if video >= len(videos):
        raise CatabolicError("input lacks requested video")
    command = [
        tools["ffmpeg"]["tool"],
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-xerror",
        "-protocol_whitelist",
        "file,pipe",
        "-format_whitelist",
        rendering.FORMATS,
        "-max_alloc",
        "33554432",
        "-threads",
        "1",
        "-i",
        f"/dev/fd/{input_fd}",
        "-map",
        f"0:{videos[video]['index']}",
        "-map_metadata",
        "-1",
        "-map_chapters",
        "-1",
        "-c:v",
        "libx264",
        "-preset",
        recipe.get("encoder_speed", "medium"),
        "-crf",
        str(recipe.get("crf", 23)),
        "-tune",
        "zerolatency",
        "-pix_fmt",
        "yuv420p",
        "-vf",
        "scale=w=-2:h='min(ih,"
        + ("720" if recipe["preset"] == "h264-720p" else "1080")
        + ")'",
        "-flags",
        "+cgop",
        "-force_key_frames",
        "expr:gte(t,n_forced*2)",
        "-sc_threshold",
        "0",
    ]
    audio = rendering.streams(before, "audio")
    selected = recipe.get("audio_stream", 0)
    if selected is not None and audio:
        if selected >= len(audio):
            raise CatabolicError("input lacks requested audio")
        command += [
            "-map",
            f"0:{audio[selected]['index']}",
            "-c:a",
            "aac",
            "-b:a",
            str(recipe.get("audio_bitrate_kbps", 160)) + "k",
            "-ac",
            "2",
            "-ar",
            "48000",
        ]
    elif selected is not None and "audio_stream" in recipe:
        raise CatabolicError("input lacks requested audio")
    command += [
        "-threads",
        "1",
        "-filter_threads",
        "1",
        "-filter_complex_threads",
        "1",
        "-f",
        "hls",
        "-hls_time",
        str(SEGMENT_SECONDS),
        "-hls_playlist_type",
        "event",
        "-hls_flags",
        "temp_file+independent_segments",
        "-hls_segment_filename",
        "segment-%06d.ts",
        "index.m3u8",
    ]
    return command


def interested(store, job):
    sessions = store.rows(
        "SELECT * FROM api_playback_sessions WHERE job_id=? AND cancelled=0 AND expires>?",
        (job["id"], time.time()),
    )
    for row in sessions:
        try:
            access = authenticate(store, token_id=row["token_id"])
            permitted(access, json.loads(job["body"]))
            return True
        except AccessError:
            continue
    return False


def execute(store, job, capabilities, *, _command=None):
    recipe, tools = json.loads(job["recipe"]), json.loads(job["tools"])
    try:
        if tools != rendering.tools() or not capabilities.get("playback_hls"):
            raise AccessError("playback_tools_changed", 409)
        if not interested(store, job):
            raise CommandFailure("cancelled", "no active viewers")
        with destination_root(
            store, job["profile"], json.loads(job["destination"])
        ) as root:
            os.mkdir(directory_name(job), mode=0o700, dir_fd=root)
            directory = os.open(
                directory_name(job),
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                dir_fd=root,
            )
            try:
                st = os.fstat(directory)
                with store.transaction() as db:
                    db.execute(
                        "UPDATE api_playback_jobs SET state='running',directory_device=?,directory_inode=?,updated_at=? WHERE id=?",
                        (st.st_dev, st.st_ino, time.time(), job["id"]),
                    )
                job.update(
                    state="running",
                    directory_device=st.st_dev,
                    directory_inode=st.st_ino,
                )
                last = last_budget = 0.0

                def poll():
                    nonlocal last, last_budget
                    if time.monotonic() - last_budget >= 0.25:
                        last_budget = time.monotonic()
                        fs = os.fstatvfs(directory)
                        if (
                            fs.f_bavail * fs.f_frsize < recipe["reserve_bytes"]
                            or cache_size(directory) >= recipe["max_output_bytes"]
                        ):
                            raise AccessError("playback_storage_budget", 409)
                    if time.monotonic() - last < 1:
                        return
                    last = time.monotonic()
                    try:
                        with Store(store.path, writable=True) as current:
                            if current.database_id != store.database_id:
                                raise AccessError("playback_catalog_changed", 409)
                            if not interested(current, job):
                                raise CommandFailure("cancelled", "no active viewers")
                            with destination_root(
                                current, job["profile"], json.loads(job["destination"])
                            ):
                                pass
                            with validated_source(json.loads(job["snapshot"])):
                                pass
                            _, segments, seconds = playlist(directory)
                            with current.transaction() as db:
                                db.execute(
                                    "UPDATE api_playback_jobs SET segment_count=?,available_seconds=?,updated_at=? WHERE id=? AND state='running'",
                                    (len(segments), seconds, time.time(), job["id"]),
                                )
                                db.execute(
                                    "UPDATE api_worker_status SET updated_at=? WHERE profile=? AND worker_pid=?",
                                    (time.time(), job["profile"], os.getpid()),
                                )
                    except CatabolicError as exc:
                        from .database_io import is_catalog_busy

                        if not is_catalog_busy(exc):
                            raise

                with store.detached():
                    with validated_source(json.loads(job["snapshot"])) as source:
                        before = rendering.probe(source, tools)
                        os.lseek(source, 0, os.SEEK_SET)
                        command = hls_command(source, directory, recipe, tools, before)
                        command = _command(command) if _command else command
                        command = [
                            sys.executable,
                            "-m",
                            "catabolic.playback_process",
                            str(os.getpid()),
                            f"{source},{directory}",
                            *command,
                        ]
                        command_output(
                            command,
                            pass_fds=(source, directory),
                            timeout=recipe["timeout"],
                            on_poll=poll,
                            umask=0o077,
                        )
                        if cache_size(directory) >= recipe["max_output_bytes"]:
                            raise AccessError("playback_storage_budget", 409)
                        raw, segments, seconds = playlist(directory)
                        if not segments or "#EXT-X-ENDLIST" not in raw:
                            raise AccessError("incomplete_playback_output", 409)
                        expected = rendering.duration(before)
                        if recipe.get("require_duration") and not expected:
                            raise AccessError("playback_duration_unknown", 409)
                        if expected and abs(seconds - expected) > max(
                            1, expected * 0.02
                        ):
                            raise AccessError("playback_duration_mismatch", 409)
                        if (
                            recipe.get("max_size_ratio")
                            and cache_size(directory)
                            > os.fstat(source).st_size * recipe["max_size_ratio"]
                        ):
                            raise AccessError("playback_size_ratio", 409)
                if not interested(store, job):
                    raise CommandFailure("cancelled", "no active viewers")
                with store.transaction() as db:
                    db.execute(
                        "UPDATE api_playback_jobs SET state='ready',reserved_bytes=0,segment_count=?,available_seconds=?,updated_at=? WHERE id=? AND state='running'",
                        (len(segments), seconds, time.time(), job["id"]),
                    )
            finally:
                os.close(directory)
    except BaseException as exc:
        state = (
            "cancelled"
            if isinstance(exc, CommandFailure) and exc.state == "cancelled"
            else "failed"
        )
        code = getattr(
            exc, "code", "playback_" + getattr(exc, "state", "encode_failed")
        )
        with store.transaction() as db:
            db.execute(
                "UPDATE api_playback_jobs SET state=?,error=?,reserved_bytes=0,updated_at=? WHERE id=?",
                (state, code, time.time(), job["id"]),
            )
        if not isinstance(exc, (CatabolicError, CommandFailure, OSError)):
            raise
    return {"processed": 1}


def recover(store, profile):
    # Called only after acquiring the exclusive API worker supervisor lock. FFmpeg
    # also checks the supervisor PID, so an orphan stops before cache retirement.
    with store.transaction() as db:
        db.execute(
            "UPDATE api_playback_jobs SET state='failed',error='playback_worker_interrupted',reserved_bytes=0,updated_at=? WHERE profile=? AND state='running'",
            (time.time(), profile),
        )


def tick(store, profile, capabilities):
    cleanup(store, profile)
    rows = store.rows(
        "SELECT * FROM api_playback_jobs WHERE profile=? AND state='queued' ORDER BY created_at LIMIT 1",
        (profile,),
    )
    if rows:
        return execute(store, rows[0], capabilities)
    return None
