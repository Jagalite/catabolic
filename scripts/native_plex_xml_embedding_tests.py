#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Installed-wheel XML qualification, with an explicit development-only mode.

Run outside the checkout with -I; all services, tokens, and catalogs are isolated
fixtures. Development mode does not qualify a paired helper or release artifact.
"""

import argparse
import hashlib
import importlib.metadata
import importlib.util
import io
import json
import os
import re
import sys
import unittest
from pathlib import Path
from xml.etree import ElementTree as ET

# Refuse before loading fixture code, installed data, or touching a report.
if not __debug__ or os.environ.get("PYTHONOPTIMIZE"):
    raise SystemExit(
        "XML embedding qualification requires assertions; do not use -O, -OO, or PYTHONOPTIMIZE"
    )


def _clear_stale_report():
    report_parser = argparse.ArgumentParser(add_help=False)
    report_parser.add_argument("--report", type=Path)
    report_args, _ = report_parser.parse_known_args()
    if report_args.report:
        report_args.report.unlink(missing_ok=True)


# A normal CLI import failure must not retain a receipt from an earlier run.
# Library imports leave the caller's arguments and files alone.
if __name__ == "__main__":
    _clear_stale_report()

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


EXPECTED_TESTS = (
    "test_installed_utf16_discovery_import_metadata_and_readback",
    "test_installed_conflicting_declaration_is_recoverable_then_legacy_readback",
)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--wheel", type=Path)
    mode.add_argument(
        "--dev", action="store_true", help="Explicit development-only artifact review"
    )
    parser.add_argument("--expected-extension-sha256")
    parser.add_argument("--report", type=Path)
    # Also protect callers that import the driver and invoke main() directly.
    _clear_stale_report()
    args = parser.parse_args()
    if args.dev and not args.expected_extension_sha256:
        parser.error("--dev requires --expected-extension-sha256")
    if args.wheel and args.expected_extension_sha256:
        parser.error("--expected-extension-sha256 is only valid with --dev")
    sources = {
        "script_sha256": Path(__file__),
        "fixture_sha256": FIXTURE,
        "result_verifier_sha256": Path(__file__).with_name(
            "native_qualification_results.py"
        ),
        "wheel_verifier_sha256": Path(__file__).with_name("qualify_native_wheel.py"),
    }
    source_hashes = {
        name: hashlib.sha256(path.read_bytes()).hexdigest()
        for name, path in sources.items()
    }
    import catabolic_native as native

    package = Path(native.__file__).resolve()
    dist = importlib.metadata.distribution("catabolic-native")
    assert package.is_relative_to(Path(dist.locate_file("")).resolve()), "not installed"
    native_extension = native._native
    extension = Path(native_extension.__file__).resolve()
    digest = hashlib.sha256(extension.read_bytes()).hexdigest()
    wheel_digest = (
        hashlib.sha256(args.wheel.read_bytes()).hexdigest() if args.wheel else None
    )
    verification = fixture.wheel_verifier()(args.wheel) if args.wheel else None
    if args.dev:
        assert digest == args.expected_extension_sha256, digest
    suite = unittest.TestSuite(
        InstalledXml(name)
        for name in sorted(InstalledXml.__dict__)
        if name.startswith("test_")
    )
    result = fixture.result_verifier()(
        suite, {InstalledXml: EXPECTED_TESTS}, expected_count=2
    )
    assert sys.modules["catabolic_native"] is native, (
        "installed package identity changed"
    )
    assert Path(native.__file__).resolve() == package, "installed package path changed"
    assert native._native is native_extension, "installed extension identity changed"
    assert Path(native._native.__file__).resolve() == extension, (
        "installed extension path changed"
    )
    assert hashlib.sha256(extension.read_bytes()).hexdigest() == digest, (
        "installed extension changed"
    )
    if args.wheel:
        assert fixture.wheel_verifier()(args.wheel) == verification, (
            "installed wheel changed"
        )
        assert hashlib.sha256(args.wheel.read_bytes()).hexdigest() == wheel_digest, (
            "supplied wheel changed"
        )
    assert {
        name: hashlib.sha256(path.read_bytes()).hexdigest()
        for name, path in sources.items()
    } == source_hashes, "qualification sources changed"
    report = {
        "format": 1,
        "scope": "Installed shared XML decoding and metadata readback",
        "tests": result.testsRun,
        "successful_scenarios": result.successful_scenarios,
        "passed": result.wasSuccessful(),
        "python": sys.version,
        "installed_package": str(package),
        "extension_sha256": digest,
        "wheel_sha256": wheel_digest,
        "installed_wheel_verification": verification,
        "helper_sha256": verification["installed_payloads_sha256"][
            "catabolic_native/bin/catabolic-native"
        ]
        if verification
        else None,
        "development_only": args.dev,
        **source_hashes,
    }
    if args.report:
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    raise SystemExit(not result.wasSuccessful())


if __name__ == "__main__":
    main()
