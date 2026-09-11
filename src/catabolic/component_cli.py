# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Explicit component packaging admission; execution remains artifact run."""

from .app import Application
from .component_claims import Claim
from .component_packaging import enqueue, extraction
from .components import get
from .store import Store


def register(commands):
    sub = commands.add_parser(
        "component", help="inspect components or explicitly admit packaging"
    ).add_subparsers(dest="operation", required=True)
    sub.add_parser("schema")
    show = sub.add_parser("show")
    show.add_argument("id")
    extract = sub.add_parser("extract")
    extract.add_argument("id")
    extract.add_argument("--recipe", required=True)
    extract.add_argument("--location", required=True)
    extract.add_argument("--apply", action="store_true")
    extract.add_argument("--expected-plan")
    package = sub.add_parser(
        "package", help="admit the exact reviewed fallback package"
    )
    package.add_argument("item")
    package.add_argument("--policy", required=True)
    package.add_argument("--location", required=True)
    package.add_argument("--expected-plan", required=True)


def run(args):
    if args.operation == "schema":
        return Claim.model_json_schema()
    writable = args.operation == "package" or (
        args.operation == "extract" and args.apply
    )
    with Store(args.db, writable=writable) as store:
        app = Application(store, args.profile)
        if args.operation == "show":
            return get(store, args.profile, args.id)
        if args.operation == "extract":
            return extraction(
                app,
                args.id,
                args.recipe,
                args.location,
                apply=args.apply,
                expected_plan=args.expected_plan,
            )
        return enqueue(app, args.policy, args.item, args.location, args.expected_plan)
