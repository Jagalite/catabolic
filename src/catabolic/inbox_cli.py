# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Small read-only inbox surface on the existing bounded query transports."""

from .domain import CatabolicError
from .graphql_query import execute_graphql
from .sql_query import execute_sql
from .store import encode
from .work_inbox import decode, evidence_token, validate_evidence


def register(commands):
    sub = commands.add_parser(
        "inbox", help="discover recorded catalog work"
    ).add_subparsers(dest="operation", required=True)
    listing = sub.add_parser("list")
    listing.add_argument("--limit", type=int, default=100)
    listing.add_argument("--after")
    listing.add_argument(
        "--include-inactive",
        action="store_true",
        help="include waiting, deferred and historical records",
    )
    show = sub.add_parser("show")
    show.add_argument("work_key")
    show.add_argument(
        "--expected-evidence",
        help="reject changed planning evidence; does not authorize or fence a later action",
    )
    sub.add_parser("summary")
    for command in (listing, show, sub.choices["summary"]):
        command.add_argument("--timeout-ms", type=int, default=5000)


def run(args):
    if args.operation == "list":
        result = execute_graphql(
            args.db,
            "query($first:Int!,$after:String,$inactive:Boolean!){workInbox(first:$first,after:$after,includeInactive:$inactive){nodes pageInfo{endCursor hasNextPage}}}",
            profile=args.profile,
            variables={
                "first": args.limit,
                "after": args.after,
                "inactive": args.include_inactive,
            },
            timeout_ms=args.timeout_ms,
        )
        if result.get("errors"):
            raise CatabolicError(
                "inbox evaluation incomplete: " + str(result["errors"])
            )
        page = result["data"]["workInbox"]
        return {
            "profile": args.profile,
            "entries": page["nodes"],
            "next_cursor": page["pageInfo"]["endCursor"],
            "complete": True,
            "result_complete": not page["pageInfo"]["hasNextPage"],
            "evaluation_complete": True,
            "evidence": "recorded only; inventory coverage and physical health may be unknown",
        }
    if args.operation == "show":
        result = execute_sql(
            args.db,
            "SELECT * FROM catalog_work_inbox WHERE profile=:profile AND work_key=:key",
            profile=args.profile,
            params=encode({"key": args.work_key}),
            max_rows=1,
            timeout_ms=args.timeout_ms,
        )
        if not result["rows"]:
            raise CatabolicError(
                "unknown work key in this profile; resolved derived work may only remain in its owner's history"
            )
        row = decode(dict(zip(result["columns"], result["rows"][0], strict=True)))
        if args.expected_evidence:
            validate_evidence(row, args.expected_evidence)
        return {
            "entry": row,
            "evidence_token": evidence_token(row),
            "evaluation_complete": True,
            "action_contract": "Descriptors only. Reload the owner and use its authorization, revision checks and transaction. This token is not an atomic claim.",
        }
    result = execute_sql(
        args.db,
        "SELECT category,actionability,count(*) AS entries,sum(countable) AS work_count FROM catalog_work_inbox WHERE profile=:profile GROUP BY category,actionability ORDER BY category,actionability",
        profile=args.profile,
        max_rows=1000,
        timeout_ms=args.timeout_ms,
    )
    if result.get("truncated"):
        raise CatabolicError("inbox summary evaluation incomplete")
    groups = [dict(zip(result["columns"], row, strict=True)) for row in result["rows"]]
    return {
        "profile": args.profile,
        "groups": groups,
        "actionable_count": sum(
            row["work_count"]
            for row in groups
            if row["actionability"] in ("ready", "blocked", "needs_decision")
        ),
        "evaluation_complete": True,
        "curation_finished": None,
        "evidence": "Counts describe recorded work, not complete inventory or verified projection health.",
    }
