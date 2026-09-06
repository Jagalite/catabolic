# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Persisted, reviewable identification and association decisions."""

import json
from pathlib import PurePosixPath
from uuid import UUID, uuid4, uuid5

from .domain import CatabolicError
from .media import ROLES, media_kind, vocabulary
from .store import encode


def bounded_text(value, label, maximum=4096):
    if not isinstance(value, str) or not value.strip() or len(value.encode()) > maximum:
        raise CatabolicError(
            f"{label} must be nonempty text of at most {maximum} bytes"
        )
    return value


def page_limit(limit):
    if type(limit) is not int or not 1 <= limit <= 1000:
        raise CatabolicError("limit must be between 1 and 1000")
    return limit


def payload_object(value):
    if not isinstance(value, dict) or len(encode(value).encode()) > 256 * 1024:
        raise CatabolicError("payload must be a JSON object of at most 256 KiB")
    return value


def bounded_rows(store, sql, args, limit):
    rows = []
    used = 0
    more = False
    cursor = store.db.execute(sql, args)
    try:
        for raw in cursor:
            row = dict(raw)
            size = len(encode(row).encode())
            if len(rows) == limit or used + size > 4 * 1024 * 1024:
                if not rows:
                    raise CatabolicError("record exceeds the result budget")
                more = True
                break
            rows.append(row)
            used += size
    finally:
        cursor.close()
    return rows, more


def occurrence(store, profile, file_id):
    rows = store.rows(
        """SELECT f.id,f.location,f.path,o.size,o.mtime_ns,o.device,o.inode,o.status,
        b.root,b.device AS root_device,b.inode AS root_inode
        FROM files f LEFT JOIN observations o ON o.file_id=f.id AND o.profile=?
        LEFT JOIN bindings b ON b.profile=? AND b.kind='source' AND b.owner=f.location
        WHERE f.id=?""",
        (profile, profile, file_id),
    )
    if not rows:
        raise CatabolicError(f"unknown file: {file_id}")
    return rows[0]


class Curation:
    def __init__(self, app):
        self.app, self.store, self.profile = app, app.store, app.profile

    def _item_id(self, payload):
        item = payload.get("item")
        if not isinstance(item, dict):
            raise CatabolicError("identify payload requires an item object")
        if set(item) - {"id", "kind", "identities", "metadata"}:
            raise CatabolicError("item supports id, kind, identities, metadata")
        bounded_text(item.get("kind"), "media kind", 128)
        media_kind(item["kind"])
        identities = item.get("identities", {})
        if not isinstance(identities, dict):
            raise CatabolicError("identities must be an object")
        payload_object(item.get("metadata", {}))
        matches = set()
        for namespace, value in identities.items():
            bounded_text(namespace, "identity namespace", 128)
            bounded_text(value, "identity value")
            matches.update(
                r["item_id"]
                for r in self.store.rows(
                    "SELECT item_id FROM identities WHERE namespace=? AND value=?",
                    (namespace, value),
                )
            )
        if len(matches) > 1 or item.get("id") and matches and item["id"] not in matches:
            raise CatabolicError("supplied identities refer to different media items")
        if item.get("id") is not None:
            bounded_text(item["id"], "item id", 256)
        if not identities and not item.get("id"):
            raise CatabolicError("supply an identity or explicit item id")
        return item.get("id") or next(iter(matches), None)

    def snapshot(self, file_id, payload):
        target = self._item_id(payload) if "item" in payload else payload.get("item_id")
        total = self.store.db.execute(
            "SELECT coalesce(sum(length(metadata)),0) FROM item_files WHERE file_id=?",
            (file_id,),
        ).fetchone()[0]
        if total > 256 * 1024:
            raise CatabolicError(
                "association metadata exceeds the proposal snapshot budget"
            )
        associations = self.store.rows(
            "SELECT * FROM item_files WHERE file_id=? ORDER BY id LIMIT 1001",
            (file_id,),
        )
        if len(associations) > 1000:
            raise CatabolicError("proposal association snapshot exceeds 1000 records")
        return {
            "file": occurrence(self.store, self.profile, file_id),
            "associations": associations,
            "item": self.store.rows("SELECT * FROM items WHERE id=?", (target,)),
            "identities": self.store.rows(
                "SELECT * FROM identities WHERE item_id=? ORDER BY namespace,value",
                (target,),
            ),
        }

    def put(self, file_id, payload, *, source="manual", evidence=None):
        self.app.require_recovered()
        payload_object(payload)
        if set(payload) - {
            "item",
            "item_id",
            "role",
            "part",
            "metadata",
            "overwrite_fields",
        }:
            raise CatabolicError(
                "proposal supports item or item_id, role, part, metadata, overwrite_fields"
            )
        if ("item" in payload) == ("item_id" in payload):
            raise CatabolicError("supply exactly one item or item_id")
        vocabulary(payload.get("role", "primary"), ROLES, "file role")
        if "metadata" in payload:
            payload_object(payload["metadata"])
        fields = payload.get("overwrite_fields", [])
        if (
            not isinstance(fields, list)
            or len(fields) > 100
            or any(not isinstance(v, str) for v in fields)
        ):
            raise CatabolicError("overwrite_fields must be a list of field names")
        bounded_text(source, "source")
        if evidence is None:
            evidence = {}
        payload_object(evidence)
        snapshot = self.snapshot(file_id, payload)
        payload_object(snapshot)
        if "item_id" in payload and not snapshot["item"]:
            raise CatabolicError("unknown target item")
        identifier = str(
            uuid5(
                UUID(self.store.database_id),
                encode([self.profile, file_id, payload, source, evidence, snapshot]),
            )
        )
        with self.store.transaction() as db:
            db.execute(
                """INSERT INTO proposals(id,profile,file_id,kind,payload,evidence,source,snapshot)
                VALUES (?,?,?,?,?,?,?,?) ON CONFLICT(id) DO NOTHING""",
                (
                    identifier,
                    self.profile,
                    file_id,
                    "identify" if "item" in payload else "associate",
                    encode(payload),
                    encode(evidence),
                    source,
                    encode(snapshot),
                ),
            )
        return self.get(identifier)

    def get(self, identifier):
        rows = self.store.rows(
            "SELECT * FROM proposals WHERE id=? AND profile=?",
            (identifier, self.profile),
        )
        if not rows:
            raise CatabolicError("unknown proposal in this profile")
        row = rows[0]
        for key in ("payload", "evidence", "snapshot", "result"):
            row[key] = json.loads(row[key]) if row[key] is not None else None
        return row

    def list(self, *, state=None, limit=100, after=""):
        page_limit(limit)
        if state is not None and state not in ("pending", "accepted", "rejected"):
            raise CatabolicError("invalid proposal state")
        rows, more = bounded_rows(
            self.store,
            "SELECT id,profile,file_id,kind,state,source,created_at FROM proposals WHERE profile=? AND id>? AND (? IS NULL OR state=?) ORDER BY id LIMIT ?",
            (self.profile, after or "", state, state, limit + 1),
            limit,
        )
        return {"proposals": rows, "next_after": rows[-1]["id"] if more else None}

    def decide(self, identifier, *, accept, actor):
        self.app.require_recovered()
        bounded_text(actor, "decision actor")
        proposal = self.get(identifier)
        desired = "accepted" if accept else "rejected"
        if proposal["state"] == desired:
            return proposal
        if proposal["state"] != "pending":
            raise CatabolicError(
                "proposal already decided; submit a new proposal for a changed decision"
            )
        payload = proposal["payload"]
        before = self.snapshot(proposal["file_id"], payload)
        if accept and before != proposal["snapshot"]:
            raise CatabolicError(
                "proposal is stale; inventory or curation changed; submit a new proposal"
            )
        if accept:
            from .source_access import validated_source

            with validated_source(before["file"]):
                pass
        with self.store.transaction() as db:
            result = {}
            if accept:
                if "item" in payload:
                    item = payload["item"]
                    item_id = self._item_id(payload) or str(uuid4())
                    existing = before["item"]
                    metadata = json.loads(existing[0]["metadata"]) if existing else {}
                    proposed = item.get("metadata", {})
                    preserved = []
                    for key, value in proposed.items():
                        if key not in metadata or key in payload.get(
                            "overwrite_fields", []
                        ):
                            metadata[key] = value
                        elif metadata[key] != value:
                            preserved.append(key)
                    self.app.put_item(
                        item["kind"],
                        item.get("identities", {}),
                        metadata,
                        item_id,
                        _db=db,
                    )
                    result["preserved_fields"] = preserved
                else:
                    item_id = payload["item_id"]
                association = self.app.media.associate_in_transaction(
                    db,
                    proposal["file_id"],
                    item_id,
                    role=payload.get("role", "primary"),
                    part=payload.get("part"),
                    metadata=payload.get("metadata"),
                    origin="explicit",
                )
                result.update(item_id=item_id, association_id=association)
            db.execute(
                "UPDATE proposals SET state=?,result=? WHERE id=?",
                (desired, encode(result), identifier),
            )
            db.execute(
                "INSERT INTO decision_events(proposal_id,action,actor,before_value,after_value) VALUES (?,?,?,?,?)",
                (
                    identifier,
                    desired,
                    actor,
                    encode(before),
                    encode(self.snapshot(proposal["file_id"], payload)),
                ),
            )
        return self.get(identifier)

    def history(self, *, limit=100, after=0):
        page_limit(limit)
        rows = self.store.rows(
            """SELECT e.* FROM decision_events e JOIN proposals p ON p.id=e.proposal_id
            WHERE p.profile=? AND e.id>? ORDER BY e.id LIMIT ?""",
            (self.profile, after, limit + 1),
        )
        more = len(rows) > limit
        rows = rows[:limit]
        for row in rows:
            for key in ("before_value", "after_value"):
                row[key] = json.loads(row[key])
        return {"events": rows, "next_after": rows[-1]["id"] if more else None}

    def sidecars(self, *, location=None, limit=100, after="", apply=False):
        """Exact stem and conventional artwork candidates; ambiguities remain proposals."""
        page_limit(limit)
        rows = self.store.rows(
            """SELECT f.* FROM files f JOIN observations o ON o.file_id=f.id AND o.profile=?
            WHERE o.status='present' AND f.id>? AND (? IS NULL OR f.location=?) ORDER BY f.id LIMIT ?""",
            (self.profile, after or "", location, location, limit + 1),
        )
        more = len(rows) > limit
        rows = rows[:limit]
        candidates = []
        for row in rows:
            path = PurePosixPath(row["path"])
            suffix = path.suffix.lower()
            if suffix in (".srt", ".vtt", ".ass", ".ssa"):
                role = "subtitle"
            elif suffix in (".jpg", ".jpeg", ".png", ".webp"):
                role = "cover"
            else:
                continue
            parent = str(path.parent)
            pattern = parent + "/" if parent != "." else ""
            primary = self.store.rows(
                """SELECT DISTINCT a.item_id,f.id,f.path FROM item_files a JOIN files f ON f.id=a.file_id
                JOIN observations o ON o.file_id=f.id AND o.profile=? WHERE f.location=? AND a.active=1 AND a.role='primary'
                AND o.status='present' AND f.path>=? AND f.path<? AND instr(substr(f.path,length(?)+1),'/')=0 AND f.id!=? LIMIT 1001""",
                (
                    self.profile,
                    row["location"],
                    pattern,
                    pattern + chr(0x10FFFF),
                    pattern,
                    row["id"],
                ),
            )
            if len(primary) > 1000:
                raise CatabolicError(
                    "sidecar candidate scope exceeds 1000 primary files; use a smaller source scope"
                )
            matches = []
            for other in primary:
                p = PurePosixPath(other["path"])
                if p.parent != path.parent:
                    continue
                stem = path.stem.casefold()
                base = p.stem.casefold()
                if (
                    stem == base
                    or stem.startswith(base + ".")
                    or role == "cover"
                    and stem in ("cover", "folder", "poster")
                ):
                    matches.append(other)
            ids = sorted({m["item_id"] for m in matches})
            tags = path.stem.split(".")[1:]
            for item_id in ids:
                metadata = {
                    "forced": any(t.lower() == "forced" for t in tags),
                    "sdh": any(t.lower() in ("sdh", "hi", "cc") for t in tags),
                }
                for other in matches:
                    if other["item_id"] == item_id:
                        tail = (
                            path.stem[len(PurePosixPath(other["path"]).stem) :]
                            .strip(".")
                            .split(".")
                        )
                        languages = [
                            t.lower()
                            for t in tail
                            if t.isalpha()
                            and len(t) in (2, 3)
                            and t.lower() not in ("sdh", "hi", "cc")
                        ]
                        if len(languages) == 1:
                            metadata["language"] = languages[0]
                metadata["primary_file_ids"] = sorted(
                    {m["id"] for m in matches if m["item_id"] == item_id}
                )
                payload = {"item_id": item_id, "role": role, "metadata": metadata}
                evidence = {
                    "method": "same-directory-stem-v1",
                    "path": row["path"],
                    "ambiguous": len(ids) > 1,
                    "candidate_items": ids,
                }
                value = {"file_id": row["id"], "payload": payload, "evidence": evidence}
                if apply:
                    value["proposal"] = self.put(
                        row["id"],
                        payload,
                        source="sidecar-discovery",
                        evidence=evidence,
                    )["id"]
                candidates.append(value)
        return {
            "candidates": candidates,
            "complete": True,
            "next_after": rows[-1]["id"] if more else None,
        }

    def expected_put(self, collection, definition):
        payload_object(definition)
        if set(definition) - {"source", "edition", "ordering", "complete", "members"}:
            raise CatabolicError(
                "expected set supports source, edition, ordering, complete, members"
            )
        if not self.store.rows("SELECT id FROM items WHERE id=?", (collection,)):
            raise CatabolicError("unknown collection")
        for field in ("source", "edition", "ordering"):
            bounded_text(definition.get(field), field)
        if type(definition.get("complete")) is not bool:
            raise CatabolicError("expected set needs explicit complete true or false")
        members = definition.get("members")
        if not isinstance(members, list) or len(members) > 10000:
            raise CatabolicError("members must be a list of at most 10000 entries")
        keys = set()
        for member in members:
            if not isinstance(member, dict) or set(member) - {
                "key",
                "title",
                "item_id",
                "identity",
            }:
                raise CatabolicError("member supports key, title, item_id or identity")
            key = bounded_text(member.get("key"), "member key")
            if key in keys:
                raise CatabolicError("duplicate member key")
            keys.add(key)
            if ("item_id" in member) == ("identity" in member):
                raise CatabolicError("each member requires item_id or identity")
            if "item_id" in member:
                bounded_text(member["item_id"], "member item id", 256)
            if "title" in member:
                bounded_text(member["title"], "member title")
            if "identity" in member and (
                not isinstance(member["identity"], dict) or len(member["identity"]) != 1
            ):
                raise CatabolicError(
                    "member identity requires one namespace/value pair"
                )
        for member in members:
            for namespace, value in member.get("identity", {}).items():
                bounded_text(namespace, "member identity namespace", 128)
                bounded_text(value, "member identity value")
        identifier = str(
            uuid5(UUID(self.store.database_id), encode([collection, definition]))
        )
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO expected_sets(id,collection_id,source,edition,ordering,complete,members) VALUES (?,?,?,?,?,?,?) ON CONFLICT(id) DO NOTHING",
                (
                    identifier,
                    collection,
                    definition["source"],
                    definition["edition"],
                    definition["ordering"],
                    int(definition["complete"]),
                    encode(members),
                ),
            )
        return {"id": identifier, **definition, "collection_id": collection}

    def completeness(self, identifier, *, limit=100, after=0):
        page_limit(limit)
        if type(after) is not int or after < 0:
            raise CatabolicError("after must be a nonnegative member offset")
        rows = self.store.rows("SELECT * FROM expected_sets WHERE id=?", (identifier,))
        if not rows:
            raise CatabolicError("unknown expected set; supply an explicit snapshot id")
        value = rows[0]
        members = json.loads(value.pop("members"))
        results = []
        for member in members[after : after + limit]:
            ids = (
                [member["item_id"]]
                if "item_id" in member
                else [
                    r["item_id"]
                    for ns, val in member["identity"].items()
                    for r in self.store.rows(
                        "SELECT item_id FROM identities WHERE namespace=? AND value=?",
                        (ns, val),
                    )
                ]
            )
            states = []
            for item_id in ids:
                states.extend(
                    r["status"]
                    for r in self.store.rows(
                        """SELECT coalesce(o.status,'unknown') AS status FROM item_files a
                    LEFT JOIN observations o ON o.file_id=a.file_id AND o.profile=?
                    WHERE a.item_id=? AND a.active=1 AND a.role='primary' """,
                        (self.profile, item_id),
                    )
                )
            status = (
                "present"
                if "present" in states
                else "unavailable"
                if states and all(s == "missing" for s in states)
                else "unknown"
                if states
                else "unidentified"
                if ids
                and any(
                    self.store.rows("SELECT id FROM items WHERE id=?", (i,))
                    for i in ids
                )
                else "missing"
            )
            results.append({**member, "status": status})
        return {
            "expected_set": value,
            "members": results,
            "expectations_complete": bool(value["complete"]),
            "next_after": after + limit if after + limit < len(members) else None,
            "availability": "recorded observations; not a live source check",
        }
