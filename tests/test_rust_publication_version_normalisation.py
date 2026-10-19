# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Controls for the version-only normalisation in the bundle-preview pairing.

The oracle stamps manifest version 0.2.0 and the native engine stamps its crate
version, so version-derived plan digests differ. The pairing compares the
native result restamped to the oracle version with the digests recomputed. These
controls prove that this normalisation absorbs the version and nothing else.
No engine or binary runs here.
"""

import copy
import unittest

from tests.test_rust_publication_bundle_preview import (
    ORACLE_VERSION,
    attest,
    digest,
    export_signature,
    native_version,
    plan_digest,
    rebase,
    rebase_envelope,
    stable,
    typed,
)


def make_result(version, *, entries=("a.epub", "b.epub")):
    document = {
        "format": "catabolic.catalog-manifest",
        "format_version": 3,
        "generator": {"name": "catabolic", "version": version},
        "generated_at": "2026-10-18T00:00:00+00:00",
        "content": {"entries": [{"path": p, "file_id": "f" + p[0]} for p in entries]},
    }
    data = {
        "document": document,
        "export": {"files": [{"path": "x.nfo", "content": "toy"}]},
        "sources": {"fa": {"path": "a.epub"}},
        "output": "/toy/out",
    }
    plan = {
        "document": copy.deepcopy(document),
        "data": data,
        "safe": True,
        "removals": 0,
        "unchanged": False,
        "scope": ["export", "/toy/out"],
    }
    plan["signature"] = export_signature(plan)
    return {
        "plan_id": plan_digest(plan),
        "mapping": "m",
        "plan": plan,
        "applied": False,
    }


class VersionNormalisationControls(unittest.TestCase):
    def comparable(self, native):
        return typed(stable(rebase_envelope({"data": native}, ORACLE_VERSION)))

    def test_exact_versions_come_from_the_engines(self):
        self.assertEqual(ORACLE_VERSION, "0.2.0")
        self.assertRegex(native_version(), r"^\d+\.\d+\.\d+$")
        self.assertNotEqual(native_version(), ORACLE_VERSION)
        oracle = make_result(ORACLE_VERSION)
        native = make_result(native_version())
        attest(self, oracle, ORACLE_VERSION)
        attest(self, native, native_version())
        with self.assertRaises(AssertionError):
            attest(self, native, ORACLE_VERSION)
        with self.assertRaises(AssertionError):
            attest(self, oracle, native_version())

    def test_version_only_difference_is_absorbed_with_recomputed_digests(self):
        oracle = make_result(ORACLE_VERSION)
        native = make_result(native_version())
        self.assertNotEqual(oracle["plan_id"], native["plan_id"])
        self.assertNotEqual(oracle["plan"]["signature"], native["plan"]["signature"])
        self.assertEqual(self.comparable(native), typed(stable({"data": oracle})))

    def test_any_other_difference_still_fails(self):
        oracle = make_result(ORACLE_VERSION)
        mutations = {
            "plan field": lambda r: r["plan"].update(removals=1),
            "plan nested": lambda r: r["plan"]["data"]["sources"]["fa"].update(
                path="other.epub"
            ),
            "document content": lambda r: r["plan"]["data"]["document"]["content"][
                "entries"
            ][0].update(path="z.epub"),
            "top-level field": lambda r: r.update(mapping="other"),
            "generator name": lambda r: r["plan"]["document"]["generator"].update(
                name="other"
            ),
            "format_version": lambda r: r["plan"]["document"].update(format_version=4),
            "type change": lambda r: r["plan"].update(removals=False),
        }
        for label, mutate in mutations.items():
            with self.subTest(label=label):
                # Mutated after the digests were computed: the engine's own
                # claimed digests no longer match its plan.
                broken = make_result(native_version())
                mutate(broken)
                if label not in ("top-level field", "type change"):
                    with self.assertRaises(AssertionError):
                        attest(self, broken, native_version())
                # Mutated with honestly recomputed digests: the digests agree
                # with the plan, but the pairing comparison must still differ.
                honest = make_result(native_version())
                mutate(honest)
                if "signature" in honest["plan"]:
                    honest["plan"]["signature"] = export_signature(honest["plan"])
                honest["plan_id"] = plan_digest(honest["plan"])
                self.assertNotEqual(
                    self.comparable(honest), typed(stable({"data": oracle}))
                )

    def test_digest_tampering_is_not_hidden_by_recomputation(self):
        native = make_result(native_version())
        for key in ("plan_id",):
            with self.subTest(key=key):
                tampered = copy.deepcopy(native)
                tampered[key] = digest("forged")
                with self.assertRaises(AssertionError):
                    attest(self, tampered, native_version())
        tampered = copy.deepcopy(native)
        tampered["plan"]["signature"] = digest("forged")
        with self.assertRaises(AssertionError):
            attest(self, tampered, native_version())

    def test_rebase_changes_only_version_and_dependent_digests(self):
        native = make_result(native_version())
        restamped = rebase(native, ORACLE_VERSION)
        expected = copy.deepcopy(native)
        for document in (
            expected["plan"]["document"],
            expected["plan"]["data"]["document"],
        ):
            document["generator"]["version"] = ORACLE_VERSION
        expected["plan"]["signature"] = export_signature(expected["plan"])
        expected["plan_id"] = plan_digest(expected["plan"])
        self.assertEqual(restamped, expected)
        attest(self, restamped, ORACLE_VERSION)
        self.assertEqual(
            native["plan"]["data"]["document"]["generator"]["version"],
            native_version(),
        )


if __name__ == "__main__":
    unittest.main()
