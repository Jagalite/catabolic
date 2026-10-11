# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Independent review of only Jellyfin's JSON response decoding boundary."""

import hashlib
import json
import unittest

from catabolic.consumer_adapters import ConsumerError
from catabolic.consumer_setup import discover
from tests.test_rust_consumer_json import NativeConsumerJsonTest


class ConsumerJsonReview(NativeConsumerJsonTest):
    def test_review_second_response_decode_failure_preserves_catalog(self):
        malformed = (
            b'"\xc0\xaf"',
            b'"\xf4\x90\x80\x80"',
            b'"\xe2\x82"',
            b"\xff\xfe[\x00]\x00\x00",
            b"\x00\x00\xfe\xff\x00\x00\x00[\x00\x00\x00]x",
            b"\xef\xbb\xbf\xef\xbb\xbf[]",
            b"[]\x00",
            "[]\ufeff".encode("utf-16-be"),
            "{}".encode("utf-32"),
            "null".encode("utf-16"),
        )
        for raw in malformed:
            with self.subTest(raw=raw.hex()):
                self.wire = lambda route, body, raw=raw: (
                    body if route == "/System/Info" else raw
                )
                before = hashlib.sha256(self.database.read_bytes()).hexdigest()
                with self.assertRaises(ConsumerError) as caught:
                    discover(self.database, "default", "jelly", "movies")
                self.assertEqual(caught.exception.code, "invalid_response")
                self.assertIn(
                    "consumer invalid_response",
                    self.native(
                        "discover", {"connection": "jelly", "type": "movies"}, ok=False
                    ),
                )
                self.assertEqual(
                    hashlib.sha256(self.database.read_bytes()).hexdigest(), before
                )
                self.assertEqual(self.remote.scans, [])

    def test_review_raw_budget_allows_larger_decoded_utf8(self):
        maximum = 8 * 1024 * 1024
        for target in ("/System/Info", "/Library/VirtualFolders"):
            with self.subTest(target=target):

                def wire(route, body, target=target):
                    if route != target:
                        return body
                    value = json.loads(body)
                    ignored = value if isinstance(value, dict) else value[0]
                    ignored["ignored"] = ""
                    empty = json.dumps(value, ensure_ascii=False)
                    count = (maximum - len(empty.encode("utf-16-be"))) // 2
                    ignored["ignored"] = "漢" * count
                    text = json.dumps(value, ensure_ascii=False)
                    raw = text.encode("utf-16-be")
                    self.assertEqual(len(raw), maximum)
                    self.assertGreater(len(text.encode("utf-8")), maximum)
                    return raw

                self.wire = wire
                self.assertTrue(self.paired()["complete"])

    def test_review_escaped_duplicate_names_and_number_boundaries(self):
        for codec in ("utf-8-sig", "utf-16-le", "utf-16-be", "utf-32-le", "utf-32-be"):
            with self.subTest(codec=codec):

                def wire(route, body, codec=codec):
                    if route == "/System/Info":
                        raw = (
                            '{"Id":"fixture-server","Version":"first",'
                            '"\\u0056ersion":"last",'
                            '"numbers":[18446744073709551616,-9223372036854775809,'
                            "9007199254740993,1.7976931348623157e308,5e-324,-1e-400],"
                            '"$serde_json::private::Number":{"nested":"1e400"}}'
                        )
                    else:
                        raw = (
                            '[{"ItemId":"1","Name":"first","\\u004eame":"last",'
                            '"CollectionType":"movies","Locations":[],"n":'
                            + "9" * 1000
                            + "}]"
                        )
                    return raw.encode(codec)

                self.wire = wire
                actual = self.paired()
                self.assertEqual(actual["identity"]["version"], "last")
                self.assertEqual(actual["libraries"][0]["name"], "last")


def load_tests(loader, tests, pattern):
    return unittest.TestSuite(
        ConsumerJsonReview(name)
        for name in sorted(ConsumerJsonReview.__dict__)
        if name.startswith("test_review_")
    )
