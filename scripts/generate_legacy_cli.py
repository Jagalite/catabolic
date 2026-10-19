#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Development-only frozen CLI data extraction; never used by native runtime.

Run with the pinned reference interpreter and an exported baseline checkout.
No catalog or network operations are executed.
"""

import argparse
import ast
import hashlib
import json
import os
import sys
import textwrap
import unicodedata
from collections import Counter, defaultdict
from pathlib import Path


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--source", type=Path, required=True)
    p.add_argument("--output", type=Path, required=True)
    args = p.parse_args()
    source = args.source.resolve()
    if sys.version_info[:3] != (3, 14, 6):
        raise RuntimeError(
            "frozen CLI generation requires the pinned Python 3.14.6 interpreter"
        )
    if os.environ.get("PYTHONHASHSEED") != "0":
        raise RuntimeError(
            "generation requires PYTHONHASHSEED=0 for frozen set-derived capabilities"
        )
    inventory_path = Path(__file__).resolve().parents[1] / "tests/parity/inventory.json"
    manifest = json.loads(inventory_path.read_text())["resource_sha256"]
    for relative, expected in manifest.items():
        if relative.startswith("src/catabolic/"):
            actual = hashlib.sha256((source / relative).read_bytes()).hexdigest()
            if actual != expected:
                raise RuntimeError(f"frozen source differs from inventory: {relative}")

    for key in list(os.environ):
        if key.startswith("CATABOLIC_"):
            del os.environ[key]
    os.environ["COLUMNS"] = "80"
    os.environ["NO_COLOR"] = "1"
    sys.path.insert(0, str(source / "src"))
    from migration_inventory import cli_inventory

    from catabolic import cli, documentation, layouts, media
    from catabolic.component_claims import Claim
    from catabolic.fallback_models import Policy
    from catabolic.interchange import specification
    from catabolic.mapping_adapters import capabilities as mapping_capabilities
    from catabolic.operations import capabilities
    from catabolic.program_contracts import definition_schema
    from catabolic.watcher_models import WatcherDefinition

    root = cli.parser()
    nodes = cli_inventory(root, user_home=str(Path.home()))

    def annotate(parser, path=()):
        node = next(
            n for n in nodes if n["id"] == "cli:" + (" ".join(path) or "<root>")
        )
        node.update(
            help_text=parser.format_help(),
            usage=parser.format_usage(),
            prog=parser.prog,
        )
        formatter = parser._get_formatter()
        parts, pos_start = formatter._get_actions_usage_parts(
            parser._actions, parser._mutually_exclusive_groups
        )
        node["usage_parts"] = parts
        node["usage_positionals_start"] = pos_start

        def chunks(value):
            return (
                textwrap.TextWrapper()._split(" ".join(value.split())) if value else []
            )

        node["description_chunks"] = chunks(parser.description)
        sections = []
        maximum = 0

        def entry(action, indent):
            nonlocal maximum
            invocation = formatter._format_action_invocation(action)
            maximum = max(maximum, len(invocation) + indent)
            return {
                "invocation": invocation,
                "chunks": chunks(formatter._expand_help(action)) if action.help else [],
                "indent": indent,
            }

        for group in parser._action_groups:
            entries = []
            for action in group._group_actions:
                if action.help is argparse.SUPPRESS:
                    continue
                entries.append(entry(action, 2))
                for child in getattr(action, "_choices_actions", []):
                    entries.append(entry(child, 4))
            if entries:
                sections.append(
                    {
                        "title": group.title,
                        "description_chunks": chunks(group.description),
                        "entries": entries,
                    }
                )
        node["help_sections"] = sections
        node["action_max_length"] = maximum
        for action in node["arguments"]:
            if isinstance(action.get("choices"), dict):
                action["choices"] = list(action["choices"])
        for action in parser._actions:
            if isinstance(action, argparse._SubParsersAction):
                for key, child in action.choices.items():
                    annotate(child, (*path, key))

    annotate(root)
    docs = {
        topic[0]: documentation.documentation(topic[0], None)
        for topic in documentation.TOPICS
    }
    docs[""] = documentation.documentation(None, None)
    orders = defaultdict(Counter)
    for python_file in sorted((source / "src/catabolic").rglob("*.py")):
        for node in ast.walk(ast.parse(python_file.read_text())):
            if (
                isinstance(node, ast.Dict)
                and node.keys
                and all(
                    isinstance(k, ast.Constant) and isinstance(k.value, str)
                    for k in node.keys
                )
            ):
                keys = tuple(k.value for k in node.keys)
                if len(set(keys)) == len(keys):
                    orders["\x1f".join(sorted(keys))][keys] += 1
    # Python repr escapes every non-printable character, including format,
    # separator, private-use and unassigned code points. Rust is_control alone
    # covers only a subset. Keep the pinned interpreter's Unicode classification
    # as compact ranges so runtime diagnostics need no Python or version guess.
    nonprintable_ranges = []
    for codepoint in range(sys.maxunicode + 1):
        if chr(codepoint).isprintable():
            continue
        if nonprintable_ranges and nonprintable_ranges[-1][1] + 1 == codepoint:
            nonprintable_ranges[-1][1] = codepoint
        else:
            nonprintable_ranges.append([codepoint, codepoint])
    resource = dict(
        object_orders={
            key: list(count.most_common(1)[0][0]) for key, count in orders.items()
        },
        decimal_digits={
            chr(i): str(unicodedata.decimal(chr(i)))
            for i in range(sys.maxunicode + 1)
            if chr(i).isdecimal()
        },
        nonprintable_ranges=nonprintable_ranges,
        unicode_version=unicodedata.unidata_version,
        reference="453fca983222c6665a48775eef67c489c96b527e",
        interpreter=sys.version,
        pythonhashseed=0,
        source_sha256={
            str(p.relative_to(source)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in sorted((source / "src/catabolic").rglob("*.py"))
            if p.name.endswith("_cli.py") or p.name in ("cli.py", "documentation.py")
        },
        nodes=nodes,
        docs=docs,
        types=media.describe_types(),
        presets=layouts.PRESETS,
        definition_schemas={
            key: definition_schema(key) for key in ("query", "projection", "operation")
        },
        extra_schemas={
            "component": Claim.model_json_schema(),
            "fallback": Policy.model_json_schema(),
            "watcher": WatcherDefinition.model_json_schema(),
        },
        operation_types=capabilities(),
        mapping_capabilities=mapping_capabilities(),
        spec_index=json.loads(
            (source / "src/catabolic/interchange/releases/index.json").read_text()
        ),
        spec={
            str(v): {
                "schema": specification.schema(v),
                "schema_text": specification.schema_text(v),
                "markdown": specification.reference_text(v),
            }
            for v in (1, 2, 3)
        },
    )
    static = {
        "item types": resource["types"],
        "layout presets": {"presets": resource["presets"]},
        "operation types": resource["operation_types"],
    }
    static["projection mapping-capabilities"] = resource["mapping_capabilities"]
    static.update({k + " schema": v for k, v in resource["definition_schemas"].items()})
    static["query contract"] = static.pop("query schema")
    static.update({k + " schema": v for k, v in resource["extra_schemas"].items()})
    resource["static_output"] = {
        key: {
            "compact": json.dumps(value, ensure_ascii=False, allow_nan=False),
            "pretty": json.dumps(value, indent=2, ensure_ascii=False, allow_nan=False),
        }
        for key, value in static.items()
    }
    args.output.write_text(json.dumps(resource, ensure_ascii=False, indent=2) + "\n")
    print(
        f"{len(nodes)} parser nodes; {len(docs) - 1} bundled guides; {args.output.stat().st_size} bytes"
    )


if __name__ == "__main__":
    main()
