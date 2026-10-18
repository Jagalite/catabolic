# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Negative controls for installed legacy adapter wiring, not product parity."""

import argparse
import ast
import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import types
import unittest
from contextlib import ExitStack, redirect_stderr, redirect_stdout
from pathlib import Path
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/native_legacy_adapter_tests.py"


def load(name):
    spec = importlib.util.spec_from_file_location(
        name, ROOT / "scripts" / (name + ".py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


GATE = load("native_legacy_adapter_tests")
SCENARIOS = load("native_legacy_adapter_scenarios")
RESULTS = load("native_qualification_results")


class LegacyAdapterQualificationControls(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.report = self.root / "result.json"
        self.attempt = self.report.with_suffix(".attempt.json")
        self.environment = dict(os.environ)
        self.environment.pop("PYTHONOPTIMIZE", None)

    def gate(self, *arguments, flags=()):
        return subprocess.run(
            [sys.executable, "-I", "-B", *flags, str(SCRIPT), *arguments],
            cwd=self.root,
            env=self.environment,
            capture_output=True,
            text=True,
            timeout=30,
        )

    def seed(self):
        self.report.write_text("stale success")
        self.attempt.write_text("stale attempt")

    def test_canonical_scanner_matches_existing_receipt_boundary(self):
        def scanner(path):
            tree = ast.parse(path.read_text())
            return ast.dump(
                next(
                    node
                    for node in tree.body
                    if isinstance(node, ast.FunctionDef)
                    and node.name == "clear_stale_report"
                )
            )

        self.assertEqual(
            scanner(SCRIPT), scanner(ROOT / "scripts/native_facade_qualification.py")
        )

    def test_parser_preflight_and_optimization_retain_failed_attempt_only(self):
        for flags, arguments in (
            ((), ("--unknown",)),
            ((), ("--dev",)),
            (("-O",), ()),
            (("-OO",), ()),
            (
                (),
                tuple(
                    value
                    for name in (
                        "wheel",
                        "source-root",
                        "build-manifest",
                        "native-binary",
                        "legacy-binary",
                        "inventory",
                        "reference-root",
                        "reference-python",
                    )
                    for value in ("--" + name, str(self.root / "missing"))
                ),
            ),
        ):
            with self.subTest(flags=flags, arguments=arguments):
                self.seed()
                result = self.gate(
                    "--report", str(self.report), *arguments, flags=flags
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.report.exists())
                attempt = json.loads(self.attempt.read_text())
                self.assertFalse(attempt["passed"])
                self.assertIn("error", attempt)
                self.assertGreaterEqual(attempt["elapsed_seconds"], 0)
                self.assertFalse(attempt["release_qualified"])
                self.assertFalse(attempt["m9_complete"])

    def test_every_explicit_report_is_invalidated_and_gets_new_attempt(self):
        other = self.root / "other.json"
        self.seed()
        other.write_text("stale success")
        result = self.gate(
            "--report", str(self.report), "--report=" + str(other), "--invalid"
        )
        self.assertNotEqual(result.returncode, 0)
        for path in (self.report, other):
            self.assertFalse(path.exists())
            self.assertFalse(
                json.loads(path.with_suffix(".attempt.json").read_text())["passed"]
            )

    def test_abbreviations_and_delimiter_do_not_authorize_report_mutation(self):
        for arguments in (
            ("--rep", str(self.report)),
            ("--", "--report", str(self.report)),
        ):
            self.seed()
            result = self.gate(*arguments)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(self.report.read_text(), "stale success")
            self.assertEqual(self.attempt.read_text(), "stale attempt")

    def test_exact_inventory_and_stdlib_only_native_fixture_imports(self):
        SCENARIOS.validate_sources(ROOT)
        self.assertEqual(len(SCENARIOS.REQUIRED_IDS), 108)
        self.assertEqual(len(set(SCENARIOS.REQUIRED_IDS)), 108)
        self.assertEqual(
            {
                name.rpartition(".")[2]: len(methods)
                for name, methods in SCENARIOS.REQUIRED_CASES.items()
            },
            {
                "NativeDestinationMappingTest": 17,
                "NativeJellyfinDestinationMappingTest": 19,
                "PublicationMappingPreviewTest": 9,
                "PublicationBundlePreviewTest": 16,
                "NativeLegacyExternalRuleTest": 18,
                "NativeLegacyMappingHistoryTest": 11,
                "NativeLegacyMappingEventOrderTest": 6,
                "ExternalWorkerDiagnosticsTest": 12,
            },
        )
        witness = (
            "tests.test_rust_legacy_mapping_history.NativeLegacyMappingHistoryTest."
            "test_publication_version_two_without_identity_refuses_explicitly"
        )
        self.assertIn(witness, SCENARIOS.NONPARITY_WITNESSES)
        self.assertIn(witness, SCENARIOS.NATIVE_ONLY_WITNESSES)
        self.assertTrue(
            set(SCENARIOS.NONPARITY_WITNESSES) <= set(SCENARIOS.REQUIRED_IDS)
        )
        self.assertTrue(
            set(SCENARIOS.NATIVE_ONLY_WITNESSES) <= set(SCENARIOS.REQUIRED_IDS)
        )
        for relative in SCENARIOS.FIXTURE_FILES:
            for name in SCENARIOS.fixture_imports(
                ast.parse((ROOT / relative).read_text())
            ):
                if name.partition(".")[0] == "tests":
                    self.assertIn(
                        name.replace(".", "/") + ".py", SCENARIOS.FIXTURE_FILES
                    )
                else:
                    self.assertIn(name.partition(".")[0], sys.stdlib_module_names)

    def test_real_isolated_discovery_routes_inheritance_and_selection_owners(self):
        # Imports actual stdlib-only fixtures outside the checkout, without
        # loading business code, running fixtures or qualifying an old wheel.
        program = r"""
import importlib.util,json,pathlib,sys,unittest
from unittest.mock import patch
root = pathlib.Path(sys.argv[1])
def load(name):
    spec = importlib.util.spec_from_file_location(name, root / 'scripts' / (name + '.py'))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value
gate, scenarios = load('native_legacy_adapter_tests'), load('native_legacy_adapter_scenarios')
binary, reference, python = [pathlib.Path(value).absolute() for value in sys.argv[2:]]
modules = gate.load_fixtures(root, scenarios, binary, reference, python)
selection = gate.loaded_selection(modules, root, binary, reference, python)
suite, expected = gate.required_suite(modules, scenarios)
ids = list(load('native_qualification_results').scenario_ids(suite))
assert len(ids) == len(set(ids)) == 108
assert set(ids) == set(scenarios.REQUIRED_IDS)
assert not any(name == 'catabolic' or name.startswith('catabolic.') for name in sys.modules)
assert all(not pathlib.Path(value or '.').resolve().is_relative_to(root) for value in sys.path)
module = modules['tests.test_rust_legacy_external_worker_diagnostics']
case = module.ExternalWorkerDiagnosticsTest
assert len(unittest.defaultTestLoader.getTestCaseNames(case)) == 27
assert len([value for value in ids if value.startswith(case.__module__ + '.')]) == 9
assert case.__bases__ == (modules['tests.test_rust_legacy_external_rule'].NativeLegacyExternalRuleTest,)
def refuses(call, text):
    try:
        call()
    except RuntimeError as error:
        assert text in str(error), str(error)
    else:
        raise AssertionError('mutation admitted: ' + text)
for route in (
    lambda *args: unittest.TestSuite(),
    lambda *args: unittest.TestSuite([case('test_cancel_cleanup_neighbor')] * 9),
    lambda *args: unittest.defaultTestLoader.loadTestsFromTestCase(case),
):
    with patch.object(module, 'load_tests', route):
        refuses(lambda: gate.required_suite(modules, scenarios), 'own-method routing')
base = case.__bases__[0]
with patch.object(base, 'test_pending_same_worker_polls_once_without_new_generation', unittest.skip('forbidden')(lambda self: None)):
    refuses(lambda: gate.required_suite(modules, scenarios), 'is skipped')
original = case.__bases__
try:
    case.__bases__ = (unittest.TestCase,)
    refuses(lambda: gate.required_suite(modules, scenarios), 'inheritance')
finally:
    case.__bases__ = original
for name, alias in (
    ('tests.test_rust_legacy_mapping_history', 'projection'),
    ('tests.test_rust_legacy_mapping_event_order', 'history'),
    ('tests.test_rust_legacy_external_worker_diagnostics', 'fixture'),
    ('tests.test_rust_publication_bundle_preview', 'mapping_preview'),
):
    with patch.object(modules[name], alias, object()):
        refuses(lambda: gate.loaded_selection(modules, root, binary, reference, python), 'selection owner')
assert gate.loaded_selection(modules, root, binary, reference, python) == selection
print(json.dumps({'count': len(ids), 'unique': len(set(ids)), 'modules': list(modules),
                  'python_business_imports': [], 'product_scenarios_executed': 0}))
"""
        result = subprocess.run(
            [
                sys.executable,
                "-I",
                "-B",
                "-c",
                program,
                str(ROOT),
                str(self.root / "packaged-helper"),
                str(self.root / "frozen/src"),
                sys.executable,
            ],
            cwd=self.root,
            env=self.environment,
            capture_output=True,
            text=True,
            timeout=30,
        )
        if not Path("/proc/self/task").exists():
            # The mandatory Linux descriptor witness must fail closed here;
            # importing fixtures does not qualify unavailable product scenarios.
            # (The external-rule interrupt test no longer needs /proc since
            # 363bcb8 made tests/process_tree.py portable, so the first skipped
            # required method is the event-order one that still requires Linux
            # process descriptors.)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(
                "RuntimeError: legacy fixture method is skipped: "
                "tests.test_rust_legacy_mapping_event_order."
                "NativeLegacyMappingEventOrderTest."
                "test_real_sigint_discards_selected_sidecar_and_releases_snapshot",
                result.stderr,
            )
            self.assertEqual(result.stdout, "")
            return
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        evidence = json.loads(result.stdout)
        self.assertEqual(evidence["count"], 108)
        self.assertEqual(evidence["product_scenarios_executed"], 0)

    def test_new_inventory_routes_and_support_imports_fail_closed(self):
        for relative in SCENARIOS.FIXTURE_FILES:
            destination = self.root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / relative, destination)
        diagnostic = "tests/test_rust_legacy_external_worker_diagnostics.py"
        mutations = [
            (
                diagnostic,
                lambda text: text.replace(
                    "fixture.NativeLegacyExternalRuleTest):", "unittest.TestCase):"
                ),
            ),
            (
                diagnostic,
                lambda text: text.replace(
                    'if n.startswith("test_")', 'if n.startswith("test_redirect")'
                ),
            ),
            (
                diagnostic,
                lambda text: text.replace(
                    "def load_tests(", "def disabled_load_tests("
                ),
            ),
            (
                diagnostic,
                lambda text: (
                    text + "\ndef load_tests(*args):\n    return unittest.TestSuite()\n"
                ),
            ),
            (
                "tests/test_rust_legacy_projection.py",
                lambda text: text + "\nimport catabolic.app\n",
            ),
            (
                "tests/test_rust_legacy_projection.py",
                lambda text: text + "\nfrom tests import undeclared_fixture\n",
            ),
        ]
        for module, case in SCENARIOS.DECLARED_CASES.items():
            if (
                "mapping_history" in module
                or "mapping_event_order" in module
                or "worker_diagnostics" in module
            ):
                relative = module.rpartition(".")[0].replace(".", "/") + ".py"
                method = case[0]
                mutations.append(
                    (
                        relative,
                        lambda text, method=method: text.replace(
                            "def " + method + "(", "def removed_method("
                        ),
                    )
                )
                mutations.append(
                    (
                        relative,
                        lambda text: (
                            text
                            + "\nclass Hidden(unittest.TestCase):\n    def test_hidden(self): pass\n"
                        ),
                    )
                )
        for relative, mutate in mutations:
            target = self.root / relative
            original = target.read_text()
            target.write_text(mutate(original))
            with self.subTest(relative=relative), self.assertRaises(RuntimeError):
                SCENARIOS.validate_sources(self.root)
            target.write_text(original)
        self.assertEqual(
            SCENARIOS.validate_sources(self.root), SCENARIOS.validate_sources(ROOT)
        )

    def test_source_mutations_cannot_hide_missing_duplicate_added_or_inherited_tests(
        self,
    ):
        for relative in SCENARIOS.FIXTURE_FILES:
            destination = self.root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / relative, destination)
        target = self.root / "tests/test_rust_legacy_destination_mappings.py"
        original = target.read_text()
        method = "    def test_standard_preview_stdout_and_typed_dto_reports(self):"
        for changed in (
            original.replace(method, "    def removed_method(self):"),
            original + "\nclass Hidden(NativeDestinationMappingTest):\n    pass\n",
            original.replace(
                method, "    def test_added(self):\n        pass\n\n" + method
            ),
            original.replace(method, method + "\n        pass\n\n" + method),
            original.replace(
                "class NativeJellyfinDestinationMappingTest(NativeDestinationMappingTest):",
                "class NativeJellyfinDestinationMappingTest(unittest.TestCase):",
            ),
            original + "\ndef load_tests(*args):\n    return unittest.TestSuite()\n",
        ):
            target.write_text(changed)
            with self.assertRaises(RuntimeError):
                SCENARIOS.validate_sources(self.root)
        target.write_text(original)
        self.assertEqual(
            SCENARIOS.validate_sources(self.root), SCENARIOS.validate_sources(ROOT)
        )

    def make_launcher(self):
        manifest = self.root / "oracle.json"
        manifest.write_text("{}")
        verifier = Mock()
        launcher = GATE.BoundSubprocesses(
            self.root / "packaged-legacy",
            self.root / "pinned-python",
            self.root / "frozen/src",
            manifest,
            str(self.root / "cache"),
            verifier,
        )
        launcher.original = Mock(return_value=object())
        launcher.current = "exact.case.test_method"
        return launcher, verifier

    def test_all_native_launches_use_exact_packaged_helper_and_strip_python_source(
        self,
    ):
        launcher, verify = self.make_launcher()
        result = launcher(
            [str(launcher.binary), "--machine"],
            env={"PYTHONPATH": "/checkout", "PYTHONHOME": "/ambient"},
        )
        self.assertIs(result, launcher.original.return_value)
        verify.assert_called_once_with()
        command = launcher.original.call_args.args[0]
        self.assertEqual(command, [str(launcher.binary), "--machine"])
        self.assertNotIn("PYTHONPATH", launcher.original.call_args.kwargs["env"])
        self.assertNotIn("PYTHONHOME", launcher.original.call_args.kwargs["env"])
        for arguments, kwargs in (
            ([str(self.root / "target/debug/catabolic-legacy-native")], {}),
            ([sys.executable, "-c", "pass"], {}),
            ([str(launcher.binary)], {"shell": True}),
            ([str(launcher.binary)], {"executable": str(self.root / "other")}),
            (str(launcher.binary), {}),
        ):
            with self.assertRaises(RuntimeError):
                launcher(arguments, **kwargs)
        verify.side_effect = RuntimeError("artifact mutation")
        with self.assertRaisesRegex(RuntimeError, "artifact mutation"):
            launcher([str(launcher.binary)])

    def test_oracle_parent_and_child_receive_exact_pins_and_isolated_bootstrap(self):
        launcher, _ = self.make_launcher()
        program = "import sys; print(sys.argv)"
        launcher(
            [str(launcher.python), "-c", program, "argument"],
            env={"PYTHONPATH": str(launcher.reference)},
        )
        command = launcher.original.call_args.args[0]
        self.assertEqual(command[:3], [str(launcher.python), "-I", "-B"])
        self.assertEqual(command[-2:], [program, "argument"])
        self.assertIn("native_legacy_adapter_oracle.py", command[3])
        environment = launcher.original.call_args.kwargs["env"]
        self.assertEqual(environment["PYTHONPATH"], str(launcher.reference))
        self.assertEqual(environment["PYTHONPYCACHEPREFIX"], launcher.cache)
        self.assertEqual(environment["PYTHONDONTWRITEBYTECODE"], "1")
        self.assertEqual(environment["PYTHONSAFEPATH"], "1")
        self.assertEqual(environment["PYTHONNOUSERSITE"], "1")
        for command, environment in (
            ([str(launcher.python), "-c", program], {"PYTHONPATH": "/checkout/src"}),
            ([str(launcher.python.resolve()) + "-alias", "-c", program], {}),
            ([str(launcher.python), "-m", "catabolic.cli"], {}),
        ):
            with self.assertRaises(RuntimeError):
                launcher(command, env=environment)
        launcher.manifest.write_text('{"changed":true}')
        with self.assertRaisesRegex(RuntimeError, "manifest changed"):
            launcher([str(launcher.python), "-c", program], env={})

    def test_real_child_ignores_cwd_and_user_site_shadows_and_records_exact_bytes(self):
        # Synthetic frozen files test import selection only, not product behavior.
        frozen = self.root / "frozen"
        package = frozen / "src/catabolic"
        package.mkdir(parents=True)
        for name in ("__init__", "app", "store", "http_worker"):
            (package / (name + ".py")).write_text(
                "# isolated import-selection fixture\n"
            )
        oracle = {
            "root": str(frozen),
            "tracked_source_sha256": {
                str(path.relative_to(frozen)): GATE.digest(path)
                for path in package.iterdir()
            },
        }
        runtime = {
            "version": list(sys.version_info[:3]),
            "executable": sys.executable,
            "prefix": sys.prefix,
            "base_prefix": sys.base_prefix,
            "executable_sha256": GATE.digest(sys.executable),
        }
        shadow = self.root / "catabolic"
        shadow.mkdir()
        (shadow / "__init__.py").write_text("")
        (shadow / "http_worker.py").write_text("print('UNQUALIFIED_CHILD_IMPORT')\n")
        userbase = self.root / "userbase"
        usersite = (
            userbase
            / "lib"
            / f"python{sys.version_info.major}.{sys.version_info.minor}"
            / "site-packages"
        )
        usersite.mkdir(parents=True)
        marker = self.root / "user-site-was-executed"
        (usersite / "usercustomize.py").write_text(
            "from pathlib import Path;Path("
            + repr(str(marker))
            + ").write_text('unexpected')\n"
        )
        environment = dict(
            self.environment,
            PYTHONPATH=str(frozen / "src"),
            PYTHONUSERBASE=str(userbase),
        )
        for key in ("PYTHONSAFEPATH", "PYTHONNOUSERSITE"):
            environment.pop(key, None)
        result = subprocess.run(
            [sys.executable, "-m", "catabolic.http_worker"],
            cwd=self.root,
            env=environment,
            text=True,
            capture_output=True,
            check=True,
        )
        self.assertIn("UNQUALIFIED_CHILD_IMPORT", result.stdout)
        marker.unlink(missing_ok=True)
        cwd = Path.cwd()
        try:
            os.chdir(self.root)
            with patch.dict(os.environ, environment, clear=True):
                identity = GATE.reference_child_identity(
                    Path(sys.executable),
                    frozen / "src",
                    str(self.root / "cache"),
                    oracle,
                    runtime,
                )
                self.assertTrue(identity["flags"]["safe_path"])
                self.assertEqual(identity["flags"]["no_user_site"], 1)
                self.assertEqual(
                    identity["imports"]["catabolic.http_worker"]["path"],
                    str(package / "http_worker.py"),
                )
                self.assertEqual(
                    identity["imports"]["catabolic.http_worker"]["sha256"],
                    GATE.digest(package / "http_worker.py"),
                )
                self.assertFalse(marker.exists())
                (package / "http_worker.py").write_text("# changed source\n")
                with self.assertRaisesRegex(RuntimeError, "unqualified source"):
                    GATE.reference_child_identity(
                        Path(sys.executable),
                        frozen / "src",
                        str(self.root / "cache"),
                        oracle,
                        runtime,
                    )
        finally:
            os.chdir(cwd)

    def test_base_interpreter_user_site_hook_is_active_until_explicitly_disabled(self):
        # Venvs normally disable user-site even without our flag. Use the base
        # interpreter only for this synthetic startup control; never replace or
        # modify the pinned oracle environment.
        python = Path(sys._base_executable).absolute()
        environment = dict(self.environment, PYTHONUSERBASE=str(self.root / "userbase"))
        for name in ("PYTHONPATH", "PYTHONHOME", "PYTHONSAFEPATH", "PYTHONNOUSERSITE"):
            environment.pop(name, None)
        usersite = Path(
            subprocess.check_output(
                [
                    str(python),
                    "-P",
                    "-S",
                    "-c",
                    "import site;print(site.getusersitepackages())",
                ],
                env=environment,
                cwd=self.root,
                text=True,
            ).strip()
        )
        self.assertTrue(usersite.is_relative_to(self.root))
        usersite.mkdir(parents=True)
        marker = self.root / "active-user-site-hook"
        (usersite / "usercustomize.py").write_text(
            "from pathlib import Path;Path("
            + repr(str(marker))
            + ").write_text('activated')\n"
        )
        program = "import json,sys;print(json.dumps([sys.flags.safe_path,sys.flags.no_user_site]))"
        command = [str(python), "-c", program]
        unguarded = subprocess.check_output(
            command, env=environment, cwd=self.root, text=True
        )
        self.assertEqual(marker.read_text(), "activated")
        self.assertEqual(json.loads(unguarded), [False, 0])
        marker.unlink()
        guarded = subprocess.check_output(
            command,
            env=GATE.oracle_environment(
                environment, self.root / "explicit-frozen-src", str(self.root / "cache")
            ),
            cwd=self.root,
            text=True,
        )
        self.assertEqual(json.loads(guarded), [True, 1])
        self.assertFalse(marker.exists())

    def test_loaded_subprocess_module_path_or_program_mutation_is_refused(self):
        binary, reference, python = (
            self.root / name for name in ("legacy", "source", "venv/bin/python")
        )
        package = types.ModuleType("tests")
        package.__file__ = str(ROOT / "tests/__init__.py")
        helper = types.ModuleType("tests.test_rust_legacy_review")
        helper.__file__ = str(ROOT / "tests/test_rust_legacy_review.py")
        helper.BINARY, helper.REFERENCE, helper.PYTHON, helper.ORACLE = (
            binary,
            reference,
            python,
            "program",
        )
        modules = {"tests": package, helper.__name__: helper}
        with patch.dict(sys.modules, modules):
            expected = GATE.loaded_selection(modules, ROOT, binary, reference, python)
            for key in ("BINARY", "REFERENCE", "PYTHON", "__file__"):
                with patch.object(helper, key, self.root / "other"):
                    with self.assertRaises(RuntimeError):
                        GATE.loaded_selection(modules, ROOT, binary, reference, python)
            self.assertEqual(
                GATE.loaded_selection(modules, ROOT, binary, reference, python),
                expected,
            )
            with patch.dict(
                sys.modules, {helper.__name__: types.ModuleType(helper.__name__)}
            ):
                with self.assertRaises(RuntimeError):
                    GATE.loaded_selection(modules, ROOT, binary, reference, python)

    def test_once_only_zero_skips_and_both_engines_are_required(self):
        launcher, _ = self.make_launcher()
        holder = []

        class Fixture(unittest.TestCase):
            def test_pair(self):
                launcher.launches.extend(
                    {"scenario": self.id(), "engine": engine}
                    for engine in ("installed_legacy", "frozen_python")
                )

        expected = {Fixture: ("test_pair",)}
        with (
            patch.object(
                RESULTS,
                "QualificationResult",
                GATE.paired_result_class(RESULTS, launcher, holder),
            ),
            redirect_stderr(io.StringIO()),
        ):
            RESULTS.run_required_suite(
                unittest.TestSuite([Fixture("test_pair")]), expected, 1
            )
            with self.assertRaises(RuntimeError):
                RESULTS.run_required_suite(
                    unittest.TestSuite([Fixture("test_pair"), Fixture("test_pair")]),
                    expected,
                    1,
                )
            for replacement in (
                lambda self: None,
                unittest.skip("must fail")(lambda self: None),
                lambda self: self.fail("fixture failure"),
                lambda self: (_ for _ in ()).throw(ValueError("fixture error")),
            ):
                with patch.object(Fixture, "test_pair", replacement):
                    with self.assertRaises(RuntimeError):
                        RESULTS.run_required_suite(
                            unittest.TestSuite([Fixture("test_pair")]), expected, 1
                        )

    def harness(self, mutation=None, fail=False, business_import=False):
        """Synthetic engine calls exercise real gate sequencing, never parity."""
        binary = self.root / "legacy"
        binary.write_bytes(b"fixed packaged helper")
        inventory = self.root / "inventory.json"
        inventory.write_text("{}")
        args = argparse.Namespace(
            source_root=ROOT,
            reference_root=self.root / "frozen",
            reference_python=self.root / "pinned-python",
            inventory=inventory,
            wheel=self.root / "wheel",
            build_manifest=self.root / "build.json",
            native_binary=self.root / "native",
            legacy_binary=binary,
            report=self.report,
        )
        artifact = {
            "installed_wheel": {"installed_payloads_sha256": {}},
            "build": {"legacy_binary_sha256": GATE.digest(binary)},
        }
        strict = Mock(
            return_value=artifact,
            side_effect=[artifact, {}] if mutation == "artifact" else None,
        )
        identity = types.SimpleNamespace(verify=Mock())
        qualification = types.SimpleNamespace(
            FROZEN_COMMIT="frozen",
            NativeIdentity=Mock(return_value=identity),
            strict_artifact=strict,
            inventory_rows=Mock(),
            reference_runtime=Mock(
                return_value={},
                side_effect=[{}, {"changed": True}] if mutation == "runtime" else None,
            ),
        )
        oracle = {"tracked_source_sha256": {}}
        provenance = types.SimpleNamespace(
            reference_provenance=Mock(
                return_value=oracle,
                side_effect=[oracle, {}] if mutation == "oracle" else None,
            )
        )
        native = types.ModuleType("catabolic_native")
        cli = types.ModuleType("catabolic_native.cli")
        cli.legacy_binary = lambda: binary
        native.cli = cli
        launcher_holder = []
        real_launcher = GATE.BoundSubprocesses

        def make_launcher(*arguments):
            launcher = real_launcher(*arguments)
            launcher.original = Mock()
            launcher_holder.append(launcher)
            return launcher

        if business_import:
            business = self.root / "fixture-business" / "catabolic"
            business.mkdir(parents=True, exist_ok=True)
            (business / "__init__.py").write_text(
                "# disposable business import witness\n"
            )
            (business / "fixture_owner.py").write_text("FIXTURE_VALUE = 41\n")

        class Fixture(unittest.TestCase):
            def test_pair(self):
                if business_import:
                    owner = importlib.import_module("catabolic.fixture_owner")
                    self.assertEqual(owner.FIXTURE_VALUE, 41)
                subprocess.Popen([str(binary)])
                subprocess.Popen([str(args.reference_python), "-c", "pass"], env={})
                if fail:
                    self.fail("synthetic fixture failure")

        scenarios = types.SimpleNamespace(
            REQUIRED_IDS=(Fixture("test_pair").id(),),
            REQUIRED_COUNT=1,
            NONPARITY_WITNESSES=(),
            NATIVE_ONLY_WITNESSES=(),
        )
        imported = Mock(return_value={})
        if mutation == "imports":
            imported.side_effect = [{}, RuntimeError("imported bytes changed")]
        modules = {
            "native_facade_qualification": qualification,
            "qualify_native_embedding": provenance,
            "native_legacy_adapter_scenarios": scenarios,
            "native_qualification_results": RESULTS,
            "native_asgi_installed_tests": types.SimpleNamespace(
                ImportedBytes=Mock(
                    return_value=types.SimpleNamespace(snapshot=imported)
                )
            ),
        }
        self.last_report = {"passed": False}
        with ExitStack() as stack:
            stack.enter_context(patch.object(GATE, "preflight", return_value=ROOT))
            stack.enter_context(
                patch.object(GATE, "sibling", side_effect=modules.__getitem__)
            )
            stack.enter_context(
                patch.object(
                    GATE,
                    "source_fence",
                    return_value={"files": {}},
                    side_effect=[{"files": {}}, {"changed": True}]
                    if mutation == "source"
                    else None,
                )
            )
            stack.enter_context(patch.object(GATE, "runtime_identity", return_value={}))
            stack.enter_context(
                patch.object(
                    GATE,
                    "reference_child_identity",
                    return_value={},
                    side_effect=[{}, {"changed": True}]
                    if mutation == "child"
                    else None,
                )
            )
            stack.enter_context(patch.object(GATE, "load_fixtures", return_value={}))
            stack.enter_context(
                patch.object(
                    GATE,
                    "loaded_selection",
                    return_value={},
                    side_effect=[{}, {}, {"changed": True}]
                    if mutation == "selection"
                    else None,
                )
            )
            stack.enter_context(
                patch.object(
                    GATE,
                    "required_suite",
                    return_value=(
                        unittest.TestSuite([Fixture("test_pair")]),
                        {Fixture: ("test_pair",)},
                    ),
                )
            )
            stack.enter_context(
                patch.object(GATE, "BoundSubprocesses", side_effect=make_launcher)
            )
            stack.enter_context(
                patch.dict(
                    sys.modules,
                    {"catabolic_native": native, "catabolic_native.cli": cli},
                )
            )
            # This mock represents a fresh installed-gate interpreter, whereas
            # aggregate source discovery may already have imported the frozen
            # Python owners. Hide only that preexisting namespace for the mock;
            # patch.dict restores every original module object on all exits.
            # New imports during the fixture remain visible to the real fence.
            for name in tuple(sys.modules):
                if name == "catabolic" or name.startswith("catabolic."):
                    del sys.modules[name]
            if business_import:
                stack.enter_context(
                    patch.object(sys, "path", [str(business.parent), *sys.path])
                )
            stack.enter_context(patch.object(sys, "pycache_prefix", None))
            stack.enter_context(patch.object(sys, "dont_write_bytecode", False))
            stack.enter_context(redirect_stderr(io.StringIO()))
            GATE.run_gate(args, self.last_report)
        return self.last_report, strict, identity

    def test_mock_gate_restores_preloaded_business_modules_after_every_outcome(self):
        preloaded = {
            name: types.ModuleType(name)
            for name in (
                "catabolic",
                "catabolic.app",
                "catabolic.store",
                "catabolic_fixture",
            )
        }
        with patch.dict(sys.modules, preloaded):
            report, _, _ = self.harness()
            self.assertTrue(report["passed"])
            for name, module in preloaded.items():
                self.assertIs(sys.modules[name], module)
            with self.assertRaisesRegex(RuntimeError, "every scenario"):
                self.harness(fail=True)
            for name, module in preloaded.items():
                self.assertIs(sys.modules[name], module)
            with self.assertRaisesRegex(RuntimeError, "imported Python business logic"):
                self.harness(business_import=True)
            for name, module in preloaded.items():
                self.assertIs(sys.modules[name], module)
            self.assertNotIn("catabolic.fixture_owner", sys.modules)

    def test_actual_business_import_inside_mock_fixture_still_rejects(self):
        with self.assertRaisesRegex(RuntimeError, "imported Python business logic"):
            self.harness(business_import=True)
        # The physical module was imported and the fixture passed before the
        # unchanged postflight admission fence rejected the process.
        self.assertEqual(self.last_report["tests_run"], 1)
        self.assertEqual(len(self.last_report["successful_scenarios"]), 1)
        self.assertFalse(self.last_report["passed"])
        self.assertNotIn("catabolic.fixture_owner", sys.modules)

    def test_gate_uses_strict_installed_fences_before_and_after(self):
        report, strict, identity = self.harness()
        self.assertTrue(report["passed"])
        self.assertEqual(report["tests_run"], 1)
        self.assertEqual(strict.call_count, 2)
        self.assertEqual(strict.call_args_list[0], strict.call_args_list[1])
        self.assertEqual(identity.verify.call_count, 2)
        self.assertTrue(report["source_stable"])
        self.assertTrue(report["artifact_stable"])
        self.assertEqual(
            [row["engine"] for row in report["launches"]],
            ["installed_legacy", "frozen_python"],
        )

    def test_gate_rejects_each_postflight_mutation_and_keeps_failed_outcomes(self):
        for mutation in (
            "artifact",
            "source",
            "oracle",
            "runtime",
            "selection",
            "imports",
            "child",
        ):
            with self.subTest(mutation=mutation), self.assertRaises(RuntimeError):
                self.harness(mutation=mutation)
            self.assertFalse(self.last_report["passed"])
        with self.assertRaisesRegex(RuntimeError, "every scenario"):
            self.harness(fail=True)
        self.assertFalse(self.last_report["passed"])
        self.assertEqual(self.last_report["failures"], 1)
        self.assertEqual(self.last_report["successful_scenarios"], [])
        self.assertTrue(self.last_report["source_stable"])
        self.assertTrue(self.last_report["artifact_stable"])

    def test_main_retains_failed_run_counts_without_success_receipt(self):
        self.seed()
        arguments = [
            value
            for name in (
                "wheel",
                "source-root",
                "build-manifest",
                "native-binary",
                "legacy-binary",
                "inventory",
                "reference-root",
                "reference-python",
            )
            for value in ("--" + name, str(self.root / name))
        ]
        arguments += ["--report", str(self.report)]

        def fail(args, report):
            report.update(tests_run=3, failures=1, artifact_stable=True)
            raise RuntimeError("synthetic failure after fixtures")

        with (
            patch.object(GATE, "run_gate", side_effect=fail),
            redirect_stdout(io.StringIO()),
        ):
            self.assertEqual(GATE.main(arguments), 1)
        self.assertFalse(self.report.exists())
        attempt = json.loads(self.attempt.read_text())
        self.assertFalse(attempt["passed"])
        self.assertEqual(attempt["tests_run"], 3)
        self.assertEqual(attempt["failures"], 1)
        self.assertTrue(attempt["artifact_stable"])

    def test_ci_uses_exact_strict_command_and_always_uploads_attempts(self):
        from tests.native_ci_contract import require_installed_gate

        workflow = (ROOT / ".github/workflows/native-qualification.yml").read_text()
        require_installed_gate(
            workflow,
            SCRIPT.name,
            "native-legacy-adapters",
            (
                '--native-binary "$root/dist/native/catabolic-native"',
                '--legacy-binary "$root/dist/native/catabolic-legacy-native"',
            ),
        )


if __name__ == "__main__":
    unittest.main()
