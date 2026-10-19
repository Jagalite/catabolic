# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Synthetic assertion controls only; no product or installed qualification."""

import json
import subprocess
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import Mock

import tests.test_rust_legacy_destination_mappings as fixtures


class MappingContentionDiagnosticControls(unittest.TestCase):
    def fixture(self, failing_engine=None):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        method = "test_recovery_catalog_writer_contention_preserves_event_and_lock"
        case = fixtures.NativeDestinationMappingTest(method)
        case.db = {True: root / "python.db", False: root / "native.db"}
        requests = [("GET", "/", {}, None)]
        case.receiver = types.SimpleNamespace(
            ignore=False, requests={True: list(requests), False: list(requests)}
        )
        case.apply_pair = Mock()
        case.events = Mock(return_value=[("event", {"state": "uncertain"}, "warning")])
        calls = {True: 0, False: 0}
        error = {
            "type": "ConsumerError",
            "code": "temporarily_failed",
            "message": "consumer temporarily_failed; retry delivery",
            "safe_to_retry": True,
        }
        stdout = json.dumps({"data": {"error": error}}) + "\n"
        # Exercise complete output retention, including Unicode and long stderr.
        stderr = "diagnostic 雪\n" + "x" * 5000 + "\nend of stderr\n"

        def call(engine, args):
            calls[engine] += 1
            if engine is failing_engine:
                return subprocess.CompletedProcess(args, 2, stdout, stderr)
            if calls[engine] == 1:
                data = {"error": {"type": "CatalogBusy"}}
                code = 2
            else:
                data = {"state": "not_applied"}
                code = 0
            return subprocess.CompletedProcess(
                args, code, json.dumps({"data": data}), ""
            )

        case.call = Mock(side_effect=call)
        return case, stdout, stderr, calls

    def test_consumer_error_still_fails_and_exposes_complete_engine_evidence(self):
        for engine in (True, False):
            with self.subTest(engine=engine):
                case, stdout, stderr, calls = self.fixture(engine)
                with self.assertRaises(AssertionError) as caught:
                    case.test_recovery_catalog_writer_contention_preserves_event_and_lock()
                message = str(caught.exception)
                self.assertIn("'ConsumerError' != 'CatalogBusy'", message)
                diagnostic = json.loads(
                    message.partition("recovery contention diagnostic: ")[2]
                )
                self.assertEqual(
                    diagnostic["engine"], "frozen_python" if engine else "native"
                )
                self.assertEqual(diagnostic["stdout"], stdout)
                self.assertEqual(diagnostic["stderr"], stderr)
                self.assertEqual(
                    diagnostic["receiver_requests"], [["GET", "/", {}, None]]
                )
                self.assertEqual(
                    json.loads(diagnostic["stdout"])["data"]["error"]["code"],
                    "temporarily_failed",
                )
                self.assertEqual(calls[engine], 1)
                case.apply_pair.assert_called_once_with(code=3)

    def test_catalog_busy_still_requires_post_lock_recovery_for_both_engines(self):
        case, _, _, calls = self.fixture()
        case.test_recovery_catalog_writer_contention_preserves_event_and_lock()
        self.assertEqual(calls, {True: 2, False: 2})
        case.apply_pair.assert_called_once_with(code=3)
