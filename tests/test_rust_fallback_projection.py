# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native logical projection against frozen plans and independent durable state."""

import copy
import hashlib
import json
import os
import subprocess
import unittest
from unittest.mock import patch

from catabolic.fallback_policies import Policies
from catabolic.fallback_projection import FallbackProjection
from catabolic.fallback_resolution import Resolver
from catabolic.layouts import PRESETS, Layouts
from catabolic.reconcile import Reconciler
from catabolic.saved_queries import Queries
from tests import test_fallback_evidence as evidence_fixtures
from tests import test_fallback_resolution as fixtures
from tests.test_rust_migration import BINARY


class NativeProjectionTest(unittest.TestCase):
    setUp = fixtures.ResolverTest.setUp
    policy = fixtures.ResolverTest.policy
    maxDiff = None

    def native(
        self, action, options, *, failure=None, surface="fallback", code=None, env=None
    ):
        options = {"disposable_fixture": True, **options}
        with self.store.detached():
            p = subprocess.run(
                [
                    str(BINARY),
                    "--db",
                    str(self.database),
                    "--json",
                    "operation",
                    surface,
                    action,
                    "--options",
                    json.dumps(options),
                ],
                text=True,
                capture_output=True,
                timeout=60,
                env=None if env is None else {**os.environ, **env},
            )
        if code is not None:
            self.assertEqual(p.returncode, code, p.stdout + p.stderr)
            return None
        if failure:
            self.assertNotEqual(p.returncode, 0)
            self.assertIn(failure, p.stdout + p.stderr)
            return None
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        return json.loads(p.stdout)

    def bind(self, mode="immediate", seconds=0):
        definition = Policies(self.store).get(self.policy())["definition"]
        definition["failback"] = {
            "mode": mode,
            "minimum_healthy_seconds": seconds,
            "minimum_successful_checks": 2,
        }
        policy = Policies(self.store).put("projection-policy", definition)["id"]
        query = Queries(self.store).put(
            "members",
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT id AS item_id FROM items",
                }
            },
        )["id"]
        Layouts(self.app).put("flat", copy.deepcopy(PRESETS["flat"]))
        result = self.native(
            "bind",
            {
                "catalog": "global",
                "policy_id": policy,
                "query_id": query,
                "layout": "flat",
            },
        )
        self.assertEqual(result, self.store.rows("SELECT * FROM fallback_bindings")[0])
        return FallbackProjection(self.app)

    def plan_pair(self):
        before = {
            table: self.store.rows(f"SELECT * FROM {table}")
            for table in [
                "fallback_health",
                "fallback_entries",
                "fallback_history",
                "journal",
            ]
        }
        expected = json.loads(json.dumps(FallbackProjection(self.app).run("global")))
        actual = self.native("run", {"catalog": "global"})
        self.assertEqual(actual, expected)
        for table, rows in before.items():
            self.assertEqual(rows, self.store.rows(f"SELECT * FROM {table}"), table)
        return actual

    def apply(self, **kw):
        plan = self.plan_pair()
        return self.native(
            "run",
            {
                "catalog": "global",
                "apply": True,
                "expected_plan": plan["plan_id"],
                **kw,
            },
        )

    def test_nonfixture_apply_refuses_before_any_generation_admission(self):
        self.bind()
        plan = self.plan_pair()
        self.native(
            "run",
            {
                "catalog": "global",
                "apply": True,
                "expected_plan": plan["plan_id"],
                "disposable_fixture": False,
            },
            failure="restricted to disposable debug fixtures",
        )
        self.assertEqual(self.store.rows("SELECT * FROM fallback_entries"), [])
        self.assertEqual(self.store.rows("SELECT * FROM fallback_history"), [])
        self.assertEqual(self.store.rows("SELECT * FROM journal"), [])
        self.assertEqual(
            self.store.rows("SELECT generation FROM fallback_bindings"),
            [{"generation": 0}],
        )
        self.assertEqual(list(self.output.rglob("*")), [])

    def test_bulk_reconciliation_covers_mixed_exact_and_fallback_catalogs(self):
        self.bind()
        self.assertTrue(self.apply()["complete"])
        output = self.root / "exact-output"
        output.mkdir()
        self.app.bind("output", "exact", str(output))
        self.app.put_mapping("exact", self.files["backup.bin"], self.item, "exact.bin")
        self.assertTrue(Reconciler(self.app).apply("exact")["healthy"])
        for operation in ("preview", "verify", "apply", "recover"):
            with self.subTest(operation=operation):
                expected = getattr(Reconciler(self.app), operation)(None)
                actual = self.native(operation, {"catalog": None}, surface="outputs")
                self.assertEqual(actual, expected)
        self.assertEqual(self.store.rows("SELECT * FROM journal"), [])
        entry = self.store.rows("SELECT * FROM fallback_entries")[0]
        self.assertEqual(entry["generation"], entry["published_generation"])

    def test_preview_is_exact_and_write_free(self):
        self.bind()
        result = self.plan_pair()
        self.assertTrue(result["safe"])
        self.assertEqual(result["desired"][0]["file_id"], self.files["main.bin"])
        self.assertEqual(list(self.output.rglob("*")), [])

    def test_retarget_and_all_offline_retain_membership(self):
        self.bind()
        first = self.apply()
        self.assertTrue(first["complete"], first)
        link = next(p for p in self.output.rglob("*") if p.is_symlink())
        previous_path = str(link)
        previous_target = link.readlink()
        (self.source / "main.bin").unlink()
        second = self.apply()
        self.assertTrue(second["complete"], second)
        self.assertEqual(str(link), previous_path)
        self.assertNotEqual(link.readlink(), previous_target)
        saved = link.readlink()
        row = self.store.rows("SELECT * FROM fallback_entries")[0]
        self.assertEqual(row["generation"], row["published_generation"])
        (self.source / "backup.bin").unlink()
        third = self.apply()
        self.assertFalse(third["applied"])
        self.assertTrue(third["membership_retained"])
        self.assertEqual(link.readlink(), saved)
        retained = self.store.rows("SELECT * FROM fallback_entries WHERE active=1")[0]
        self.assertEqual(retained["file_id"], row["file_id"])
        self.assertEqual(retained["published_generation"], row["published_generation"])
        self.assertGreater(retained["generation"], retained["published_generation"])
        self.assertEqual(self.store.rows("SELECT * FROM journal"), [])

    def test_preview_does_not_advance_stability_and_restart_keeps_health(self):
        self.bind("stable", 300)
        self.assertTrue(self.apply()["complete"])
        primary = self.source / "main.bin"
        offline = self.root / "offline.bin"
        primary.rename(offline)
        self.assertEqual(
            self.apply()["desired"][0]["file_id"], self.files["backup.bin"]
        )
        offline.rename(primary)
        for _ in range(3):
            self.assertEqual(
                self.plan_pair()["desired"][0]["file_id"], self.files["backup.bin"]
            )
        self.assertEqual(self.store.rows("SELECT * FROM fallback_health"), [])
        self.apply()
        health = self.store.rows("SELECT * FROM fallback_health")[0]
        self.assertEqual(health["successes"], 1)
        self.plan_pair()
        self.assertEqual(self.store.rows("SELECT * FROM fallback_health")[0], health)
        primary.rename(offline)
        self.apply()
        self.assertEqual(self.store.rows("SELECT * FROM fallback_health"), [])
        offline.rename(primary)
        self.apply()
        with self.store.transaction() as db:
            db.execute("UPDATE fallback_health SET healthy_since=healthy_since-301")
        self.assertEqual(self.apply()["desired"][0]["file_id"], self.files["main.bin"])

    def test_manual_never_delays_escape_from_failed_previous(self):
        self.bind("manual")
        self.apply()
        primary = self.source / "main.bin"
        offline = self.root / "offline.bin"
        primary.rename(offline)
        self.apply()
        offline.rename(primary)
        self.assertEqual(
            self.plan_pair()["desired"][0]["file_id"], self.files["backup.bin"]
        )
        (self.source / "backup.bin").unlink()
        self.assertEqual(self.apply()["desired"][0]["file_id"], self.files["main.bin"])

    def test_stale_plan_and_change_budget_cannot_admit(self):
        self.bind()
        plan = self.plan_pair()
        self.native(
            "run",
            {
                "catalog": "global",
                "apply": True,
                "expected_plan": plan["plan_id"],
                "max_changes": 0,
            },
            failure="fallback_change_or_removal_budget",
        )
        self.assertEqual(self.store.rows("SELECT * FROM fallback_entries"), [])
        with self.store.transaction() as db:
            db.execute(
                "UPDATE items SET metadata=json_set(metadata,'$.title','changed') WHERE id=?",
                (self.item,),
            )
        self.native(
            "run",
            {"catalog": "global", "apply": True, "expected_plan": plan["plan_id"]},
            failure="stale_or_missing_resolution_plan",
        )
        self.assertEqual(self.store.rows("SELECT * FROM fallback_entries"), [])
        self.assertEqual(list(self.output.rglob("*")), [])

    def database_state(self):
        tables = [
            row["name"]
            for row in self.store.rows(
                "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
            )
        ]
        return {table: self.store.rows(f'SELECT * FROM "{table}"') for table in tables}

    def test_default_feature_build_refuses_failpoints_before_any_admission(self):
        # Sol round 2: projection admission used to commit membership,
        # history and generation changes before the executor refused a
        # failpoint field. A default-feature build now refuses first.
        self.bind()
        plan = self.plan_pair()
        before = self.database_state()
        simulate = {"CATABOLIC_TEST_SIMULATE_DEFAULT_FEATURES": "1"}
        for options in (
            {"apply": True, "expected_plan": plan["plan_id"], "failpoint": None},
            {"apply": True, "expected_plan": plan["plan_id"], "crash": False},
            {"apply": True, "expected_plan": "stale", "failpoint": None},
            {"failpoint": None},
        ):
            with self.subTest(options=options):
                self.native(
                    "run",
                    {"catalog": "global", **options},
                    failure="failpoint_injection_unavailable",
                    env=simulate,
                )
                self.assertEqual(self.database_state(), before)
                self.assertEqual(list(self.output.rglob("*")), [])

    def test_default_feature_build_refuses_failpoints_on_every_route(self):
        # Sol round 3: playback-worker cleanup, rules run, artifact and
        # processing routes reached effects before any failpoint admission.
        # The CLI now refuses the fields when decoding the request, before
        # dispatch, so even an unknown surface is refused the same way.
        self.bind()
        self.apply()
        before = self.database_state()
        tree = sorted(str(p) for p in self.output.rglob("*"))
        simulate = {"CATABOLIC_TEST_SIMULATE_DEFAULT_FEATURES": "1"}
        for surface, action in (
            ("playback-worker", "cleanup"),
            ("rules", "run"),
            ("rendition-worker", "tick"),
            ("api-worker", "tick"),
            ("catalog-refresh", "run"),
            ("watcher", "run"),
            ("import", "catalog"),
            ("artifact", "build"),
            ("outputs", "apply"),
            ("no-such-surface", "anything"),
        ):
            for field in ({"failpoint": None}, {"crash": False}):
                with self.subTest(surface=surface, action=action, field=field):
                    self.native(
                        action,
                        {"catalog": "global", **field},
                        surface=surface,
                        failure="failpoint_injection_unavailable",
                        env=simulate,
                    )
                    self.assertEqual(self.database_state(), before)
                    self.assertEqual(
                        sorted(str(p) for p in self.output.rglob("*")), tree
                    )

    def test_native_crash_boundaries_recover_in_frozen_python(self):
        self.bind()
        self.apply()
        primary = self.source / "main.bin"
        backup = self.source / "backup.bin"
        expected = {
            p.name: hashlib.sha256(p.read_bytes()).hexdigest()
            for p in [primary, backup]
        }
        link = next(p for p in self.output.rglob("*") if p.is_symlink())
        offline = self.root / "offline.bin"
        for index, stage in enumerate(
            ["after_journal", "after_filesystem", "before_commit", "after_commit"]
        ):
            with self.subTest(stage=stage):
                if index % 2 == 0:
                    primary.rename(offline)
                else:
                    offline.rename(primary)
                plan = self.plan_pair()
                self.native(
                    "run",
                    {
                        "catalog": "global",
                        "apply": True,
                        "expected_plan": plan["plan_id"],
                        "failpoint": stage,
                        "crash": True,
                    },
                    code=86,
                )
                Reconciler(self.app).recover()
                Reconciler(self.app).apply()
                self.assertTrue(Reconciler(self.app).verify()["healthy"])
                self.assertEqual(self.store.rows("SELECT * FROM journal"), [])
                self.assertTrue(link.exists())
                self.assertEqual(
                    hashlib.sha256(
                        (primary if primary.exists() else offline).read_bytes()
                    ).hexdigest(),
                    expected["main.bin"],
                )
                self.assertEqual(
                    hashlib.sha256(backup.read_bytes()).hexdigest(),
                    expected["backup.bin"],
                )
                row = self.store.rows("SELECT * FROM fallback_entries")[0]
                if stage == "after_commit":
                    self.assertLess(row["published_generation"], row["generation"])
                else:
                    self.assertEqual(row["generation"], row["published_generation"])

    def test_python_interrupted_retarget_recovers_natively(self):
        projection = self.bind()
        self.apply()
        original = Reconciler._execute
        primary = self.source / "main.bin"
        offline = self.root / "offline.bin"
        for index, after_filesystem in enumerate([False, True]):
            with self.subTest(after_filesystem=after_filesystem):
                if index == 0:
                    primary.rename(offline)
                else:
                    offline.rename(primary)
                plan = projection.run("global")

                def interrupted(
                    reconciler, operation, after=after_filesystem, **kwargs
                ):
                    if not after:
                        raise RuntimeError("fixture interruption")

                    def stop(*args):
                        raise RuntimeError("fixture interruption")

                    return original(reconciler, operation, after_filesystem=stop)

                with patch.object(Reconciler, "_execute", interrupted):
                    with self.assertRaisesRegex(RuntimeError, "fixture interruption"):
                        projection.run(
                            "global", apply=True, expected_plan=plan["plan_id"]
                        )
                self.assertTrue(self.store.rows("SELECT * FROM journal"))
                self.native("recover", {"catalog": "global"}, surface="outputs")
                self.assertFalse(self.store.rows("SELECT * FROM journal"))
                self.assertTrue(Reconciler(self.app).verify()["healthy"])
                row = self.store.rows("SELECT * FROM fallback_entries")[0]
                self.assertEqual(row["generation"], row["published_generation"])

    def test_busy_probe_retains_unfinished_intent_and_owned_link(self):
        self.bind()
        self.apply()
        link = next(p for p in self.output.rglob("*") if p.is_symlink())
        target = link.readlink()
        (self.source / "main.bin").rename(self.root / "offline.bin")
        plan = self.plan_pair()
        self.native(
            "run",
            {
                "catalog": "global",
                "apply": True,
                "expected_plan": plan["plan_id"],
                "failpoint": "after_journal",
            },
            failure="injected filesystem failure",
        )
        pending = self.store.rows("SELECT * FROM journal")
        self.assertTrue(pending)
        import os

        with self.store.transaction() as db:
            db.execute(
                "UPDATE fallback_probe_slots SET owner='fixture-live',pid=?",
                (os.getpid(),),
            )
        self.native(
            "recover",
            {"catalog": "global"},
            surface="outputs",
            failure="probe_capacity",
        )
        self.assertEqual(self.store.rows("SELECT * FROM journal"), pending)
        self.assertEqual(link.readlink(), target)
        with self.store.transaction() as db:
            db.execute("UPDATE fallback_probe_slots SET owner=NULL,pid=NULL")
        self.native("recover", {"catalog": "global"}, surface="outputs")
        self.assertFalse(self.store.rows("SELECT * FROM journal"))
        self.assertNotEqual(link.readlink(), target)

    def interrupted_retarget(self):
        self.bind()
        self.apply()
        link = next(p for p in self.output.rglob("*") if p.is_symlink())
        target = link.readlink()
        (self.source / "main.bin").rename(self.root / "offline.bin")
        plan = self.plan_pair()
        self.native(
            "run",
            {
                "catalog": "global",
                "apply": True,
                "expected_plan": plan["plan_id"],
                "failpoint": "after_journal",
            },
            failure="injected filesystem failure",
        )
        pending = self.store.rows("SELECT * FROM journal")
        self.assertTrue(pending)
        return link, target, pending

    def assert_obsolete_cancel_keeps_unacknowledged_output(self, link, target, pending):
        result = self.native("recover", {"catalog": "global"}, surface="outputs")
        self.assertEqual(result["cancelled"], [pending[0]["id"]])
        self.assertEqual(result["recovered"], [])
        self.assertEqual(link.readlink(), target)
        self.assertEqual(self.store.rows("SELECT * FROM journal"), [])
        row = self.store.rows("SELECT * FROM fallback_entries")[0]
        self.assertLess(row["published_generation"], row["generation"])

    def test_stale_membership_cancels_only_proven_obsolete_intent(self):
        link, target, pending = self.interrupted_retarget()
        query = Queries(self.store).put(
            "empty-members",
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT id AS item_id FROM items WHERE 0",
                }
            },
        )["id"]
        with self.store.transaction() as db:
            db.execute("UPDATE fallback_bindings SET query_id=?", (query,))
        self.assert_obsolete_cancel_keeps_unacknowledged_output(link, target, pending)

    def test_stale_policy_cancels_without_publication_acknowledgment(self):
        link, target, pending = self.interrupted_retarget()
        old = self.store.rows("SELECT policy_id FROM fallback_bindings")[0]["policy_id"]
        definition = Policies(self.store).get(old)["definition"]
        definition["failback"]["mode"] = "manual"
        policy = Policies(self.store).put("different-policy", definition)["id"]
        with self.store.transaction() as db:
            db.execute("UPDATE fallback_bindings SET policy_id=?", (policy,))
        self.assert_obsolete_cancel_keeps_unacknowledged_output(link, target, pending)

    def test_stale_candidate_evidence_does_not_acknowledge_output(self):
        link, target, pending = self.interrupted_retarget()
        with self.store.transaction() as db:
            db.execute(
                "UPDATE item_files SET active=0 WHERE file_id=?",
                (self.files["backup.bin"],),
            )
        self.assert_obsolete_cancel_keeps_unacknowledged_output(link, target, pending)

    def test_incomplete_selection_never_authorizes_cancellation(self):
        link, target, pending = self.interrupted_retarget()
        query = Queries(self.store).put(
            "invalid-publication",
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT id AS item_id FROM items",
                }
            },
        )["id"]
        with self.store.transaction() as db:
            db.execute(
                "UPDATE saved_queries SET definition=json_set(definition,'$.selection.query','SELECT id AS item_id FROM missing_relation') WHERE id=?",
                (query,),
            )
            db.execute("UPDATE fallback_bindings SET query_id=?", (query,))
        self.native(
            "recover",
            {"catalog": "global", "cancel_unapplied": True},
            surface="outputs",
            failure="missing_relation",
        )
        self.assertEqual(self.store.rows("SELECT * FROM journal"), pending)
        self.assertEqual(link.readlink(), target)


class NativeHistoricalProjectionTest(unittest.TestCase):
    setUpClass = classmethod(evidence_fixtures.FallbackEvidenceTest.setUpClass.__func__)
    tearDownClass = classmethod(
        evidence_fixtures.FallbackEvidenceTest.tearDownClass.__func__
    )
    generate_media = evidence_fixtures.FallbackEvidenceTest.generate_media
    policy = evidence_fixtures.FallbackEvidenceTest.policy
    native = NativeProjectionTest.native
    plan_pair = NativeProjectionTest.plan_pair
    apply = NativeProjectionTest.apply
    maxDiff = None

    def setUp(self):
        evidence_fixtures.FallbackEvidenceTest.setUp(self)
        self.database = self.path

    def test_offline_original_accepted_lineage_and_container_transition(self):
        generated = self.generate_media()
        digest = hashlib.sha256(self.source_file.read_bytes()).hexdigest()
        accepted = self.policy("accepted_source_revision")
        strict = self.policy("current_source_revision")
        membership = Queries(self.store).put(
            "members",
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT id AS item_id FROM items",
                }
            },
        )["id"]
        output = self.root / "projection"
        output.mkdir()
        self.app.bind("output", "global", str(output))
        Layouts(self.app).put("flat", copy.deepcopy(PRESETS["flat"]))
        self.native(
            "bind",
            {
                "catalog": "global",
                "policy_id": accepted,
                "query_id": membership,
                "layout": "flat",
            },
        )
        self.assertTrue(self.apply(max_removals=1)["complete"])
        self.source.rename(self.root / "offline")
        plan = self.plan_pair()
        self.assertEqual(plan["desired"][0]["file_id"], generated)
        self.assertEqual(
            plan["desired"][0]["evidence"]["source_currentness"], "not_live_checked"
        )
        self.assertTrue(self.apply(max_removals=1)["complete"])
        links = [p for p in output.rglob("*") if p.is_symlink()]
        self.assertEqual(len(links), 1)
        self.assertEqual(links[0].suffix, ".mp4")
        self.assertTrue(links[0].exists())
        for policy in [accepted, strict]:
            expected = json.loads(
                json.dumps(
                    Resolver(self.app).resolve([{"item_id": self.item_id}], policy)
                )
            )
            result = self.native(
                "resolve", {"policy_id": policy, "entries": [{"item_id": self.item_id}]}
            )
            self.assertEqual(result, expected)
        self.assertEqual(result["decisions"][0]["state"], "unresolved")
        self.assertEqual(
            hashlib.sha256(
                (self.root / "offline" / self.source_file.name).read_bytes()
            ).hexdigest(),
            digest,
        )


if __name__ == "__main__":
    unittest.main()
