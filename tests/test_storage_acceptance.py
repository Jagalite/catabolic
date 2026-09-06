# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Disposable mount cleanup must tolerate busy disks without hiding failures."""

import subprocess
import unittest
from unittest.mock import patch

from scripts.storage_acceptance import detach


class StorageCleanupTest(unittest.TestCase):
    def result(self, stderr=""):
        return subprocess.CompletedProcess([], int(bool(stderr)), "", stderr)

    def test_busy_macos_disk_retries_then_detaches_without_force(self):
        with (
            patch(
                "scripts.storage_acceptance.subprocess.run",
                side_effect=[self.result("Resource busy"), self.result()],
            ) as run,
            patch("scripts.storage_acceptance.time.sleep") as sleep,
        ):
            detach("/fixture/output", "Darwin")
        self.assertEqual(run.call_count, 2)
        self.assertEqual(
            run.call_args.args[0], ["hdiutil", "detach", "/fixture/output"]
        )
        sleep.assert_called_once_with(2)

    def test_persistently_busy_disk_still_fails(self):
        with (
            patch(
                "scripts.storage_acceptance.subprocess.run",
                return_value=self.result("Resource busy"),
            ) as run,
            patch("scripts.storage_acceptance.time.sleep") as sleep,
            self.assertRaisesRegex(AssertionError, "Resource busy"),
        ):
            detach("/fixture/output", "Darwin")
        self.assertEqual(run.call_count, 5)
        self.assertEqual(sleep.call_count, 4)

    def test_other_errors_and_linux_fail_immediately(self):
        for system, error in [
            ("Darwin", "Permission denied"),
            ("Linux", "Resource busy"),
        ]:
            with (
                self.subTest(system=system),
                patch(
                    "scripts.storage_acceptance.subprocess.run",
                    return_value=self.result(error),
                ) as run,
                patch("scripts.storage_acceptance.time.sleep") as sleep,
                self.assertRaisesRegex(AssertionError, error),
            ):
                detach("/fixture/output", system)
            run.assert_called_once()
            sleep.assert_not_called()
