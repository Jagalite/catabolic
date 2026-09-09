# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Revision-pinned catalog content admission and descriptor ownership."""

import hashlib
import mimetypes
import os
import re
from contextlib import contextmanager
from pathlib import Path

from .access import AccessError
from .source_access import validated_source
from .source_trust import source_policy
from .store import encode


def snapshot(store, profile, file_id):
    rows = store.rows(
        """SELECT f.id,f.location,f.path,o.size,o.mtime_ns,o.device,o.inode,o.status,
        b.root,b.device AS root_device,b.inode AS root_inode FROM main.files f
        LEFT JOIN main.observations o ON o.file_id=f.id AND o.profile=?
        LEFT JOIN main.bindings b ON b.profile=? AND b.kind='source' AND b.owner=f.location
        WHERE f.id=?""",
        (profile, profile, file_id),
    )
    if not rows:
        raise AccessError("not_found", 404)
    row = rows[0]
    row["source_policy"] = source_policy(store, profile, row["location"])
    volumes = store.rows(
        "SELECT volume_uuid FROM binding_volumes WHERE profile=? AND kind='source' AND owner=?",
        (profile, row["location"]),
    )
    row["volume_uuid"] = volumes[0]["volume_uuid"] if volumes else None
    return row


def revision_of(store, profile, file_id):
    return hashlib.sha256(
        encode(snapshot(store, profile, file_id)).encode()
    ).hexdigest()


def authorize(access, file_id, revision):
    if not revision or not access.file(file_id, "content:read", revision):
        raise AccessError("not_found", 404)
    row = snapshot(access.store, access.profile, file_id)
    if not access.store.rows(
        "SELECT 1 FROM api_sources WHERE profile=? AND location=?",
        (access.profile, row["location"]),
    ):
        raise AccessError("source_not_exposed")
    filename = Path(row["root"] or "/") / row["path"]
    private = Path.home() / ".config/catabolic/credentials"
    if (
        filename == access.store.path
        or str(filename).startswith(str(access.store.path) + "-")
        or filename.is_relative_to(private)
        or access.store.rows(
            "SELECT 1 FROM api_private_files WHERE path=? OR (device=? AND inode=?)",
            (str(filename), row["device"], row["inode"]),
        )
    ):
        raise AccessError("private_content", 403)
    for connection in access.store.rows(
        "SELECT credential_env FROM consumer_connections WHERE credential_env LIKE 'file:%'"
    ):
        credential = Path(connection["credential_env"][5:])
        try:
            credential_stat = credential.stat()
        except OSError:
            credential_stat = None
        if filename == credential or (
            credential_stat
            and (credential_stat.st_dev, credential_stat.st_ino)
            == (row["device"], row["inode"])
        ):
            raise AccessError("private_content", 403)
    actual = hashlib.sha256(encode(row).encode()).hexdigest()
    if revision != actual:
        raise AccessError("revision_mismatch", 409)
    artifacts = access.store.rows(
        "SELECT state FROM processing_artifacts WHERE file_id=?", (file_id,)
    )
    if artifacts and any(a["state"] != "ready" for a in artifacts):
        raise AccessError("rendition_not_ready", 409)
    return row


@contextmanager
def opened(access, file_id, revision):
    row = authorize(access, file_id, revision)
    with validated_source(row) as fd:
        if os.pread(fd, 16, 0) == b"SQLite format 3\x00":
            raise AccessError("private_content", 403)
        st = os.fstat(fd)
        mime = mimetypes.guess_type(row["path"])[0] or "application/octet-stream"
        yield fd, st, mime


def byte_range(value, size):
    if value is None:
        return 0, size, False
    if value.partition("=")[0].lower() != "bytes":
        return 0, size, False
    match = re.fullmatch(r"bytes=([0-9]*)-([0-9]*)", value, re.IGNORECASE)
    if not match:
        raise AccessError("range_not_satisfiable", 416)
    try:
        left, right = match.groups()
        if left:
            start = int(left)
            end = int(right) if right else size - 1
        else:
            length = int(right)
            if length <= 0:
                raise ValueError
            start = max(0, size - length)
            end = size - 1
        if size == 0 or start < 0 or start >= size or end < start:
            raise ValueError
        end = min(end, size - 1)
        return start, end - start + 1, True
    except ValueError:
        raise AccessError("range_not_satisfiable", 416) from None


def chunks(fd, start, length, stat, buffer_size=65536):
    keys = ("st_dev", "st_ino", "st_size", "st_mtime_ns", "st_ctime_ns")
    offset = start
    while length:
        current = os.fstat(fd)
        if any(getattr(current, k) != getattr(stat, k) for k in keys):
            raise OSError("content changed during transfer")
        data = os.pread(fd, min(buffer_size, length), offset)
        if not data:
            raise OSError("content truncated during transfer")
        yield data
        offset += len(data)
        length -= len(data)
