# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""The release-candidate lane builds and verifies only; it can never publish."""

import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / ".github/workflows/native-release-candidate.yml"


class ReleaseCandidateWorkflow(unittest.TestCase):
    def setUp(self):
        # Whole-line comments explain the lane and are not behavior.
        self.text = re.sub(r"(?m)^\s*#.*\n", "", WORKFLOW.read_text(encoding="utf-8"))

    def test_manual_dispatch_only_with_read_only_token(self):
        self.assertRegex(self.text, r"(?m)^on:\n  workflow_dispatch:\n(?!  \S)")
        for trigger in ("push:", "pull_request", "schedule:", "workflow_call", "tags"):
            self.assertNotIn(trigger, self.text)
        self.assertRegex(self.text, r"(?m)^permissions:\n  contents: read\n")
        self.assertEqual(self.text.count("permissions:"), 1)
        self.assertNotIn("id-token", self.text)
        self.assertNotIn("write", self.text)

    def test_runs_on_macos_arm64_only(self):
        self.assertEqual(re.findall(r"runs-on: (\S+)", self.text), ["macos-14"])
        self.assertIn('test "$(uname -m)" = arm64', self.text)
        self.assertIn("catabolic-1.0.0-*-macosx_*_arm64.whl", self.text)

    def test_no_publishing_step(self):
        lowered = self.text.lower()
        for word in (
            "twine",
            "pypi",
            "gh release",
            "git push",
            "git tag",
            "maturin publish",
            "maturin upload",
            "trusted-publisher",
            "password",
            "secrets.",
            "environment:",
        ):
            self.assertNotIn(word, lowered)
        uses = re.findall(r"uses: (\S+)", self.text)
        self.assertEqual(
            sorted(u.split("@")[0] for u in uses),
            [
                "actions/checkout",
                "actions/setup-python",
                "actions/upload-artifact",
            ],
        )
        for use in uses:
            self.assertRegex(use, r"@[0-9a-f]{40}$")

    def test_failpoint_graph_check_precedes_the_wheel_build(self):
        graph = self.text.index("python scripts/check_failpoints_graph.py")
        build = self.text.index("scripts/build_native_wheel.py")
        verify = self.text.index("qualify_native_wheel.py")
        upload = self.text.index("actions/upload-artifact")
        self.assertLess(graph, build)
        self.assertLess(build, verify)
        self.assertLess(verify, upload)
        self.assertIn("generate_api_disposition.py --check", self.text)
        self.assertIn("qualify_native_compatibility.py", self.text)

    def test_release_workflow_is_not_changed_by_this_lane(self):
        release = (ROOT / ".github/workflows/release.yml").read_text(encoding="utf-8")
        self.assertNotIn("native-release-candidate", release)
        self.assertNotIn("build_native_wheel", release)


if __name__ == "__main__":
    unittest.main(verbosity=2)
