# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Independent shared XML regressions against immutable native CLI artifacts.

Uses fresh numeric-loopback fixtures, fake tokens and disposable catalogs. The
parent owner fixtures compare complete frozen/native values and database state.
"""

import json
import unittest
from xml.etree import ElementTree as ET

from catabolic.consumer_adapters import ConsumerError
from catabolic.consumer_setup import discover, verify_indexing
from tests.test_rust_plex_xml import (
    ConsumerXml,
    ImportXml,
    MatchingXml,
    MetadataXml,
    encoded,
)


def reserved_pi(body, target, position, codec):
    instruction = f'<?{target} version="1.0" encoding="UTF-8"?>'
    if position == "initial":
        text = instruction + body.decode()
    elif position == "trailing":
        text = body.decode() + instruction
    else:
        root = ET.fromstring(body)
        root.insert(0, ET.ProcessingInstruction(target, 'encoding="UTF-8"'))
        text = ET.tostring(root, encoding="unicode")
    return text.encode(codec)


def mixed_wire(route, body):
    if route == "/":
        return encoded(body, "utf-16-be", "UTF-16BE")
    if route == "/library/sections":
        return encoded(body, "cp1252", "WINDOWS___1252___")
    if route.endswith("/all"):
        return encoded(body, "utf-8-sig", "UTF-8")
    return encoded(body, "utf-16-le", "UTF-16LE")


def namespace_resets(route, body):
    root = ET.fromstring(body)
    root.set("xmlns", "")
    if route.endswith("/all") or route.startswith("/library/metadata/"):
        for item in root:
            # Item tags have no name requirement, while their direct Media,
            # Guid and Field children must reset the foreign default namespace.
            item.set("xmlns", "urn:foreign-item")
            for child in item:
                child.set("xmlns", "")
    return ET.tostring(root, encoding="unicode").encode("utf-16")


class ConsumerReview(ConsumerXml):
    def test_review_reserved_pi_target_spelling_position_and_wire(self):
        for target in ("xml", "xmL", "xMl", "xML", "Xml", "XmL", "XMl", "XML"):
            for position in ("initial", "nested", "trailing"):
                if target == "xml" and position == "initial":
                    continue
                for codec in ("utf-8", "utf-8-sig", "utf-16-le", "utf-16-be"):
                    with self.subTest(target=target, position=position, codec=codec):
                        self.xml_transform = (
                            lambda route, body, target=target, position=position, codec=codec: (
                                reserved_pi(body, target, position, codec)
                                if route == "/"
                                else body
                            )
                        )
                        with self.assertRaisesRegex(ConsumerError, "invalid_response"):
                            discover(self.database, "default", "plex", "movie")
                        self.assertIn(
                            "invalid_response",
                            self.native("discover", {"connection": "plex"}, ok=False),
                        )
        self.assertEqual(self.remote.scans, [])

    def test_review_reserved_target_literal_and_allowed_pi_controls(self):
        for mode in ("comment", "cdata", "attribute", "stylesheet", "declaration"):
            for codec in ("utf-8", "utf-16"):
                with self.subTest(mode=mode, codec=codec):

                    def transform(route, body, mode=mode, codec=codec):
                        if route != "/":
                            return body
                        root = ET.fromstring(body)
                        if mode == "attribute":
                            root.set("version", "<?XML encoding='UTF-8'?>")
                        if mode in ("comment", "cdata"):
                            root.append(ET.Element("ReviewControl"))
                        text = ET.tostring(root, encoding="unicode")
                        if mode == "comment":
                            text = text.replace(
                                "<ReviewControl />", "<!-- <?XML version='1.0'?> -->"
                            )
                        elif mode == "cdata":
                            text = text.replace(
                                "<ReviewControl />", "<![CDATA[<?XML version='1.0'?>]]>"
                            )
                        elif mode == "stylesheet":
                            text = '<?xml-stylesheet href="fixture"?>' + text
                        elif mode == "declaration":
                            text = '<?xml version="1.0"?>' + text
                        return text.encode(codec)

                    self.xml_transform = transform
                    self.assertTrue(self.compare_discovery()["complete"])

    def test_review_mixed_encodings_across_owned_discovery_documents(self):
        self.xml_transform = mixed_wire
        self.assertTrue(self.compare_discovery()["complete"])

    def test_review_default_namespace_resets_and_attribute_aliases(self):
        def transform(route, body):
            if route == "/":
                text = '<Root xmlns="urn:root" xmlns:q="urn:q" q:machineIdentifier="wrong" machineIdentifier="fixture-server" q:version="wrong" version="Café"/>'
            elif route == "/library/sections":
                text = '<Root xmlns="urn:root" xmlns:q="urn:q"><Directory key="ignored"/><Directory xmlns="" key="7" q:key="wrong" uuid="review" title="Café" type="movie"><Location xmlns="urn:foreign" path="/wrong"/><Location path="/right"/></Directory></Root>'
            elif "/scanners/" in route:
                text = '<Scanner xmlns="urn:root" name="ignored"><Wrapper xmlns=""><Scanner xmlns:q="urn:q" q:name="wrong" name="Right"/><Scanner name="Right"/></Wrapper></Scanner>'
            else:
                text = '<Agent xmlns="urn:root" identifier="ignored"><Wrapper xmlns=""><Agent xmlns:q="urn:q" q:identifier="wrong" identifier="right"/></Wrapper></Agent>'
            return text.encode("utf-16-be")

        self.xml_transform = transform
        actual = self.compare_discovery()
        self.assertEqual(actual["libraries"][0]["roots"], ["/right"])
        self.assertEqual(actual["creation_choices"]["scanners"], ["Right"])
        self.assertEqual(actual["creation_choices"]["agents"], ["right"])

    def test_review_indexed_namespace_reset_and_nested_foreign_identity(self):
        self.pending()
        path = "/plex/Movies/One.mkv"
        self.remote.indexed = [path]
        pages = (
            f'<Root xmlns="urn:root" xmlns:q="urn:q"><q:Video q:ratingKey="wrong" ratingKey="7" guid="real"><Wrapper xmlns=""><Part file="{path}"/><Part xmlns="urn:q" file="/wrong"/></Wrapper></q:Video></Root>',
            f'<Part xmlns="" file="{path}"/>',
            f'<Root xmlns="urn:root"><Part file="{path}"/><Part xmlns="" file="{path}"/></Root>',
        )
        for text in pages:
            with self.subTest(text=text):
                self.xml_transform = lambda route, body, text=text: (
                    text.encode("utf-16-le") if route.endswith("/all") else body
                )
                expected = verify_indexing(self.database, "default", "movies")
                self.assertEqual(
                    self.native("verify-indexing", {"id": "movies"}), expected
                )

    def test_review_jellyfin_json_keeps_its_own_decoder(self):
        self.native(
            "connection-put",
            {
                "id": "jelly",
                "application": "jellyfin",
                "endpoint": self.remote.url,
                "credential_env": "CATABOLIC_TEST_PLEX_TOKEN",
                "apply": True,
            },
        )

        def transform(route, body):
            value = json.loads(body)
            if route == "/System/Info":
                value["Version"] = "Café😀"
            return json.dumps(value, ensure_ascii=False).encode()

        self.xml_transform = transform
        expected = discover(self.database, "default", "jelly")
        self.assertEqual(self.native("discover", {"connection": "jelly"}), expected)


class ImportReview(ImportXml):
    def test_review_import_namespace_resets_under_foreign_item(self):
        self.xml_transform = namespace_resets
        self.assertTrue(self.preview()["complete"])

    def test_review_import_mixed_document_encodings(self):
        self.items[0].set("title", "Café😀")
        self.xml_transform = mixed_wire
        self.assertTrue(self.preview()["complete"])

    def test_review_import_page_reserved_pi(self):
        for position in ("initial", "nested", "trailing"):
            with self.subTest(position=position):
                self.xml_transform = lambda route, body, position=position: (
                    reserved_pi(body, "xMl", position, "utf-16-be")
                    if route.endswith("/all")
                    else body
                )
                with self.assertRaisesRegex(ConsumerError, "invalid_response"):
                    self.preview()

    def test_review_import_requires_unqualified_root_after_decoding(self):
        def transform(route, body):
            if not route.endswith("/all"):
                return body
            root = ET.fromstring(body)
            root.tag = "{urn:foreign}MediaContainer"
            return encoded(ET.tostring(root), "utf-16-le", "UTF-16LE")

        self.xml_transform = transform
        with self.assertRaisesRegex(ConsumerError, "invalid_response"):
            self.preview()


class MatchingReview(MatchingXml):
    def test_review_matching_namespace_resets_under_foreign_item(self):
        self.xml_transform = namespace_resets
        self.assertTrue(self.preview_match()["complete"])

    def test_review_matching_mixed_document_encodings(self):
        self.node.set("title", "Café😀")
        self.xml_transform = mixed_wire
        self.assertTrue(self.preview_match()["complete"])

    def test_review_matching_detail_reserved_pi(self):
        self.xml_transform = lambda route, body: (
            reserved_pi(body, "XmL", "trailing", "utf-16-le")
            if route.startswith("/library/metadata/")
            else body
        )
        with self.assertRaisesRegex(ConsumerError, "invalid_response"):
            self.preview_match()

    def test_review_matching_requires_unqualified_detail_root(self):
        def transform(route, body):
            if not route.startswith("/library/metadata/"):
                return body
            root = ET.fromstring(body)
            root.tag = "{urn:foreign}MediaContainer"
            return encoded(ET.tostring(root), "utf-16-be", "UTF-16BE")

        self.xml_transform = transform
        with self.assertRaisesRegex(ConsumerError, "metadata_invalid_item_response"):
            self.preview_match()


class MetadataReview(MetadataXml):
    def test_review_metadata_namespace_resets_put_and_readback(self):
        self.wire_transform = namespace_resets
        preview = self.preview(fields=["title"])
        self.assertTrue(self.apply(preview, fields=["title"])["complete"])

    def test_review_metadata_requires_unqualified_detail_root(self):
        def transform(route, body):
            if not route.startswith("/library/metadata/"):
                return body
            root = ET.fromstring(body)
            root.tag = "{urn:foreign}MediaContainer"
            return encoded(ET.tostring(root), "utf-16-be", "UTF-16BE")

        self.wire_transform = transform
        with self.assertRaisesRegex(ConsumerError, "metadata_invalid_item_response"):
            self.preview(fields=["title"])
        self.assertEqual(self.writes(), [])

    def test_review_metadata_mixed_document_encodings_put_readback(self):
        self.curate(title="Café😀")
        self.wire_transform = mixed_wire
        preview = self.preview(fields=["title"])
        self.assertTrue(self.apply(preview, fields=["title"])["complete"])

    def test_review_metadata_snapshot_reserved_pi(self):
        self.wire_transform = lambda route, body: (
            reserved_pi(body, "XML", "initial", "utf-16")
            if route.startswith("/library/metadata/")
            else body
        )
        with self.assertRaisesRegex(ConsumerError, "invalid_response"):
            self.preview(fields=["title"])
        self.assertEqual(self.writes(), [])

    def test_review_metadata_readback_reserved_pi_is_uncertain(self):
        preview = self.preview(fields=["title"])

        def transform(route, body):
            if route.startswith("/library/metadata/") and self.writes():
                return reserved_pi(body, "XML", "trailing", "utf-16-be")
            return mixed_wire(route, body)

        self.wire_transform = transform
        result = self.apply(preview, fields=["title"])
        self.assertFalse(result["complete"])
        self.assertIn("invalid_response", json.dumps(result))
        self.assertEqual(len(self.writes()), 1)


def load_tests(loader, tests, pattern):
    return unittest.TestSuite(
        cls(name)
        for cls in (ConsumerReview, ImportReview, MatchingReview, MetadataReview)
        for name in sorted(cls.__dict__)
        if name.startswith("test_review_")
    )
