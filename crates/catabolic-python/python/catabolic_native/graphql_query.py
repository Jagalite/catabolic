"""Bounded GraphQL using the native resolver owner and persistent snapshots."""

import json

from . import _native
from .store import CatabolicError, Store, encode


def execute_graphql(
    path,
    document,
    *,
    profile="default",
    variables=None,
    operation_name=None,
    timeout_ms=5000,
    _store=None,
    _context_factory=None,
):
    if type(timeout_ms) is not int or not 1 <= timeout_ms <= 60000:
        raise CatabolicError("timeout-ms must be between 1 and 60000")
    if not isinstance(document, str) or len(document.encode()) > 131072:
        raise CatabolicError("query document must be at most 131072 bytes")
    if isinstance(variables, str):
        if len(variables.encode()) > 131072:
            raise CatabolicError("variables exceed input budget")
        variables = json.loads(variables)
    if variables is not None and not isinstance(variables, dict):
        raise CatabolicError("variables must be a JSON object")
    if variables is not None and len(encode(variables).encode()) > 131072:
        raise CatabolicError("variables exceed input budget")
    if _context_factory is not None:
        raise CatabolicError(
            "_context_factory is unsupported by the native GraphQL facade; "
            "Python resolver/access callbacks cannot be transferred to a native snapshot"
        )
    if _store is not None and not isinstance(_store, Store):
        raise CatabolicError("_store requires a catabolic_native.store.Store")
    request = dict(
        document=document,
        variables={} if variables is None else variables,
        operation_name=operation_name,
        timeout_ms=timeout_ms,
    )
    validation = json.loads(
        _native.NativeStore.validate_query(
            "graphql", json.dumps(request, allow_nan=False), profile
        )
    )
    if validation is not None:
        return validation
    if _store is not None:
        return _store._call("query_execution", "graphql", request, profile)
    with Store(path) as store:
        return store._call("query_execution", "graphql", request, profile)
