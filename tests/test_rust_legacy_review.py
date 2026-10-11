# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Independent preserved-terminal review: only disposable local catalogs.

The oracle executes frozen Python, not generated CLI metadata. Exact byte
comparisons intentionally include populated reads and numeric/Unicode edges.
Underlying filesystem, publication, query, and scheduler owners are collaborators,
not newly qualified by these terminal/control tests.
"""

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

ROOT = Path(__file__).resolve().parents[1]
PYTHON = Path(
    os.environ.get(
        "CATABOLIC_PYTHON_ORACLE",
        ROOT.parent / ".local-tools/catabolic-reference-3.14.6/bin/python",
    )
).absolute()
REFERENCE = Path(
    os.environ.get(
        "CATABOLIC_PYTHON_REFERENCE", "/tmp/catabolic-embedding-frozen-453fca9/src"
    )
).resolve()
BINARY = Path(
    os.environ.get(
        "CATABOLIC_LEGACY_BINARY", ROOT / "target/debug/catabolic-legacy-native"
    )
).resolve()
ORACLE = "import pathlib,sys;sys.path.insert(0,sys.argv.pop(1));from catabolic.cli import main;raise SystemExit(main(sys.argv[1:]))"


class NativeLegacyIndependentReview(unittest.TestCase):
    def setUp(self):
        for path in (PYTHON, REFERENCE / "catabolic/cli.py", BINARY):
            self.assertTrue(path.is_file(), str(path))
        temporary = tempfile.TemporaryDirectory(prefix="legacy-independent-review-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.db = {
            True: self.root / "reference.sqlite3",
            False: self.root / "native.sqlite3",
        }
        self.env = {
            k: v
            for k, v in os.environ.items()
            if not k.startswith("CATABOLIC_") and k not in ("PYTHONPATH", "PYTHONHOME")
        }
        self.env.update(
            HOME=str(self.root / "home"),
            COLUMNS="80",
            NO_COLOR="1",
            TERM="dumb",
            PYTHONUTF8="1",
            PYTHONHASHSEED="0",
        )
        initialized = self.run_cli(True, ["init"])
        self.assertEqual(initialized.returncode, 0, initialized.stderr)
        shutil.copyfile(self.db[True], self.db[False])

    def run_cli(self, oracle, argv, *, database=True, env=None):
        command = (
            [str(PYTHON), "-c", ORACLE, str(REFERENCE)] if oracle else [str(BINARY)]
        )
        if database:
            command += ["--db", str(self.db[oracle])]
        return subprocess.run(
            command + argv,
            text=True,
            capture_output=True,
            env=env or self.env,
            timeout=30,
        )

    def compare(self, scenario, argv, **kwargs):
        oracle, native = (self.run_cli(flag, argv, **kwargs) for flag in (True, False))

        def normalized(value):
            return value.replace(str(self.db[True]), "<DB>").replace(
                str(self.db[False]), "<DB>"
            )

        with self.subTest(scenario=scenario, argv=argv):
            self.assertEqual(
                (
                    native.returncode,
                    normalized(native.stdout),
                    normalized(native.stderr),
                ),
                (
                    oracle.returncode,
                    normalized(oracle.stdout),
                    normalized(oracle.stderr),
                ),
            )
        return oracle, native

    def seed(self, callback):
        with closing(sqlite3.connect(self.db[True])) as db:
            callback(db)
            db.commit()
        shutil.copyfile(self.db[True], self.db[False])

    def test_invalid_utf8_argv_refuses_without_panic_or_catalog_mutation(self):
        before = self.db[False].read_bytes()
        for token in (b"\xff", b"\xed\xa0\x80"):
            result = subprocess.run(
                [
                    os.fsencode(BINARY),
                    b"--db",
                    os.fsencode(self.db[False]),
                    b"--machine",
                    b"docs",
                    token,
                ],
                capture_output=True,
                env=self.env,
                timeout=30,
            )
            self.assertEqual(result.returncode, 2)
            self.assertEqual(result.stdout, b"")
            self.assertEqual(
                result.stderr,
                b"error: native legacy CLI requires UTF-8 command-line arguments\n",
            )
        self.assertEqual(self.db[False].read_bytes(), before)
        self.compare(
            "LCR-VALID-UNICODE-AFTER-REFUSAL",
            ["--machine", "docs", "--search", "雪"],
            database=False,
        )

    def test_float_rendering_all_terminal_formats(self):
        numbers = (
            "1e-7,1e-5,1e-4,1e15,1e16,1e20,-1e-9,-0.0,2.2250738585072014e-308,5e-324"
        )
        for mode in ([], ["--json"], ["--machine"]):
            self.compare("LCR-FLOAT", mode + ["query", "SELECT " + numbers])

    def test_nonprintable_unicode_parser_diagnostics(self):
        for codepoint in (
            0xA0,
            0xAD,
            0x378,
            0x200B,
            0x2028,
            0x2029,
            0x2066,
            0xE000,
            0x1FFFE,
            0xE0001,
        ):
            token = chr(codepoint)
            self.compare(f"LCR-REPR-{codepoint:x}", [token], database=False)
            self.compare(
                f"LCR-INT-REPR-{codepoint:x}",
                ["query", "SELECT 1", "--max-rows=" + token],
                database=False,
            )

    def test_environment_overrides_and_output_escaping(self):
        for oracle in (True, False):
            p = self.run_cli(oracle, ["profile", "add", "review"])
            self.assertEqual(p.returncode, 0, p.stderr)
        for oracle in (True, False):
            env = {
                **self.env,
                "CATABOLIC_DB": str(self.db[oracle]),
                "CATABOLIC_PROFILE": "review",
            }
            result = self.run_cli(
                oracle,
                [
                    "--machine",
                    "query",
                    "SELECT 'line' || char(9) || char(1) || 'é' AS value",
                ],
                database=False,
                env=env,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout)["profile"], "review")
            if oracle:
                expected = result.stdout
            else:
                self.assertEqual(result.stdout, expected)
        self.compare("LCR-PROFILE-EMPTY", ["--profile", "", "--machine", "status"])

    def test_processing_populated_pages_and_result_omission(self):
        def rows(db):
            for n in range(3):
                db.execute(
                    "INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,result,created_at) VALUES (?,?,?,?,?,?,?,?,?,?)",
                    (
                        f"job-{n}",
                        "default",
                        "file",
                        "sniff",
                        "source",
                        '{"file_id":"file"}',
                        '{"future":1099511627776}',
                        f"key-{n}",
                        json.dumps({"value": "z" * (9000 if n == 2 else 2)}),
                        "2026-01-01 00:00:00",
                    ),
                )
                db.execute(
                    "INSERT INTO processing_attempts(job_id,attempt,state,retryable,retry_after,result,created_at) VALUES (?,?,?,?,?,?,?)",
                    (
                        "job-0",
                        n + 1,
                        "failed",
                        1,
                        1.5,
                        '{"value":1}',
                        "2026-01-01 00:00:00",
                    ),
                )

        self.seed(rows)
        for mode in ([], ["--json"], ["--machine"]):
            for args in (
                ["process", "list", "--limit", "1"],
                ["process", "list", "--after", "job-0"],
                ["process", "attempts", "job-0", "--limit", "1"],
                ["process", "attempts", "job-0", "--after", "1"],
            ):
                self.compare("LCR-PROCESS-PAGE", mode + args)

    def test_recipe_pages_profile_and_bounds(self):
        def rows(db):
            for n in range(3):
                db.execute(
                    "INSERT INTO processing_recipes(id,name,revision,preset,definition,digest,created_at) VALUES (?,?,?,?,?,?,?)",
                    (
                        f"recipe-{n}",
                        "fixture",
                        n + 1,
                        "fixture",
                        '{"kind":"analysis","operation":"sniff"}',
                        f"digest-{n}",
                        "2026-01-01 00:00:00",
                    ),
                )

        self.seed(rows)
        for args in (
            ["artifact", "recipes", "--limit", "1"],
            ["artifact", "recipes", "--after", "recipe-0"],
            ["operation", "list", "--limit", "1"],
            ["artifact", "recipes", "--limit", "0"],
            ["--profile", "missing", "artifact", "recipes"],
        ):
            self.compare("LCR-RECIPE-PAGE", ["--machine"] + args)

    def test_four_mib_read_budget_and_resume(self):
        # Decode only persisted synthetic data, with no file/source access.
        def rows(db):
            for n in range(3):
                definition = json.dumps(
                    {"fixture": "a" * (1500000 if n < 2 else 4300000)}
                )
                db.execute(
                    "INSERT INTO output_definitions(id,name,revision,definition,digest,created_at) VALUES (?,?,?,?,?,?)",
                    (
                        f"output-{n}",
                        "fixture",
                        n + 1,
                        definition,
                        f"digest-{n}",
                        "2026-01-01 00:00:00",
                    ),
                )

        self.seed(rows)
        a, _ = self.compare("LCR-BOUND-PAGE", ["--json", "rendition", "definitions"])
        self.assertEqual(json.loads(a.stdout)["next_after"], "output-1")
        self.compare(
            "LCR-BOUND-RECORD",
            ["--machine", "rendition", "definitions", "--after", "output-1"],
        )

    def test_fallback_control_boundaries_and_atomic_rejection(self):
        def rows(db):
            db.execute(
                "INSERT INTO saved_queries(id,profile,name,revision,definition,digest) VALUES ('q','default','fixture',1,'{}','q')"
            )
            db.execute(
                "INSERT INTO fallback_policies VALUES ('p','default','fixture',1,'{}','p')"
            )
            db.execute(
                "INSERT INTO fallback_bindings(profile,catalog,policy_id,query_id,layout) VALUES ('default','global','p','q','fixture')"
            )

        self.seed(rows)
        for args in (
            [
                "--interval",
                "86400",
                "--max-changes",
                "10000",
                "--max-removals",
                "10000",
            ],
            ["--interval", "86401"],
            ["--max-changes", "0"],
            ["--max-removals", "-1"],
            ["--max-removals", "10001"],
        ):
            self.compare(
                "LCR-FALLBACK-BOUNDS",
                ["--machine", "fallback", "enable", "global"] + args,
            )
        self.compare(
            "LCR-FALLBACK-DISABLE", ["--machine", "fallback", "disable", "global"]
        )
        for table in (
            "fallback_bindings",
            "catalog_refresh_settings",
            "fallback_epoch",
        ):
            states = []
            for path in self.db.values():
                with closing(sqlite3.connect(path)) as db:
                    states.append(
                        db.execute(f"SELECT * FROM {table} ORDER BY 1").fetchall()
                    )
            self.assertEqual(*states, table)

    def test_http_operation_page_profile_fences(self):
        def rows(db):
            db.execute("INSERT INTO profiles(id) VALUES ('other')")
            for identifier, profile in (
                ("a", "default"),
                ("b", "other"),
                ("c", "default"),
            ):
                db.execute(
                    "INSERT INTO http_operations VALUES (?,?,?,?,?,?,?,?,?)",
                    (
                        identifier,
                        profile,
                        identifier,
                        1,
                        '{"method":"POST","operation_id":"test","spec_digest":"digest"}',
                        "digest",
                        1,
                        None,
                        None,
                    ),
                )

        self.seed(rows)
        for args in (
            ["projection", "http-operation-list", "--limit", "1"],
            ["projection", "http-operation-list", "--after", "a"],
            ["projection", "http-operation-show", "b"],
            ["--profile", "other", "projection", "http-operation-show", "b"],
        ):
            self.compare("LCR-HTTP-PAGE", ["--machine"] + args)

    def test_unpaged_oversized_result_refusal_is_explicit_and_read_only(self):
        # This is an intentional native safety-limit difference. The old
        # unpaged command returns all rows, while native must never imply that
        # a truncated prefix is the complete result.
        def rows(db):
            for n in range(1050):
                db.execute(
                    "INSERT INTO bindings VALUES ('default','source',?,?,1,1)",
                    (str(n), str(n) + "x" * 4000),
                )

        self.seed(rows)
        before = self.db[False].read_bytes()
        native = self.run_cli(False, ["--machine", "location", "list"])
        self.assertEqual(native.returncode, 2, native.stderr)
        self.assertEqual(native.stderr, "")
        response = json.loads(native.stdout)
        self.assertEqual(response["outcome"], "error")
        self.assertEqual(
            response["data"]["error"]["message"], "result exceeds the result budget"
        )
        self.assertEqual(self.db[False].read_bytes(), before)
        reference = self.run_cli(True, ["--machine", "location", "list"])
        self.assertEqual(reference.returncode, 0, reference.stderr)
        self.assertEqual(len(json.loads(reference.stdout)["data"]["bindings"]), 1050)

    def test_scan_extraction_is_same_body(self):
        original = subprocess.run(
            [
                "git",
                "-C",
                str(ROOT.parent / "catabolic"),
                "show",
                "c69859e:crates/catabolic-python/src/embedding.rs",
            ],
            text=True,
            capture_output=True,
            check=True,
        ).stdout
        original = original[original.index("fn scan(") :]
        original = original[: original.index("\npub fn register(")]
        old_body = (
            original[original.index("{") :]
            .strip()
            .replace("catabolic_runtime::observation::", "crate::observation::")
        )
        new = (ROOT / "crates/catabolic-runtime/src/public_scan.rs").read_text()
        new_body = new[new.index("pub fn execute(") :]
        new_body = new_body[new_body.index("{") :].strip()
        self.assertEqual(old_body, new_body)
        forwarding = (ROOT / "crates/catabolic-python/src/embedding.rs").read_text()
        self.assertIn(
            "public_scan::execute(session.store_mut()?, profile, &request)", forwarding
        )

    def test_generated_source_provenance_and_entrypoint_separation(self):
        resource = json.loads(
            (ROOT / "crates/catabolic-cli/resources/legacy-cli.json").read_text()
        )
        for path, expected in resource["source_sha256"].items():
            self.assertEqual(
                hashlib.sha256((REFERENCE.parent / path).read_bytes()).hexdigest(),
                expected,
                path,
            )
        self.assertEqual(resource["pythonhashseed"], 0)
        self.assertIn("3.14.6", resource["interpreter"])
        for relative in (
            "legacy_main.rs",
            "legacy/dispatch.rs",
            "legacy/parser.rs",
            "legacy/presentation.rs",
        ):
            source = (ROOT / "crates/catabolic-cli/src" / relative).read_text()
            self.assertNotRegex(source, r"Command::|helper::register|current_exe\(")
        project = (ROOT / "pyproject.toml").read_text()
        self.assertIn('catabolic = "catabolic.cli:main"', project)
        ledger = json.loads((ROOT / "tests/parity/legacy-cli-owners.json").read_text())
        self.assertFalse(ledger["full_surface_parity"])
        self.assertTrue(ledger["python_remains_default"])
        rows = {row["command"]: row for row in ledger["rows"]}
        for command, owner in {
            "rendition exclude": "catabolic_store::public_controls::rendition_decide(excluded=true)",
            "process list": "catabolic_store::public_reads::processing(list)",
            "query contract": "embedded frozen resource: definition_schemas.query",
        }.items():
            self.assertEqual(rows[command]["native_owner"], owner)
            self.assertEqual(rows[command]["owner_mapping"], "entrypoint")

    def test_resource_regeneration_is_byte_identical(self):
        output = self.root / "resource.json"
        result = subprocess.run(
            [
                str(PYTHON),
                str(ROOT / "scripts/generate_legacy_cli.py"),
                "--source",
                str(REFERENCE.parent),
                "--output",
                str(output),
            ],
            text=True,
            capture_output=True,
            env=self.env,
            timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            output.read_bytes(),
            (ROOT / "crates/catabolic-cli/resources/legacy-cli.json").read_bytes(),
        )


if __name__ == "__main__":
    unittest.main()
