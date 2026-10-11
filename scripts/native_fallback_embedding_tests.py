#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Selected 15-row fallback facade installed development qualification.

Helperless unresolved plans and direct captures are deliberate: native Rust
owner tests cover probe-state behavior separately. Neither callable coverage nor
this report closes publication, access, callback, aggregate, or release gates.
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
import time
import unittest
import zipfile
from pathlib import Path
from types import SimpleNamespace

PREFIX = "python:catabolic."
GATE = "native fallback publication is unqualified; only disposable debug fixtures may apply or recover"
APPLY_GATE = "native fallback publication is restricted to disposable debug fixtures pending filesystem qualification"
EXPECTED_ROWS = 15
CONTRACTS = {}
for owner, entries in (
    (
        "fallback_resolution.Resolver",
        (
            (
                "",
                "Class binding, persistent Store/profile and four frozen signatures",
                "Native Access objects are not exposed; foreign Python Access is refused",
            ),
            (
                ".__init__",
                "App/store/profile identity and lazy session allocation",
                "Foreign Access and legacy Python Store sharing are unsupported",
            ),
            (
                ".resolve",
                "Exact helperless unresolved decisions/digests, lazy session, mutable counters and native session fences",
                "Live probe/tier failover and package matrix are owner-tested separately, not installed parity",
            ),
            (
                ".health",
                "Fresh/prior observations, uncommitted visibility and snapshot reads",
                "Clock fields are validated then normalized; concurrent health races are unqualified",
            ),
            (
                ".capture",
                "Direct evidence, active state and mutable primary choices",
                "Python check callbacks are explicitly unsupported; full package/rendition combinations unqualified",
            ),
        ),
    ),
    (
        "fallback_projection.FallbackProjection",
        (
            (
                "",
                "Class binding and six frozen methods",
                "Public publication remains safety-gated",
            ),
            (
                ".__init__",
                "App/store/profile identity",
                "Legacy Python Store sharing is unsupported",
            ),
            (
                ".bind",
                "Complete item membership, same-owner transaction/rollback and exact returned binding",
                "All mapping adoption/layout validation cases are outside this forwarding slice",
            ),
            (
                ".plan",
                "Full unresolved plan/digest, caller session, sealed native ownership",
                "Resolved layouts and live publication are not installed-paired here",
            ),
            (
                ".run",
                "Exact read-only presentation; public apply refusal",
                "Native public apply always refuses; oracle apply mismatch is explicit non-parity evidence",
            ),
            (
                ".apply_prepared",
                "Native sealed token mutation/spoof/profile/owner/drop checks and publication gate",
                "Oracle stale-plan refusal is only a call witness, not native publication parity",
            ),
            (
                ".present",
                "Exact omission of private captures/health and full remaining values",
                "Arbitrary non-JSON Python plan objects unsupported",
            ),
        ),
    ),
    (
        "fallback_worker",
        (
            (
                ".configure",
                "Exact enabled/limits values, refresh settings, nested/outer rollback",
                "Invalid Python types/SQLite exception categories and all numeric boundaries unqualified",
            ),
            (
                ".tick",
                "Empty/disabled parity; native gate/backoff, watcher skip and recovery refusal",
                "Enabled publication intentionally blocked; tick accepts only integer limits 0..1000, unlike legacy negative/unbounded limits; perpetual worker run() is not selected",
            ),
        ),
    ),
):
    for suffix, tested, gap in entries:
        CONTRACTS[PREFIX + owner + suffix] = dict(tested=tested, gap=gap)
CONTRACTS[PREFIX + "fallback_projection.binding"] = dict(
    tested="Absent, present, profile-local, transaction and snapshot binding reads",
    gap="Historical schema and every corrupt binding combination unqualified",
)
EXPECTED_GROUPS = (
    "attributes",
    "binding_missing",
    "capture",
    "capture_primary_choices",
    "capture_inactive",
    "health_fresh",
    "health_prior",
    "resolve",
    "resolve_repeated",
    "resolve_state",
    "resolve_empty",
    "resolve_transaction",
    "bind",
    "binding",
    "binding_other_profile",
    "full_plan",
    "present",
    "run_preview",
    "present_custom",
    "configure",
    "refresh_settings",
    "configure_invalid",
    "configure_missing",
    "configure_rollback",
    "configure_nested_rollback",
    "worker_disabled",
    "worker_empty",
)
PARTIAL_CASES = ("run_apply_refusal", "apply_prepared_refusal")


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
        Resolver=module("fallback_resolution").Resolver,
        projection=module("fallback_projection"),
        worker=module("fallback_worker"),
        Policies=module("fallback_policies").Policies,
        Queries=module("saved_queries").Queries,
        Session=module("evaluation").EvaluationSession,
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

    for owner, prefix, names in (
        (
            a.Resolver,
            "fallback_resolution.Resolver",
            ("__init__", "resolve", "health", "capture"),
        ),
        (
            a.projection.FallbackProjection,
            "fallback_projection.FallbackProjection",
            ("__init__", "bind", "plan", "run", "apply_prepared"),
        ),
        (a.projection, "fallback_projection", ("binding",)),
        (a.worker, "fallback_worker", ("configure", "tick")),
    ):
        for name in names:
            setattr(
                owner, name, wrap(getattr(owner, name), PREFIX + prefix + "." + name)
            )
    projection = a.projection.FallbackProjection
    projection.present = staticmethod(
        wrap(
            projection.present,
            PREFIX + "fallback_projection.FallbackProjection.present",
        )
    )
    return calls


def seed(store, engine):
    """One disposable seed is copied before either engine evaluates it."""
    a = api(engine)
    statements = (
        "INSERT OR IGNORE INTO profiles VALUES ('default')",
        "INSERT INTO profiles VALUES ('alternate')",
        "INSERT INTO catalogs VALUES ('catalog'),('empty'),('unbound')",
        "INSERT INTO locations VALUES ('media')",
        "INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','media',1,1,'[]')",
        "INSERT INTO files(id,location,path) VALUES ('video','media','fixture.mkv')",
        "INSERT INTO observations(profile,file_id,size,mtime_ns,device,inode,status,scan_id) VALUES ('default','video',10,20,30,40,'present','scan')",
        "INSERT INTO items(id,kind,metadata) VALUES ('item','movie','{\"title\":\"Fixture\"}')",
        "INSERT INTO item_files(id,file_id,item_id,role,origin,metadata) VALUES ('association','video','item','primary','explicit','{}')",
    )
    for sql in statements:
        store.db.execute(sql)
    store.db.execute(
        "INSERT INTO meta VALUES ('layout:flat',?)",
        (
            a.encode(
                {
                    "version": 1,
                    "rules": [
                        {"name": "all", "path": "{item.kind}/{item.id}/{file.name}"}
                    ],
                }
            ),
        ),
    )
    queries = {}
    for name, sql in (
        ("candidates", "SELECT id AS file_id FROM files WHERE 0"),
        ("members", "SELECT id AS item_id FROM items"),
        ("empty-members", "SELECT id AS item_id FROM items WHERE 0"),
    ):
        queries[name] = a.Queries(store).put(
            name, {"selection": {"language": "sql", "query": sql}}
        )["id"]
    a.Policies(store).put(
        "policy",
        {"fallbacks": [{"name": "preferred", "query_id": queries["candidates"]}]},
    )


def fixture(a, store):
    app = a.Application(store)
    return SimpleNamespace(
        app=app,
        resolver=a.Resolver(app),
        projection=a.projection.FallbackProjection(app),
        policy=a.Policies(store).get(
            store.rows("SELECT id FROM fallback_policies WHERE name='policy'")[0]["id"]
        ),
        queries={
            row["name"]: row["id"]
            for row in store.rows("SELECT name,id FROM saved_queries")
        },
        row=store.rows(
            "SELECT a.*,f.path,f.location FROM item_files a JOIN files f ON f.id=a.file_id WHERE a.id='association'"
        )[0],
    )


def error(a, action):
    try:
        action()
    except a.Error as exc:
        return str(exc)
    raise AssertionError("expected CatabolicError")


def health_value(value, before, after, *, prior=False):
    assert before <= value["last_check"] <= after
    if prior:
        assert value["healthy_since"] == 100.0
    else:
        assert value["healthy_since"] == value["last_check"]
    return {
        key: val
        for key, val in value.items()
        if key not in ("last_check", "healthy_since")
    } | {"healthy_since": 100.0 if prior else "CLOCK"}


def scenario(root, engine):
    a, output, partial = api(engine), {}, {}
    with a.Store(root / (engine + ".db"), writable=True) as store:
        f = fixture(a, store)
        policy_id = f.policy["id"]
        output["attributes"] = [
            f.resolver.app is f.app,
            f.resolver.store is store,
            f.resolver.profile,
            f.resolver.session is None,
            f.projection.app is f.app,
            f.projection.store is store,
            f.projection.profile,
        ]
        output["binding_missing"] = a.projection.binding(store, "default", "catalog")
        output["capture"] = f.resolver.capture(f.row, f.policy["definition"], "catalog")
        related = {**f.row, "metadata": a.encode({"for_file_id": "video"})}
        first = f.resolver.capture(related, f.policy["definition"], "catalog")
        f.resolver.primary_choices = {"item": "video"}
        second = f.resolver.capture(related, f.policy["definition"], "catalog")
        output["capture_primary_choices"] = [first, second]
        output["capture_inactive"] = f.resolver.capture(
            {**f.row, "active": 0}, f.policy["definition"], "catalog"
        )
        candidate = {"file_id": "video", "revision": "revision"}
        before = time.time()
        health = f.resolver.health("entry", candidate, "catalog")
        output["health_fresh"] = health_value(health, before, time.time())
        store.db.execute(
            "INSERT INTO fallback_health VALUES ('default','catalog','entry','video','revision',100,4,101)"
        )
        before = time.time()
        health = f.resolver.health("entry", candidate, "catalog")
        output["health_prior"] = health_value(health, before, time.time(), prior=True)
        f.resolver.total_checks = 3
        choices = f.resolver.primary_choices
        entries = [{"item_id": "item"}]
        output["resolve"] = f.resolver.resolve(entries, policy_id, catalog="catalog")
        session = f.resolver.session
        output["resolve_repeated"] = f.resolver.resolve(
            entries, policy_id, catalog="catalog", previous={}, manual_failback=True
        )
        output["resolve_state"] = [
            session is f.resolver.session,
            f.resolver.total_checks,
            f.resolver.primary_choices,
            f.resolver.primary_choices is choices,
        ]
        output["resolve_empty"] = error(a, lambda: f.resolver.resolve([], policy_id))
        with store.transaction():
            output["resolve_transaction"] = error(
                a, lambda: f.resolver.resolve(entries, policy_id)
            )
        output["bind"] = f.projection.bind(
            "catalog", policy_id, f.queries["members"], "flat"
        )
        output["binding"] = a.projection.binding(store, "default", "catalog")
        output["binding_other_profile"] = a.projection.binding(
            store, "alternate", "catalog"
        )
        plan = f.projection.plan(
            "catalog", session=a.Session(store), manual_failback=True
        )
        assert plan["safe"] is False and plan["desired"][0]["state"] == "unresolved"
        output["full_plan"] = dict(plan)
        output["present"] = f.projection.present(plan)
        output["run_preview"] = f.projection.run("catalog")
        output["present_custom"] = f.projection.present(
            {"captures": {"private": 1}, "health": [1], "safe": False, "extra": ["é"]}
        )
        # These calls qualify only explicit refusal paths. Their diagnostics and
        # semantics differ intentionally, so they are never paired as equality.
        partial["run_apply_refusal"] = error(
            a, lambda: f.projection.run("catalog", apply=True, expected_plan="wrong")
        )
        if engine == "native":
            partial["apply_prepared_refusal"] = error(
                a, lambda: f.projection.apply_prepared(plan)
            )
            assert (
                GATE in partial["run_apply_refusal"]
                and APPLY_GATE in partial["apply_prepared_refusal"]
            )
        else:
            partial["apply_prepared_refusal"] = error(
                a, lambda: f.projection.apply_prepared({**plan, "epoch": -1})
            )
            assert partial == dict(
                run_apply_refusal="stale_or_missing_resolution_plan",
                apply_prepared_refusal="stale_resolution_plan",
            )
        output["configure"] = a.worker.configure(
            f.app, "catalog", enabled=True, interval=7, max_changes=8, max_removals=2
        )
        output["refresh_settings"] = store.rows(
            "SELECT * FROM catalog_refresh_settings ORDER BY catalog"
        )
        output["configure_invalid"] = [
            error(
                a,
                lambda options=options: a.worker.configure(
                    f.app, "catalog", enabled=False, **options
                ),
            )
            for options in (
                {"interval": 0},
                {"interval": 86401},
                {"max_changes": 0},
                {"max_removals": -1},
            )
        ]
        output["configure_missing"] = error(
            a, lambda: a.worker.configure(f.app, "unbound", enabled=False)
        )
        original = a.projection.binding(store, "default", "catalog")
        try:
            with store.transaction():
                a.worker.configure(f.app, "catalog", enabled=False, interval=11)
                raise ValueError("rollback")
        except ValueError:
            pass
        output["configure_rollback"] = (
            a.projection.binding(store, "default", "catalog") == original
        )
        with store.transaction():
            a.worker.configure(f.app, "catalog", enabled=False, interval=12)
            try:
                with store.transaction():
                    a.worker.configure(f.app, "catalog", enabled=True, interval=13)
                    raise ValueError("inner rollback")
            except ValueError:
                pass
            output["configure_nested_rollback"] = a.projection.binding(
                store, "default", "catalog"
            )
    output["worker_disabled"] = a.worker.tick(root / (engine + ".db"))
    output["worker_empty"] = a.worker.tick(
        root / (engine + ".db"), "alternate", limit=0
    )
    assert set(output) == set(EXPECTED_GROUPS)
    assert set(partial) == set(PARTIAL_CASES)
    return output, partial


class InstalledFallback(unittest.TestCase):
    def setUp(self):
        self.a = api("native")
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / "catalog.db"
        self.a.Store.initialize(self.path)
        self.store = self.a.Store(self.path, writable=True)
        self.addCleanup(self.store.close)
        seed(self.store, "native")
        self.f = fixture(self.a, self.store)

    def bind(self):
        return self.f.projection.bind(
            "catalog", self.f.policy["id"], self.f.queries["members"], "flat"
        )

    def resolve(self, resolver=None):
        return (resolver or self.f.resolver).resolve(
            [{"item_id": "item"}], self.f.policy["id"], catalog="catalog"
        )

    def test_import_binding_attributes_and_lazy_session(self):
        self.assertIs(self.a.package.fallback_projection, self.a.projection)
        self.assertIs(self.a.package.fallback_worker, self.a.worker)
        self.assertIs(self.f.resolver.app, self.f.app)
        self.assertIs(self.f.projection.store, self.store)
        self.assertIsNone(self.a.projection.binding(self.store, "default", "catalog"))
        self.assertIsNone(self.f.resolver.session)
        with self.assertRaisesRegex(self.a.Error, "resolution_entry_budget"):
            self.f.resolver.resolve([], self.f.policy["id"])
        self.assertIsNone(self.f.resolver.session)
        self.resolve()
        self.assertIsInstance(self.f.resolver.session, self.a.Session)

    def test_resolver_reuses_session_and_mutable_state(self):
        self.f.resolver.total_checks = 9
        self.f.resolver.primary_choices = {"item": "video"}
        choices = self.f.resolver.primary_choices
        first = self.resolve()
        session = self.f.resolver.session
        self.assertEqual(self.resolve(), first)
        self.assertIs(self.f.resolver.session, session)
        self.assertIs(self.f.resolver.primary_choices, choices)
        self.assertEqual(self.f.resolver.total_checks, 9)
        self.assertEqual(self.f.resolver.primary_choices, {"item": "video"})
        self.assertEqual(first["checks"], 0)
        self.assertEqual(first["decisions"][0]["reasons"], ["no_usable_candidate"])
        self.assertEqual(first["captures"], {})
        self.assertTrue(
            all(
                row["owner"] is None and row["pid"] is None
                for row in self.store.rows("SELECT * FROM fallback_probe_slots")
            )
        )

    def test_capture_evidence_primary_choices_and_no_callback(self):
        f = self.f
        captured = f.resolver.capture(f.row, f.policy["definition"], "catalog")
        self.assertEqual(captured["file_id"], "video")
        self.assertIsInstance(captured["metadata"], dict)
        self.assertEqual(len(captured["checks"]), 1)
        self.assertEqual(
            captured["evidence"]["semantic_relationship"], "curated_association"
        )
        row = {**f.row, "metadata": self.a.encode({"for_file_id": "video"})}
        self.assertIsNone(f.resolver.capture(row, f.policy["definition"], "catalog"))
        f.resolver.primary_choices["item"] = "video"
        self.assertIsNotNone(f.resolver.capture(row, f.policy["definition"], "catalog"))
        with self.assertRaisesRegex(self.a.Error, "check callbacks"):
            f.resolver.capture(
                f.row,
                f.policy["definition"],
                "catalog",
                check=lambda *_: self.fail("callback called"),
            )

    def test_health_same_store_transaction_and_rollback(self):
        candidate = {"file_id": "video", "revision": "r"}
        try:
            with self.store.transaction():
                self.store.db.execute(
                    "INSERT INTO fallback_health VALUES ('default','catalog','entry','video','r',100,5,101)"
                )
                result = self.f.resolver.health("entry", candidate, "catalog")
                self.assertEqual(
                    (result["successes"], result["healthy_since"]), (6, 100.0)
                )
                self.assertEqual(
                    len(self.store.rows("SELECT * FROM fallback_health")), 1
                )
                raise ValueError("rollback")
        except ValueError:
            pass
        self.assertEqual(
            self.f.resolver.health("entry", candidate, "catalog")["successes"], 1
        )
        self.assertEqual(self.store.rows("SELECT * FROM fallback_health"), [])

    def test_resolve_rejects_open_transaction_and_readonly(self):
        with self.store.transaction():
            with self.assertRaisesRegex(self.a.Error, "outside a transaction"):
                self.resolve()
        with self.a.Store(self.path) as reader:
            f = fixture(self.a, reader)
            with self.assertRaisesRegex(self.a.Error, "short writable session"):
                f.resolver.resolve([{"item_id": "item"}], f.policy["id"])

    def test_bind_and_configure_nested_outer_rollback(self):
        try:
            with self.store.transaction():
                self.bind()
                original = self.a.worker.configure(
                    self.f.app, "catalog", enabled=True, interval=12
                )
                try:
                    with self.store.transaction():
                        self.a.worker.configure(
                            self.f.app, "catalog", enabled=False, interval=15
                        )
                        raise ValueError("inner")
                except ValueError:
                    pass
                self.assertEqual(
                    self.a.projection.binding(self.store, "default", "catalog"),
                    original,
                )
                raise ValueError("outer")
        except ValueError:
            pass
        self.assertIsNone(self.a.projection.binding(self.store, "default", "catalog"))
        self.assertEqual(self.store.rows("SELECT * FROM catalog_refresh_settings"), [])

    def test_read_snapshot_and_readonly_configure(self):
        self.bind()
        self.store.db.execute("PRAGMA journal_mode=WAL")
        with self.a.Store(self.path) as reader:
            before = self.a.projection.binding(reader, "default", "catalog")
            self.a.worker.configure(self.f.app, "catalog", enabled=True, interval=4)
            self.assertEqual(
                self.a.projection.binding(reader, "default", "catalog"), before
            )
            with self.assertRaises(self.a.Error):
                self.a.worker.configure(
                    self.a.Application(reader), "catalog", enabled=False
                )

    def test_closed_and_detached_owner_refusal(self):
        with self.store.detached():
            with self.assertRaisesRegex(self.a.Error, "detached"):
                self.a.projection.binding(self.store, "default", "catalog")
            with self.assertRaisesRegex(self.a.Error, "detached"):
                self.f.resolver.health(
                    "entry", {"file_id": "video", "revision": "r"}, "catalog"
                )
        self.store.close()
        with self.assertRaisesRegex(self.a.Error, "closed"):
            self.a.projection.binding(self.store, "default", "catalog")

    def test_foreign_sessions_profiles_and_access_refused(self):
        self.bind()
        with self.assertRaisesRegex(self.a.Error, "foreign Python Access"):
            self.a.Resolver(self.f.app, access=object())
        for session in (object(), self.a.Session(self.store, "alternate")):
            with self.assertRaisesRegex(
                self.a.Error, "foreign Python|identity_mismatch"
            ):
                self.f.projection.plan("catalog", session=session)
        other = self.path.parent / "foreign.db"
        self.a.Store.initialize(other)
        with self.a.Store(other, writable=True) as foreign:
            session = self.a.Session(foreign)
            session.cancelled = True
            with self.assertRaisesRegex(self.a.Error, "identity_mismatch"):
                self.f.projection.plan("catalog", session=session)
        self.assertFalse(self.f.projection.plan("catalog")["safe"])

    def test_session_cumulative_bytes_deadline_cancel_and_drop(self):
        session = self.a.Session(self.store, max_bytes=64)
        session.account("x" * 60)
        resolver = self.a.Resolver(self.f.app, session=session)
        result = self.resolve(resolver)
        self.assertEqual(result["decisions"][0]["state"], "blocked")
        self.assertIn("byte_budget", result["tiers"][0]["reason"])
        for cause in ("cancel", "deadline", "drop"):
            session = self.a.Session(self.store)
            if cause == "cancel":
                session.cancelled = True
            elif cause == "deadline":
                session.pause_for_io(-6)
            else:
                session.close()
            with self.assertRaisesRegex(
                self.a.Error, "evaluation_cancelled|timed out|identity_mismatch"
            ):
                self.resolve(self.a.Resolver(self.f.app, session=session))
        self.assertEqual(self.resolve()["decisions"][0]["state"], "unresolved")

    def test_stale_session_and_owner_recovery(self):
        session = self.a.Session(self.store)
        self.store.db.execute("UPDATE observations SET size=11")
        session.cancelled = True
        with self.assertRaisesRegex(self.a.Error, "stale_evaluation_snapshot"):
            self.resolve(self.a.Resolver(self.f.app, session=session))
        self.assertEqual(self.resolve()["decisions"][0]["state"], "unresolved")

    def test_full_plan_present_and_preview(self):
        self.bind()
        plan = self.f.projection.plan(
            "catalog", manual_failback=True, session=self.a.Session(self.store)
        )
        self.assertIsInstance(plan, dict)
        self.assertIn("captures", plan)
        self.assertIn("health", plan)
        self.assertFalse(plan["safe"])
        self.assertEqual(plan["desired"][0]["state"], "unresolved")
        shown = self.f.projection.present(plan)
        self.assertEqual(
            shown,
            {
                key: value
                for key, value in plan.items()
                if key not in ("captures", "health")
            },
        )
        self.assertEqual(self.f.projection.run("catalog"), shown)
        self.assertEqual(self.store.rows("SELECT * FROM fallback_entries"), [])
        self.assertEqual(self.store.rows("SELECT * FROM journal"), [])

    def test_plan_session_budget_and_cancel(self):
        self.bind()
        with self.assertRaisesRegex(
            self.a.Error, "byte_budget|incomplete_logical_membership"
        ):
            self.f.projection.plan(
                "catalog", session=self.a.Session(self.store, max_bytes=1)
            )
        session = self.a.Session(self.store)
        session.cancelled = True
        with self.assertRaisesRegex(self.a.Error, "evaluation_cancelled"):
            self.f.projection.plan("catalog", session=session)
        self.assertFalse(self.f.projection.plan("catalog")["safe"])

    def test_prepared_token_spoof_mutation_owner_and_drop(self):
        self.bind()
        plan = self.f.projection.plan("catalog")
        with self.assertRaisesRegex(self.a.Error, "identity mismatch"):
            self.f.projection.apply_prepared(dict(plan))
        plan["safe"] = True
        with self.assertRaisesRegex(self.a.Error, "changed or belongs"):
            self.f.projection.apply_prepared(plan)
        plan["safe"] = False
        alternate = self.a.projection.FallbackProjection(
            self.a.Application(self.store, "alternate")
        )
        with self.assertRaisesRegex(self.a.Error, "identity mismatch"):
            alternate.apply_prepared(plan)
        other = self.path.parent / "other.db"
        self.a.Store.initialize(other)
        with self.a.Store(other, writable=True) as foreign:
            projection = self.a.projection.FallbackProjection(
                self.a.Application(foreign)
            )
            with self.assertRaisesRegex(self.a.Error, "identity mismatch"):
                projection.apply_prepared(plan)
        self.store._call(
            "fallback", "projection-drop", dict(plan_token=plan._token), "default"
        )
        with self.assertRaisesRegex(self.a.Error, "identity mismatch"):
            self.f.projection.apply_prepared(plan)
        self.f.projection.bind(
            "empty", self.f.policy["id"], self.f.queries["empty-members"], "flat"
        )
        held = [self.f.projection.plan("empty") for _ in range(256)]
        with self.assertRaisesRegex(self.a.Error, "prepared projection handle limit"):
            self.f.projection.plan("empty")
        del held[-1]
        self.assertTrue(self.f.projection.plan("empty")["safe"])

    def test_public_apply_safety_gate_has_no_override(self):
        self.bind()
        plan = self.f.projection.plan("catalog")
        for action, message in (
            (
                lambda: self.f.projection.run(
                    "catalog", apply=True, expected_plan=plan["plan_id"]
                ),
                GATE,
            ),
            (
                lambda: self.f.projection.run("catalog", apply=True, automatic=True),
                GATE,
            ),
            (lambda: self.f.projection.apply_prepared(plan), APPLY_GATE),
        ):
            self.assertEqual(error(self.a, action), message)
        with self.assertRaises(TypeError):
            self.f.projection.run("catalog", apply=True, disposable_fixture=True)
        self.assertEqual(self.store.rows("SELECT * FROM fallback_entries"), [])
        self.assertEqual(self.store.rows("SELECT * FROM journal"), [])

    def test_configure_validation_and_disabled_empty_tick(self):
        with self.assertRaisesRegex(self.a.Error, "bind an explicit"):
            self.a.worker.configure(self.f.app, "catalog", enabled=True)
        self.bind()
        for kwargs in ({"interval": 0}, {"max_changes": 0}, {"max_removals": -1}):
            with self.assertRaisesRegex(
                self.a.Error, "invalid fallback maintenance limits"
            ):
                self.a.worker.configure(self.f.app, "catalog", enabled=True, **kwargs)
        config = self.a.worker.configure(self.f.app, "catalog", enabled=False)
        self.assertEqual(config["enabled"], 0)
        self.store.close()
        self.assertEqual(
            self.a.worker.tick(self.path),
            dict(processed=0, complete=True, projections=[]),
        )
        self.assertEqual(
            self.a.worker.tick(self.path, "alternate", limit=0),
            dict(processed=0, complete=True, projections=[]),
        )

        for limit in (-1, 1001, 1.5):
            with self.assertRaisesRegex(
                self.a.Error, "limit must be between 0 and 1000"
            ):
                self.a.worker.tick(self.path, limit=limit)

    def test_worker_gate_backoff_and_recovery_refusal(self):
        self.bind()
        self.a.worker.configure(self.f.app, "catalog", enabled=True, interval=60)
        self.store.close()
        before = time.time()
        result = self.a.worker.tick(self.path, limit=1)
        self.assertEqual(result["processed"], 1)
        self.assertFalse(result["complete"])
        self.assertIn(GATE, result["projections"][0]["error"])
        with self.a.Store(self.path, writable=True) as store:
            config = self.a.projection.binding(store, "default", "catalog")
            self.assertGreaterEqual(config["next_attempt"], before + 60)
            self.assertEqual(store.rows("SELECT * FROM fallback_entries"), [])
            store.db.execute("UPDATE fallback_bindings SET next_attempt=0")
            store.db.execute(
                "INSERT INTO journal(id,profile,catalog,path,kind) VALUES ('pending','default','catalog','fixture','create')"
            )
        result = self.a.worker.tick(self.path)
        self.assertEqual(result["processed"], 1)
        self.assertIn(GATE, result["projections"][0]["error"])
        with self.a.Store(self.path) as store:
            self.assertEqual(len(store.rows("SELECT * FROM journal")), 1)
        self.assertEqual(self.a.worker.tick(self.path)["processed"], 0)

    def test_worker_watcher_owner_skip(self):
        self.bind()
        self.a.worker.configure(self.f.app, "catalog", enabled=True)
        self.store.db.execute(
            "INSERT INTO watcher_definitions VALUES ('watch-definition','default','watcher',1,'{}','digest',0)"
        )
        self.store.db.execute(
            "INSERT INTO watchers(profile,name,definition_id) VALUES ('default','watcher','watch-definition')"
        )
        self.store.db.execute(
            "INSERT INTO watcher_owners VALUES ('default','catalog','watcher')"
        )
        self.store.close()
        self.assertEqual(
            self.a.worker.tick(self.path),
            dict(processed=0, complete=True, projections=[]),
        )
        with self.a.Store(self.path) as store:
            self.assertEqual(
                self.a.projection.binding(store, "default", "catalog")["next_attempt"],
                0,
            )


EXPECTED_TESTS = (
    "test_import_binding_attributes_and_lazy_session",
    "test_resolver_reuses_session_and_mutable_state",
    "test_capture_evidence_primary_choices_and_no_callback",
    "test_health_same_store_transaction_and_rollback",
    "test_resolve_rejects_open_transaction_and_readonly",
    "test_bind_and_configure_nested_outer_rollback",
    "test_read_snapshot_and_readonly_configure",
    "test_closed_and_detached_owner_refusal",
    "test_foreign_sessions_profiles_and_access_refused",
    "test_session_cumulative_bytes_deadline_cancel_and_drop",
    "test_stale_session_and_owner_recovery",
    "test_full_plan_present_and_preview",
    "test_plan_session_budget_and_cancel",
    "test_prepared_token_spoof_mutation_owner_and_drop",
    "test_public_apply_safety_gate_has_no_override",
    "test_configure_validation_and_disabled_empty_tick",
    "test_worker_gate_backoff_and_recovery_refusal",
    "test_worker_watcher_owner_skip",
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
        "scripts/native_fallback_embedding_tests.py",
        "scripts/native_qualification_results.py",
        "scripts/native_facade_qualification.py",
        "scripts/qualify_native_embedding.py",
        "scripts/build_native_wheel.py",
        "tests/test_native_fallback_embedding_contract.py",
        "crates/catabolic-store/src/public_fallback.rs",
        "crates/catabolic-store/src/fallback_resolution.rs",
        "crates/catabolic-store/src/fallback_projection.rs",
        "crates/catabolic-store/src/embedding.rs",
        "crates/catabolic-store/src/lib.rs",
        "crates/catabolic-runtime/src/public_fallback.rs",
        "crates/catabolic-runtime/tests/public_fallback_facade.rs",
        "crates/catabolic-runtime/src/fallback.rs",
        "crates/catabolic-runtime/src/lib.rs",
        "crates/catabolic-python/src/embedding.rs",
        "crates/catabolic-python/src/public_fallback.rs",
        "crates/catabolic-python/src/lib.rs",
        "crates/catabolic-python/python/catabolic_native/__init__.py",
        "crates/catabolic-python/python/catabolic_native/fallback_resolution.py",
        "crates/catabolic-python/python/catabolic_native/fallback_projection.py",
        "crates/catabolic-python/python/catabolic_native/fallback_worker.py",
        "crates/catabolic-python/python/catabolic_native/store.py",
        "crates/catabolic-python/python/catabolic_native/evaluation.py",
    )
    return {
        name: hashlib.sha256((root / name).read_bytes()).hexdigest() for name in files
    }


def main():
    if not __debug__ or os.environ.get("PYTHONOPTIMIZE"):
        raise SystemExit(
            "fallback facade qualification requires assertions; do not run with -O or -OO"
        )
    # An explicitly reused output path must never retain a stale success when
    # the new attempt fails, including while importing shared gate helpers.
    receipt_parser = argparse.ArgumentParser(add_help=False)
    receipt_parser.add_argument("--report", type=Path)
    receipt_args, _ = receipt_parser.parse_known_args()
    if receipt_args.report:
        receipt_args.report.unlink(missing_ok=True)
        receipt_args.report.with_suffix(".attempt.json").unlink(missing_ok=True)
    qualification = sibling("native_facade_qualification")
    parser = argparse.ArgumentParser(description=__doc__)
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
        unittest.defaultTestLoader.loadTestsFromTestCase(InstalledFallback),
        {InstalledFallback: EXPECTED_TESTS},
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
        qualification.paired_groups(reference, native, EXPECTED_GROUPS, 27)
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
        scope="Separate selected fallback facade checkpoint; callable coverage is not exhaustive parity; no aggregate, default, publication or release gate closure",
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
        paired_scope="27 exact value groups; explicit publication refusal calls are separately recorded and not equality/parity claims",
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
