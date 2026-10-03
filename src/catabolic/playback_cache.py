# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Descriptor-pinned temporary playback storage and whole-segment manifests."""

import json
import math
import os
import re
import stat
import time
from contextlib import contextmanager

from .access import AccessError
from .app import Application
from .artifacts import Artifacts
from .domain import CatabolicError
from .filesystem import owner_state, root_handle

SEGMENT = re.compile(r"segment-[0-9]{6}\.ts")
MAX_PLAYLIST = 4 * 1024 * 1024
CACHE_TTL = 3600


def reserved_bytes(store, device):
    return store.rows(
        """SELECT (SELECT coalesce(sum(bytes),0) FROM api_reservations WHERE device=?)
        + (SELECT coalesce(sum(reserved_bytes),0) FROM api_playback_jobs
        WHERE reservation_device=? AND state IN ('queued','running')) AS bytes""",
        (device, device),
    )[0]["bytes"]


@contextmanager
def destination_root(store, profile, binding):
    app = Application(store, profile)
    if app.binding("source", binding["owner"]) != binding:
        raise AccessError("playback_destination_changed", 409)
    with root_handle(binding) as fd:
        if owner_state(fd, Artifacts(app)._owner(binding["owner"])) != "owned":
            raise AccessError("playback_destination_not_owned", 409)
        yield fd


def directory_name(job):
    return ".playback-" + job["id"]


@contextmanager
def job_directory(store, job):
    with destination_root(
        store, job["profile"], json.loads(job["destination"])
    ) as root:
        fd = os.open(
            directory_name(job),
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
            dir_fd=root,
        )
        try:
            st = os.fstat(fd)
            if (st.st_dev, st.st_ino) != (
                job["directory_device"],
                job["directory_inode"],
            ):
                raise AccessError("playback_cache_changed", 409)
            yield fd
        finally:
            os.close(fd)


def read_file(directory, name, maximum):
    fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
    try:
        st = os.fstat(fd)
        if not stat.S_ISREG(st.st_mode) or st.st_size > maximum:
            raise AccessError("invalid_playback_cache", 409)
        data = os.read(fd, maximum + 1)
        if len(data) != st.st_size:
            raise AccessError("invalid_playback_cache", 409)
        return data
    finally:
        os.close(fd)


def playlist(directory):
    try:
        raw = read_file(directory, "index.m3u8", MAX_PLAYLIST).decode("utf-8")
    except FileNotFoundError:
        return None, [], 0.0
    except UnicodeDecodeError:
        raise AccessError("invalid_playback_cache", 409) from None
    if not raw.startswith("#EXTM3U\n"):
        raise AccessError("invalid_playback_cache", 409)
    segments, seconds = [], 0.0
    pending = None
    for line in raw.splitlines():
        if line.startswith("#EXTINF:"):
            try:
                pending = float(line[8:].split(",", 1)[0])
            except ValueError:
                raise AccessError("invalid_playback_cache", 409) from None
            if not math.isfinite(pending) or pending <= 0:
                raise AccessError("invalid_playback_cache", 409)
        elif line and not line.startswith("#"):
            if not SEGMENT.fullmatch(line) or pending is None or line in segments:
                raise AccessError("invalid_playback_cache", 409)
            segments.append(line)
            seconds += pending
            pending = None
        elif line.startswith("#") and not re.fullmatch(
            r"#(?:EXTM3U|EXT-X-VERSION:[0-9]+|EXT-X-TARGETDURATION:[0-9]+|EXT-X-MEDIA-SEQUENCE:[0-9]+|EXT-X-PLAYLIST-TYPE:EVENT|EXT-X-INDEPENDENT-SEGMENTS|EXT-X-ENDLIST)",
            line,
        ):
            raise AccessError("invalid_playback_cache", 409)
    if pending is not None:
        raise AccessError("invalid_playback_cache", 409)
    return raw, segments, seconds


def cache_size(directory):
    total = 0
    for name in os.listdir(directory):
        if name not in ("index.m3u8", "index.m3u8.tmp") and not re.fullmatch(
            r"segment-[0-9]{6}\.ts(?:\.tmp)?", name
        ):
            raise AccessError("invalid_playback_cache", 409)
        try:
            st = os.stat(name, dir_fd=directory, follow_symlinks=False)
        except FileNotFoundError:
            # The HLS muxer atomically renames its temporary files during a poll.
            continue
        if not stat.S_ISREG(st.st_mode):
            raise AccessError("invalid_playback_cache", 409)
        total += st.st_size
    return total


def cleanup(store, profile):
    now = time.time()
    jobs = store.rows(
        """SELECT j.* FROM api_playback_jobs j WHERE j.profile=? AND j.state!='running'
        AND (j.state!='stale' OR j.directory_inode IS NOT NULL)
        AND NOT EXISTS(SELECT 1 FROM api_playback_sessions s WHERE s.job_id=j.id AND s.cancelled=0 AND s.expires>?)
        AND (j.state!='ready' OR j.updated_at<?) ORDER BY j.created_at LIMIT 20""",
        (profile, now, now - CACHE_TTL),
    )
    for job in jobs:
        try:
            if job["directory_inode"] is not None:
                with job_directory(store, job) as directory:
                    cache_size(directory)  # Refuse unexpected entries and links.
                    for name in os.listdir(directory):
                        os.unlink(name, dir_fd=directory)
                with destination_root(
                    store, profile, json.loads(job["destination"])
                ) as root:
                    current = os.stat(
                        directory_name(job), dir_fd=root, follow_symlinks=False
                    )
                    if (current.st_dev, current.st_ino) != (
                        job["directory_device"],
                        job["directory_inode"],
                    ):
                        raise AccessError("playback_cache_changed", 409)
                    os.rmdir(directory_name(job), dir_fd=root)
        except FileNotFoundError:
            pass
        except (OSError, CatabolicError):
            continue
        with store.transaction() as db:
            # Retain session/idempotency tombstones; only bounded cache bytes expire.
            db.execute(
                "UPDATE api_playback_jobs SET state='stale',directory_inode=NULL,directory_device=NULL,reserved_bytes=0 WHERE id=?",
                (job["id"],),
            )
