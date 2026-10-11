# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Exact native target/export bytes and exclusive detached bundle effects."""

import copy
import json
import os
import subprocess
import unittest
from pathlib import Path

from catabolic.domain import CatabolicError
from catabolic.exports import build_export, publish_bundle
from catabolic.targets import definitions, describe
from tests import test_targets as reference

BINARY = Path(
    os.environ.get("CATABOLIC_NATIVE_BINARY", "target/debug/catabolic-native")
).resolve()


@unittest.skipUnless(BINARY.exists(), "build native executable explicitly")
class NativeExportTest(unittest.TestCase):
    setUp = reference.TargetTest.setUp
    source_state = reference.TargetTest.source_state
    document = reference.TargetTest.document

    def native(self, surface, action, **request):
        result = subprocess.run(
            [
                str(BINARY),
                "--db",
                str(self.path),
                "operation",
                surface,
                action,
                "--options",
                json.dumps(request, ensure_ascii=False),
            ],
            text=True,
            capture_output=True,
            timeout=20,
        )
        return (
            json.loads(result.stdout)
            if result.returncode == 0
            else result.stderr.strip().removeprefix("catabolic: ")
        )

    def test_all_twenty_target_descriptions_and_definitions_match(self):
        self.assertEqual(self.native("target", "list"), describe())
        self.assertEqual(self.native("target", "definitions"), definitions())
        for target in describe()["targets"]:
            self.assertEqual(
                self.native("target", "describe", target=target["id"]),
                describe(target["id"]),
            )
        self.assertEqual(
            self.native("target", "describe", target="unknown"),
            "unknown target: unknown; use target list",
        )

    def test_all_export_format_bytes_match(self):
        document = self.document()
        for format in ["xspf", "opds", "nfo"]:
            for base in [
                "https://media.example/library/",
                "HTTPS://media.example/a%20b",
                "http://[::1]/é/",
                None,
            ]:
                with self.subTest(format=format, base=base):
                    try:
                        expected = build_export(document, format, base)
                    except (CatabolicError, ValueError) as error:
                        expected = str(error)
                    self.assertEqual(
                        self.native(
                            "export",
                            "build",
                            document=document,
                            format=format,
                            base_url=base,
                        ),
                        expected,
                    )
        self.assertEqual(self.source_state(), self.before)

    def test_export_metadata_and_url_rejections_match(self):
        document = self.document()
        for base in [
            "file:///tmp/",
            "https://user:password@host",
            "https://host/?query",
            "https://host/#fragment",
            "",
            "https://host/?",
            "https://host/#",
        ]:
            with self.subTest(base=base):
                try:
                    expected = build_export(document, "opds", base)
                except (CatabolicError, ValueError) as error:
                    expected = str(error)
                self.assertEqual(
                    self.native(
                        "export",
                        "build",
                        document=document,
                        format="opds",
                        base_url=base,
                    ),
                    expected,
                )
        for title in [
            "A & <B> 😀",
            "",
            0,
            False,
            12,
            1.0,
            ["a", True],
            {"a": "b"},
            "\x00",
            "\ufffe",
        ]:
            changed = copy.deepcopy(document)
            for item in changed["content"]["items"]:
                item["metadata"]["title"] = title
            for format in ["xspf", "opds", "nfo"]:
                with self.subTest(title=title, format=format):
                    try:
                        expected = build_export(
                            changed, format, "https://media.example/"
                        )
                    except (CatabolicError, ValueError) as error:
                        expected = str(error)
                    self.assertEqual(
                        self.native(
                            "export",
                            "build",
                            document=changed,
                            format=format,
                            base_url="https://media.example/",
                        ),
                        expected,
                    )

    def test_detached_bundle_never_overwrites_and_preserves_sources(self):
        document = self.document()
        export = build_export(document, "xspf")
        reference = self.root / "python-bundle"
        native = self.root / "native-bundle"
        expected = publish_bundle(self.app, document, export, reference)
        self.store.close()
        actual = self.native(
            "export", "publish", document=document, export=export, output=str(native)
        )
        self.assertEqual({**expected, "output": str(native)}, actual)
        for path in reference.rglob("*"):
            other = native / path.relative_to(reference)
            self.assertEqual(path.is_symlink(), other.is_symlink())
            if path.is_symlink():
                self.assertEqual(path.resolve(), other.resolve())
            elif path.is_file():
                self.assertEqual(path.read_bytes(), other.read_bytes())
        before = {
            str(p.relative_to(native)): p.lstat().st_ino for p in native.rglob("*")
        }
        result = self.native(
            "export", "publish", document=document, export=export, output=str(native)
        )
        self.assertIsInstance(result, str)
        self.assertEqual(
            before,
            {str(p.relative_to(native)): p.lstat().st_ino for p in native.rglob("*")},
        )
        self.assertEqual(self.source_state(), self.before)

    def test_bundle_collisions_source_overlap_and_symlink_parents_are_preflight(self):
        document = self.document()
        export = build_export(document, "nfo")
        self.store.close()
        for index, artifact in enumerate(
            [
                {"path": "movies/movie/movie.MKV", "content": "bad"},
                {"path": "catalog-manifest.json", "content": "bad"},
                {"path": "../escape", "content": "bad"},
                {"path": "a", "content": "a"},
                {"path": "e\u0301", "content": "x"},
            ]
        ):
            files = [artifact]
            if artifact["path"] == "a":
                files.append({"path": "a/b", "content": "b"})
            if artifact["path"] == "e\u0301":
                files.append({"path": "é", "content": "y"})
            output = self.root / f"refuse-{index}"
            report = self.native(
                "export",
                "publish",
                document=document,
                export={"format": "nfo", "files": files},
                output=str(output),
            )
            self.assertIsInstance(report, str, report)
            self.assertFalse(output.exists())
        (self.root / "alias").symlink_to(self.root / "media", target_is_directory=True)
        for output in [self.root / "media/new", self.root / "alias/new"]:
            report = self.native(
                "export",
                "publish",
                document=document,
                export=export,
                output=str(output),
            )
            self.assertIsInstance(report, str)
            self.assertFalse(output.exists())
        self.assertEqual(self.source_state(), self.before)
