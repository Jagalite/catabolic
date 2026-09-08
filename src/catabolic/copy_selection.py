# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Explicit catalog-specific copy preferences, evaluated before layout naming."""

import json
from collections import defaultdict
from contextlib import nullcontext

from .curation import occurrence, payload_object
from .domain import CatabolicError
from .processing import current_fact
from .store import encode

FIELDS = {
    "size",
    "location",
    "path",
    "height",
    "width",
    "hdr",
    "duration",
    "language",
    "video_codec",
    "audio_codec",
}


class CopySelection:
    def __init__(self, app):
        self.app, self.store = app, app.store

    def put(self, catalog, definition, *, _db=None):
        self.app.require_recovered()
        payload_object(definition)
        if set(definition) - {"require", "prefer", "tie_break", "available_only"}:
            raise CatabolicError(
                "copy policy supports require, prefer, tie_break, available_only"
            )
        if not self.store.rows("SELECT id FROM catalogs WHERE id=?", (catalog,)):
            raise CatabolicError("unknown catalog")
        if definition.get("tie_break") not in (None, "file_id"):
            raise CatabolicError("tie_break must be null or file_id")
        if type(definition.get("available_only", False)) is not bool:
            raise CatabolicError("available_only must be boolean")
        requirements = definition.get("require", {})
        preferences = definition.get("prefer", [])
        if not isinstance(requirements, dict) or set(requirements) - FIELDS:
            raise CatabolicError("unsupported required field")
        if not isinstance(preferences, list) or len(preferences) > 20:
            raise CatabolicError("prefer requires at most 20 rules")
        for rule in preferences:
            if (
                not isinstance(rule, dict)
                or set(rule) - {"field", "order", "values"}
                or rule.get("field") not in FIELDS
            ):
                raise CatabolicError("preference supports field, order or values")
            if ("order" in rule) == ("values" in rule):
                raise CatabolicError("preference needs order or values")
            if "order" in rule and rule["order"] not in ("asc", "desc"):
                raise CatabolicError("order must be asc or desc")
            if "values" in rule and (
                not isinstance(rule["values"], list) or len(rule["values"]) > 100
            ):
                raise CatabolicError("values must be a bounded ordered list")
        with nullcontext(_db) if _db is not None else self.store.transaction() as db:
            db.execute(
                "INSERT INTO copy_policies VALUES (?,?) ON CONFLICT(catalog) DO UPDATE SET definition=excluded.definition",
                (catalog, encode(definition)),
            )
        return {"catalog": catalog, "definition": definition}

    def get(self, catalog):
        rows = self.store.rows(
            "SELECT definition FROM copy_policies WHERE catalog=?", (catalog,)
        )
        return json.loads(rows[0]["definition"]) if rows else None

    def filter(self, catalog, associations):
        definition = self.get(catalog)
        if definition is None:
            return associations, {"configured": False}
        groups = defaultdict(list)
        retained = []
        for row in associations:
            if row["role"] == "primary":
                groups[(row["item_id"], row.get("part"))].append(row)
            else:
                retained.append(row)
        decisions = []
        blocked = []
        for (item, part), rows in groups.items():
            eligible = []
            explanations = []
            for row in rows:
                observed = occurrence(self.store, self.app.profile, row["file_id"])
                fact = current_fact(self.store, self.app.profile, row["file_id"])
                summary = (
                    fact["data"].get("summary", {}) if fact and fact["current"] else {}
                )
                values = {k: observed.get(k) for k in ("size", "location", "path")}
                values.update(
                    {k: summary.get(k) for k in ("height", "width", "hdr", "duration")}
                )
                values.update(
                    language=summary.get("languages"),
                    video_codec=summary.get("video_codecs"),
                    audio_codec=summary.get("audio_codecs"),
                )
                failed = [
                    k
                    for k, v in definition.get("require", {}).items()
                    if values.get(k) is None
                    or (
                        v not in values[k]
                        if isinstance(values[k], list)
                        else values[k] != v
                    )
                ]
                if (
                    definition.get("available_only", False)
                    and observed["status"] != "present"
                ):
                    failed.append("availability")
                score = []
                for rule in definition.get("prefer", []):
                    value = values.get(rule["field"])
                    order = rule.get("order")
                    if "values" in rule:
                        candidates = value if isinstance(value, list) else [value]
                        score.append(
                            (
                                0,
                                min(
                                    (
                                        rule["values"].index(v)
                                        for v in candidates
                                        if v in rule["values"]
                                    ),
                                    default=len(rule["values"]),
                                ),
                            )
                        )
                    elif value is None:
                        score.append((1, 0))
                    else:
                        try:
                            number = float(value)
                        except (TypeError, ValueError):
                            raise CatabolicError(
                                "numeric ordering needs a numeric field; use values for text"
                            ) from None
                        score.append((0, -number if order == "desc" else number))
                explanations.append(
                    {
                        "file_id": row["file_id"],
                        "required_fields_failed": failed,
                        "unknown_fields": sorted(
                            k for k, v in values.items() if v is None
                        ),
                        "rank": score,
                        "observed_status": observed["status"],
                    }
                )
                if not failed:
                    eligible.append((score, row))
            if not eligible:
                blocked.append(
                    {
                        "item_id": item,
                        "part": part,
                        "reason": "no copy meets required criteria",
                    }
                )
                continue
            eligible.sort(key=lambda pair: (pair[0], pair[1]["file_id"]))
            tied = [row for score, row in eligible if score == eligible[0][0]]
            if len(tied) > 1 and definition.get("tie_break") != "file_id":
                blocked.append(
                    {
                        "item_id": item,
                        "part": part,
                        "reason": "copy preference tie; set an explicit tie_break or refine preferences",
                        "files": [r["file_id"] for r in tied],
                    }
                )
                continue
            winner = eligible[0][1]
            retained.append(winner)
            decisions.append(
                {
                    "item_id": item,
                    "part": part,
                    "selected": winner["file_id"],
                    "candidates": explanations,
                }
            )
        selected_files = {r["file_id"] for r in retained if r["role"] == "primary"}
        attached = []
        for row in retained:
            related = json.loads(row["metadata"]).get("primary_file_ids")
            if related is not None and (
                not isinstance(related, list)
                or any(not isinstance(value, str) for value in related)
            ):
                raise CatabolicError(
                    "sidecar primary_file_ids must be an array of file IDs"
                )
            if (
                row["role"] == "primary"
                or not related
                or selected_files.intersection(related)
            ):
                attached.append(row)
        retained = attached
        return retained, {
            "configured": True,
            "definition": definition,
            "decisions": decisions,
            "blockers": blocked,
            "safe": not blocked,
        }

    def plan(self, catalog, *, limit=10000):
        if type(limit) is not int or not 1 <= limit <= 100000:
            raise CatabolicError("limit must be 1..100000")
        rows = self.store.rows(
            "SELECT a.*,f.location,f.path FROM item_files a JOIN files f ON f.id=a.file_id WHERE a.active=1 ORDER BY a.id LIMIT ?",
            (limit + 1,),
        )
        if len(rows) > limit:
            raise CatabolicError(
                "copy selection exceeds requested limit; narrow the saved layout selection"
            )
        selected, report = self.filter(catalog, rows)
        return {
            "catalog": catalog,
            **report,
            "association_ids": [r["id"] for r in selected],
        }
