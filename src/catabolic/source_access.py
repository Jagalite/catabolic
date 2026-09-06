# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Shared descriptor-based validation of inventoried source revisions."""

import os
import stat
from contextlib import contextmanager

from .domain import CatabolicError
from .filesystem import parent_handle, root_handle


@contextmanager
def validated_source(snapshot):
    if snapshot.get("status") != "present" or not snapshot.get("root"):
        raise CatabolicError(
            "source is not recorded present; scan the mounted source first"
        )
    binding = {
        "root": snapshot["root"],
        "device": snapshot["root_device"],
        "inode": snapshot["root_inode"],
    }
    with root_handle(binding) as root:
        with parent_handle(root, snapshot["path"]) as (parent, leaf):
            fd = os.open(
                leaf, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent
            )
            try:
                before = os.fstat(fd)
                if (
                    not stat.S_ISREG(before.st_mode)
                    or any(
                        getattr(before, "st_" + k) != snapshot[k]
                        for k in ("size", "mtime_ns")
                    )
                    or before.st_ino != snapshot["inode"]
                    or before.st_dev != snapshot["device"]
                ):
                    raise CatabolicError(
                        "source changed since scan; rescan before reading"
                    )
                if (
                    "ctime_ns" in snapshot
                    and before.st_ctime_ns != snapshot["ctime_ns"]
                ):
                    raise CatabolicError(
                        "source revision changed; rescan and start a new operation"
                    )
                yield fd
                after = os.fstat(fd)
                named = os.stat(leaf, dir_fd=parent, follow_symlinks=False)
                keys = ("st_size", "st_mtime_ns", "st_ctime_ns", "st_dev", "st_ino")
                if any(
                    getattr(before, k) != getattr(after, k)
                    or getattr(before, k) != getattr(named, k)
                    for k in keys
                ):
                    raise CatabolicError(
                        "source changed while reading; result discarded"
                    )
                with root_handle(binding):
                    pass
            finally:
                os.close(fd)
