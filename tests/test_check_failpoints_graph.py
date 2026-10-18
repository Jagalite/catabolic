# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""The shipped-graph failpoints check fails closed on every malformed input."""

import contextlib
import importlib.util
import io
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "check_failpoints_graph", ROOT / "scripts/check_failpoints_graph.py"
)
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)


def line(name, features="", path="/w/crates/x", repeat=False):
    return f"{features}@@{name} v0.2.0 ({path}){' (*)' if repeat else ''}"


class ParseTest(unittest.TestCase):
    def test_features_are_read_independently_of_path_punctuation(self):
        output = "\n".join(
            [
                line("catabolic-cli"),
                line(
                    "catabolic-store",
                    "failpoints",
                    path="/tmp/Catabolic (review), copy)/crates/catabolic-store",
                ),
            ]
        )
        self.assertEqual(CHECK.parse("catabolic-cli", output), (True, {"failpoints"}))

    def test_repeat_marker_and_multiple_features(self):
        output = "\n".join(
            [
                line("catabolic-python"),
                line("catabolic-store", "default,failpoints", repeat=True),
            ]
        )
        self.assertEqual(
            CHECK.parse("catabolic-python", output),
            (True, {"default", "failpoints"}),
        )

    def test_clean_graph(self):
        output = "\n".join([line("catabolic-cli"), line("catabolic-store")])
        self.assertEqual(CHECK.parse("catabolic-cli", output), (True, set()))

    def test_empty_output_fails(self):
        for output in ("", "\n  \n"):
            with self.subTest(output=output):
                with self.assertRaisesRegex(CHECK.GraphError, "no dependency graph"):
                    CHECK.parse("catabolic-cli", output)

    def test_wrong_root_fails(self):
        with self.assertRaisesRegex(CHECK.GraphError, "rooted at"):
            CHECK.parse("catabolic-cli", line("catabolic-core"))

    def test_malformed_line_fails(self):
        for bad in (
            "catabolic-store v0.2.0 (/w) failpoints",
            "failpoints@@",
            "@@catabolic-store",
        ):
            with self.subTest(bad=bad):
                with self.assertRaisesRegex(CHECK.GraphError, "malformed"):
                    CHECK.parse("catabolic-cli", line("catabolic-cli") + "\n" + bad)


class MaturinFeaturesTest(unittest.TestCase):
    def root(self, maturin, root_tool=""):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        (root / "crates/catabolic-python").mkdir(parents=True)
        (root / "pyproject.toml").write_text(f"[project]\nname='x'\n{root_tool}")
        (root / "crates/catabolic-python/pyproject.toml").write_text(
            "[tool.maturin]\n" + maturin
        )
        return root

    def test_features_and_flags_are_reproduced(self):
        root = self.root(
            'features = ["pyo3/extension-module", "catabolic-store/failpoints"]\n'
            "no-default-features = true\n"
        )
        self.assertEqual(
            CHECK.maturin_feature_args(root),
            [
                "--no-default-features",
                "--features",
                "pyo3/extension-module,catabolic-store/failpoints",
            ],
        )
        self.assertEqual(
            CHECK.maturin_feature_args(self.root("all-features = true\n")),
            ["--all-features"],
        )
        self.assertEqual(CHECK.maturin_feature_args(self.root("")), [])

    def test_unreproducible_settings_fail(self):
        for maturin in (
            'cargo-extra-args = "--features catabolic-store/failpoints"\n',
            'config = ["x=1"]\n',
            'features = "catabolic-store/failpoints"\n',
            'all-features = "yes"\n',
        ):
            with self.subTest(maturin=maturin):
                with self.assertRaises(CHECK.GraphError):
                    CHECK.maturin_feature_args(self.root(maturin))

    def test_root_maturin_configuration_fails(self):
        with self.assertRaisesRegex(CHECK.GraphError, "only crates/catabolic-python"):
            CHECK.maturin_feature_args(self.root("", "[tool.maturin]\nfeatures = []\n"))


class CheckTest(unittest.TestCase):
    def run_check(self, graphs):
        def resolve(package):
            value = graphs[package]
            if isinstance(value, Exception):
                raise value
            return value

        with contextlib.redirect_stdout(io.StringIO()):
            return CHECK.check(resolve)

    def test_clean_workspace_passes(self):
        self.assertEqual(
            self.run_check(
                {
                    "catabolic-cli": (True, set()),
                    "catabolic-python": (True, {"default"}),
                    "catabolic-dist": (False, set()),
                }
            ),
            [],
        )

    def test_store_required_for_cli_and_python(self):
        failures = self.run_check(
            {
                "catabolic-cli": (False, set()),
                "catabolic-python": (False, set()),
                "catabolic-dist": (False, set()),
            }
        )
        self.assertEqual(len(failures), 2)
        self.assertTrue(all("missing" in failure for failure in failures))

    def test_enabled_feature_fails_for_any_shipped_crate(self):
        for package in CHECK.SHIPPED:
            with self.subTest(package=package):
                graphs = {
                    "catabolic-cli": (True, set()),
                    "catabolic-python": (True, set()),
                    "catabolic-dist": (False, set()),
                }
                graphs[package] = (True, {"failpoints"})
                failures = self.run_check(graphs)
                self.assertEqual(
                    failures, [f"{package}: enables catabolic-store/failpoints"]
                )

    def test_resolution_error_fails(self):
        failures = self.run_check(
            {
                "catabolic-cli": CHECK.GraphError(
                    "catabolic-cli: cargo tree failed (101): x"
                ),
                "catabolic-python": (True, set()),
                "catabolic-dist": (False, set()),
            }
        )
        self.assertEqual(failures, ["catabolic-cli: cargo tree failed (101): x"])


if __name__ == "__main__":
    unittest.main()
