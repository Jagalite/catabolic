#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Fixture/failure-context checks only; these do not qualify watch cancellation."""

import importlib.util
import json
import signal
import sys
import unittest
from pathlib import Path
from unittest.mock import MagicMock, Mock, call, patch

spec = importlib.util.spec_from_file_location(
    "native_catalog_refresh_embedding_tests",
    Path(__file__).with_name("native_catalog_refresh_embedding_tests.py"),
)
qualification = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qualification)


READY = {
    "processed": 1,
    "refreshed": [],
    "errors": [],
    "pending": 1,
    "complete": False,
}


class IdleSignalDiagnostics(unittest.TestCase):
    def fixture(self, engines, failing_engine):
        case = qualification.CatalogRefreshCLI(
            "test_paired_watch_idle_silent_and_error_continuation"
        )
        # No Store or child is opened. All effects remain disposable mock state.
        case.root = Path("/unused-disposable-fixture")
        case.paths = {engine: case.root / engine for engine in engines}
        case.command = Mock(side_effect=lambda engine, arguments: [engine, *arguments])
        case.environment = Mock(return_value={})
        store_type = MagicMock()
        store = store_type.return_value.__enter__.return_value
        store.rows.side_effect = [
            [{"profile": "default", "catalog": "global", "generation": 1}],
            [],
        ] * len(engines)
        processes, selectors = [], []
        for engine in engines:
            process = Mock()
            process.pid = 100 + len(processes)
            process.returncode = -2 if engine == failing_engine else 130
            process.poll.return_value = process.returncode
            process.stdout.readline.return_value = json.dumps(READY) + "\n"
            process.communicate.return_value = ("", f"{engine} captured stderr")
            processes.append(process)
            selector = Mock()
            selector.select.side_effect = [[object()], []]
            selectors.append(selector)
        self.addCleanup(patch.stopall)
        patch.dict(sys.modules, {"catabolic_native": Mock(Store=store_type)}).start()
        patch.object(qualification.subprocess, "Popen", side_effect=processes).start()
        patch.object(
            qualification.selectors, "DefaultSelector", side_effect=selectors
        ).start()
        patch.object(
            qualification.time, "monotonic", side_effect=[10, 11.2, 12, 13.2]
        ).start()
        return case, processes, selectors, store

    def check_failure(self, failing_engine):
        engines = (
            ["reference"] if failing_engine == "reference" else ["reference", "native"]
        )
        case, processes, selectors, store = self.fixture(engines, failing_engine)
        with self.assertRaises(AssertionError) as failure:
            case.test_paired_watch_idle_silent_and_error_continuation()
        message = str(failure.exception)
        for expected in (
            "-2 != 130",
            f"'engine': '{failing_engine}'",
            "'phase': 'idle'",
            f"'command': ['{failing_engine}', 'watch', '--interval', '1']",
            "'signal_elapsed_seconds':",
            "'readiness': {'tick': {'processed': 1,",
            "'remaining_queue': []",
            "'stdout': ''",
            f"'stderr': '{failing_engine} captured stderr'",
        ):
            self.assertIn(expected, message)
        for process, selector in zip(processes, selectors, strict=True):
            self.assertEqual(selector.select.call_args_list, [call(10), call(1.2)])
            process.send_signal.assert_called_once_with(signal.SIGINT)
            process.communicate.assert_called_once_with(timeout=10)
            selector.close.assert_called_once_with()
            process.kill.assert_not_called()
        deletes = [
            invocation.args[0]
            for invocation in store.transaction.return_value.__enter__.return_value.execute.call_args_list
            if invocation.args[0].startswith("DELETE")
        ]
        self.assertEqual(
            deletes,
            [
                "DELETE FROM catalog_refresh_queue WHERE profile='default' AND catalog='global' AND generation=1"
            ]
            * len(engines),
        )

    def test_reference_failure_preserves_evidence_and_rejects_signal_exit(self):
        self.check_failure("reference")

    def test_native_failure_preserves_evidence_and_rejects_signal_exit(self):
        self.check_failure("native")

    def test_unwitnessed_process_is_not_signaled_as_steady_state(self):
        case, processes, selectors, _ = self.fixture(["reference"], "reference")
        selectors[0].select.side_effect = [[]]
        processes[0].poll.return_value = None
        with self.assertRaisesRegex(AssertionError, "bounded readiness tick"):
            case.test_paired_watch_idle_silent_and_error_continuation()
        selectors[0].select.assert_called_once_with(10)
        processes[0].send_signal.assert_not_called()
        processes[0].kill.assert_called_once_with()
        processes[0].communicate.assert_called_once_with(timeout=10)

    def test_unprocessed_tick_is_not_accepted_as_readiness(self):
        case, processes, selectors, _ = self.fixture(["reference"], "reference")
        processes[0].stdout.readline.return_value = json.dumps(dict(READY, processed=0))
        with self.assertRaisesRegex(AssertionError, "unexpected readiness tick"):
            case.test_paired_watch_idle_silent_and_error_continuation()
        selectors[0].select.assert_called_once_with(10)
        processes[0].send_signal.assert_not_called()

    def test_output_after_readiness_does_not_pass_idle_silence(self):
        case, processes, selectors, _ = self.fixture(["reference"], "reference")
        selectors[0].select.side_effect = [[object()], [object()]]
        with self.assertRaisesRegex(AssertionError, "empty watch produced idle output"):
            case.test_paired_watch_idle_silent_and_error_continuation()
        self.assertEqual(selectors[0].select.call_args_list, [call(10), call(1.2)])
        processes[0].send_signal.assert_not_called()


if __name__ == "__main__":
    unittest.main(verbosity=2)
