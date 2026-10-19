#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Fixed ten-row Components/Policies installed wheel qualification.

Strict paired-release-wheel mode and explicit helperless development mode are
separate. Neither closes aggregate, publication, default or release gates.
"""

import argparse
import ast
import functools
import hashlib
import importlib.util
import inspect
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path
from uuid import UUID


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
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--wheel", type=Path, help="Exact installed paired release wheel")
    mode.add_argument(
        "--dev-wheel",
        type=Path,
        help="Exact installed helperless development wheel; never release qualification",
    )
    parser.add_argument(
        "--source-root", type=Path, default=Path(__file__).resolve().parents[1]
    )
    parser.add_argument("--inventory", type=Path)
    parser.add_argument("--build-manifest", type=Path)
    parser.add_argument("--reference-root", type=Path)
    parser.add_argument("--reference-python", type=Path)
    parser.add_argument("--reference-commit")
    parser.add_argument("--report", type=Path)
    parser.add_argument("--scenario", choices=("reference", "native"))
    parser.add_argument("--root", type=Path)
    return parser


# CLI preflight precedes project imports and optimized-mode refusal.
# Library imports never inspect caller arguments or invalidate their receipts.
if __name__ == "__main__":
    clear_stale_report(parser=argument_parser())

if not __debug__ or os.environ.get("PYTHONOPTIMIZE"):
    raise SystemExit(
        "component/policy qualification requires assertions; do not run with -O or -OO or PYTHONOPTIMIZE"
    )

PREFIX = "python:catabolic."
CONTRACTS = {
    PREFIX + "components" + suffix: {"tested": tested, "gap": gap}
    for suffix, tested, gap in (
        (
            "",
            "Installed module import binding and four frozen signatures",
            "Opt-in catabolic_native namespace; catabolic remains the default",
        ),
        (
            ".revision",
            "Canonical JSON identity, Unicode, nested ctime, omitted top-level ctime",
            "Non-JSON Python objects and invalid non-mapping exception categories are unsupported",
        ),
        (
            ".index_file",
            "Current/historical evidence, typed stream indexes, whole-file roles, idempotence and caller rollback",
            "Exhaustive corrupt evidence and all historical schema fixtures are unqualified",
        ),
        (
            ".backfill",
            "Historical adoption, unchanged epoch, same-owner command scope, rollback and expired-handle refusal",
            "Python sqlite3 connections are unsupported; native handles deliberately expire with their lexical scope",
        ),
        (
            ".get",
            "Decoded shape, profile isolation, currentness, same-owner EvaluationSession budget/epoch/cancellation and snapshot reads",
            "Foreign Python sessions/access contexts are rejected; exhaustive corruption and timing boundaries are unqualified",
        ),
    )
}
CONTRACTS.update(
    {
        PREFIX + "fallback_policies.Policies" + suffix: {"tested": tested, "gap": gap}
        for suffix, tested, gap in (
            (
                "",
                "Installed class binding and four frozen method signatures",
                "Opt-in namespace; foreign Store objects are unsupported",
            ),
            (
                ".__init__",
                "Store/profile attributes and alternate profiles",
                "Legacy sqlite3 Store sharing is unsupported",
            ),
            (
                ".get",
                "Decoded definition, identity, unknown revision and profile isolation",
                "Corrupt stored JSON and SQLite exception classes are unqualified",
            ),
            (
                ".put",
                "Defaults, canonical digest, idempotence, new revisions, dependencies, packages, semantic errors and nested rollback",
                "Structural validation uses native diagnostics rather than exact Pydantic text; all invalid-field combinations are unqualified",
            ),
            (
                ".listing",
                "Defaults, page/cursor ordering, empty/alternate profile and invalid limits",
                "Blob/memoryview cursors and invalid non-integer limit exception classes are unqualified",
            ),
        )
    }
)
EXPECTED_GROUPS = (
    "revision",
    "index_return",
    "component_rows",
    "component_get",
    "get_unknown",
    "get_other_profile",
    "index_idempotent",
    "historical_current_only",
    "historical_return",
    "historical_rows",
    "stale_current",
    "require_current",
    "backfill_return",
    "backfill_epoch",
    "backfill_idempotent",
    "component_rollback",
    "session_get",
    "session_budget",
    "session_cancelled",
    "session_live_cancelled",
    "session_stale",
    "policy_attributes",
    "policy_default",
    "policy_get",
    "policy_idempotent",
    "policy_revision",
    "policy_dependencies",
    "policy_package",
    "policy_pagination",
    "policy_other_profile",
    "policy_unknown",
    "policy_invalid_limits",
    "policy_semantic_errors",
    "policy_structural_errors",
    "policy_nested_rollback",
    "policy_outer_rollback",
    "owner_usable",
)


def sibling(name):
    spec = importlib.util.spec_from_file_location(
        name, Path(__file__).with_name(name + ".py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def api(engine):
    namespace = "catabolic" if engine == "reference" else "catabolic_native"
    module = __import__(namespace, fromlist=["components"])
    # The shared frozen provenance verifier requires the public Application module.
    __import__(namespace + ".app", fromlist=["Application"])
    components = __import__(namespace + ".components", fromlist=["revision"])
    store = __import__(namespace + ".store", fromlist=["Store"])
    policies = __import__(namespace + ".fallback_policies", fromlist=["Policies"])
    queries = __import__(namespace + ".saved_queries", fromlist=["Queries"])
    evaluation = __import__(namespace + ".evaluation", fromlist=["EvaluationSession"])
    curation = __import__(namespace + ".curation", fromlist=["occurrence"])
    domain = __import__(namespace + ".domain", fromlist=["CatabolicError"])
    assert module.components is components
    return (
        components,
        store.Store,
        store.encode,
        policies.Policies,
        queries.Queries,
        evaluation.EvaluationSession,
        curation.occurrence,
        domain.CatabolicError,
    )


def trace():
    components, _, _, Policies, *_ = api("native")
    calls = {PREFIX + "components": 1}

    def wrap(fn, key):
        @functools.wraps(fn)
        def counted(*args, **kwargs):
            calls[key] = calls.get(key, 0) + 1
            return fn(*args, **kwargs)

        return counted

    for name in ("revision", "index_file", "backfill", "get"):
        setattr(
            components,
            name,
            wrap(getattr(components, name), PREFIX + "components." + name),
        )
    for name in ("__init__", "get", "put", "listing"):
        setattr(
            Policies,
            name,
            wrap(
                getattr(Policies, name), PREFIX + "fallback_policies.Policies." + name
            ),
        )
    return calls


def seed(store, engine):
    _, _, encode, _, Queries, _, occurrence, _ = api(engine)
    statements = (
        "INSERT OR IGNORE INTO profiles VALUES ('default')",
        "INSERT INTO profiles VALUES ('alternate')",
        "INSERT INTO locations VALUES ('media')",
        "INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','media',1,2,'[]')",
        "INSERT INTO files(id,location,path) VALUES ('video','media','movie.mkv'),('subtitle','media','movie.srt')",
        "INSERT INTO observations(profile,file_id,size,mtime_ns,device,inode,status,scan_id) VALUES ('default','video',10,20,30,40,'present','scan'),('default','subtitle',5,20,30,41,'present','scan')",
        "INSERT INTO items(id,kind,metadata) VALUES ('item','movie','{\"title\":\"Fixture\"}')",
        "INSERT INTO item_files(id,file_id,item_id,role,origin,metadata) VALUES ('video-association','video','item','primary','explicit','{}'),('subtitle-association','subtitle','item','subtitle','explicit','{\"language\":\"fr\"}')",
    )
    for sql in statements:
        store.db.execute(sql)
    snapshot = occurrence(store, "default", "video")
    data = {
        "streams": [
            {"index": 0, "codec_type": "video"},
            {"index": 1, "codec_type": "audio", "tags": {"language": "eng"}},
            {"index": 2, "codec_type": "subtitle", "tags": {"language": "fr"}},
            {"index": True, "codec_type": "audio"},
            {"index": "3", "codec_type": "subtitle"},
            {"index": 4, "codec_type": "data"},
        ]
    }
    store.db.execute(
        "INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state,result,created_at) VALUES ('probe','default','video','probe','media',?,'{}','probe','complete',?,'2026-01-01')",
        (encode(snapshot), encode(data)),
    )
    store.db.execute(
        "INSERT INTO file_facts VALUES ('default','video','probe','probe',?,?,'complete')",
        (encode(snapshot), encode(data)),
    )
    for name, profile, mode in (
        ("query", "default", "selection"),
        ("rows-query", "default", "rows"),
        ("alternate-query", "alternate", "selection"),
    ):
        record = Queries(store, profile).put(
            name,
            {
                "mode": mode,
                "selection": {
                    "language": "sql",
                    "query": "SELECT id AS file_id FROM files",
                },
            },
        )
        store.db.execute(
            "UPDATE saved_queries SET id=? WHERE id=?", (name, record["id"])
        )


def definition(query="query"):
    return {"fallbacks": [{"name": "preferred", "query_id": query}]}


def cancel_after_initial_read(session_type, store, exception=None):
    """Deterministically change cancellation after the initial boundary read."""

    class CancelDuringLookup(session_type):
        def __init__(self):
            self._armed = False
            super().__init__(store)
            self._cancel_reads = 0
            self._armed = True

        def __getattribute__(self, name):
            if name == "cancelled" and self._armed:
                self._cancel_reads += 1
                if self._cancel_reads > 1:
                    if exception is not None:
                        raise exception
                    return True
            return super().__getattribute__(name)

    return CancelDuringLookup()


def scenario(root, engine):
    components, Store, encode, Policies, _, Session, _, Error = api(engine)
    output, ids = {}, {}
    with Store(root / (engine + ".db"), writable=True) as store:
        seed(store, engine)

        def error(action):
            try:
                action()
            except Error as exc:
                return str(exc)
            raise AssertionError("expected CatabolicError")

        output["revision"] = [
            components.revision(value)
            for value in (
                {},
                {"ctime_ns": 1},
                {"z": "français", "a": {"ctime_ns": 2}},
                {"size": 1, "ctime_ns": 1},
                {"size": 1, "ctime_ns": 9},
            )
        ]
        output["index_return"] = [
            components.index_file(store, "default", file)
            for file in ("video", "subtitle", "unknown")
        ]
        records = store.rows("SELECT id FROM component_occurrences ORDER BY id")
        output["component_rows"] = store.rows(
            "SELECT * FROM component_occurrences ORDER BY id"
        )
        output["component_get"] = [
            components.get(store, "default", r["id"], require_current=True)
            for r in records
        ]
        identifier = records[0]["id"]
        output["get_unknown"] = error(
            lambda: components.get(store, "default", "missing")
        )
        output["get_other_profile"] = error(
            lambda: components.get(store, "alternate", identifier)
        )
        components.index_file(store, "default", "video")
        output["index_idempotent"] = store.rows(
            "SELECT count(*) AS n FROM component_occurrences"
        )
        store.db.execute("UPDATE observations SET size=11 WHERE file_id='video'")
        store.db.execute("DELETE FROM component_occurrences WHERE file_id='video'")
        components.index_file(store, "default", "video")
        output["historical_current_only"] = store.rows(
            "SELECT count(*) AS n FROM component_occurrences WHERE file_id='video'"
        )
        output["historical_return"] = components.index_file(
            store, "default", "video", historical=True
        )
        output["historical_rows"] = store.rows(
            "SELECT id,revision FROM component_occurrences WHERE file_id='video' ORDER BY id"
        )
        historical = output["historical_rows"][0]["id"]
        output["stale_current"] = components.get(store, "default", historical)[
            "current"
        ]
        output["require_current"] = error(
            lambda: components.get(store, "default", historical, require_current=True)
        )
        before = store.rows("SELECT generation FROM fallback_epoch")[0]["generation"]
        output["backfill_return"] = components.backfill(store.db)
        output["backfill_epoch"] = (
            store.rows("SELECT generation FROM fallback_epoch")[0]["generation"]
            == before
        )
        components.backfill(store.db)
        output["backfill_idempotent"] = store.rows(
            "SELECT count(*) AS n FROM component_occurrences"
        )
        try:
            with store.transaction() as db:
                db.execute("DELETE FROM component_occurrences")
                components.backfill(db)
                assert (
                    store.rows("SELECT count(*) AS n FROM component_occurrences")[0][
                        "n"
                    ]
                    == 4
                )
                raise ValueError("roll back")
        except ValueError:
            pass
        output["component_rollback"] = store.rows(
            "SELECT count(*) AS n FROM component_occurrences"
        )
        current = store.rows(
            "SELECT id FROM component_occurrences WHERE file_id='subtitle'"
        )[0]["id"]
        session = Session(store)
        output["session_get"] = components.get(
            store, "default", current, session=session
        )
        output["session_budget"] = error(
            lambda: components.get(
                store, "default", current, session=Session(store, max_bytes=1)
            )
        )
        cancelled = Session(store)
        cancelled.cancelled = True
        output["session_cancelled"] = error(
            lambda: components.get(store, "default", current, session=cancelled)
        )
        live = cancel_after_initial_read(Session, store)
        live_error = error(
            lambda: components.get(store, "default", current, session=live)
        )
        assert live_error in (
            "evaluation_cancelled",
            "evaluation_expansion_cancelled_or_timed_out",
        )
        live._armed = False
        output["session_live_cancelled"] = [
            live._cancel_reads > 1,
            components.get(store, "default", current, session=live)["current"],
        ]
        store.db.execute("UPDATE observations SET size=6 WHERE file_id='subtitle'")
        output["session_stale"] = error(
            lambda: components.get(store, "default", current, session=session)
        )
        policies = Policies(store)
        output["policy_attributes"] = [policies.store is store, policies.profile]
        first = policies.put("movie", definition())
        ids[first["id"]] = "POLICY_1"
        assert UUID(first["id"]).version == 4
        assert (
            first["digest"]
            == hashlib.sha256(encode(first["definition"]).encode()).hexdigest()
        )
        output["policy_default"] = first
        output["policy_get"] = policies.get(first["id"])
        output["policy_idempotent"] = policies.put("movie", definition()) == first
        second = policies.put(
            "movie", {**definition(), "failback": {"mode": "immediate"}}
        )
        ids[second["id"]] = "POLICY_2"
        output["policy_revision"] = second
        output["policy_dependencies"] = store.rows(
            "SELECT query_id FROM fallback_dependencies WHERE policy_id=?",
            (first["id"],),
        )
        package = policies.put(
            "package",
            {
                **definition(),
                "package": {
                    "requirements": [{"name": "subtitle", **definition()}],
                    "publication": "selected_only",
                },
            },
        )
        ids[package["id"]] = "PACKAGE_POLICY"
        output["policy_package"] = package
        pages, after = [], ""
        while True:
            page = policies.listing(1, after)
            assert len(page["policies"]) <= 1
            if page["policies"]:
                record = page["policies"][0]
                assert record["id"] > after
                after = record["id"]
                pages.append((record["name"], record["revision"]))
            if not page["has_more"]:
                break
        output["policy_pagination"] = {
            "pages": sorted(pages),
            "bool_limit": len(policies.listing(True)["policies"]) == 1,
            "scalar_cursors": [
                [row["id"] for row in policies.listing(after=cursor)["policies"]]
                == [
                    row["id"]
                    for row in store.rows(
                        "SELECT id FROM fallback_policies WHERE profile=? AND id>? ORDER BY id",
                        ("default", cursor),
                    )
                ]
                for cursor in (None, False, True, 0, 1, 1.5, "1")
            ],
        }
        assert all(output["policy_pagination"]["scalar_cursors"])
        alternate = Policies(store, "alternate").put(
            "movie", definition("alternate-query")
        )
        ids[alternate["id"]] = "ALTERNATE_POLICY"
        output["policy_other_profile"] = [
            alternate,
            error(lambda: Policies(store, "alternate").get(first["id"])),
            Policies(store, "unknown").listing(),
        ]
        output["policy_unknown"] = error(lambda: policies.get("missing"))
        output["policy_invalid_limits"] = [
            error(lambda n=n: policies.listing(n)) for n in (0, -1, 1001)
        ]
        invalid = [
            ("bad name", definition()),
            ("bad", {"fallbacks": definition()["fallbacks"] * 2}),
            ("bad", {**definition(), "within_tier": {"require": {"bogus": True}}}),
            (
                "bad",
                {
                    **definition(),
                    "within_tier": {"prefer": [{"field": "bogus", "order": "asc"}]},
                },
            ),
            ("bad", definition("missing")),
            ("bad", definition("rows-query")),
            (
                "bad",
                {
                    **definition(),
                    "package": {"requirements": [{"name": "video", **definition()}]},
                },
            ),
            (
                "bad",
                {
                    **definition(),
                    "package": {
                        "requirements": [{"name": "subtitle", **definition()}],
                        "operation_id": "missing",
                    },
                },
            ),
        ]
        invalid.append(
            (
                "bad",
                {
                    **definition("missing-query"),
                    "package": {
                        "requirements": [{"name": "subtitle", **definition()}],
                        "operation_id": "missing-operation",
                    },
                },
            )
        )
        output["policy_semantic_errors"] = [
            error(lambda n=n, d=d: policies.put(n, d)) for n, d in invalid
        ]
        output["policy_structural_errors"] = [
            error(lambda d=d: policies.put("bad", d)).split(":", 1)[0]
            for d in (
                {},
                {**definition(), "unknown": True},
                {**definition(), "budgets": {"max_checks": 0}},
            )
        ]
        with store.transaction():
            outer = policies.put("outer", definition())
            try:
                with store.transaction():
                    inner = policies.put("inner", definition())
                    raise ValueError("inner")
            except ValueError:
                pass
            output["policy_nested_rollback"] = [
                policies.get(outer["id"])["name"],
                error(lambda: policies.get(inner["id"])),
            ]
        try:
            with store.transaction():
                reverted = policies.put("reverted", definition())
                raise ValueError("outer")
        except ValueError:
            pass
        output["policy_outer_rollback"] = error(lambda: policies.get(reverted["id"]))
        output["owner_usable"] = store.rows(
            "SELECT count(*) AS n FROM fallback_policies"
        )

    def normalize(value):
        if isinstance(value, dict):
            return {k: normalize(v) for k, v in value.items() if k != "created_at"}
        if isinstance(value, (list, tuple)):
            return [normalize(v) for v in value]
        return ids.get(value, value) if isinstance(value, str) else value

    assert set(output) == set(EXPECTED_GROUPS)
    return normalize(output)


class InstalledComponentPolicies(unittest.TestCase):
    def setUp(self):
        (
            self.components,
            self.Store,
            self.encode,
            self.Policies,
            self.Queries,
            self.Session,
            self.occurrence,
            self.Error,
        ) = api("native")
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / "catalog.db"
        self.Store.initialize(self.path)
        self.store = self.Store(self.path, writable=True)
        self.addCleanup(self.store.close)
        seed(self.store, "native")

    def indexed(self):
        self.components.index_file(self.store, "default", "subtitle")
        return self.store.rows("SELECT id FROM component_occurrences")[0]["id"]

    def test_revision_and_import_binding(self):
        self.assertEqual(
            self.components.revision({"size": 1, "ctime_ns": 2}),
            self.components.revision({"size": 1}),
        )
        self.assertNotEqual(
            self.components.revision({"nested": {"ctime_ns": 2}}),
            self.components.revision({"nested": {}}),
        )
        self.assertEqual(
            self.components.revision({"é": 1}),
            hashlib.sha256(self.encode({"é": 1}).encode()).hexdigest(),
        )

    def test_index_current_and_historical(self):
        self.components.index_file(self.store, "default", "video")
        self.assertEqual(
            len(self.store.rows("SELECT id FROM component_occurrences")), 3
        )
        self.store.db.execute("UPDATE observations SET size=99 WHERE file_id='video'")
        self.store.db.execute("DELETE FROM component_occurrences")
        self.components.index_file(self.store, "default", "video")
        self.assertEqual(self.store.rows("SELECT id FROM component_occurrences"), [])
        self.components.index_file(self.store, "default", "video", historical=True)
        self.assertEqual(
            len(self.store.rows("SELECT id FROM component_occurrences")), 3
        )

    def test_get_decode_profile_and_staleness(self):
        identifier = self.indexed()
        result = self.components.get(
            self.store, "default", identifier, require_current=True
        )
        self.assertIs(result["current"], True)
        self.assertIsInstance(result["locator"], dict)
        with self.assertRaisesRegex(self.Error, "unknown component"):
            self.components.get(self.store, "alternate", identifier)
        self.store.db.execute(
            "UPDATE observations SET size=99 WHERE file_id='subtitle'"
        )
        with self.assertRaisesRegex(self.Error, "stale_or_conflicting"):
            self.components.get(self.store, "default", identifier, require_current=True)

    def test_backfill_scope_epoch_nested_rollback(self):
        generation = self.store.rows("SELECT generation FROM fallback_epoch")
        with self.store.transaction() as outer:
            try:
                with self.store.transaction() as inner:
                    self.components.backfill(inner)
                    self.assertEqual(
                        self.store.rows("SELECT generation FROM fallback_epoch"),
                        generation,
                    )
                    raise ValueError("rollback")
            except ValueError:
                pass
            self.assertEqual(
                self.store.rows("SELECT id FROM component_occurrences"), []
            )
            with self.assertRaisesRegex(self.Error, "no longer active"):
                self.components.backfill(inner)
            self.components.backfill(outer)
        with self.assertRaisesRegex(self.Error, "no longer active"):
            self.components.backfill(outer)
        with self.assertRaisesRegex(self.Error, "native Store command handle"):
            self.components.backfill(object())

    def test_foreign_session_rejected_before_use(self):
        identifier = self.indexed()
        foreign_path = self.path.parent / "foreign.db"
        self.Store.initialize(foreign_path)
        with self.Store(foreign_path, writable=True) as other:
            session = self.Session(other)
            with self.assertRaisesRegex(self.Error, "identity_mismatch"):
                self.components.get(self.store, "default", identifier, session=session)
        with self.assertRaisesRegex(self.Error, "foreign Python"):
            self.components.get(self.store, "default", identifier, session=object())
        with self.assertRaisesRegex(self.Error, "identity_mismatch"):
            self.components.get(
                self.store, "alternate", identifier, session=self.Session(self.store)
            )

    def test_session_byte_epoch_cancel_and_drop(self):
        identifier = self.indexed()
        session = self.Session(self.store)
        self.assertEqual(
            self.components.get(self.store, "default", identifier, session=session)[
                "occurrence_id"
            ],
            identifier,
        )
        with self.assertRaisesRegex(self.Error, "byte_budget"):
            self.components.get(
                self.store,
                "default",
                identifier,
                session=self.Session(self.store, max_bytes=1),
            )
        session.cancelled = True
        with self.assertRaisesRegex(self.Error, "evaluation_cancelled"):
            self.components.get(self.store, "default", identifier, session=session)
        session.cancelled = False
        session.close()
        with self.assertRaisesRegex(self.Error, "identity_mismatch"):
            self.components.get(self.store, "default", identifier, session=session)
        stale = self.Session(self.store)
        self.store.db.execute("UPDATE observations SET size=33")
        with self.assertRaisesRegex(self.Error, "stale_evaluation_snapshot"):
            self.components.get(self.store, "default", identifier, session=stale)

    def test_session_deadline_and_owner_recovery(self):
        identifier = self.indexed()
        session = self.Session(self.store)
        session.pause_for_io(-6)
        with self.assertRaisesRegex(self.Error, "timed out"):
            self.components.get(self.store, "default", identifier, session=session)
        self.assertTrue(
            self.components.get(self.store, "default", identifier)["current"]
        )

    def test_session_live_cancellation_and_exception_cleanup(self):
        identifier = self.indexed()
        live = cancel_after_initial_read(self.Session, self.store)
        with self.assertRaisesRegex(
            self.Error, "evaluation_(cancelled|expansion_cancelled_or_timed_out)"
        ):
            self.components.get(self.store, "default", identifier, session=live)
        live._armed = False
        self.assertGreater(live._cancel_reads, 1)
        self.assertTrue(
            self.components.get(self.store, "default", identifier, session=live)[
                "current"
            ]
        )

        class Marker(Exception):
            pass

        marker = Marker("exact live-cancellation callback exception")
        raising = cancel_after_initial_read(self.Session, self.store, marker)
        with self.assertRaises(Marker) as caught:
            self.components.get(self.store, "default", identifier, session=raising)
        self.assertIs(caught.exception, marker)
        raising._armed = False
        self.assertTrue(
            self.components.get(self.store, "default", identifier, session=raising)[
                "current"
            ]
        )

    def test_closed_detached_readonly_and_snapshot(self):
        identifier = self.indexed()
        self.store.db.execute("PRAGMA journal_mode=WAL")
        with self.Store(self.path) as reader:
            result = self.components.get(reader, "default", identifier)
            self.store.db.execute(
                "UPDATE observations SET size=99 WHERE file_id='subtitle'"
            )
            self.assertEqual(self.components.get(reader, "default", identifier), result)
            with self.assertRaises(self.Error):
                self.components.backfill(reader.db)
        handle = self.store.db
        with self.store.detached():
            with self.assertRaisesRegex(self.Error, "detached"):
                self.components.backfill(handle)
        self.store.close()
        with self.assertRaisesRegex(self.Error, "closed"):
            self.components.backfill(handle)

    def test_policies_identity_digest_and_profiles(self):
        policies = self.Policies(self.store)
        first = policies.put("p", definition())
        self.assertIs(policies.store, self.store)
        self.assertEqual(policies.get(first["id"]), first)
        self.assertEqual(policies.put("p", definition()), first)
        self.assertEqual(
            first["digest"],
            hashlib.sha256(self.encode(first["definition"]).encode()).hexdigest(),
        )
        second = policies.put("p", {**definition(), "failback": {"mode": "manual"}})
        self.assertEqual(second["revision"], 2)
        with self.assertRaisesRegex(self.Error, "this profile"):
            self.Policies(self.store, "alternate").get(first["id"])

    def test_policies_paging_and_errors(self):
        policies = self.Policies(self.store)
        records = sorted(
            [policies.put(name, definition()) for name in ("a", "b", "c")],
            key=lambda row: row["id"],
        )
        self.assertEqual(policies.listing()["policies"], records)
        self.assertEqual(
            policies.listing(1), {"policies": records[:1], "has_more": True}
        )
        self.assertEqual(
            policies.listing(1, records[1]["id"]),
            {"policies": records[2:], "has_more": False},
        )
        self.assertEqual(policies.listing(True), policies.listing(1))
        for cursor in (None, False, True, 0, 1, 1.5, "1"):
            self.assertEqual(
                [row["id"] for row in policies.listing(after=cursor)["policies"]],
                [
                    row["id"]
                    for row in self.store.rows(
                        "SELECT id FROM fallback_policies WHERE profile=? AND id>? ORDER BY id",
                        ("default", cursor),
                    )
                ],
            )
        with self.assertRaises(self.Error):
            policies.listing(after=[])
        for value in (0, -1, 1001):
            with self.assertRaisesRegex(self.Error, "invalid policy page"):
                policies.listing(value)
        with self.assertRaisesRegex(self.Error, "tier names must be unique"):
            policies.put("bad", {"fallbacks": definition()["fallbacks"] * 2})

    def test_policies_outer_and_nested_rollback(self):
        policies = self.Policies(self.store)
        try:
            with self.store.transaction():
                policies.put("outer", definition())
                try:
                    with self.store.transaction():
                        policies.put("inner", definition())
                        raise ValueError("inner")
                except ValueError:
                    pass
                self.assertEqual(
                    [r["name"] for r in policies.listing()["policies"]], ["outer"]
                )
                raise ValueError("outer")
        except ValueError:
            pass
        self.assertEqual(policies.listing()["policies"], [])

    def test_policies_readonly_closed_detached(self):
        policy = self.Policies(self.store).put("p", definition())
        self.store.db.execute("PRAGMA journal_mode=WAL")
        with self.Store(self.path) as reader:
            self.assertEqual(self.Policies(reader).get(policy["id"]), policy)
            with self.assertRaises(self.Error):
                self.Policies(reader).put("new", definition())
        with self.store.detached():
            with self.assertRaisesRegex(self.Error, "detached"):
                self.Policies(self.store).listing()
        self.store.close()
        with self.assertRaisesRegex(self.Error, "closed"):
            self.Policies(self.store).listing()


EXPECTED_TESTS = (
    "test_revision_and_import_binding",
    "test_index_current_and_historical",
    "test_get_decode_profile_and_staleness",
    "test_backfill_scope_epoch_nested_rollback",
    "test_foreign_session_rejected_before_use",
    "test_session_byte_epoch_cancel_and_drop",
    "test_session_deadline_and_owner_recovery",
    "test_session_live_cancellation_and_exception_cleanup",
    "test_closed_detached_readonly_and_snapshot",
    "test_policies_identity_digest_and_profiles",
    "test_policies_paging_and_errors",
    "test_policies_outer_and_nested_rollback",
    "test_policies_readonly_closed_detached",
)


def installed_artifact(wheel):
    import catabolic_native

    package = Path(catabolic_native.__file__).resolve().parent
    checkout = Path(__file__).resolve().parents[1]
    assert not package.is_relative_to(checkout), (
        "installed qualification must run outside checkout"
    )
    with zipfile.ZipFile(wheel) as archive:
        assert "catabolic_native/native-helper.json" not in archive.namelist(), (
            "development wheel must be helperless"
        )
        assert "catabolic_native/bin/catabolic-native" not in archive.namelist(), (
            "development wheel must be helperless"
        )
        expected = {
            name.removeprefix("catabolic_native/"): hashlib.sha256(
                archive.read(name)
            ).hexdigest()
            for name in archive.namelist()
            if name.startswith("catabolic_native/") and not name.endswith("/")
        }
    actual = {
        str(p.relative_to(package)): hashlib.sha256(p.read_bytes()).hexdigest()
        for p in package.rglob("*")
        if p.is_file() and "__pycache__" not in p.parts
    }
    assert expected and actual == expected, (
        "installed package differs from exact development wheel"
    )
    return {
        "wheel_sha256": hashlib.sha256(wheel.read_bytes()).hexdigest(),
        "package_sha256": actual,
        "package_path": str(package),
    }


def build_identity(manifest_path, artifact):
    assert manifest_path is not None, "qualification requires --build-manifest"
    root = Path(__file__).resolve().parents[1]
    source = sibling("build_native_wheel").source_digest(root)
    manifest = json.loads(manifest_path.read_text())
    extensions = {
        name: digest
        for name, digest in artifact["package_sha256"].items()
        if name.startswith("_native.") and name.endswith((".so", ".pyd"))
    }
    assert len(extensions) == 1, "expected exactly one installed native extension"
    expected = {
        "profile": "development",
        "source_sha256": source,
        "source_sha256_after": source,
        "wheel_sha256": artifact["wheel_sha256"],
        "extension_sha256": next(iter(extensions.values())),
    }
    for key, value in expected.items():
        assert manifest.get(key) == value, ("build identity mismatch", key)
    return {
        **expected,
        "build_manifest_sha256": hashlib.sha256(manifest_path.read_bytes()).hexdigest(),
    }


def installed_signatures(rows):
    def arguments(signature):
        return ast.dump(ast.parse("def preserved" + signature + ": pass").body[0].args)

    verified = {}
    for row in rows:
        module, *attributes = row["id"].removeprefix(PREFIX).split(".")
        value = importlib.import_module("catabolic_native." + module)
        for name in attributes:
            value = getattr(value, name)
        kind = row["signature"]["kind"]
        if kind == "module-or-value-or-reexport":
            assert inspect.ismodule(value), row["id"]
            verified[row["id"]] = "module"
        elif kind == "class":
            assert inspect.isclass(value), row["id"]
            verified[row["id"]] = "class"
        else:
            actual = str(inspect.signature(value))
            expected = "(" + row["signature"]["arguments"] + ")"
            assert arguments(actual) == arguments(expected), row["id"]
            verified[row["id"]] = actual
    return verified


def source_manifest():
    script = Path(__file__).resolve()
    source_files = [
        script,
        script.with_name("native_qualification_results.py"),
        script.with_name("qualify_native_embedding.py"),
        script.with_name("build_native_wheel.py"),
        script.with_name("qualify_native_wheel.py"),
        script.with_name("native_facade_qualification.py"),
    ]
    root = script.parents[1]
    source_files += [
        root / path
        for path in (
            "crates/catabolic-store/src/public_components.rs",
            "crates/catabolic-store/src/fallback_mutation.rs",
            "crates/catabolic-store/src/components.rs",
            "crates/catabolic-store/src/component_mutation.rs",
            "crates/catabolic-core/src/fallback.rs",
            "crates/catabolic-store/src/embedding.rs",
            "crates/catabolic-store/src/lib.rs",
            "crates/catabolic-python/src/embedding.rs",
            "crates/catabolic-python/src/lib.rs",
            "crates/catabolic-python/python/catabolic_native/components.py",
            "crates/catabolic-python/python/catabolic_native/fallback_policies.py",
            "crates/catabolic-python/python/catabolic_native/store.py",
            "crates/catabolic-python/python/catabolic_native/evaluation.py",
            "crates/catabolic-store/tests/public_components_facade.rs",
        )
    ]
    return {
        str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in source_files
    }


def main():
    parser = argument_parser()
    clear_stale_report(parser=parser)
    if not __debug__:
        raise SystemExit(
            "component/policy qualification requires assertions; do not run with -O or -OO"
        )
    qualification = sibling("native_facade_qualification")
    args = parser.parse_args()
    for name, value in vars(args).items():
        if isinstance(value, Path):
            # Keep interpreter symlinks to preserve the selected environment.
            setattr(args, name, value.absolute())
    provenance = sibling("qualify_native_embedding")
    if args.scenario:
        oracle, calls = None, {}
        if args.scenario == "reference":
            assert list(sys.version_info[:3]) == qualification.REFERENCE_VERSION, (
                "oracle requires Python 3.14.6"
            )
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
    assert (
        (args.dev_wheel or args.wheel)
        and args.inventory
        and args.reference_root
        and args.reference_python
        and args.report
    )
    assert args.build_manifest is not None, "qualification requires --build-manifest"
    source_before = source_manifest()
    identity = qualification.NativeIdentity(args.source_root)
    fence = qualification.file_fence(__file__, args.inventory, args.build_manifest)
    runtime = qualification.reference_runtime(args.reference_python)
    if args.wheel:
        artifact = qualification.strict_artifact(
            args.wheel, args.build_manifest, args.source_root
        )
        build = artifact["build"]
    else:
        artifact = installed_artifact(args.dev_wheel)
        build = build_identity(args.build_manifest, artifact)
    inventory = json.loads(args.inventory.read_text())
    rows = qualification.inventory_rows(inventory, CONTRACTS, 10)
    signatures = installed_signatures(rows)
    oracle = provenance.reference_provenance(
        args.reference_root, inventory["reference_commit"]
    )
    attempt = args.report.with_suffix(".attempt.json")
    attempt.write_text(
        json.dumps(
            dict(
                development_only=bool(args.dev_wheel),
                artifact=artifact,
                build=build,
                installed_signatures=signatures,
                source_sha256=source_before,
                oracle=oracle,
                inventory_sha256=hashlib.sha256(
                    args.inventory.read_bytes()
                ).hexdigest(),
            ),
            indent=2,
        )
        + "\n"
    )
    calls = trace()
    result = sibling("native_qualification_results").run_required_suite(
        unittest.defaultTestLoader.loadTestsFromTestCase(InstalledComponentPolicies),
        {InstalledComponentPolicies: EXPECTED_TESTS},
        expected_count=13,
    )
    installed_calls = calls.copy()
    from catabolic_native import Store

    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        Store.initialize(root / "seed.db")
        outputs = {}
        for engine, python in (
            ("reference", args.reference_python),
            ("native", Path(sys.executable)),
        ):
            shutil.copyfile(root / "seed.db", root / (engine + ".db"))
            command = [
                str(python),
                "-I",
                "-B",
                str(Path(__file__).resolve()),
                "--scenario",
                engine,
                "--root",
                str(root),
            ]
            if engine == "reference":
                command += [
                    "--reference-root",
                    str(args.reference_root.resolve()),
                    "--reference-commit",
                    inventory["reference_commit"],
                ]
            env = dict(
                os.environ,
                CATABOLIC_PYTHON_REFERENCE=str(args.reference_root / "src"),
                PYTHONPATH=str(args.reference_root / "src"),
            )
            completed = subprocess.run(
                command,
                cwd=directory,
                env=env,
                capture_output=True,
                text=True,
                timeout=120,
            )
            assert completed.returncode == 0, (
                engine,
                completed.stdout,
                completed.stderr,
            )
            outputs[engine] = json.loads(completed.stdout)
        reference, native = outputs["reference"]["result"], outputs["native"]["result"]
        qualification.paired_groups(reference, native, EXPECTED_GROUPS, 37)
        assert outputs["reference"]["oracle"] == oracle
        paired_calls = outputs["native"]["calls"]
        oracle["imported_modules"] = outputs["reference"]["imports"]
    coverage = []
    for row in rows:
        key = row["id"] + (".__init__" if row["signature"]["kind"] == "class" else "")
        assert installed_calls.get(key, 0) > 0 and paired_calls.get(key, 0) > 0, row[
            "id"
        ]
        coverage.append(
            dict(
                id=row["id"],
                classification=row["classification"],
                qualification="selected-contracts-tested-with-gaps",
                installed_calls=installed_calls[key],
                paired_calls=paired_calls[key],
                **CONTRACTS[row["id"]],
            )
        )
    identity.verify()
    if args.wheel:
        assert artifact == qualification.strict_artifact(
            args.wheel, args.build_manifest, args.source_root
        )
    else:
        assert artifact == installed_artifact(args.dev_wheel)
        assert build == build_identity(args.build_manifest, artifact)
    assert fence == qualification.file_fence(
        __file__, args.inventory, args.build_manifest
    ), "qualification files changed"
    assert runtime == qualification.reference_runtime(args.reference_python), (
        "oracle interpreter changed"
    )
    assert signatures == installed_signatures(rows)
    assert provenance.reference_provenance(
        args.reference_root, inventory["reference_commit"]
    ) == {k: v for k, v in oracle.items() if k != "imported_modules"}
    assert source_before == source_manifest(), (
        "qualification sources changed during execution"
    )
    report = dict(
        format=1,
        scope="Separate Components/Policies selected-contract checkpoint; no aggregate, default, publication or release gate closure",
        development_only=bool(args.dev_wheel),
        passed=True,
        reference_runtime=runtime,
        qualification_scripts_sha256=fence["qualification_scripts_sha256"],
        artifact=artifact,
        build=build,
        installed_signatures=signatures,
        acceptance_tests=result.testsRun,
        successful_scenarios=result.successful_scenarios,
        paired_groups=len(native),
        paired_group_names=list(EXPECTED_GROUPS),
        inventory_sha256=hashlib.sha256(args.inventory.read_bytes()).hexdigest(),
        oracle=oracle,
        coverage=coverage,
        source_sha256=source_before,
    )
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(
        json.dumps(
            dict(
                tests=result.testsRun,
                paired_groups=len(native),
                inventory_rows=len(rows),
                development_only=bool(args.dev_wheel),
                report=str(args.report),
            )
        )
    )


if __name__ == "__main__":
    main()
