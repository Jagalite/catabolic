#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Selected pure revision facade checks; no content opening or authorization.

Run this script in separate frozen and installed-native processes. Its output
is selected development evidence, not a release/aggregate qualification report.
The owning wheel gate must bind the artifacts and compare the fixed groups.
"""

import argparse
import importlib
import inspect
import json
import os
import signal
import sys
import tempfile
from pathlib import Path

if not __debug__ or os.environ.get("PYTHONOPTIMIZE"):
    raise SystemExit("revision checks require assertions; do not run with -O or -OO")

CONTRACTS = (
    "python:catabolic.access.AccessError",
    "python:catabolic.access.AccessError.__init__",
    "python:catabolic.content_access.revision_of",
)
EXPECTED_GROUPS = (
    "signatures",
    "access_error",
    "unobserved",
    "observed",
    "alternate_profile",
    "missing",
    "revision_changes",
    "policy",
    "volume",
    "nested_rollback",
    "outer_rollback",
    "readonly",
)


def fixture(store):
    # All values are synthetic database facts. No source path is opened.
    with store.transaction() as db:
        db.execute("INSERT INTO profiles VALUES ('alternate')")
        db.execute("INSERT INTO locations VALUES ('media')")
        db.execute("INSERT INTO files VALUES ('file','media','snow-雪.txt')")


def observed(store):
    with store.transaction() as db:
        db.execute(
            "INSERT INTO scans(id,profile,location,complete,observed,errors) "
            "VALUES ('scan','default','media',1,1,'[]')"
        )
        db.execute(
            "INSERT INTO observations VALUES "
            "('default','file',17,9007199254740993,101,202,'present','scan')"
        )
        db.execute(
            "INSERT INTO bindings VALUES "
            "('default','source','media','/synthetic-revision-root/雪',101,303)"
        )


def scenario(namespace, path):
    Store = importlib.import_module(namespace + ".store").Store
    revision_of = importlib.import_module(namespace + ".content_access").revision_of
    AccessError = importlib.import_module(namespace + ".access").AccessError
    Store.initialize(path)
    values = {
        "signatures": [
            str(inspect.signature(revision_of)),
            str(inspect.signature(AccessError.__init__)),
        ],
        "access_error": [
            [str(error), error.code, error.status]
            for error in (AccessError(), AccessError("not_found", 404))
        ],
    }
    with Store(path, writable=True) as store:
        fixture(store)
        values["unobserved"] = revision_of(store, "default", "file")
        observed(store)
        values["observed"] = revision_of(store, "default", "file")
        assert values["observed"] != values["unobserved"]
        values["alternate_profile"] = revision_of(store, "alternate", "file")
        try:
            revision_of(store, "default", "unknown")
        except AccessError as error:
            values["missing"] = [
                type(error).__name__,
                str(error),
                error.code,
                error.status,
            ]
        else:
            raise AssertionError("unknown file must fail")
        changes = []
        for column, value in (
            ("size", 18),
            ("mtime_ns", 9007199254740995),
            ("device", 102),
            ("inode", 204),
            ("status", "missing"),
        ):
            with store.transaction() as db:
                db.execute(
                    "UPDATE observations SET " + column + "=? WHERE file_id='file'",
                    (value,),
                )
            current = revision_of(store, "default", "file")
            assert current not in changes + [values["observed"]]
            changes.append(current)
        values["revision_changes"] = changes
        with store.transaction() as db:
            db.execute(
                "INSERT INTO source_identity_policies(profile,location,identity_policy) "
                "VALUES ('default','media','path')"
            )
        values["policy"] = revision_of(store, "default", "file")
        with store.transaction() as db:
            db.execute(
                "INSERT INTO binding_volumes VALUES ('default','source','media','fake-volume')"
            )
        values["volume"] = revision_of(store, "default", "file")
        try:
            with store.transaction() as outer:
                outer.execute("UPDATE files SET path='outer' WHERE id='file'")
                before = revision_of(store, "default", "file")
                try:
                    with store.transaction() as inner:
                        inner.execute("UPDATE files SET path='inner' WHERE id='file'")
                        during = revision_of(store, "default", "file")
                        raise LookupError("rollback inner")
                except LookupError:
                    pass
                after = revision_of(store, "default", "file")
                assert before == after and during != before
                values["nested_rollback"] = [before, during, after]
                raise LookupError("rollback outer")
        except LookupError:
            pass
        values["outer_rollback"] = revision_of(store, "default", "file")
        assert values["outer_rollback"] == values["volume"]
    with Store(path) as readonly:
        values["readonly"] = revision_of(readonly, "default", "file")
    assert set(values) == set(EXPECTED_GROUPS)
    return values


def native_ownership(path):
    from catabolic_native.access import AccessError
    from catabolic_native.content_access import revision_of
    from catabolic_native.store import CatabolicError, Store

    checks = []
    with Store(path, writable=True) as store:
        expected = revision_of(store, "default", "file")
        try:
            with store.transaction() as db:
                db.execute("INSERT INTO files VALUES ('pending','media','pending')")
                assert len(revision_of(store, "default", "pending")) == 64
                raise LookupError("rollback new row")
        except LookupError:
            pass
        try:
            revision_of(store, "default", "pending")
        except AccessError as error:
            assert (error.code, error.status) == ("not_found", 404)
        else:
            raise AssertionError("rollback leaked an inserted row")
        checks.append("same-owner-uncommitted-row-and-rollback")
        with store.detached():
            try:
                revision_of(store, "default", "file")
            except CatabolicError as error:
                assert "detached" in str(error)
            else:
                raise AssertionError("detached owner was silently reopened")
        assert revision_of(store, "default", "file") == expected
        checks.append("detached-refusal-and-reconnect")

        # An ordinary Python interrupt must unwind the caller's transaction and
        # leave the persistent owner usable. This is not a timing-complete proof.
        previous = signal.getsignal(signal.SIGALRM)

        def interrupted(signum, frame):
            raise KeyboardInterrupt("revision-fixture-interrupt")

        signal.signal(signal.SIGALRM, interrupted)
        try:
            try:
                with store.transaction() as db:
                    db.execute("UPDATE files SET path='interrupted' WHERE id='file'")
                    signal.setitimer(signal.ITIMER_REAL, 0.02)
                    while True:
                        revision_of(store, "default", "file")
            except KeyboardInterrupt as error:
                assert str(error) == "revision-fixture-interrupt"
        finally:
            signal.setitimer(signal.ITIMER_REAL, 0)
            signal.signal(signal.SIGALRM, previous)
        assert revision_of(store, "default", "file") == expected
        checks.append("ordinary-interrupt-rollback-and-reuse")
    for file_id in ("file", "unknown"):
        try:
            revision_of(store, "default", file_id)
        except CatabolicError as error:
            assert "closed" in str(error)
            assert not isinstance(error, AccessError)
        else:
            raise AssertionError("closed owner was silently reopened")
    checks.append("closed-before-lookup-precedence")
    with Store(path, writable=True) as writer:
        writer.db.execute("PRAGMA journal_mode=WAL")
        with Store(path) as reader:
            before = revision_of(reader, "default", "file")
            with writer.transaction() as db:
                db.execute("UPDATE files SET path='newer-snapshot' WHERE id='file'")
            assert revision_of(reader, "default", "file") == before
            assert revision_of(writer, "default", "file") != before
    checks.append("caller-pinned-readonly-snapshot")
    return checks


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--engine", choices=("reference", "native"), required=True)
    parser.add_argument("--reference-src", type=Path)
    args = parser.parse_args()
    if args.engine == "reference":
        assert tuple(sys.version_info[:3]) == (3, 14, 6), (
            "oracle requires Python 3.14.6"
        )
        assert args.reference_src is not None
        sys.path.insert(0, str(args.reference_src))
        namespace = "catabolic"
    else:
        namespace = "catabolic_native"
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / "catalog.db"
        result = scenario(namespace, path)
        checks = native_ownership(path) if args.engine == "native" else []
    print(json.dumps({"groups": result, "native_checks": checks}, sort_keys=True))


if __name__ == "__main__":
    main()
