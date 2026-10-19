# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Frozen Python plans executed solely by the native artifact/tool owner.

The reference fixtures create admitted package jobs. The actual run is a fresh
native CLI process against that disposable catalog, then independent Python
component/resolver/publication readers verify the resulting durable evidence.
"""

import json
import subprocess
import unittest
from unittest.mock import patch

from catabolic.artifacts import Artifacts
from tests import test_component_media as reference
from tests.probe_timing import delay_first_probe_start
from tests.test_rust_migration import BINARY


class NativeComponentMediaTest(unittest.TestCase):
    setUp = reference.MediaTest.setUp
    rows = reference.MediaTest.rows
    query = reference.MediaTest.query
    policy = reference.MediaTest.policy
    claim = reference.MediaTest.claim
    prepare = reference.MediaTest.prepare
    artifact = reference.MediaTest.artifact

    def native_run(self, artifacts, **options):
        with artifacts.store.detached():
            result = subprocess.run(
                [
                    str(BINARY),
                    "--db",
                    str(artifacts.store.path),
                    "--json",
                    "operation",
                    "artifact",
                    "run",
                    "--options",
                    json.dumps(options),
                ],
                capture_output=True,
                text=True,
                timeout=180,
            )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        value = json.loads(result.stdout)
        self.assertTrue(value["complete"], value)
        return value

    def run_reference_case(self, method):
        self.assertTrue(BINARY.is_file(), "build native CLI before qualification")
        with patch.object(
            Artifacts, "run", lambda artifacts, **kw: self.native_run(artifacts, **kw)
        ):
            method(self)

    def test_selected_streams_packet_evidence_and_reference_publication(self):
        self.run_reference_case(
            reference.MediaTest.test_mux_selected_streams_then_resolve_verified_artifact
        )

    def test_selected_streams_publication_tolerates_delayed_reference_probe(self):
        with delay_first_probe_start("a.mkv") as observed:
            self.test_selected_streams_packet_evidence_and_reference_publication()
        self.assertEqual(len(observed), 1)
        self.assertEqual(observed[0]["delayed_children"], 1)
        self.assertEqual(observed[0]["timeout_ms"], 10000)
        self.assertEqual(observed[0]["result"], {"usable": True, "reason": None})

    def test_external_audio_subtitle_and_exact_accepted_offset(self):
        self.run_reference_case(
            reference.MediaTest.test_external_audio_and_subtitle_mux_with_accepted_offset
        )

    def test_font_attachment_exact_bytes_and_lineage(self):
        self.run_reference_case(
            reference.MediaTest.test_font_dependency_is_preserved_and_hashed
        )

    def operation(self, surface, action, options, *, code=0, env=None):
        with self.store.detached():
            result = subprocess.run(
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
                capture_output=True,
                text=True,
                timeout=180,
                env=env,
            )
        self.assertEqual(result.returncode, code, result.stdout + result.stderr)
        return json.loads(result.stdout) if code == 0 else result

    def test_exact_reference_extraction_plan_and_native_publication(self):
        from catabolic.component_packaging import extraction

        self.prepare()
        row = self.rows("kind='subtitle' AND language='en' AND current=1")[0]
        recipe = self.a.recipe("extract", "subtitle-srt")
        expected = extraction(self.app, row["occurrence_id"], recipe["id"], "generated")
        request = {
            "id": row["occurrence_id"],
            "operation_id": recipe["id"],
            "location": "generated",
        }
        self.assertEqual(self.operation("component", "extract", request), expected)
        self.assertEqual(
            self.operation(
                "component",
                "get",
                {"id": row["occurrence_id"], "require_current": True},
            ),
            expected["package"]["components"][0]["occurrence"],
        )
        self.operation(
            "component",
            "extract",
            {**request, "apply": True, "expected_plan": "stale"},
            code=2,
        )
        admitted = self.operation(
            "component",
            "extract",
            {**request, "apply": True, "expected_plan": expected["plan_id"]},
        )
        artifact = self.artifact(self.native_run(self.a, job_ids=[admitted["job_id"]]))
        self.assertEqual(len(artifact["validation"]["output"]["streams"]), 1)
        self.assertEqual(
            artifact["validation"]["components"][0]["relationship"],
            "extracted_conversion",
        )
        self.assertEqual(
            len(self.rows("file_id=? AND current=1", (artifact["file_id"],))), 1
        )

    def test_actual_mux_worker_death_recovers_packet_validated_intent(self):
        import os

        from catabolic.component_packaging import enqueue
        from catabolic.fallback_resolution import Resolver

        self.prepare()
        recipe = self.a.recipe("crash-mux", "component-mux")
        policy = self.policy("selected_only", recipe["id"])
        plan = Resolver(self.app).resolve([{"item_id": self.item}], policy)
        queued = enqueue(self.app, policy, self.item, "generated", plan["plan_id"])
        self.operation(
            "artifact",
            "run",
            {"job_ids": [queued["job_id"]]},
            code=86,
            env={**os.environ, "CATABOLIC_TEST_ARTIFACT_CRASH_AT": "after_intent"},
        )
        result = self.operation("artifact", "recover", {})
        self.assertTrue(result["complete"], result)
        self.assertEqual(len(result["recovered"]), 1)
        artifact = self.a.get(result["recovered"][0])
        self.assertEqual(artifact["state"], "ready")
        self.assertEqual(len(artifact["validation"]["packet_verification"]), 3)
        self.assertEqual(
            len(
                self.store.rows(
                    "SELECT * FROM component_output_lineage WHERE artifact_id=?",
                    (artifact["id"],),
                )
            ),
            3,
        )
        self.assertEqual(self.operation("artifact", "recover", {})["recovered"], [])
