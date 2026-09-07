# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Retroactive rule planning, storage estimates, real rendering and safety gates."""

import contextlib
import io
import json
import os
import shutil
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from catabolic.app import Application
from catabolic.cli import main
from catabolic.domain import CatabolicError
from catabolic.item_workflow import ItemWorkflow
from catabolic.maintenance import run as maintenance
from catabolic.migration import load_migrations, upgrade_database
from catabolic.processing import Processing
from catabolic.rule_estimates import estimate, total
from catabolic.rules import Rules, render_preview
from catabolic.store import Store
from tests import test_artifacts
from tests.test_migrations import contents, create_legacy

SELECTION = {
    "language": "sql",
    "query": "SELECT item_id FROM catalog_items WHERE kind='movie'",
}


class EstimateTest(unittest.TestCase):
    def test_duration_and_bitrate_units_and_unknowns(self):
        recipe = {"preset": "h264-720p", "max_output_bytes": 20 * 1024**3}
        probe = {"summary": {"duration": "3600"}, "streams": [{"codec_type": "audio"}]}
        result = estimate(recipe, {}, probe, {"video_kbps": 2500})
        self.assertEqual(result["expected_bytes"], 1220940000)
        self.assertLess(result["low_bytes"], result["expected_bytes"])
        self.assertGreater(result["high_bytes"], result["expected_bytes"])
        unknown = estimate(recipe, {}, None, {})
        combined = total([result, unknown])
        self.assertIsNone(combined["expected_bytes"])
        self.assertEqual(combined["unknown_count"], 1)
        self.assertEqual(combined["known_expected_bytes"], result["expected_bytes"])
        self.assertEqual(
            combined["planning_bytes"],
            result["high_bytes"] + recipe["max_output_bytes"],
        )

    def test_preview_duration_is_clamped_and_audio_omission_is_accounted_for(self):
        recipe = {
            "preset": "preview",
            "max_output_bytes": 1024**3,
            "start_seconds": 50,
            "duration_seconds": 30,
            "audio_stream": None,
        }
        value = estimate(
            recipe,
            {},
            {"summary": {"duration": 60}, "streams": []},
            {"video_kbps": 1000},
        )
        self.assertEqual(value["expected_bytes"], 1275000)
        self.assertIn("0 kbps audio", value["assumptions"][0])

    def test_preservation_upgrade_from_schema_ten(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = create_legacy(root)
            upgrade_database(path, migrations=load_migrations()[:10])
            before = contents(path)
            result = upgrade_database(path)
            self.assertEqual(result["schema"], 11)
            with Store(path) as store:
                self.assertEqual(store.rows("SELECT * FROM processing_rules"), [])
                self.assertEqual(store.rows("SELECT * FROM rule_jobs"), [])
            # Existing rows, including migration history, must retain their values.
            after = contents(path)
            for table, rows in before.items():
                if table != "schema_migrations":
                    self.assertEqual(rows, after[table], table)


@unittest.skipUnless(
    shutil.which("ffmpeg") and shutil.which("ffprobe"), "real media tools required"
)
class RuleTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        test_artifacts.ArtifactTest.setUpClass.__func__(cls)

    @classmethod
    def tearDownClass(cls):
        cls.fixture.cleanup()

    def setUp(self):
        test_artifacts.ArtifactTest.setUp(self)
        self.rules = Rules(self.app)

    def probe(self):
        processor = Processing(self.app)
        processor.enqueue("probe", location="media", limit=1000)
        self.assertTrue(processor.run(limit=1000)["complete"])

    def rule(self, preset="thumbnail", **kwargs):
        recipe = self.a.recipe("test-" + preset, preset)
        return self.rules.put("test", recipe["id"], "generated", SELECTION, **kwargs)

    def test_preview_is_readonly_and_does_not_encode_or_probe(self):
        self.probe()
        rule = self.rule()
        before = self.path.read_bytes()
        with (
            Store(self.path) as store,
            patch(
                "catabolic.rendering.capabilities",
                side_effect=AssertionError("preview launched tool"),
            ),
        ):
            result = Rules(Application(store)).preview(rule["id"])
        self.assertEqual(before, self.path.read_bytes())
        self.assertEqual(result["matched_inputs"], 1)
        self.assertEqual(result["counts"]["missing"], 1)
        self.assertGreater(result["space"]["expected_bytes"], 0)
        self.assertIn("Additional output space", render_preview(result))
        self.assertEqual(
            len(list(self.generated.iterdir())), 1
        )  # ownership marker only

    def test_missing_probe_is_deferred_and_not_zero_bytes(self):
        rule = self.rule()
        result = self.rules.apply(rule["id"])
        self.assertEqual(result["counts"]["deferred"], 1)
        self.assertEqual(result["space"]["unknown_count"], 1)
        self.assertIsNone(result["space"]["expected_bytes"])
        self.assertEqual(result["queued"], [])
        self.assertFalse(result["complete"])

    def test_backfill_required_output_reuses_jobs_and_preserves_source(self):
        self.probe()
        rule = self.rule(required=True)
        workflow = ItemWorkflow(self.app)
        workflow.set_status(self.item_id, "complete", note="Reviewed")
        first = self.rules.apply(rule["id"])
        self.assertEqual(len(first["queued"]), 1)
        self.assertEqual(workflow.get(self.item_id)["status"], "needs_attention")
        repeated = self.rules.apply(rule["id"])
        self.assertEqual(repeated["queued"], [])
        self.assertEqual(repeated["counts"]["queued"], 1)
        self.assertEqual(len(self.store.rows("SELECT * FROM item_requirements")), 1)
        completed = self.rules.run(rule["id"])
        self.assertTrue(completed["complete"], completed)
        self.assertEqual(completed["after"]["space"]["expected_bytes"], 0)
        self.assertGreater(completed["after"]["space"]["already_satisfied_bytes"], 0)
        self.assertEqual(workflow.get(self.item_id)["status"], "complete")
        self.assertEqual(self.source_file.read_bytes(), self.payload)
        self.assertEqual(self.rules.run(rule["id"])["execution"]["completed"], [])

    def test_selection_totals_are_not_display_limits_and_batches_resume(self):
        for n in range(2):
            (self.source / f"another-{n}.mkv").write_bytes(self.payload)
        self.app.scan()
        for row in self.store.rows(
            "SELECT id FROM files WHERE location='media' AND id!=?", (self.file_id,)
        ):
            self.app.media.associate(row["id"], self.item_id)
        self.probe()
        rule = self.rule()
        preview = self.rules.preview(rule["id"], limit=1)
        self.assertEqual(preview["matched_inputs"], 3)
        self.assertEqual(len(preview["matches"]), 1)
        self.assertTrue(preview["details_truncated"])
        self.assertEqual(preview["space"]["estimated_count"], 3)
        for expected in (2, 1, 0):
            result = self.rules.apply(rule["id"], batch=1)
            self.assertEqual(result["remaining_to_enqueue"], expected)
        self.assertEqual(len(self.store.rows("SELECT * FROM rule_jobs")), 3)

    def test_space_and_user_budget_block_before_queue_writes(self):
        self.probe()
        rule = self.rule()
        result = self.rules.apply(rule["id"], max_new_bytes=0)
        self.assertFalse(result["safe"])
        self.assertEqual(result["queued"], [])
        with patch(
            "catabolic.rules.os.fstatvfs",
            return_value=SimpleNamespace(f_bavail=0, f_frsize=4096),
        ):
            result = self.rules.apply(rule["id"])
        self.assertFalse(result["safe"])
        self.assertEqual(self.store.rows("SELECT * FROM rule_jobs"), [])
        self.assertEqual(
            self.store.rows("SELECT * FROM processing_jobs WHERE operation='render'"),
            [],
        )

    def test_recipe_revision_preserves_old_outputs_and_requires_new_enablement(self):
        self.probe()
        first = self.rule()
        self.rules.enable(first["id"], True)
        self.rules.apply(first["id"])
        done = self.rules.run(first["id"])
        artifact = self.a.get(done["execution"]["completed"][0]["artifact_id"])
        old = self.generated / artifact["path"]
        old_bytes = old.read_bytes()
        recipe = self.a.recipe("test-thumbnail", "thumbnail", {"start_seconds": 1})
        second = self.rules.put("test", recipe["id"], "generated", SELECTION)
        self.assertEqual(second["revision"], 2)
        self.assertFalse(second["enabled"])
        self.assertFalse(self.rules.get(first["id"])["enabled"])
        self.assertEqual(self.rules.preview(second["id"])["counts"]["missing"], 1)
        self.rules.apply(second["id"])
        self.assertTrue(self.rules.run(second["id"])["complete"])
        self.assertEqual(old.read_bytes(), old_bytes)
        self.assertEqual(
            len(
                self.store.rows(
                    "SELECT * FROM processing_artifacts WHERE state='ready'"
                )
            ),
            2,
        )

    def test_missing_output_becomes_stale_without_deleting_its_record(self):
        self.probe()
        rule = self.rule()
        self.rules.apply(rule["id"])
        done = self.rules.run(rule["id"])
        artifact = self.a.get(done["execution"]["completed"][0]["artifact_id"])
        (self.generated / artifact["path"]).unlink()
        result = self.rules.preview(rule["id"])
        self.assertEqual(result["counts"]["stale"], 1)
        self.assertGreater(result["space"]["expected_bytes"], 0)
        self.assertEqual(len(self.store.rows("SELECT * FROM processing_artifacts")), 1)

    def test_generated_inputs_are_excluded_even_if_primary(self):
        self.probe()
        rule = self.rule()
        self.rules.apply(rule["id"])
        done = self.rules.run(rule["id"])
        artifact = self.a.get(done["execution"]["completed"][0]["artifact_id"])
        self.app.media.associate(artifact["file_id"], self.item_id, role="primary")
        result = self.rules.preview(rule["id"])
        self.assertEqual(result["matched_inputs"], 1)
        self.assertGreater(result["excluded_associations"], 0)

    def test_failed_jobs_need_explicit_retry(self):
        self.probe()
        rule = self.rule()
        job = self.rules.apply(rule["id"])["queued"][0]["job_id"]
        with self.store.transaction() as db:
            db.execute("UPDATE processing_jobs SET state='failed' WHERE id=?", (job,))
        result = self.rules.apply(rule["id"])
        self.assertEqual(result["counts"]["failed"], 1)
        self.assertEqual(result["queued"], [])
        self.assertFalse(result["complete"])
        self.assertEqual(
            len(self.rules.apply(rule["id"], retry_failed=True)["queued"]), 1
        )

    def test_rule_execution_ignores_unrelated_render_jobs(self):
        self.probe()
        recipe = self.a.recipe("unrelated", "preview")
        unrelated = self.a.enqueue(
            self.file_id, recipe["id"], "generated", self.item_id
        )["job_id"]
        rule = self.rule()
        self.rules.apply(rule["id"])
        self.assertTrue(self.rules.run(rule["id"])["complete"])
        self.assertEqual(
            self.store.rows(
                "SELECT state FROM processing_jobs WHERE id=?", (unrelated,)
            )[0]["state"],
            "queued",
        )

    def test_maintenance_enablement_queue_only_then_opt_in_execution(self):
        self.probe()
        rule = self.rule()
        self.assertTrue(
            maintenance(self.app, inventory_only=True, rules=True)["complete"]
        )
        self.assertEqual(self.store.rows("SELECT * FROM rule_jobs"), [])
        self.rules.enable(rule["id"], True)
        queued = maintenance(self.app, inventory_only=True, rules=True)
        self.assertFalse(queued["complete"])
        self.assertEqual(queued["summary"]["rules"]["backlog"]["queued"], 1)
        self.assertEqual(self.store.rows("SELECT * FROM processing_artifacts"), [])
        complete = maintenance(
            self.app, inventory_only=True, rules=True, render_rules=1
        )
        self.assertTrue(complete["complete"], complete)
        self.assertEqual(complete["summary"]["rules"]["backlog"]["satisfied"], 1)

    def test_maintenance_exclusions_do_not_enqueue_old_observations(self):
        self.probe()
        rule = self.rule()
        self.rules.enable(rule["id"], True)
        result = maintenance(
            self.app, inventory_only=True, rules=True, exclude=["input.mkv"]
        )
        self.assertFalse(result["complete"])
        self.assertEqual(self.store.rows("SELECT * FROM rule_jobs"), [])

    def test_profile_and_selection_fail_closed(self):
        recipe = self.a.recipe("thumb", "thumbnail")
        for selection in ([], {**SELECTION, "profile": "other"}):
            with self.assertRaises(CatabolicError):
                self.rules.put("bad", recipe["id"], "generated", selection)
        rule = self.rules.put(
            "unknown",
            recipe["id"],
            "generated",
            {"language": "sql", "query": "SELECT 'unknown' AS item_id"},
        )
        with self.assertRaises(CatabolicError):
            self.rules.apply(rule["id"])
        self.assertEqual(self.store.rows("SELECT * FROM rule_jobs"), [])

    def test_shared_device_budget_does_not_overcommit_two_rules(self):
        self.probe()
        first = self.rule()
        different = self.a.recipe("second-thumb", "thumbnail", {"start_seconds": 1})
        second = self.rules.put("z-second", different["id"], "generated", SELECTION)
        for rule in (first, second):
            self.rules.enable(rule["id"], True)
        plan = self.rules.preview(first["id"])
        available = (
            plan["space"]["planning_bytes"] + plan["space"]["reserve_bytes"] + 100
        )
        with patch(
            "catabolic.rules.os.fstatvfs",
            return_value=SimpleNamespace(f_bavail=available, f_frsize=1),
        ):
            result = maintenance(self.app, inventory_only=True, rules=True)
        self.assertFalse(result["complete"])
        self.assertEqual(
            len(
                self.store.rows(
                    "SELECT * FROM processing_jobs WHERE operation='render'"
                )
            ),
            1,
        )
        self.assertEqual(result["summary"]["rules"]["backlog"]["queued"], 1)
        self.assertEqual(result["summary"]["rules"]["backlog"]["missing"], 1)

    def test_verified_identical_output_can_restore_stale_publication_evidence(self):
        self.probe()
        rule = self.rule()
        self.rules.apply(rule["id"])
        done = self.rules.run(rule["id"])
        artifact = self.a.get(done["execution"]["completed"][0]["artifact_id"])
        path = self.generated / artifact["path"]
        st = path.stat()
        os.utime(path, ns=(st.st_atime_ns, st.st_mtime_ns + 1000000000))
        self.app.scan("generated")
        self.assertEqual(self.rules.preview(rule["id"])["counts"]["stale"], 1)
        blocked = self.rules.apply(rule["id"])
        self.assertFalse(blocked["complete"])
        self.assertIn("evidence is stale", blocked["errors"][0]["message"])
        processor = Processing(self.app)
        processor.enqueue("verify", file_ids=[artifact["file_id"]])
        self.assertTrue(processor.run()["complete"])
        self.assertEqual(self.rules.preview(rule["id"])["counts"]["satisfied"], 1)
        self.assertEqual(len(self.store.rows("SELECT * FROM processing_artifacts")), 1)

    def test_rule_does_not_run_a_queued_job_after_selection_changes(self):
        self.probe()
        rule = self.rule()
        self.rules.apply(rule["id"])
        association = self.store.rows(
            "SELECT id FROM item_files WHERE file_id=?", (self.file_id,)
        )[0]["id"]
        self.app.media.disable_association(association)
        result = self.rules.run(rule["id"])
        self.assertTrue(result["complete"])
        self.assertEqual(result["execution"]["completed"], [])
        self.assertEqual(self.store.rows("SELECT * FROM processing_artifacts"), [])

    def test_current_probe_duration_missing_stays_unknown(self):
        self.probe()
        fact = self.store.rows("SELECT * FROM file_facts WHERE operation='probe'")[0]
        data = json.loads(fact["data"])
        data["summary"].pop("duration", None)
        data["format"].pop("duration", None)
        with self.store.transaction() as db:
            db.execute(
                "UPDATE file_facts SET data=? WHERE job_id=?",
                (json.dumps(data), fact["job_id"]),
            )
        rule = self.rule("h264-720p")
        result = self.rules.preview(rule["id"])
        self.assertEqual(result["counts"]["missing"], 1)
        self.assertEqual(result["space"]["unknown_count"], 1)
        self.assertIsNone(result["space"]["expected_bytes"])

    def test_cli_json_preview_is_readonly_and_provides_space(self):
        self.probe()
        rule = self.rule("h264-720p", estimates={"video_kbps": 2000})
        self.store.close()
        before = self.path.read_bytes()
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            status = main(
                ["--db", str(self.path), "--json", "rule", "preview", rule["id"]]
            )
        self.assertEqual(status, 0, err.getvalue())
        result = json.loads(out.getvalue())
        self.assertGreater(result["space"]["expected_bytes"], 0)
        self.assertIn("2000 kbps", result["matches"][0]["estimate"]["assumptions"][0])
        self.assertEqual(before, self.path.read_bytes())
