#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Fixed 11-row ordinary Watchers/source_events installed-wheel checkpoint.

Only disposable database metadata is created. Query report/event runs have no
bound sources. Other run families are pre-admission native refusal witnesses,
not paired parity, filesystem qualification, or aggregate/release gate closure.
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
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import zipfile
from datetime import datetime, timezone
from pathlib import Path
from types import SimpleNamespace
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

PREFIX = "python:catabolic."
EXPECTED_ROWS = 11
RUN_GATE = "native public watcher run currently requires a query plan with report or event reaction; other run families require a borrowed runtime owner"
PROJECTION_GATE = "native fallback publication is unqualified; only disposable debug fixtures may apply or recover"
CONTRACTS = {
    PREFIX + "watchers.Watchers" + suffix: dict(tested=tested, gap=gap)
    for suffix, tested, gap in (
        (
            "",
            "Installed class, exact frozen signatures and persistent Store identity",
            "Opt-in namespace; foreign Python Store sharing is unsupported",
        ),
        (
            ".__init__",
            "Application/Store/profile identity and profile isolation",
            "Legacy sqlite3 Store sharing is unsupported",
        ),
        (
            ".get",
            "Full decoded shape, unknown names and same-owner uncommitted visibility",
            "Malformed stored JSON and exhaustive historical-schema cases are unqualified",
        ),
        (
            ".list",
            "Exact empty/populated/profile-local ordered rows",
            "The 1000-entry capacity boundary is not installed-paired here",
        ),
        (
            ".put",
            "Defaults, digest, revision reuse/replacement, errors, nested and outer rollback",
            "Structural diagnostics use native errors, not exact Pydantic text; all invalid-field combinations are unqualified",
        ),
        (
            ".admit",
            "Exact durable generation demand, nested rollback and owner fences",
            "Concurrent competing writers are unqualified",
        ),
        (
            ".enable",
            "Manual enable/disable values, replacement/active-run errors and rollback",
            "Projection ownership transfer and every cron/timezone boundary are not paired here",
        ),
        (
            ".require_run",
            "Valid live lease, wrong ID/definition and expired lease refusal",
            "Native also requires a persisted run row; corrupt lease states are not exhaustive parity",
        ),
        (
            ".history",
            "Empty and full decoded query-run lifecycle rows",
            "The 100-entry truncation boundary and concurrent recovery are unqualified",
        ),
        (
            ".run",
            "Exact source-free query report/event results, baseline, history, repeated/scheduled runs, owner and cancellation fences",
            "Observation/fallback/projection/processing/HTTP runs are native pre-admission refusals, explicitly not parity; bound-source runs and publication are unqualified",
        ),
    )
}
CONTRACTS[PREFIX + "source_events.invalidate"] = dict(
    tested="Full result/reason, dirty generation, enabled-event watcher demand, generated-source ignore, profile isolation and rollback",
    gap="Source observation, native event delivery, live filesystems and queue races are unqualified",
)
EXPECTED_GROUPS = (
    "attributes",
    "empty",
    "put",
    "get",
    "list",
    "idempotent",
    "revision",
    "reuse",
    "admit",
    "enable",
    "disable",
    "other_profile",
    "unknown",
    "semantic_errors",
    "history_empty",
    "require_valid",
    "require_stale",
    "uncommitted",
    "nested_rollback",
    "outer_rollback",
    "invalidate",
    "invalidate_state",
    "invalidate_generated",
    "invalidate_rollback",
    "report_run",
    "report_row",
    "report_history",
    "report_repeat",
    "event_run",
    "event_notifications",
    "scheduled_not_due",
    "owner_usable",
)
PARTIAL_CASES = (
    "observation_run_refusal",
    "fallback_run_refusal",
    "processing_run_refusal",
    "http_run_refusal",
    "projection_run_refusal",
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

    def module(name):
        return importlib.import_module(namespace + "." + name)

    return SimpleNamespace(
        package=importlib.import_module(namespace),
        Store=module("store").Store,
        encode=module("store").encode,
        Application=module("app").Application,
        Watchers=module("watchers").Watchers,
        events=module("source_events"),
        Queries=module("saved_queries").Queries,
        Error=module("domain").CatabolicError,
    )


def trace():
    a, calls = api("native"), {}

    def wrap(function, key):
        @functools.wraps(function)
        def counted(*args, **kwargs):
            calls[key] = calls.get(key, 0) + 1
            return function(*args, **kwargs)

        return counted

    for name in (
        "__init__",
        "get",
        "list",
        "put",
        "admit",
        "enable",
        "require_run",
        "history",
        "run",
    ):
        setattr(
            a.Watchers,
            name,
            wrap(getattr(a.Watchers, name), PREFIX + "watchers.Watchers." + name),
        )
    a.events.invalidate = wrap(a.events.invalidate, PREFIX + "source_events.invalidate")
    return calls


def seed(store, engine):
    """Disposable metadata only: no source bindings/files/media/output/services."""
    a = api(engine)
    with store.transaction() as db:
        db.execute("INSERT INTO profiles VALUES ('alternate')")
        db.execute("INSERT INTO locations VALUES ('hint'),('generated')")
        db.execute("INSERT INTO generated_locations VALUES ('default','generated')")
        db.execute(
            "INSERT INTO items(id,kind,metadata) VALUES ('fixture-item','movie','{}')"
        )
    for profile in ("default", "alternate"):
        query = a.Queries(store, profile).put(
            "fixture",
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT id AS item_id FROM items",
                }
            },
        )
        store.db.execute(
            "UPDATE saved_queries SET id=? WHERE id=?",
            ("query-" + profile, query["id"]),
        )
    assert store.rows("SELECT * FROM bindings") == []
    assert store.rows("SELECT * FROM files") == []


def definition(profile="default", **changes):
    return {"plan": {"kind": "query", "query_id": "query-" + profile}, **changes}


def error(call):
    try:
        call()
    except Exception as exc:
        return {"type": type(exc).__name__, "message": str(exc)}
    raise AssertionError("expected an exception")


class Normalize:
    """Validate nondeterminism, retaining all keys, values and identity links."""

    def __init__(self, started):
        self.started, self.ids = started, {}

    def __call__(self, value, key=""):
        if value is None:
            return None
        if key in ("event_first", "event_last", "completed_at", "created_at"):
            if key == "created_at" and isinstance(value, str):
                value = (
                    datetime.strptime(value, "%Y-%m-%d %H:%M:%S")
                    .replace(tzinfo=timezone.utc)
                    .timestamp()
                )
            assert (
                isinstance(value, (int, float))
                and self.started - 2 <= value <= time.time() + 2
            ), (key, value)
            return "<clock>"
        if key == "worker_pid":
            assert value == os.getpid(), value
            return "<pid>"
        if isinstance(value, dict):
            return {name: self(value[name], name) for name in sorted(value)}
        if isinstance(value, list):
            return [self(item) for item in value]
        if isinstance(value, str):
            if key in ("baseline", "result", "references"):
                return self(json.loads(value))
            try:
                identifier = UUID(value)
            except ValueError:
                return value
            assert identifier.version == 4 and str(identifier) == value
            return self.ids.setdefault(value, "<uuid:" + str(len(self.ids) + 1) + ">")
        return value


def fence_run(store, watchers, name="audit"):
    row = watchers.get(name)
    store.db.execute(
        "INSERT INTO watcher_runs(id,profile,watcher,definition_id,generation,trigger,scheduled_at,started_at,state,worker_pid) VALUES (?,?,?,?,1,'manual',2000000000,2000000000,'observing',?)",
        ("fenced-run", watchers.profile, name, row["definition_id"], os.getpid()),
    )
    store.db.execute(
        "UPDATE watchers SET active_run='fenced-run',lease_until=2000007200,pending_generation=1 WHERE profile=? AND name=?",
        (watchers.profile, name),
    )
    return row["definition_id"]


def unfence_run(store, profile="default", name="audit"):
    store.db.execute("DELETE FROM watcher_runs WHERE id='fenced-run'")
    store.db.execute(
        "UPDATE watchers SET active_run=NULL,lease_until=0 WHERE profile=? AND name=?",
        (profile, name),
    )


def unsupported_definition(kind):
    result = definition()
    if kind == "observation":
        result.update(
            plan={
                "kind": "observation",
                "sources": ["unbound-fixture"],
                "exclusions": [],
            },
            reaction=None,
        )
    elif kind == "fallback":
        result["plan"] = {
            "kind": "fallback",
            "membership_query_id": "query-default",
            "policy_id": "fixture-policy",
            "role": "primary",
            "part": None,
            "variant": "",
        }
    elif kind == "projection":
        result.update(
            plan={
                "kind": "projection",
                "catalog": "fixture-catalog",
                "binding_digest": "fixture-digest",
            },
            reaction="projection",
        )
    elif kind == "processing":
        result.update(
            reaction="processing",
            operation_id="fixture-operation",
            destination="fixture-destination",
        )
    else:
        result.update(reaction="http", mapping={"query": "query-default"})
    return result


def refusal_evidence(a, store, watchers, engine):
    """Persisted unsupported-family witnesses do not execute the oracle effects."""
    evidence = {}
    for kind in ("observation", "fallback", "processing", "http", "projection"):
        name = "unsupported-" + kind
        row = watchers.put(name, definition())
        # This intentionally exercises the adapter's pre-admission discriminator,
        # not validation/admission of the other family's dependencies or effects.
        value = {**row["definition"], **unsupported_definition(kind)}
        store.db.execute(
            "UPDATE watcher_definitions SET definition=? WHERE id=?",
            (a.encode(value), row["definition_id"]),
        )
        if engine == "native":
            before = watchers.get(name)
            caught = error(lambda name=name: watchers.run(name))
            assert caught == {
                "type": "CatabolicError",
                "message": PROJECTION_GATE if kind == "projection" else RUN_GATE,
            }, caught
            assert watchers.get(name) == before
            assert watchers.history(name) == []
            evidence[kind + "_run_refusal"] = {
                "native_refusal": caught,
                "no_admission": True,
                "persisted_fixture_only": True,
            }
        else:
            evidence[kind + "_run_refusal"] = {
                "oracle_not_executed": "Other run families may observe sources or create effects; no paired parity is claimed",
                "persisted_fixture_only": True,
            }
    return evidence


def scenario(root, engine):
    a, output, started = api(engine), {}, time.time()
    with a.Store(root / (engine + ".db"), writable=True) as store:
        app = a.Application(store)
        w = a.Watchers(app)
        output["attributes"] = dict(
            app=w.app is app, store=w.store is store, profile=w.profile
        )
        output["empty"] = w.list()
        original = definition(events=True)
        first = w.put("audit", original)
        output["put"], output["get"] = first, w.get("audit")
        output["list"] = w.list()
        again = w.put("audit", original)
        assert again == first
        output["idempotent"] = again
        revised = w.put("audit", definition(events=True, retry_seconds=45))
        assert (
            revised["revision"] == 2
            and revised["definition_id"] != first["definition_id"]
        )
        output["revision"] = revised
        reused = w.put("audit", original)
        assert reused == first
        output["reuse"] = reused
        output["admit"] = w.admit("audit")
        output["enable"] = w.enable("audit", transfer=True)
        output["semantic_errors"] = [error(lambda: w.put("audit", original))]
        output["disable"] = w.enable("audit", False)
        other = a.Watchers(a.Application(store, "alternate"))
        output["other_profile"] = {
            "empty": other.list(),
            "put": other.put("audit", definition("alternate")),
            "default": w.get("audit"),
        }
        output["unknown"] = [
            error(lambda: w.get("missing")),
            error(lambda: w.admit("missing")),
            error(lambda: w.enable("missing")),
        ]
        output["history_empty"] = w.history("missing")
        identifier = fence_run(store, w)
        output["require_valid"] = w.require_run("audit", "fenced-run", identifier)
        output["require_stale"] = [
            error(lambda: w.require_run("audit", "wrong", identifier)),
            error(lambda: w.require_run("audit", "fenced-run", "wrong-definition")),
        ]
        output["semantic_errors"].extend(
            [
                error(lambda: w.enable("audit", False)),
                error(lambda: w.put("audit", original)),
            ]
        )
        unfence_run(store)
        before = w.get("audit")
        try:
            with store.transaction():
                created = w.put("transaction", definition())
                store.db.execute(
                    "INSERT INTO items(id,kind,metadata) VALUES ('uncommitted-item','movie','{}')"
                )
                report = w.run("transaction", now=2000000050)
                assert report["complete"] and "uncommitted-item" in json.dumps(
                    report["evaluation"]
                )
                repeated = w.run("transaction", now=2000000060)
                assert repeated["evaluation"]["changed"] is False
                output["uncommitted"] = {
                    "row": created,
                    "run": report,
                    "repeated": repeated,
                    "history": w.history("transaction"),
                }
                w.admit("audit")
                after_outer = w.get("audit")
                try:
                    with store.transaction():
                        w.admit("audit")
                        w.enable("audit")
                        raise ValueError("inner rollback")
                except ValueError:
                    pass
                assert w.get("audit") == after_outer
                output["nested_rollback"] = w.get("audit")
                raise ValueError("outer rollback")
        except ValueError:
            pass
        assert w.get("audit") == before
        output["outer_rollback"] = {
            "row": w.get("audit"),
            "missing": error(lambda: w.get("transaction")),
            "history": w.history("transaction"),
            "items": store.rows("SELECT id FROM items ORDER BY id"),
        }
        w.enable("audit")
        output["invalidate"] = [
            a.events.invalidate(app, "hint"),
            a.events.invalidate(app, "hint", reason="fixture-reason"),
        ]
        output["invalidate_state"] = {
            "watcher": w.get("audit"),
            "sources": store.rows(
                "SELECT * FROM observation_sources ORDER BY profile,source"
            ),
            "other": other.get("audit"),
        }
        output["invalidate_generated"] = a.events.invalidate(
            app, "generated", reason="must-not-appear"
        )
        before = w.get("audit")
        try:
            with store.transaction():
                a.events.invalidate(app, "hint", reason="rollback")
                raise ValueError("rollback")
        except ValueError:
            pass
        assert w.get("audit") == before
        output["invalidate_rollback"] = {
            "watcher": w.get("audit"),
            "sources": store.rows(
                "SELECT * FROM observation_sources ORDER BY profile,source"
            ),
        }
        w.put("report", definition())
        output["scheduled_not_due"] = error(
            lambda: w.run("report", trigger="scheduled", now=2000000000)
        )
        output["report_run"] = w.run("report", now=2000000000)
        assert (
            output["report_run"]["complete"]
            and output["report_run"]["observations"] == []
        )
        output["report_row"], output["report_history"] = (
            w.get("report"),
            w.history("report"),
        )
        output["report_repeat"] = w.run("report", now=2000000010)
        assert output["report_repeat"]["evaluation"]["changed"] is False
        w.put("event", definition(reaction="event"))
        output["event_run"] = w.run("event", now=2000000020)
        assert output["event_run"]["complete"]
        w.run("event", now=2000000030)
        events = store.rows("SELECT * FROM consumer_events ORDER BY rowid")
        assert len(events) == 1
        assert (
            events[0]["id"]
            == hashlib.sha256(
                a.encode(
                    ["default", "watcher_changed", [output["event_run"]["run_id"]]]
                ).encode()
            ).hexdigest()
        )
        # The event ID is the verified digest of a nondeterministic run UUID.
        output["event_notifications"] = [{**events[0], "id": "<verified-event-digest>"}]
        partial = refusal_evidence(a, store, w, engine)
        output["owner_usable"] = {
            "query": store.rows("SELECT 41+1 AS answer"),
            "bound_sources": store.rows("SELECT * FROM bindings"),
            "files": store.rows("SELECT * FROM files"),
        }
    return Normalize(started)(output), partial


class InstalledWatchers(unittest.TestCase):
    def setUp(self):
        self.a = api("native")
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "watchers.db"
        self.a.Store.initialize(self.path)
        self.store = self.a.Store(self.path, writable=True)
        self.addCleanup(self.store.close)
        seed(self.store, "native")
        self.app = self.a.Application(self.store)
        self.w = self.a.Watchers(self.app)

    def put(self, name="audit", **changes):
        return self.w.put(name, definition(**changes))

    def test_import_identity_defaults_and_shapes(self):
        import catabolic_native.source_events as events
        import catabolic_native.watchers as module

        self.assertIs(self.a.package.watchers, module)
        self.assertIs(self.a.package.source_events, events)
        self.assertIs(self.w.app, self.app)
        self.assertIs(self.w.store, self.store)
        self.assertEqual(self.w.profile, "default")
        row = self.put()
        self.assertEqual(self.w.get("audit"), row)
        self.assertEqual(
            row["definition"]["schedule"],
            dict(kind="manual", seconds=60, cron="0 * * * *", timezone="UTC"),
        )
        self.assertEqual(row["definition"]["reaction"], "report")
        self.assertEqual(
            row["digest"],
            hashlib.sha256(self.a.encode(row["definition"]).encode()).hexdigest(),
        )
        self.assertEqual(
            set(self.w.list()[0]),
            {
                "name",
                "definition_id",
                "enabled",
                "next_due",
                "pending_generation",
                "completed_generation",
                "active_run",
            },
        )

    def test_revision_reuse_and_replacement_errors(self):
        first = self.put()
        self.assertEqual(self.put(), first)
        revised = self.put(retry_seconds=45)
        self.assertEqual(revised["revision"], 2)
        self.assertEqual(self.put(), first)
        self.w.enable("audit")
        with self.assertRaisesRegex(self.a.Error, "disable and finish watcher"):
            self.put()
        self.w.enable("audit", False)
        fence_run(self.store, self.w)
        with self.assertRaisesRegex(self.a.Error, "watcher run active"):
            self.w.enable("audit", False)
        with self.assertRaisesRegex(self.a.Error, "disable and finish watcher"):
            self.put()

    def test_unknown_names_and_structural_errors(self):
        for call in (
            lambda: self.w.get("missing"),
            lambda: self.w.admit("missing"),
            lambda: self.w.enable("missing"),
            lambda: self.w.run("missing"),
        ):
            with self.assertRaisesRegex(self.a.Error, "unknown watcher"):
                call()
        self.assertEqual(self.w.history("missing"), [])
        for value in (
            {},
            {"unknown": True},
            definition(retry_seconds=0),
            definition(events="not-bool"),
            definition(reaction="unknown"),
            definition(schedule={"kind": "interval", "seconds": 0}),
        ):
            with self.subTest(value=value), self.assertRaises(self.a.Error):
                self.w.put("invalid", value)
        self.assertEqual(self.w.list(), [])

    def test_profiles_and_list_order(self):
        self.put("zeta")
        self.put("alpha")
        other = self.a.Watchers(self.a.Application(self.store, "alternate"))
        self.assertEqual(other.list(), [])
        other.put("alpha", definition("alternate"))
        self.assertEqual([r["name"] for r in self.w.list()], ["alpha", "zeta"])
        self.assertNotEqual(
            other.get("alpha")["definition_id"], self.w.get("alpha")["definition_id"]
        )
        with self.assertRaisesRegex(self.a.Error, "unknown query revision"):
            other.put("wrong", definition())

    def test_uncommitted_definitions_nested_and_outer_rollback(self):
        with self.assertRaisesRegex(ValueError, "outer"):
            with self.store.transaction():
                first = self.put()
                self.assertEqual(self.w.get("audit"), first)
                with self.assertRaisesRegex(ValueError, "inner"):
                    with self.store.transaction():
                        self.w.admit("audit")
                        self.w.enable("audit")
                        self.put("inner")
                        raise ValueError("inner")
                self.assertEqual(self.w.get("audit"), first)
                self.assertEqual(len(self.w.list()), 1)
                raise ValueError("outer")
        self.assertEqual(self.w.list(), [])
        self.assertEqual(self.store.rows("SELECT * FROM watcher_definitions"), [])

    def test_admit_enable_disable_demands(self):
        self.put()
        self.assertEqual(self.w.admit("audit")["pending_generation"], 1)
        self.assertEqual(self.w.admit("audit")["requested_generation"], 2)
        self.assertEqual(self.w.enable("audit", transfer=True)["enabled"], 1)
        row = self.w.enable("audit", False)
        self.assertEqual(
            (row["enabled"], row["requested_generation"], row["pending_generation"]),
            (0, 0, 2),
        )
        self.put("interval", schedule={"kind": "interval", "seconds": 120})
        before = time.time()
        row = self.w.enable("interval")
        self.assertGreaterEqual(row["next_due"], before + 120)
        self.assertLessEqual(row["next_due"], time.time() + 120)
        self.assertEqual(self.w.enable("interval", False)["next_due"], 0)

    def test_require_run_fences(self):
        self.put()
        identifier = fence_run(self.store, self.w)
        self.assertIsNone(self.w.require_run("audit", "fenced-run", identifier))
        for run, definition_id in (("wrong", identifier), ("fenced-run", "wrong")):
            with self.assertRaisesRegex(self.a.Error, "stale_watcher_run"):
                self.w.require_run("audit", run, definition_id)
        self.store.db.execute("UPDATE watchers SET lease_until=1")
        with self.assertRaisesRegex(self.a.Error, "stale_watcher_run"):
            self.w.require_run("audit", "fenced-run", identifier)

    def test_invalidation_reason_generation_profiles_and_generated(self):
        self.put(events=True)
        self.put("disabled", events=True)
        self.put("noevents")
        self.w.enable("audit")
        self.w.enable("noevents")
        reason = object()
        result = self.a.events.invalidate(self.app, "hint", reason=reason)
        self.assertIs(result["reason"], reason)
        self.assertEqual(
            result, dict(source="hint", reason=reason, coverage="full_source")
        )
        self.assertEqual(self.w.get("audit")["pending_generation"], 1)
        self.assertEqual(self.w.get("disabled")["pending_generation"], 0)
        self.assertEqual(self.w.get("noevents")["pending_generation"], 0)
        self.assertEqual(
            self.a.events.invalidate(self.app, "generated"),
            {"ignored": "generated_source_uses_completion_events"},
        )
        self.assertEqual(
            self.store.rows("SELECT * FROM observation_sources"),
            [dict(profile="default", source="hint", dirty_generation=1)],
        )
        other = self.a.Application(self.store, "alternate")
        self.a.events.invalidate(other, "hint")
        self.assertEqual(self.w.get("audit")["pending_generation"], 1)

    def test_invalidation_nested_rollback(self):
        self.put(events=True)
        self.w.enable("audit")
        with self.assertRaises(ValueError):
            with self.store.transaction():
                self.a.events.invalidate(self.app, "hint")
                with self.assertRaises(ValueError):
                    with self.store.transaction():
                        self.a.events.invalidate(self.app, "hint")
                        raise ValueError("inner")
                self.assertEqual(self.w.get("audit")["pending_generation"], 1)
                raise ValueError("outer")
        self.assertEqual(self.w.get("audit")["pending_generation"], 0)
        self.assertEqual(self.store.rows("SELECT * FROM observation_sources"), [])

    def test_query_report_lifecycle_history_and_repeated_baseline(self):
        row = self.put()
        first = self.w.run("audit", now=2000000000)
        self.assertEqual(
            set(first),
            {
                "run_id",
                "definition_id",
                "observations",
                "evaluation",
                "reaction",
                "complete",
            },
        )
        self.assertEqual(first["definition_id"], row["definition_id"])
        self.assertTrue(first["complete"])
        self.assertTrue(first["evaluation"]["changed"])
        self.assertEqual(first["observations"], [])
        again = self.w.run("audit", now=2000000010)
        self.assertFalse(again["evaluation"]["changed"])
        current = self.w.get("audit")
        self.assertEqual(current["completed_generation"], 2)
        self.assertIsNone(current["active_run"])
        self.assertIsNone(current["pending_reaction"])
        history = self.w.history("audit")
        self.assertEqual([r["id"] for r in history], [again["run_id"], first["run_id"]])
        self.assertEqual([r["state"] for r in history], ["complete", "complete"])
        self.assertEqual(json.loads(history[0]["result"]), again)

    def test_query_event_only_emits_on_change(self):
        self.put(reaction="event")
        self.assertTrue(self.w.run("audit")["complete"])
        self.assertTrue(self.w.run("audit")["complete"])
        notifications = self.store.rows("SELECT * FROM consumer_events")
        self.assertEqual(len(notifications), 1)
        self.assertEqual(notifications[0]["event"], "watcher_changed")
        self.store.db.execute(
            "INSERT INTO items(id,kind,metadata) VALUES ('another-item','movie','{}')"
        )
        self.assertTrue(self.w.run("audit")["complete"])
        self.assertEqual(len(self.store.rows("SELECT * FROM consumer_events")), 2)

    def test_scheduled_admission_and_uncommitted_run_rollback(self):
        self.put()
        with self.assertRaisesRegex(self.a.Error, "watcher_not_due"):
            self.w.run("audit", trigger="scheduled")
        self.assertEqual(self.w.history("audit"), [])
        self.w.admit("audit")
        self.assertTrue(self.w.run("audit", trigger="scheduled")["complete"])
        before = self.w.get("audit")
        with self.assertRaisesRegex(ValueError, "rollback"):
            with self.store.transaction() as db:
                db.execute(
                    "INSERT INTO items(id,kind,metadata) VALUES ('uncommitted-item','movie','{}')"
                )
                current = self.w.run("audit")
                self.assertTrue(current["complete"])
                self.assertTrue(current["evaluation"]["changed"])
                self.assertIn("uncommitted-item", json.dumps(current["evaluation"]))
                self.assertFalse(self.w.run("audit")["evaluation"]["changed"])
                raise ValueError("rollback")
        self.assertEqual(self.w.get("audit"), before)
        self.assertEqual(len(self.w.history("audit")), 1)

    def test_readonly_reads_and_mutation_refusal(self):
        row = self.put()
        self.store.close()
        with self.a.Store(self.path) as store:
            app = self.a.Application(store)
            w = self.a.Watchers(app)
            self.assertEqual(w.get("audit"), row)
            self.assertEqual(len(w.list()), 1)
            self.assertEqual(w.history("audit"), [])
            for call in (
                lambda: w.put("other", definition()),
                lambda: w.admit("audit"),
                lambda: w.enable("audit"),
                lambda: w.run("audit"),
                lambda: self.a.events.invalidate(app, "hint"),
            ):
                with self.assertRaises(self.a.Error):
                    call()
            self.assertEqual(w.get("audit"), row)

    def test_closed_detached_reconnect_owner(self):
        row = self.put()
        with self.store.detached():
            for call in (
                lambda: self.w.get("audit"),
                lambda: self.w.list(),
                lambda: self.w.run("audit"),
                lambda: self.a.events.invalidate(self.app, "hint"),
            ):
                with self.assertRaisesRegex(self.a.Error, "detached"):
                    call()
        self.assertEqual(self.w.get("audit"), row)
        self.store.close()
        for call in (
            lambda: self.w.get("audit"),
            lambda: self.w.admit("audit"),
            lambda: self.a.events.invalidate(self.app, "hint"),
        ):
            with self.assertRaises(self.a.Error):
                call()

    def test_foreign_thread_refusal_and_owner_recovery(self):
        row = self.put()
        caught = []

        def foreign():
            for call in (
                lambda: self.w.get("audit"),
                lambda: self.w.run("audit"),
                lambda: self.a.events.invalidate(self.app, "hint"),
            ):
                caught.append(error(call))

        with self.store.transaction():
            thread = threading.Thread(target=foreign)
            thread.start()
            thread.join(5)
        self.assertFalse(thread.is_alive())
        self.assertEqual(len(caught), 3)
        for value in caught:
            self.assertEqual(value["type"], "CatabolicError")
            self.assertIn("thread", value["message"])
        self.assertEqual(self.w.get("audit"), row)

    def test_unsupported_families_refuse_before_admission(self):
        evidence = refusal_evidence(self.a, self.store, self.w, "native")
        self.assertEqual(set(evidence), set(PARTIAL_CASES))
        self.assertEqual(self.store.rows("SELECT * FROM watcher_runs"), [])
        self.assertEqual(self.store.rows("SELECT * FROM bindings"), [])
        self.assertEqual(self.store.rows("SELECT * FROM files"), [])

    def interrupt(self, operation, table, custom):
        self.store.db.execute(
            f"CREATE TRIGGER slow_watcher BEFORE INSERT ON {table} BEGIN SELECT (WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<100000000) SELECT sum(x) FROM n); END"
        )
        number = signal.SIGUSR1 if custom else signal.SIGINT
        old = signal.getsignal(number)
        cancellation = (
            RuntimeError("exact watcher cancellation")
            if custom
            else KeyboardInterrupt("exact watcher interrupt")
        )

        def handler(*_):
            raise cancellation

        signal.signal(number, handler)
        thread = threading.Thread(
            target=lambda: (time.sleep(0.15), os.kill(os.getpid(), number))
        )
        thread.start()
        try:
            with self.assertRaises(type(cancellation)) as caught:
                operation()
            self.assertIs(caught.exception, cancellation)
        finally:
            thread.join(5)
            signal.signal(number, old)
        self.assertFalse(thread.is_alive())
        self.assertFalse(self.store.db.in_transaction)
        self.store.db.execute("DROP TRIGGER slow_watcher")
        self.assertEqual(self.store.rows("SELECT 42 AS answer"), [dict(answer=42)])

    def test_signal_cancellation_put_rolls_back_and_recovers(self):
        self.interrupt(lambda: self.put(), "watcher_definitions", custom=True)
        self.assertEqual(self.w.list(), [])
        self.assertEqual(self.store.rows("SELECT * FROM watcher_definitions"), [])
        self.assertEqual(self.put()["name"], "audit")

    def interrupt_completion(self, inline):
        name = "inline-completion" if inline else "idle-completion"
        before = self.put(name)
        self.store.db.execute(
            "CREATE TRIGGER slow_completion BEFORE UPDATE OF baseline ON watchers "
            "BEGIN SELECT (WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 "
            "FROM n WHERE x<3000000) SELECT sum(x) FROM n); END"
        )
        cancellation = RuntimeError("exact completion cancellation")

        def handler(*_):
            raise cancellation

        old = signal.signal(signal.SIGUSR1, handler)
        thread = threading.Thread(
            target=lambda: (time.sleep(0.15), os.kill(os.getpid(), signal.SIGUSR1))
        )
        thread.start()
        try:
            with self.assertRaises(RuntimeError) as caught:
                if inline:
                    with self.store.transaction() as db:
                        db.execute(
                            "INSERT INTO items(id,kind,metadata) VALUES "
                            "('cancelled-completion-item','movie','{}')"
                        )
                        self.w.run(name)
                else:
                    self.w.run(name)
            self.assertIs(caught.exception, cancellation)
        finally:
            thread.join(5)
            signal.signal(signal.SIGUSR1, old)
        self.assertFalse(thread.is_alive())
        self.assertFalse(self.store.db.in_transaction)
        row, history = self.w.get(name), self.w.history(name)
        self.assertIsNone(row["baseline"])
        self.assertIsNone(row["active_run"])
        self.assertEqual(row["completed_generation"], before["completed_generation"])
        if inline:
            self.assertEqual(row, before)
            self.assertEqual(history, [])
            self.assertEqual(
                self.store.rows(
                    "SELECT id FROM items WHERE id='cancelled-completion-item'"
                ),
                [],
            )
        else:
            self.assertEqual(len(history), 1)
            self.assertEqual(history[0]["state"], "failed")
        self.store.db.execute("DROP TRIGGER slow_completion")
        self.assertTrue(self.w.run(name)["complete"])

    def test_signal_cancellation_run_admission_and_completion_cleanup(self):
        row = self.put()
        self.interrupt(lambda: self.w.run("audit"), "watcher_runs", custom=False)
        self.assertEqual(self.w.get("audit"), row)
        self.assertEqual(self.w.history("audit"), [])
        self.assertTrue(self.w.run("audit")["complete"])
        # A signal first observed after successful commit cannot undo that commit.
        # These fixtures instead interrupt a slow completion mutation. Exact
        # phase entry is independently controlled by the native scalar fixture.
        for inline in (False, True):
            with self.subTest(inline=inline):
                self.interrupt_completion(inline)


EXPECTED_TESTS = (
    "test_import_identity_defaults_and_shapes",
    "test_revision_reuse_and_replacement_errors",
    "test_unknown_names_and_structural_errors",
    "test_profiles_and_list_order",
    "test_uncommitted_definitions_nested_and_outer_rollback",
    "test_admit_enable_disable_demands",
    "test_require_run_fences",
    "test_invalidation_reason_generation_profiles_and_generated",
    "test_invalidation_nested_rollback",
    "test_query_report_lifecycle_history_and_repeated_baseline",
    "test_query_event_only_emits_on_change",
    "test_scheduled_admission_and_uncommitted_run_rollback",
    "test_readonly_reads_and_mutation_refusal",
    "test_closed_detached_reconnect_owner",
    "test_foreign_thread_refusal_and_owner_recovery",
    "test_unsupported_families_refuse_before_admission",
    "test_signal_cancellation_put_rolls_back_and_recovers",
    "test_signal_cancellation_run_admission_and_completion_cleanup",
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
    root = Path(__file__).resolve().parents[1]
    files = (
        "scripts/native_watchers_embedding_tests.py",
        "scripts/native_qualification_results.py",
        "scripts/native_facade_qualification.py",
        "scripts/qualify_native_embedding.py",
        "scripts/qualify_native_wheel.py",
        "scripts/build_native_wheel.py",
        "tests/test_native_watchers_contract.py",
        "crates/catabolic-store/src/watcher.rs",
        "crates/catabolic-store/src/public_watchers.rs",
        "crates/catabolic-store/src/lib.rs",
        "crates/catabolic-store/src/observation.rs",
        "crates/catabolic-store/src/plans.rs",
        "crates/catabolic-store/src/embedding.rs",
        "crates/catabolic-runtime/src/public_watchers.rs",
        "crates/catabolic-runtime/src/watchers.rs",
        "crates/catabolic-runtime/tests/public_watchers_facade.rs",
        "crates/catabolic-runtime/src/lib.rs",
        "crates/catabolic-python/src/embedding.rs",
        "crates/catabolic-python/src/public_watchers.rs",
        "crates/catabolic-python/src/lib.rs",
        "crates/catabolic-python/python/catabolic_native/__init__.py",
        "crates/catabolic-python/python/catabolic_native/watchers.py",
        "crates/catabolic-python/python/catabolic_native/source_events.py",
        "crates/catabolic-python/python/catabolic_native/store.py",
        "crates/catabolic-python/python/catabolic_native/app.py",
    )
    return {
        name: hashlib.sha256((root / name).read_bytes()).hexdigest() for name in files
    }


def main():
    parser = argument_parser()
    clear_stale_report(parser=parser)
    if not __debug__ or os.environ.get("PYTHONOPTIMIZE"):
        raise SystemExit(
            "watchers facade qualification requires assertions; do not run with -O or -OO"
        )
    qualification = sibling("native_facade_qualification")
    args = parser.parse_args()
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
        result, partial = scenario(args.root, args.scenario)
        imports = (
            provenance.reference_imports(
                args.reference_root, oracle["tracked_source_sha256"]
            )
            if oracle
            else None
        )
        print(
            json.dumps(
                dict(
                    result=result,
                    partial=partial,
                    calls=calls,
                    oracle=oracle,
                    imports=imports,
                )
            )
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
    assert args.source_root.resolve() == Path(__file__).resolve().parents[1], (
        "source root differs from qualification checkout"
    )
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
    rows = qualification.inventory_rows(inventory, CONTRACTS, EXPECTED_ROWS)
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
        unittest.defaultTestLoader.loadTestsFromTestCase(InstalledWatchers),
        {InstalledWatchers: EXPECTED_TESTS},
        expected_count=18,
    )
    installed_calls = calls.copy()
    from catabolic_native import Store

    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        Store.initialize(root / "seed.db")
        with Store(root / "seed.db", writable=True) as seed_store:
            seed(seed_store, "native")
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
        qualification.paired_groups(reference, native, EXPECTED_GROUPS, 32)
        assert outputs["reference"]["oracle"] == oracle
        paired_calls = outputs["native"]["calls"]
        partial_evidence = {engine: outputs[engine]["partial"] for engine in outputs}
        assert all(
            set(value) == set(PARTIAL_CASES) for value in partial_evidence.values()
        )
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
        scope="Separate selected ordinary Watchers/source_events checkpoint; callable coverage is not exhaustive parity; no aggregate, default, publication or release gate closure",
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
        non_parity_refusal_evidence=partial_evidence,
        paired_scope="32 exact metadata/query value groups; five unsupported run-family refusals are separately recorded and not equality/parity claims",
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
