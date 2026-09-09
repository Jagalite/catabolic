# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Versioned persistence. Opening a database never initializes or migrates it."""

from __future__ import annotations

import json
import os
from contextlib import contextmanager
from pathlib import Path

from .database_io import acquire_writer_lock, connect_database, database_path
from .domain import CatabolicError
from .migration import (
    SCHEMA_VERSION,
    initialize_database,
    load_migrations,
    validate_database,
)

# Reviewed recovery adapters exist for these schemas. New journal versions must
# be admitted explicitly, with compatibility tests, rather than by a numeric range.
RECOVERABLE_SCHEMAS = frozenset(
    {1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17}
)


class Store:
    def __init__(
        self, path: str | Path, *, writable: bool = False, for_recovery: bool = False
    ):
        self.writable = writable
        self.path = database_path(path)
        self.lock_fd = None
        self.db = None
        self.after_close = {}
        try:
            if writable:
                self.lock_fd = acquire_writer_lock(self.path)
            self.db = connect_database(self.path, writable=writable)
            if not writable:
                self.db.execute("BEGIN")
            version = self.db.execute("PRAGMA user_version").fetchone()[0]
            self.schema_version = version
            legacy_recovery = for_recovery and version in RECOVERABLE_SCHEMAS
            if version != SCHEMA_VERSION and not legacy_recovery:
                if 1 <= version < SCHEMA_VERSION:
                    raise CatabolicError(
                        f"database schema {version} needs an upgrade to {SCHEMA_VERSION}; run catabolic --db {self.path} db upgrade"
                    )
                raise CatabolicError(
                    f"unsupported database schema {version}; no automatic migration was attempted"
                )
            info = validate_database(self.db, load_migrations())
            self.database_id = info["database_id"]
        except BaseException:
            self.close()
            raise

    @contextmanager
    def detached(self):
        """Release this session for slow work, retaining deferred publication callbacks."""
        import time

        if self.db.in_transaction:
            raise CatabolicError("cannot detach an active transaction")
        self.db.close()
        self.db = None
        if self.lock_fd is not None:
            os.close(self.lock_fd)
            self.lock_fd = None
        try:
            yield
        finally:
            deadline = time.monotonic() + 5
            while True:
                try:
                    if self.writable:
                        self.lock_fd = acquire_writer_lock(self.path)
                    break
                except CatabolicError:
                    if time.monotonic() >= deadline:
                        raise
                    time.sleep(0.02)
            try:
                self.db = connect_database(self.path, writable=self.writable)
                info = validate_database(self.db, load_migrations())
                if info["database_id"] != self.database_id:
                    raise CatabolicError("catalog replaced during detached execution")
            except BaseException:
                if self.db is not None:
                    self.db.close()
                    self.db = None
                if self.lock_fd is not None:
                    os.close(self.lock_fd)
                    self.lock_fd = None
                raise

    @classmethod
    def initialize(cls, path: str | Path) -> dict:
        return initialize_database(path)

    def close(self):
        if self.db is not None:
            self.db.close()
            self.db = None
        if self.lock_fd is not None:
            os.close(self.lock_fd)
            self.lock_fd = None
        callbacks, self.after_close = self.after_close, {}
        for callback in callbacks.values():
            callback()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()

    @contextmanager
    def transaction(self):
        if self.db.in_transaction:
            if not self.writable:
                raise CatabolicError(
                    "a read-only session cannot start a write transaction"
                )
            from uuid import uuid4

            savepoint = "nested_" + uuid4().hex
            self.db.execute("SAVEPOINT " + savepoint)
            try:
                yield self.db
            except BaseException:
                self.db.execute("ROLLBACK TO " + savepoint)
                self.db.execute("RELEASE " + savepoint)
                raise
            else:
                self.db.execute("RELEASE " + savepoint)
            return
        self.db.execute("BEGIN IMMEDIATE")
        try:
            yield self.db
        except BaseException:
            self.db.rollback()
            raise
        else:
            self.db.commit()

    def rows(self, sql: str, args: tuple = ()) -> list[dict]:
        return [dict(row) for row in self.db.execute(sql, args)]


def encode(value: object) -> str:
    return json.dumps(value, sort_keys=True, ensure_ascii=False, allow_nan=False)
