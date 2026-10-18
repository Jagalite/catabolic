# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Independent, parser-only differential qualification against frozen argparse.

Build ``cargo build -p catabolic-cli --example legacy_parse`` first. This never
calls CLI dispatch, opens a database, or makes a network request. The case tree
and expected results come from the actual frozen Python parser, never from the
native generated JSON resource. Missing dependencies are failures, not skips.

CATABOLIC_NATIVE_PARSER_BINARY, CATABOLIC_PYTHON_REFERENCE (src/), and
CATABOLIC_PYTHON_ORACLE select explicit qualification artifacts. Optional
CATABOLIC_LEGACY_PARSER_REPORT writes per-matrix counts and complete mismatches.
"""

import hashlib
import itertools
import json
import math
import os
import subprocess
import tempfile
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FROZEN_COMMIT = "453fca983222c6665a48775eef67c489c96b527e"
PINNED_PYTHON = "3.14.6"
BINARY = Path(
    os.environ.get(
        "CATABOLIC_NATIVE_PARSER_BINARY", ROOT / "target/debug/examples/legacy_parse"
    )
)
REFERENCE = Path(
    os.environ.get(
        "CATABOLIC_PYTHON_REFERENCE", "/tmp/catabolic-embedding-frozen-453fca9/src"
    )
).parent
PYTHON = Path(
    os.environ.get(
        "CATABOLIC_PYTHON_ORACLE",
        ROOT.parent / ".local-tools/catabolic-reference-3.14.6/bin/python",
    )
)

# This program imports only the reference checkout selected above. It exposes
# argparse's own action metadata for case generation and captures parse_args.
ORACLE = r"""
import argparse, contextlib, io, json, os, pathlib, sys
sys.path.insert(0, str(pathlib.Path(sys.argv[1]) / "src"))
from catabolic import cli
assert pathlib.Path(cli.__file__).resolve() == pathlib.Path(sys.argv[1]).resolve() / "src/catabolic/cli.py"
p = cli.parser()
def action(a):
    return {"dest": a.dest, "options": a.option_strings,
            "required": a.required, "nargs": a.nargs,
            "kind": type(a).__name__,
            "type": getattr(a.type, "__name__", None),
            "choices": list(a.choices) if a.choices is not None else None}
def walk(p, path):
    nodes = [{"path": path, "actions": [action(a) for a in p._actions],
              "groups": [{"required": g.required,
                          "members": [a.dest for a in g._group_actions]}
                         for g in p._mutually_exclusive_groups]}]
    for a in p._actions:
        if isinstance(a, argparse._SubParsersAction):
            for name, child in a.choices.items():
                nodes.extend(walk(child, path + [name]))
    return nodes
for line in sys.stdin:
    request = json.loads(line)
    if isinstance(request, dict):
        result = {"python": sys.version.split()[0], "module": cli.__file__,
                  "nodes": walk(p, [])}
    else:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            try:
                result = {"args": vars(p.parse_args(request))}
            except SystemExit as e:
                result = {"code": e.code, "stdout": out.getvalue(), "stderr": err.getvalue()}
        if "args" in result:
            assert out.getvalue() == err.getvalue() == ""
    print(json.dumps(result, ensure_ascii=False))
"""


def environment(columns=80, overrides=None):
    env = os.environ.copy()
    for key in ("CATABOLIC_DB", "CATABOLIC_PROFILE", "PYTHONPATH", "PYTHONHOME"):
        env.pop(key, None)
    env.update(
        {
            "COLUMNS": str(columns),
            "NO_COLOR": "1",
            "TERM": "dumb",
            "PYTHONUTF8": "1",
            "PYTHONINTMAXSTRDIGITS": "4300",
        }
    )
    if overrides:
        env.update(overrides)
    return env


def run_json_lines(command, requests, env):
    result = subprocess.run(
        command,
        input="".join(json.dumps(v, ensure_ascii=False) + "\n" for v in requests),
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=env,
        timeout=180,
    )
    if result.returncode:
        completed = len(result.stdout.splitlines())
        next_request = requests[completed] if completed < len(requests) else None
        raise AssertionError(
            f"{command[0]} exited {result.returncode} after {completed} results; "
            f"next request {next_request!r}: {result.stderr}"
        )
    if result.stderr:
        raise AssertionError(f"Unexpected probe process stderr: {result.stderr}")
    lines = result.stdout.splitlines()
    if len(lines) != len(requests):
        raise AssertionError(
            f"Probe produced {len(lines)} results for {len(requests)} requests"
        )

    # Retain an incorrectly accepted >4300-digit native integer as evidence
    # without Python's JSON decoder rejecting it before the comparison.
    def integer(token):
        return (
            {"__probe_integer_digits__": token}
            if len(token.lstrip("-")) > 4300
            else int(token)
        )

    return [json.loads(line, parse_int=integer) for line in lines]


FROZEN_REFERENCE_VERSION_OUTPUT = "catabolic 0.2.0\n"
RELEASE_VERSION = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"][
    "package"
]["version"]


def release_version(result):
    """Map only the frozen reference's exact --version output to this release.

    The reference reports its own package version; the native CLI reports the
    workspace release. No other output is rewritten.
    """
    if result.get("stdout") == FROZEN_REFERENCE_VERSION_OUTPUT:
        return {**result, "stdout": f"catabolic {RELEASE_VERSION}\n"}
    return result


def strict(value):
    """Do not let Python's True == 1 or 1 == 1.0 hide namespace drift."""
    if isinstance(value, dict):
        return {k: strict(v) for k, v in value.items()}
    if isinstance(value, list):
        return [strict(v) for v in value]
    if isinstance(value, float):
        return ["float", "nan" if math.isnan(value) else repr(value)]
    return [type(value).__name__, value]


class NativeLegacyParserTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        for file in (BINARY, PYTHON, REFERENCE / "src/catabolic/cli.py"):
            if not file.is_file():
                raise AssertionError(
                    f"Required parser qualification artifact missing: {file}"
                )
        from scripts.qualify_native_mutations import oracle_snapshot

        snapshot = oracle_snapshot(REFERENCE, ROOT)
        if snapshot["reference_commit"] != FROZEN_COMMIT:
            raise AssertionError("Frozen source provenance differs")
        cls.cache = tempfile.TemporaryDirectory(prefix="legacy-parser-oracle-pycache-")
        cls.addClassCleanup(cls.cache.cleanup)
        cls.oracle_command = [str(PYTHON), "-c", ORACLE, str(REFERENCE)]
        schema = run_json_lines(
            cls.oracle_command,
            [{}],
            environment(overrides={"PYTHONPYCACHEPREFIX": cls.cache.name}),
        )[0]
        if schema["python"] != PINNED_PYTHON:
            raise AssertionError(
                f"Frozen argparse interpreter {schema['python']} != {PINNED_PYTHON}"
            )
        cls.nodes = schema["nodes"]
        cls.index = {tuple(node["path"]): node for node in cls.nodes}
        cls.leaves = [
            node
            for node in cls.nodes
            if not any(a["kind"] == "_SubParsersAction" for a in node["actions"])
        ]
        if len(cls.nodes) != 271 or len(cls.leaves) != 237:
            raise AssertionError(
                f"Unexpected frozen tree: {len(cls.nodes)} nodes, {len(cls.leaves)} leaves"
            )
        cls.provenance = {
            "frozen_commit": snapshot["reference_commit"],
            "python": schema["python"],
            "module": schema["module"],
            "binary": str(BINARY.resolve()),
            "frozen_inventory_sha256": snapshot["inventory_sha256"],
            "node_count": len(cls.nodes),
            "leaf_count": len(cls.leaves),
        }

    @classmethod
    def sample(cls, action, second=False):
        if action["choices"]:
            return str(action["choices"][-1 if second else 0])
        return (
            "2"
            if action["type"] == "int"
            else "1.25"
            if action["type"] == "float"
            else "other-é😀"
            if second
            else "sample-é😀"
        )

    @classmethod
    def bundle(cls, action, values=None, spelling=None):
        option = [spelling or action["options"][0]] if action["options"] else []
        return (
            option
            if action["nargs"] == 0
            else option + (values if values is not None else [cls.sample(action)])
        )

    @classmethod
    def argv(cls, node, overrides=None):
        """Minimal valid leaf; overrides replace raw token bundles by (path,dest)."""
        overrides = overrides or {}
        leaf = (
            node
            if node in cls.leaves
            else next(
                x for x in cls.leaves if x["path"][: len(node["path"])] == node["path"]
            )
        )
        result = []
        for depth in range(len(leaf["path"]) + 1):
            path = tuple(leaf["path"][:depth])
            current = cls.index[path]
            included = {
                a["dest"]
                for a in current["actions"]
                if a["required"] and a["kind"] != "_SubParsersAction"
            }
            for group in current["groups"]:
                explicit = [
                    dest for dest in group["members"] if overrides.get((path, dest))
                ]
                if group["required"] and not explicit:
                    included.add(group["members"][0])
                if explicit:
                    included.difference_update(group["members"])
                    included.update(explicit)
            for action in current["actions"]:
                key = (path, action["dest"])
                if key in overrides:
                    result.extend(overrides[key] or [])
                elif action["dest"] in included:
                    result.extend(cls.bundle(action))
            if depth < len(leaf["path"]):
                result.append(leaf["path"][depth])
        return result

    def write_report(self, name, report):
        destination = os.environ.get("CATABOLIC_LEGACY_PARSER_REPORT")
        if destination:
            directory = Path(destination)
            directory.mkdir(parents=True, exist_ok=True)
            (directory / f"{name}.json").write_text(
                json.dumps(report, ensure_ascii=False, indent=2) + "\n",
                encoding="utf-8",
            )

    def compare(self, name, cases, columns=80, overrides=None, require_success=False):
        # Preserve duplicates with distinct labels: they represent different
        # independently generated parser features even when argv happens to match.
        env = environment(
            columns, {"PYTHONPYCACHEPREFIX": self.cache.name, **(overrides or {})}
        )
        binary_sha256 = hashlib.sha256(BINARY.read_bytes()).hexdigest()
        args = [case[1] for case in cases]
        metadata = {
            **self.provenance,
            "binary_sha256": binary_sha256,
            "matrix": name,
            "columns": columns,
            "environment_overrides": overrides or {},
            "cases": len(cases),
        }
        try:
            expected = run_json_lines(self.oracle_command, args, env)
            actual = run_json_lines([str(BINARY)], args, env)
        except (AssertionError, ValueError, subprocess.TimeoutExpired) as error:
            # A crashed run must replace the earlier report instead of leaving
            # stale success/failure evidence under the same matrix name.
            self.write_report(
                name, {**metadata, "outcome": "error", "error": str(error)}
            )
            raise
        if require_success:
            self.assertTrue(
                all("args" in result for result in expected),
                "Generated valid leaf unexpectedly fails frozen argparse",
            )
        self.assertEqual(
            binary_sha256,
            hashlib.sha256(BINARY.read_bytes()).hexdigest(),
            "Parser binary changed during matrix execution",
        )
        mismatches = [
            {"label": label, "argv": argv, "expected": want, "actual": got}
            for (label, argv), want, got in zip(cases, expected, actual, strict=True)
            if strict(release_version(want)) != strict(got)
        ]
        report = {
            **metadata,
            "outcome": "failed" if mismatches else "passed",
            "matched": len(cases) - len(mismatches),
            "mismatch_count": len(mismatches),
            "mismatches": mismatches,
        }
        self.write_report(name, report)
        examples = "\n".join(
            json.dumps(row, ensure_ascii=False) for row in mismatches[:4]
        )
        if mismatches:
            self.fail(f"{name}: {len(mismatches)}/{len(cases)} mismatches\n{examples}")

    def test_all_node_help_and_usage(self):
        cases = []
        for node in self.nodes:
            path = node["path"]
            for flag in ("-h", "--help"):
                cases.append((f"help:{path}:{flag}", path + [flag]))
            cases.append((f"usage-or-defaults:{path}", path))
            cases.append((f"unknown-option:{path}", path + ["--not-a-real-option"]))
            cases.append((f"unknown-word:{path}", path + ["not-a-real-subcommand"]))
        self.compare("all-node-help-usage", cases)

    def test_all_valid_leaf_namespaces(self):
        self.compare(
            "valid-leaf-defaults",
            [(" ".join(n["path"]), self.argv(n)) for n in self.leaves],
            require_success=True,
        )

    def test_argument_actions_and_missing_values(self):
        cases = []
        for node in self.nodes:
            path = tuple(node["path"])
            for a in node["actions"]:
                if a["kind"] in ("_SubParsersAction", "_HelpAction", "_VersionAction"):
                    continue
                key = (path, a["dest"])

                def add(label, bundle, path=path, a=a, node=node, key=key):
                    cases.append(
                        (f"{path}:{a['dest']}:{label}", self.argv(node, {key: bundle}))
                    )

                add("explicit", self.bundle(a))
                add("repeat", self.bundle(a) + self.bundle(a, [self.sample(a, True)]))
                if a["required"]:
                    add("missing-required", [])
                if a["options"]:
                    add("missing-value", [a["options"][0]])
                    add("equals", [a["options"][0] + "=" + self.sample(a)])
                    add("empty-equals", [a["options"][0] + "="])
                    if a["nargs"] != 0:
                        add("option-as-value", self.bundle(a, ["--not-a-real-option"]))
                if a["choices"]:
                    add("bad-choice", self.bundle(a, ["not-a-real-choice"]))
                    for choice in a["choices"]:
                        add(f"choice-{choice}", self.bundle(a, [str(choice)]))
        self.compare("argument-actions", cases)

    def test_abbreviations_and_short_options(self):
        cases = []
        for node in self.nodes:
            for a in node["actions"]:
                if a["kind"] == "_SubParsersAction":
                    continue
                key = (tuple(node["path"]), a["dest"])
                for option in a["options"]:
                    if option.startswith("--"):
                        for length in (3, max(3, len(option) - 1)):
                            if length < len(option):
                                short = option[:length]
                                cases.append(
                                    (
                                        f"{node['path']}:{short}",
                                        self.argv(
                                            node, {key: self.bundle(a, spelling=short)}
                                        ),
                                    )
                                )
        for args in (
            ["-hh"],
            ["-hfoo"],
            ["-h=foo"],
            ["--ver"],
            ["--vers"],
            ["--version"],
            ["--version=yes"],
            ["--j", "status"],
            ["--d", "status"],
            ["status", "--j"],
            ["--", "status"],
            ["--", "--help"],
            ["status", "--"],
            ["docs", "--", "--help"],
        ):
            cases.append(("explicit-short-option", args))
        self.compare("abbreviations", cases)

    def test_integer_unicode_and_numeric_boundaries(self):
        tokens = [
            "0",
            "-0",
            "+0",
            "1_000",
            "0001",
            " 12 ",
            "\t-12\n",
            "+",
            "-",
            "++1",
            "+-1",
            "-+1",
            "1__2",
            "_1",
            "1_",
            "0x10",
            "1.0",
            "1e2",
            "-1e2",
            "-1foo",
            "-1__2",
            "-١x",
            "--value has spaces",
            "-x y",
            "-.1bad",
            "-1.",
            "-.5",
            "NaN",
            "Infinity",
            "",
            "١٢",
            "１２",
            "𝟠",
            "²",
            "一",
            "\u00a012\u00a0",
            "\u200312\u2003",
            "\x1c12\x1c",
            "1\x00",
            "1\x7f",
            str(2**63 - 1),
            str(2**63),
            str(-(2**63) - 1),
            str(2**64 - 1),
            str(2**64),
            str(2**128),
            "9" * 4300,
            "9" * 4301,
        ]
        cases = []
        for node in self.nodes:
            for a in node["actions"]:
                if a["type"] != "int":
                    continue
                key = (tuple(node["path"]), a["dest"])
                for token in tokens:
                    cases.append(
                        (
                            f"{node['path']}:{a['dest']}:{token[:32]!r}",
                            self.argv(node, {key: self.bundle(a, [token])}),
                        )
                    )
        self.compare("integer-unicode-bounds", cases)

    def float_cases(self, tokens):
        cases = []
        for node in self.nodes:
            for a in node["actions"]:
                if a["type"] != "float":
                    continue
                for token in tokens:
                    key = (tuple(node["path"]), a["dest"])
                    cases.append(
                        (
                            f"{node['path']}:{token!r}",
                            self.argv(node, {key: self.bundle(a, [token])}),
                        )
                    )
        return cases

    def test_float_values(self):
        tokens = (
            "0",
            "-0",
            "1.5",
            "1e2",
            "-1e2",
            "-.5",
            "-1.",
            "1_000.25",
            "1__2",
            "١٢.٥",
            "１２.５",
            "1e-400",
            "",
            " ",
            "abc",
            "\u20031.5\u2003",
            "\x1c1.5\x1c",
            "0x1.2p3",
            "1.e2",
            "+.5",
            "1_2e1_0",
            "-1foo",
            "-.1bad",
            "--value has spaces",
        )
        self.compare("float-values", self.float_cases(tokens))

    def test_nonfinite_float_values(self):
        # Kept separate so an inability to encode these diagnostic namespaces
        # cannot conceal the remaining finite or invalid-float comparisons.
        tokens = (
            "inf",
            "-inf",
            "+Infinity",
            "nan",
            "NaN",
            "1e400",
            "-1e400",
            "-nan",
            "INF",
            "infinity",
        )
        cases = self.float_cases(tokens)
        for label, argv in list(cases):
            cases.append((label + ":equals", argv[:-2] + [argv[-2] + "=" + argv[-1]]))
        self.compare("nonfinite-float-values", cases)

    def test_mutually_exclusive_groups(self):
        cases = []
        for node in self.nodes:
            path = tuple(node["path"])
            actions = {a["dest"]: a for a in node["actions"]}
            for group in node["groups"]:
                cases.append(
                    (
                        f"{path}:group-empty",
                        self.argv(
                            node, {(path, dest): [] for dest in group["members"]}
                        ),
                    )
                )
                for dest in group["members"]:
                    cases.append(
                        (
                            f"{path}:group-{dest}",
                            self.argv(node, {(path, dest): self.bundle(actions[dest])}),
                        )
                    )
                for first, second in itertools.permutations(group["members"], 2):
                    # Inject in the selected order at the first member's location.
                    overrides = {(path, dest): [] for dest in group["members"]}
                    overrides[(path, group["members"][0])] = self.bundle(
                        actions[first]
                    ) + self.bundle(actions[second])
                    cases.append(
                        (f"{path}:group-{first}-{second}", self.argv(node, overrides))
                    )
        self.compare("mutual-exclusion", cases)

    def test_nargs_separators_and_option_placement(self):
        cases = []
        for node in self.nodes:
            for a in node["actions"]:
                if a["options"] or a["kind"] == "_SubParsersAction":
                    continue
                key = (tuple(node["path"]), a["dest"])
                for values in (
                    [],
                    ["-1"],
                    ["-1e2"],
                    ["-1foo"],
                    ["-x y"],
                    ["--literal value"],
                    ["-"],
                    ["--", "--literal"],
                    ["--", "one", "two"],
                    ["one", "two"],
                    ["--", "--"],
                    ["'\"\\\n\r\t\x00\x7f😀"],
                    ["--help"],
                ):
                    cases.append(
                        (
                            f"{node['path']}:{a['dest']}:{values!r}",
                            self.argv(node, {key: values}),
                        )
                    )
        for args in (
            ["tag", "add", "one", "--item", "1", "two"],
            ["tag", "add", "--", "-a", "-b"],
            ["--json", "docs", "--json"],
            ["docs", "--json", "topic"],
            ["--profile", "first", "--profile=last", "status"],
            ["item", "list", "--tag=one", "--tag", "two"],
            ["--unknown", "status"],
            ["status", "--unknown"],
            ["--unknown", "value", "status"],
            ["--unknown", "--help"],
            ["item", "list", "--limit", "--help"],
        ):
            cases.append(("explicit-option-placement", args))
        self.compare("nargs-separators", cases)

    def test_help_error_precedence_and_intermixed_positionals(self):
        cases = []
        for node in self.nodes:
            path = node["path"]
            for tail in (
                ["--help", "--not-a-real-option"],
                ["--not-a-real-option", "--help"],
                ["--not-a-real-option=bad", "--help"],
                ["not-a-real-subcommand", "--help"],
                ["--"],
                ["--", "--help"],
            ):
                cases.append((f"{path}:precedence:{tail}", path + tail))
            option_names = [
                option
                for action in node["actions"]
                for option in action["options"]
                if option.startswith("--")
            ]
            prefixes = {
                option[:length]
                for option in option_names
                for length in range(3, len(option))
            }
            for prefix in sorted(prefixes):
                if sum(option.startswith(prefix) for option in option_names) > 1:
                    cases.append(
                        (
                            f"{path}:help-before-ambiguous:{prefix}",
                            path + ["--help", prefix],
                        )
                    )
                    break
        for values in (
            [],
            ["first"],
            ["first", "second"],
            ["first", "second", "third"],
        ):
            for position in range(len(values) + 1):
                for option in (["--schema"], ["--max-rows", "3"], ["--unknown"]):
                    argv = ["query"] + values[:position] + option + values[position:]
                    cases.append(("optional-positionals-with-options", argv))
        for command in ("add", "remove"):
            for values in (
                ["first"],
                ["first", "second"],
                ["first", "second", "third"],
            ):
                for position in range(len(values) + 1):
                    for option in (["--item", "3"], ["--file=3"], ["--"]):
                        argv = (
                            ["tag", command]
                            + values[:position]
                            + option
                            + values[position:]
                        )
                        cases.append(("nargs-plus-with-options", argv))
        self.compare("help-error-precedence", cases)

    def test_environment_defaults_and_overrides(self):
        cases = [
            ("all-leaf-environment:" + " ".join(n["path"]), self.argv(n))
            for n in self.leaves
        ]
        cases.extend(
            [
                (
                    "explicit-environment-override",
                    ["--db", "/explicit/é.db", "--profile", "explicit", "status"],
                ),
                ("empty-environment-override", ["--db=", "--profile=", "status"]),
            ]
        )
        for i, env in enumerate(
            (
                {"CATABOLIC_DB": "/env/数据库.db", "CATABOLIC_PROFILE": "é😀"},
                {"CATABOLIC_DB": "", "CATABOLIC_PROFILE": ""},
                {"HOME": "/tmp/parser-only-homé"},
            )
        ):
            with self.subTest(environment=i):
                self.compare(
                    f"environment-{i}", cases, overrides=env, require_success=True
                )

    def test_terminal_widths(self):
        cases = [(f"{n['path']}:help", n["path"] + ["--help"]) for n in self.nodes]
        cases += [
            (f"{n['path']}:usage", n["path"] + ["--not-a-real-option"])
            for n in self.nodes
        ]
        for width in (40, 79, 81, 120):
            with self.subTest(columns=width):
                self.compare(f"terminal-width-{width}", cases, columns=width)


if __name__ == "__main__":
    unittest.main()
