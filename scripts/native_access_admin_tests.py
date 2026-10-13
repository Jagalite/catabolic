#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Three-row local administration facade gate; disposable catalogs only.

Generated fixture tokens stay in memory. Reports contain only normalized shape,
state and digest checks, never raw secrets or a credential-file protocol.
"""

import argparse
import functools
import hashlib
import importlib
import inspect
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path


def clear_stale_report(argv=None, parser=None):
    """Invalidate every canonical --report destination before fallible preflight.

    Only --report PATH and --report=PATH opt in; abbreviations are rejected by
    the full parser. Stop at -- and use that parser's option lexer to distinguish
    filename values (including version-specific negative numbers) from options.
    No argv parsing or project imports happen here. Bootstrap copies are checked
    against this implementation by the receipt-boundary tests.
    """
    if parser is None:
        parser = argparse.ArgumentParser(add_help=False, allow_abbrev=False)
        parser.add_argument("--report")
    arguments = sys.argv[1:] if argv is None else argv
    first_error = None
    for index, argument in enumerate(arguments):
        if argument == "--":
            break
        if argument == "--report":
            value = arguments[index + 1] if index + 1 < len(arguments) else ""
            if parser._parse_optional(value) is not None:
                continue
        elif argument.startswith("--report="):
            value = argument.partition("=")[2]
        else:
            continue
        if value:
            report = Path(value)
            for suffix in (None, ".attempt.json"):
                try:
                    target = report if suffix is None else report.with_suffix(suffix)
                    target.unlink(missing_ok=True)
                except (OSError, ValueError) as error:
                    if first_error is None:
                        first_error = error
    if first_error is not None:
        raise first_error


def argument_parser():
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument("--scenario", choices=("reference", "native"))
    parser.add_argument("--root", type=Path)
    parser.add_argument("--reference-root", type=Path, required=True)
    parser.add_argument("--reference-python", type=Path)
    parser.add_argument("--reference-commit")
    parser.add_argument("--inventory", type=Path)
    parser.add_argument("--source-root", type=Path)
    parser.add_argument("--wheel", type=Path)
    parser.add_argument("--build-manifest", type=Path)
    parser.add_argument("--native-binary", type=Path)
    parser.add_argument("--legacy-binary", type=Path)
    parser.add_argument("--report", type=Path)
    parser.add_argument(
        "--dev",
        action="store_true",
        help="explicit helperless development evidence, never strict release qualification",
    )
    return parser


# CLI preflight precedes project imports and optimized-mode refusal.
# Library imports never inspect caller arguments or invalidate their receipts.
if __name__ == "__main__":
    clear_stale_report(parser=argument_parser())

if not __debug__ or os.environ.get("PYTHONOPTIMIZE"):
    raise RuntimeError(
        "access administration qualification requires Python assertions; do not use -O or -OO"
    )

CONTRACTS = {
    "python:catabolic.access.principal_put": "Validation, conflicts, disabled state and persistent ownership",
    "python:catabolic.access.grant_put": "Typed definition transport, sealed preflight, revision and audit rollback",
    "python:catabolic.access.issue": "Exact TTL/list types, digest-only persistence, expiry and rollback",
}
EXPECTED_GROUPS = (
    "signatures",
    "principal",
    "principal_invalid",
    "principal_conflict",
    "principal_disabled",
    "grant",
    "grant_update",
    "grant_conflict",
    "grant_invalid",
    "grant_type_transport",
    "grant_unknown",
    "grant_error_order",
    "sql_preflight",
    "graphql_preflight",
    "private_preflight",
    "missing_preflight",
    "wrong_entity",
    "grant_generated",
    "token",
    "token_invalid",
    "token_type_transport",
    "token_grant_order",
    "token_disabled_principal",
    "scalar_token_persistence",
    "nested_rollback",
    "outer_rollback",
    "audit_failure",
    "readonly_validation",
    "readonly_mutation",
    "final_state",
)


def sibling(name):
    import importlib.util

    spec = importlib.util.spec_from_file_location(
        name, Path(__file__).with_name(name + ".py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def trace():
    from catabolic_native import access

    calls = {}
    for name in ("principal_put", "grant_put", "issue"):
        original = getattr(access, name)
        key = "python:catabolic.access." + name

        def wrapped(*args, _original=original, _key=key, **kwargs):
            calls[_key] = calls.get(_key, 0) + 1
            return _original(*args, **kwargs)

        setattr(access, name, functools.wraps(original)(wrapped))
    return calls


def seed(store):
    with store.transaction() as db:
        db.execute("INSERT INTO profiles VALUES ('alternate')")
        db.execute("INSERT INTO items(id,kind,metadata) VALUES ('item-a','movie','{}')")
        for identifier, language, query in (
            ("sql", "sql", "SELECT id AS item_id FROM items"),
            (
                "graphql",
                "graphql",
                "query($after:String){items(first:100,after:$after){nodes{id} pageInfo{endCursor hasNextPage}}}",
            ),
            ("private", "sql", "SELECT digest AS item_id FROM api_tokens"),
            ("wrong", "sql", "SELECT id AS file_id FROM items"),
        ):
            definition = {
                "mode": "selection",
                "selection": {
                    "language": language,
                    "profile": "default",
                    "query": query,
                },
            }
            db.execute(
                "INSERT INTO saved_queries(id,profile,name,revision,definition,digest) VALUES(?,?,?,1,?,?)",
                (
                    identifier,
                    "default",
                    identifier,
                    json.dumps(definition),
                    "fixture-" + identifier,
                ),
            )


def capture(function):
    try:
        return {"value": function()}
    except Exception as error:
        return {
            "error": str(error),
            "status": getattr(error, "status", None),
            "code": getattr(error, "code", None),
        }


def snapshot(store):
    return {
        "principals": store.rows(
            "SELECT id,profile,enabled FROM api_principals ORDER BY id"
        ),
        "grants": store.rows(
            "SELECT id,principal,definition,revision,enabled FROM api_grants ORDER BY id"
        ),
        "tokens": store.rows(
            "SELECT principal,grants,revoked FROM api_tokens ORDER BY rowid"
        ),
        "audit": [
            {
                "principal": row["principal"],
                "action": row["action"],
                "resource": "<token>"
                if row["action"] == "token.issue"
                else row["resource"],
            }
            for row in store.rows(
                "SELECT principal,action,resource FROM api_audit ORDER BY id"
            )
        ],
        "epoch": store.rows("SELECT generation FROM fallback_epoch WHERE id=1"),
        "transaction": store.db.in_transaction,
    }


def sqlite_error_provenance(access, store, native):
    """Known SQLite class gap, with exact non-Access provenance and rollback."""
    messages = (
        "fixture audit failure",
        "invalid_principal",
        "invalid_grant_fields",
        "invalid_grant",
        "invalid_revisions",
        "invalid_callback_url",
        "invalid_callback_origin",
        "grant_requires_complete_item_selection",
        "invalid_token_configuration",
        "invalid_token_grant",
        "unknown_principal",
        "principal_profile_conflict",
        "grant_principal_conflict",
    )
    operations = (
        (
            "principal",
            "api_principals",
            lambda: access.principal_put(store, "default", "sqlite-collision"),
        ),
        (
            "grant",
            "api_audit",
            lambda: access.grant_put(
                store, "fixture", {"actions": ["metadata:read"]}, "sqlite-collision"
            ),
        ),
        ("issue", "api_audit", lambda: access.issue(store, "fixture", ["grant"])),
    )
    result = []
    original = snapshot(store)

    class UndoFixture(Exception):
        pass

    try:
        with store.transaction():
            for operation, table, call in operations:
                for message in messages:
                    # Install after admission. A new Store/CLI would reject
                    # this altered schema before reaching the mutation.
                    store.db.execute(
                        f"CREATE TRIGGER fail_provenance BEFORE INSERT ON {table} "
                        f"BEGIN SELECT RAISE(ABORT,'{message}'); END"
                    )
                    before = snapshot(store)
                    try:
                        call()
                    except Exception as error:
                        assert type(error).__name__ == (
                            "CatabolicError" if native else "IntegrityError"
                        ), (operation, message, type(error).__name__)
                        assert not isinstance(error, access.AccessError)
                        assert str(error) == message
                        assert not hasattr(error, "status") and not hasattr(
                            error, "code"
                        )
                    else:
                        raise AssertionError((operation, message, "did not fail"))
                    finally:
                        store.db.execute("DROP TRIGGER fail_provenance")
                    assert snapshot(store) == before
                    call()  # Same owner is reusable after every nested rollback.
                    result.append(
                        {
                            "operation": operation,
                            "message": message,
                            "access_error": False,
                            "rollback": True,
                            "reuse": True,
                        }
                    )
            raise UndoFixture
    except UndoFixture:
        pass
    assert snapshot(store) == original
    return result


def issued(access, store, principal, grants, ttl=3600):
    before = time.time()
    result = access.issue(store, principal, grants, ttl)
    after = time.time()
    assert set(result) == {"token_id", "token", "expires"}
    assert re.fullmatch(r"[a-f0-9]{24}\.[A-Za-z0-9_-]{43}", result["token"])
    assert result["token"].startswith(result["token_id"] + ".")
    assert before + ttl <= result["expires"] <= after + ttl
    row = store.rows("SELECT * FROM api_tokens WHERE id=?", (result["token_id"],))[0]
    assert row["digest"] == hashlib.sha256(result["token"].encode()).hexdigest()
    assert json.loads(row["grants"]) == grants
    assert row["expires"] == result["expires"]
    assert result["token"] not in json.dumps(row)
    return {
        "shape": True,
        "digest_only": True,
        "expiry": True,
        "principal": row["principal"],
        "grants": json.loads(row["grants"]),
        "revoked": row["revoked"],
    }


def scenario(root, engine):
    namespace = "catabolic" if engine == "reference" else "catabolic_native"
    Store = importlib.import_module(namespace + ".store").Store
    access = importlib.import_module(namespace + ".access")
    path = root / (engine + ".db")
    Store.initialize(path)
    groups = {
        "signatures": [
            str(inspect.signature(getattr(access, name)))
            for name in ("principal_put", "grant_put", "issue")
        ]
    }
    with Store(path, writable=True) as store:
        seed(store)
        groups["principal"] = access.principal_put(store, "default", "fixture")
        access.principal_put(store, "alternate", "other")
        groups["principal_invalid"] = [
            capture(lambda value=value: access.principal_put(store, "missing", value))
            for value in (None, 1, True, [], {}, (), "", "x" * 129)
        ]
        groups["principal_conflict"] = capture(
            lambda: access.principal_put(store, "alternate", "fixture")
        )
        with store.transaction() as db:
            db.execute("UPDATE api_principals SET enabled=0 WHERE id='fixture'")
        groups["principal_disabled"] = [
            access.principal_put(store, "default", "fixture"),
            store.rows("SELECT enabled FROM api_principals WHERE id='fixture'"),
        ]
        definition = {
            "actions": ["metadata:read"],
            "item_ids": ["item-a"],
            "revisions": {"雪": "digest"},
        }
        groups["grant"] = access.grant_put(store, "fixture", definition, "grant")
        with store.transaction() as db:
            db.execute("UPDATE api_grants SET enabled=0 WHERE id='grant'")
        groups["grant_update"] = [
            access.grant_put(store, "fixture", definition, "grant"),
            store.rows("SELECT revision,enabled FROM api_grants WHERE id='grant'"),
        ]
        groups["grant_conflict"] = capture(
            lambda: access.grant_put(store, "other", definition, "grant")
        )
        invalid = [
            None,
            [],
            (),
            {"unknown": 1},
            {},
            {"actions": []},
            {"actions": [1]},
            {"actions": ["x" * 257]},
            {"actions": ["a"], "operator": 1},
            {"actions": ["a"], "callback_origins": ["https://example.invalid/path"]},
        ]
        groups["grant_invalid"] = [
            capture(lambda value=value: access.grant_put(store, "absent", value, "bad"))
            for value in invalid
        ]
        typed = [
            {"actions": ("metadata:read",)},
            {"actions": ["a"], "item_ids": ("item-a",)},
            {"actions": ["a"], "revisions": {1: "digest"}},
            {"actions": ["a"], "revisions": []},
            {"actions": ["a"], "revisions": {"k": object()}},
            {1: object()},
            {"unknown": object()},
        ]
        groups["grant_type_transport"] = [
            capture(lambda value=value: access.grant_put(store, "absent", value, "bad"))
            for value in typed
        ]
        groups["grant_unknown"] = capture(
            lambda: access.grant_put(store, "absent", definition, "grant")
        )
        groups["grant_error_order"] = capture(
            lambda: access.grant_put(
                store, "other", {"actions": ["a"], "query_id": "absent"}, "grant"
            )
        )
        for key, query in (
            ("sql_preflight", "sql"),
            ("graphql_preflight", "graphql"),
            ("private_preflight", "private"),
            ("missing_preflight", "absent"),
            ("wrong_entity", "wrong"),
        ):
            groups[key] = capture(
                lambda key=key, query=query: access.grant_put(
                    store,
                    "fixture",
                    {"actions": ["metadata:read"], "query_id": query},
                    key,
                )
            )
        generated = access.grant_put(store, "fixture", definition)
        import uuid

        assert uuid.UUID(generated["grant_id"]).version == 4
        with store.transaction() as db:
            db.execute("DELETE FROM api_grants WHERE id=?", (generated["grant_id"],))
            db.execute(
                "DELETE FROM api_audit WHERE resource=?", (generated["grant_id"],)
            )
        groups["grant_generated"] = {
            "uuid4": True,
            "definition": generated["definition"],
        }
        groups["token"] = issued(access, store, "fixture", ["grant", "grant"], 17)
        groups["token_invalid"] = [
            capture(
                lambda grants=grants, ttl=ttl: access.issue(
                    store, "fixture", grants, ttl
                )
            )
            for grants, ttl in (
                ([], 10),
                (None, 10),
                (["grant"] * 101, 10),
                (["grant"], 0),
                (["grant"], 2592001),
            )
        ]

        class Integer(int):
            pass

        groups["token_type_transport"] = [
            capture(
                lambda grants=grants, ttl=ttl: access.issue(
                    store, "fixture", grants, ttl
                )
            )
            for grants, ttl in (
                (("grant",), 10),
                (["grant"], True),
                (["grant"], 10.0),
                (["grant"], Integer(10)),
            )
        ]
        groups["token_grant_order"] = capture(
            lambda: access.issue(store, "absent", ["missing", "grant"], 10)
        )
        groups["token_disabled_principal"] = issued(
            access, store, "fixture", ["grant"], 1
        )
        for identifier in ("1", "1.0"):
            access.grant_put(
                store, "fixture", {"actions": ["metadata:read"]}, identifier
            )
        groups["scalar_token_persistence"] = issued(
            access, store, "fixture", [True, 1, 1.0, True], 1
        )
        before = snapshot(store)
        try:
            with store.transaction():
                access.principal_put(store, "default", "outer")
                try:
                    with store.transaction():
                        access.grant_put(store, "outer", definition, "inner")
                        issued(access, store, "outer", ["inner"], 1)
                        raise LookupError("inner")
                except LookupError:
                    pass
                groups["nested_rollback"] = [
                    store.rows("SELECT id FROM api_principals WHERE id='outer'"),
                    store.rows("SELECT id FROM api_grants WHERE id='inner'"),
                    store.db.in_transaction,
                ]
                raise LookupError("outer")
        except LookupError:
            pass
        assert snapshot(store) == before
        groups["outer_rollback"] = True
        with store.transaction() as db:
            db.execute(
                "CREATE TRIGGER fail_audit BEFORE INSERT ON api_audit BEGIN SELECT RAISE(ABORT,'fixture audit failure'); END"
            )
        failures = [
            capture(
                lambda: access.grant_put(store, "fixture", definition, "audit-fails")
            ),
            capture(lambda: access.issue(store, "fixture", ["grant"], 10)),
        ]
        assert snapshot(store) == before
        with store.transaction() as db:
            db.execute("DROP TRIGGER fail_audit")
        groups["audit_failure"] = {
            "ordinary": failures,
            "message_collisions": sqlite_error_provenance(
                access, store, engine == "native"
            ),
        }
        groups["final_state"] = snapshot(store)
    with Store(path) as store:
        groups["readonly_validation"] = [
            capture(lambda: access.principal_put(store, "default", "")),
            capture(lambda: access.grant_put(store, "fixture", {}, "bad")),
            capture(lambda: access.issue(store, "fixture", [], 1)),
        ]
        groups["readonly_mutation"] = [
            capture(lambda: access.principal_put(store, "default", "readonly")),
            capture(lambda: access.grant_put(store, "fixture", definition, "readonly")),
            capture(lambda: access.issue(store, "fixture", ["grant"], 1)),
        ]
    assert set(groups) == set(EXPECTED_GROUPS)
    return groups


class InstalledAccess(unittest.TestCase):
    def setUp(self):
        from catabolic_native import Store, access

        self.access, self.Store = access, Store
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / "catalog.db"
        Store.initialize(self.path)
        self.store = Store(self.path, writable=True)
        self.addCleanup(self.store.close)
        seed(self.store)
        access.principal_put(self.store, "default", "fixture")
        access.grant_put(self.store, "fixture", {"actions": ["metadata:read"]}, "grant")

    def test_exact_public_types_and_signatures(self):
        expected = [
            "(store, profile, identifier)",
            "(store, principal, definition, identifier=None)",
            "(store, principal, grants, ttl=3600)",
        ]
        self.assertEqual(
            [
                str(inspect.signature(getattr(self.access, name)))
                for name in ("principal_put", "grant_put", "issue")
            ],
            expected,
        )
        error = self.access.AccessError()
        self.assertEqual(
            (str(error), error.code, error.status), ("forbidden", "forbidden", 403)
        )
        definition = {"actions": ["metadata:read"]}
        self.assertIs(
            self.access.grant_put(self.store, "fixture", definition, "identity")[
                "definition"
            ],
            definition,
        )

    def test_same_owner_uncommitted_graphql_and_rollback(self):
        before = snapshot(self.store)
        with self.assertRaisesRegex(LookupError, "rollback"):
            with self.store.transaction():
                self.access.principal_put(self.store, "default", "pending")
                self.access.grant_put(
                    self.store,
                    "pending",
                    {"actions": ["metadata:read"], "query_id": "graphql"},
                    "pending",
                )
                issued(self.access, self.store, "pending", ["pending"])
                raise LookupError("rollback")
        self.assertEqual(snapshot(self.store), before)

    def test_detached_closed_and_reconnect(self):
        from catabolic_native.store import CatabolicError

        operations = [
            lambda: self.access.principal_put(self.store, "default", "x"),
            lambda: self.access.grant_put(self.store, "fixture", {}, "x"),
            lambda: self.access.issue(self.store, "fixture", [], 1),
        ]
        with self.store.detached():
            for operation in operations:
                with self.assertRaisesRegex(CatabolicError, "detached") as caught:
                    operation()
                self.assertNotIsInstance(caught.exception, self.access.AccessError)
        self.access.principal_put(self.store, "default", "after")
        self.store.close()
        for operation in operations:
            with self.assertRaisesRegex(CatabolicError, "closed"):
                operation()

    def test_active_transaction_thread_refusal(self):
        results = []
        with self.store.transaction():
            thread = threading.Thread(
                target=lambda: results.append(
                    capture(
                        lambda: self.access.principal_put(
                            self.store, "default", "other-thread"
                        )
                    )
                )
            )
            thread.start()
            thread.join(5)
            self.assertFalse(thread.is_alive())
            self.assertIn("thread", results[0]["error"])
        self.assertFalse(
            self.store.rows("SELECT id FROM api_principals WHERE id='other-thread'")
        )

    def test_no_secret_file_or_cleartext_database(self):
        before = {p.name for p in self.path.parent.iterdir()}
        result = self.access.issue(self.store, "fixture", ["grant"])
        after = {p.name for p in self.path.parent.iterdir()}
        self.assertEqual(before, after)
        self.assertNotIn(result["token"].encode(), self.path.read_bytes())
        self.assertFalse(self.store.rows("SELECT * FROM api_private_files"))

    def test_no_generic_admin_http_admission(self):
        from catabolic_native.saved_queries import Queries

        queries = Queries(self.store)
        for flags in (
            {"_http": True},
            {"_http": True, "grant_preflight": True, "local_grant_preflight": True},
        ):
            result = capture(
                lambda flags=flags: self.store._call(
                    "query_execution", "select", {"id": "graphql", **flags}
                )
            )
            self.assertIn("error", result)
        self.access.grant_put(
            self.store,
            "fixture",
            {"actions": ["metadata:read"], "query_id": "graphql"},
            "local-only",
        )
        self.assertIn("error", capture(lambda: queries.select("graphql", _http=True)))

    def test_unrepresentable_values_are_explicitly_unsupported(self):
        # Exact exception precedence for these objects is deliberately not claimed.
        before = snapshot(self.store)
        for operation in (
            lambda: self.access.principal_put(self.store, b"default", "x"),
            lambda: self.access.grant_put(
                self.store, "fixture", {"actions": ["a"], "query_id": ("sql",)}, "x"
            ),
            lambda: self.access.issue(self.store, "fixture", [object()]),
            lambda: self.access.issue(self.store, "fixture", [float("nan")]),
        ):
            with self.assertRaises((TypeError, ValueError)):
                operation()
        recursive = []
        recursive.append(recursive)
        with self.assertRaisesRegex(ValueError, "nesting exceeds 128"):
            self.access.issue(self.store, "fixture", recursive)
        self.assertEqual(snapshot(self.store), before)

    def test_sqlite_messages_cannot_impersonate_access_errors(self):
        self.assertEqual(
            len(sqlite_error_provenance(self.access, self.store, True)), 39
        )

    def test_signal_identity_rollback_and_owner_reuse(self):
        self.store.db.execute(
            "CREATE TRIGGER slow_access BEFORE INSERT ON api_audit BEGIN SELECT sum(n) FROM (WITH RECURSIVE t(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM t WHERE n<100000000) SELECT n FROM t); END"
        )
        before = snapshot(self.store)
        exact = KeyboardInterrupt("fixture access interruption")
        previous = signal.getsignal(signal.SIGALRM)

        def interrupt(*_):
            raise exact

        signal.signal(signal.SIGALRM, interrupt)
        try:
            with self.assertRaises(KeyboardInterrupt) as caught:
                with self.store.transaction():
                    signal.setitimer(signal.ITIMER_REAL, 0.05)
                    self.access.grant_put(
                        self.store,
                        "fixture",
                        {"actions": ["metadata:read"], "query_id": "graphql"},
                        "interrupted",
                    )
            self.assertIs(caught.exception, exact)
        finally:
            signal.setitimer(signal.ITIMER_REAL, 0)
            signal.signal(signal.SIGALRM, previous)
        self.store.db.execute("DROP TRIGGER slow_access")
        self.assertEqual(snapshot(self.store), before)
        self.access.grant_put(
            self.store, "fixture", {"actions": ["metadata:read"]}, "after-interrupt"
        )


EXPECTED_TESTS = (
    "test_exact_public_types_and_signatures",
    "test_same_owner_uncommitted_graphql_and_rollback",
    "test_detached_closed_and_reconnect",
    "test_active_transaction_thread_refusal",
    "test_no_secret_file_or_cleartext_database",
    "test_no_generic_admin_http_admission",
    "test_unrepresentable_values_are_explicitly_unsupported",
    "test_signal_identity_rollback_and_owner_reuse",
    "test_sqlite_messages_cannot_impersonate_access_errors",
)


def main():
    parser = argument_parser()
    clear_stale_report(parser=parser)
    qualification = sibling("native_facade_qualification")
    parser.set_defaults(reference_commit=qualification.FROZEN_COMMIT)
    args = parser.parse_args()
    for name, value in vars(args).items():
        if isinstance(value, Path):
            setattr(args, name, value.absolute())
    provenance = sibling("qualify_native_embedding")
    if args.scenario:
        oracle, calls = None, {}
        if args.scenario == "reference":
            assert list(sys.version_info[:3]) == qualification.REFERENCE_VERSION
            assert args.reference_commit == qualification.FROZEN_COMMIT
            oracle = provenance.reference_provenance(
                args.reference_root, args.reference_commit
            )
            sys.path.insert(0, str(args.reference_root / "src"))
        else:
            calls = trace()
        result = scenario(args.root, args.scenario)
        imports = (
            provenance.reference_imports(
                args.reference_root, oracle["tracked_source_sha256"]
            )
            if oracle
            else None
        )
        print(
            json.dumps(dict(result=result, calls=calls, oracle=oracle, imports=imports))
        )
        return
    assert all(
        (
            args.reference_python,
            args.inventory,
            args.source_root,
            args.wheel,
            args.build_manifest,
            args.report,
        )
    )
    assert not Path.cwd().resolve().is_relative_to(args.source_root.resolve())
    inventory = json.loads(args.inventory.read_text())
    rows = qualification.inventory_rows(inventory, CONTRACTS, 3)
    identity = qualification.NativeIdentity(args.source_root)
    fence = qualification.file_fence(__file__, args.inventory, args.build_manifest)
    # The helperless verifier is reused verbatim; fence its own source as well.
    fence["development_verifier_sha256"] = qualification.digest(
        Path(__file__).with_name("native_catalog_refresh_embedding_tests.py")
    )
    runtime = qualification.reference_runtime(args.reference_python)
    native_runtime = {
        "version": list(sys.version_info[:3]),
        "executable": sys.executable,
        "prefix": sys.prefix,
        "executable_sha256": qualification.digest(sys.executable),
    }
    source = sibling("build_native_wheel").source_digest(args.source_root)
    manifest = json.loads(args.build_manifest.read_text())
    assert manifest["profile"] == ("development" if args.dev else "release")
    assert manifest["source_sha256"] == manifest["source_sha256_after"] == source
    assert manifest["wheel_sha256"] == qualification.digest(args.wheel)

    def artifact():
        if args.dev:
            return sibling(
                "native_catalog_refresh_embedding_tests"
            ).verify_development_wheel(args.wheel)
        return qualification.strict_artifact(
            args.wheel,
            args.build_manifest,
            args.source_root,
            native_binary=args.native_binary,
            legacy_binary=args.legacy_binary,
        )

    installed = artifact()
    oracle = provenance.reference_provenance(
        args.reference_root, inventory["reference_commit"]
    )
    calls = trace()
    result = sibling("native_qualification_results").run_required_suite(
        unittest.defaultTestLoader.loadTestsFromTestCase(InstalledAccess),
        {InstalledAccess: EXPECTED_TESTS},
        9,
    )
    with tempfile.TemporaryDirectory() as directory:
        outputs = {}
        for engine, interpreter in (
            ("reference", args.reference_python),
            ("native", Path(sys.executable)),
        ):
            run = subprocess.run(
                [
                    str(interpreter),
                    "-I",
                    "-B",
                    str(Path(__file__).resolve()),
                    "--scenario",
                    engine,
                    "--root",
                    directory,
                    "--reference-root",
                    str(args.reference_root),
                    "--reference-commit",
                    inventory["reference_commit"],
                ],
                cwd=directory,
                capture_output=True,
                text=True,
                timeout=90,
            )
            assert run.returncode == 0, (engine, run.stdout, run.stderr)
            outputs[engine] = json.loads(run.stdout)
        qualification.paired_groups(
            outputs["reference"]["result"],
            outputs["native"]["result"],
            EXPECTED_GROUPS,
            30,
        )
        assert outputs["reference"]["oracle"] == oracle
    paired_calls = outputs["native"]["calls"]
    coverage = []
    for row in rows:
        key = row["id"]
        assert calls.get(key, 0) > 0 and paired_calls.get(key, 0) > 0
        coverage.append(
            dict(
                id=key,
                installed_calls=calls[key],
                paired_calls=paired_calls[key],
                selected_contracts=CONTRACTS[key],
                qualification="selected-contracts-tested-with-gaps",
            )
        )
    assert sibling("build_native_wheel").source_digest(args.source_root) == source
    identity.verify()
    assert artifact() == installed
    repeated = qualification.file_fence(__file__, args.inventory, args.build_manifest)
    repeated["development_verifier_sha256"] = qualification.digest(
        Path(__file__).with_name("native_catalog_refresh_embedding_tests.py")
    )
    assert repeated == fence
    assert qualification.reference_runtime(args.reference_python) == runtime
    assert qualification.digest(sys.executable) == native_runtime["executable_sha256"]
    assert (
        provenance.reference_provenance(
            args.reference_root, inventory["reference_commit"]
        )
        == oracle
    )
    report = dict(
        format=1,
        scope="Three-row local access administration facade only",
        passed=True,
        development_only=args.dev,
        tests=result.testsRun,
        successful_scenarios=result.successful_scenarios,
        paired_groups=30,
        paired_group_names=list(EXPECTED_GROUPS),
        coverage=coverage,
        build=manifest,
        artifact=installed,
        oracle=oracle,
        oracle_imports=outputs["reference"]["imports"],
        reference_runtime=runtime,
        native_runtime=native_runtime,
        **fence,
        limitations=[
            "Non-JSON scalar objects, nonfinite/out-of-range numbers, surrogates, tuple-valued query IDs and pathological container subclasses remain outside selected parity; unsupported values are refused before owner admission, so exact frozen mixed-invalid-input precedence is not claimed",
            "Native authentication intentionally refuses scalar-ID issued tokens; Python bool/int/float membership-cache collisions are not reproduced",
            "No credential file writer, token-create/rotate CLI, external account, live service, full access/API parity or release qualification",
        ],
    )
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(
        json.dumps(
            dict(
                tests=result.testsRun,
                paired_groups=30,
                inventory_rows=3,
                development_only=args.dev,
                report=str(args.report),
            )
        )
    )


if __name__ == "__main__":
    main()
