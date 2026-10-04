# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Reviewed file-level Plex matches, retained as accepted curation proposals."""

import hashlib
import json
import os
from pathlib import Path

from .app import Application
from .consumer_adapters import ConsumerError, translate, validate_remote
from .consumer_setup import get_connection
from .consumers import binding, connection
from .curation import Curation
from .filesystem import link_state, owner_state, root_handle
from .plex_import import page
from .reconcile import Reconciler
from .source_access import validated_source
from .store import Store, encode


def published_snapshot(app, binding_id, mapping_id):
    app.require_recovered()
    b = binding(app, binding_id)
    if not b["enabled"]:
        raise ConsumerError("metadata_binding_disabled")
    rows = app.store.rows(
        "SELECT * FROM mappings WHERE id=? AND catalog=? AND active=1",
        (mapping_id, b["catalog"]),
    )
    if len(rows) != 1:
        raise ConsumerError("metadata_mapping_not_active_in_binding")
    m = rows[0]
    associations = app.store.rows(
        "SELECT * FROM item_files WHERE file_id=? AND item_id=? AND active=1 AND role='primary'",
        (m["file_id"], m["item_id"]),
    )
    if len(associations) != 1:
        raise ConsumerError("metadata_requires_unique_primary_association")
    association = associations[0]
    payload = {"item_id": m["item_id"], "role": "primary", "part": association["part"]}
    snapshot = Curation(app).snapshot(m["file_id"], payload)
    output = app.binding("output", m["catalog"])
    source = app.binding("source", snapshot["file"]["location"])
    expected = os.path.relpath(
        Path(source["root"]) / snapshot["file"]["path"],
        (Path(output["root"]) / m["path"]).parent,
    )
    with validated_source(snapshot["file"]), root_handle(output) as fd:
        if owner_state(fd, Reconciler(app).owner(m["catalog"])) != "owned":
            raise ConsumerError("metadata_output_not_owned")
        owned = app.store.rows(
            "SELECT target FROM owned_links WHERE profile=? AND catalog=? AND path=?",
            (app.profile, m["catalog"], m["path"]),
        )
        if (
            len(owned) != 1
            or owned[0]["target"] != expected
            or link_state(fd, m["path"]) != ("link", expected)
        ):
            raise ConsumerError("metadata_published_link_changed")
    # Only identity-bearing binding fields: delivery counters are independent.
    return {
        "binding": {
            k: b[k]
            for k in (
                "id",
                "connection_id",
                "catalog",
                "subtree",
                "remote_root",
                "library",
                "revision",
                "enabled",
            )
        },
        "mapping": m,
        "association": association,
        "item": snapshot["item"][0],
        "output": dict(output),
        "source": dict(source),
        "remote_path": translate(b, m["path"]),
        "source_snapshot": snapshot["file"],
    }


def match(
    database,
    profile,
    binding_id,
    mapping_id,
    *,
    metadata_source="association",
    max_items=10000,
    apply=False,
    expected_plan=None,
):
    if (
        metadata_source not in ("association", "item")
        or type(max_items) is not int
        or not 1 <= max_items <= 10000
    ):
        raise ConsumerError("metadata_invalid_match_options")
    if apply and not expected_plan:
        raise ConsumerError("metadata_match_requires_expected_plan")
    with Store(database) as s:
        local = published_snapshot(Application(s, profile), binding_id, mapping_id)
        database_id = s.database_id
    c = get_connection(database, profile, local["binding"]["connection_id"])
    if c["application"] != "plex":
        raise ConsumerError("metadata_requires_plex_connection")
    library = json.loads(local["binding"]["library"])
    a, _ = validate_remote(c, library)
    offset = 0
    matches = []
    while True:
        data = page(a, library, offset, min(100, max_items - offset))
        if data["total"] > max_items:
            raise ConsumerError("metadata_match_library_exceeds_limit")
        matches.extend(
            r
            for r in data["records"]
            if any(p["path"] == local["remote_path"] for p in r["parts"])
        )
        if data["next_offset"] is None:
            break
        offset = data["next_offset"]
    if len(matches) != 1:
        raise ConsumerError("metadata_match_path_missing_or_ambiguous")
    remote = matches[0]
    from .plex_metadata import KINDS

    if (
        remote["reported_type"] != remote["kind"]
        or remote["kind"] not in KINDS
        or not remote["guid"]
    ):
        raise ConsumerError("metadata_match_unsupported_remote_item")
    if metadata_source == "item" and local["item"]["kind"] != remote["kind"]:
        raise ConsumerError("metadata_item_source_kind_mismatch_use_association")
    evidence = {
        "version": 1,
        "connection_id": c["id"],
        "server_id": c["server_id"],
        "library": {
            k: library[k] for k in ("id", "uuid", "type", "roots", "created_at")
        },
        "rating_key": remote["rating_key"],
        "kind": remote["kind"],
        "guid": remote["guid"],
        "remote_paths": sorted(p["path"] for p in remote["parts"]),
        "remote_path": local["remote_path"],
        "binding_id": binding_id,
        "mapping_id": mapping_id,
        "association_id": local["association"]["id"],
        "association_identity": {
            k: local["association"][k] for k in ("file_id", "item_id", "role", "part")
        },
        "source_snapshot": local["source_snapshot"],
        "output": local["output"],
        "source": local["source"],
        "metadata_source": metadata_source,
        "binding": local["binding"],
        "mapping": local["mapping"],
    }
    from .plex_metadata import remote_snapshot

    remote_snapshot(
        a,
        library,
        {
            "rating_key": remote["rating_key"],
            "kind": remote["kind"],
            "evidence": [{"evidence": encode(evidence)}],
        },
        [],
    )
    validate_remote(c, library)
    digest = hashlib.sha256(
        encode([database_id, profile, c, local, evidence]).encode()
    ).hexdigest()
    with Store(database, writable=apply) as s:
        app = Application(s, profile)
        if (
            connection(app, c["id"]) != c
            or published_snapshot(app, binding_id, mapping_id) != local
        ):
            raise ConsumerError("metadata_match_local_changed")
        if apply and digest != expected_plan:
            raise ConsumerError("metadata_match_plan_changed_preview_again")
        match_id = None
        if apply:
            # Associate an already associated file through its owning service.
            # No imported item, metadata overwrite, or identity replacement occurs.
            payload = {
                "item_id": local["mapping"]["item_id"],
                "role": "primary",
                "part": local["association"]["part"],
            }
            curator = Curation(app)
            proposal = curator.put(
                local["mapping"]["file_id"],
                payload,
                source="plex-match",
                evidence=evidence,
            )
            curator.accept_existing_association_evidence(
                proposal["id"], actor="plex-match"
            )
            match_id = proposal["id"]
    return {
        "plan_id": digest,
        "applied": apply,
        "match_id": match_id,
        "match": evidence,
        "item_kind": local["item"]["kind"],
        "complete": True,
    }


def matched_snapshot(database, profile, c, library, match_ids, fields, clear_fields=()):
    from .plex_metadata import KINDS, value

    with Store(database) as s:
        app = Application(s, profile)
        if connection(app, c["id"]) != c:
            raise ConsumerError("metadata_connection_changed")
        rows = []
        for match_id in match_ids:
            p = Curation(app).get(match_id)
            if p["source"] != "plex-match" or p["state"] != "accepted":
                raise ConsumerError("metadata_requires_accepted_match")
            e = p["evidence"]
            if (
                e["connection_id"] != c["id"]
                or e["server_id"] != c["server_id"]
                or e["library"]
                != {
                    k: library[k] for k in ("id", "uuid", "type", "roots", "created_at")
                }
            ):
                raise ConsumerError("metadata_match_connection_or_library_changed")
            local = published_snapshot(app, e["binding_id"], e["mapping_id"])
            if (
                local["binding"] != e["binding"]
                or local["mapping"] != e["mapping"]
                or local["association"]["id"] != e["association_id"]
                or local["remote_path"] != e["remote_path"]
                or local["source_snapshot"] != e["source_snapshot"]
                or local["output"] != e["output"]
                or local["source"] != e["source"]
                or {
                    k: local["association"][k]
                    for k in ("file_id", "item_id", "role", "part")
                }
                != e["association_identity"]
            ):
                raise ConsumerError("metadata_match_publication_changed")
            if e["metadata_source"] == "item" and local["item"]["kind"] != e["kind"]:
                raise ConsumerError(
                    "metadata_item_source_kind_mismatch_use_association"
                )
            if set(fields) - KINDS[e["kind"]][2]:
                raise ConsumerError("metadata_field_unsupported_for_item_kind")
            metadata = json.loads(local[e["metadata_source"]]["metadata"])
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
            rows.append(
                {
                    "item_id": local["item"]["id"],
                    "match_id": match_id,
                    "association_id": e["association_id"],
                    "kind": e["kind"],
                    "rating_key": e["rating_key"],
                    "desired": desired,
                    "evidence": [{"evidence": encode(e)}],
                    "publication": local,
                }
            )
        if len({r["rating_key"] for r in rows}) != len(rows):
            raise ConsumerError("metadata_duplicate_remote_item")
        return {"database_id": s.database_id, "items": rows}
