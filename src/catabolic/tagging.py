"""Catalog-wide vocabulary and explicit, independently attributed tag assertions."""

import math
import re
import unicodedata
from datetime import datetime, timezone
from uuid import uuid4

from .domain import CatabolicError


def text_value(value, label, maximum, *, empty=False):
    if (
        not isinstance(value, str)
        or (not empty and not value.strip())
        or len(value) > maximum
        or any(unicodedata.category(c).startswith("C") for c in value)
    ):
        raise CatabolicError(
            f"{label} must be text without control characters, at most {maximum} characters"
        )
    return value


def tag_name(value):
    text_value(value, "tag name", 255)
    value = unicodedata.normalize("NFC", value.strip().casefold())
    namespace, separator, label = value.partition(":")
    if separator:
        namespace, label = namespace.strip(), label.strip()
        if not re.fullmatch(r"[a-z0-9][a-z0-9_.-]*", namespace) or not label:
            raise CatabolicError(
                "namespaced tags require namespace:value; namespace uses letters, digits, dots, underscores or hyphens"
            )
        value = namespace + ":" + label
    return text_value(value, "tag name", 255)


def resolve_tag(db, value):
    row = db.execute(
        "SELECT tag_id FROM tag_names WHERE name=?", (tag_name(value),)
    ).fetchone()
    if row is None:
        raise CatabolicError(f"unknown tag: {value}; create it with tag put")
    return row[0]


def tag_filters(db, subject, *, tags=(), any_tags=(), not_tags=(), descendants=False):
    """Return parameterized predicates; hierarchy expansion starts at selected tags."""
    clauses, values = [], []
    groups = [list(group or ()) for group in (tags, any_tags, not_tags)]
    if sum(map(len, groups)) > 100:
        raise CatabolicError("at most 100 tag filters are allowed")
    table, alias = ("item_tags", "i") if subject == "item" else ("file_tags", "f")

    def predicate(names, negate=False):
        ids = sorted({resolve_tag(db, name) for name in names})
        marks = ",".join("?" for _ in ids)
        selected = f"SELECT id FROM tags WHERE id IN ({marks})"
        if descendants:
            selected = f"""WITH RECURSIVE selected(id) AS (
                {selected} UNION SELECT p.child_id FROM tag_parents p
                JOIN selected s ON s.id=p.parent_id) SELECT id FROM selected"""
        clauses.append(
            f"{alias}.id {'NOT ' if negate else ''}IN (SELECT a.{subject}_id FROM {table} a WHERE a.active=1 AND a.tag_id IN ({selected}))"
        )
        values.extend(ids)

    for name in groups[0]:
        predicate([name])
    if groups[1]:
        predicate(groups[1])
    if groups[2]:
        predicate(groups[2], True)
    return clauses, values


def reserve_name(db, name, identifier):
    if db.execute("SELECT 1 FROM tag_names WHERE name=?", (name,)).fetchone():
        return
    if (
        db.execute(
            "SELECT count(*) FROM tag_names WHERE tag_id=?", (identifier,)
        ).fetchone()[0]
        >= 101
    ):
        raise CatabolicError("a tag may have at most 100 aliases")
    db.execute("INSERT INTO tag_names VALUES (?,?)", (name, identifier))


class Tagging:
    def __init__(self, app):
        self.app, self.store = app, app.store

    def put(self, name, *, description=None):
        name = tag_name(name)
        if description is not None:
            text_value(description, "description", 4000, empty=True)
        self.app.require_recovered()
        with self.store.transaction() as db:
            row = db.execute(
                "SELECT tag_id FROM tag_names WHERE name=?", (name,)
            ).fetchone()
            identifier = row[0] if row else str(uuid4())
            if row is None:
                db.execute(
                    "INSERT INTO tags VALUES (?,?,?)",
                    (identifier, name, description or ""),
                )
                db.execute("INSERT INTO tag_names VALUES (?,?)", (name, identifier))
            elif description is not None:
                db.execute(
                    "UPDATE tags SET description=? WHERE id=?",
                    (description, identifier),
                )
        return self.store.rows("SELECT * FROM tags WHERE id=?", (identifier,))[0]

    def rename(self, name, new_name):
        new_name = tag_name(new_name)
        self.app.require_recovered()
        with self.store.transaction() as db:
            identifier = resolve_tag(db, name)
            existing = db.execute(
                "SELECT tag_id FROM tag_names WHERE name=?", (new_name,)
            ).fetchone()
            if existing and existing[0] != identifier:
                raise CatabolicError("that name belongs to another tag")
            reserve_name(db, new_name, identifier)
            db.execute("UPDATE tags SET name=? WHERE id=?", (new_name, identifier))
        return {"id": identifier, "name": new_name}

    def alias(self, name, alias, *, remove=False):
        alias = tag_name(alias)
        self.app.require_recovered()
        with self.store.transaction() as db:
            identifier = resolve_tag(db, name)
            existing = db.execute(
                "SELECT tag_id FROM tag_names WHERE name=?", (alias,)
            ).fetchone()
            if existing and existing[0] != identifier:
                raise CatabolicError("that name belongs to another tag")
            if remove:
                if db.execute("SELECT 1 FROM tags WHERE name=?", (alias,)).fetchone():
                    raise CatabolicError(
                        "cannot remove a canonical name; use tag rename"
                    )
                db.execute(
                    "DELETE FROM tag_names WHERE name=? AND tag_id=?",
                    (alias, identifier),
                )
            else:
                reserve_name(db, alias, identifier)
        return {"tag_id": identifier, "alias": alias, "removed": remove}

    def parent(self, child, parent, *, remove=False):
        self.app.require_recovered()
        with self.store.transaction() as db:
            child_id, parent_id = resolve_tag(db, child), resolve_tag(db, parent)
            if (
                not remove
                and db.execute(
                    """WITH RECURSIVE ancestors(id) AS (
                VALUES (?) UNION SELECT p.parent_id FROM tag_parents p
                JOIN ancestors a ON a.id=p.child_id) SELECT 1 FROM ancestors WHERE id=?""",
                    (parent_id, child_id),
                ).fetchone()
            ):
                raise CatabolicError("tag parent would create a cycle")
            if remove:
                db.execute(
                    "DELETE FROM tag_parents WHERE child_id=? AND parent_id=?",
                    (child_id, parent_id),
                )
            else:
                if (
                    not db.execute(
                        "SELECT 1 FROM tag_parents WHERE child_id=? AND parent_id=?",
                        (child_id, parent_id),
                    ).fetchone()
                    and db.execute(
                        "SELECT count(*) FROM tag_parents WHERE child_id=?", (child_id,)
                    ).fetchone()[0]
                    >= 100
                ):
                    raise CatabolicError("a tag may have at most 100 direct parents")
                db.execute(
                    "INSERT OR IGNORE INTO tag_parents VALUES (?,?)",
                    (child_id, parent_id),
                )
        return {"child_id": child_id, "parent_id": parent_id, "removed": remove}

    def assign(
        self,
        names,
        *,
        items=(),
        files=(),
        source="manual",
        confidence=None,
        note="",
        remove=False,
    ):
        names, items, files = list(names), sorted(set(items)), sorted(set(files))
        if (
            not names
            or not items
            and not files
            or len(names) * (len(items) + len(files)) > 1000
        ):
            raise CatabolicError(
                "provide tags and item/file IDs; at most 1000 assignments per atomic batch"
            )
        text_value(source, "source", 200)
        text_value(note, "note", 4000, empty=True)
        if confidence is not None and (
            type(confidence) not in (int, float)
            or not math.isfinite(confidence)
            or not 0 <= confidence <= 1
        ):
            raise CatabolicError("confidence must be a finite number between 0 and 1")
        self.app.require_recovered()
        result = []
        with self.store.transaction() as db:
            ids = sorted({resolve_tag(db, name) for name in names})
            now = datetime.now(timezone.utc).isoformat()
            for subject, targets in (("item", items), ("file", files)):
                for target in targets:
                    if not db.execute(
                        f"SELECT 1 FROM {subject}s WHERE id=?", (target,)
                    ).fetchone():
                        raise CatabolicError(f"unknown {subject}: {target}")
                    for tag_id in ids:
                        row = db.execute(
                            f"SELECT * FROM {subject}_tags WHERE {subject}_id=? AND tag_id=? AND source=?",
                            (target, tag_id, source),
                        ).fetchone()
                        identifier = row["id"] if row else str(uuid4())
                        if remove:
                            if row and row["active"]:
                                db.execute(
                                    f"UPDATE {subject}_tags SET active=0,updated_at=? WHERE id=?",
                                    (now, identifier),
                                )
                        elif row is None:
                            db.execute(
                                f"INSERT INTO {subject}_tags VALUES (?,?,?,?,?,?,1,?,?)",
                                (
                                    identifier,
                                    target,
                                    tag_id,
                                    source,
                                    confidence,
                                    note,
                                    now,
                                    now,
                                ),
                            )
                        elif (row["active"], row["confidence"], row["note"]) != (
                            1,
                            confidence,
                            note,
                        ):
                            db.execute(
                                f"UPDATE {subject}_tags SET active=1,confidence=?,note=?,updated_at=? WHERE id=?",
                                (confidence, note, now, identifier),
                            )
                        result.append(
                            {
                                "id": identifier if row or not remove else None,
                                "subject_type": subject,
                                "subject_id": target,
                                "tag_id": tag_id,
                                "source": source,
                                "active": not remove,
                            }
                        )
        return {"assignments": result, "count": len(result)}
