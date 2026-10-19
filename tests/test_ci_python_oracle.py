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
from unittest.mock import Mock, patch

from scripts import prepare_ci_python_oracle as oracle


class CiPythonOracleTest(unittest.TestCase):
    def test_native_dependency_drift_requires_reviewing_the_oracle_pin(self):
        oracle.native_sqlite_dependency(
            Path(__file__).resolve().parents[1] / "Cargo.lock"
        )
        with tempfile.TemporaryDirectory() as directory:
            lockfile = Path(directory) / "Cargo.lock"
            for version, checksum in (
                ("0.38.3", oracle.SQLITE_CRATE_CHECKSUM),
                (oracle.SQLITE_CRATE_VERSION, "0" * 64),
            ):
                lockfile.write_text(
                    '[[package]]\nname="libsqlite3-sys"\n'
                    f'version="{version}"\nchecksum="{checksum}"\n'
                )
                with self.assertRaisesRegex(RuntimeError, "dependency changed"):
                    oracle.native_sqlite_dependency(lockfile)

    def test_configured_linker_refuses_the_system_sqlite_fallback(self):
        with tempfile.TemporaryDirectory() as directory:
            makefile = Path(directory) / "Makefile"
            archive = Path(directory) / "oracle/lib/libsqlite3.a"
            accepted = f"MODULE__SQLITE3_LDFLAGS={archive} -ldl -lm -lpthread\n"
            for content in (
                "MODULE__SQLITE3_LDFLAGS=-lsqlite3\n",
                f"MODULE__SQLITE3_LDFLAGS={archive} -lsqlite3\n",
                "MODULE__SQLITE3_STATE=missing\n",
                accepted * 2,
            ):
                with self.subTest(content=content):
                    makefile.write_text(content)
                    with self.assertRaisesRegex(RuntimeError, "pinned SQLite archive"):
                        oracle.configured_sqlite(makefile, archive)
            makefile.write_text(accepted)
            oracle.configured_sqlite(makefile, archive)

    def test_sqlite_source_substitution_is_refused_before_compilation(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)
            (source / "sqlite3.c").write_bytes(b"substituted SQLite")
            run = Mock()
            with self.assertRaisesRegex(RuntimeError, "pinned bundled native source"):
                oracle.build_sqlite(source / "prefix", source, run)
            run.assert_not_called()

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
                sqlite=oracle.SQLITE_VERSION,
                sqlite_foreign_keys=0,
                sqlite_options=["ENABLE_MATH_FUNCTIONS"],
            )
            for key, replacement in (
                ("version", [3, 14, 5]),
                ("expat", "expat_2.8.1"),
                ("system", "Darwin"),
                ("prefix", "/ambient/python"),
                ("executable", "/ambient/bin/python"),
                ("sqlite", "3.45.1"),
                ("sqlite_foreign_keys", 1),
                ("sqlite_options", []),
                ("sqlite_options", ["ENABLE_MATH_FUNCTIONS", "SOUNDEX"]),
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
            compile(run.call_args.args[0][-1], "runtime probe", "exec")

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
