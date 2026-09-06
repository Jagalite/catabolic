"""Domain validation shared by every application entry point."""

from __future__ import annotations

import re
from dataclasses import asdict, dataclass
from pathlib import PurePosixPath


class CatabolicError(Exception):
    """An actionable application error suitable for terminal output."""


def name(value: str) -> str:
    if not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9_.-]{0,63}", value):
        raise CatabolicError(
            "names must be 1–64 letters, digits, dots, underscores, or hyphens"
        )
    return value


def relative_path(value: str) -> str:
    if not value or "\x00" in value or "\\" in value:
        raise CatabolicError("destination must be a nonempty relative POSIX path")
    parts = value.split("/")
    if any(part in ("", ".", "..") for part in parts):
        raise CatabolicError(
            "relative paths cannot contain empty, dot, or parent components"
        )
    if any(part.startswith(".catabolic") for part in parts):
        raise CatabolicError(".catabolic names are reserved")
    try:
        value.encode("utf-8")
    except UnicodeError as exc:
        raise CatabolicError("paths must be valid UTF-8") from exc
    if PurePosixPath(value).is_absolute():
        raise CatabolicError("path must be relative")
    return value


def source_health(size: int, path: str) -> str | None:
    if size == 0:
        return "empty"
    if path.lower().endswith(
        (
            ".!qb",
            ".aria2",
            ".crdownload",
            ".download",
            ".incomplete",
            ".part",
            ".partial",
            ".tmp",
        )
    ):
        return "incomplete_download"
    return None


@dataclass(frozen=True)
class Action:
    kind: str
    catalog: str
    path: str
    target: str | None = None
    previous: str | None = None
    reason: str | None = None

    def to_dict(self) -> dict:
        return asdict(self)
