# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Manifest v2 adds explicit tag vocabulary and attributed item/file assertions."""

from typing import Annotated, Literal

from pydantic import Field

from . import v1
from .v1 import FORMAT as FORMAT
from .v1 import Id, Nonnegative, Record

VERSION = 2
SCHEMA_ID = "urn:catabolic:open-catalog:manifest:2"


def label(description, **kwargs):
    return Field(description=description, json_schema_extra={"x-since": 2}, **kwargs)


class Tag(Record):
    """Stable tag identity; names are NFC casefolded text, optionally namespace:value."""

    id: Id = label("Opaque tag ID scoped to database_id; retained on rename.")
    name: Id = label("Canonical normalized tag name.")
    description: str = label("Human-readable vocabulary definition.")


class TagName(Record):
    """Canonical and alias names share one unique namespace."""

    name: Id = label("Normalized canonical name or alias.")
    tag_id: Id = label("Included tag to which this name resolves.")


class TagParent(Record):
    """Direct edge in an acyclic hierarchy with optional multiple parents."""

    child_id: Id = label("Included child tag.")
    parent_id: Id = label(
        "Included broader tag; inference is opt-in, never a stored assertion."
    )


class Tagging(Record):
    """Explicit assertion; each source may independently assert or withdraw a tag."""

    id: Id = label("Opaque assignment ID scoped to database_id.")
    subject_type: Literal["item", "file"] = label("Kind of included entity tagged.")
    subject_id: Id = label(
        "Included item or file ID; no automatic inheritance between them."
    )
    tag_id: Id = label("Included canonical tag ID.")
    source: Id = label(
        "Provenance label, e.g. manual or agent:curator; not authenticated identity."
    )
    confidence: Annotated[float | int, Field(ge=0, le=1)] | None = label(
        "Optional reported confidence; not a calibrated probability."
    )
    note: str = label("Evidence or explanation.")
    active: bool = label(
        "False preserves a withdrawn assertion; other sources remain independent."
    )
    created_at: str = label("Original assertion timestamp.")
    updated_at: str = label(
        "Latest assertion update or withdrawal timestamp; not a full edit history."
    )


class Counts(v1.Counts):
    """Exact lengths of the original six and four tagging collections."""

    tags: Nonnegative = label("Number of included vocabulary tags.")
    tag_names: Nonnegative = label("Number of canonical names and aliases.")
    tag_parents: Nonnegative = label("Number of direct hierarchy edges.")
    taggings: Nonnegative = label("Number of active and withdrawn explicit assertions.")


class Content(v1.Content):
    """Catalog snapshot plus tag assertions and their ancestor vocabulary closure."""

    tags: list[Tag] = label(
        "Tags referenced by included assertions, plus their ancestors."
    )
    tag_names: list[TagName] = label("All canonical and alias names for included tags.")
    tag_parents: list[TagParent] = label(
        "Complete outgoing parent closure for included tags."
    )
    taggings: list[Tagging] = label(
        "Active and withdrawn assertions for included items and files."
    )
    counts: Counts = label("Exact array lengths for all ten record collections.")


class Document(v1.Document):
    """Catabolic manifest v2. Version 1 remains separately supported and frozen."""

    format_version: Literal[2] = label(
        "Wire-format version; no implicit down-conversion."
    )
    content: Content = label(
        "Complete checksummed projection content, including tagging."
    )
