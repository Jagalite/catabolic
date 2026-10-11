#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Independent installed-development-wheel XML correction regressions."""

import argparse
import hashlib
import importlib.metadata
import importlib.util
import io
import json
import re
import unittest
from pathlib import Path
from xml.etree import ElementTree as ET

FIXTURE = Path(__file__).with_name("native_plex_metadata_embedding_tests.py")
spec = importlib.util.spec_from_file_location("_installed_xml_review_fixture", FIXTURE)
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


class InstalledXmlReview(fixture.InstalledPlexMetadata):
    def setUp(self):
        super().setUp()
        self.mode = "namespace_reset"
        original = self.server.RequestHandlerClass
        owner = self

        class Transformed(original):
            def do_GET(self):
                stream, buffer = self.wfile, io.BytesIO()
                self.wfile = buffer
                try:
                    super().do_GET()
                finally:
                    self.wfile = stream
                headers, body = buffer.getvalue().split(b"\r\n\r\n", 1)
                root = ET.fromstring(body)
                root.set("xmlns", "")
                for item in root:
                    if item.tag == "Video":
                        item.set("xmlns", "urn:foreign-item")
                        for child in item:
                            child.set("xmlns", "")
                if owner.mode == "literal":
                    root.append(ET.Comment(" <?XML encoding='UTF-8'?> "))
                    root.set("review", "<?XML literal?>")
                text = ET.tostring(root, encoding="unicode")
                if owner.mode == "pi":
                    text = '<?XML version="1.0" encoding="UTF-8"?>' + text
                elif owner.mode == "literal":
                    text = '<?xml-stylesheet href="fixture"?>' + text
                body = text.encode("utf-16-be")
                headers = re.sub(
                    rb"(?im)^Content-Length: [0-9]+\r?$",
                    b"Content-Length: " + str(len(body)).encode(),
                    headers,
                )
                self.wfile.write(headers + b"\r\n\r\n" + body)

        self.server.RequestHandlerClass = Transformed

    def test_installed_reserved_pi_then_namespace_reset_recovery(self):
        self.mode = "pi"
        with self.assertRaisesRegex(RuntimeError, "invalid_response"):
            self.metadata()
        self.assertEqual(self.events(), [])
        self.assertFalse(any(method == "PUT" for method, _ in self.requests))
        self.mode = "namespace_reset"
        self.assertTrue(
            self.native.execute_operation(
                self.path, "consumers", "discover", connection="fixture"
            )["complete"]
        )
        self.assertTrue(
            self.native.execute_operation(
                self.path,
                "plex-import",
                "run",
                connection="fixture",
                library_id="7",
                mappings=[{"remote_root": "/media", "location": "media"}],
            )["complete"]
        )
        preview = self.metadata()
        self.assertTrue(
            self.metadata(apply=True, expected_plan=preview["plan_id"])["complete"]
        )
        self.assertEqual(self.events()[0]["state"], "verified")
        self.assertEqual(sum(method == "PUT" for method, _ in self.requests), 1)

    def test_installed_literal_reserved_target_and_stylesheet_controls(self):
        self.mode = "literal"
        self.assertTrue(
            self.native.execute_operation(
                self.path, "consumers", "discover", connection="fixture"
            )["complete"]
        )
        self.assertTrue(self.metadata()["complete"])
        self.assertEqual(self.events(), [])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dev", action="store_true", required=True)
    parser.add_argument("--expected-extension-sha256", required=True)
    args = parser.parse_args()
    import catabolic_native as native

    package = Path(native.__file__).resolve()
    dist = importlib.metadata.distribution("catabolic-native")
    assert package.is_relative_to(Path(dist.locate_file("")).resolve()), "not installed"
    extension = Path(native._native.__file__).resolve()
    digest = hashlib.sha256(extension.read_bytes()).hexdigest()
    assert digest == args.expected_extension_sha256, digest
    names = tuple(
        sorted(name for name in InstalledXmlReview.__dict__ if name.startswith("test_"))
    )
    result = fixture.result_verifier()(
        unittest.TestSuite(InstalledXmlReview(name) for name in names),
        {InstalledXmlReview: names},
        expected_count=len(names),
    )
    assert hashlib.sha256(extension.read_bytes()).hexdigest() == digest
    print(
        json.dumps(
            {
                "scope": "Independent installed development-wheel XML correction tests",
                "tests": result.testsRun,
                "successful_scenarios": result.successful_scenarios,
                "extension_sha256": digest,
                "development_only": args.dev,
                "fixture_sha256": hashlib.sha256(FIXTURE.read_bytes()).hexdigest(),
                "script_sha256": hashlib.sha256(
                    Path(__file__).read_bytes()
                ).hexdigest(),
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
