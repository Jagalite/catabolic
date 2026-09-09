# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Local owner policy; evidence and state correctness are independent."""

import json
import time

from .domain import CatabolicError
from .store import encode

CHECKS = ("uuid", "device", "inode")


def effective(app, location):
    return source_policy(app.store, app.profile, location)


def source_policy(store, profile, location):
    rows = (
        store.rows(
            "SELECT * FROM source_identity_policies WHERE profile=? AND location=?",
            (profile, location),
        )
        if store.schema_version >= 19
        else []
    )
    row = rows[0] if rows else {}
    preset = row.get("identity_policy", "strict")
    settings = dict.fromkeys(CHECKS, "skip" if preset == "path" else "check")
    if row.get("settings"):
        settings.update(json.loads(row["settings"]))
    return {"preset": preset, "settings": settings, "revision": row.get("revision", 0)}


def configure(app, location, policy=None, *, apply=False, overrides=None):
    if policy is not None and policy not in ("strict", "path"):
        raise CatabolicError("identity policy must be strict or path")
    overrides = {} if overrides is None else overrides
    if not isinstance(overrides, dict):
        raise CatabolicError("identity settings must be an object")
    if any(k not in CHECKS or v not in ("check", "skip") for k, v in overrides.items()):
        raise CatabolicError("unknown identity check or policy value")
    binding = app.binding("source", location)
    old = effective(app, location)
    preset = policy or old["preset"]
    settings = (
        dict.fromkeys(CHECKS, "skip" if preset == "path" else "check")
        if policy
        else dict(old["settings"])
    )
    settings.update(overrides)
    if (
        app.store.rows(
            "SELECT 1 FROM generated_locations WHERE profile=? AND location=?",
            (app.profile, location),
        )
        and "skip" in settings.values()
    ):
        raise CatabolicError("generated artifact locations require strict identity")
    changed = preset != old["preset"] or settings != old["settings"]
    revision = old["revision"]
    if apply and changed:
        app.require_recovered()
        if app.store.rows(
            "SELECT 1 FROM consumer_deliveries WHERE profile=? AND lease_until>?",
            (app.profile, time.time()),
        ):
            raise CatabolicError(
                "consumer delivery is in flight; retry after its lease ends"
            )
        revision += 1
        with app.store.transaction() as db:
            db.execute(
                "INSERT INTO source_identity_policies(profile,location,identity_policy,settings,revision) VALUES (?,?,?,?,?) ON CONFLICT(profile,location) DO UPDATE SET identity_policy=excluded.identity_policy,settings=excluded.settings,revision=excluded.revision",
                (app.profile, location, preset, encode(settings), revision),
            )
            db.execute(
                "INSERT INTO source_policy_history(profile,location,revision,settings) VALUES (?,?,?,?)",
                (
                    app.profile,
                    location,
                    revision,
                    encode({"preset": preset, "settings": settings}),
                ),
            )
    return {
        "source": location,
        "root": binding["root"],
        "previous_identity_policy": old["preset"],
        "identity_policy": preset,
        "settings": settings,
        "revision": revision,
        "applied": apply,
        "complete": True,
        "precedence": "strict default < stored source policy < explicit preset reset < explicit check settings",
        "scope": "source root identity only; file versions, availability, output ownership and state correctness remain required",
        "history": app.store.rows(
            "SELECT revision,settings,created_at FROM source_policy_history WHERE profile=? AND location=? ORDER BY revision",
            (app.profile, location),
        )
        if app.store.schema_version >= 20
        else [],
    }
