# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Frozen package decisions versus native Store/Runtime on disposable catalogs.

Run with the frozen Python 3.14 interpreter and frozen source/tests on sys.path.
Each resolution uses independent database copies with the same catalog identity,
while real disposable source paths provide live presence/revision checks.
"""

import json
import os
import shutil
import subprocess
import unittest
from pathlib import Path

from catabolic.app import Application
from catabolic.fallback_policies import Policies
from catabolic.fallback_resolution import Resolver
from catabolic.store import Store
from tests import test_component_packages as reference

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(
    os.environ.get("CATABOLIC_NATIVE_BINARY", ROOT / "target/debug/catabolic-native")
)


class NativeComponentSelectionTest(reference.PackageTest):
    def operation(self, surface, action, options, *, code=0):
        with self.store.detached():
            run = subprocess.run(
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
            )
        self.assertEqual(run.returncode, code, run.stdout + run.stderr)
        return json.loads(run.stdout) if code == 0 else run.stderr

    def resolve(self, policy):
        expected = Resolver(self.app).resolve([{"item_id": self.item}], policy)
        candidate = self.root / "native-component-selection.db"
        self.store.close()
        try:
            shutil.copyfile(self.database, candidate)
            run = subprocess.run(
                [
                    str(BINARY),
                    "--db",
                    str(candidate),
                    "--json",
                    "operation",
                    "fallback",
                    "resolve",
                    "--options",
                    json.dumps(
                        {"policy_id": policy, "entries": [{"item_id": self.item}]}
                    ),
                ],
                capture_output=True,
                text=True,
                timeout=90,
            )
            self.assertEqual(run.returncode, 0, run.stderr + run.stdout)
            actual = json.loads(run.stdout)
            self.assertEqual(actual, expected)
        finally:
            self.store = Store(self.database, writable=True)
            self.addCleanup(self.store.close)
            self.app = Application(self.store)
        return actual["decisions"][0]

    def test_direct_and_bounded_component_decoding_match_frozen_shape(self):
        from catabolic.components import get

        file_id = self.video("a.mkv", True)
        occurrence_id = self.rows(
            "file_id=? AND kind='subtitle' AND current=1", (file_id,)
        )[0]["occurrence_id"]
        self.assertEqual(
            self.operation(
                "component", "get", {"id": occurrence_id, "require_current": True}
            ),
            get(self.store, "default", occurrence_id, require_current=True),
        )
        row = self.rows("occurrence_id=?", (occurrence_id,))[0]
        self.claim(row, attributes={"title": "Accepted subtitle name"})
        self.assertEqual(
            self.operation("component", "get", {"id": occurrence_id}),
            get(self.store, "default", occurrence_id),
        )
        self.resolve(self.policy())
        (self.source / "a.mkv").write_bytes(b"new file revision")
        self.app.scan()
        self.assertEqual(
            self.operation("component", "get", {"id": occurrence_id}),
            get(self.store, "default", occurrence_id),
        )
        self.assertIn(
            "stale_or_conflicting_component_occurrence",
            self.operation(
                "component",
                "get",
                {"id": occurrence_id, "require_current": True},
                code=2,
            ),
        )

    def test_optional_component_and_selected_exact_container(self):
        self.video("a.mkv")
        definition = Policies(self.store).get(self.policy("selected_only"))[
            "definition"
        ]
        definition["package"]["requirements"][1]["required"] = False
        policy = Policies(self.store).put("optional", definition)["id"]
        decision = self.resolve(policy)
        self.assertEqual(decision["state"], "resolved_preferred")
        self.assertEqual(
            decision["evidence"]["component_package"]["packaging"],
            {
                "action": "use_container",
                "ready": True,
                "exposes_unselected_components": False,
            },
        )

    def test_component_candidate_budget_is_shared_across_requirements(self):
        self.video("a.mkv", True)
        definition = Policies(self.store).get(self.policy())["definition"]
        definition["budgets"]["max_candidates"] = 2
        audio = definition["package"]["requirements"][0]
        definition["package"]["requirements"] = [
            {**audio, "name": f"audio-{index}"} for index in range(3)
        ]
        policy = Policies(self.store).put("component-candidates", definition)["id"]
        decision = self.resolve(policy)
        self.assertEqual(decision["state"], "blocked")
        self.assertIn("component_candidate_budget", decision["reasons"])

    def test_changed_sidecar_snapshot_is_not_reused(self):
        policy, _ = self.subtitle_fallbacks()
        (self.source / "preferred.srt").write_bytes(b"changed revision")
        decision = self.resolve(policy)
        self.assertEqual(decision["state"], "resolved_preferred")
        self.assertEqual(
            decision["evidence"]["component_package"]["components"][-1]["occurrence"][
                "snapshot"
            ]["path"],
            "backup.srt",
        )

    def test_incomplete_component_selection_refuses_package(self):
        self.video("a.mkv", True)
        definition = Policies(self.store).get(self.policy())["definition"]
        # A file selection cannot silently stand in for exact component evidence.
        definition["package"]["requirements"][0]["fallbacks"][0]["query_id"] = (
            definition["fallbacks"][0]["query_id"]
        )
        policy = Policies(self.store).put("wrong-entity", definition)["id"]
        decision = self.resolve(policy)
        self.assertEqual(decision["state"], "blocked")
        self.assertIn(
            "component_requirement_needs_complete_component_selection",
            decision["reasons"],
        )

    def test_supported_codec_requires_explicit_conversion(self):
        self.video("a.mkv", True)
        definition = Policies(self.store).get(self.policy())["definition"]
        definition["package"]["supported_codecs"] = ["h264"]
        policy = Policies(self.store).put("codec-conversion", definition)["id"]
        decision = self.resolve(policy)
        self.assertEqual(
            decision["evidence"]["component_package"]["packaging"]["action"], "convert"
        )
        self.assertIsNone(decision["content_path"])

    def test_path_identity_still_pins_file_and_policy_revision(self):
        from catabolic.source_trust import configure

        file_id = self.video("a.mkv", True)
        policy = self.policy()
        self.assertEqual(self.resolve(policy)["state"], "resolved_preferred")
        configure(self.app, "media", "path", apply=True)
        self.assertNotEqual(self.resolve(policy)["state"], "resolved_preferred")
        streams = [
            json.loads(row["technical"]) for row in self.rows("file_id=?", (file_id,))
        ]
        streams.sort(key=lambda stream: stream["index"])
        # Weak root-device identity is explicit in policy; file versions remain mandatory.
        with self.store.transaction() as db:
            db.execute(
                "UPDATE bindings SET device=-1,inode=-1 WHERE kind='source' AND owner='media'"
            )
        self.probe(file_id, streams)
        decision = self.resolve(policy)
        self.assertEqual(decision["state"], "resolved_preferred")
        snapshot = decision["evidence"]["component_package"]["components"][0][
            "occurrence"
        ]["snapshot"]
        self.assertEqual(snapshot["source_policy"]["preset"], "path")
        self.assertEqual(snapshot["source_policy"]["revision"], 1)
        (self.source / "a.mkv").write_bytes(b"changed version under weak root policy")
        self.assertEqual(self.resolve(policy)["state"], "unresolved")

    def test_video_expansion_honors_evaluation_id_limit(self):
        file_id = self.video("a.mkv", True)
        streams = [
            json.loads(row["technical"])
            for row in self.rows("file_id=? AND current=1", (file_id,))
        ]
        streams.append({"index": 3, "codec_type": "video", "codec_name": "h264"})
        self.probe(file_id, streams)
        definition = Policies(self.store).get(self.policy())["definition"]
        definition["budgets"]["max_candidates"] = 1
        policy = Policies(self.store).put("bounded-videos", definition)["id"]
        self.assertEqual(
            self.resolve(policy)["reasons"], ["evaluation_expansion_budget"]
        )

    def test_exact_video_occurrence_selection_and_same_file_ambiguity(self):
        file_id = self.video("a.mkv", True)
        current = self.rows("file_id=? AND current=1", (file_id,))
        streams = [json.loads(row["technical"]) for row in current]
        streams.append({"index": 3, "codec_type": "video", "codec_name": "h264"})
        self.probe(file_id, streams)
        base = self.policy("selected_only")
        self.assertEqual(self.resolve(base)["reasons"], ["ambiguous_video_component"])
        definition = Policies(self.store).get(base)["definition"]
        query_id = self.query(
            "video-zero",
            "SELECT occurrence_id FROM catalog_component_occurrences WHERE current=1 AND kind='video' AND json_extract(locator,'$.index')=0",
        )
        definition["fallbacks"] = [{"name": "exact", "query_id": query_id}]
        exact = Policies(self.store).put("exact-video", definition)["id"]
        decision = self.resolve(exact)
        self.assertEqual(decision["state"], "resolved_preferred")
        self.assertEqual(
            decision["evidence"]["component_package"]["components"][0]["occurrence"][
                "locator"
            ]["index"],
            0,
        )
        definition["fallbacks"][0]["query_id"] = self.query(
            "both-videos",
            "SELECT occurrence_id FROM catalog_component_occurrences WHERE current=1 AND kind='video'",
        )
        ambiguous = Policies(self.store).put("ambiguous-video", definition)["id"]
        self.assertEqual(self.resolve(ambiguous)["reasons"], ["ambiguous_tier"])

    def test_sidecar_projection_exact_preview_publication_and_revalidation(self):
        from tests.test_component_lifecycle import LifecycleTest

        policy, files = self.subtitle_fallbacks()
        projection = LifecycleTest.bind(self, policy)

        def preview():
            before = {
                table: self.store.rows(f"SELECT * FROM {table}")
                for table in ("fallback_entries", "fallback_history", "journal")
            }
            expected = json.loads(json.dumps(projection.run("global")))
            actual = self.operation(
                "fallback", "run", {"catalog": "global", "disposable_fixture": True}
            )
            self.assertEqual(actual, expected)
            for table, rows in before.items():
                self.assertEqual(self.store.rows(f"SELECT * FROM {table}"), rows)
            return actual

        def apply():
            plan = preview()
            return self.operation(
                "fallback",
                "run",
                {
                    "catalog": "global",
                    "apply": True,
                    "expected_plan": plan["plan_id"],
                    "disposable_fixture": True,
                },
            )

        first = apply()
        self.assertTrue(first["complete"], first)
        self.assertEqual(len(first["desired"]), 2)
        self.assertIn(files[0], {row["file_id"] for row in first["desired"]})
        links = {
            str(path): (str(path.readlink()), path.lstat().st_ino)
            for path in self.output.rglob("*")
            if path.is_symlink()
        }
        second = apply()
        self.assertTrue(second["complete"], second)
        self.assertEqual(second["publication"]["applied"], [])
        self.assertEqual(
            {
                str(path): (str(path.readlink()), path.lstat().st_ino)
                for path in self.output.rglob("*")
                if path.is_symlink()
            },
            links,
        )
        (self.source / "preferred.srt").unlink()
        swapped = apply()
        self.assertTrue(swapped["complete"], swapped)
        self.assertIn(files[1], {row["file_id"] for row in swapped["desired"]})
        retained_links = {
            str(path): (str(path.readlink()), path.lstat().st_ino)
            for path in self.output.rglob("*")
            if path.is_symlink()
        }
        (self.source / "backup.srt").unlink()
        self.app.scan()
        blocked = apply()
        self.assertTrue(blocked["membership_retained"], blocked)
        self.assertEqual(
            {
                str(path): (str(path.readlink()), path.lstat().st_ino)
                for path in self.output.rglob("*")
                if path.is_symlink()
            },
            retained_links,
        )
        self.assertEqual(self.store.rows("PRAGMA foreign_key_check"), [])

    @unittest.skipUnless(
        shutil.which("ffmpeg") and shutil.which("ffprobe"), "FFmpeg required"
    )
    def test_real_selected_media_and_ready_artifact_reuse(self):
        from tests.test_component_media import MediaTest

        MediaTest.prepare(self)
        recipe = self.a.recipe("selected", "component-mux")
        policy = self.policy("selected_only", recipe["id"])
        first = self.resolve(policy)
        self.assertFalse(first["evidence"]["component_package"]["packaging"]["ready"])
        # Refresh the artifact helper after the reference store was reopened.
        from catabolic.artifacts import Artifacts

        self.a = Artifacts(self.app)
        plan = Resolver(self.app).resolve([{"item_id": self.item}], policy)
        request = {"policy_id": policy, "item_id": self.item, "location": "generated"}
        self.assertEqual(self.operation("component", "package", request), plan)
        rejected = self.operation(
            "component",
            "package",
            {**request, "apply": True, "expected_plan": "stale"},
            code=2,
        )
        self.assertIn("stale_or_missing_component_package_plan", rejected)
        self.assertEqual(
            self.store.rows("SELECT id FROM processing_jobs WHERE operation='render'"),
            [],
        )
        queued = self.operation(
            "component",
            "package",
            {**request, "apply": True, "expected_plan": plan["plan_id"]},
        )
        with self.store.detached():
            run = subprocess.run(
                [
                    str(BINARY),
                    "--db",
                    str(self.database),
                    "--json",
                    "operation",
                    "artifact",
                    "run",
                    "--options",
                    json.dumps({"job_ids": [queued["job_id"]]}),
                ],
                capture_output=True,
                text=True,
                timeout=180,
            )
        self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
        artifact = MediaTest.artifact(self, json.loads(run.stdout))
        decision = self.resolve(policy)
        self.assertEqual(decision["file_id"], artifact["file_id"])
        self.assertTrue(decision["evidence"]["component_package"]["packaging"]["ready"])
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
        # A changed output cannot continue to be advertised as the exact package.
        with (self.generated / artifact["path"]).open("ab") as stream:
            stream.write(b"changed-output")
        stale = self.resolve(policy)
        self.assertFalse(stale["evidence"]["component_package"]["packaging"]["ready"])

    @unittest.skipUnless(
        shutil.which("ffmpeg") and shutil.which("ffprobe"), "FFmpeg required"
    )
    def test_admitted_package_revalidates_changed_component_evidence(self):
        import hashlib

        from catabolic.component_packaging import validate
        from tests.test_component_media import MediaTest

        MediaTest.prepare(self)
        recipe = self.a.recipe("stale-package", "component-mux")
        policy = self.policy("selected_only", recipe["id"])
        request = {"policy_id": policy, "item_id": self.item, "location": "generated"}
        preview = self.operation("component", "package", request)
        package = preview["decisions"][0]["evidence"]["component_package"]
        admitted = self.operation(
            "component",
            "package",
            {**request, "apply": True, "expected_plan": preview["plan_id"]},
        )
        row = self.rows("kind='subtitle' AND language='en' AND current=1")[0]
        self.claim(row, attributes={"title": "Changed accepted translation"})
        with self.assertRaisesRegex(
            Exception, "component evidence changed since packaging admission"
        ):
            validate(self.store, "default", package, recipe)
        result = self.operation("artifact", "run", {"job_ids": [admitted["job_id"]]})
        self.assertFalse(result["complete"])
        self.assertEqual(result["completed"], [])
        self.assertIn(
            "component evidence changed since packaging admission",
            result["errors"][0]["error"],
        )
        self.assertEqual(
            self.store.rows("SELECT id FROM processing_artifacts WHERE state='ready'"),
            [],
        )
        self.assertEqual(
            hashlib.sha256((self.source / "a.mkv").read_bytes()).hexdigest(),
            self.original,
        )


if __name__ == "__main__":
    unittest.main()
