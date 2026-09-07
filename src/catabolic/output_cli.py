# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Reusable rendition definitions and externally produced file registration."""

import json

from .outputs import Outputs


def register(commands):
    commands = commands.add_parser(
        "rendition", help="catalog custom renditions and define recipe output semantics"
    ).add_subparsers(dest="operation", required=True)
    define = commands.add_parser(
        "define", help="create or reuse an immutable definition revision"
    )
    define.add_argument("name")
    payload = define.add_mutually_exclusive_group(required=True)
    payload.add_argument("--definition", help="JSON output definition")
    payload.add_argument("--file", help="JSON definition file, or - for stdin")
    register = commands.add_parser(
        "register",
        help="record an existing scanned rendition and user-declared provenance",
    )
    for flag in ("file-id", "source-file-id", "item-id", "output-definition"):
        register.add_argument("--" + flag, required=True)
    register.add_argument("--metadata", default="{}")
    for operation in ("list", "definitions"):
        command = commands.add_parser(operation)
        command.add_argument("--limit", type=int, default=100)
        command.add_argument("--after", default="")
        if operation == "list":
            command.add_argument("--source-file-id")
    for operation in ("show", "definition"):
        commands.add_parser(operation).add_argument("id")


def dispatch(app, args):
    from .cli import read_text

    outputs = Outputs(app)
    if args.operation == "define":
        return outputs.define(
            args.name,
            json.loads(read_text(args.file, 65536) if args.file else args.definition),
        )
    if args.operation == "register":
        return outputs.register(
            args.file_id,
            args.source_file_id,
            args.item_id,
            args.output_definition,
            json.loads(args.metadata),
        )
    if args.operation == "definition":
        return outputs.get_definition(args.id)
    if args.operation == "show":
        return outputs.get(args.id)
    return outputs.list(
        definitions=args.operation == "definitions",
        limit=args.limit,
        after=args.after,
        source_file_id=getattr(args, "source_file_id", None),
    )
