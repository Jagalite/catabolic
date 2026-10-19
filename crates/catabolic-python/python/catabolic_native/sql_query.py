"""Bounded read-only SQL, borrowing a native Store when explicitly supplied."""

import json

from . import _native
from .store import CatabolicError, Store


def execute_sql(
    path,
    sql: str | None = None,
    *,
    profile="default",
    params=None,
    describe=False,
    max_rows=1000,
    timeout_ms=5000,
    _store=None,
    _stable=False,
    _http=False,
):
    if type(max_rows) is not int or not 1 <= max_rows <= 10000:
        raise CatabolicError("max-rows must be between 1 and 10000")
    if type(timeout_ms) is not int or not 1 <= timeout_ms <= 60000:
        raise CatabolicError("timeout-ms must be between 1 and 60000")
    if params is not None and not isinstance(params, str):
        raise CatabolicError(
            "--params must be a JSON object with named scalar values; profile is reserved, numbers must be finite and integers fit signed 64 bits"
        )
    if _store is not None and not isinstance(_store, Store):
        raise CatabolicError("_store requires a catabolic_native.store.Store")
    request = dict(
        sql=sql,
        params=params,
        describe=bool(describe),
        max_rows=max_rows,
        timeout_ms=timeout_ms,
        _stable=bool(_stable),
        _http=bool(_http),
    )
    validation = json.loads(
        _native.NativeStore.validate_query(
            "sql", json.dumps(request, allow_nan=False), profile
        )
    )
    if validation is not None:
        return validation
    if _store is not None:
        return _store._call("query_execution", "sql", request, profile)
    with Store(path) as store:
        return store._call("query_execution", "sql", request, profile)
