# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Legacy metadata transport against isolated frozen and fake-loopback states."""

import contextlib
import copy
import io
import json
import re
import shutil
import signal
import subprocess
import sys
import time
import unittest
from pathlib import Path
from xml.etree import ElementTree as ET

from tests.test_rust_legacy_parser import REFERENCE

sys.path.insert(0, str(REFERENCE / "src"))
from catabolic import cli
from tests.test_rust_legacy_cli import BINARY
from tests.test_rust_plex_metadata import NativeFixture, frozen


class NativeLegacyPlexTest(NativeFixture, unittest.TestCase):
    curate = frozen.PlexMetadataTest.curate
    writes = frozen.PlexMetadataTest.writes

    def setUp(self):
        self.assertEqual(
            Path(cli.__file__).resolve(), REFERENCE / "src/catabolic/cli.py"
        )
        frozen.PlexMetadataTest.setUp(self)
        self.start_remote()

    def arguments(self, *, apply=False, expected_plan=None, machine=True):
        args = (["--machine"] if machine else []) + [
            "consumer",
            "metadata",
            "home",
            "--library-id",
            "7",
            "--item",
            self.item_id,
            "--field",
            "title",
        ]
        if apply:
            args.append("--apply")
        if expected_plan:
            args += ["--expected-plan", expected_plan]
        return args

    def oracle_cli(self, args):
        original = self.database
        reference = self.root / "legacy-reference.db"
        shutil.copyfile(original, reference)
        fields = (
            "nodes",
            "node",
            "calls",
            "detail_reads",
            "put_error",
            "put_ignored",
            "after_put",
            "on_read",
            "server",
            "library_uuid",
            "library_type",
        )
        saved = {name: getattr(self, name) for name in fields}
        self.database = reference
        self.nodes = copy.deepcopy(self.nodes)
        self.node = self.nodes[0]
        self.calls = []
        out, err = io.StringIO(), io.StringIO()
        try:
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                code = cli.main(["--db", str(reference)] + args)
            return (
                code,
                out.getvalue(),
                err.getvalue(),
                [ET.tostring(n) for n in self.nodes],
                self.contents(reference),
            )
        finally:
            self.database = original
            for name, value in saved.items():
                setattr(self, name, value)

    def paired(self, scenario, args, expected_code):
        expected = self.oracle_cli(args)
        actual = subprocess.run(
            [str(BINARY), "--db", str(self.database)] + args,
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(
            actual.returncode, expected_code, (scenario, actual.stdout, actual.stderr)
        )
        self.assertEqual(actual.returncode, expected[0], scenario)
        self.assertEqual(actual.stderr, expected[2], scenario)

        # Random event UUIDs are the only output normalization. Keep object order.
        def normalize(s):
            return re.sub(r'("event_id":\s*")[^"]+("|$)', r"\1<EVENT>\2", s)

        self.assertEqual(normalize(actual.stdout), normalize(expected[1]), scenario)
        self.assertEqual([ET.tostring(n) for n in self.nodes], expected[3], scenario)
        self.assertEqual(self.contents(self.database), expected[4], scenario)
        self.assertNotIn("FAKE_METADATA_ONLY", actual.stdout + actual.stderr)
        return json.loads(actual.stdout)

    def test_metadata_preview_error_incomplete_and_success(self):
        preview = self.paired("LC-PLEX-META-000", self.arguments(), 0)["data"]
        self.paired("LC-PLEX-META-002", self.arguments(apply=True), 2)
        self.put_ignored = True
        self.paired(
            "LC-PLEX-META-003",
            self.arguments(apply=True, expected_plan=preview["plan_id"]),
            3,
        )
        self.put_ignored = False
        self.paired(
            "LC-PLEX-META-004",
            self.arguments(apply=True, expected_plan=preview["plan_id"]),
            0,
        )

    def test_metadata_interrupt_before_remote_effect(self):
        self.block_route = "/library/metadata/1"
        process = subprocess.Popen(
            [str(BINARY), "--db", str(self.database)] + self.arguments(),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(lambda: self.reap(process))
        self.assertTrue(self.entered.wait(10), "LC-PLEX-SIG-ENTER")
        process.send_signal(signal.SIGINT)
        time.sleep(0.1)
        count = len(self.native_calls)
        self.release.set()
        out, err = process.communicate(timeout=20)
        self.assertEqual(process.returncode, 130, "LC-PLEX-SIG-130")
        self.assertEqual(err, "")
        envelope = json.loads(out)
        self.assertEqual(envelope["exit_code"], 130)
        self.assertEqual(envelope["data"]["error"]["type"], "KeyboardInterrupt")
        self.assertEqual(len(self.native_calls), count)
        self.assertEqual(self.writes(), [])
