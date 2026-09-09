# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Bounded Plex metadata intake through existing curation decisions."""

import hashlib
import json
from pathlib import PurePosixPath
from urllib.parse import quote
from uuid import NAMESPACE_URL, uuid5

from .app import Application
from .consumer_adapters import ConsumerError, path, text, validate_remote, within
from .consumer_setup import get_connection
from .consumers import connection
from .curation import Curation, payload_object
from .domain import CatabolicError
from .source_access import validated_source
from .store import Store, encode

TYPES = {
    "movie": (1, "movie"),
    "show": (4, "episode"),
    "artist": (10, "track"),
    "photo": (13, "photo"),
}
FIELDS = {
    "title": "title",
    "year": "year",
    "summary": "summary",
    "originalTitle": "original_title",
    "editionTitle": "edition",
    "originallyAvailableAt": "release_date",
    "duration": "duration_ms",
}


def mappings(value):
    if not isinstance(value, list) or not 1 <= len(value) <= 100:
        raise ConsumerError("import_mapping_requires_1_to_100_entries")
    result = []
    for entry in value:
        if not isinstance(entry, dict) or set(entry) - {
            "remote_root",
            "location",
            "subtree",
        }:
            raise ConsumerError("invalid_import_mapping")
        row = {
            "remote_root": path(entry.get("remote_root")),
            "location": text(entry.get("location"), 255),
            "subtree": path(entry.get("subtree", ""), relative=True),
        }
        if any(
            within(row["remote_root"], old["remote_root"])
            or within(old["remote_root"], row["remote_root"])
            for old in result
        ):
            raise ConsumerError("overlapping_import_mappings")
        result.append(row)
    return sorted(result, key=lambda row: row["remote_root"])


def integer(value, label):
    try:
        if not isinstance(value, str) or not value.isascii() or not value.isdecimal():
            raise ValueError
        return int(value)
    except ValueError:
        raise ConsumerError("invalid_import_" + label) from None


def page(adapter, library, offset, limit):
    if library["type"] not in TYPES:
        raise ConsumerError("unsupported_import_library_type")
    number, kind = TYPES[library["type"]]
    root = adapter.xml(
        "/library/sections/" + quote(library["id"], safe="") + "/all",
        {
            "type": number,
            "includeGuids": 1,
            # PlexAPI exposes the rating-key sort under its wire name, id.
            "sort": "id:asc",
            "X-Plex-Container-Start": offset,
            "X-Plex-Container-Size": limit,
        },
    )
    if root.tag != "MediaContainer":
        raise ConsumerError("invalid_response")
    rows = list(root)
    total = integer(root.get("totalSize"), "total_size")
    returned_offset = integer(root.get("offset", "0"), "offset")
    size = integer(root.get("size"), "size")
    if (
        returned_offset != offset
        or size != len(rows)
        or size > limit
        or offset + size > total
        or (size == 0 and offset < total)
    ):
        raise ConsumerError("inconsistent_import_pagination")
    records, seen = [], set()
    for item in rows:
        key = text(item.get("ratingKey"), 128)
        if key in seen:
            raise ConsumerError("duplicate_import_rating_key")
        seen.add(key)
        metadata = {}
        for remote, local in FIELDS.items():
            value = item.get(remote)
            if value not in (None, ""):
                metadata[local] = (
                    integer(value, remote)
                    if remote in ("year", "duration")
                    else text(value, 65536)
                )
        if kind == "episode":
            extra = {
                "grandparentTitle": "series",
                "parentIndex": "season",
                "index": "episode",
            }
        elif kind == "track":
            extra = {
                "grandparentTitle": "artist",
                "parentTitle": "album",
                "parentIndex": "disc",
                "index": "track",
            }
        elif kind == "photo":
            extra = {"parentTitle": "album"}
        else:
            extra = {}
        for remote, local in extra.items():
            if item.get(remote) is not None:
                metadata[local] = (
                    integer(item.get(remote), remote)
                    if remote.endswith("Index") or remote == "index"
                    else text(item.get(remote))
                )
        parts = []
        for media in item.findall("Media"):
            children = media.findall("Part")
            for index, part in enumerate(children, 1):
                parts.append(
                    {
                        "path": path(part.get("file")),
                        "part": index if len(children) > 1 else None,
                    }
                )
        if len(parts) > 100:
            raise ConsumerError("import_part_limit")
        record = {
            "rating_key": key,
            "kind": kind,
            "metadata": metadata,
            "parts": parts,
            "guids": sorted({text(g.get("id")) for g in item.findall("Guid")}),
            "guid": item.get("guid"),
            "reported_type": item.get("type"),
        }
        payload_object(record)
        records.append(record)
    if sum(len(record["parts"]) for record in records) > 1000:
        raise ConsumerError("import_page_part_limit")
    return {
        "records": records,
        "total": total,
        "next_offset": offset + size if offset + size < total else None,
    }


def candidate(app, record, part, identity, map_rows):
    matches = [m for m in map_rows if within(part["path"], m["remote_root"])]
    if len(matches) != 1:
        raise ConsumerError("unmapped_remote_path")
    mapping = matches[0]
    relative = PurePosixPath(part["path"]).relative_to(mapping["remote_root"])
    local = path(str(PurePosixPath(mapping["subtree"]) / relative), relative=True)
    files = app.store.rows(
        "SELECT id FROM files WHERE location=? AND path=?", (mapping["location"], local)
    )
    if len(files) != 1:
        raise ConsumerError("file_not_in_inventory")
    file_id = files[0]["id"]
    item_matches = app.store.rows(
        "SELECT item_id FROM identities WHERE namespace='plex' AND value=?", (identity,)
    )
    item_id = (
        item_matches[0]["item_id"]
        if item_matches
        else str(uuid5(NAMESPACE_URL, "catabolic:plex:" + identity))
    )
    payload = {
        "item": {
            "id": item_id,
            "kind": record["kind"],
            "identities": {"plex": identity},
            "metadata": record["metadata"],
        },
        "role": "primary",
        "part": part["part"],
    }
    payload_object(payload)
    snapshot = Curation(app).snapshot(file_id, payload)
    payload_object(snapshot)
    if snapshot["file"]["status"] != "present":
        raise ConsumerError("source_not_present")
    with validated_source(snapshot["file"]):
        pass
    active = [a for a in snapshot["associations"] if a["active"]]
    if any(
        a["item_id"] != item_id or a["role"] != "primary" or a["part"] != part["part"]
        for a in active
    ):
        raise ConsumerError("existing_association_conflict")
    if part["part"] is not None and app.store.rows(
        "SELECT id FROM item_files WHERE item_id=? AND role='primary' AND part=? AND active=1 AND file_id!=?",
        (item_id, part["part"], file_id),
    ):
        raise ConsumerError("existing_part_conflict")
    previous = app.store.rows(
        "SELECT evidence FROM proposals WHERE source='plex-import' AND state='accepted' "
        "AND json_extract(result,'$.item_id')=? ORDER BY rowid DESC LIMIT 1",
        (item_id,),
    )
    if previous:
        old_guid = json.loads(previous[0]["evidence"]).get("guid")
        if old_guid and record["guid"] != old_guid:
            raise ConsumerError("plex_item_guid_changed")
    existing = snapshot["item"]
    if existing and existing[0]["kind"] != record["kind"]:
        raise ConsumerError("existing_item_kind_conflict")
    old_metadata = json.loads(existing[0]["metadata"]) if existing else {}
    preserved = sorted(
        k
        for k, v in record["metadata"].items()
        if k in old_metadata and old_metadata[k] != v
    )
    unchanged = bool(
        item_matches and active and all(k in old_metadata for k in record["metadata"])
    )
    return {
        "file_id": file_id,
        "location": mapping["location"],
        "path": local,
        "payload": payload,
        "snapshot": snapshot,
        "action": "unchanged" if unchanged else "import",
        "preserved_fields": preserved,
    }


def run(
    database,
    profile,
    identifier,
    library_id,
    map_rows,
    *,
    offset=0,
    limit=100,
    apply=False,
    expected_plan=None,
):
    if (
        type(offset) is not int
        or not 0 <= offset <= 2147483647
        or type(limit) is not int
        or not 1 <= limit <= 100
    ):
        raise ConsumerError("invalid_import_page")
    if apply and not expected_plan:
        raise ConsumerError("import_apply_requires_expected_plan")
    map_rows = mappings(map_rows)
    c = get_connection(database, profile, identifier)
    if c["application"] != "plex":
        raise ConsumerError("plex_import_requires_plex_connection")
    a, _ = validate_remote(c)
    libraries = [lib for lib in a.libraries() if lib["id"] == library_id]
    if len(libraries) != 1:
        raise ConsumerError("unknown_import_library")
    library = {
        k: libraries[0][k] for k in ("id", "uuid", "type", "roots", "created_at")
    }
    data = page(a, library, offset, limit)
    # Recheck identity after the read; never hold the local writer lock across HTTP.
    validate_remote(c, library)
    with Store(database, writable=apply) as store:
        app = Application(store, profile)
        if connection(app, identifier) != c:
            raise ConsumerError("import_connection_changed")
        app.require_recovered()
        for mapping in map_rows:
            app.binding("source", mapping["location"])
        candidates, deferred = [], []
        for record in data["records"]:
            if (
                record["reported_type"] != record["kind"]
                or not record["metadata"].get("title")
                or not record["parts"]
            ):
                deferred.append(
                    {
                        "rating_key": record["rating_key"],
                        "reason": "unsupported_or_incomplete_item",
                    }
                )
                continue
            identity = encode([c["server_id"], library["uuid"], record["rating_key"]])
            for part in record["parts"]:
                evidence = {
                    "server_id": c["server_id"],
                    "library": library,
                    "rating_key": record["rating_key"],
                    "remote_path": part["path"],
                    "guids": record["guids"],
                    "guid": record["guid"],
                }
                try:
                    row = candidate(app, record, part, identity, map_rows)
                    row["evidence"] = evidence
                    candidates.append(row)
                except (CatabolicError, OSError) as exc:
                    deferred.append(
                        {
                            "rating_key": record["rating_key"],
                            "remote_path": part["path"],
                            "reason": str(exc),
                        }
                    )
        # A file mentioned more than once is ambiguous even if each row looks valid alone.
        counts = {}
        slots = {}
        for row in candidates:
            counts[row["file_id"]] = counts.get(row["file_id"], 0) + 1
            slot = (row["payload"]["item"]["id"], row["payload"]["part"])
            slots[slot] = slots.get(slot, 0) + 1
        unique = []
        for row in candidates:
            slot = (row["payload"]["item"]["id"], row["payload"]["part"])
            if counts[row["file_id"]] > 1 or (slot[1] is not None and slots[slot] > 1):
                deferred.append(
                    {
                        "file_id": row["file_id"],
                        "reason": "duplicate_remote_file_or_part",
                    }
                )
            else:
                unique.append(row)
        candidates = unique
        if len(encode([data, candidates, deferred]).encode()) > 4 * 1024 * 1024:
            raise ConsumerError("import_plan_too_large_reduce_limit")
        digest = hashlib.sha256(
            encode(
                [
                    store.database_id,
                    profile,
                    c,
                    library,
                    map_rows,
                    offset,
                    limit,
                    data,
                    candidates,
                    deferred,
                ]
            ).encode()
        ).hexdigest()
        if apply and expected_plan != digest:
            raise ConsumerError("import_plan_changed_preview_again")
        applied = []
        errors = []
        if apply:
            curator = Curation(app)
            for row in candidates:
                if row["action"] == "unchanged":
                    continue
                try:
                    proposal = curator.put(
                        row["file_id"],
                        row["payload"],
                        source="plex-import",
                        evidence=row["evidence"],
                    )
                    result = curator.decide(
                        proposal["id"], accept=True, actor="plex-import"
                    )
                    applied.append({"proposal_id": proposal["id"], **result["result"]})
                except (CatabolicError, OSError) as exc:
                    errors.append({"file_id": row["file_id"], "reason": str(exc)})
                    break
        return {
            "plan_id": digest,
            "applied": apply,
            "imported": applied,
            "candidates": [
                {k: v for k, v in row.items() if k != "snapshot"} for row in candidates
            ],
            "deferred": deferred,
            "errors": errors,
            "offset": offset,
            "next_offset": data["next_offset"],
            "total": data["total"],
            "page_complete": not deferred and not errors,
            "complete": data["next_offset"] is None and not deferred and not errors,
        }
