# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Execute the model section of the qualification gate with a recording adapter.

This checks gate wiring and fail-fast behavior, not Rust models or fault replay.
"""

import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODELS = {
    "",
    "playback",
    "coordination",
    "rendition",
    "shared-job",
    "resolution",
    "execution",
    "scan-continuation",
    "scan-publication",
    "scan-publication-race",
    "hardlink-recovery",
    "hardlink-cancellation",
    "publication-recovery",
    "notification-lease",
    "notification",
    "processing-acceptance",
    "transfer-completion",
    "content-ticket",
    "watcher-reaction",
    "watcher-generation",
    "watcher-schedule",
    "publication-progress",
    "playback-admission",
    "playback-retirement",
}


def command(operation, model):
    return operation + ("-" + model if model else "")


class StatelessnessGateTest(unittest.TestCase):
    def run_models(self, failure=""):
        text = (ROOT / "scripts/check-native-mutations.sh").read_text()
        start = text.index('"$target_dir"/debug/catabolic-verification check\n')
        end = text.index("cargo build --locked --release", start)
        section = text[start:end]
        with tempfile.TemporaryDirectory(prefix="statelessness gate ") as directory:
            root = Path(directory)
            binary = root / "target/debug/catabolic-verification"
            binary.parent.mkdir(parents=True)
            binary.write_text(
                "#!/bin/sh\nset -eu\n"
                'printf "%s\\t%s\\n" "$1" "${2-}" >> "$CALLS"\n'
                '[ "$1" != "$FAILURE" ] || exit 19\n'
                'case "$1" in\n'
                ' find*) mkdir "$2"; printf "%s" "$1" > "$2/minimized.sttrace" ;;\n'
                ' replay*) test "$(cat "$2")" = "find${1#replay}" ;;\n'
                "esac\n"
            )
            binary.chmod(0o700)
            evidence = root / "evidence"
            (evidence / "artifacts").mkdir(parents=True)
            log = root / "calls.tsv"
            env = os.environ | {
                "target_dir": str(root / "target"),
                "evidence": str(evidence),
                "CALLS": str(log),
                "FAILURE": failure,
            }
            result = subprocess.run(
                ["sh", "-c", "set -eu\n" + section],
                env=env,
                capture_output=True,
                text=True,
                timeout=30,
            )
            calls = [line.split("\t") for line in log.read_text().splitlines()]
            return result, calls

    def test_all_models_check_discover_and_replay_matching_trace(self):
        result, calls = self.run_models()
        self.assertEqual(result.returncode, 0, result.stderr)
        expected = {
            command(op, model) for op in ("check", "find", "replay") for model in MODELS
        }
        self.assertEqual({args[0] for args in calls}, expected)
        self.assertEqual(len(calls), len(expected))
        positions = {args[0]: index for index, args in enumerate(calls)}
        for model in MODELS:
            self.assertLess(
                positions[command("check", model)], positions[command("find", model)]
            )
            self.assertEqual(
                positions[command("find", model)] + 1,
                positions[command("replay", model)],
            )
        source = (ROOT / "crates/catabolic-verification/src/main.rs").read_text()
        supported = set(re.findall(r'\["((?:check|find|replay)(?:-[a-z-]+)?)"', source))
        self.assertEqual(supported, expected, "new models must join the mandatory gate")

    def test_each_model_failure_stops_the_gate(self):
        # Exercise every command, not just a representative first model: a
        # swallowed failure later in the gate must also invalidate qualification.
        for model in sorted(MODELS):
            for operation in ("check", "find", "replay"):
                failure = command(operation, model)
                with self.subTest(command=failure):
                    result, calls = self.run_models(failure)
                    self.assertEqual(result.returncode, 19, result.stderr)
                    self.assertEqual(calls[-1][0], failure)


if __name__ == "__main__":
    unittest.main()
