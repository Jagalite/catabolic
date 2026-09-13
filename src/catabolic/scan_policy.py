# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Owner observation policy. Coverage is independent of execution allowances."""

import json

from .domain import CatabolicError, relative_path
from .store import encode

DEFAULTS = dict(
    directory_entries=50000,
    depth=64,
    directory_seconds=15,
    metadata_seconds=2,
    errors=16,
    total_seconds=120,
    total_entries=250000,
    pending_directories=100000,
    batch_records=500,
    batch_bytes=1048576,
    batch_seconds=1,
    temporary_bytes=67108864,
    minimum_free_bytes=67108864,
)
EXTENDED = dict(
    directory_entries=1000000,
    depth=4096,
    directory_seconds=300,
    metadata_seconds=10,
    errors=64,
    total_seconds=1800,
    total_entries=5000000,
)


def limits(values=None):
    values = {} if values is None else values
    if not isinstance(values, dict) or set(values) - set(DEFAULTS):
        raise CatabolicError("unknown scan budget")
    if any(type(v) not in (int, float) or not 0 < v < 10**12 for v in values.values()):
        raise CatabolicError("scan budgets must be finite positive numbers")
    integer_keys = set(DEFAULTS) - {
        "directory_seconds",
        "metadata_seconds",
        "total_seconds",
        "batch_seconds",
    }
    if any(k in integer_keys and type(v) is not int for k, v in values.items()):
        raise CatabolicError("scan count and byte budgets must be integers")
    result = {**DEFAULTS, **values}
    # Even extended requests keep hard memory bounds.
    if result["batch_records"] > 10000 or result["batch_bytes"] > 16777216:
        raise CatabolicError("scan batch exceeds hard memory allowance")
    return result


def effective(app, source):
    rows = app.store.rows(
        "SELECT * FROM source_observation_policies WHERE profile=? AND source=?",
        (app.profile, source),
    )
    row = rows[0] if rows else {}
    policy = json.loads(row.get("policy", "{}"))
    return dict(
        revision=row.get("revision", 0),
        exclusions=policy.get("exclusions", []),
        budgets=limits(policy.get("budgets")),
    )


def configure(app, source, policy=None, *, apply=False):
    app.binding("source", source)
    old = effective(app, source)
    if policy is None:
        return old
    if not isinstance(policy, dict) or set(policy) - {"exclusions", "budgets"}:
        raise CatabolicError("invalid observation policy")
    exclusions = policy.get("exclusions", old["exclusions"])
    if not isinstance(exclusions, list):
        raise CatabolicError("exclusions must be a list")
    new = dict(
        exclusions=sorted({relative_path(p) for p in exclusions}),
        budgets=limits(policy.get("budgets", old["budgets"])),
    )
    changed = any(old[k] != new[k] for k in new)
    revision = old["revision"] + int(changed)
    if apply and changed:
        with app.store.transaction() as db:
            db.execute(
                "INSERT INTO source_observation_policies VALUES (?,?,?,?) ON CONFLICT(profile,source) DO UPDATE SET revision=excluded.revision,policy=excluded.policy",
                (app.profile, source, revision, encode(new)),
            )
            db.execute(
                "INSERT INTO source_observation_policy_history(profile,source,revision,policy) VALUES (?,?,?,?)",
                (app.profile, source, revision, encode(new)),
            )
            from .source_events import invalidate

            invalidate(app, source, reason="observation_policy_changed")
    return dict(previous=old, policy=new, revision=revision, applied=apply)


def execution(app, source, *, extended=False, budgets=None, scopes=None):
    policy = effective(app, source)
    merged = {**policy["budgets"], **(EXTENDED if extended else {}), **(budgets or {})}
    return dict(
        budgets=limits(merged),
        scopes=sorted({relative_path(p) if p else "" for p in (scopes or [""])}),
        policy_revision=policy["revision"],
    )
