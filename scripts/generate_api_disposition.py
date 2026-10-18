# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Generate the Catabolic 1.0 Python API disposition from the frozen inventory.

The user's October 16 decision: the 1.0 `catabolic` distribution supports the
CLI and the 159 preserved names, and deprecates every other public name of the
frozen 0.2 package with a documented list. This reads
`tests/parity/python-api.json` (the 1,027-row frozen inventory) and writes:

- `crates/catabolic-python/python/catabolic/_api_disposition.json`, read by the
  compatibility package to explain removed names and modules, and
- `docs/PYTHON_API_1_0.md`, the documented list.

Run with ``--check`` to refuse stale outputs.
"""

import argparse
import ast
import json
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
INVENTORY = ROOT / "tests/parity/python-api.json"
PACKAGE = ROOT / "crates/catabolic-python/python/catabolic"
DISPOSITION = PACKAGE / "_api_disposition.json"
DOCUMENT = ROOT / "docs/PYTHON_API_1_0.md"
VERSION = "1.0.0"


def module_of(row):
    source = row["source"]
    if not source.startswith("src/") or not source.endswith(".py"):
        raise ValueError(f"unexpected inventory source: {source}")
    parts = source[len("src/") : -len(".py")].split("/")
    if parts[-1] == "__init__":
        parts.pop()
    return ".".join(parts)


def package_modules():
    """Modules shipped by the compatibility package and the names each binds."""
    modules = {}
    for path in sorted(PACKAGE.rglob("*.py")):
        parts = list(path.relative_to(PACKAGE).with_suffix("").parts)
        if parts[-1] == "__init__":
            parts.pop()
        names = set()
        for node in ast.parse(path.read_text(encoding="utf-8")).body:
            if (
                isinstance(node, ast.ImportFrom)
                and node.module
                and (
                    node.module == "catabolic_native"
                    or node.module.startswith("catabolic_native.")
                )
            ):
                names.update(alias.asname or alias.name for alias in node.names)
        modules[".".join(["catabolic", *parts])] = names
    return modules


def build():
    inventory = json.loads(INVENTORY.read_text(encoding="utf-8"))
    supported = []
    deprecated = defaultdict(list)
    for row in inventory["rows"]:
        identifier = row["id"]
        if not identifier.startswith("python:"):
            raise ValueError(f"unexpected inventory identifier: {identifier}")
        name = identifier[len("python:") :]
        module = module_of(row)
        if name == module:
            # The module binding itself; it is never a deprecated name.
            if not row["classification"].startswith("preserve"):
                raise ValueError(f"{identifier} is a module but not preserved")
            qualified = ""
        elif not name.startswith(module + "."):
            raise ValueError(f"{identifier} is not inside {module}")
        else:
            qualified = name[len(module) + 1 :]
        if row["classification"].startswith("preserve"):
            supported.append({"module": module, "name": qualified})
        else:
            deprecated[module].append(qualified)
    supported.sort(key=lambda entry: (entry["module"], entry["name"]))
    supported_modules = sorted({entry["module"] for entry in supported})
    present = package_modules()
    deprecated = {module: sorted(names) for module, names in sorted(deprecated.items())}
    # Deprecated names the thin layer still binds so that native result types
    # stay nameable. They are deprecated, not supported, and may go later.
    retained = {
        module: sorted(
            name
            for name in names
            if name in {entry.split(".")[0] for entry in deprecated.get(module, [])}
        )
        for module, names in sorted(present.items())
    }
    removed = set()
    for module in deprecated:
        if module in supported_modules or module in present:
            continue
        parts = module.split(".")
        # Parent packages that the thin layer does not ship are removed too.
        for end in range(2, len(parts) + 1):
            parent = ".".join(parts[:end])
            if parent not in supported_modules and parent not in present:
                removed.add(parent)
    return {
        "format": 1,
        "version": VERSION,
        "inventory_sha256": inventory["inventory_sha256"],
        "reference_commit": inventory["reference_commit"],
        "supported": supported,
        "supported_modules": supported_modules,
        "retained": {module: names for module, names in retained.items() if names},
        "removed_modules": sorted(removed),
        "deprecated": deprecated,
    }


def document(disposition):
    supported = disposition["supported"]
    deprecated = disposition["deprecated"]
    count = sum(len(names) for names in deprecated.values())
    lines = [
        "# Catabolic 1.0 Python API",
        "",
        "<!--",
        "SPDX-FileCopyrightText: 2026 The Catabolic Contributors",
        "SPDX-License-Identifier: MIT",
        "-->",
        "",
        "Catabolic 1.0 is the native engine, distributed as `catabolic`. Its",
        "supported interfaces are the `catabolic` command line and the "
        f"{len(supported)} Python",
        "names below, which forward to the native implementation. Every other",
        f"public name of the 0.2 Python package ({count} names) is deprecated",
        "and is not supported by 1.0. Importing a removed module raises",
        "`ModuleNotFoundError`, and reading a removed name from a module that",
        "remains raises `AttributeError`; both messages point to this page.",
        "(`from module import name` reports Python's standard `ImportError`.)",
        "",
        "This list is generated by `scripts/generate_api_disposition.py` from the",
        "frozen 0.2 inventory (`tests/parity/python-api.json`, reference",
        f"`{disposition['reference_commit'][:12]}`). Do not edit it by hand.",
        "",
        f"## Supported ({len(supported)})",
        "",
    ]
    by_module = defaultdict(list)
    for entry in supported:
        by_module[entry["module"]].append(entry["name"])
    for module in sorted(by_module):
        names = ", ".join(
            f"`{name}`" if name else "(the module itself)"
            for name in sorted(by_module[module])
        )
        lines.append(f"- `{module}`: {names}")
    retained = disposition["retained"]
    if retained:
        lines += [
            "",
            "## Deprecated but still importable",
            "",
            "These deprecated names are still bound by the compatibility layer",
            "so that the types returned by supported functions stay nameable.",
            "They are not supported and may be removed in a later release.",
            "",
        ]
        for module, names in retained.items():
            lines.append(f"- `{module}`: " + ", ".join(f"`{name}`" for name in names))
    lines += ["", f"## Deprecated in 1.0 ({count})", ""]
    supported_modules = set(disposition["supported_modules"])
    removed_modules = set(disposition["removed_modules"])
    for module, names in deprecated.items():
        if module in removed_modules:
            state = "module removed"
        elif module in supported_modules:
            state = "module kept; these names removed"
        else:
            state = "module kept for result types; other names removed"
        lines.append(f"### `{module}` ({len(names)}, {state})")
        lines.append("")
        lines.append(", ".join(f"`{name}`" for name in names))
        lines.append("")
    return "\n".join(lines).rstrip("\n") + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args(argv)
    disposition = build()
    outputs = {
        DISPOSITION: json.dumps(disposition, indent=1, sort_keys=True) + "\n",
        DOCUMENT: document(disposition),
    }
    stale = [
        path
        for path, text in outputs.items()
        if not path.is_file() or path.read_text(encoding="utf-8") != text
    ]
    if args.check:
        if stale:
            print(
                "stale API disposition: "
                + ", ".join(
                    str(p.relative_to(ROOT) if p.is_relative_to(ROOT) else p)
                    for p in stale
                )
            )
            return 1
        return 0
    for path, text in outputs.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    supported = len(disposition["supported"])
    removed = sum(len(v) for v in disposition["deprecated"].values())
    print(f"supported {supported}, deprecated {removed}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
