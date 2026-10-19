# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Frozen Jellyfin JSON wire parity through disposable real HTTP adapters."""

import hashlib
import json
import unittest

from catabolic.consumer_adapters import ConsumerError
from catabolic.consumer_setup import discover
from tests import test_rust_consumers as consumers
from tests.test_rust_plex_xml import transform_server

ENCODINGS = (
    "utf-8",
    "utf-8-sig",
    "utf-16",
    "utf-16-le",
    "utf-16-be",
    "utf-32",
    "utf-32-le",
    "utf-32-be",
)


class NativeConsumerJsonTest(consumers.NativeConsumerTest):
    def setUp(self):
        super().setUp()
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
        self.wire = lambda _route, body: body
        transform_server(self.remote.server, lambda route, body: self.wire(route, body))

    def paired(self):
        before = hashlib.sha256(self.database.read_bytes()).hexdigest()
        expected = discover(self.database, "default", "jelly", "movies")
        actual = self.native("discover", {"connection": "jelly", "type": "movies"})
        self.assertEqual(actual, expected)
        self.assertEqual(hashlib.sha256(self.database.read_bytes()).hexdigest(), before)
        self.assertEqual(self.remote.scans, [])
        self.assertEqual(self.remote.lock_failures, [])
        return actual

    def test_json_unicode_wire_matrix(self):
        for encoding in ENCODINGS:
            with self.subTest(encoding=encoding):

                def wire(route, body, encoding=encoding):
                    value = json.loads(body)
                    if route == "/System/Info":
                        value["Version"] = "Café€漢字😀"
                    else:
                        value[0]["Name"] = "Café€漢字😀"
                    return json.dumps(value, ensure_ascii=False).encode(encoding)

                self.wire = wire
                actual = self.paired()
                self.assertEqual(actual["identity"]["version"], "Café€漢字😀")
                self.assertEqual(actual["libraries"][0]["name"], "Café€漢字😀")

    def test_json_boms_and_mixed_request_encodings(self):
        for identity, library in (("utf-32", "utf-16-be"), ("utf-16-le", "utf-8-sig")):
            with self.subTest(identity=identity, library=library):
                self.wire = lambda route, body, identity=identity, library=library: (
                    body.decode().encode(
                        identity if route == "/System/Info" else library
                    )
                )
                self.assertTrue(self.paired()["complete"])
        for prefix, encoding in (
            (b"\xfe\xff", "utf-16-be"),
            (b"\x00\x00\xfe\xff", "utf-32-be"),
        ):
            self.wire = lambda _route, body, prefix=prefix, encoding=encoding: (
                prefix + body.decode().encode(encoding)
            )
            self.assertTrue(self.paired()["complete"])

    def test_json_duplicate_keys_big_integers_and_private_number_keys(self):
        for encoding in ENCODINGS:
            with self.subTest(encoding=encoding):

                def wire(route, body, encoding=encoding):
                    if route == "/System/Info":
                        value = (
                            '{"Id":"fixture-server","Version":"first","Version":"last",'
                            '"ignored":1208925819614629174706177,'
                            '"$serde_json::private::Number":"1e400"}'
                        )
                    else:
                        value = body.decode()
                    return value.encode(encoding)

                self.wire = wire
                self.assertEqual(self.paired()["identity"]["version"], "last")

    def test_json_malformed_encodings_refuse_without_persistent_effects(self):
        malformed = (
            b"\xff",
            b"\xef\xbb",
            b"\xff\xfe{",
            b"\xff\xfe\x00\x00{\x00\x00",
            b"\x00\x00\xfe\xff\x00\x11\x00\x00",
            b"\xff\xfe{\x00",
            b"\x00\x00\xfe\xff\x00\x00\x00{",
            b"{",
            b"\xef\xbb\xbf[]",
        )
        for raw in malformed:
            with self.subTest(raw=raw.hex()):
                self.wire = lambda _route, _body, raw=raw: raw
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

    def test_json_raw_response_budget_is_unchanged(self):
        maximum = 8 * 1024 * 1024
        for length in (maximum, maximum + 1):
            with self.subTest(length=length):

                def wire(_route, body, length=length):
                    return body + b" " * (length - len(body))

                self.wire = wire
                if length == maximum:
                    self.assertTrue(self.paired()["complete"])
                else:
                    with self.assertRaises(ConsumerError) as caught:
                        discover(self.database, "default", "jelly", "movies")
                    # Retain the pre-existing transport classification gap:
                    # Python's subprocess transport wraps an oversized response
                    # as temporarily_failed; native explicitly says response_limit.
                    # Both refuse before JSON decoding or persistent effects.
                    self.assertEqual(caught.exception.code, "temporarily_failed")
                    self.assertIn(
                        "consumer response_limit",
                        self.native(
                            "discover",
                            {"connection": "jelly", "type": "movies"},
                            ok=False,
                        ),
                    )
                self.assertEqual(self.remote.scans, [])


def load_tests(loader, tests, pattern):
    return unittest.TestSuite(
        NativeConsumerJsonTest(name)
        for name in sorted(NativeConsumerJsonTest.__dict__)
        if name.startswith("test_json_")
    )
