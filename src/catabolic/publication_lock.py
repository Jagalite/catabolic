# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Serialize publication/recovery while candidate probes release the DB writer."""

import os
from contextvars import ContextVar
from functools import wraps
from pathlib import Path

from .database_io import acquire_writer_lock

_HELD = ContextVar("catabolic_publication_locks", default=frozenset())


def serialized(method):
    @wraps(method)
    def run(self, *args, **kwargs):
        path = Path(self.store.path)
        if str(path) in _HELD.get():
            return method(self, *args, **kwargs)
        descriptor = acquire_writer_lock(path.with_name(path.name + ".publication"))
        token = _HELD.set(_HELD.get() | {str(path)})
        try:
            return method(self, *args, **kwargs)
        finally:
            _HELD.reset(token)
            os.close(descriptor)

    return run
