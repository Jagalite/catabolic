# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Immutable policy revisions; binding and execution remain explicit operations."""

import hashlib
import json
from uuid import uuid4

from pydantic import ValidationError

from .copy_selection import FIELDS
from .domain import CatabolicError, name
from .fallback_models import Policy
from .saved_queries import Queries
from .store import encode


class Policies:
    def __init__(self, store, profile="default"):
        self.store, self.profile = store, profile

    def get(self, identifier):
        rows = self.store.rows(
            "SELECT * FROM fallback_policies WHERE profile=? AND id=?",
            (self.profile, identifier),
        )
        if not rows:
            raise CatabolicError("unknown fallback policy revision in this profile")
        return {**rows[0], "definition": json.loads(rows[0]["definition"])}

    def put(self, policy_name, definition):
        name(policy_name)
        try:
            value = Policy.model_validate(definition).model_dump(exclude_none=True)
        except ValidationError as exc:
            raise CatabolicError("invalid fallback policy: " + str(exc)) from exc
        if len({t["name"] for t in value["fallbacks"]}) != len(value["fallbacks"]):
            raise CatabolicError("fallback tier names must be unique")
        ranking = value["within_tier"]
        if set(ranking["require"]) - FIELDS:
            raise CatabolicError("unsupported required field")
        for rule in ranking["prefer"]:
            if rule["field"] not in FIELDS or ("order" in rule) == ("values" in rule):
                raise CatabolicError(
                    "preference requires a supported field and one of order or values"
                )
        queries = Queries(self.store, self.profile)
        component_tiers = [
            t
            for r in (value.get("package") or {}).get("requirements", [])
            for t in r["fallbacks"]
        ]
        if value.get("package"):
            if value["slot_roles"] != ["primary"]:
                raise CatabolicError(
                    "package policies use primary slots; components are selected by their requirement queries"
                )
            names = [r["name"] for r in value["package"]["requirements"]]
            if len(set(names)) != len(names) or "video" in names:
                raise CatabolicError(
                    "component requirement names must be unique and not video"
                )
            operation = value["package"].get("operation_id")
            if operation and not self.store.rows(
                "SELECT 1 FROM processing_recipes WHERE id=?", (operation,)
            ):
                raise CatabolicError("unknown approved packaging operation")
        for tier in value["fallbacks"] + component_tiers:
            if queries.get(tier["query_id"])["definition"]["mode"] != "selection":
                raise CatabolicError("fallback tiers require complete typed selections")
        serialized = encode(value)
        if len(serialized.encode()) > 65536:
            raise CatabolicError("fallback policy exceeds 64 KiB")
        digest = hashlib.sha256(serialized.encode()).hexdigest()
        with self.store.transaction() as db:
            old = db.execute(
                "SELECT id FROM fallback_policies WHERE profile=? AND name=? AND digest=?",
                (self.profile, policy_name, digest),
            ).fetchone()
            if old:
                return self.get(old[0])
            revision = db.execute(
                "SELECT coalesce(max(revision),0)+1 FROM fallback_policies WHERE profile=? AND name=?",
                (self.profile, policy_name),
            ).fetchone()[0]
            identifier = str(uuid4())
            db.execute(
                "INSERT INTO fallback_policies VALUES (?,?,?,?,?,?)",
                (identifier, self.profile, policy_name, revision, serialized, digest),
            )
            db.executemany(
                "INSERT INTO fallback_dependencies VALUES (?,?)",
                [
                    (identifier, q)
                    for q in sorted(
                        {t["query_id"] for t in value["fallbacks"] + component_tiers}
                    )
                ],
            )
        return self.get(identifier)

    def listing(self, limit=100, after=""):
        if not 1 <= limit <= 1000:
            raise CatabolicError("invalid policy page")
        rows = self.store.rows(
            "SELECT id FROM fallback_policies WHERE profile=? AND id>? ORDER BY id LIMIT ?",
            (self.profile, after, limit + 1),
        )
        return {
            "policies": [self.get(r["id"]) for r in rows[:limit]],
            "has_more": len(rows) > limit,
        }
