# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""New v2 folder-preview adapter, paired with independent frozen Python.

Only disposable metadata, empty output/source roots and read-only reconciliation.
No publication, source-race probing, broad owner review, service, or fallback.
"""

import copy
import hashlib
import json
import os
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
from contextlib import closing
from pathlib import Path

from tests.test_rust_legacy_review import BINARY, ORACLE, PYTHON, REFERENCE

SEED = r"""
from catabolic.store import Store
from catabolic.app import Application
from catabolic.saved_queries import Queries
import json, sys
with Store(sys.argv[1], writable=True) as store:
    app = Application(store)
    app.bind('output', 'global', sys.argv[2])
    with store.transaction() as db:
        db.execute("INSERT INTO locations(id) VALUES ('offline')")
        for n in range(3):
            db.execute("INSERT INTO files(id,location,path) VALUES (?,'offline',?)", (f'f{n}', f'book{n}.epub'))
            db.execute("INSERT INTO items(id,kind,metadata) VALUES (?,'book',?)", (f'i{n}', json.dumps({'title':f'Title {n} Café','author':'Author'})))
            db.execute("INSERT INTO item_files(id,file_id,item_id,role,origin) VALUES (?,?,?,'primary','explicit')", (f'a{n}',f'f{n}',f'i{n}'))
    app.bind('source', 'offline', sys.argv[3])
    with store.transaction() as db:
        db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors,finished_at) VALUES ('fixture','default','offline',1,0,'[]','2026-01-01 00:00:00')")
        db.execute("INSERT INTO observations(profile,file_id,size,mtime_ns,device,inode,status,scan_id) SELECT 'default',id,0,0,0,0,'missing','fixture' FROM files")
    q = Queries(store).put('selected', {'mode':'selection','selection':{'language':'sql','query':'SELECT item_id FROM catalog_items'}})
    print(q['id'])
"""


def typed(value):
    if isinstance(value, dict):
        return {key: typed(child) for key, child in value.items()}
    if isinstance(value, list):
        return [typed(child) for child in value]
    return [type(value).__name__, value]


class PublicationMappingPreviewTest(unittest.TestCase):
    def setUp(self):
        for path in (PYTHON, REFERENCE / "catabolic/publication_mappings.py", BINARY):
            self.assertTrue(path.is_file(), str(path))
        temporary = tempfile.TemporaryDirectory(prefix="publication-preview-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.db = {True: self.root / "python.db", False: self.root / "native.db"}
        self.source, self.output = self.root / "source", self.root / "output"
        self.source.mkdir()
        self.output.mkdir()
        self.env = {
            k: v
            for k, v in os.environ.items()
            if not k.startswith("CATABOLIC_") and k not in ("PYTHONPATH", "PYTHONHOME")
        }
        self.env.update(
            HOME=str(self.root / "home"),
            COLUMNS="80",
            TERM="dumb",
            NO_COLOR="1",
            PYTHONUTF8="1",
            PYTHONHASHSEED="0",
        )
        result = self.cli(True, ["init"])
        self.assertEqual(result.returncode, 0, result.stderr)
        result = self.python(
            SEED, str(self.db[True]), str(self.output), str(self.source)
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.definition = {
            "version": 2,
            "id": "books-café",
            "query": result.stdout.strip(),
            "destination": {
                "adapter": "komga",
                "operation": "folder",
                "catalog": "global",
            },
        }
        shutil.copyfile(self.db[True], self.db[False])

    def python(self, code, *args):
        return subprocess.run(
            [
                str(PYTHON),
                "-c",
                "import sys;sys.path.insert(0,sys.argv.pop(1));" + code,
                str(REFERENCE),
                *args,
            ],
            env=self.env,
            text=True,
            capture_output=True,
            timeout=45,
        )

    def cli(self, oracle, args):
        if "--machine" in args:
            args = ["--machine", *(value for value in args if value != "--machine")]
        command = (
            [str(PYTHON), "-c", ORACLE, str(REFERENCE)] if oracle else [str(BINARY)]
        )
        return subprocess.run(
            command + ["--db", str(self.db[oracle])] + args,
            env=self.env,
            text=True,
            capture_output=True,
            timeout=45,
        )

    def write(self, definition):
        path = self.root / "definition.json"
        path.write_text(json.dumps(definition, ensure_ascii=False))
        return str(path)

    def snapshot(self):
        state = {}
        for flag, path in self.db.items():
            with closing(sqlite3.connect(path)) as db:
                state[str(flag)] = list(db.iterdump())
        for name, root in [("output", self.output), ("source", self.source)]:
            state[name] = [
                (
                    str(p.relative_to(root)),
                    p.lstat().st_mode,
                    hashlib.sha256(p.read_bytes()).hexdigest() if p.is_file() else "",
                )
                for p in root.rglob("*")
            ]
        return state

    def normalize(self, value):
        for path in self.db.values():
            value = value.replace(str(path), "<DB>")
        return value

    def pair(self, definition=None, *, options=()):
        path = self.write(self.definition if definition is None else definition)
        before = self.snapshot()
        old, new = (
            self.cli(
                flag,
                [
                    "projection",
                    "mapping-preview",
                    "--definition",
                    path,
                    "--machine",
                    *options,
                ],
            )
            for flag in (True, False)
        )
        self.assertEqual(
            new.returncode,
            old.returncode,
            (old.stdout, old.stderr, new.stdout, new.stderr),
        )
        self.assertEqual(self.normalize(new.stderr), self.normalize(old.stderr))
        a, b = json.loads(old.stdout), json.loads(new.stdout)
        self.assertEqual(typed(b), typed(a))
        self.assertEqual(before, self.snapshot())
        return b.get("data", b)

    def sql(self, statement, values=()):
        for path in self.db.values():
            with closing(sqlite3.connect(path)) as db:
                db.execute(statement, values)
                db.commit()

    def query(self, query, *, rows=False, definition=None):
        code = """
from catabolic.store import Store
from catabolic.saved_queries import Queries
import sys,json
with Store(sys.argv[1],writable=True) as store:
    q=Queries(store).put('revision',json.loads(sys.argv[2]))
    print(q['id'])
"""
        definition = definition or (
            {"mode": "selection", "selection": {"language": "sql", "query": query}}
            if not rows
            else {"mode": "rows", "language": "sql", "query": query}
        )
        result = self.python(code, str(self.db[True]), json.dumps(definition))
        self.assertEqual(result.returncode, 0, result.stderr)
        shutil.copyfile(self.db[True], self.db[False])
        return result.stdout.strip()

    def test_populated_complete_plan_and_repeat_rollback(self):
        first = self.pair()
        self.assertEqual(
            first["plan"]["data"]["layout"]["selection_statistics"]["files"], 3
        )
        self.assertEqual(len(first["plan"]["data"]["state"]["mappings"]), 3)
        self.assertEqual(first["plan"]["data"]["layout"]["applied"], True)
        self.assertEqual(first["applied"], False)
        self.assertEqual(self.pair(), first)

    def test_all_seventeen_presets_use_complete_selection(self):
        presets = json.loads(
            (
                Path(__file__).resolve().parents[1]
                / "crates/catabolic-store/resources/targets.json"
            ).read_text()
        )["definitions"]
        self.assertEqual(len(presets), 17)
        for adapter in presets:
            with self.subTest(adapter=adapter):
                definition = copy.deepcopy(self.definition)
                definition["destination"]["adapter"] = adapter
                self.pair(definition)

    def test_definition_and_option_scalar_admission(self):
        cases = []
        for value in (2.0, True, "2"):
            d = copy.deepcopy(self.definition)
            d["version"] = value
            # Only numeric 2 routes to v2 in frozen dispatch; other versions
            # belong to the separate v1 owner and are outside this adapter.
            if value == 2.0:
                cases.append(d)
        for update in ({"id": ""}, {"query": False}, {"extra": 1}):
            cases.append({**self.definition, **update})
        for update in (
            {"allow_empty": 1},
            {"extra": 1},
            {"adapter": "unknown"},
            {"catalog": ""},
            {"operation": "metadata"},
        ):
            d = copy.deepcopy(self.definition)
            d["destination"].update(update)
            cases.append(d)
        for definition in cases:
            with self.subTest(definition=definition):
                self.pair(definition)

    def test_unknown_query_catalog_and_foreign_projection_errors(self):
        definition = copy.deepcopy(self.definition)
        definition["query"] = "absent"
        self.pair(definition)
        definition = copy.deepcopy(self.definition)
        definition["destination"]["catalog"] = "absent"
        self.pair(definition)
        self.sql(
            "INSERT INTO projection_bindings(profile,catalog,query_id,layout) VALUES ('default','global',?,'foreign')",
            (self.definition["query"],),
        )
        self.pair()

    def test_query_revision_and_definition_options_pin_plan(self):
        first = self.pair()
        changed = copy.deepcopy(self.definition)
        changed["destination"]["allow_empty"] = False
        self.assertNotEqual(self.pair(changed)["plan_id"], first["plan_id"])
        changed = copy.deepcopy(self.definition)
        changed["query"] = self.query(
            "SELECT item_id FROM catalog_items WHERE item_id='i1'"
        )
        second = self.pair(changed)
        self.assertNotEqual(second["plan_id"], first["plan_id"])
        self.assertEqual(len(second["plan"]["data"]["state"]["mappings"]), 1)
        self.assertEqual(self.pair(changed)["plan_id"], second["plan_id"])

    def test_complete_file_association_and_paginated_graphql_selections(self):
        selections = [
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT file_id FROM catalog_files WHERE file_id='f1'",
                }
            },
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT association_id FROM catalog_item_files WHERE association_id='a1'",
                }
            },
            {
                "selection": {
                    "language": "graphql",
                    "query": "query Books($after: String, $kind: String!) { chosen: items(kind: $kind, first: 1, after: $after) { nodes { id } pageInfo { hasNextPage endCursor } } }",
                    "variables": {"kind": "book"},
                }
            },
        ]
        for definition, expected in zip(selections, (1, 1, 3), strict=True):
            with self.subTest(definition=definition):
                self.definition["query"] = self.query("", definition=definition)
                result = self.pair()
                self.assertEqual(
                    len(result["plan"]["data"]["state"]["mappings"]), expected
                )
        self.assertEqual(result["plan"]["data"]["layout"]["selection"]["pages"], 3)

    def test_empty_query_guard_and_allow_empty_plans_rollback(self):
        self.definition["query"] = self.query(
            "SELECT item_id FROM catalog_items WHERE 0"
        )
        self.pair()
        self.definition["destination"]["allow_empty"] = True
        result = self.pair()
        self.assertEqual(result["safe"], True)
        self.assertEqual(result["removals"], 0)

    def test_pending_history_order_and_other_profile_isolation(self):
        event = {
            "version": 2,
            "owner": "other",
            "key": "global",
            "scope": ["folder", "global"],
            "state": "uncertain",
            "plan_id": "plan",
        }
        self.sql(
            "INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES ('event-123','default','mapping_publish','info',?)",
            (json.dumps(event),),
        )
        result = self.pair()
        self.assertEqual(
            result["error"]["code"], "mapping_unresolved_write_recover_event-123"
        )
        self.sql("UPDATE consumer_events SET profile='elsewhere'")
        self.pair()

    def test_nonfolder_preview_admission_and_apply_recover_no_effects(self):
        before = self.snapshot()
        for adapter, operation in [
            ("nfo", "export"),
            ("calibre", "import"),
            ("immich", "import"),
        ]:
            definition = copy.deepcopy(self.definition)
            definition["destination"].update(
                adapter=adapter,
                operation=operation,
                path=str(self.root / "destination"),
            )
            result = self.pair(definition)
            self.assertIn(
                "mapping selection includes associations outside the source catalog",
                str(result),
            )
            self.assertFalse((self.root / "destination").exists())
        # Folder apply is open (track C4); an unreviewed plan is refused as
        # stale in both engines before any effect.
        old, new = (
            self.cli(
                flag,
                [
                    "projection",
                    "mapping-apply",
                    "--definition",
                    self.write(self.definition),
                    "--expected-plan",
                    "unreviewed",
                    "--machine",
                ],
            )
            for flag in (True, False)
        )
        self.assertEqual(new.returncode, 2, new.stdout)
        self.assertEqual(self.normalize(new.stdout), self.normalize(old.stdout))
        self.assertIn("stale_plan_preview_again", new.stdout)
        self.assertEqual(before, self.snapshot())
        event = {
            "version": 2,
            "owner": "books",
            "key": "global",
            "scope": ["folder", "global"],
            "state": "uncertain",
            "plan_id": "plan",
        }
        self.sql(
            "INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES ('publication','default','mapping_publish','info',?)",
            (json.dumps(event),),
        )
        before = self.snapshot()
        # This hand-written event has no database_id. Frozen recovery raises
        # KeyError on it; native refuses it as a structured, documented
        # safety difference, with nothing written.
        for options in ([], ["--apply"]):
            result = self.cli(
                False,
                ["projection", "mapping-recover", "publication", "--machine", *options],
            )
            self.assertEqual(result.returncode, 2, result.stdout)
            self.assertIn("mapping_event_database_changed", result.stdout)
        self.assertEqual(before, self.snapshot())


if __name__ == "__main__":
    unittest.main()
