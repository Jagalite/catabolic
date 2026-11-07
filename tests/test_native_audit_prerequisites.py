# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Disposable diagnostic controls; these do not qualify production behavior."""

import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch

from scripts import check_native_audit_prerequisites as preflight


class AuditPrerequisiteTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def test_historical_python_checks_exact_bytes_without_execution(self):
        executable = self.root / "not-an-executable"
        executable.write_bytes(b"fixture executable bytes")
        fixture = self.root / "tests/parity/mapping-subject-json-diagnostics-v1.json"
        fixture.parent.mkdir(parents=True)
        fixture.write_text(
            json.dumps(
                {
                    "oracle": {
                        "python_sha256": hashlib.sha256(
                            executable.read_bytes()
                        ).hexdigest()
                    }
                }
            )
        )
        with patch.object(preflight.subprocess, "run") as invocation:
            self.assertEqual(
                preflight.historical_python(self.root, executable)["path"],
                str(executable),
            )
            executable.write_bytes(b"same-version but different executable")
            with self.assertRaisesRegex(ValueError, "executable mismatch"):
                preflight.historical_python(self.root, executable)
            executable.unlink()
            with self.assertRaises(FileNotFoundError):
                preflight.historical_python(self.root, executable)
        invocation.assert_not_called()

    def volume(self, payload):
        with (
            patch.object(preflight.sys, "platform", "linux"),
            patch.object(preflight.os, "readlink", return_value=str(self.root)),
            patch.object(
                preflight.subprocess,
                "run",
                return_value=types.SimpleNamespace(stdout=payload),
            ) as invocation,
        ):
            result = preflight.linux_volume_uuid(self.root)
        args, kwargs = invocation.call_args
        self.assertEqual(
            args[0],
            ["findmnt", "--json", "--target", str(self.root), "--output", "UUID"],
        )
        self.assertEqual(kwargs["timeout"], 5)
        self.assertTrue(kwargs["check"])
        return result

    def test_volume_requires_one_nonempty_identity(self):
        self.assertEqual(
            self.volume(b'{"filesystems":[{"uuid":"AB-CD"}]}')["volume_uuid"], "ab-cd"
        )
        for payload in (
            b"not json",
            b"[]",
            b"null",
            b'"text"',
            b"{}",
            b'{"filesystems":[]}',
            b'{"filesystems":[{"uuid":"a"},{"uuid":"b"}]}',
            b'{"filesystems":[{"uuid":null}]}',
            b'{"filesystems":[{"uuid":""}]}',
            b'{"filesystems":[{"uuid":4}]}',
            b'{"filesystems":[4]}',
            b" " * 65537,
        ):
            with self.subTest(payload=payload[:80]), self.assertRaises(ValueError):
                self.volume(payload)

    def test_native_root_does_not_use_python_temp_fallbacks(self):
        with patch.dict(
            os.environ, {"TEMP": str(self.root), "TMP": str(self.root)}, clear=True
        ):
            self.assertEqual(preflight.native_temp_root(), Path("/tmp"))
            missing = self.root / "missing-root"
            os.environ["TMPDIR"] = str(missing)
            self.assertEqual(preflight.native_temp_root(), missing)
            with patch.object(preflight.sys, "platform", "linux"):
                with self.assertRaises(FileNotFoundError):
                    preflight.linux_volume_uuid(preflight.native_temp_root())

    def test_directory_descriptor_closes_on_timeout(self):
        with (
            patch.object(preflight.sys, "platform", "linux"),
            patch.object(preflight.os, "open", return_value=321),
            patch.object(preflight.os, "readlink", return_value=str(self.root)),
            patch.object(preflight.os, "close") as close,
            patch.object(
                preflight.subprocess,
                "run",
                side_effect=subprocess.TimeoutExpired("findmnt", 5),
            ),
        ):
            with self.assertRaises(subprocess.TimeoutExpired):
                preflight.linux_volume_uuid(self.root)
        close.assert_called_once_with(321)

    def test_other_platform_is_not_reported_as_verified(self):
        with patch.object(preflight.sys, "platform", "darwin"):
            with self.assertRaisesRegex(ValueError, "only supports Linux"):
                preflight.linux_volume_uuid(self.root)

    def test_capture_check_requires_existing_complete_verifier(self):
        contract = object()
        module = types.SimpleNamespace()
        with (
            patch.object(
                preflight.importlib.util, "spec_from_file_location"
            ) as make_spec,
            patch.object(
                preflight.importlib.util, "module_from_spec", return_value=module
            ),
        ):
            module.load_contract = unittest.mock.Mock(return_value=contract)
            module.verify_raw_receipts = unittest.mock.Mock(return_value=4974)
            self.assertEqual(
                preflight.original_captures(self.root), {"verified_receipts": 4974}
            )
            module.verify_raw_receipts.assert_called_once_with(contract)
            make_spec.return_value.loader.exec_module.assert_called_once_with(module)
            self.assertEqual(
                make_spec.call_args.args[1],
                self.root / "tests/test_rust_graphql_lexical_capture.py",
            )
            module.verify_raw_receipts.return_value = 4973
            with self.assertRaisesRegex(ValueError, "expected 4974"):
                preflight.original_captures(self.root)
            module.verify_raw_receipts.side_effect = ValueError(
                "raw receipt hash changed"
            )
            with self.assertRaisesRegex(ValueError, "hash changed"):
                preflight.original_captures(self.root)

    def test_all_blockers_are_reported_without_qualification_claim(self):
        with (
            patch.object(
                preflight, "linux_volume_uuid", side_effect=ValueError("no UUID")
            ),
            patch.object(
                preflight,
                "original_captures",
                side_effect=FileNotFoundError("missing original"),
            ),
            patch.object(
                preflight, "historical_python", side_effect=ValueError("wrong hash")
            ),
        ):
            report = preflight.inspect(self.root, self.root / "python", self.root)
        self.assertFalse(report["known_prerequisites_available"])
        self.assertEqual(report["qualification_status"], "not_run")
        self.assertEqual(len(report["checks"]), 3)
        self.assertTrue(
            all(
                row["status"] == "blocked" and row["required_action"]
                for row in report["checks"]
            )
        )

    def test_available_prerequisites_do_not_mean_tests_passed(self):
        with (
            patch.object(
                preflight, "linux_volume_uuid", return_value={"volume_uuid": "fixture"}
            ),
            patch.object(
                preflight, "original_captures", return_value={"verified_receipts": 4974}
            ),
            patch.object(
                preflight, "historical_python", return_value={"sha256": "fixture"}
            ),
        ):
            report = preflight.inspect(self.root, self.root / "python", self.root)
        self.assertTrue(report["known_prerequisites_available"])
        self.assertEqual(report["qualification_status"], "not_run")
        self.assertTrue(report["limits"])

    def test_linux_gate_refuses_before_any_build_or_scenario(self):
        scripts = self.root / "scripts"
        scripts.mkdir()
        shutil.copyfile(
            preflight.ROOT / "scripts/check-native-mutations.sh",
            scripts / "check-native-mutations.sh",
        )
        reference = self.root / "reference/src/catabolic"
        reference.mkdir(parents=True)
        fixture = (
            self.root
            / ".local-tests/rust-migration/m0-completion/legacy-fixtures/schema-32/before.sqlite3"
        )
        fixture.parent.mkdir(parents=True)
        fixture.touch()
        tools = self.root / "tools"
        tools.mkdir()
        log = self.root / "calls"
        for name in ("ffmpeg", "ffprobe", "pdftotext", "tesseract"):
            (tools / name).write_text("#!/bin/sh\nexit 0\n")
        (tools / "uname").write_text("#!/bin/sh\nprintf 'Linux\\n'\n")
        (tools / "cargo").write_text(
            f"#!/bin/sh\necho forbidden-cargo >> '{log}'\nexit 99\n"
        )
        python = tools / "fixture-python"
        python.write_text(f"#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{log}'\nexit 37\n")
        for tool in tools.iterdir():
            tool.chmod(0o700)
        result = subprocess.run(
            ["sh", str(scripts / "check-native-mutations.sh")],
            env={
                **os.environ,
                "PATH": str(tools) + os.pathsep + os.environ["PATH"],
                "CATABOLIC_QUALIFICATION_PYTHON": str(python),
                "CATABOLIC_PYTHON_REFERENCE": str(reference.parent),
            },
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertEqual(result.returncode, 37, result.stderr)
        self.assertEqual(
            log.read_text().splitlines(),
            ["scripts/check_native_audit_prerequisites.py"],
        )
