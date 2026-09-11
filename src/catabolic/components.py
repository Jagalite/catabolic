# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""A revision-pinned index over retained probe jobs and file identifications.

Only locators and evidence references are indexed. Technical metadata continues
to belong to the probe job; identity/compatibility assertions belong to proposals.
"""

import hashlib
import json
from uuid import UUID, uuid5

from .curation import occurrence
from .domain import CatabolicError
from .store import encode

KINDS = {"subtitle": "subtitle", "custom:audio": "audio", "custom:video": "video"}


def revision(snapshot):
    return hashlib.sha256(
        encode({k: v for k, v in snapshot.items() if k != "ctime_ns"}).encode()
    ).hexdigest()


def index_file(store, profile, file_id, *, historical=False):
    if store.schema_version < 25:
        return
    associations = store.rows(
        "SELECT * FROM item_files WHERE file_id=? AND (active=1 OR EXISTS (SELECT 1 FROM media_outputs mo WHERE mo.file_id=item_files.file_id AND mo.item_id=item_files.item_id)) ORDER BY id",
        (file_id,),
    )
    if not associations:
        return
    now = occurrence(store, profile, file_id)
    if historical:
        jobs = store.rows(
            "SELECT * FROM processing_jobs WHERE profile=? AND file_id=? AND operation='probe' AND state='complete' ORDER BY created_at,id",
            (profile, file_id),
        )
    else:
        jobs = []
    jobs += store.rows(
        "SELECT j.*,f.snapshot AS evidence_snapshot,f.data AS evidence_data FROM file_facts f JOIN processing_jobs j ON j.id=f.job_id WHERE f.profile=? AND f.file_id=? AND f.operation='probe' AND f.status='complete' AND j.state='complete'",
        (profile, file_id),
    )
    for j in jobs:
        if "evidence_snapshot" in j:
            j["snapshot"] = j["evidence_snapshot"]
    jobs = [
        j
        for j in jobs
        if historical or revision(json.loads(j["snapshot"])) == revision(now)
    ]
    for a in associations:
        item = store.rows("SELECT metadata FROM items WHERE id=?", (a["item_id"],))[0][
            "metadata"
        ]
        metadata = json.loads(a["metadata"])
        indexed = False
        for job in jobs:
            data = json.loads(job.get("evidence_data", job["result"]) or "{}")
            snapshot = json.loads(job["snapshot"])
            for key, stream in enumerate(data.get("streams", [])):
                kind = stream.get("codec_type")
                if (
                    kind not in ("video", "audio", "subtitle", "attachment")
                    or type(stream.get("index")) is not int
                ):
                    continue
                if a["role"] != "primary" and KINDS.get(a["role"]) != kind:
                    continue
                if a["role"] != "primary" and metadata.get("stream_index") not in (
                    None,
                    stream["index"],
                ):
                    continue
                _insert(
                    store,
                    profile,
                    a,
                    item,
                    snapshot,
                    kind,
                    {
                        "convention": "ffprobe_absolute_stream_index",
                        "index": stream["index"],
                    },
                    job["id"],
                    key,
                    stream,
                    "validation.output" if job["operation"] == "render" else "",
                )
                indexed = indexed or revision(snapshot) == revision(now)
        if not indexed and a["role"] in KINDS and now["status"] == "present":
            _insert(
                store,
                profile,
                a,
                item,
                now,
                KINDS[a["role"]],
                {"convention": "whole_file"},
                None,
                None,
            )


def _insert(
    store,
    profile,
    association,
    item,
    snapshot,
    kind,
    locator,
    job,
    key,
    stream=None,
    probe_path="",
):
    rev = revision(snapshot)
    identifier = str(
        uuid5(
            UUID(store.database_id),
            encode(
                [
                    "component_occurrence",
                    profile,
                    association["id"],
                    rev,
                    locator,
                    association["metadata"],
                    association["part"],
                    stream,
                ]
            ),
        )
    )
    logical = str(
        uuid5(
            UUID(store.database_id),
            encode(["component", profile, association["file_id"], rev, locator]),
        )
    )
    store.db.execute(
        "INSERT OR IGNORE INTO media_components VALUES (?,?)", (logical, kind)
    )
    store.db.execute(
        """INSERT INTO component_occurrences(id,component_id,profile,file_id,association_id,item_id,revision,snapshot,association_evidence,item_evidence,storage,locator,probe_job_id,stream_key,probe_path)
      VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO NOTHING""",
        (
            identifier,
            logical,
            profile,
            association["file_id"],
            association["id"],
            association["item_id"],
            rev,
            encode(snapshot),
            encode(association),
            item,
            "embedded" if association["role"] == "primary" else "external",
            encode(locator),
            job,
            key,
            probe_path,
        ),
    )


def backfill(db):
    """Schema-25 adoption of existing evidence; never probe or touch source files."""

    class Adapter:
        schema_version = 25

        def rows(self, sql, args=()):
            cursor = db.execute(sql, args)
            columns = [c[0] for c in cursor.description]
            return [dict(zip(columns, r, strict=True)) for r in cursor]

    store = Adapter()
    store.db = db
    rows = store.rows("SELECT value FROM meta WHERE key='database_id'")
    if not rows:
        return
    store.database_id = rows[0]["value"]
    generation = store.rows("SELECT generation FROM fallback_epoch WHERE id=1")[0][
        "generation"
    ]
    for row in store.rows(
        "SELECT DISTINCT o.profile,o.file_id FROM observations o JOIN item_files a ON a.file_id=o.file_id"
    ):
        index_file(store, row["profile"], row["file_id"], historical=True)
    # Adoption adds a new surface without rewriting existing migration data.
    db.execute("UPDATE fallback_epoch SET generation=? WHERE id=1", (generation,))


def get(store, profile, identifier, *, require_current=False, session=None):
    from .component_sql import OCCURRENCES_SQL

    rows = (session.rows if session else store.rows)(
        "SELECT * FROM (" + OCCURRENCES_SQL + ") WHERE profile=? AND occurrence_id=?",
        (profile, identifier),
    )
    if not rows:
        raise CatabolicError("unknown component occurrence in this profile")
    row = rows[0]
    for field in (
        "locator",
        "snapshot",
        "technical",
        "observed",
        "asserted",
        "compatibility",
        "dependencies",
        "provenance",
        "conflicts",
    ):
        row[field] = json.loads(row[field]) if row[field] is not None else None
    row["current"] = (
        bool(row["current"])
        and revision(occurrence(store, profile, row["file_id"])) == row["revision"]
    )
    if require_current and (not row["current"] or row["conflicts"]):
        raise CatabolicError("stale_or_conflicting_component_occurrence")
    return row
