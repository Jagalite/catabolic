"""POSIX directory-relative operations; never traverse a symbolic-link parent."""

from __future__ import annotations

import json
import os
import stat
import sys
from contextlib import contextmanager
from pathlib import Path

from .domain import CatabolicError, relative_path
from .store import encode

DIRECTORY_FLAGS = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
MARKER = ".catabolic-owner.json"


def rename_noreplace(source_fd, source_name, destination_fd, destination_name):
    """Atomic retirement must never overwrite even a concurrently created file."""
    import ctypes

    library = ctypes.CDLL(None, use_errno=True)
    if sys.platform == "darwin":
        symbol, flags = "renameatx_np", 0x00000004  # SDK sys/stdio.h: RENAME_EXCL
    elif sys.platform.startswith("linux"):
        symbol, flags = "renameat2", 1  # Linux RENAME_NOREPLACE
    else:
        raise CatabolicError(
            "safe hardlink retirement requires macOS or Linux exclusive rename support"
        )
    function = getattr(library, symbol, None)
    if function is None:
        raise CatabolicError(
            "exclusive rename is unavailable; hardlink retirement refused without an unsafe fallback"
        )
    function.argtypes = [
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_uint,
    ]
    function.restype = ctypes.c_int
    if function(
        source_fd,
        os.fsencode(source_name),
        destination_fd,
        os.fsencode(destination_name),
        flags,
    ):
        error = ctypes.get_errno()
        raise CatabolicError(
            f"exclusive hardlink retirement refused: {os.strerror(error)}; destination was not overwritten"
        )


def open_directory(path: str | Path) -> int:
    path = Path(path)
    if not path.is_absolute() or ".." in path.parts:
        raise CatabolicError("root must be absolute and contain no parent traversal")
    fd = os.open("/", DIRECTORY_FLAGS)
    try:
        for component in path.parts[1:]:
            child = os.open(component, DIRECTORY_FLAGS, dir_fd=fd)
            os.close(fd)
            fd = child
        return fd
    except BaseException:
        os.close(fd)
        raise


@contextmanager
def root_handle(binding: dict):
    try:
        fd = open_directory(binding["root"])
    except OSError as exc:
        raise CatabolicError(
            f"unavailable {binding['kind']} root: {binding['root']}: {exc}"
        ) from exc
    try:
        st = os.fstat(fd)
        if (st.st_dev, st.st_ino) != (binding["device"], binding["inode"]):
            raise CatabolicError(
                f"root identity changed: {binding['root']}; explicitly rebind it"
            )
        yield fd
    finally:
        os.close(fd)


@contextmanager
def parent_handle(root_fd: int, path: str, *, create: bool = False):
    parts = relative_path(path).split("/")
    fd = os.dup(root_fd)
    try:
        for component in parts[:-1]:
            try:
                child = os.open(component, DIRECTORY_FLAGS, dir_fd=fd)
            except FileNotFoundError:
                if not create:
                    raise
                try:
                    os.mkdir(component, mode=0o755, dir_fd=fd)
                    os.fsync(fd)
                except FileExistsError:
                    pass
                child = os.open(component, DIRECTORY_FLAGS, dir_fd=fd)
            os.close(fd)
            fd = child
        yield fd, parts[-1]
    finally:
        os.close(fd)


def link_state(root_fd: int, path: str) -> tuple[str, str | None]:
    try:
        with parent_handle(root_fd, path) as (parent, leaf):
            st = os.stat(leaf, dir_fd=parent, follow_symlinks=False)
            if stat.S_ISLNK(st.st_mode):
                return "link", os.readlink(leaf, dir_fd=parent)
            return "other", None
    except FileNotFoundError:
        return "absent", None
    except OSError as exc:
        raise CatabolicError(f"unsafe output parent for {path}: {exc}") from exc


def source_stat(root_fd: int, path: str):
    with parent_handle(root_fd, path) as (parent, leaf):
        st = os.stat(leaf, dir_fd=parent, follow_symlinks=False)
        if not stat.S_ISREG(st.st_mode):
            raise CatabolicError(f"source is no longer a regular file: {path}")
        return st


def owner_state(root_fd: int, expected: dict) -> str:
    try:
        fd = os.open(
            MARKER, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=root_fd
        )
    except FileNotFoundError:
        return "empty" if not os.listdir(root_fd) else "unowned"
    except OSError as exc:
        raise CatabolicError(f"unsafe output ownership marker: {exc}") from exc
    with os.fdopen(fd) as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise CatabolicError("output ownership marker is not a regular file")
        try:
            actual = json.loads(stream.read(8193))
        except (ValueError, UnicodeError) as exc:
            raise CatabolicError("invalid output ownership marker") from exc
    if actual != expected:
        raise CatabolicError(
            "output belongs to a different database, profile, or catalog"
        )
    return "owned"


def claim_output(root_fd: int, expected: dict, operation_id: str):
    temporary = f".catabolic-claim-{operation_id}"
    entries = os.listdir(root_fd)
    if MARKER in entries:
        if owner_state(root_fd, expected) != "owned":
            raise CatabolicError("output ownership could not be confirmed")
        if temporary in entries:
            _remove_claim_temp(root_fd, temporary)
        return
    if set(entries) - {temporary}:
        raise CatabolicError("only an empty output can be claimed")
    if temporary in entries:
        _remove_claim_temp(root_fd, temporary)
    fd = os.open(
        temporary,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
        0o600,
        dir_fd=root_fd,
    )
    with os.fdopen(fd, "w") as stream:
        stream.write(encode(expected) + "\n")
        stream.flush()
        os.fsync(stream.fileno())
    # Publishing a fully written marker with link() cannot overwrite an entry.
    os.link(
        temporary, MARKER, src_dir_fd=root_fd, dst_dir_fd=root_fd, follow_symlinks=False
    )
    os.fsync(root_fd)
    os.unlink(temporary, dir_fd=root_fd)
    os.fsync(root_fd)


def _remove_claim_temp(root_fd: int, temporary: str):
    st = os.stat(temporary, dir_fd=root_fd, follow_symlinks=False)
    if not stat.S_ISREG(st.st_mode):
        raise CatabolicError("claim recovery file was changed externally")
    os.unlink(temporary, dir_fd=root_fd)


def walk_files(root_fd: int, *, exclude: tuple[str, ...] = (), sink=None):
    observed = []
    errors = []
    omitted = 0

    def error(message):
        nonlocal omitted
        if len(errors) < 1000:
            errors.append(message[:4096])
        else:
            omitted += 1

    def visit(fd, prefix):
        before = os.fstat(fd)
        try:
            with os.scandir(fd) as entries:
                for entry in entries:
                    leaf = entry.name
                    path = f"{prefix}/{leaf}" if prefix else leaf
                    if path in exclude:
                        continue
                    try:
                        relative_path(path)
                        st = os.stat(leaf, dir_fd=fd, follow_symlinks=False)
                        if stat.S_ISDIR(st.st_mode):
                            if st.st_dev != before.st_dev:
                                raise CatabolicError(
                                    "nested filesystem boundary; register this mount as a separate location"
                                )
                            child = os.open(leaf, DIRECTORY_FLAGS, dir_fd=fd)
                            try:
                                now = os.fstat(child)
                                if (st.st_dev, st.st_ino) != (now.st_dev, now.st_ino):
                                    raise CatabolicError(
                                        "directory changed during scan"
                                    )
                                visit(child, path)
                            finally:
                                os.close(child)
                        elif stat.S_ISREG(st.st_mode):
                            observation = {
                                "path": path,
                                "size": st.st_size,
                                "mtime_ns": st.st_mtime_ns,
                                "device": st.st_dev,
                                "inode": st.st_ino,
                            }
                            (sink if sink is not None else observed.append)(observation)
                    except (OSError, CatabolicError) as exc:
                        error(f"{path}: {exc}")
        except OSError as exc:
            error(f"{prefix or '.'}: {exc}")
        after = os.fstat(fd)
        if (before.st_mtime_ns, before.st_ctime_ns) != (
            after.st_mtime_ns,
            after.st_ctime_ns,
        ):
            error(f"{prefix or '.'}: directory changed during scan")

    try:
        visit(root_fd, "")
    except RecursionError:
        error("directory nesting exceeds traversal limit")
    if omitted:
        errors.append(f"{omitted} additional traversal errors omitted")
    return observed, errors
