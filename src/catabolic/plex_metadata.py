# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Explicit, reviewed Plex metadata publication. HTTP never owns a Store lock."""

import hashlib
import json
import re
from urllib.parse import quote, urlencode
from uuid import uuid4

from .app import Application
from .consumer_adapters import ConsumerError, path, text, validate_remote
from .consumer_setup import get_connection
from .consumers import connection
from .metadata_publication import serialize_writes, value
from .store import Store, encode

# Scalar metadata only. In particular, catalog tags are not Plex genres/labels.
FIELDS = {
    "title": "title",
    "sort_title": "titleSort",
    "original_title": "originalTitle",
    "summary": "summary",
    "year": "year",
    "release_date": "originallyAvailableAt",
    "tagline": "tagline",
    "studio": "studio",
    "content_rating": "contentRating",
    "edition": "editionTitle",
}
KINDS = {
    "movie": ("movie", 1, set(FIELDS)),
    "episode": (
        "show",
        4,
        {"title", "sort_title", "summary", "release_date", "content_rating"},
    ),
    "track": ("artist", 10, {"title"}),
    "photo": ("photo", 13, {"title", "sort_title", "summary"}),
}


def local_snapshot(database, profile, c, library, item_ids, fields, clear_fields=()):
    with Store(database) as store:
        app = Application(store, profile)
        if connection(app, c["id"]) != c:
            raise ConsumerError("metadata_connection_changed")
        app.require_recovered()
        rows = []
        for item_id in item_ids:
            found = store.rows("SELECT * FROM items WHERE id=?", (item_id,))
            if len(found) != 1:
                raise ConsumerError("metadata_unknown_item")
            item = found[0]
            if item["kind"] not in KINDS or KINDS[item["kind"]][0] != library["type"]:
                raise ConsumerError("metadata_item_kind_mismatch")
            if set(fields) - KINDS[item["kind"]][2]:
                raise ConsumerError("metadata_field_unsupported_for_item_kind")
            metadata = json.loads(item["metadata"])
            if any(
                f not in metadata or metadata[f] is None
                for f in fields
                if f not in clear_fields
            ):
                raise ConsumerError("metadata_selected_field_missing")
            desired = {
                f: (None if f == "year" else "")
                if f in clear_fields
                else value(f, metadata[f])
                for f in fields
            }
            identities = store.rows(
                "SELECT value FROM identities WHERE namespace='plex' AND item_id=? ORDER BY value",
                (item_id,),
            )
            keys = []
            for identity in identities:
                try:
                    parts = json.loads(identity["value"])
                except ValueError:
                    continue
                if (
                    isinstance(parts, list)
                    and len(parts) == 3
                    and parts[:2] == [c["server_id"], library["uuid"]]
                ):
                    keys.append(parts[2])
            if (
                len(keys) != 1
                or not isinstance(keys[0], str)
                or not re.fullmatch(r"[0-9]{1,20}", keys[0])
            ):
                raise ConsumerError("metadata_requires_unique_imported_plex_identity")
            key = keys[0]
            # Accepted import evidence ties this identity to an active file association
            # in this profile. A manually supplied rating key alone is insufficient.
            proofs = store.rows(
                "SELECT p.id,p.file_id,p.evidence FROM proposals p "
                "JOIN item_files f ON f.file_id=p.file_id AND f.item_id=? AND f.active=1 AND f.role='primary' "
                "WHERE p.profile=? AND p.source='plex-import' AND p.state='accepted' "
                "AND json_extract(p.result,'$.item_id')=? ORDER BY p.rowid DESC LIMIT 101",
                (item_id, profile, item_id),
            )
            if len(proofs) > 100:
                raise ConsumerError("metadata_import_evidence_limit")
            evidence = []
            for proof in proofs:
                e = json.loads(proof["evidence"])
                if (
                    e.get("server_id") == c["server_id"]
                    and e.get("library", {}).get("uuid") == library["uuid"]
                    and e.get("rating_key") == key
                ):
                    evidence.append(proof)
            if not evidence or any(
                not json.loads(p["evidence"]).get("guid") for p in evidence
            ):
                raise ConsumerError("metadata_requires_import_provenance")
            rows.append(
                {
                    "item_id": item_id,
                    "kind": item["kind"],
                    "rating_key": key,
                    "desired": desired,
                    "evidence": evidence,
                }
            )
        if len({r["rating_key"] for r in rows}) != len(rows):
            raise ConsumerError("metadata_duplicate_remote_item")
        return {"database_id": store.database_id, "items": rows}


def remote_snapshot(a, library, local, fields):
    key = local["rating_key"]
    if not isinstance(key, str) or not re.fullmatch(r"[0-9]{1,20}", key):
        raise ConsumerError("metadata_invalid_rating_key")
    root = a.xml("/library/metadata/" + quote(key, safe=""), {"includeGuids": 1})
    if root.tag != "MediaContainer" or len(root) != 1:
        raise ConsumerError("metadata_invalid_item_response")
    node = root[0]
    if (
        node.get("ratingKey") != key
        or node.get("librarySectionID", root.get("librarySectionID")) != library["id"]
        or node.get("type") != local["kind"]
    ):
        raise ConsumerError("metadata_remote_item_mismatch")
    guid = text(node.get("guid"))
    paths = sorted({path(p.get("file")) for p in node.findall("Media/Part")})
    if not paths or len(paths) > 100:
        raise ConsumerError("metadata_invalid_item_paths")
    for proof in local["evidence"]:
        evidence = json.loads(proof["evidence"])
        if (
            evidence["guid"] != guid
            or evidence["remote_path"] not in paths
            or (
                "remote_paths" in evidence and sorted(evidence["remote_paths"]) != paths
            )
        ):
            raise ConsumerError("metadata_import_identity_changed")
    locks = {}
    for f in node.findall("Field"):
        name = f.get("name")
        if name in locks or f.get("locked") not in ("0", "1"):
            raise ConsumerError("metadata_invalid_field_locks")
        locks[name] = f.get("locked") == "1"
    values = {}
    for field in fields:
        raw = node.get(FIELDS[field], "")
        if len(raw) > 65536:
            raise ConsumerError("metadata_remote_field_too_large")
        if field == "year":
            if raw and not re.fullmatch(r"[0-9]{1,4}", raw):
                raise ConsumerError("metadata_invalid_remote_year")
            raw = int(raw) if raw else None
        values[field] = {"value": raw, "locked": locks.get(FIELDS[field], False)}
    return {"guid": guid, "paths": paths, "fields": values}


def parameters(row):
    params = {"id": row["rating_key"], "type": KINDS[row["kind"]][1]}
    for field, change in row["changes"].items():
        params[FIELDS[field] + ".value"] = (
            change["after"]["value"] if change["after"]["value"] is not None else ""
        )
        params[FIELDS[field] + ".locked"] = int(change["after"]["locked"])
    if len(urlencode(params).encode()) > 16384:
        raise ConsumerError("metadata_request_too_large_select_fewer_fields")
    return params


def record_attempt(database, profile, event_id, payload, *, create=False):
    with Store(database, writable=True) as store, store.transaction() as db:
        if create:
            db.execute(
                "INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES (?,?,'metadata_publish','info',?)",
                (event_id, profile, encode(payload)),
            )
        else:
            db.execute(
                "UPDATE consumer_events SET subject=?,severity=? WHERE id=? AND profile=? AND event='metadata_publish'",
                (
                    encode(payload),
                    "info" if payload["state"] == "verified" else "warning",
                    event_id,
                    profile,
                ),
            )


@serialize_writes
def run(
    database,
    profile,
    identifier,
    library_id,
    item_ids,
    fields,
    *,
    lock_fields=False,
    unlock_fields=False,
    clear_fields=(),
    match_ids=None,
    apply=False,
    expected_plan=None,
):
    if match_ids is not None:
        if item_ids:
            raise ConsumerError("metadata_select_items_or_matches")
        item_ids = match_ids
    if not isinstance(item_ids, (list, tuple)) or not 1 <= len(item_ids) <= 100:
        raise ConsumerError("metadata_requires_1_to_100_items")
    item_ids = sorted({text(i, 255) for i in item_ids})
    if (
        not isinstance(fields, (list, tuple))
        or not fields
        or any(f not in FIELDS for f in fields)
    ):
        raise ConsumerError("metadata_invalid_fields")
    fields = sorted(set(fields))
    if (
        type(lock_fields) is not bool
        or type(unlock_fields) is not bool
        or type(apply) is not bool
        or (lock_fields and unlock_fields)
    ):
        raise ConsumerError("metadata_invalid_options")
    if (
        not isinstance(clear_fields, (list, tuple))
        or set(clear_fields) - set(fields)
        or "title" in clear_fields
    ):
        raise ConsumerError("metadata_invalid_clear_fields")
    if match_ids is not None:
        from .plex_matching import matched_snapshot as snapshot
    else:
        snapshot = local_snapshot
    if apply and not expected_plan:
        raise ConsumerError("metadata_apply_requires_expected_plan")
    c = get_connection(database, profile, identifier)
    if c["application"] != "plex":
        raise ConsumerError("metadata_requires_plex_connection")
    a, _ = validate_remote(c)
    libraries = [lib for lib in a.libraries() if lib["id"] == library_id]
    if len(libraries) != 1:
        raise ConsumerError("metadata_unknown_library")
    library = {
        k: libraries[0][k] for k in ("id", "uuid", "type", "roots", "created_at")
    }
    local = snapshot(database, profile, c, library, item_ids, fields, clear_fields)
    planned = []
    for item in local["items"]:
        remote = remote_snapshot(a, library, item, fields)
        changes = {}
        for field in fields:
            before = remote["fields"][field]
            after = {
                "value": item["desired"][field],
                "locked": False if unlock_fields else lock_fields or before["locked"],
            }
            if before != after:
                changes[field] = {"before": before, "after": after}
        row = {
            "item_id": item["item_id"],
            **(
                {"match_id": item["match_id"], "association_id": item["association_id"]}
                if "match_id" in item
                else {}
            ),
            "rating_key": item["rating_key"],
            "kind": item["kind"],
            "remote": remote,
            "changes": changes,
        }
        parameters(row)  # Validate the complete batch before any mutation.
        planned.append(row)
    validate_remote(c, library)
    if snapshot(database, profile, c, library, item_ids, fields, clear_fields) != local:
        raise ConsumerError("metadata_local_changed_preview_again")
    encoded = encode(
        [
            profile,
            c,
            library,
            local,
            fields,
            lock_fields,
            unlock_fields,
            clear_fields,
            planned,
        ]
    ).encode()
    if len(encoded) > 4 * 1024 * 1024:
        raise ConsumerError("metadata_plan_too_large_reduce_items")
    digest = hashlib.sha256(encoded).hexdigest()
    if apply and expected_plan != digest:
        raise ConsumerError("metadata_plan_changed_preview_again")
    result = {
        "plan_id": digest,
        "applied": apply,
        "connection": identifier,
        "library_id": library_id,
        "items": planned,
        "results": [],
        "complete": True,
    }
    if not apply:
        return result
    for item, row in zip(local["items"], planned, strict=True):
        target = {
            k: row[k]
            for k in ("item_id", "rating_key", "match_id", "association_id")
            if k in row
        }
        if not row["changes"]:
            result["results"].append({**target, "state": "unchanged"})
            continue
        event_id = str(uuid4())
        attempt = {
            "plan_id": digest,
            "connection": identifier,
            "server_id": c["server_id"],
            "library": library,
            **row,
            "state": "started",
        }
        started = False
        try:
            a, _ = validate_remote(c, library)
            if (
                snapshot(database, profile, c, library, item_ids, fields, clear_fields)
                != local
            ):
                raise ConsumerError("metadata_local_changed_preview_again")
            if remote_snapshot(a, library, item, fields) != row["remote"]:
                raise ConsumerError("metadata_remote_changed_preview_again")
            # Persist intent before PUT. A process interruption leaves a visible
            # started event. Recovery is always a new preview, never a blind retry.
            record_attempt(database, profile, event_id, attempt, create=True)
            started = True
            a.call(
                "/library/sections/" + quote(library_id, safe="") + "/all",
                "PUT",
                parameters(row),
            )
            validate_remote(c, library)
            observed = remote_snapshot(a, library, item, fields)
            if any(
                observed["fields"][f] != change["after"]
                for f, change in row["changes"].items()
            ):
                raise ConsumerError("metadata_verification_failed")
            if any(
                observed["fields"][f] != row["remote"]["fields"][f]
                for f in fields
                if f not in row["changes"]
            ):
                raise ConsumerError("metadata_verification_failed")
            if (
                snapshot(database, profile, c, library, item_ids, fields, clear_fields)
                != local
            ):
                raise ConsumerError("metadata_local_changed_during_publish")
            attempt.update(state="verified", observed=observed)
            record_attempt(database, profile, event_id, attempt)
            result["results"].append(
                {**target, "event_id": event_id, "state": "verified"}
            )
        except ConsumerError as exc:
            state = "uncertain" if started else "blocked"
            error = {
                **exc.result(),
                "safe_to_retry": False,
                "recovery": "preview_again",
                "message": f"Plex metadata {exc.code}; inspect the item and preview again",
            }
            attempt.update(state=state, error=error)
            if started:
                record_attempt(database, profile, event_id, attempt)
            result["results"].append(
                {
                    **target,
                    "event_id": event_id if started else None,
                    "state": state,
                    "error": error,
                }
            )
            result["complete"] = False
            break
    result["remaining_item_ids"] = [
        r["item_id"] for r in planned[len(result["results"]) :]
    ]
    result["remaining_match_ids"] = [
        r["match_id"] for r in planned[len(result["results"]) :] if "match_id" in r
    ]
    return result
