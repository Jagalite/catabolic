# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Entry status, completion requirements, and append-only worklogs."""

from .domain import CatabolicError
from .item_workflow import KINDS, MAX_NOTE_BYTES, STATUSES, ItemWorkflow

COMMANDS = ("status", "note", "worklog", "require", "requirements", "resolve")


def register(items):
    for operation in COMMANDS:
        command = items.add_parser(
            operation,
            help={
                "status": "read or change entry status; complete requires passing checks",
                "note": "append an attributed note, decision, or progress update",
                "worklog": "read the entry's append-only worklog",
                "require": "add a required completion check",
                "requirements": "show required checks and automatic blockers",
                "resolve": "resolve a review task or waive a requirement with a reason",
            }[operation],
        )
        command.add_argument(
            "id", help="requirement ID" if operation == "resolve" else "item ID"
        )
        if operation in ("status", "note", "require", "resolve"):
            command.add_argument("--actor", default="user")
            command.add_argument(
                "--expected-revision",
                type=int,
                help="reject if another writer changed this entry",
            )
        if operation in ("status", "require", "resolve"):
            command.add_argument(
                "--note",
                required=operation == "resolve",
                help="reason recorded in the worklog",
            )
        if operation == "status":
            command.add_argument("--set", dest="set_status", choices=STATUSES)
        elif operation == "note":
            source = command.add_mutually_exclusive_group(required=True)
            source.add_argument("--text")
            source.add_argument(
                "--file", help="UTF-8 worklog text file, or - for stdin"
            )
            command.add_argument(
                "--kind", choices=("note", "decision", "progress"), default="note"
            )
        elif operation == "require":
            command.add_argument("--kind", choices=KINDS, required=True)
            command.add_argument("--label", required=True)
            command.add_argument(
                "--target", help="file, job, artifact, or proposal ID; omit for review"
            )
        elif operation == "resolve":
            command.add_argument(
                "--state", choices=("pending", "complete", "waived"), required=True
            )
        else:
            command.add_argument("--limit", type=int, default=100)
            command.add_argument(
                "--after",
                type=int if operation == "worklog" else str,
                default=0 if operation == "worklog" else "",
            )


def writable(args):
    return args.command == "item" and (
        args.operation in ("note", "require", "resolve")
        or args.operation == "status"
        and args.set_status is not None
    )


def dispatch(app, args):
    from .cli import read_text

    workflow = ItemWorkflow(app)
    if args.operation == "worklog":
        return workflow.log(args.id, limit=args.limit, after=args.after)
    if args.operation == "requirements":
        return workflow.checks(args.id, limit=args.limit, after=args.after)
    options = {"actor": args.actor, "expected_revision": args.expected_revision}
    if args.operation == "note":
        return workflow.note(
            args.id,
            read_text(args.file, MAX_NOTE_BYTES) if args.file else args.text,
            kind=args.kind,
            **options,
        )
    if args.operation == "require":
        return workflow.require(
            args.id,
            args.kind,
            args.label,
            target=args.target,
            note=args.note or "",
            **options,
        )
    if args.operation == "resolve":
        return workflow.resolve(args.id, args.state, note=args.note, **options)
    if args.set_status is None:
        if (
            args.note is not None
            or args.expected_revision is not None
            or args.actor != "user"
        ):
            raise CatabolicError("status mutation options require --set")
        return workflow.get(args.id)
    return workflow.set_status(
        args.id, args.set_status, note=args.note or "", **options
    )
