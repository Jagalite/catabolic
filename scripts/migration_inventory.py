#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Inventory a Python reference checkout without executing catalog operations.

Run in a fresh process with --source pointing at an exported, pinned reference.
The generated scope is evidence for M0 review, not a completed parity claim.
"""

import argparse
import ast
import hashlib
import json
import os
import sys
import tomllib
from pathlib import Path


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def value(item):
    if item is None or isinstance(item, (str, int, float, bool)):
        return item
    if isinstance(item, (list, tuple)):
        return [value(part) for part in item]
    if isinstance(item, dict):
        return {str(key): value(part) for key, part in item.items()}
    if callable(item):
        return f"{item.__module__}.{item.__qualname__}"
    raise TypeError(f"unsupported parser value: {type(item).__name__}")


def cli_inventory(parser, path=(), *, user_home=None):
    """Keep group nodes, aliases, defaults and inherited option scopes explicit."""
    actions = []
    children = []
    for action in parser._actions:
        if isinstance(action, argparse._SubParsersAction):
            actions.append(
                dict(
                    dest=action.dest,
                    action=type(action).__name__,
                    required=action.required,
                    choices=list(action.choices),
                )
            )
            for name, child in action.choices.items():
                children.extend(
                    cli_inventory(child, (*path, name), user_home=user_home)
                )
        else:
            actions.append(
                {
                    key: value(getattr(action, key, None))
                    for key in (
                        "dest",
                        "option_strings",
                        "default",
                        "required",
                        "nargs",
                        "const",
                        "type",
                        "choices",
                        "help",
                        "metavar",
                        "version",
                    )
                }
                | {"action": type(action).__name__}
            )
            default = actions[-1]["default"]
            if (
                user_home
                and isinstance(default, str)
                and (default == user_home or default.startswith(user_home + "/"))
            ):
                actions[-1]["default"] = "${HOME}" + default[len(user_home) :]
                actions[-1]["default_encoding"] = "home-relative-path"
    groups = [
        {
            "required": group.required,
            "members": [action.dest for action in group._group_actions],
        }
        for group in parser._mutually_exclusive_groups
    ]
    return [
        dict(
            id="cli:" + (" ".join(path) or "<root>"),
            entry_point="catabolic " + " ".join(path),
            arguments=actions,
            mutually_exclusive_groups=groups,
            parser_defaults=value(parser._defaults),
        )
    ] + children


def python_candidates(source):
    """Public-looking symbols are candidates, not promises of public support."""
    result = []
    for path in sorted((source / "src/catabolic").rglob("*.py")):
        module = ".".join(path.relative_to(source / "src").with_suffix("").parts)
        tree = ast.parse(path.read_text())
        names = []
        for node in tree.body:
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                if not node.name.startswith("_"):
                    names.append(node.name)
                    if isinstance(node, ast.ClassDef):
                        names.extend(
                            f"{node.name}.{child.name}"
                            for child in node.body
                            if isinstance(
                                child, (ast.FunctionDef, ast.AsyncFunctionDef)
                            )
                            and not child.name.startswith("_")
                        )
        if names:
            result.append(
                dict(
                    module=module,
                    source=path.relative_to(source).as_posix(),
                    symbols=names,
                    classification="needs-public-api-review",
                )
            )
    return result


def inventory(source):
    source = source.resolve()
    if any(
        name == "catabolic" or name.startswith("catabolic.") for name in sys.modules
    ):
        raise RuntimeError(
            "inventory requires a fresh process before importing Catabolic"
        )
    # Environment-derived defaults must not expose or freeze a user's configuration.
    for key in list(os.environ):
        if key.startswith("CATABOLIC_"):
            del os.environ[key]
    sys.path.insert(0, str(source / "src"))
    from catabolic import cli
    from catabolic.http.contract import VERSION
    from catabolic.migration import SCHEMA_VERSION

    if Path(cli.__file__).resolve() != source / "src/catabolic/cli.py":
        raise RuntimeError("reference import escaped the requested source tree")
    package = source / "src/catabolic"
    project = tomllib.loads((source / "pyproject.toml").read_text())["project"]
    openapi_path = package / "http/releases" / VERSION / "openapi.json"
    openapi = json.loads(openapi_path.read_text())
    http = []
    for path, methods in sorted(openapi["paths"].items()):
        for method, operation in sorted(methods.items()):
            if method not in {
                "get",
                "put",
                "post",
                "delete",
                "options",
                "head",
                "patch",
                "trace",
            }:
                continue
            http.append(
                dict(
                    id=f"http:{method.upper()} {path}",
                    operation_id=operation["operationId"],
                    contract=operation,
                )
            )
    files = {}
    for directory in (
        "src",
        "requirements",
        "tests",
        "scripts",
        "docs",
        ".github/workflows",
    ):
        for path in sorted((source / directory).rglob("*")):
            if (
                path.is_file()
                and "__pycache__" not in path.parts
                and not any(part.endswith(".egg-info") for part in path.parts)
            ):
                files[path.relative_to(source).as_posix()] = digest(path)
    for name in ("pyproject.toml", "MANIFEST.in", "LICENSE", "README.md"):
        files[name] = digest(source / name)
    tests = []
    for path in sorted((source / "tests").rglob("test_*.py")):
        tree = ast.parse(path.read_text())
        module = ".".join(path.relative_to(source).with_suffix("").parts)
        tests.extend(
            f"{module}.{node.name}.{child.name}"
            for node in tree.body
            if isinstance(node, ast.ClassDef)
            for child in node.body
            if isinstance(child, ast.FunctionDef) and child.name.startswith("test_")
        )
    migrations = [
        dict(
            id=f"migration:{path.stem}",
            source=path.relative_to(source).as_posix(),
            sha256=digest(path),
        )
        for path in sorted((package / "migrations").glob("*.sql"))
    ]
    return dict(
        format_version=1,
        package_version=project["version"],
        schema_version=SCHEMA_VERSION,
        http_version=VERSION,
        requires_python=project["requires-python"],
        extras=project["optional-dependencies"],
        entry_points=project["scripts"],
        environment_policy="CATABOLIC_* unset; only home-relative default path prefixes encoded as ${HOME}; environment-specific defaults require separate scenarios",
        cli=cli_inventory(cli.parser(), user_home=str(Path.home())),
        http=http,
        migrations=migrations,
        python_candidates=python_candidates(source),
        test_methods=tests,
        resource_sha256=files,
        acceptance_scripts=[
            path.relative_to(source).as_posix()
            for path in sorted((source / "scripts").glob("*acceptance*.py"))
        ],
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    content = json.dumps(inventory(args.source), indent=2, sort_keys=True) + "\n"
    if args.check:
        if args.output.read_text() != content:
            raise SystemExit("reference inventory drift")
    else:
        args.output.write_text(content)


if __name__ == "__main__":
    main()
