# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native released interchange structure, semantics and lossless encoding parity."""

import copy
import json
import select
import subprocess
import unittest

from catabolic.domain import CatabolicError
from catabolic.interchange import specification
from catabolic.interchange.validation import (
    decode_document,
    digest,
    document_value,
    encode_document,
    validate_document,
)
from catabolic.store import encode
from tests.test_interchange import FIXTURE
from tests.test_rust_migration import BINARY


@unittest.skipUnless(BINARY.is_file(), "build catabolic-cli before qualification")
class NativeInterchangeTest(unittest.TestCase):
    def setUp(self):
        self.worker = subprocess.Popen(
            [str(BINARY), "--stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(self.close_worker)

    def close_worker(self):
        self.worker.stdin.close()
        try:
            self.worker.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.worker.kill()
            self.worker.wait()
        self.worker.stdout.close()
        self.worker.stderr.close()

    def native(self, action, args=None, *, ok=True):
        self.worker.stdin.write(
            json.dumps(["interchange", action, json.dumps(args or {})]) + "\n"
        )
        self.worker.stdin.flush()
        self.assertTrue(
            select.select([self.worker.stdout], [], [], 120)[0], "native timeout"
        )
        line = self.worker.stdout.readline()
        self.assertTrue(line, "native worker terminated")
        result = json.loads(line)
        self.assertEqual(result["status"], 0 if ok else 2, result)
        return result["result"] if ok else result["error"].removeprefix("catabolic: ")

    def sample(self, version=1):
        value = json.loads(FIXTURE.read_text())
        value["format_version"] = version
        if version >= 2:
            for key in ("tags", "tag_names", "tag_parents", "taggings"):
                value["content"][key] = []
                value["content"]["counts"][key] = 0
        if version == 3:
            value["content"]["catalog"]["link_mode"] = "symlink"
            for key in ("hardlinks", "retained_hardlinks"):
                value["content"][key] = []
                value["content"]["counts"][key] = 0
        return self.seal(value)

    def seal(self, value):
        value["content_sha256"] = digest(value["content"])
        return value

    def error(self, value, *, seal=True):
        if seal:
            self.seal(value)
        with self.assertRaises(CatabolicError) as expected:
            validate_document(value)
        self.assertEqual(
            self.native("validate", {"value": value}, ok=False), str(expected.exception)
        )

    def test_all_three_frozen_schemas_and_roundtrips(self):
        for version in (1, 2, 3):
            with self.subTest(version=version):
                self.assertEqual(
                    self.native("schema", {"version": version}),
                    specification.schema(version),
                )
                value = self.sample(version)
                expected = document_value(validate_document(value))
                self.assertEqual(self.native("validate", {"value": value}), expected)
                self.assertEqual(
                    self.native("decode", {"raw": encode(value)}), expected
                )
                self.assertEqual(
                    self.native("encode", {"value": value})["encoded"],
                    encode_document(validate_document(value)),
                )

    def test_unknown_fields_everywhere_big_integers_and_float_spelling(self):
        value = self.sample(3)

        def extend(node):
            if isinstance(node, dict):
                for child in list(node.values()):
                    extend(child)
                node["vendor:future"] = {
                    "nested": [None, True, 1.0, -0.0, 2**80, -(2**80), 1e-20, "Café😀"],
                    "$serde_json::private::Number": "123",
                }
            elif isinstance(node, list):
                for child in node:
                    extend(child)

        extend(value)
        self.seal(value)
        raw = encode(value)
        self.assertEqual(
            self.native("decode", {"raw": raw}), document_value(decode_document(raw))
        )
        self.assertEqual(
            self.native("encode", {"value": value})["encoded"],
            encode_document(validate_document(value)),
        )
        # JSON's numeric spellings are normalized using Python numeric values,
        # while integers beyond 64 bits remain exact in unknown extension data.
        unbounded = self.sample()
        unbounded["content"]["database_schema"] = 10**400
        self.seal(unbounded)
        self.assertEqual(
            self.native("validate", {"value": unbounded}),
            document_value(validate_document(unbounded)),
        )
        for spelling in ("1e2", "1.000", "-0", "1E-07", "-0.00"):
            value = self.sample()
            value["vendor:number"] = json.loads(spelling)
            raw = encode(value).replace(
                '"vendor:number": ' + encode(value["vendor:number"]),
                '"vendor:number": ' + spelling,
            )
            native = self.native("decode", {"raw": raw})
            expected = document_value(decode_document(raw))
            self.assertEqual(encode(native), encode(expected))

    def test_required_fields_strict_types_and_bounded_collections(self):
        value = self.sample(3)
        for path in (
            ("generator",),
            ("content",),
            ("content", "catalog"),
            ("content", "counts"),
            ("content", "items", 0),
            ("content", "files", 0),
            ("content", "associations", 0),
            ("content", "entries", 0),
        ):
            original = value
            for key in path:
                original = original[key]
            # The original sample contains the complete required model fields.
            for key in original:
                changed = copy.deepcopy(value)
                node = changed
                for step in path:
                    node = node[step]
                del node[key]
                with self.subTest(path=path, key=key):
                    self.error(changed)
        mutations = [
            lambda v: v.update(format_version=True),
            lambda v: v["content"].update(database_schema="32"),
            lambda v: v["content"].update(database_schema=0),
            lambda v: v["content"]["files"][0].update(size=100),
            lambda v: v["content"]["files"][0].update(size="01"),
            lambda v: v["content"]["files"][0].update(mtime_ns="+1"),
            lambda v: v["content"]["files"][0].update(status="other"),
            lambda v: v["content"]["associations"][0].update(active=1),
            lambda v: v["content"]["associations"][0].update(part=False),
            lambda v: v["content"]["items"][0].update(metadata=[]),
            lambda v: v["content"].update(catalog=[]),
            lambda v: v["content"].update(files="bad"),
            lambda v: v["content"].update(layout=True),
            lambda v: v["content"].update(items=[None] * 100001),
        ]
        for change in mutations:
            value = self.sample(3)
            change(value)
            self.error(value)

    def test_checksum_references_duplicate_ids_and_link_comparisons(self):
        mutations = [
            lambda c: c["entries"][0].update(item_id="absent"),
            lambda c: c["entries"][0].update(association_ids=[]),
            lambda c: c["entries"][0].update(
                association_ids=[c["entries"][0]["association_ids"][0]] * 2
            ),
            lambda c: c["entries"][0].update(managed_by_layout="absent"),
            lambda c: c["relationships"][0].update(target_id="absent"),
            lambda c: c["relationships"][0].update(
                target_id=c["relationships"][0]["source_id"]
            ),
            lambda c: c["counts"].update(items=100),
            lambda c: c["items"].append(copy.deepcopy(c["items"][0])),
            lambda c: c["associations"][0].update(active=False),
            lambda c: c.update(filesystem_verified=True),
            lambda c: c["entries"][0].update(recorded_link_matches_desired=True),
        ]
        for change in mutations:
            value = self.sample()
            change(value["content"])
            self.error(value)
        value = self.sample()
        value["content"]["extra"]["edited"] = True
        self.error(value, seal=False)
        value = self.sample()
        definition = {"vendor:opaque-query": "never execute"}
        checksum = digest(definition)
        value["content"]["layout"] = {
            "name": "layout",
            "current_definition": definition,
            "current_definition_sha256": checksum,
            "last_applied_definition_sha256": checksum,
            "definition_matches_last_apply": True,
        }
        self.seal(value)
        self.native("validate", {"value": value})
        value["content"]["layout"]["current_definition"]["edit"] = True
        self.error(value)

    def tagged(self):
        value = self.sample(3)
        content = value["content"]
        content["tags"] = [
            {"id": "tag", "name": "genre:science fiction", "description": ""},
            {"id": "parent", "name": "fiction", "description": ""},
        ]
        content["tag_names"] = [
            {"name": "genre:science fiction", "tag_id": "tag"},
            {"name": "fiction", "tag_id": "parent"},
            {"name": "scifi", "tag_id": "tag"},
        ]
        content["tag_parents"] = [{"child_id": "tag", "parent_id": "parent"}]
        content["taggings"] = [
            {
                "id": "assertion",
                "subject_type": "item",
                "subject_id": content["items"][0]["id"],
                "tag_id": "tag",
                "source": "manual",
                "confidence": 0.5,
                "note": "",
                "active": False,
                "created_at": "t",
                "updated_at": "t",
            }
        ]
        for key in ("tags", "tag_names", "tag_parents", "taggings"):
            content["counts"][key] = len(content[key])
        return self.seal(value)

    def test_tag_hierarchy_withdrawn_assertions_and_strict_confidence(self):
        value = self.tagged()
        self.assertEqual(
            self.native("validate", {"value": value}),
            document_value(validate_document(value)),
        )
        changes = [
            lambda c: c["tag_names"][0].update(name="Genre:Science Fiction"),
            lambda c: c["tag_names"][0].update(tag_id="missing"),
            lambda c: c["tags"][0].update(name="absent"),
            lambda c: c["tags"][0].update(description="\0"),
            lambda c: c["tag_parents"].append(
                {"child_id": "parent", "parent_id": "tag"}
            ),
            lambda c: c["tag_parents"].append(copy.deepcopy(c["tag_parents"][0])),
            lambda c: c["taggings"][0].update(subject_id="missing"),
            lambda c: c["taggings"][0].update(source="\0"),
            lambda c: c["taggings"][0].update(confidence=True),
            lambda c: c["taggings"][0].update(confidence=[]),
            lambda c: c["taggings"][0].update(confidence=2),
            lambda c: c["taggings"][0].update(confidence=10**400),
        ]
        for change in changes:
            value = self.tagged()
            change(value["content"])
            for key in ("tags", "tag_names", "tag_parents", "taggings"):
                value["content"]["counts"][key] = len(value["content"][key])
            self.error(value)

    def test_hardlink_ownership_is_separate_from_symlink_targets(self):
        value = self.sample(3)
        content = value["content"]
        content["hardlinks"] = [
            {
                "path": "x",
                "file_id": "old-file",
                "location": "disk",
                "source_path": "source",
                "device": str(2**80),
                "inode": "1",
            }
        ]
        content["counts"]["hardlinks"] = 1
        self.error(value)
        content["catalog"]["link_mode"] = "hardlink"
        content["recorded_links"] = [{"path": "old", "target": "../source"}]
        content["counts"]["recorded_links"] = 1
        self.error(value)
        content["recorded_links"] = []
        content["counts"]["recorded_links"] = 0
        for entry in content["entries"]:
            for key in (
                "expected_link_target",
                "recorded_link_target",
                "recorded_link_matches_desired",
            ):
                entry[key] = None
        self.seal(value)
        self.assertEqual(
            self.native("validate", {"value": value}),
            document_value(validate_document(value)),
        )

    def test_decoder_duplicate_keys_nonfinite_and_invalid_json(self):
        raw = encode(self.sample())
        samples = [
            raw.replace(
                '"format_version": 1', '"format_version": 1, "format_version": 1'
            ),
            raw.replace('"extra": {}', '"extra": {"x": 1, "\\u0078": 2}'),
            raw + "x",
            raw[:-1],
            raw.replace('"extra": {}', '"extra": {"x": 1,}'),
        ]
        for token in ("NaN", "Infinity", "-Infinity", "1e9999"):
            samples.append(raw.replace('"extra": {}', '"extra": {"x": ' + token + "}"))
        for broken in samples:
            with self.subTest(raw=broken[-90:]):
                with self.assertRaises(CatabolicError) as expected:
                    decode_document(broken)
                self.assertEqual(
                    self.native("decode", {"raw": broken}, ok=False),
                    str(expected.exception),
                )
