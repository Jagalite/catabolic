# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Explicit CLI for generated locations, immutable recipes and saved outputs."""

import json

from .artifacts import Artifacts
from .rendering import PRESETS, capabilities


def register(commands):
    commands = commands.add_parser(
        "artifact",
        help="generate and recover separate media files; originals stay read-only",
    ).add_subparsers(dest="operation", required=True)
    commands.add_parser("capabilities")
    bind = commands.add_parser("bind")
    bind.add_argument("location")
    bind.add_argument(
        "--root",
        required=True,
        help="existing empty directory dedicated to generated files",
    )
    recipe = commands.add_parser("recipe")
    recipe.add_argument("name")
    recipe.add_argument("--preset", choices=PRESETS, required=True)
    recipe.add_argument("--options", default="{}")
    recipe.add_argument(
        "--output-definition",
        help="immutable rendition definition ID; defaults to preset semantics",
    )
    enqueue = commands.add_parser("enqueue")
    enqueue.add_argument("--file-id", required=True)
    enqueue.add_argument("--item-id", required=True)
    enqueue.add_argument("--recipe", required=True, help="immutable recipe ID")
    enqueue.add_argument("--location", required=True)
    for operation in ("list", "recipes", "run", "recover"):
        command = commands.add_parser(operation)
        command.add_argument("--limit", type=int, default=100)
        if operation in ("list", "recipes"):
            command.add_argument("--after", default="")
        if operation == "list":
            command.add_argument("--state")
            command.add_argument("--file-id")
    commands.add_parser("show").add_argument("id")


def dispatch(app, args):
    artifacts = Artifacts(app)
    op = args.operation
    if op == "capabilities":
        return capabilities()
    if op == "bind":
        return artifacts.bind(args.location, args.root)
    if op == "recipe":
        return artifacts.recipe(
            args.name, args.preset, json.loads(args.options), args.output_definition
        )
    if op == "enqueue":
        return artifacts.enqueue(args.file_id, args.recipe, args.location, args.item_id)
    if op == "show":
        return artifacts.get(args.id)
    if op == "recipes":
        return artifacts.recipes(args.limit, args.after)
    if op == "list":
        return artifacts.list(
            limit=args.limit, after=args.after, state=args.state, file_id=args.file_id
        )
    return getattr(artifacts, op)(limit=args.limit)
