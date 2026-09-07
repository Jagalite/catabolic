# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Catalog semantics shared by externally registered and generated renditions."""

import hashlib
import json
from uuid import uuid4

from .curation import bounded_rows, occurrence, page_limit
from .domain import CatabolicError, name
from .media import ROLES, media_kind, relationship, vocabulary
from .source_access import validated_source
from .store import encode

PURPOSES = ("transcode", "remux", "preview", "thumbnail", "audio", "subtitle", "custom")
PRESET_PURPOSE = {
    "h264-720p": "transcode",
    "h264-1080p": "transcode",
    "preview": "preview",
    "thumbnail": "thumbnail",
    "remux-mkv": "remux",
    "audio-aac": "audio",
    "audio-flac": "audio",
    "subtitle-srt": "subtitle",
}
# Historical definitions remain byte-for-byte intact. Only known producing presets
# supply a fallback purpose; codec or filename guesses are never provenance.
RENDITIONS_SQL = """SELECT m.*,coalesce(json_extract(d.definition,'$.purpose'),
 CASE r.preset WHEN 'h264-720p' THEN 'transcode' WHEN 'h264-1080p' THEN 'transcode'
 WHEN 'preview' THEN 'preview' WHEN 'thumbnail' THEN 'thumbnail'
 WHEN 'remux-mkv' THEN 'remux' WHEN 'audio-aac' THEN 'audio'
 WHEN 'audio-flac' THEN 'audio' WHEN 'subtitle-srt' THEN 'subtitle' END,'unknown') AS purpose,
 j.recipe_id, e.producer, e.instance AS producer_instance
 FROM main.media_outputs m JOIN main.output_definitions d ON d.id=m.definition_id
 LEFT JOIN main.processing_artifacts a ON a.id=m.artifact_id
 LEFT JOIN main.processing_jobs j ON j.id=a.job_id
 LEFT JOIN main.processing_recipes r ON r.id=j.recipe_id
 LEFT JOIN main.receipt_outputs ro ON ro.output_id=m.id
 LEFT JOIN main.external_receipts e ON e.id=ro.receipt_id"""


def definition(value):
    if not isinstance(value, dict) or set(value) - {
        "version",
        "mode",
        "role",
        "item_kind",
        "relationship",
        "item_metadata",
        "file_metadata",
        "purpose",
    }:
        raise CatabolicError("invalid output definition fields")
    result = {
        "version": 2 if "purpose" in value else 1,
        "mode": "same_item",
        "role": "primary",
        "file_metadata": {},
        **value,
    }
    if type(result["version"]) is not int or result["version"] not in (1, 2):
        raise CatabolicError("unsupported output definition version")
    if result["version"] == 2:
        if result.get("purpose") not in PURPOSES:
            raise CatabolicError("version 2 output definitions require a valid purpose")
    elif "purpose" in result:
        raise CatabolicError("purpose requires output definition version 2")
    vocabulary(result["role"], ROLES, "file role")
    if result["mode"] not in ("same_item", "new_item"):
        raise CatabolicError("output mode must be same_item or new_item")
    if result["mode"] == "same_item":
        if set(result) & {"item_kind", "relationship", "item_metadata"}:
            raise CatabolicError(
                "same_item outputs cannot replace item metadata or relationships"
            )
    else:
        media_kind(result.get("item_kind"))
        result.setdefault("relationship", "derived_from")
        if result["relationship"] not in ("derived_from", "edition_of"):
            raise CatabolicError("new_item outputs require derived_from or edition_of")
        result.setdefault("item_metadata", {})
    for key in ("file_metadata", "item_metadata"):
        if key in result and not isinstance(result[key], dict):
            raise CatabolicError(f"{key} must be a JSON object")
    try:
        serialized = json.dumps(result, allow_nan=False)
    except (TypeError, ValueError, RecursionError) as exc:
        raise CatabolicError(
            "output definition must contain finite JSON values"
        ) from exc
    if len(serialized.encode()) > 65536:
        raise CatabolicError("output definition exceeds 64 KiB")
    # Detach caller-owned mutable metadata.
    return json.loads(serialized)


class Outputs:
    def __init__(self, app):
        self.app, self.store, self.profile = app, app.store, app.profile

    @staticmethod
    def decode(row):
        row = dict(row)
        for key in ("definition", "metadata"):
            if key in row:
                row[key] = json.loads(row[key])
        return row

    def define(self, output_name, value):
        name(output_name)
        serialized = encode(definition(value))
        digest = hashlib.sha256(serialized.encode()).hexdigest()
        with self.store.transaction() as db:
            old = db.execute(
                "SELECT id FROM output_definitions WHERE name=? AND digest=?",
                (output_name, digest),
            ).fetchone()
            if old:
                identifier = old[0]
            else:
                identifier = str(uuid4())
                revision = db.execute(
                    "SELECT coalesce(max(revision),0)+1 FROM output_definitions WHERE name=?",
                    (output_name,),
                ).fetchone()[0]
                db.execute(
                    "INSERT INTO output_definitions(id,name,revision,definition,digest) VALUES (?,?,?,?,?)",
                    (identifier, output_name, revision, serialized, digest),
                )
        return self.get_definition(identifier)

    def get_definition(self, identifier):
        rows = self.store.rows(
            "SELECT * FROM output_definitions WHERE id=?", (identifier,)
        )
        if not rows:
            raise CatabolicError(
                "unknown output definition; use an immutable definition ID"
            )
        return self.decode(rows[0])

    def get(self, identifier):
        rows = self.store.rows(
            f"SELECT * FROM ({RENDITIONS_SQL}) WHERE profile=? AND id=?",
            (self.profile, identifier),
        )
        if not rows:
            raise CatabolicError("unknown media output")
        return self.decode(rows[0])

    def list(self, *, definitions=False, limit=100, after="", source_file_id=None):
        page_limit(limit)
        if definitions:
            key, sql, values = (
                "definitions",
                "SELECT * FROM output_definitions WHERE id>? ORDER BY id LIMIT ?",
                (after, limit + 1),
            )
        else:
            key, sql, values = (
                "outputs",
                f"SELECT * FROM ({RENDITIONS_SQL}) WHERE profile=? AND id>? AND (? IS NULL OR source_file_id=?) ORDER BY id LIMIT ?",
                (self.profile, after, source_file_id, source_file_id, limit + 1),
            )
        rows, more = bounded_rows(self.store, sql, values, limit)
        return {
            key: [self.decode(r) for r in rows],
            "next_after": rows[-1]["id"] if more else None,
        }

    def validate_source_item(self, file_id, item_id, value):
        if not self.store.rows(
            "SELECT 1 FROM item_files WHERE file_id=? AND item_id=? AND active=1",
            (file_id, item_id),
        ):
            raise CatabolicError(
                "input must have an active association with the requested item"
            )
        if value["mode"] == "new_item":
            kind = self.store.rows("SELECT kind FROM items WHERE id=?", (item_id,))[0][
                "kind"
            ]
            relationship(value["relationship"], value["item_kind"], kind, None)

    def record(
        self,
        db,
        *,
        file_id,
        source_file_id,
        source_item_id,
        definition_id,
        artifact_id=None,
        metadata=None,
    ):
        """Called inside the publication/registration transaction. Never writes media."""
        # File identity and lineage are shared across profiles. Enforce this at
        # the common write boundary, including generated and receipted outputs.
        if db.execute(
            """WITH RECURSIVE ancestors(id) AS (
                VALUES (?) UNION SELECT m.source_file_id FROM media_outputs m
                JOIN ancestors a ON m.file_id=a.id
            ) SELECT 1 FROM ancestors WHERE id=?""",
            (source_file_id, file_id),
        ).fetchone():
            raise CatabolicError("output lineage would create a cycle")
        value = self.get_definition(definition_id)["definition"]
        identifier = str(uuid4())
        item_id = source_item_id
        if value["mode"] == "new_item":
            item_id = str(uuid4())
            # New identity, explicit metadata only: provider IDs and curated source metadata stay put.
            self.app.put_item(
                value["item_kind"], {}, value["item_metadata"], item_id, _db=db
            )
            self.app.media.relate(
                item_id, source_item_id, value["relationship"], _db=db
            )
        payload = {**value["file_metadata"], **(metadata or {})}
        association = db.execute(
            "SELECT id FROM item_files WHERE file_id=? AND item_id=? AND role=?",
            (file_id, item_id, value["role"]),
        ).fetchone()
        if not association:
            db.execute(
                "INSERT INTO item_files(id,file_id,item_id,role,metadata,origin,active) VALUES (?,?,?,?,?,'explicit',0)",
                (str(uuid4()), file_id, item_id, value["role"], encode(payload)),
            )
        # Existing associations and their active state/metadata remain user-controlled.
        db.execute(
            "INSERT INTO media_outputs(id,profile,file_id,source_file_id,source_item_id,item_id,definition_id,artifact_id,origin,metadata) VALUES (?,?,?,?,?,?,?,?,?,?)",
            (
                identifier,
                self.profile,
                file_id,
                source_file_id,
                source_item_id,
                item_id,
                definition_id,
                artifact_id,
                "generated" if artifact_id else "registered",
                encode(payload),
            ),
        )
        return {"output_id": identifier, "item_id": item_id}

    def register(self, file_id, source_file_id, item_id, definition_id, metadata=None):
        self.app.require_recovered()
        value = self.get_definition(definition_id)["definition"]
        if not isinstance(metadata if metadata is not None else {}, dict):
            raise CatabolicError("output metadata must be a JSON object")
        payload = definition(
            {"file_metadata": {**value["file_metadata"], **(metadata or {})}}
        )["file_metadata"]
        self.validate_source_item(source_file_id, item_id, value)
        old = self.store.rows(
            "SELECT * FROM media_outputs WHERE profile=? AND file_id=?",
            (self.profile, file_id),
        )
        if old:
            row = self.decode(old[0])
            if (
                row["origin"],
                row["source_file_id"],
                row["source_item_id"],
                row["definition_id"],
                row["metadata"],
            ) == ("registered", source_file_id, item_id, definition_id, payload):
                return row
            raise CatabolicError("file already has a different output registration")
        if self.store.rows(
            "SELECT 1 FROM processing_artifacts WHERE file_id=?", (file_id,)
        ):
            raise CatabolicError(
                "generated files keep their recorded processing provenance"
            )
        # Presence checks are not a claim that Catabolic performed or verified the conversion.
        for key in (source_file_id, file_id):
            with validated_source(occurrence(self.store, self.profile, key)):
                pass
        with self.store.transaction() as db:
            result = self.record(
                db,
                file_id=file_id,
                source_file_id=source_file_id,
                source_item_id=item_id,
                definition_id=definition_id,
                metadata=metadata,
            )
        return self.get(result["output_id"])
