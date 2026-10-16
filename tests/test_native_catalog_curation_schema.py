# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Reproducibility and additive schema34 checks; not independent review."""

import json
import unittest

from scripts import native_catalog_curation_schema as curation
from scripts import native_catalog_schema as foundation


class NativeCurationSchemaTests(unittest.TestCase):
    def test_generated_resources_are_exact(self):
        for path, text in curation.generate().items():
            with self.subTest(path=str(path)):
                self.assertEqual((curation.REPO / path).read_text(), text)

    def test_frozen_signature_is_preserved_as_exact_subset(self):
        old = json.loads((curation.REPO / foundation.SIGNATURE_PATH).read_text())
        current = json.loads((curation.REPO / curation.SIGNATURE).read_text())
        lookup = {(row[0], row[1]): row for row in current}
        for row in old:
            self.assertEqual(row, lookup[row[0], row[1]])

    def test_populated_legacy_evidence_bootstrap_preserves_generation(self):
        db = foundation.legacy_db()
        db.execute("INSERT INTO items VALUES ('one','movie','{}')")
        db.execute("INSERT INTO identities VALUES ('provider','value','one')")
        db.execute("PRAGMA recursive_triggers=ON")
        db.executescript((curation.REPO / foundation.SQL_PATH).read_text())
        before = db.execute("SELECT * FROM catalog_state").fetchall()
        old_index = db.execute(
            "SELECT * FROM catalog_entity_revisions ORDER BY 1,2"
        ).fetchall()
        db.executescript((curation.REPO / curation.SQL).read_text())
        self.assertEqual(before, db.execute("SELECT * FROM catalog_state").fetchall())
        self.assertEqual(
            old_index,
            db.execute(
                "SELECT * FROM catalog_entity_revisions WHERE entity_kind <> 'catalog_legacy_identity_evidence' ORDER BY 1,2"
            ).fetchall(),
        )
        self.assertEqual(
            [("provider", "value", "one")],
            db.execute(
                "SELECT namespace,value,item_id FROM catalog_legacy_identity_evidence"
            ).fetchall(),
        )
        self.assertEqual(
            [('["provider","value","one"]', 1, 0)],
            db.execute(
                "SELECT entity_key,revision,deleted FROM catalog_entity_revisions WHERE entity_kind='catalog_legacy_identity_evidence'"
            ).fetchall(),
        )
        self.assertEqual([], db.execute("PRAGMA foreign_key_check").fetchall())
        db.close()


if __name__ == "__main__":
    unittest.main()
