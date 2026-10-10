"""Opt-in native engine. No catalog is opened or migrated on import."""

import json

from . import _native

__version__ = _native.version()


def encode(value):
    return _native.encode_json(json.dumps(value, allow_nan=False))


def initialize_database(path):
    return json.loads(_native.initialize_database(str(path)))


def inspect_database(path, *, full=False):
    return json.loads(_native.inspect_database(str(path), full))


def upgrade_database(path, *, dry_run=False, backup_dir=None):
    return json.loads(
        _native.upgrade_database(
            str(path), dry_run, str(backup_dir) if backup_dir is not None else None
        )
    )


def execute_sql(
    path,
    sql=None,
    *,
    profile="default",
    params=None,
    describe=False,
    max_rows=1000,
    timeout_ms=5000,
    _stable=False,
    _http=False,
):
    return json.loads(
        _native.execute_sql(
            str(path),
            sql,
            profile,
            params,
            describe,
            max_rows,
            timeout_ms,
            _stable,
            _http,
        )
    )


def catalog_query(path, entity, *, profile="default", **options):
    return json.loads(
        _native.catalog_query(
            str(path), entity, json.dumps(options, allow_nan=False), profile
        )
    )


def execute_graphql(
    path,
    document,
    *,
    profile="default",
    variables=None,
    operation_name=None,
    timeout_ms=5000,
):
    return json.loads(
        _native.execute_graphql(
            str(path),
            document,
            profile,
            json.dumps(variables or {}, allow_nan=False),
            operation_name,
            timeout_ms,
        )
    )


def select_ids(path, selection, *, profile="default"):
    return json.loads(
        _native.evaluate_read(
            str(path), "select", json.dumps(selection, allow_nan=False), profile
        )
    )


def run_query(path, identifier, *, profile="default", limit=1000):
    return json.loads(
        _native.evaluate_read(
            str(path), "query", json.dumps({"id": identifier, "limit": limit}), profile
        )
    )


def plan_layout(
    path,
    identifier,
    catalog="global",
    *,
    profile="default",
    replace_layout=False,
    allow_empty=False,
    limit=100,
):
    return json.loads(
        _native.evaluate_read(
            str(path),
            "layout",
            json.dumps(
                {
                    "id": identifier,
                    "catalog": catalog,
                    "replace_layout": replace_layout,
                    "allow_empty": allow_empty,
                    "limit": limit,
                }
            ),
            profile,
        )
    )
