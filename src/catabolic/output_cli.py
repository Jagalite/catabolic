# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Reusable rendition definitions and externally produced file registration."""

import json

from .outputs import Outputs


def register(commands):
    commands = commands.add_parser(
        "rendition", help="catalog custom renditions and define recipe output semantics"
    ).add_subparsers(dest="operation", required=True)
    policy = commands.add_parser(
        "policy", help="read or save catalog-specific rendition publication"
    )
    policy.add_argument("--catalog", required=True)
    policy.add_argument("--definition", help="JSON publication policy; omit to read")
    for action in ("exclude", "allow"):
        choice = commands.add_parser(
            action, help="record a rendition decision for one catalog"
        )
        choice.add_argument("id")
        choice.add_argument("--catalog", required=True)
        choice.add_argument("--reason", required=True)
    receipt = commands.add_parser(
        "import-receipt",
        help="validate and atomically register delivered external outputs",
    )
    receipt.add_argument("--file", required=True)
    source = commands.add_parser(
        "receipt-source",
        help="capture source identity and revision before external processing",
    )
    source.add_argument("--file-id", required=True)
    source.add_argument("--item-id", required=True)
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
    if args.operation == "receipt-source":
        from .receipts import source_receipt

        return source_receipt(app, args.file_id, args.item_id)
    if args.operation in ("policy", "exclude", "allow"):
        from .rendition_publication import Publication

        publication = Publication(app)
        if args.operation == "policy":
            return (
                publication.put(args.catalog, json.loads(args.definition))
                if args.definition is not None
                else publication.get(args.catalog)
            )
        return publication.decide(
            args.catalog, args.id, args.operation == "exclude", args.reason
        )
    if args.operation == "import-receipt":
        from .receipts import import_receipt

        return import_receipt(app, json.loads(read_text(args.file, 1048576)))
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
