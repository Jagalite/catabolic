# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Versioned exact CatalogQuery diagnostics at owner and companion boundaries.

Historical v1/v2 remain immutable. V3 adds bounded list provenance through
existing detailed boundaries. Unknown-profile and no-sort-keyword differences
remain separate residuals. Development artifacts are not installed-wheel evidence.
"""

import hashlib
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

from scripts.qualify_native_mutations import (
    oracle_snapshot,
    package_snapshot,
    source_snapshot,
    stage_completion_extension,
)
from tests import test_rust_legacy_parser as fixture

ROOT = Path(__file__).resolve().parents[1]
CONTRACT = json.loads(
    (ROOT / "tests/parity/catalog-query-sort-diagnostics-v1.json").read_text()
)
PREVIOUS = json.loads(
    (ROOT / "tests/parity/catalog-query-sort-diagnostics-v2.json").read_text()
)
CURRENT = json.loads(
    (ROOT / "tests/parity/catalog-query-sort-diagnostics-v3.json").read_text()
)
ORACLE = r"""
import json, pathlib, sys
sys.path.insert(0, sys.argv.pop(1))
from catabolic.store import Store
from catabolic.query import CatalogQuery
assert pathlib.Path(sys.modules['catabolic.query'].__file__).resolve() == pathlib.Path(sys.path[0]).resolve() / 'catabolic/query.py'
with Store(sys.argv[1]) as store:
    try:
        result = {'data': getattr(CatalogQuery(store, sys.argv[4] if len(sys.argv) > 4 else 'default'), sys.argv[3])(**json.loads(sys.argv[2]))}
    except Exception as error:
        result = {'error': str(error), 'type': type(error).__name__}
print(json.dumps(result, ensure_ascii=False, sort_keys=True, separators=(',', ':')))
"""

NATIVE_OWNER = (
    ORACLE.replace("from catabolic.", "from catabolic_native.")
    .replace("sys.modules['catabolic.query']", "sys.modules['catabolic_native.query']")
    .replace("'catabolic/query.py'", "'catabolic_native/query.py'")
    .replace(
        "with Store(sys.argv[1]) as store:",
        "from catabolic_native import _native\n"
        "assert pathlib.Path(_native.__file__).resolve() == "
        "pathlib.Path(sys.path[0]).resolve() / 'catabolic_native/_native.abi3.so'\n"
        "with Store(sys.argv[1]) as store:",
    )
)

STATELESS_OWNER = r"""
import json, pathlib, sys
sys.path.insert(0,sys.argv[1])
from catabolic_native import catalog_query, _native
assert pathlib.Path(_native.__file__).resolve() == pathlib.Path(sys.argv[1]).resolve() / 'catabolic_native/_native.abi3.so'
try:
    result = {'data':catalog_query(sys.argv[2],sys.argv[4],profile=sys.argv[5] if len(sys.argv)>5 else 'default',**json.loads(sys.argv[3]))}
except Exception as error:
    result = {'error':str(error),'type':type(error).__name__}
print(json.dumps(result,ensure_ascii=False,sort_keys=True,separators=(',',':')))
"""


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


class CatalogSortDiagnosticsTest(unittest.TestCase):
    def test_historical_fixture_and_full_baselines_remain_explicit(self):
        provenance = CONTRACT["provenance"]
        path = ROOT / provenance["historical_fixture"]
        self.assertEqual(digest(path), provenance["historical_fixture_sha256"])
        historical = json.loads(path.read_text())["cases"]
        for old, new in zip(historical, CONTRACT["cases"][:8], strict=True):
            self.assertEqual(fixture.strict(old["args"]), fixture.strict(new["args"]))
            self.assertEqual(
                fixture.strict(old["expected"]),
                fixture.strict(json.loads(new["oracle_owner"]["stdout"])),
            )
            baseline = new["baseline_companion"]
            if "error" in old["native"]:
                self.assertEqual(
                    baseline,
                    dict(
                        code=2,
                        stdout="",
                        stderr=f"catabolic: {old['native']['error']}\n",
                    ),
                )
            else:
                self.assertEqual(baseline["code"], 0)
                self.assertEqual(baseline["stderr"], "")
                self.assertEqual(json.loads(baseline["stdout"]), old["native"]["data"])
        self.assertEqual(sum(not c["diagnostic_parity"] for c in CONTRACT["cases"]), 1)
        self.assertEqual(
            sum(
                c["baseline_companion"] != c["expected_companion"]
                for c in CONTRACT["cases"]
            ),
            7,
        )

        previous = PREVIOUS["provenance"]
        self.assertEqual(
            digest(ROOT / previous["previous_contract"]),
            previous["previous_contract_sha256"],
        )
        self.assertEqual(len(PREVIOUS["cases"]), 25)
        for old, current in zip(CONTRACT["cases"], PREVIOUS["cases"][:17], strict=True):
            self.assertEqual(
                fixture.strict(old["args"]), fixture.strict(current["args"])
            )
            self.assertEqual(old["oracle_owner"], current["oracle_owner"])
            self.assertEqual(old["expected_companion"], current["baseline_companion"])
            self.assertTrue(current["diagnostic_parity"])
        self.assertEqual(sum(not c["diagnostic_parity"] for c in PREVIOUS["cases"]), 2)

        self.assertEqual(
            digest(ROOT / CURRENT["provenance"]["previous_contract"]),
            CURRENT["provenance"]["previous_contract_sha256"],
        )
        self.assertEqual(len(CURRENT["cases"]), 88)
        self.assertEqual(
            CURRENT["provenance"]["supported_methods"], ["items", "files", "mappings"]
        )
        for previous, current in zip(
            PREVIOUS["cases"], CURRENT["cases"][:25], strict=True
        ):
            self.assertEqual(previous["name"], current["name"])
            self.assertEqual(
                fixture.strict(previous["args"]), fixture.strict(current["args"])
            )
            self.assertEqual(previous["oracle_owner"], current["oracle_owner"])
            self.assertEqual(
                previous["expected_native_owner"], current["previous_native_owner"]
            )
            self.assertEqual(
                previous["expected_stateless_owner"],
                current["previous_stateless_owner"],
            )
            self.assertEqual(
                previous["expected_companion"], current["previous_companion"]
            )
        promoted = []
        for case in CURRENT["cases"]:
            self.assertIn(case["operation"], ("items", "files", "mappings"))
            self.assertEqual(case["profile"], "default")
            self.assertTrue(case["diagnostic_parity"])
            self.assertEqual(case["oracle_owner"], case["expected_native_owner"])
            if case["expected_native_owner"] != case["previous_native_owner"]:
                self.assertIsInstance(case["args"]["sort"], list)
                self.assertEqual(
                    json.loads(case["oracle_owner"]["stdout"]),
                    {
                        "error": "cannot use 'list' as a dict key (unhashable type: 'list')",
                        "type": "TypeError",
                    },
                )
                self.assertEqual(
                    json.loads(case["previous_native_owner"]["stdout"]),
                    {"error": "sort must be text", "type": "CatabolicError"},
                )
                promoted.append(case["name"])
        self.assertEqual(
            len(promoted), 26
        )  # Two historical plus 24 expanded witnesses.
        residuals = CURRENT["profile_precedence_residuals"]
        self.assertEqual(len(residuals), 6)
        self.assertTrue(
            all(c["profile"] == "array-sort-missing-profile" for c in residuals)
        )
        unsupported = CURRENT["no_sort_keyword_residuals"]
        self.assertEqual(
            [c["operation"] for c in unsupported],
            ["tags", "taggings", "associations", "relationships"],
        )
        for case in unsupported:
            self.assertEqual(
                json.loads(case["oracle_owner"]["stdout"]),
                {
                    "error": f"CatalogQuery.{case['operation']}() got an unexpected keyword argument 'sort'",
                    "type": "TypeError",
                },
            )

    def test_frozen_owner_and_native_complete_process_envelopes(self):
        binary = Path(os.environ["CATABOLIC_NATIVE_BINARY"])
        self.assertTrue(binary.is_file())
        binary_before = digest(binary)
        extension = Path(os.environ["CATABOLIC_NATIVE_EXTENSION"])
        self.assertTrue(extension.is_file())
        extension_before = digest(extension)
        staged = stage_completion_extension(
            ROOT, source_snapshot(ROOT), extension, extension_before
        )
        oracle_before = oracle_snapshot(fixture.REFERENCE, ROOT)
        self.assertEqual(
            oracle_before["reference_commit"],
            CONTRACT["provenance"]["reference_commit"],
        )
        env = fixture.environment()
        env["PYTHONDONTWRITEBYTECODE"] = "1"

        def invoke(argv):
            run = subprocess.run(
                argv, env=env, capture_output=True, text=True, timeout=90
            )
            return dict(code=run.returncode, stdout=run.stdout, stderr=run.stderr)

        self.assertEqual(
            invoke(
                [str(fixture.PYTHON), "-c", "import sys;print(sys.version.split()[0])"]
            ),
            dict(code=0, stdout=CONTRACT["provenance"]["python"] + "\n", stderr=""),
        )
        with tempfile.TemporaryDirectory(prefix="catalog-sort-diagnostics-") as tmp:
            root = Path(tmp)
            env["HOME"] = str(root / "home")
            db = root / "catalog.sqlite3"
            self.assertEqual(
                invoke(
                    [
                        str(fixture.PYTHON),
                        "-c",
                        "import sys;sys.path.insert(0,sys.argv[1]);from catabolic.store import Store;Store.initialize(sys.argv[2])",
                        str(fixture.REFERENCE / "src"),
                        str(db),
                    ]
                ),
                dict(code=0, stdout="", stderr=""),
            )
            db_before = digest(db)
            results = []
            residuals = [
                dict(
                    name=c["name"],
                    operation=c["operation"],
                    args=c["args"],
                    profile=c["profile"],
                    oracle_owner=c["frozen_owner"],
                    expected_native_owner=c["persistent_owner"],
                    expected_stateless_owner=c["stateless_owner"],
                    expected_companion=c["companion"],
                    diagnostic_parity=False,
                )
                for c in CURRENT["profile_precedence_residuals"]
            ]
            for case in CURRENT["cases"] + residuals:
                with self.subTest(case=case["name"]):
                    args = json.dumps(case["args"], ensure_ascii=False)
                    oracle = invoke(
                        [
                            str(fixture.PYTHON),
                            "-c",
                            ORACLE,
                            str(fixture.REFERENCE / "src"),
                            str(db),
                            args,
                            case["operation"],
                            case["profile"],
                        ]
                    )
                    native_owner = invoke(
                        [
                            str(fixture.PYTHON),
                            "-c",
                            NATIVE_OWNER,
                            staged["root"],
                            str(db),
                            args,
                            case["operation"],
                            case["profile"],
                        ]
                    )
                    stateless_owner = invoke(
                        [
                            str(fixture.PYTHON),
                            "-c",
                            STATELESS_OWNER,
                            staged["root"],
                            str(db),
                            args,
                            case["operation"],
                            case["profile"],
                        ]
                    )
                    native = invoke(
                        [
                            str(binary),
                            "--db",
                            str(db),
                            "--profile",
                            case["profile"],
                            "--json",
                            "catalog",
                            case["operation"],
                            "--options",
                            args,
                        ]
                    )
                    self.assertEqual(oracle, case["oracle_owner"])
                    self.assertEqual(native, case["expected_companion"])
                    self.assertEqual(native_owner, case["expected_native_owner"])
                    self.assertEqual(stateless_owner, case["expected_stateless_owner"])
                    if case["diagnostic_parity"]:
                        self.assertEqual(native_owner, oracle)
                    else:
                        self.assertNotEqual(native_owner, oracle)
                        self.assertEqual(
                            json.loads(native_owner["stdout"]),
                            {
                                "error": "unknown profile: array-sort-missing-profile",
                                "type": "CatabolicError",
                            },
                        )
                    results.append(
                        dict(
                            name=case["name"],
                            oracle=oracle,
                            native_owner=native_owner,
                            stateless_owner=stateless_owner,
                            native=native,
                        )
                    )
            self.assertEqual(digest(db), db_before)
            if report := os.environ.get("CATABOLIC_SORT_DIAGNOSTICS_REPORT"):
                Path(report).write_text(json.dumps(results, indent=2) + "\n")
        self.assertEqual(digest(binary), binary_before)
        self.assertEqual(digest(extension), extension_before)
        self.assertEqual(package_snapshot(Path(staged["root"])), staged["files"])
        self.assertEqual(oracle_snapshot(fixture.REFERENCE, ROOT), oracle_before)
        print(
            "Catalog sort v3: 88 matching bounded cases (25 historical + 63 expanded); "
            "six explicit profile precedence residuals; four unsupported keywords retained"
        )

    def test_typed_owner_recovery_and_exact_message_spoof_controls(self):
        extension = Path(os.environ["CATABOLIC_NATIVE_EXTENSION"])
        before = digest(extension)
        staged = stage_completion_extension(
            ROOT, source_snapshot(ROOT), extension, before
        )
        program = r"""
import json, pathlib, sys
sys.path.insert(0,sys.argv[1])
from catabolic_native import _native
from catabolic_native.store import Store, CatabolicError
from catabolic_native.query import CatalogQuery
assert pathlib.Path(_native.__file__).resolve() == pathlib.Path(sys.argv[1]).resolve() / 'catabolic_native/_native.abi3.so'
message="cannot use 'dict' as a dict key (unhashable type: 'dict')"
with Store(sys.argv[2], writable=True) as store:
    with store.transaction() as db:
        db.execute('CREATE TEMP TABLE sort_owner(value)')
        db.execute('INSERT INTO sort_owner VALUES(71)')
        query=CatalogQuery(store,'default')
        outcomes=[]
        for args in ({'sort':{}},{'sort':{'nested':[]}},{'sort':{},'limit':0},{'sort':[]},{'sort':'id'}):
            try:
                outcomes.append({'data':query.items(**args)})
            except Exception as failure:
                outcomes.append({'error':str(failure),'type':type(failure).__name__})
            assert db.in_transaction
            assert db.execute('SELECT value FROM sort_owner').fetchone()[0] == 71
        literal=message.replace("'","''")
        db.execute("CREATE TEMP TRIGGER sort_spoof BEFORE INSERT ON sort_owner BEGIN SELECT RAISE(ABORT,'"+literal+"'); END")
        try:
            db.execute('INSERT INTO sort_owner VALUES(99)')
        except Exception as failure:
            assert type(failure) is CatabolicError
            assert not isinstance(failure,TypeError)
            assert str(failure) == message
            outcomes.append({'error':str(failure),'type':type(failure).__name__})
        else:
            raise AssertionError('spoof trigger did not fail')
        assert db.in_transaction
        assert db.execute('SELECT value FROM sort_owner').fetchone()[0] == 71
        assert query.items() == {'items':[],'next_cursor':None}
    assert not store.db.in_transaction
    assert query.items() == {'items':[],'next_cursor':None}
print(json.dumps(outcomes,sort_keys=True,separators=(',',':')))
"""
        message = "cannot use 'dict' as a dict key (unhashable type: 'dict')"
        expected = [
            dict(error=message, type="TypeError"),
            dict(error=message, type="TypeError"),
            dict(error="limit must be between 1 and 1000", type="CatabolicError"),
            dict(
                error="cannot use 'list' as a dict key (unhashable type: 'list')",
                type="TypeError",
            ),
            dict(data=dict(items=[], next_cursor=None)),
            dict(error=message, type="CatabolicError"),
        ]
        with tempfile.TemporaryDirectory(prefix="sort-type-controls-") as tmp:
            db = Path(tmp) / "catalog.sqlite3"
            env = fixture.environment()
            env["PYTHONDONTWRITEBYTECODE"] = "1"
            env["HOME"] = str(Path(tmp) / "home")
            initialize = subprocess.run(
                [
                    str(fixture.PYTHON),
                    "-c",
                    "import sys;sys.path.insert(0,sys.argv[1]);from catabolic.store import Store;Store.initialize(sys.argv[2])",
                    str(fixture.REFERENCE / "src"),
                    str(db),
                ],
                env=env,
                capture_output=True,
                text=True,
                timeout=90,
            )
            self.assertEqual(
                (initialize.returncode, initialize.stdout, initialize.stderr),
                (0, "", ""),
            )
            db_before = digest(db)
            run = subprocess.run(
                [str(fixture.PYTHON), "-c", program, staged["root"], str(db)],
                env=env,
                capture_output=True,
                text=True,
                timeout=90,
            )
            self.assertEqual(
                dict(code=run.returncode, stdout=run.stdout, stderr=run.stderr),
                dict(
                    code=0,
                    stdout=json.dumps(expected, sort_keys=True, separators=(",", ":"))
                    + "\n",
                    stderr="",
                ),
            )
            self.assertEqual(digest(db), db_before)
        self.assertEqual(digest(extension), before)
        self.assertEqual(package_snapshot(Path(staged["root"])), staged["files"])

    def test_list_owner_recovery_and_exact_message_spoof_controls(self):
        extension = Path(os.environ["CATABOLIC_NATIVE_EXTENSION"])
        before = digest(extension)
        staged = stage_completion_extension(
            ROOT, source_snapshot(ROOT), extension, before
        )
        program = r"""
import json, pathlib, sys
sys.path.insert(0,sys.argv[1])
from catabolic_native import _native
from catabolic_native.store import Store, CatabolicError
from catabolic_native.query import CatalogQuery
assert pathlib.Path(_native.__file__).resolve() == pathlib.Path(sys.argv[1]).resolve() / 'catabolic_native/_native.abi3.so'
message="cannot use 'list' as a dict key (unhashable type: 'list')"
with Store(sys.argv[2], writable=True) as store:
    with store.transaction() as db:
        db.execute('CREATE TEMP TABLE sort_owner(value)')
        db.execute('INSERT INTO sort_owner VALUES(71)')
        changes=db.execute('SELECT total_changes()').fetchone()[0]
        query=CatalogQuery(store,'default')
        outcomes=[]
        for args in ({'sort':[]},{'sort':[{'nested':[]}]},{'sort':[],'limit':0},{'sort':[['id']]},{'sort':'id'}):
            try:
                outcomes.append({'data':query.items(**args)})
            except Exception as failure:
                outcomes.append({'error':str(failure),'type':type(failure).__name__})
            assert db.in_transaction
            assert db.execute('SELECT total_changes()').fetchone()[0] == changes
            assert db.execute('SELECT value FROM sort_owner').fetchone()[0] == 71
        literal=message.replace("'","''")
        db.execute("CREATE TEMP TRIGGER sort_spoof BEFORE INSERT ON sort_owner BEGIN SELECT RAISE(ABORT,'"+literal+"'); END")
        try:
            db.execute('INSERT INTO sort_owner VALUES(99)')
        except Exception as failure:
            assert type(failure) is CatabolicError
            assert not isinstance(failure,TypeError)
            assert str(failure) == message
            outcomes.append({'error':str(failure),'type':type(failure).__name__})
        else:
            raise AssertionError('spoof trigger did not fail')
        assert db.in_transaction
        assert db.execute('SELECT value FROM sort_owner').fetchone()[0] == 71
        assert query.items() == {'items':[],'next_cursor':None}
    assert not store.db.in_transaction
    assert query.items() == {'items':[],'next_cursor':None}
print(json.dumps(outcomes,sort_keys=True,separators=(',',':')))
"""
        message = "cannot use 'list' as a dict key (unhashable type: 'list')"
        expected = [
            dict(error=message, type="TypeError"),
            dict(error=message, type="TypeError"),
            dict(error="limit must be between 1 and 1000", type="CatabolicError"),
            dict(
                error="cannot use 'list' as a dict key (unhashable type: 'list')",
                type="TypeError",
            ),
            dict(data=dict(items=[], next_cursor=None)),
            dict(error=message, type="CatabolicError"),
        ]
        with tempfile.TemporaryDirectory(prefix="sort-type-controls-") as tmp:
            db = Path(tmp) / "catalog.sqlite3"
            env = fixture.environment()
            env["PYTHONDONTWRITEBYTECODE"] = "1"
            env["HOME"] = str(Path(tmp) / "home")
            initialize = subprocess.run(
                [
                    str(fixture.PYTHON),
                    "-c",
                    "import sys;sys.path.insert(0,sys.argv[1]);from catabolic.store import Store;Store.initialize(sys.argv[2])",
                    str(fixture.REFERENCE / "src"),
                    str(db),
                ],
                env=env,
                capture_output=True,
                text=True,
                timeout=90,
            )
            self.assertEqual(
                (initialize.returncode, initialize.stdout, initialize.stderr),
                (0, "", ""),
            )
            db_before = digest(db)
            run = subprocess.run(
                [str(fixture.PYTHON), "-c", program, staged["root"], str(db)],
                env=env,
                capture_output=True,
                text=True,
                timeout=90,
            )
            self.assertEqual(
                dict(code=run.returncode, stdout=run.stdout, stderr=run.stderr),
                dict(
                    code=0,
                    stdout=json.dumps(expected, sort_keys=True, separators=(",", ":"))
                    + "\n",
                    stderr="",
                ),
            )
            self.assertEqual(digest(db), db_before)
        self.assertEqual(digest(extension), before)
        self.assertEqual(package_snapshot(Path(staged["root"])), staged["files"])
