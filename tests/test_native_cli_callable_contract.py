# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Source-only transport and fail-closed packaged helper controls."""

import hashlib
import importlib.util
import io
import json
import struct
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "crates/catabolic-python/python/catabolic_native/cli.py"


def load():
    package = types.ModuleType("callable_fixture")
    package.__version__ = "0.2.0"
    package.CatabolicError = type("CatabolicError", (RuntimeError,), {})
    spec = importlib.util.spec_from_file_location("callable_fixture.cli", SOURCE)
    module = importlib.util.module_from_spec(spec)
    with patch.dict(sys.modules, {"callable_fixture": package}):
        spec.loader.exec_module(module)
    return module


class CallableContractTest(unittest.TestCase):
    def setUp(self):
        self.cli = load()

    def test_bounded_framing_and_eof(self):
        stream = io.BytesIO()
        self.cli._send(stream, {"kind": "write", "text": "é\n𝄞"})
        stream.seek(0)
        self.assertEqual(self.cli._receive(stream)["text"], "é\n𝄞")
        for raw in (b"", b"\0", struct.pack("!I", 0), struct.pack("!I", 1048577)):
            with self.subTest(raw=raw), self.assertRaises(RuntimeError):
                self.cli._receive(io.BytesIO(raw))
        with self.assertRaises(RuntimeError):
            self.cli._send(io.BytesIO(), {"text": "x" * 1048576})

    def test_helper_manifest_and_artifact_refusals(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            self.cli.__file__ = str(root / "cli.py")
            helper = root / "bin/catabolic-legacy-native"
            helper.parent.mkdir()
            helper.write_bytes(b"disposable artifact")
            helper.chmod(0o755)
            metadata = {
                "format": 1,
                "version": "0.2.0",
                "path": "bin/catabolic-legacy-native",
                "sha256": hashlib.sha256(helper.read_bytes()).hexdigest(),
                "source_sha256": "a" * 64,
                "build": {"scope": "test"},
            }
            legacy = root / "legacy-helper.json"
            companion = root / "native-helper.json"
            companion.write_text(
                json.dumps(metadata | {"path": "bin/catabolic-native"})
            )
            for value in (
                None,
                [],
                {},
                metadata | {"source_sha256": "b" * 64},
                metadata | {"sha256": "c" * 64},
            ):
                legacy.write_text(json.dumps(value))
                with (
                    self.subTest(value=value),
                    self.assertRaisesRegex(RuntimeError, "legacy helper"),
                ):
                    self.cli.legacy_binary()
            legacy.write_text(json.dumps(metadata))
            self.assertEqual(self.cli.legacy_binary(), helper)
            helper.chmod(0o644)
            with self.assertRaisesRegex(RuntimeError, "legacy helper"):
                self.cli.legacy_binary()
            helper.chmod(0o755)
            helper.write_bytes(b"changed")
            with self.assertRaisesRegex(RuntimeError, "legacy helper"):
                self.cli.legacy_binary()
            helper.unlink()
            with self.assertRaisesRegex(RuntimeError, "legacy helper"):
                self.cli.legacy_binary()

    def test_companion_console_and_default_binding_unchanged(self):
        original = (SOURCE.parent / "__init__.py").read_text()
        self.assertIn("os.execv(executable, [executable, *sys.argv[1:]])", original)
        self.assertIn(
            'catabolic = "catabolic.cli:main"', (ROOT / "pyproject.toml").read_text()
        )
        self.assertNotIn("catabolic.cli", SOURCE.read_text())

    def test_installed_gate_is_wired_in_ci(self):
        from tests.native_ci_contract import require_installed_gate

        require_installed_gate(
            (ROOT / ".github/workflows/native-qualification.yml").read_text(),
            "native_cli_callable_tests.py",
            "native-cli-callable",
        )

    def test_argv_is_text_before_launch(self):
        with patch.object(self.cli, "legacy_binary") as helper:
            with self.assertRaises(TypeError):
                self.cli.main([1])
            helper.assert_not_called()


if __name__ == "__main__":
    unittest.main()
