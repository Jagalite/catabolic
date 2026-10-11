# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Frozen XML wire differentials at real native consumer owner boundaries.

All transport uses disposable numeric-loopback fixtures and fake credentials.
The separate generated Rust matrix checks every byte for every accepted pinned
standard-library encoding declaration; these cases exercise complete owners.
"""

import io
import re
import unittest
from urllib.parse import urlsplit
from xml.etree import ElementTree as ET

from catabolic.consumer_adapters import ConsumerError
from catabolic.consumer_setup import discover, verify_indexing
from tests import test_rust_consumers as consumers
from tests import test_rust_plex_import as importing
from tests import test_rust_plex_matching as matching
from tests import test_rust_plex_metadata as metadata

UTF_WIRES = (
    ("utf-8", None),
    ("utf-8-sig", "UTF-8"),
    ("utf-16", None),
    ("utf-16", "UTF-16"),
    ("utf-16-le", None),
    ("utf-16-le", "UTF-16LE"),
    ("utf-16-be", None),
    ("utf-16-be", "UTF-16BE"),
)


def encoded(body, encoding, declaration=None):
    text = ET.tostring(ET.fromstring(body), encoding="unicode")
    if declaration is not None:
        text = f'<?xml version="1.0" encoding="{declaration}"?>' + text
    return text.encode(encoding)


def transform_server(server, transform):
    original = server.RequestHandlerClass

    class Transformed(original):
        def do_GET(self):
            stream = self.wfile
            buffer = io.BytesIO()
            self.wfile = buffer
            try:
                super().do_GET()
            finally:
                self.wfile = stream
            headers, body = buffer.getvalue().split(b"\r\n\r\n", 1)
            body = transform(urlsplit(self.path).path, body)
            headers = re.sub(
                rb"(?im)^Content-Length: [0-9]+\r?$",
                b"Content-Length: " + str(len(body)).encode(),
                headers,
            )
            self.wfile.write(headers + b"\r\n\r\n" + body)

    server.RequestHandlerClass = Transformed


class ConsumerXml(consumers.NativeConsumerTest):
    def setUp(self):
        super().setUp()
        self.xml_transform = lambda _route, body: body
        transform_server(
            self.remote.server, lambda route, body: self.xml_transform(route, body)
        )

    def compare_discovery(self):
        expected = discover(self.database, "default", "plex", "movie")
        actual = self.native("discover", {"connection": "plex", "type": "movie"})
        self.assertEqual(actual, expected)
        return actual

    def test_xml_discovery_utf_wire_matrix_and_arbitrary_roots(self):
        for encoding, declaration in UTF_WIRES:
            with self.subTest(encoding=encoding, declaration=declaration):

                def transform(route, body, encoding=encoding, declaration=declaration):
                    root = ET.fromstring(body)
                    root.tag = "{urn:root}Arbitrary"
                    if route == "/":
                        root.set("version", "Café 漢字😀")
                    return encoded(ET.tostring(root), encoding, declaration)

                self.xml_transform = transform
                self.assertEqual(
                    self.compare_discovery()["identity"]["version"], "Café 漢字😀"
                )

    def test_xml_declared_single_byte_and_aliases(self):
        for codec, name, sample in (
            ("latin1", "ISO-8859-1", "Café\x80"),
            ("cp1252", "windows--1252", "Café€"),
            ("cp1251", "WINDOWS_1251", "Привет"),
            ("mac_roman", "mac-roman", "Café"),
            ("ascii", "utf8", "ASCII fallback"),
            ("ascii", "unicode_escape", r"literal \u00e9"),
        ):
            with self.subTest(name=name):

                def transform(route, body, codec=codec, name=name, sample=sample):
                    root = ET.fromstring(body)
                    if route == "/":
                        root.set("version", sample)
                    text = ET.tostring(root, encoding="unicode")
                    return (f'<?xml version="1.0" encoding="{name}"?>' + text).encode(
                        codec
                    )

                self.xml_transform = transform
                self.assertEqual(
                    self.compare_discovery()["identity"]["version"], sample
                )

    def test_xml_choices_qualified_children_attributes_and_root_iteration(self):
        for mode in (
            "foreign_children",
            "shadow_attributes",
            "foreign_attributes",
            "root",
        ):
            with self.subTest(mode=mode):

                def transform(route, body, mode=mode):
                    if not route.startswith("/system/"):
                        return body
                    root = ET.fromstring(body)
                    node = root[0]
                    key = "name" if node.tag == "Scanner" else "identifier"
                    if mode == "foreign_children":
                        root.insert(
                            0, ET.Element("{urn:foreign}" + node.tag, {key: "forged"})
                        )
                    elif mode == "shadow_attributes":
                        real = node.attrib.pop(key)
                        node.set("{urn:foreign}" + key, "forged")
                        node.set(key, real)
                    elif mode == "foreign_attributes":
                        node.set("{urn:foreign}" + key, node.attrib.pop(key))
                    else:
                        root = node
                    return ET.tostring(root)

                self.xml_transform = transform
                result = self.compare_discovery()["creation_choices"]
                if mode == "foreign_attributes":
                    self.assertEqual(result["error"]["code"], "invalid_configuration")
                else:
                    self.assertEqual(result["scanners"], ["Fixture Scanner"])
                    self.assertEqual(result["agents"], ["fixture.agent"])

    def test_xml_indexed_exact_namespace_identity_and_root_iteration(self):
        self.pending()
        self.remote.indexed = ["/plex/Movies/One.mkv"]
        path = self.remote.indexed[0]
        pages = (
            f'<Anything xmlns:q="urn:q"><q:Video ratingKey="1" guid="real"><Wrapper><Part file="{path}"/></Wrapper></q:Video></Anything>',
            f'<Anything xmlns:q="urn:q"><Video ratingKey="1"><q:Part file="{path}"/></Video></Anything>',
            f'<Anything xmlns:q="urn:q"><Video q:ratingKey="forged" q:guid="forged"><Part q:file="{path}"/></Video></Anything>',
            f'<Anything xmlns:q="urn:q"><Video q:ratingKey="forged" ratingKey="1" q:guid="forged" guid="real"><Part q:file="/wrong" file="{path}"/></Video></Anything>',
            f'<Part file="{path}"/>',
            f'<Anything><Part file="{path}" ratingKey="direct" guid="direct"/></Anything>',
        )
        for xml in pages:
            for encoding, declaration in (("utf-8", None), ("utf-16-be", "UTF-16")):
                with self.subTest(xml=xml, encoding=encoding):
                    self.xml_transform = (
                        lambda route, body, xml=xml, encoding=encoding, declaration=declaration: (
                            encoded(xml.encode(), encoding, declaration)
                            if route.endswith("/all")
                            else body
                        )
                    )
                    expected = verify_indexing(self.database, "default", "movies")
                    actual = self.native("verify-indexing", {"id": "movies"})
                    self.assertEqual(actual, expected)

    def test_xml_invalid_utf16_utf32_and_conflicting_declarations(self):
        cases = [
            ("utf-16", "UTF-8"),
            ("utf-16-le", "UTF-16BE"),
            ("utf-16-be", "UTF-16LE"),
            ("utf-8", "UTF-16"),
            ("utf-32", "UTF-32"),
            ("utf-32-le", None),
            ("utf-32-be", None),
        ]
        for encoding, declaration in cases:
            with self.subTest(encoding=encoding, declaration=declaration):
                self.xml_transform = (
                    lambda _route, body, encoding=encoding, declaration=declaration: (
                        encoded(body, encoding, declaration)
                    )
                )
                with self.assertRaisesRegex(ConsumerError, "invalid_response"):
                    discover(self.database, "default", "plex", "movie")
                self.assertIn(
                    "invalid_response",
                    self.native("discover", {"connection": "plex"}, ok=False),
                )
        for bad in (
            b"\xff\xfe<\x00x",
            b"\xff\xfe<\x00x\x00>\x00\x00\xd8<\x00/\x00x\x00>\x00",
        ):
            self.xml_transform = lambda _route, _body, bad=bad: bad
            with self.assertRaisesRegex(ConsumerError, "invalid_response"):
                discover(self.database, "default", "plex")
            self.assertIn(
                "invalid_response",
                self.native("discover", {"connection": "plex"}, ok=False),
            )

    def test_xml_utf16_dtd_is_an_explicit_native_safety_tightening(self):
        def transform(_route, body):
            return ("<!DOCTYPE MediaContainer []>" + body.decode()).encode("utf-16")

        self.xml_transform = transform
        self.assertTrue(discover(self.database, "default", "plex")["complete"])
        self.assertIn(
            "invalid_response",
            self.native("discover", {"connection": "plex"}, ok=False),
        )
        self.assertEqual(self.remote.scans, [])


class ImportXml(importing.NativePlexImportTest):
    def protocol(self, route, params):
        body = super().protocol(route, params)
        transform = getattr(self, "xml_transform", None)
        return transform(route, body) if transform else body

    def test_xml_import_utf_wire_matrix(self):
        self.items[0].set("title", "Café 漢字😀")
        for encoding, declaration in UTF_WIRES:
            with self.subTest(encoding=encoding, declaration=declaration):
                self.xml_transform = (
                    lambda _route, body, encoding=encoding, declaration=declaration: (
                        encoded(body, encoding, declaration)
                    )
                )
                self.assertTrue(self.preview()["complete"])
        self.assertTrue(self.apply()["complete"])

    def test_xml_import_declared_legacy_encoding(self):
        self.items[0].set("title", "Café€")
        self.xml_transform = lambda _route, body: encoded(
            body, "cp1252", "windows-1252"
        )
        self.assertTrue(self.preview()["complete"])


class MatchingXml(matching.NativePlexMatchingTest):
    def protocol(self, route, method="GET", params=None):
        body = super().protocol(route, method, params)
        transform = getattr(self, "xml_transform", None)
        return transform(route, body) if transform and method == "GET" else body

    def test_xml_matching_page_and_detail_utf_wire_matrix(self):
        self.node.set("title", "Café 漢字😀")
        for encoding, declaration in UTF_WIRES:
            with self.subTest(encoding=encoding, declaration=declaration):
                self.xml_transform = (
                    lambda _route, body, encoding=encoding, declaration=declaration: (
                        encoded(body, encoding, declaration)
                    )
                )
                self.assertTrue(self.preview_match()["complete"])
        preview = self.preview_match()
        self.assertTrue(
            self.preview_match(apply=True, expected_plan=preview["plan_id"])["complete"]
        )

    def test_xml_matching_page_and_detail_legacy_encoding(self):
        self.node.set("title", "Café€")
        self.xml_transform = lambda _route, body: encoded(
            body, "cp1252", "windows-1252"
        )
        self.assertTrue(self.preview_match()["complete"])


class MetadataXml(metadata.NativePlexMetadataTest):
    def test_xml_metadata_preview_write_readback_utf_wire_matrix(self):
        for encoding, declaration in UTF_WIRES:
            with self.subTest(encoding=encoding, declaration=declaration):
                self.curate(title=f"Café 漢字😀 {encoding} {declaration}")
                self.wire_transform = (
                    lambda _route, body, encoding=encoding, declaration=declaration: (
                        encoded(body, encoding, declaration)
                    )
                )
                preview = self.preview(fields=["title"])
                self.assertTrue(self.apply(preview, fields=["title"])["complete"])

    def test_xml_metadata_preview_write_readback_legacy_encoding(self):
        self.curate(title="Café€")
        self.wire_transform = lambda _route, body: encoded(
            body, "cp1252", "windows-1252"
        )
        preview = self.preview(fields=["title"])
        self.assertTrue(self.apply(preview, fields=["title"])["complete"])


def load_tests(loader, tests, pattern):
    # Reuse owner fixtures without rerunning their inherited test suites here.
    return unittest.TestSuite(
        cls(name)
        for cls in (ConsumerXml, ImportXml, MatchingXml, MetadataXml)
        for name in sorted(cls.__dict__)
        if name.startswith("test_xml_")
    )
