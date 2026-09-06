# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Non-mutating document validation and lossless JSON-value round trips."""

import hashlib
import json

from pydantic import ValidationError

from ..domain import CatabolicError
from ..store import encode
from ..tagging import tag_name, text_value
from .v1 import FORMAT
from .v1 import Document as DocumentV1
from .v2 import Document as DocumentV2
from .v3 import Document as DocumentV3

MAX_BYTES = 128 * 1024 * 1024
MAX_RECORDS = 100000
COLLECTIONS = (
    "entries",
    "items",
    "files",
    "associations",
    "relationships",
    "recorded_links",
)


def digest(content):
    return hashlib.sha256(encode(content).encode()).hexdigest()


def _index(records, key, label):
    result = {getattr(row, key): row for row in records}
    if len(result) != len(records):
        raise CatabolicError(f"duplicate {label} identifiers")
    return result


def validate_document(value):
    """Validate types, references and integrity without resolving paths or queries."""
    if (
        not isinstance(value, dict)
        or value.get("format") != FORMAT
        or type(value.get("format_version")) is not int
        or value["format_version"] not in (1, 2, 3)
    ):
        raise CatabolicError(
            "unsupported catalog format or version; no conversion attempted"
        )
    collections = COLLECTIONS + (
        ("tags", "tag_names", "tag_parents", "taggings")
        if value["format_version"] >= 2
        else ()
    )
    collections += (
        ("hardlinks", "retained_hardlinks") if value["format_version"] == 3 else ()
    )
    Document = {1: DocumentV1, 2: DocumentV2, 3: DocumentV3}[value["format_version"]]
    raw_content = value.get("content")
    if isinstance(raw_content, dict):
        for key in collections:
            rows = raw_content.get(key)
            if isinstance(rows, list) and len(rows) > MAX_RECORDS:
                raise CatabolicError(
                    f"catalog exceeds {MAX_RECORDS} {key}; partition the export"
                )
    try:
        # Reject nonfinite JSON numbers even in arbitrary nested extensions.
        # Serialize before constructing models to avoid overlapping large buffers.
        expected = digest(raw_content)
        encode(value)
        document = Document.model_validate(value)
    except (ValidationError, ValueError, TypeError, RecursionError) as exc:
        if isinstance(exc, ValidationError):
            first = exc.errors(include_input=False, include_url=False)[0]
            detail = f"{'.'.join(map(str, first['loc']))}: {first['msg']}"
        else:
            detail = "document must contain finite JSON values"
        raise CatabolicError(f"invalid catalog document: {detail}") from exc
    if expected != document.content_sha256:
        raise CatabolicError("catalog content checksum mismatch")
    content = document.content
    if content.filesystem_verified:
        raise CatabolicError("this profile cannot claim live filesystem verification")
    for key in collections:
        rows = getattr(content, key)
        if getattr(content.counts, key) != len(rows):
            raise CatabolicError(f"catalog count mismatch: {key}")
    items = _index(content.items, "id", "item")
    files = _index(content.files, "id", "file")
    associations = _index(content.associations, "id", "association")
    _index(content.entries, "mapping_id", "mapping")
    _index(content.entries, "path", "destination")
    _index(content.relationships, "id", "relationship")
    _index(content.recorded_links, "path", "recorded link")
    identities = set()
    for item in content.items:
        for identity in item.identities:
            pair = (identity.namespace, identity.value)
            if pair in identities:
                raise CatabolicError("duplicate provider identity")
            identities.add(pair)
    pairs = {}
    for association in content.associations:
        if (
            not association.active
            or association.item_id not in items
            or association.file_id not in files
        ):
            raise CatabolicError(
                "association must be active and reference included items/files"
            )
        pairs.setdefault((association.item_id, association.file_id), set()).add(
            association.id
        )
    edges = set()
    for edge in content.relationships:
        if (
            not edge.active
            or edge.source_id not in items
            or edge.target_id not in items
            or edge.source_id == edge.target_id
        ):
            raise CatabolicError(
                "relationship must be active and reference distinct included items"
            )
        key = (edge.source_id, edge.target_id, edge.kind)
        if key in edges:
            raise CatabolicError("duplicate directed relationship")
        edges.add(key)
    for entry in content.entries:
        if entry.item_id not in items or entry.file_id not in files:
            raise CatabolicError("entry references an absent item/file")
        ids = set(entry.association_ids)
        if (
            len(ids) != len(entry.association_ids)
            or not ids
            or ids != pairs.get((entry.item_id, entry.file_id), set())
            or not ids <= associations.keys()
        ):
            raise CatabolicError(
                "entry association IDs must match its included item/file identifications"
            )
        if entry.managed_by_layout is not None and (
            content.layout is None or entry.managed_by_layout != content.layout.name
        ):
            raise CatabolicError("entry references an absent managing layout")
        matches = (
            entry.recorded_link_target == entry.expected_link_target
            if entry.expected_link_target is not None
            else None
        )
        if entry.recorded_link_matches_desired is not matches:
            raise CatabolicError(
                "entry link comparison does not match recorded targets"
            )
    if content.layout is not None:
        layout = content.layout
        # Use the original raw object so unknown definition fields participate.
        if layout.current_definition_sha256 != digest(
            value["content"]["layout"]["current_definition"]
        ):
            raise CatabolicError("layout definition checksum mismatch")
        if layout.definition_matches_last_apply != (
            layout.current_definition_sha256 == layout.last_applied_definition_sha256
        ):
            raise CatabolicError("layout definition comparison mismatch")
    if value["format_version"] >= 2:
        validate_tags(content, items, files)
    if value["format_version"] == 3:
        _index(content.hardlinks, "path", "hardlink path")
        _index(content.retained_hardlinks, "id", "retirement")
        _index(content.retained_hardlinks, "path", "retained path")
        if content.catalog.link_mode == "symlink" and (
            content.hardlinks or content.retained_hardlinks
        ):
            raise CatabolicError("symlink catalog cannot claim hardlink ownership")
        if content.catalog.link_mode == "hardlink":
            if content.recorded_links or any(
                entry.expected_link_target is not None
                or entry.recorded_link_target is not None
                or entry.recorded_link_matches_desired is not None
                for entry in content.entries
            ):
                raise CatabolicError("hardlink catalog cannot claim symlink targets")
    return document


def _object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise CatabolicError("duplicate JSON object key; refusing lossy parsing")
        result[key] = value
    return result


def _constant(value):
    raise CatabolicError(f"nonfinite JSON number is not permitted: {value}")


def decode_document(raw):
    if len(raw.encode() if isinstance(raw, str) else raw) > MAX_BYTES:
        raise CatabolicError("catalog input exceeds 128 MiB")
    try:
        value = json.loads(raw, object_pairs_hook=_object, parse_constant=_constant)
    except (ValueError, RecursionError, UnicodeError) as exc:
        raise CatabolicError("invalid catalog JSON") from exc
    return validate_document(value)


def document_value(document):
    """Preserve unknown fields and nulls; no coercion, defaults, or filtering."""
    return document.model_dump(mode="python", round_trip=True)


def encode_document(document):
    value = document_value(document)
    validate_document(value)
    raw = encode(value) + "\n"
    if len(raw.encode()) > MAX_BYTES:
        raise CatabolicError("catalog output exceeds 128 MiB")
    return raw


def validate_tags(content, items, files):
    tags = _index(content.tags, "id", "tag")
    _index(content.tags, "name", "canonical tag name")
    names = _index(content.tag_names, "name", "tag name")
    _index(content.taggings, "id", "tagging")
    for row in content.tag_names:
        if row.tag_id not in tags or tag_name(row.name) != row.name:
            raise CatabolicError(
                "tag name must be normalized and reference an included tag"
            )
    for row in content.tags:
        if row.name not in names or names[row.name].tag_id != row.id:
            raise CatabolicError("canonical tag name must resolve to its included tag")
        text_value(row.description, "tag description", 4000, empty=True)
    edges = set()
    parents = {identifier: set() for identifier in tags}
    children = {identifier: set() for identifier in tags}
    for edge in content.tag_parents:
        pair = (edge.child_id, edge.parent_id)
        if edge.child_id not in tags or edge.parent_id not in tags or pair in edges:
            raise CatabolicError("invalid or duplicate tag parent edge")
        edges.add(pair)
        parents[edge.child_id].add(edge.parent_id)
        children[edge.parent_id].add(edge.child_id)
    # Iterative topological check avoids Python recursion limits on deep trees.
    pending = {identifier: len(value) for identifier, value in parents.items()}
    ready = [identifier for identifier, count in pending.items() if count == 0]
    visited = 0
    while ready:
        identifier = ready.pop()
        visited += 1
        for child in children[identifier]:
            pending[child] -= 1
            if pending[child] == 0:
                ready.append(child)
    if visited != len(tags):
        raise CatabolicError("tag hierarchy contains a cycle")
    assignments = set()
    for row in content.taggings:
        pair = (row.subject_type, row.subject_id, row.tag_id, row.source)
        subjects = items if row.subject_type == "item" else files
        if (
            row.subject_id not in subjects
            or row.tag_id not in tags
            or pair in assignments
        ):
            raise CatabolicError("invalid or duplicate tag assignment")
        text_value(row.source, "tag source", 200)
        text_value(row.note, "tag note", 4000, empty=True)
        assignments.add(pair)
