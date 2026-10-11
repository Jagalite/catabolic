"""Persistent native store; no Python sqlite3 connection is created or shared."""

import json
from contextlib import contextmanager
from pathlib import Path

from . import _native

CatabolicError = _native.CatabolicError


def encode(value):
    return _native.encode_json(json.dumps(value, ensure_ascii=False, allow_nan=False))


def _parameter(value):
    if value is None:
        return ["null"]
    if isinstance(value, bool):
        return ["int", str(int(value))]
    if isinstance(value, int):
        return ["int", str(value)]
    if isinstance(value, float):
        return ["float", repr(value)]
    if isinstance(value, str):
        return ["str", value]
    if isinstance(value, (bytes, bytearray, memoryview)):
        return ["bytes", list(bytes(value))]
    raise TypeError(f"unsupported native SQL parameter: {type(value).__name__}")


def _value(value):
    kind = value[0]
    if kind == "null":
        return None
    return {"int": int, "float": float, "str": str, "bytes": bytes}[kind](value[1])


class NativeRow:
    """Native result row with indexed and named access, plus dict(row) support."""

    def __init__(self, columns, values):
        self._columns, self._values = tuple(columns), tuple(values)

    def __getitem__(self, key):
        if isinstance(key, str):
            try:
                index = next(
                    i
                    for i, name in enumerate(self._columns)
                    if name.lower() == key.lower()
                )
            except StopIteration:
                raise IndexError("No item with that key") from None
            return self._values[index]
        return self._values[key]

    def keys(self):
        return list(self._columns)

    def __iter__(self):
        return iter(self._values)

    def __len__(self):
        return len(self._values)


class NativeResult:
    """Materialized native command result, not a sqlite3 cursor."""

    def __init__(self, value):
        columns = value["columns"]
        self.description = (
            tuple((name, None, None, None, None, None, None) for name in columns)
            or None
        )
        self.rowcount = value["rowcount"]
        self.lastrowid = value["lastrowid"]
        self._rows = iter(
            NativeRow(columns, [_value(v) for v in row]) for row in value["rows"]
        )

    def fetchone(self):
        return next(self._rows, None)

    def fetchall(self):
        return list(self._rows)

    def fetchmany(self, size=1):
        result = []
        for _ in range(size):
            row = self.fetchone()
            if row is None:
                break
            result.append(row)
        return result

    def __iter__(self):
        return self._rows


class NativeCommandHandle:
    """Explicit native commands. Transactions belong to Store.transaction().

    Implements execute/executemany and in_transaction only. It deliberately
    provides no connection sharing, cursor factories, raw transaction SQL,
    attach/detach, extensions, or Python SQLite callback registration.
    """

    def __init__(self, store, token=None):
        self._store, self._token = store, token

    def execute(self, sql, parameters=()):
        args = (
            {k: _parameter(v) for k, v in parameters.items()}
            if isinstance(parameters, dict)
            else [_parameter(v) for v in parameters]
        )
        return NativeResult(
            json.loads(self._store._owner.command(sql, json.dumps(args), self._token))
        )

    def executemany(self, sql, parameters):
        result = None
        changed = 0
        for args in parameters:
            result = self.execute(sql, args)
            changed += max(result.rowcount, 0)
        if result is None:
            result = NativeResult(
                {"columns": [], "rows": [], "rowcount": 0, "lastrowid": None}
            )
        result.rowcount = changed
        return result

    @property
    def in_transaction(self):
        self._store._owner.check_scope(self._token)
        return self._store._owner.in_transaction()


class Store:
    def __init__(
        self, path: str | Path, *, writable: bool = False, for_recovery: bool = False
    ):
        self._owner = _native.NativeStore(str(path), writable, for_recovery)
        info = json.loads(self._owner.info())
        self.path = Path(info["path"])
        self.writable = info["writable"]
        self.schema_version = info["schema_version"]
        self.database_id = info["database_id"]
        self.after_close, self.after_reconnect = {}, {}
        self._commands = NativeCommandHandle(self)

    @property
    def db(self):
        return None if json.loads(self._owner.info())["closed"] else self._commands

    @classmethod
    def initialize(cls, path: str | Path) -> dict:
        return json.loads(_native.initialize_database(str(path)))

    def close(self):
        self._owner.close()
        callbacks, self.after_close = self.after_close, {}
        for callback in callbacks.values():
            callback()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()

    @contextmanager
    def transaction(self):
        token = self._owner.begin()
        try:
            yield NativeCommandHandle(self, token)
        except BaseException as original:
            try:
                self._owner.finish(token, False)
            except BaseException as cleanup:
                original.add_note(f"native transaction cleanup failed: {cleanup}")
            raise
        else:
            self._owner.finish(token, True)

    @contextmanager
    def detached(self):
        self._owner.detach()
        try:
            yield
        finally:
            self._owner.reconnect()
            try:
                for callback in tuple(self.after_reconnect.values()):
                    callback()
            except BaseException:
                self._owner.close()
                raise

    def rows(self, sql: str, args: tuple = ()) -> list[dict]:
        return [dict(row) for row in self._commands.execute(sql, args)]

    def _handle(self, handle):
        if not isinstance(handle, NativeCommandHandle) or handle._store is not self:
            raise CatabolicError("expected a command handle owned by this native Store")
        self._owner.check_scope(handle._token)

    def _call(self, surface, operation, request, profile="default"):
        return json.loads(
            self._owner.call(
                surface, operation, json.dumps(request, allow_nan=False), profile
            )
        )
