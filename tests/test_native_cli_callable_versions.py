# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""The callable qualification masks only each engine's own --version text."""

import copy
import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "native_cli_callable_tests", ROOT / "scripts/native_cli_callable_tests.py"
)
GATE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GATE)


def groups(release):
    version = {"return": None, "stdout": f"catabolic {release}\n", "stderr": ""}
    return {
        "parser_system_exit": [
            {"exception": "SystemExit", "code": 0, "stdout": "help", "stderr": ""},
            {"exception": "SystemExit", "code": 0, **{"stdout": version["stdout"]}},
            {"exception": "SystemExit", "code": 2, "stdout": "", "stderr": "bad"},
        ],
        "argv_none_and_iterables": [dict(version), dict(version), dict(version)],
        "signature": ["(argv)"],
    }


class ReleaseVersionMask(unittest.TestCase):
    def test_both_engines_compare_equal_only_after_their_own_versions(self):
        reference = GATE.expected_release_version(groups("0.2.0"), "0.2.0", "reference")
        native = GATE.expected_release_version(groups("1.0.0"), "1.0.0", "native")
        self.assertEqual(reference, native)
        self.assertNotEqual(groups("0.2.0"), groups("1.0.0"))

    def test_input_is_not_mutated_and_other_groups_are_untouched(self):
        original = groups("1.0.0")
        before = copy.deepcopy(original)
        masked = GATE.expected_release_version(original, "1.0.0", "native")
        self.assertEqual(original, before)
        self.assertEqual(masked["signature"], before["signature"])
        self.assertEqual(
            masked["parser_system_exit"][0], before["parser_system_exit"][0]
        )
        self.assertEqual(
            masked["parser_system_exit"][2], before["parser_system_exit"][2]
        )

    def test_wrong_version_or_any_other_difference_still_fails(self):
        for release, engine in (("0.2.0", "native"), ("1.0.0", "reference")):
            with self.subTest(engine=engine), self.assertRaises(AssertionError):
                GATE.expected_release_version(
                    groups(release), "1.0.0" if engine == "native" else "0.2.0", engine
                )
        changed = groups("1.0.0")
        changed["argv_none_and_iterables"][2]["stderr"] = "extra"
        reference = GATE.expected_release_version(groups("0.2.0"), "0.2.0", "reference")
        native = GATE.expected_release_version(changed, "1.0.0", "native")
        self.assertNotEqual(reference, native)
        changed = groups("1.0.0")
        changed["parser_system_exit"][1]["code"] = 1
        native = GATE.expected_release_version(changed, "1.0.0", "native")
        self.assertNotEqual(reference, native)

    def test_declared_positions_are_the_version_scenarios(self):
        self.assertEqual(
            GATE.VERSION_RESULTS,
            {"parser_system_exit": (1,), "argv_none_and_iterables": (0, 1, 2)},
        )
        self.assertEqual(GATE.REFERENCE_RELEASE, "0.2.0")


if __name__ == "__main__":
    unittest.main(verbosity=2)
