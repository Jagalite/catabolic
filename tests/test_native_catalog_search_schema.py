"""Implementation reproducibility fixtures, not independent schema review."""

import unittest

from scripts import native_catalog_search_schema as schema


class NativeSearchSchemaTests(unittest.TestCase):
    def test_all_generated_resources_match(self):
        for path, text in schema.generate().items():
            with self.subTest(path=str(path)):
                self.assertEqual((schema.REPO / path).read_bytes(), text.encode())

    def test_historical_schema_is_preserved_and_new_index_is_not_ready(self):
        # generate validates exact historical DDL, FK consistency and empty
        # not-ready state before it returns the versioned signature.
        resources = schema.generate()
        self.assertIn(schema.SIGNATURE_PATH, resources)
        self.assertIn("catalog_search_dirty", resources[schema.SIGNATURE_PATH])
        self.assertIn("catalog_search_fts", resources[schema.SIGNATURE_PATH])


if __name__ == "__main__":
    unittest.main()
