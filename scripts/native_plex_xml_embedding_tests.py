#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Installed development-wheel smoke for the shared XML decoding boundary.

Run outside the checkout with -I; all services, tokens, and catalogs are isolated
fixtures. This does not qualify a final paired helper or release artifact.
"""

import argparse
import hashlib
import importlib.metadata
import importlib.util
import io
import json
import re
import sys
import unittest
from pathlib import Path
from xml.etree import ElementTree as ET

FIXTURE = Path(__file__).with_name("native_plex_metadata_embedding_tests.py")
spec = importlib.util.spec_from_file_location("_installed_xml_fixture", FIXTURE)
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


class InstalledXml(fixture.InstalledPlexMetadata):
    def setUp(self):
        super().setUp()
        self.encoding, self.declaration = "utf-16-be", "UTF-16BE"
        original = self.server.RequestHandlerClass
        owner = self

        class Encoded(original):
            def do_GET(self):
                stream, buffer = self.wfile, io.BytesIO()
                self.wfile = buffer
                try:
                    super().do_GET()
                finally:
                    self.wfile = stream
                headers, body = buffer.getvalue().split(b"\r\n\r\n", 1)
                text = ET.tostring(ET.fromstring(body), encoding="unicode")
                body = (
                    f'<?xml version="1.0" encoding="{owner.declaration}"?>' + text
                ).encode(owner.encoding)
                headers = re.sub(
                    rb"(?im)^Content-Length: [0-9]+\r?$",
                    b"Content-Length: " + str(len(body)).encode(),
                    headers,
                )
                self.wfile.write(headers + b"\r\n\r\n" + body)

        self.server.RequestHandlerClass = Encoded

    def test_installed_utf16_discovery_import_metadata_and_readback(self):
        title = "Installed Café 漢字😀"
        with self.native.Store(self.path, writable=True) as store:
            self.native.Application(store).put_item(
                "movie", {}, {"title": title}, self.item
            )
        discovery = self.native.execute_operation(
            self.path, "consumers", "discover", connection="fixture"
        )
        self.assertTrue(discovery["complete"])
        imported = self.native.execute_operation(
            self.path,
            "plex-import",
            "run",
            connection="fixture",
            library_id="7",
            mappings=[{"remote_root": "/media", "location": "media"}],
        )
        self.assertTrue(imported["complete"])
        preview = self.metadata()
        self.assertTrue(
            self.metadata(apply=True, expected_plan=preview["plan_id"])["complete"]
        )
        self.assertEqual(self.node.get("title"), title)
        self.assertEqual(self.events()[0]["state"], "verified")
        self.assertEqual(sum(method == "PUT" for method, _ in self.requests), 1)

    def test_installed_conflicting_declaration_is_recoverable_then_legacy_readback(
        self,
    ):
        self.declaration = "UTF-8"
        with self.assertRaisesRegex(RuntimeError, "invalid_response"):
            self.metadata()
        self.assertEqual(self.events(), [])
        self.assertFalse(any(method == "PUT" for method, _ in self.requests))
        with self.native.Store(self.path, writable=True) as store:
            self.native.Application(store).put_item(
                "movie", {}, {"title": "Café€"}, self.item
            )
        self.encoding, self.declaration = "cp1252", "windows-1252"
        preview = self.metadata()
        self.assertTrue(
            self.metadata(apply=True, expected_plan=preview["plan_id"])["complete"]
        )
        self.assertEqual(self.node.get("title"), "Café€")
        self.assertEqual(sum(method == "PUT" for method, _ in self.requests), 1)


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
    names = (
        "test_installed_utf16_discovery_import_metadata_and_readback",
        "test_installed_conflicting_declaration_is_recoverable_then_legacy_readback",
    )
    result = fixture.result_verifier()(
        unittest.TestSuite(InstalledXml(name) for name in names),
        {InstalledXml: names},
        expected_count=len(names),
    )
    assert hashlib.sha256(extension.read_bytes()).hexdigest() == digest, (
        "installed extension changed"
    )
    print(
        json.dumps(
            {
                "scope": "Installed development-wheel shared XML smoke; no release qualification",
                "tests": result.testsRun,
                "successful_scenarios": result.successful_scenarios,
                "python": sys.version,
                "installed_package": str(package),
                "extension_sha256": digest,
                "development_only": args.dev,
                "fixture_sha256": hashlib.sha256(FIXTURE.read_bytes()).hexdigest(),
                "result_verifier_sha256": hashlib.sha256(
                    Path(__file__)
                    .with_name("native_qualification_results.py")
                    .read_bytes()
                ).hexdigest(),
                "script_sha256": hashlib.sha256(
                    Path(__file__).read_bytes()
                ).hexdigest(),
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
