# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Generator oracle admission only; no XML behavior or codec qualification."""

import contextlib
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import catabolic.consumer_adapters as owner
from scripts import generate_plex_xml_encodings as generator


class PlexGeneratorReferenceTest(unittest.TestCase):
    def run_fixture(self, *, live=False, enclosing=False, revision=None, drift=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "worktree"
            reference = root if live else root / ".local-tests/reference"
            source = reference / "src/catabolic/consumer_adapters.py"
            source.parent.mkdir(parents=True)
            source.write_bytes(b"disposable frozen consumer source")
            calls = []

            def git(command, **kwargs):
                self.assertEqual(command[:3], ["git", "-C", str(reference)])
                calls.append(command[3:])
                if command[3:] == ["rev-parse", "--show-toplevel"]:
                    return str(root if enclosing else reference) + "\n"
                if command[3:] == ["rev-parse", "HEAD"]:
                    return (revision or generator.REVISION) + "\n"
                self.assertEqual(
                    command[3:],
                    [
                        "show",
                        f"{generator.REVISION}:src/catabolic/consumer_adapters.py",
                    ],
                )
                return b"different frozen bytes" if drift else source.read_bytes()

            with contextlib.ExitStack() as stack:
                stack.enter_context(patch.object(generator, "ROOT", root))
                stack.enter_context(patch.object(owner, "__file__", str(source)))
                stack.enter_context(
                    patch.object(generator.sys, "version_info", (3, 14, 6))
                )
                stack.enter_context(
                    patch.object(generator.expat, "EXPAT_VERSION", "expat_2.8.2")
                )
                stack.enter_context(
                    patch.object(generator.platform, "system", return_value="Linux")
                )
                stack.enter_context(
                    patch.object(generator.subprocess, "check_output", side_effect=git)
                )
                # Keep this focused on reference admission, not codec semantics.
                stack.enter_context(
                    patch.dict(generator.encodings.aliases.aliases, {}, clear=True)
                )
                stack.enter_context(
                    patch.object(generator.pkgutil, "iter_modules", return_value=[])
                )
                stack.enter_context(
                    patch.object(generator, "wire_cases", return_value=[])
                )
                _, evidence = generator.generate()
            return json.loads(evidence), calls

    def test_nested_frozen_checkout_with_own_git_root_is_admitted(self):
        result, calls = self.run_fixture()
        self.assertEqual(result["reference"], generator.REVISION)
        self.assertIn(["rev-parse", "--show-toplevel"], calls)
        self.assertIn(["rev-parse", "HEAD"], calls)

    def test_live_source_enclosing_git_wrong_revision_and_byte_drift_are_refused(self):
        for options, message in (
            ({"live": True}, "frozen reference src"),
            ({"enclosing": True}, "own Git root"),
            ({"revision": "0" * 40}, "Wrong frozen reference revision"),
            ({"drift": True}, "source has changed"),
        ):
            with (
                self.subTest(options=options),
                self.assertRaisesRegex(RuntimeError, message),
            ):
                self.run_fixture(**options)
