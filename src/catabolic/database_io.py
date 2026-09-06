"""Shared connection and writer-lock rules for ordinary use and upgrades."""

import fcntl
import os
import sqlite3
import stat
from contextlib import contextmanager
from pathlib import Path

from .domain import CatabolicError


def database_path(path: str | Path) -> Path:
    requested = Path(path).absolute()
    resolved = requested.parent.resolve() / requested.name
    if resolved.is_symlink() or not resolved.is_file():
        raise CatabolicError(
            f"database does not exist or is a symlink: {resolved}; use init explicitly"
        )
    if resolved.stat().st_nlink != 1:
        raise CatabolicError(
            "hard-linked databases are unsupported; use one canonical database path"
        )
    return resolved


def acquire_writer_lock(path: Path) -> int:
    fd = os.open(str(path) + ".lock", os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        st = os.fstat(fd)
        if not stat.S_ISREG(st.st_mode) or st.st_nlink != 1:
            raise CatabolicError("database lock must be a regular file with one link")
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return fd
    except BlockingIOError as exc:
        os.close(fd)
        raise CatabolicError(
            "another Catabolic writer or upgrade is using this database"
        ) from exc
    except BaseException:
        os.close(fd)
        raise


@contextmanager
def writer_lock(path: Path):
    fd = acquire_writer_lock(path)
    try:
        yield
    finally:
        os.close(fd)


def connect_database(path: Path, *, writable: bool = False) -> sqlite3.Connection:
    mode = "rw" if writable else "ro"
    db = sqlite3.connect(
        path.as_uri() + f"?mode={mode}", uri=True, isolation_level=None
    )
    try:
        db.row_factory = sqlite3.Row
        db.execute("PRAGMA foreign_keys=ON")
        db.execute("PRAGMA busy_timeout=5000")
        return db
    except BaseException:
        db.close()
        raise
