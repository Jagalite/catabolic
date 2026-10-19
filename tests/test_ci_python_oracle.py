# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""CI must reject substituted archives, runtimes and stale success receipts."""

import hashlib
import io
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from scripts import prepare_ci_python_oracle as oracle


class CiPythonOracleTest(unittest.TestCase):
    def test_configured_linker_refuses_the_system_expat_fallback(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            makefile = root / "Makefile"
            archive = root / "oracle/lib/libexpat.a"
            for content in (
                "MODULE_PYEXPAT_LDFLAGS=-lexpat\n",
                f"MODULE_PYEXPAT_LDFLAGS={archive} -lexpat\n",
                "MODULE_PYEXPAT_LDFLAGS=/ambient/libexpat.a -lm\n",
                "MODULE_PYEXPAT_STATE=missing\n",
                f"MODULE_PYEXPAT_LDFLAGS={archive} -lm\n" * 2,
            ):
                with self.subTest(content=content):
                    makefile.write_text(content)
                    with self.assertRaisesRegex(RuntimeError, "pinned Expat archive"):
                        oracle.configured_expat(makefile, archive)
            makefile.write_text(f"MODULE_PYEXPAT_LDFLAGS={archive} -lm\n")
            oracle.configured_expat(makefile, archive)

    def test_source_checksum_failure_does_not_publish_download(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "source.tar.xz"
            with patch.object(
                oracle.urllib.request, "urlopen", return_value=io.BytesIO(b"substitute")
            ):
                with self.assertRaisesRegex(RuntimeError, "checksum mismatch"):
                    oracle.download("https://example.invalid/source", "0" * 64, target)
            self.assertFalse(target.exists())

    def test_source_checksum_success_retains_exact_bytes(self):
        data = b"verified source archive"
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "source.tar.xz"
            with patch.object(
                oracle.urllib.request, "urlopen", return_value=io.BytesIO(data)
            ):
                oracle.download(
                    "https://example.invalid/source",
                    hashlib.sha256(data).hexdigest(),
                    target,
                )
            self.assertEqual(target.read_bytes(), data)

    def test_runtime_substitution_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory)
            executable = prefix / "python"
            executable.write_bytes(b"disposable executable identity")
            identity = dict(
                version=[3, 14, 6],
                expat="expat_2.8.2",
                system="Linux",
                prefix=str(prefix),
                executable=str(executable),
            )
            for key, replacement in (
                ("version", [3, 14, 5]),
                ("expat", "expat_2.8.1"),
                ("system", "Darwin"),
                ("prefix", "/ambient/python"),
                ("executable", "/ambient/bin/python"),
            ):
                with self.subTest(key=key):
                    result = subprocess.CompletedProcess(
                        [], 0, json.dumps({**identity, key: replacement}), ""
                    )
                    with patch.object(oracle.subprocess, "run", return_value=result):
                        with self.assertRaisesRegex(RuntimeError, "identity mismatch"):
                            oracle.runtime_identity(executable, prefix)
            result = subprocess.CompletedProcess([], 0, json.dumps(identity), "")
            with patch.object(oracle.subprocess, "run", return_value=result) as run:
                actual = oracle.runtime_identity(executable, prefix)
            self.assertEqual(
                actual["executable_sha256"],
                hashlib.sha256(executable.read_bytes()).hexdigest(),
            )
            self.assertEqual(run.call_args.args[0][1:3], ["-I", "-B"])

    def test_existing_prefix_is_never_adopted(self):
        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory) / "existing"
            prefix.mkdir()
            marker = prefix / "ambient"
            marker.write_bytes(b"preserve")
            with patch.object(oracle, "download") as download:
                with self.assertRaises(FileExistsError):
                    oracle.build(prefix, Path(directory) / "work")
            download.assert_not_called()
            self.assertEqual(marker.read_bytes(), b"preserve")

    def test_failed_preflight_invalidates_old_success(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = root / "receipt.json"
            receipt.write_text('{"passed": true, "stale": true}')
            with (
                patch.object(
                    sys,
                    "argv",
                    [
                        "builder",
                        "--prefix",
                        str(root / "prefix"),
                        "--work-dir",
                        str(root / "work"),
                        "--receipt",
                        str(receipt),
                    ],
                ),
                patch.object(oracle.platform, "system", return_value="Darwin"),
            ):
                with self.assertRaisesRegex(RuntimeError, "requires Linux"):
                    oracle.main()
            result = json.loads(receipt.read_text())
            self.assertFalse(result["passed"])
            self.assertNotIn("stale", result)


if __name__ == "__main__":
    unittest.main()
