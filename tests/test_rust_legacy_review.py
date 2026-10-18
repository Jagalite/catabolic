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


# Exact accepted source evolution, not a new behavioral qualification. Reversing
# these hunks must still reproduce the original extracted-body digest below.
# bc762bfc61bd8382e62891c0ddcf185a7880041d: public resume and request boundary.
SCAN_RESUME = """    if resume {
        // Public admission above rejected actual scope changes. Do not turn
        // its normalized empty default into an amendment at the owner boundary.
        args.as_object_mut().unwrap().remove("exclude");
    }
"""
SCAN_REQUEST_BOUNDARY = """    // Record the actual public request boundary. The native owner retains its
    // existing (later) admission-time minimum_start floor when reuse is off.
    args["after"] = json!(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
    );
"""
# a8c5a8ee0aae81e805772906b5fcb3679b90b122: shared callback transport.
SCAN_TRANSPORT = """) -> Result<Value> {
    execute_with(store, profile, request, |store, profile, args| {
        crate::observation::execute(store, profile, "scan", args)
    })
}

/// Use the same orchestration with an embedding's publication transport.
pub fn execute_with(
    store: &mut catabolic_store::Store,
    profile: &str,
    request: &Value,
    mut observe: impl FnMut(&mut catabolic_store::Store, &str, &Value) -> Result<Value>,
) -> Result<Value> {"""
# 4e653d53b89c8d3cacef4097eff950fc8b479017: the current executable supplies
# the native supervised processor transport, not Python or engine fallback.
RULE_WORKER_ARM = """        "preview" | "apply" | "run" => {
            if a["worker"].as_str().is_some_and(|v| !v.is_empty()) {
                return catabolic_runtime::rules::run_external(
                    Path::new(text(a, "db")),
                    text(a, "profile"),
                    text(a, "id"),
                    &r,
                    &mut catabolic_runtime::processors::SupervisedTransport {
                        executable: &std::env::current_exe()?,
                        cancel,
                    },
                    cancel,
                );
            }
            runtime(a, "rules", op, &r)
        }
"""


def replace_once(case, source, old, new=""):
    case.assertEqual(source.count(old), 1, "accepted source hunk changed: " + old)
    return source.replace(old, new, 1)


def assert_scan_extraction(case, source):
    source = replace_once(case, source, SCAN_TRANSPORT, ") -> Result<Value> {")
    for hunk, following in (
        (SCAN_RESUME, "    if resume || continuation {"),
        (SCAN_REQUEST_BOUNDARY, "    let mut scans = vec![];"),
    ):
        source = replace_once(case, source, hunk + following, following)
    for before, after in (
        (
            "return observe(store, profile, &args);",
            'return crate::observation::execute(store, profile, "scan", &args);',
        ),
        (
            "let value = observe(store, profile, &args)?;",
            'let value = crate::observation::execute(store, profile, "scan", &args)?;',
        ),
    ):
        source = replace_once(case, source, before, after)
    body = source[source.index("pub fn execute(") :]
    body = body[body.index("{") :].strip()
    case.assertEqual(
        hashlib.sha256(body.encode()).hexdigest(),
        "5065c8dd4c75f4a93fa0f65a3ab268507ffa7a30009235365726723ea236e781",
    )


# 15c8b64: the reviewed supervision entry point (Track D supervision rounds).
# Every Catabolic binary must run it first: it either becomes the tool
# supervisor (never returns) or registers this executable as the supervisor
# helper. It takes no path, Python or PATH input.
SUPERVISOR_ENTRY = """    // First: tool supervisor mode (never returns) or supervisor registration.
    catabolic_runtime::process::supervise_or_configure();
"""
NOTIFICATION_HELPER_REGISTRATION = """    if let Err(error) = std::env::current_exe()
        .map_err(catabolic_store::Error::from)
        .and_then(|path| catabolic_runtime::notifications::register_provider_helper(&path))
    {
        eprintln!("error: {error}");
        std::process::exit(2);
    }
"""


def assert_entrypoint_separation(case, relative, source):
    # Keep the original exclusions everywhere. Only the exact native blocks
    # below may obtain the current executable; no general exemption exists.
    if relative == "legacy_main.rs":
        # Only the exact supervision entry point followed by the exact native
        # notification worker registration at the entry boundary is allowed.
        # Neither can choose a Python executable or PATH.
        before = (
            "fn main() {\n"
            + SUPERVISOR_ENTRY
            + NOTIFICATION_HELPER_REGISTRATION
            + "    let mut args ="
        )
        source = replace_once(case, source, before, "fn main() {\n    let mut args =")
    case.assertNotRegex(source, r"Command::|helper::register")
    if relative == "legacy/dispatch.rs":
        start = source.index("\nfn rule(")
        end = source.index("\nfn watcher(", start)
        rule = replace_once(case, source[start:end], RULE_WORKER_ARM)
        source = source[:start] + rule + source[end:]
    case.assertNotRegex(source, r"current_exe\(")


class LegacySourceContractGuards(unittest.TestCase):
    """In-memory negative witnesses for the two source checks; no CLI effects."""

    def test_scan_evolution_rejects_missing_changed_and_duplicate_hunks(self):
        source = (ROOT / "crates/catabolic-runtime/src/public_scan.rs").read_text()
        for hunk in (SCAN_RESUME, SCAN_REQUEST_BOUNDARY, SCAN_TRANSPORT):
            for replacement in ("", hunk.replace("    ", "   ", 1), hunk + hunk):
                with self.subTest(hunk=hunk, replacement=replacement):
                    with self.assertRaises(AssertionError):
                        assert_scan_extraction(
                            self, source.replace(hunk, replacement, 1)
                        )

    def test_scan_evolution_still_rejects_unrecorded_body_drift(self):
        source = (ROOT / "crates/catabolic-runtime/src/public_scan.rs").read_text()
        for before, after in (
            (
                'args["reuse_completed"] = json!(false);',
                'args["reuse_completed"] = json!(true);',
            ),
            (
                "return observe(store, profile, &args);",
                "return observe(store, profile, request);",
            ),
            (
                "let value = observe(store, profile, &args)?;",
                "let value = observe(store, profile, request)?;",
            ),
        ):
            with self.subTest(before=before), self.assertRaises(AssertionError):
                assert_scan_extraction(self, source.replace(before, after, 1))
        for hunk in (SCAN_RESUME, SCAN_REQUEST_BOUNDARY):
            with self.subTest(moved_hunk=hunk), self.assertRaises(AssertionError):
                assert_scan_extraction(self, source.replace(hunk, "", 1) + hunk)

    def test_worker_exception_rejects_changed_moved_and_duplicate_arms(self):
        relative = "legacy/dispatch.rs"
        source = (ROOT / "crates/catabolic-cli/src" / relative).read_text()
        for label, changed in (
            ("missing", source.replace(RULE_WORKER_ARM, "", 1)),
            (
                "changed transport",
                source.replace(
                    "executable: &std::env::current_exe()?",
                    'executable: Path::new("python")',
                    1,
                ),
            ),
            ("duplicate", source + RULE_WORKER_ARM),
            ("moved", source.replace(RULE_WORKER_ARM, "", 1) + RULE_WORKER_ARM),
        ):
            with self.subTest(label=label), self.assertRaises(AssertionError):
                assert_entrypoint_separation(self, relative, changed)

    def test_notification_registration_baseline_is_valid(self):
        relative = "legacy_main.rs"
        source = (ROOT / "crates/catabolic-cli/src" / relative).read_text()
        assert_entrypoint_separation(self, relative, source)

    def test_notification_registration_rejects_changed_moved_and_duplicate_blocks(self):
        relative = "legacy_main.rs"
        source = (ROOT / "crates/catabolic-cli/src" / relative).read_text()
        block = NOTIFICATION_HELPER_REGISTRATION
        for label, changed in (
            ("missing", source.replace(block, "", 1)),
            (
                "changed",
                source.replace(
                    "std::env::current_exe()", 'std::path::PathBuf::from("python")', 1
                ),
            ),
            ("duplicate", source + block),
            ("moved", source.replace(block, "", 1) + block),
            ("extra", source + "std::env::current_exe()"),
            ("supervisor missing", source.replace(SUPERVISOR_ENTRY, "", 1)),
            (
                "supervisor changed",
                source.replace(
                    "supervise_or_configure()", "supervise_or_configure(&path)", 1
                ),
            ),
            (
                "supervisor after registration",
                source.replace(SUPERVISOR_ENTRY, "", 1).replace(
                    block, block + SUPERVISOR_ENTRY, 1
                ),
            ),
        ):
            with self.subTest(label=label), self.assertRaises(AssertionError):
                assert_entrypoint_separation(self, relative, changed)

    def test_entrypoint_exclusions_still_reject_unapproved_calls(self):
        for relative in (
            "legacy_main.rs",
            "legacy/dispatch.rs",
            "legacy/parser.rs",
            "legacy/presentation.rs",
        ):
            source = (ROOT / "crates/catabolic-cli/src" / relative).read_text()
            for added in (
                "Command::new",
                "helper::register",
                "std::env::current_exe()",
            ):
                with self.subTest(relative=relative, added=added):
                    with self.assertRaises(AssertionError):
                        assert_entrypoint_separation(self, relative, source + added)


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
        # Preserve the extraction modulo the exact accepted evolution above,
        # without requiring a deep Git clone. This remains a source contract,
        # not scan behavior/full-report parity or accounting qualification.
        # Verified upstream source: Jagalite/catabolic commit
        # c69859eff3169545fddcb57e7d396744e48c738c,
        # crates/catabolic-python/src/embedding.rs, Git blob
        # 14b2f09af481cce56c67b8c3a6aa4635239d6399; full file SHA-256
        # d4617e91120f56797356aa8bf277c3222a7394e46ff341eea2397abecabc79f7.
        # Extract fn scan through the next register function, keep its body,
        # strip outer whitespace, and replace only the moved module prefix
        # catabolic_runtime::observation:: with crate::observation::.
        new = (ROOT / "crates/catabolic-runtime/src/public_scan.rs").read_text()
        assert_scan_extraction(self, new)
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
            assert_entrypoint_separation(self, relative, source)
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
        self.assertEqual(rows["rule run"]["status"], "partial")
        self.assertEqual(rows["rule run"]["owner_mapping"], "entrypoint")
        self.assertEqual(
            rows["rule run"]["native_owner"],
            "catabolic_runtime::rules::{dispatch,run_external} -> "
            "processor_mutation::{claim,job}, "
            "processors::{dispatch_with_transport,SupervisedTransport}",
        )
        self.assertTrue(rows["rule run"]["gap"])

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
