# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Open Catalog's Catabolic manifest v1 profile.

Field types and annotations generate the normative structural schema and field
reference. Cross-record semantics live in validation.py and the protocol guide.
"""

from typing import Annotated, Literal

from pydantic import BaseModel, ConfigDict, Field, JsonValue

FORMAT = "catabolic.catalog-manifest"
VERSION = 1
SCHEMA_ID = "urn:catabolic:open-catalog:manifest:1"
Id = Annotated[str, Field(min_length=1)]
Sha256 = Annotated[str, Field(pattern=r"^[0-9a-f]{64}$")]
Positive = Annotated[int, Field(gt=0)]
Nonnegative = Annotated[int, Field(ge=0)]
IntegerText = Annotated[str, Field(pattern=r"^-?(0|[1-9][0-9]*)$")]
SizeText = Annotated[str, Field(pattern=r"^(0|[1-9][0-9]*)$")]


def label(description, **kwargs):
    """Annotate a contract field once for schema and reference generation."""
    return Field(description=description, json_schema_extra={"x-since": 1}, **kwargs)


class Record(BaseModel):
    """Unknown JSON fields must survive reading and re-exporting."""

    model_config = ConfigDict(extra="allow", strict=True, allow_inf_nan=False)
    __pydantic_extra__: dict[str, JsonValue] = Field(init=False)


class Identity(Record):
    """Provider identity; the namespace/value pair identifies one item."""

    namespace: Id = label(
        "Provider namespace, including custom namespaces.", examples=["tmdb.movie"]
    )
    value: Id = label("Opaque provider-specific identifier.", examples=["329865"])


class Item(Record):
    """Media item independent of its files and generated destinations."""

    id: Id = label(
        "Opaque item ID scoped to database_id; never infer identity from a title."
    )
    kind: Id = label(
        "Media vocabulary name; custom:name and future names remain readable.",
        examples=["movie", "custom:research_notes"],
    )
    metadata: dict[str, JsonValue] = label(
        "Unmodified item metadata; unknown keys and nested JSON are retained."
    )
    identities: list[Identity] = label("External identities assigned to this item.")


class File(Record):
    """Inventoried file with recorded availability, not a live filesystem check."""

    id: Id = label("Opaque file ID scoped to database_id.")
    location: Id = label("Logical source location; not an absolute machine path.")
    path: Id = label("Relative POSIX path within the logical source location.")
    size: SizeText | None = label(
        "Recorded byte size as decimal text, preserving integers beyond JavaScript precision."
    )
    mtime_ns: IntegerText | None = label(
        "Recorded modification time in nanoseconds as signed decimal text."
    )
    status: Literal["present", "missing", "unknown"] = label(
        "Availability from the selected profile's last observation."
    )
    scan_id: Id | None = label(
        "Observation scan identifier; scans themselves are outside this projection."
    )
    observed_at: str | None = label(
        "Producer's recorded scan timestamp, or null when unobserved."
    )
    source_root: str | None = label(
        "Machine-specific source root, or null if unbound; never instructions to access a path."
    )
    source_path: str | None = label(
        "Machine-specific absolute source path, or null if unbound."
    )


class Association(Record):
    """A file's active identification and role for one media item."""

    id: Id = label("Opaque association ID scoped to database_id.")
    file_id: Id = label("Reference to files[].id.")
    item_id: Id = label("Reference to items[].id.")
    role: Id = label("File role such as primary, subtitle, cover, or custom:name.")
    part: Positive | None = label("Optional one-based ordered part number.")
    metadata: dict[str, JsonValue] = label(
        "Unmodified association metadata, including language or identification evidence."
    )
    origin: Id = label(
        "Identification provenance, currently explicit, mapping, or migration."
    )
    active: bool = label("Must be true in this active-catalog projection.")


class Relationship(Record):
    """Directed relationship; outgoing closure is included without unrelated siblings."""

    id: Id = label("Opaque relationship ID scoped to database_id.")
    source_id: Id = label("Reference to the source items[].id.")
    target_id: Id = label("Reference to the target items[].id.")
    kind: Id = label("Relationship vocabulary name, including custom:name.")
    position: Positive | None = label(
        "Optional one-based position, meaningful for ordered relationships."
    )
    metadata: dict[str, JsonValue] = label(
        "Unmodified relationship metadata and extension fields."
    )
    active: bool = label("Must be true in this active-catalog projection.")


class Entry(Record):
    """Desired catalog destination; importing metadata does not authorize symlink writes."""

    mapping_id: Id = label("Opaque mapping ID scoped to database_id.")
    path: Id = label("Relative POSIX destination in this output catalog.")
    output_path: str | None = label(
        "Machine-specific absolute output path, or null if unbound."
    )
    item_id: Id = label("Reference to items[].id.")
    file_id: Id = label("Reference to files[].id.")
    association_ids: list[Id] = label(
        "All exported active identifications for this item/file pair."
    )
    managed_by_layout: Id | None = label(
        "Name of the managing layout, or null for an explicit mapping."
    )
    expected_link_target: str | None = label(
        "Computed relative symlink target, or null if bindings are unavailable."
    )
    recorded_link_target: str | None = label(
        "Last database-owned target; does not prove the link exists now."
    )
    recorded_link_matches_desired: bool | None = label(
        "Whether the recorded target equals the expected target; null when no expectation is available."
    )


class RecordedLink(Record):
    """Database ownership evidence, which may include stale destinations."""

    path: Id = label("Recorded relative output path.")
    target: str = label("Recorded link target, retained verbatim as metadata.")


class Layout(Record):
    """Saved naming/query provenance; current and last-applied definitions may differ."""

    name: Id = label("Saved layout name.")
    current_definition: dict[str, JsonValue] = label(
        "Opaque versioned layout definition; consumers must not execute embedded queries implicitly."
    )
    current_definition_sha256: Sha256 = label(
        "Checksum of current_definition using the profile's canonical encoding."
    )
    last_applied_definition_sha256: Sha256 = label(
        "Checksum of the definition used at the last apply."
    )
    definition_matches_last_apply: bool = label(
        "Whether current and last-applied definition checksums match."
    )


class Catalog(Record):
    """One output projection, not a complete database backup."""

    id: Id = label("Logical output catalog identifier.")
    output_root: str | None = label("Machine-specific output root, or null if unbound.")


class Counts(Record):
    """Collection sizes; each count must equal its corresponding array length."""

    entries: Nonnegative = label("Number of desired mapping entries.")
    items: Nonnegative = label("Number of included media items.")
    files: Nonnegative = label("Number of included source files.")
    associations: Nonnegative = label("Number of included active identifications.")
    relationships: Nonnegative = label(
        "Number of included active directed relationships."
    )
    recorded_links: Nonnegative = label("Number of recorded owned links.")


class Content(Record):
    """Checksummed catalog snapshot with profile-scoped observations."""

    database_id: Id = label("Origin database identity; IDs are scoped by this value.")
    database_schema: Positive = label(
        "Producer database schema version, independent of the interchange format version."
    )
    profile: Id = label("Machine profile used for bindings and observations.")
    catalog: Catalog = label("The exported output projection.")
    state: Literal["desired_catalog"] = label(
        "This document records desired catalog state."
    )
    filesystem_verified: bool = label(
        "Must be false: exporting does not verify live files or links."
    )
    layout: Layout | None = label(
        "Layout/query provenance, or null for an unmanaged catalog."
    )
    entries: list[Entry] = label(
        "Active desired destinations, each referencing included items and files."
    )
    items: list[Item] = label(
        "Mapped items and recursively reachable outgoing related items."
    )
    files: list[File] = label("Files referenced by desired entries.")
    associations: list[Association] = label(
        "Active identifications for mapped item/file pairs."
    )
    relationships: list[Relationship] = label(
        "Active outgoing relationship closure for included items."
    )
    recorded_links: list[RecordedLink] = label(
        "Recorded owned destinations, potentially including stale links."
    )
    extra: dict[str, JsonValue] = label(
        "User-provided document metadata; namespaced keys are recommended."
    )
    counts: Counts = label("Exact array lengths for the six record collections.")


class Generator(Record):
    """Producer information, independent of the schema version."""

    name: Id = label("Producer application name.", examples=["catabolic"])
    version: Id = label("Producer application version.", examples=["0.1.0"])


class Document(Record):
    """Open Catalog profile preserving the existing Catabolic manifest v1 wire format."""

    format: Literal[FORMAT] = label("Interchange profile identifier.")
    format_version: Literal[1] = label(
        "Wire-format version; reject unsupported versions without rewriting."
    )
    generator: Generator = label("Application that produced this snapshot.")
    generated_at: str = label(
        "Generation timestamp supplied by the producer, retained verbatim."
    )
    content_sha256: Sha256 = label(
        "SHA-256 of canonical content JSON; integrity only, not authentication."
    )
    content: Content = label("Complete checksummed projection content.")
