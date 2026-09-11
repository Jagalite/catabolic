# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Component assertions use the existing proposal snapshot and decision log."""

import json

from pydantic import Field, ValidationError

from . import components
from .curation import occurrence
from .domain import CatabolicError
from .fallback_models import Model
from .store import encode


class Attributes(Model):
    language: str | None = None
    title: str | None = None
    forced: bool | None = None
    default: bool | None = None
    commentary: bool | None = None
    hearing_impaired: bool | None = None
    visual_impaired: bool | None = None


class Compatibility(Model):
    edition_id: str
    video_file_id: str
    video_revision: str
    part: int | None
    timeline_id: str = Field(min_length=1, max_length=256)
    coverage: str = Field(pattern="^full$")
    offset_seconds: float = Field(allow_inf_nan=False)
    synchronization: str = Field(pattern="^(declared|verified)$")


class Dependency(Model):
    file_id: str
    revision: str
    purpose: str = Field(pattern="^(font|subtitle_index|subtitle_data|support)$")


class Claim(Model):
    occurrence_id: str
    component_id: str | None = None
    attributes: Attributes = Field(default_factory=Attributes)
    compatibility: Compatibility | None = None
    dependencies_complete: bool | None = None
    dependencies: list[Dependency] = Field(default_factory=list, max_length=32)
    supersedes: list[str] = Field(default_factory=list, max_length=32)


def validate(store, profile, file_id, item_id, raw):
    try:
        claim = Claim.model_validate(raw)
    except ValidationError as exc:
        raise CatabolicError("invalid component assertion: " + str(exc)) from exc
    row = components.get(store, profile, claim.occurrence_id)
    if row["file_id"] != file_id or row["item_id"] != item_id or not row["current"]:
        raise CatabolicError("component assertion target is stale or mismatched")
    logical = claim.component_id or row["component_id"]
    if not store.rows(
        "SELECT 1 FROM media_components WHERE id=? AND kind=?", (logical, row["kind"])
    ):
        raise CatabolicError(
            "component identity requires an existing component of the same kind"
        )
    for old in claim.supersedes:
        if not store.rows(
            "SELECT 1 FROM component_assertions WHERE id=? AND occurrence_id=?",
            (old, row["occurrence_id"]),
        ):
            raise CatabolicError("superseded assertion must belong to this occurrence")
    checks = []
    if claim.compatibility:
        c = claim.compatibility
        if c.part != row["part"]:
            raise CatabolicError("component compatibility edition or part mismatch")
        if not store.rows(
            "SELECT 1 FROM item_files WHERE item_id=? AND file_id=? AND role='primary' AND active=1 AND part IS ?",
            (item_id, c.video_file_id, c.part),
        ):
            raise CatabolicError(
                "compatibility requires a primary of this exact item and part"
            )
        primaries = store.rows(
            "SELECT metadata FROM item_files WHERE item_id=? AND file_id=? AND role='primary' AND active=1 AND part IS ?",
            (item_id, c.video_file_id, c.part),
        )
        editions = {
            json.loads(a["metadata"]).get("edition_id", item_id) for a in primaries
        }
        if editions != {c.edition_id}:
            raise CatabolicError("component compatibility edition mismatch")
        checks.append((c.video_file_id, c.video_revision))
    checks.extend((d.file_id, d.revision) for d in claim.dependencies)
    pinned = []
    for identifier, rev in checks:
        snap = occurrence(store, profile, identifier)
        if snap["status"] != "present" or components.revision(snap) != rev:
            raise CatabolicError("stale component compatibility or dependency revision")
        pinned.append(snap)
    return claim.model_dump(exclude_none=True), row, pinned


def accept(store, profile, proposal):
    raw = proposal["payload"]
    claim, row, _ = validate(
        store, profile, proposal["file_id"], raw["item_id"], raw["component"]
    )
    for identifier in claim["supersedes"]:
        store.db.execute(
            "UPDATE component_assertions SET active=0 WHERE id=?", (identifier,)
        )
    logical = claim.get("component_id") or row["component_id"]
    store.db.execute(
        "INSERT INTO component_assertions(id,occurrence_id,component_id,payload) VALUES (?,?,?,?)",
        (proposal["id"], row["occurrence_id"], logical, encode(claim)),
    )
    return {"component_id": logical, "occurrence_id": row["occurrence_id"]}
