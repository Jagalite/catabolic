# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import contextlib
import json
import sqlite3
import tempfile
import unittest
from dataclasses import replace
from pathlib import Path

from catabolic.domain import CatabolicError
from catabolic.migration import load_migrations, upgrade_database, validate_preservation
from catabolic.saved_queries import Queries
from catabolic.store import Store
from tests.test_migrations import contents, create_legacy


class ProgrammableMigrationTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.path = create_legacy(Path(temporary.name))
        upgrade_database(self.path, migrations=load_migrations()[:14])
        selection = json.dumps(
            {
                "language": "sql",
                "query": "SELECT item_id FROM catalog_items",
                "max_ids": 100000,
                "page_size": 1000,
                "timeout_ms": 60000,
            }
        )
        with contextlib.closing(sqlite3.connect(self.path)) as db, db:
            db.execute("PRAGMA foreign_keys=ON")
            db.execute("INSERT INTO generated_locations VALUES ('default','media')")
            db.execute(
                "INSERT INTO processing_recipes(id,name,revision,preset,definition,digest) VALUES ('recipe','recipe',1,'thumbnail','{}','recipe-digest')"
            )
            db.execute(
                "INSERT INTO processing_rules(id,profile,name,revision,recipe_id,location,selection,estimate_options,required,enabled,digest) VALUES ('rule','default','rule',1,'recipe','media',?,'{}',1,1,'rule-digest')",
                (selection,),
            )
            db.execute(
                "INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,recipe_id) VALUES ('job','default','file-a','render','media','{}','{}','cache','recipe')"
            )
            db.execute(
                "INSERT INTO rule_jobs(rule_id,job_id,file_id,item_id) VALUES ('rule','job','file-a','item-a')"
            )
            db.execute(
                "INSERT INTO rule_evaluations(id,rule_id,matched_inputs,counts) VALUES ('evaluation','rule',1,'{}')"
            )
            db.execute(
                "INSERT INTO rule_requirements(id,rule_id,profile,file_id,item_id,evaluation_id,job_id) VALUES ('required','rule','default','file-a','item-a','evaluation','job')"
            )
            for suffix in ("a", "b"):
                db.execute(
                    "INSERT INTO meta VALUES (?,?)",
                    (
                        "layout:" + "long-name" * 6 + suffix,
                        json.dumps(
                            {
                                "version": 1,
                                "selection": json.loads(selection),
                                "rules": [],
                            }
                        ),
                    ),
                )
        self.before = contents(self.path)

    def test_schema14_preserves_all_original_columns_ids_and_foreign_keys(self):
        result = upgrade_database(self.path)
        self.assertEqual(result["schema"], 16)
        with Store(self.path, writable=True) as store:
            validate_preservation(store.db, self.before)
            self.assertEqual(store.rows("PRAGMA foreign_key_check"), [])
            self.assertEqual(
                store.rows(
                    "SELECT operation_kind FROM processing_recipes WHERE id='recipe'"
                )[0]["operation_kind"],
                "render",
            )
            query = Queries(store).get("legacy-rule-rule")
            self.assertEqual(query["definition"]["max_ids"], 100000)
            self.assertEqual(query["definition"]["timeout_ms"], 60000)
            self.assertEqual(
                Queries(store).put(query["name"], query["definition"])["id"],
                query["id"],
            )
            self.assertEqual(len(Queries(store).list()["queries"]), 3)
            self.assertEqual(
                store.rows("SELECT job_id FROM rule_requirements WHERE id='required'"),
                [{"job_id": "job"}],
            )

    def test_failed_table_rebuild_preserves_original_database(self):
        migrations = load_migrations()
        broken = replace(
            migrations[-1],
            sql=migrations[-1].sql + "\nDELETE FROM files WHERE id='file-a';",
        )
        with self.assertRaises(CatabolicError):
            upgrade_database(self.path, migrations=(*migrations[:-1], broken))
        self.assertEqual(contents(self.path), self.before)
        with contextlib.closing(sqlite3.connect(self.path)) as db, db:
            self.assertEqual(db.execute("PRAGMA user_version").fetchone()[0], 14)
