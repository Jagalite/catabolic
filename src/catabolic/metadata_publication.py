# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Shared destination metadata value validation and local writer serialization."""

import os
from datetime import date
from functools import wraps
from pathlib import Path

from .consumer_adapters import ConsumerError


def serialize_writes(function):
    @wraps(function)
    def wrapped(database, *args, **kwargs):
        if not kwargs.get("apply", False):
            return function(database, *args, **kwargs)
        from .database_io import acquire_writer_lock
        from .domain import CatabolicError

        path = Path(database).expanduser().resolve()
        try:
            fd = acquire_writer_lock(path.with_name(path.name + ".plex-metadata"))
        except CatabolicError:
            raise ConsumerError("metadata_writer_busy_preview_again") from None
        try:
            return function(database, *args, **kwargs)
        finally:
            os.close(fd)

    return wrapped


def value(field, raw):
    if field == "year":
        if type(raw) is not int or not 1 <= raw <= 9999:
            raise ConsumerError("metadata_invalid_year")
        return raw
    if (
        not isinstance(raw, str)
        or len(raw) > 8192
        or any(ord(c) < 32 and c not in "\n\r\t" for c in raw)
        or (field == "title" and not raw.strip())
    ):
        raise ConsumerError("metadata_invalid_" + field)
    if field == "release_date":
        try:
            if date.fromisoformat(raw).isoformat() != raw:
                raise ValueError
        except ValueError:
            raise ConsumerError("metadata_invalid_release_date") from None
    return raw
