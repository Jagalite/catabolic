# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Identification and media relationships independent of catalog placement."""

import json
from contextlib import nullcontext
from uuid import UUID, uuid5

from .domain import CatabolicError
from .media import ROLES, STRUCTURAL, ordinal, relationship, vocabulary
from .store import encode


class MediaCatalog:
    def __init__(self, application):
        self.app = application
        self.store = application.store

    def _id(self, *parts):
        return str(uuid5(UUID(self.store.database_id), encode(parts)))

    def associate_in_transaction(
        self,
        db,
        file_id,
        item_id,
        *,
        role="primary",
        part=None,
        clear_part=False,
        metadata=None,
        origin="explicit",
    ):
        vocabulary(role, ROLES, "file role")
        ordinal(part, "part")
        if clear_part and part is not None:
            raise CatabolicError("part and clear-part cannot be combined")
        if metadata is not None and not isinstance(metadata, dict):
            raise CatabolicError("association metadata must be a JSON object")
        for table, key in (("files", file_id), ("items", item_id)):
            if not db.execute(f"SELECT 1 FROM {table} WHERE id=?", (key,)).fetchone():
                raise CatabolicError(f"unknown {table} ID: {key}")
        existing = db.execute(
            "SELECT * FROM item_files WHERE file_id=? AND item_id=? AND role=?",
            (file_id, item_id, role),
        ).fetchone()
        if part is None and existing and not clear_part:
            part = existing["part"]
        identifier = (
            existing["id"]
            if existing
            else self._id("association", file_id, item_id, role)
        )
        if (
            part is not None
            and db.execute(
                "SELECT 1 FROM item_files WHERE item_id=? AND role=? AND part=? AND active=1 AND id!=?",
                (item_id, role, part, identifier),
            ).fetchone()
        ):
            raise CatabolicError(
                "that item/role already has a file at this part number"
            )
        payload = (
            encode(metadata)
            if metadata is not None
            else existing["metadata"]
            if existing
            else "{}"
        )
        db.execute(
            """INSERT INTO item_files(id,file_id,item_id,role,part,metadata,origin,active)
            VALUES (?,?,?,?,?,?,?,1) ON CONFLICT(id) DO UPDATE SET
            part=excluded.part,metadata=excluded.metadata,origin=excluded.origin,active=1""",
            (identifier, file_id, item_id, role, part, payload, origin),
        )
        return identifier

    def associate(
        self,
        file_id,
        item_id,
        *,
        role="primary",
        part=None,
        clear_part=False,
        metadata=None,
    ):
        self.app.require_recovered()
        with self.store.transaction() as db:
            identifier = self.associate_in_transaction(
                db,
                file_id,
                item_id,
                role=role,
                part=part,
                clear_part=clear_part,
                metadata=metadata,
            )
        result = self.store.rows("SELECT * FROM item_files WHERE id=?", (identifier,))[
            0
        ]
        result["metadata"] = json.loads(result["metadata"])
        return result

    def disable_association(self, identifier):
        self.app.require_recovered()
        with self.store.transaction() as db:
            row = db.execute(
                "SELECT * FROM item_files WHERE id=?", (identifier,)
            ).fetchone()
            if row is None:
                raise CatabolicError(f"unknown association: {identifier}")
            if (
                row["active"]
                and db.execute(
                    "SELECT 1 FROM mappings WHERE file_id=? AND item_id=? AND active=1",
                    (row["file_id"], row["item_id"]),
                ).fetchone()
                and not db.execute(
                    "SELECT 1 FROM item_files WHERE file_id=? AND item_id=? AND active=1 AND id!=?",
                    (row["file_id"], row["item_id"], identifier),
                ).fetchone()
            ):
                raise CatabolicError(
                    "disable active catalog mappings before removing their last identification"
                )
            db.execute("UPDATE item_files SET active=0 WHERE id=?", (identifier,))
        return {"id": identifier, "active": False}

    def relate(
        self,
        source,
        target,
        kind,
        *,
        position=None,
        clear_position=False,
        metadata=None,
        _db=None,
    ):
        self.app.require_recovered()
        if source == target:
            raise CatabolicError("an item cannot relate to itself")
        if metadata is not None and not isinstance(metadata, dict):
            raise CatabolicError("relationship metadata must be a JSON object")
        identifier = self._id("relationship", source, target, kind)
        if clear_position and position is not None:
            raise CatabolicError("position and clear-position cannot be combined")
        with nullcontext(_db) if _db is not None else self.store.transaction() as db:
            existing = db.execute(
                "SELECT metadata,position FROM item_relationships WHERE id=?",
                (identifier,),
            ).fetchone()
            if position is None and existing and not clear_position:
                position = existing["position"]
            types = {}
            for key in (source, target):
                row = db.execute("SELECT kind FROM items WHERE id=?", (key,)).fetchone()
                if row is None:
                    raise CatabolicError(f"unknown item: {key}")
                types[key] = row[0]
            relationship(kind, types[source], types[target], position)
            if (
                kind in STRUCTURAL
                and db.execute(
                    """WITH RECURSIVE ancestors(id) AS (
                VALUES (?) UNION SELECT r.target_id FROM item_relationships r
                JOIN ancestors a ON r.source_id=a.id WHERE r.active=1
                AND r.kind IN ('part_of','edition_of','derived_from','recording_of')
            ) SELECT 1 FROM ancestors WHERE id=?""",
                    (target, source),
                ).fetchone()
            ):
                raise CatabolicError("relationship would create a structural cycle")
            if (
                position is not None
                and db.execute(
                    "SELECT 1 FROM item_relationships WHERE target_id=? AND kind=? AND position=? AND active=1 AND id!=?",
                    (target, kind, position, identifier),
                ).fetchone()
            ):
                raise CatabolicError("that parent already has a child at this position")
            payload = (
                encode(metadata)
                if metadata is not None
                else existing[0]
                if existing
                else "{}"
            )
            db.execute(
                """INSERT INTO item_relationships(id,source_id,target_id,kind,position,metadata,active)
                VALUES (?,?,?,?,?,?,1) ON CONFLICT(id) DO UPDATE SET
                position=excluded.position,metadata=excluded.metadata,active=1""",
                (identifier, source, target, kind, position, payload),
            )
        result = self.store.rows(
            "SELECT * FROM item_relationships WHERE id=?", (identifier,)
        )[0]
        result["metadata"] = json.loads(result["metadata"])
        return result

    def disable_relationship(self, identifier):
        self.app.require_recovered()
        with self.store.transaction() as db:
            if not db.execute(
                "SELECT 1 FROM item_relationships WHERE id=?", (identifier,)
            ).fetchone():
                raise CatabolicError(f"unknown relationship: {identifier}")
            db.execute(
                "UPDATE item_relationships SET active=0 WHERE id=?", (identifier,)
            )
        return {"id": identifier, "active": False}
