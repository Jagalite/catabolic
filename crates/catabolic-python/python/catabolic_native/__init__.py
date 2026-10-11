"""Opt-in native engine. No catalog is opened or migrated on import."""

import json
import os
import sys
from pathlib import Path

from . import _native
from .app import Application as Application
from .artifacts import Artifacts as Artifacts
from .catalog_refresh import CatalogRefresh as CatalogRefresh
from .curation import Curation as Curation
from .item_workflow import ItemWorkflow as ItemWorkflow
from .media_catalog import MediaCatalog as MediaCatalog
from .processing import Processing as Processing
from .query import CatalogQuery as CatalogQuery
from .saved_queries import Queries as Queries
from .store import CatabolicError as CatabolicError
from .store import NativeCommandHandle as NativeCommandHandle
from .store import NativeResult as NativeResult
from .store import NativeRow as NativeRow
from .store import Store as Store
from .tagging import Tagging as Tagging

__version__ = _native.version()


def _register_packaged_helper():
    root = Path(__file__).resolve().parent
    manifest = root / "native-helper.json"
    executable = root / "bin" / "catabolic-native"
    if not manifest.exists() and not executable.exists():
        # Development extension builds still support in-process operations.
        # Subprocess operations explicitly refuse unless an embedding supplied
        # CATABOLIC_NATIVE_BINARY; they never invoke the Python interpreter.
        return
    if not manifest.is_file() or not executable.is_file():
        raise RuntimeError("incomplete installed Catabolic native helper")
    if manifest.stat().st_size > 4096:
        raise RuntimeError("invalid installed Catabolic native helper manifest")
    metadata = json.loads(manifest.read_text(encoding="utf-8"))
    if (
        metadata.get("format") != 1
        or metadata.get("version") != __version__
        or metadata.get("path") != "bin/catabolic-native"
        or not isinstance(metadata.get("sha256"), str)
        or len(metadata["sha256"]) != 64
    ):
        raise RuntimeError("invalid installed Catabolic native helper manifest")
    _native.register_native_helper(str(executable), metadata["sha256"])


_register_packaged_helper()


def native_binary():
    """Return the explicitly registered helper after rechecking its identity."""
    return Path(_native.native_helper())


def main():
    """Launch the opt-in packaged native CLI, never the Python application."""
    executable = str(native_binary())
    os.execv(executable, [executable, *sys.argv[1:]])


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


def execute_operation(path, surface, operation, *, profile="default", **request):
    """Run one opt-in native operation, with native ownership through completion.

    These companion operations do not replace the complete Catabolic public
    Python API. Unsupported operations fail without retrying under Python.
    """
    return json.loads(
        _native.execute_operation(
            str(path),
            surface,
            operation,
            json.dumps(request, allow_nan=False),
            profile,
        )
    )


def interchange(operation, **request):
    """Validate, decode, encode, or inspect frozen interchange documents."""
    return json.loads(
        _native.interchange(operation, json.dumps(request, allow_nan=False))
    )


def build_manifest(
    path, catalog="global", *, profile="default", extra=None, selection=None
):
    """Build a read-only interchange manifest without publishing filesystem bytes."""
    return json.loads(
        _native.evaluate_read(
            str(path),
            "manifest",
            json.dumps(
                {"catalog": catalog, "extra": extra, "selection": selection},
                allow_nan=False,
            ),
            profile,
        )
    )
