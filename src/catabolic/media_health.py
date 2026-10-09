# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Small, read-only corruption screen; never a playback certification."""

import json
import os
import stat
from pathlib import PurePosixPath

from .domain import CatabolicError, source_health
from .filesystem import parent_handle

# These containers require a nonzero header. Do not apply this to raw streams,
# disk images or arbitrary files where leading zero padding can be legitimate.
HEADER_EXTENSIONS = frozenset(
    ".mkv .mka .webm .avi .wav .wave .flac .ogg .oga .ogv .opus".split()
)
COVERAGE = "zero-filled container header screen; not a probe or decode"
REVISION_KEYS = ("st_size", "st_mtime_ns", "st_ctime_ns", "st_dev", "st_ino")


def inspect_header(fd, path, size):
    reason = source_health(size, path)
    if reason:
        return {"status": "invalid", "reason": reason, "bytes_read": 0}
    if PurePosixPath(path).suffix.lower() not in HEADER_EXTENSIONS:
        return {"status": "unknown", "reason": "unsupported_header", "bytes_read": 0}
    raw = os.pread(fd, 64, 0)
    if len(raw) != min(size, 64):
        raise CatabolicError("source changed while checking media header")
    invalid = bool(raw) and not any(raw)
    return {
        "status": "invalid" if invalid else "not_detected",
        "reason": "zero_filled_header" if invalid else None,
        "bytes_read": len(raw),
    }


def check_at(parent, leaf, path, expected):
    """Check only the observed regular file; reject replacements and symlinks."""
    reason = source_health(expected.st_size, path)
    if reason:
        # These defects are established by inventory metadata, without reading
        # content. Read permissions cannot make an empty file healthy/unknown.
        return {"status": "invalid", "reason": reason, "bytes_read": 0}
    if PurePosixPath(path).suffix.lower() not in HEADER_EXTENSIONS:
        return {"status": "unknown", "reason": "unsupported_header", "bytes_read": 0}
    fd = os.open(leaf, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
    try:
        before = os.fstat(fd)
        if not stat.S_ISREG(before.st_mode) or any(
            getattr(before, k) != getattr(expected, k) for k in REVISION_KEYS
        ):
            raise CatabolicError("source changed before checking media header")
        result = inspect_header(fd, path, before.st_size)
        after = os.fstat(fd)
        named = os.stat(leaf, dir_fd=parent, follow_symlinks=False)
        if any(
            getattr(before, k) != getattr(after, k)
            or getattr(before, k) != getattr(named, k)
            for k in REVISION_KEYS
        ):
            raise CatabolicError("source changed while checking media header")
        return result
    finally:
        os.close(fd)


def live_reason(root_fd, path, expected):
    with parent_handle(root_fd, path) as (parent, leaf):
        result = check_at(parent, leaf, path, expected)
    return result["reason"] if result["status"] == "invalid" else None


def recorded_reason(store, profile, file_id, current):
    """Use only conclusive probe/decode evidence for this exact live revision.

    Unknown attempts do not erase a confirmed defect. A successful attempt of
    the same operation supersedes its earlier evidence for that revision only
    when its analysis budgets cover the failed attempt.
    """
    if store.schema_version < 8:
        return None
    for operation in ("probe", "decode"):
        rows = store.rows(
            """WITH evidence AS (
            SELECT a.rowid AS sequence,a.state,a.result,j.options FROM processing_jobs j
            JOIN processing_attempts a ON a.job_id=j.id
            WHERE j.profile=? AND j.file_id=? AND j.operation=? AND a.state IN ('failed','complete')
            AND json_extract(snapshot,'$.size')=? AND json_extract(snapshot,'$.mtime_ns')=?
            AND json_extract(snapshot,'$.device')=? AND json_extract(snapshot,'$.inode')=?
            AND json_extract(snapshot,'$.ctime_ns')=?
            ) SELECT bad.result FROM evidence bad
            WHERE json_extract(bad.result,'$.media_health.status')='invalid'
            AND NOT EXISTS (
                SELECT 1 FROM evidence good WHERE good.sequence>bad.sequence
                AND good.state='complete'
                AND json_extract(good.result,'$.media_health.status') IN ('not_detected','content_mismatch')
                AND (?='decode' OR (
                    json_extract(good.options,'$.extractor.adapter')='probe-v3'
                    AND
                    coalesce(json_extract(good.options,'$.analysis_bytes'),0)>=coalesce(json_extract(bad.options,'$.analysis_bytes'),9223372036854775807)
                    AND coalesce(json_extract(good.options,'$.analysis_us'),0)>=coalesce(json_extract(bad.options,'$.analysis_us'),9223372036854775807)
                ))
            ) ORDER BY bad.sequence DESC LIMIT 1""",
            (
                profile,
                file_id,
                operation,
                current.st_size,
                current.st_mtime_ns,
                current.st_dev,
                current.st_ino,
                current.st_ctime_ns,
                operation,
            ),
        )
        if rows:
            health = json.loads(rows[0]["result"])["media_health"]
            if health["status"] == "invalid":
                return health["reason"]
    return None
