# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Named legacy transport scenarios, independent of parser-only qualification.

The oracle is a fresh pinned process. Catalogs and any files are disposable,
independent copies. No network, real credentials, or user catalogs are used.
"""

import json
import os
import shutil
import signal
import sqlite3
import subprocess
import tempfile
import time
import tomllib
import unittest
from contextlib import closing
from pathlib import Path

from tests.test_rust_legacy_parser import PYTHON, REFERENCE, environment

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(
    os.environ.get(
        "CATABOLIC_LEGACY_BINARY", ROOT / "target/debug/catabolic-legacy-native"
    )
)
ORACLE = "import sys;sys.path.insert(0,sys.argv.pop(1));from catabolic.cli import main;raise SystemExit(main(sys.argv[1:]))"


class NativeLegacyCliTest(unittest.TestCase):
    def setUp(self):
        self.assertTrue(
            BINARY.is_file(), "build catabolic-legacy-native before qualification"
        )
        self.assertTrue((REFERENCE / "src/catabolic/cli.py").is_file())
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.reference = self.root / "reference.sqlite3"
        self.native = self.root / "native.sqlite3"
        self.env = environment()
        self.env["HOME"] = str(self.root / "home")
        run = self.run_process(True, ["--db", str(self.reference), "init"])
        self.assertEqual(run.returncode, 0, run.stderr)
        shutil.copyfile(self.reference, self.native)

    def run_process(self, reference, args, stdin=""):
        command = (
            [str(PYTHON), "-c", ORACLE, str(REFERENCE / "src")]
            if reference
            else [str(BINARY)]
        )
        return subprocess.run(
            command + args,
            input=stdin,
            capture_output=True,
            text=True,
            env=self.env,
            timeout=90,
        )

    def normalized(self, text):
        return text.replace(str(self.reference), "<DB>").replace(
            str(self.native), "<DB>"
        )

    def compare(self, scenario, args, *, database=True, semantic=False, stdin=""):
        with self.subTest(scenario=scenario):
            oracle = self.run_process(
                True, (["--db", str(self.reference)] if database else []) + args, stdin
            )
            native = self.run_process(
                False, (["--db", str(self.native)] if database else []) + args, stdin
            )
            self.assertEqual(
                native.returncode,
                oracle.returncode,
                (scenario, oracle.stdout, oracle.stderr, native.stdout, native.stderr),
            )
            self.assertEqual(
                self.normalized(native.stderr), self.normalized(oracle.stderr), scenario
            )
            if semantic:
                self.assertEqual(
                    json.loads(self.normalized(native.stdout)),
                    json.loads(self.normalized(oracle.stdout)),
                    scenario,
                )
            else:
                self.assertEqual(
                    self.normalized(native.stdout),
                    self.normalized(oracle.stdout),
                    scenario,
                )
            return oracle, native

    def test_version_reports_this_release_not_the_frozen_reference(self):
        # The one intentional output difference from the frozen reference.
        release = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"][
            "package"
        ]["version"]
        oracle = self.run_process(True, ["--version"])
        native = self.run_process(False, ["--version"])
        self.assertEqual(
            (oracle.returncode, oracle.stdout, oracle.stderr),
            (0, "catabolic 0.2.0\n", ""),
        )
        self.assertEqual(
            (native.returncode, native.stdout, native.stderr),
            (0, f"catabolic {release}\n", ""),
        )

    def test_offline_exact_output(self):
        cases = [
            ("LC-OFF-002", ["docs"]),
            ("LC-OFF-003", ["docs", "trust"]),
            ("LC-OFF-004", ["docs", "--search", "source trust"]),
            ("LC-OFF-005", ["docs", "--search", "no-such-topic-unique"]),
            ("LC-OFF-006", ["docs", "missing-topic"]),
            ("LC-OFF-007", ["spec", "docs", "--format-version", "1"]),
            ("LC-OFF-008", ["spec", "schema", "--format-version", "2"]),
            ("LC-OFF-009", ["--machine", "spec", "check"]),
            ("LC-OFF-010", ["--machine", "query", "contract"]),
        ]
        for scenario, args in cases:
            self.compare(scenario, args, database=False)

    def test_missing_database_error_channels(self):
        for n, args in enumerate(
            [
                ["status"],
                ["--json", "status"],
                ["--machine", "status"],
                ["query", "SELECT 1"],
                ["--machine", "query", "SELECT 1"],
                ["consumer", "connections"],
                ["--machine", "consumer", "connections"],
                ["--machine", "processor", "jobs", "x"],
                ["graphql", "{ profiles { id } }"],
            ]
        ):
            self.compare(f"LC-ERR-{n:03}", args, database=False)

    def test_catalog_read_only_transport(self):
        for n, args in enumerate(
            [
                ["status"],
                ["--machine", "status"],
                ["--json", "profile", "list"],
                ["--json", "location", "list"],
                ["--json", "catalog", "list"],
                ["--json", "files"],
                ["--json", "item", "list"],
                ["--json", "tag", "list"],
                ["--json", "tag", "assignments"],
                ["--json", "association", "list"],
                ["--json", "relationship", "list"],
                ["--json", "mapping", "list"],
                ["--json", "artifact", "list"],
                ["--json", "artifact", "recipes"],
                ["--json", "rendition", "list"],
                ["--json", "rendition", "definitions"],
                ["--json", "process", "list"],
                ["--json", "process", "facts", "missing"],
                ["--json", "content", "duplicates"],
                ["--machine", "consumer", "metadata-matches"],
                ["--json", "catalog-refresh", "pending"],
                ["--json", "watcher", "list"],
                ["--json", "watcher", "pending"],
            ]
        ):
            self.compare(f"LC-READ-{n:03}", args)

    def test_sql_machine_table_and_budgets(self):
        cases = [
            ["query", "SELECT 2 AS x"],
            ["--json", "query", "SELECT 2 AS x"],
            ["--machine", "query", "SELECT 2 AS x"],
            ["query", "SELECT 'Å\\n' AS x, NULL AS n"],
            ["query", "SELECT 1 AS x UNION ALL SELECT 2", "--max-rows", "1"],
            [
                "--machine",
                "query",
                "SELECT 1 AS x UNION ALL SELECT 2",
                "--max-rows",
                "1",
            ],
            ["--machine", "query", "DELETE FROM items"],
            ["--machine", "query", "SELECT 1", "--max-rows", "-1"],
            ["--machine", "query", "SELECT 1", "--timeout-ms", "60001"],
            [
                "--machine",
                "query",
                "SELECT :n",
                "--params",
                '{"n":9223372036854775808}',
            ],
            ["--machine", "query", "SELECT 1", "--definition", "x"],
        ]
        for n, args in enumerate(cases):
            self.compare(f"LC-SQL-{n:03}", args)

    def test_native_owned_mutation_and_reopen(self):
        # Explicit item IDs and namespace identity avoid generated-ID normalization.
        for n, args in enumerate(
            [
                ["--json", "profile", "add", "second"],
                [
                    "--json",
                    "item",
                    "put",
                    "--kind",
                    "movie",
                    "--id",
                    "movie-one",
                    "--identity",
                    "external=one",
                    "--metadata",
                    '{"title":"雪","year":2026}',
                ],
                ["--json", "item", "list"],
                ["--json", "item", "show", "movie-one"],
                ["--machine", "item", "status", "movie-one"],
            ]
        ):
            self.compare(f"LC-MUT-{n:03}", args)
        with (
            closing(sqlite3.connect(self.reference)) as a,
            closing(sqlite3.connect(self.native)) as b,
        ):
            for table in [
                "items",
                "identities",
                "profiles",
                "item_workflow",
                "item_worklog",
            ]:
                self.assertEqual(
                    a.execute(f"SELECT * FROM {table} ORDER BY 1").fetchall(),
                    b.execute(f"SELECT * FROM {table} ORDER BY 1").fetchall(),
                    f"LC-MUT-DB:{table}",
                )

    def test_spec_diff_incomplete_and_unknown_field_roundtrip(self):
        schema = self.root / "other.json"
        schema.write_text("{}")
        self.compare(
            "LC-SPEC-001",
            ["--machine", "spec", "diff", "--against", str(schema)],
            database=False,
        )
        # Native pure document validation owns all fields, including unknown data.
        source = self.run_process(True, ["--db", str(self.reference), "manifest"])
        self.assertEqual(source.returncode, 0, source.stderr)
        value = json.loads(source.stdout)
        value["complete"] = False
        value["future"] = {
            "large": 2**80,
            "unicode": "雪",
            "nested": [False, None, {"x": 1}],
        }
        # An unknown envelope extension does not alter the content hash.
        path = self.root / "manifest.json"
        path.write_text(json.dumps(value, ensure_ascii=False))
        self.compare(
            "LC-SPEC-002",
            ["--machine", "spec", "roundtrip", "--file", str(path)],
            database=False,
        )
        self.compare(
            "LC-SPEC-003",
            ["--machine", "spec", "validate", "--file", str(path)],
            database=False,
        )

    def test_maintenance_preflight_and_http_gate_are_explicit(self):
        before = self.native.read_bytes()
        result = self.run_process(
            False, ["--db", str(self.native), "--machine", "maintenance"]
        )
        self.assertEqual(result.returncode, 3, "LC-MAINT-PREFLIGHT-001")
        error = json.loads(result.stdout)
        self.assertIn(
            "no source locations configured",
            error["data"]["errors"][0]["message"],
        )
        self.assertEqual(before, self.native.read_bytes(), "LC-GAP-DB-001")
        result = self.run_process(
            False, ["--db", str(self.native), "--machine", "api", "serve"]
        )
        self.assertEqual(result.returncode, 2, "LC-GATE-HTTP-001")
        self.assertIn("--experimental-native-http", result.stdout)

    def test_native_query_interrupt_channels(self):
        sql = "WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) FROM n"
        for machine in (False, True):
            args = (
                ["--db", str(self.native)]
                + (["--machine"] if machine else [])
                + ["query", sql, "--timeout-ms", "60000"]
            )
            process = subprocess.Popen(
                [str(BINARY)] + args,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                env=self.env,
            )
            time.sleep(0.25)
            self.assertIsNone(process.poll(), "LC-SIG-START")
            process.send_signal(signal.SIGINT)
            out, err = process.communicate(timeout=10)
            self.assertEqual(process.returncode, 130, "LC-SIG-130")
            if machine:
                self.assertEqual(err, "")
                value = json.loads(out)
                self.assertEqual(value["exit_code"], 130)
                self.assertEqual(value["outcome"], "error")
                self.assertEqual(value["data"]["error"]["type"], "KeyboardInterrupt")
            else:
                self.assertEqual(out, "")
                self.assertEqual(
                    err, "interrupted; use recover if an operation is pending\n"
                )
        self.compare("LC-SIG-REOPEN", ["--machine", "query", "SELECT 1 AS value"])

    def test_dynamic_queries_and_empty_scan(self):
        definition = self.root / "query.json"
        definition.write_text(
            json.dumps(
                {
                    "mode": "rows",
                    "selection": {
                        "language": "sql",
                        "query": "SELECT item_id,title FROM catalog_items ORDER BY item_id",
                    },
                }
            )
        )
        seed = self.run_process(
            True,
            [
                "--db",
                str(self.reference),
                "--json",
                "query",
                "save",
                "saved",
                "--definition",
                str(definition),
            ],
        )
        self.assertEqual(seed.returncode, 0, seed.stderr)
        identifier = json.loads(seed.stdout)["id"]
        shutil.copyfile(self.reference, self.native)
        for n, args in enumerate(
            [
                ["--machine", "query", "show", identifier],
                ["--machine", "query", "list"],
                ["--machine", "query", "run", identifier],
                ["--machine", "query", "save"],
                ["--machine", "scan"],
                ["--machine", "scan", "--scope", "folder"],
            ]
        ):
            self.compare(f"LC-DYNAMIC-{n:03}", args)

    def test_processing_owner_and_selection_transport(self):
        selection = self.root / "selection.json"
        selection.write_text(
            json.dumps({"language": "sql", "query": "SELECT id AS file_id FROM files"})
        )
        for n, args in enumerate(
            [
                ["--machine", "process", "enqueue", "sniff"],
                ["--machine", "process", "enqueue", "hash", "--limit", "0"],
                ["--machine", "process", "enqueue", "sniff", "--file-id", "missing"],
                [
                    "--machine",
                    "process",
                    "enqueue",
                    "sniff",
                    "--selection",
                    str(selection),
                ],
                [
                    "--machine",
                    "process",
                    "enqueue",
                    "sniff",
                    "--selection",
                    str(selection),
                    "--options",
                    "invalid but unused",
                ],
                [
                    "--machine",
                    "process",
                    "enqueue",
                    "sniff",
                    "--selection",
                    str(selection),
                    "--file-id",
                    "missing",
                ],
                ["--machine", "process", "run"],
                ["--machine", "process", "run", "--workers", "0"],
                ["--machine", "process", "run", "--storage-groups", '{"source": ""}'],
                ["--machine", "artifact", "run"],
                ["--machine", "artifact", "recover"],
            ]
        ):
            self.compare(f"LC-PROC-{n:03}", args)
        selection.write_text(
            json.dumps(
                {
                    "language": "sql",
                    "query": "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1001) SELECT 'missing-'||printf('%04d',x) AS file_id FROM n",
                }
            )
        )
        self.compare(
            "LC-PROC-BATCH-1001",
            ["--machine", "process", "enqueue", "sniff", "--selection", str(selection)],
        )
        selection.write_text(
            json.dumps({"language": "sql", "query": "SELECT id AS item_id FROM items"})
        )
        self.compare(
            "LC-PROC-ENTITY",
            ["--machine", "process", "enqueue", "sniff", "--selection", str(selection)],
        )
        selection.write_text(
            json.dumps(
                {
                    "language": "sql",
                    "query": "SELECT id AS file_id FROM files",
                    "profile": "other",
                }
            )
        )
        self.compare(
            "LC-PROC-PROFILE",
            ["--machine", "process", "enqueue", "sniff", "--selection", str(selection)],
        )

    def test_fallback_controls_exact_and_durable(self):
        with closing(sqlite3.connect(self.reference)) as db:
            db.execute(
                "INSERT INTO saved_queries(id,profile,name,revision,definition,digest) VALUES ('q','default','fixture',1,'{}','query-fixture')"
            )
            db.execute(
                "INSERT INTO fallback_policies VALUES ('p','default','fixture',1,'{}','policy-fixture')"
            )
            db.execute(
                "INSERT INTO fallback_bindings(profile,catalog,policy_id,query_id,layout) VALUES ('default','global','p','q','fixture')"
            )
            db.execute(
                "INSERT INTO fallback_history(profile,catalog,entry_id,generation,decision,created_at) VALUES ('default','global','entry',1,?,1.5)",
                (json.dumps({"future": 2**80, "state": "retained"}),),
            )
            db.commit()
        shutil.copyfile(self.reference, self.native)
        for n, args in enumerate(
            [
                [
                    "--machine",
                    "fallback",
                    "enable",
                    "global",
                    "--interval",
                    "123",
                    "--max-changes",
                    "7",
                    "--max-removals",
                    "2",
                ],
                ["--machine", "fallback", "disable", "global"],
                ["--machine", "fallback", "enable", "global", "--interval", "0"],
                ["--machine", "fallback", "enable", "missing"],
                ["--machine", "fallback", "history", "global"],
                ["--machine", "fallback", "history", "missing"],
                ["--profile", "other", "--machine", "fallback", "history", "global"],
            ]
        ):
            self.compare(f"LC-FBC-{n:03}", args)
        with (
            closing(sqlite3.connect(self.reference)) as a,
            closing(sqlite3.connect(self.native)) as b,
        ):
            for table in [
                "fallback_bindings",
                "fallback_history",
                "catalog_refresh_settings",
                "fallback_epoch",
            ]:
                self.assertEqual(
                    a.execute(f"SELECT * FROM {table} ORDER BY 1").fetchall(),
                    b.execute(f"SELECT * FROM {table} ORDER BY 1").fetchall(),
                    f"LC-FBC-DB:{table}",
                )

    def test_http_operation_local_controls(self):
        spec = self.root / "openapi.json"
        spec.write_text(
            json.dumps(
                {
                    "openapi": "3.1.0",
                    "info": {"title": "fixture", "version": "1"},
                    "paths": {
                        "/items": {
                            "post": {
                                "operationId": "notify",
                                "responses": {"204": {"description": "Accepted"}},
                            }
                        }
                    },
                }
            )
        )
        args = [
            "--json",
            "projection",
            "http-operation-put",
            "fixture",
            "--spec",
            str(spec),
            "--operation-id",
            "notify",
            "--base-url",
            "http://127.0.0.1:1",
        ]
        seeded = self.run_process(True, ["--db", str(self.reference)] + args)
        self.assertEqual(seeded.returncode, 0, seeded.stderr)
        identifier = json.loads(seeded.stdout)["id"]
        shutil.copyfile(self.reference, self.native)
        for n, case in enumerate(
            [
                args,
                ["--machine", "projection", "http-operation-list"],
                ["--machine", "projection", "http-operation-show", identifier],
                ["--machine", "projection", "http-operation-retry", identifier],
                ["--machine", "projection", "http-operation-disable", identifier],
                ["--machine", "projection", "http-operation-retry", identifier],
                ["--machine", "projection", "http-operation-show", "missing"],
                ["--machine", "projection", "http-operation-list", "--limit", "0"],
                ["--machine", "projection", "mapping-capabilities"],
            ]
        ):
            self.compare(
                f"LC-HTTPCTRL-{n:03}", case, semantic=n == 8
            )  # Frozen capability order uses randomized set iteration; semantic-only gap.
        with (
            closing(sqlite3.connect(self.reference)) as a,
            closing(sqlite3.connect(self.native)) as b,
        ):
            for table in [
                "http_operations",
                "notification_destinations",
                "notification_deliveries",
            ]:
                self.assertEqual(
                    a.execute(f"SELECT * FROM {table} ORDER BY 1").fetchall(),
                    b.execute(f"SELECT * FROM {table} ORDER BY 1").fetchall(),
                    f"LC-HTTPCTRL-DB:{table}",
                )

    def test_copy_plan_and_rendition_controls(self):
        for n, args in enumerate(
            [
                ["--machine", "copies", "plan"],
                ["--machine", "copies", "plan", "--catalog", "missing"],
                ["--machine", "copies", "plan", "--limit", "0"],
                ["--machine", "copies", "plan", "--limit", "100001"],
                [
                    "--machine",
                    "rendition",
                    "exclude",
                    "missing",
                    "--catalog",
                    "global",
                    "--reason",
                    "fixture",
                ],
                [
                    "--machine",
                    "rendition",
                    "allow",
                    "missing",
                    "--catalog",
                    "missing",
                    "--reason",
                    "fixture",
                ],
                [
                    "--machine",
                    "rendition",
                    "exclude",
                    "missing",
                    "--catalog",
                    "global",
                    "--reason",
                    "",
                ],
                [
                    "--machine",
                    "rendition",
                    "policy",
                    "--catalog",
                    "global",
                    "--definition",
                    '{"outputs": []}',
                ],
                [
                    "--machine",
                    "rendition",
                    "policy",
                    "--catalog",
                    "global",
                    "--definition",
                    '{"purpose": "preview"}',
                ],
                ["--machine", "content", "search", "fixture"],
                ["--machine", "content", "search", ""],
            ]
        ):
            self.compare(f"LC-CONTROL-{n:03}", args)
        # Isolated recorded output fixture: decision controls only inspect IDs,
        # and must not open any source path. Compare durable decision effects.
        with closing(sqlite3.connect(self.reference)) as db:
            db.execute(
                "INSERT INTO media_outputs(id,profile,file_id,source_file_id,source_item_id,item_id,definition_id,origin,metadata) VALUES ('out','default','file','source','item','item','def','registered','{}')"
            )
            db.commit()
        shutil.copyfile(self.reference, self.native)
        for n, op in enumerate(["exclude", "allow"]):
            self.compare(
                f"LC-CONTROL-DECISION-{n:03}",
                [
                    "--machine",
                    "rendition",
                    op,
                    "out",
                    "--catalog",
                    "global",
                    "--reason",
                    "fixture",
                ],
            )
            with (
                closing(sqlite3.connect(self.reference)) as a,
                closing(sqlite3.connect(self.native)) as b,
            ):
                self.assertEqual(
                    a.execute("SELECT * FROM rendition_decisions").fetchall(),
                    b.execute("SELECT * FROM rendition_decisions").fetchall(),
                    f"LC-CONTROL-DECISION-DB-{n:03}",
                )

    def test_processing_selection_crosses_batch_boundary(self):
        source = self.root / "batch-source"
        source.mkdir()
        for n in range(1001):
            (source / f"{n:04}.txt").write_text("fixture")
        for args in [["location", "bind", "batch", "--root", str(source)], ["scan"]]:
            result = self.run_process(True, ["--db", str(self.reference)] + args)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        shutil.copyfile(self.reference, self.native)
        selection = self.root / "batch-selection.json"
        selection.write_text(
            json.dumps({"language": "sql", "query": "SELECT id AS file_id FROM files"})
        )
        results = []
        for reference, database in [(True, self.reference), (False, self.native)]:
            result = self.run_process(
                reference,
                [
                    "--db",
                    str(database),
                    "--machine",
                    "process",
                    "enqueue",
                    "sniff",
                    "--selection",
                    str(selection),
                ],
            )
            self.assertEqual(
                result.returncode,
                0,
                ("LC-PROC-BATCH-SUCCESS", result.stdout, result.stderr),
            )
            value = json.loads(result.stdout)
            self.assertEqual(len(value["data"]["queued"]), 1001)
            value["data"]["queued"] = ["<JOB>"] * 1001
            results.append(value)
        self.assertEqual(results[0], results[1], "LC-PROC-BATCH-ENVELOPE")
        # Generated job UUIDs and enqueue timestamps differ. Every durable input,
        # cache fingerprint, options, job state and source snapshot must match.
        with (
            closing(sqlite3.connect(self.reference)) as a,
            closing(sqlite3.connect(self.native)) as b,
        ):
            sql = "SELECT profile,file_id,operation,location,snapshot,options,cache_key,state,attempts,result,error,finished_at FROM processing_jobs ORDER BY file_id"
            self.assertEqual(
                a.execute(sql).fetchall(), b.execute(sql).fetchall(), "LC-PROC-BATCH-DB"
            )

    def test_owner_ledger_exhaustive_but_not_full_parity(self):
        value = json.loads((ROOT / "tests/parity/legacy-cli-owners.json").read_text())
        self.assertEqual(len(value["rows"]), 242)
        self.assertEqual(len({r["id"] for r in value["rows"]}), 242)
        self.assertFalse(value["full_surface_parity"])
        self.assertTrue(value["python_remains_default"])
        self.assertTrue(all(r["native_owner"] or r.get("gap") for r in value["rows"]))
