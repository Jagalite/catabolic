#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Classify reference Python imports, including installed acceptance snippets."""

import argparse
import ast
import json
import sys
from pathlib import Path

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from scripts.migration_contracts import REFERENCE, sha, verify_source


def imports(text):
    try:
        tree = ast.parse(text)
    except SyntaxError:
        return set()
    found = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom) and (node.module or "").startswith(
            "catabolic"
        ):
            found.update(f"{node.module}.{name.name}" for name in node.names)
        elif isinstance(node, ast.Import):
            found.update(
                name.name for name in node.names if name.name.startswith("catabolic")
            )
        elif (
            isinstance(node, ast.Constant)
            and isinstance(node.value, str)
            and "import " in node.value
        ):
            # CLI acceptance drivers embed Python programs in string literals.
            found.update(imports(node.value))
    return found


def signatures(path):
    result = {}

    def record(node, prefix=""):
        name = prefix + node.name
        if isinstance(node, ast.ClassDef):
            result[name] = dict(
                kind="class",
                bases=[ast.unparse(b) for b in node.bases],
                docstring=ast.get_docstring(node),
            )
            for child in node.body:
                if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef)):
                    record(child, name + ".")
        else:
            result[name] = dict(
                kind="function",
                arguments=ast.unparse(node.args),
                returns=ast.unparse(node.returns) if node.returns else None,
                decorators=[ast.unparse(d) for d in node.decorator_list],
                docstring=ast.get_docstring(node),
            )

    for node in ast.parse(path.read_text()).body:
        if isinstance(node, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)):
            record(node)
    return result


def build(source, inventory, inventory_hash):
    verify_source(source, inventory)
    uses = {}
    for folder in ("scripts", "tests"):
        for path in sorted((source / folder).rglob("*.py")):
            for name in imports(path.read_text()):
                uses.setdefault(name, []).append(path.relative_to(source).as_posix())
    rows = []
    for module in inventory["python_candidates"]:
        module = {**module, "module": module["module"].removesuffix(".__init__")}
        contracts = signatures(source / module["source"])
        symbols = list(module["symbols"])
        symbols.extend(
            name
            for name in contracts
            if name.endswith((".__init__", ".__enter__", ".__exit__"))
            and name.split(".")[0] in symbols
        )
        for symbol in symbols:
            name = module["module"] + "." + symbol
            # An imported class carries its public methods, not just its constructor.
            evidence = sorted(
                {
                    p
                    for key, paths in uses.items()
                    if key == module["module"]
                    or key == name
                    or key == module["module"] + "." + symbol.split(".")[0]
                    for p in paths
                }
            )
            scripts = [p for p in evidence if p.startswith("scripts/")]
            classification = (
                "preserve-repository-consumer"
                if scripts
                else "test-only-observed"
                if evidence
                else "no-external-import-observed"
            )
            if name == "catabolic.cli.main":
                classification = "preserve-console-entry-point"
            if symbol.split(".")[0] in ("Application", "Store") and module[
                "module"
            ] in ("catabolic.app", "catabolic.store"):
                classification = "preserve-embedding-facade"
            rows.append(
                dict(
                    id="python:" + name,
                    owner="A" if module["module"] == "catabolic.store" else "F",
                    classification=classification,
                    source=module["source"],
                    signature=contracts[symbol],
                    consumers=evidence,
                    reference_tests=[
                        p[:-3].replace("/", ".")
                        for p in evidence
                        if p.startswith("tests/test_")
                    ],
                    proof_plan=[
                        "python",
                        "-m",
                        "unittest",
                        "discover",
                        "-s",
                        "tests",
                        "-q",
                    ],
                )
            )
    indexed = {row["id"].removeprefix("python:") for row in rows}
    for name, evidence in sorted(uses.items()):
        if name in indexed or not any(p.startswith("scripts/") for p in evidence):
            continue
        parts = name.split(".")
        candidates = [
            source / "src" / Path(*parts[:count]) for count in range(len(parts), 0, -1)
        ]
        locations = [
            path
            for base in candidates
            for path in (base.with_suffix(".py"), base / "__init__.py")
            if path.is_file()
        ]
        if not locations:
            raise ValueError(f"Unresolved repository Python import: {name}")
        location = locations[0]
        rows.append(
            dict(
                id="python:" + name,
                owner="F",
                classification="preserve-repository-import-binding",
                source=location.relative_to(source).as_posix(),
                signature=dict(
                    kind="module-or-value-or-reexport",
                    contract="Preserve the imported binding and its value/type from the hashed source resource; no new callable signature inferred.",
                ),
                consumers=evidence,
                reference_tests=[
                    p[:-3].replace("/", ".")
                    for p in evidence
                    if p.startswith("tests/test_")
                ],
                proof_plan=[
                    "python",
                    "-m",
                    "unittest",
                    "discover",
                    "-s",
                    "tests",
                    "-q",
                ],
            )
        )
    return dict(
        format_version=1,
        reference_commit=REFERENCE,
        inventory_sha256=inventory_hash,
        policy="Preserve repository consumers and embedding facades. Test-only/no-import classifications do not authorize removal; dynamic imports and unknown downstream consumers require review before retirement. No database connection attribute is declared public merely because tests use it.",
        installation=dict(
            package="catabolic",
            python=">=3.11",
            console="catabolic.cli:main",
            extras=["watch", "http", "notifications", "openapi", "dev"],
            proof="CI exact-wheel installation and installed acceptance lanes",
        ),
        rows=rows,
        import_uses=uses,
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument(
        "--inventory", type=Path, default=Path("tests/parity/inventory.json")
    )
    parser.add_argument(
        "--output", type=Path, default=Path("tests/parity/python-api.json")
    )
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    raw = args.inventory.read_bytes()
    result = (
        json.dumps(
            build(args.source.resolve(), json.loads(raw), sha(raw)),
            indent=2,
            sort_keys=True,
        )
        + "\n"
    )
    if args.check:
        if args.output.read_text() != result:
            raise SystemExit("Python API inventory drift")
    else:
        with args.output.open("x") as stream:
            stream.write(result)
    print("Python API inventory verified")


if __name__ == "__main__":
    main()
