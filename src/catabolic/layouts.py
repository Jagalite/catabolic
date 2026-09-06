"""Declarative naming layouts. Applying a layout records desired mappings only."""

import hashlib
import json
import re
import string
import unicodedata
from pathlib import PurePosixPath

from .domain import CatabolicError, name, relative_path
from .media import RELATIONS, ROLES, media_kind, vocabulary
from .selection import selected_associations, validate_selection
from .store import encode

PRESETS = {
    "flat": {
        "version": 1,
        "rules": [{"name": "all", "path": "{item.kind}/{item.id}/{file.name}"}],
    },
    "plex": {
        "version": 1,
        "rules": [
            {
                "name": "movies",
                "when": {"kinds": ["movie"], "roles": ["primary"]},
                "path": "Movies/{item.title} ({item.year})/{item.title} ({item.year}){file.extension}",
            },
            {
                "name": "subtitles",
                "when": {"kinds": ["movie"], "roles": ["subtitle"]},
                "path": "Movies/{item.title} ({item.year})/{item.title} ({item.year}).{association.metadata.language}{file.extension}",
            },
            {
                "name": "episodes",
                "when": {"kinds": ["episode"], "roles": ["primary"]},
                "relations": [
                    {"alias": "season", "kind": "part_of", "target_kind": "season"},
                    {
                        "alias": "series",
                        "from": "season",
                        "kind": "part_of",
                        "target_kind": "series",
                    },
                ],
                "path": "TV/{series.title}/Season {series.position:02d}/{series.title} - S{series.position:02d}E{season.position:02d}{file.extension}",
            },
            {
                "name": "tracks",
                "when": {"kinds": ["track"], "roles": ["primary"]},
                "relations": [
                    {"alias": "album", "kind": "part_of", "target_kind": "album"},
                    {
                        "alias": "artist",
                        "from": "album",
                        "kind": "performed_by",
                        "target_kind": "artist",
                    },
                ],
                "path": "Music/{artist.title}/{album.title}/{album.position:02d} - {item.title}{file.extension}",
            },
            {
                "name": "covers",
                "when": {"kinds": ["album"], "roles": ["cover"]},
                "relations": [
                    {"alias": "artist", "kind": "performed_by", "target_kind": "artist"}
                ],
                "path": "Music/{artist.title}/{item.title}/cover{file.extension}",
            },
        ],
    },
}


def _parts(template):
    if not isinstance(template, str) or not template or len(template) > 4096:
        raise CatabolicError(
            "layout path must be a nonempty template of at most 4096 characters"
        )
    try:
        result = list(string.Formatter().parse(template))
    except ValueError as exc:
        raise CatabolicError(f"invalid path template: {exc}") from exc
    for literal, field, spec, conversion in result:
        if re.search(r"[\\\x00-\x1f\x7f]", literal):
            raise CatabolicError(
                "template literals cannot contain backslashes or control characters"
            )
        if field is not None and (
            not re.fullmatch(r"[a-zA-Z][a-zA-Z0-9_]*(?:\.[a-zA-Z0-9_-]+)+", field)
            or conversion
            or (spec and not re.fullmatch(r"0?[1-9][0-9]?d", spec))
        ):
            raise CatabolicError(
                "templates allow dotted field names and small integer formats such as :02d; expressions, indexing, and conversions are not allowed"
            )
    # Reject dangerous static structure before any metadata is available.
    relative_path(
        "".join(literal + ("value" if field else "") for literal, field, _, _ in result)
    )
    return result


def validate_layout(definition):
    if (
        not isinstance(definition, dict)
        or set(definition) - {"version", "rules", "selection"}
        or type(definition.get("version")) is not int
        or definition["version"] != 1
    ):
        raise CatabolicError("layout requires version: 1 and rules")
    if "selection" in definition:
        validate_selection(definition["selection"])
    rules = definition.get("rules")
    if not isinstance(rules, list) or not 1 <= len(rules) <= 100:
        raise CatabolicError("layout must contain 1–100 rules")
    names = set()
    for rule in rules:
        if not isinstance(rule, dict) or set(rule) - {
            "name",
            "when",
            "path",
            "relations",
        }:
            raise CatabolicError("invalid layout rule fields")
        identifier = rule.get("name")
        if not isinstance(identifier, str):
            raise CatabolicError("each rule needs a name")
        name(identifier)
        if identifier in names:
            raise CatabolicError("rule names must be unique")
        names.add(identifier)
        _parts(rule.get("path"))
        when = rule.get("when", {})
        if not isinstance(when, dict) or set(when) - {
            "kinds",
            "roles",
            "has",
            "metadata",
        }:
            raise CatabolicError("when supports kinds, roles, has, and metadata")
        for key in ("kinds", "roles", "has"):
            if key in when and (
                not isinstance(when[key], list)
                or not when[key]
                or not all(isinstance(v, str) for v in when[key])
            ):
                raise CatabolicError(f"when.{key} must be a nonempty string array")
        for kind in when.get("kinds", []):
            media_kind(kind)
        for role in when.get("roles", []):
            vocabulary(role, ROLES, "role")
        if not isinstance(when.get("metadata", {}), dict):
            raise CatabolicError("when.metadata must be a JSON object")
        relations = rule.get("relations", [])
        if not isinstance(relations, list) or len(relations) > 8:
            raise CatabolicError("relations must be an array of at most 8 selectors")
        aliases = {"item"}
        for selector in relations:
            if not isinstance(selector, dict) or set(selector) - {
                "alias",
                "from",
                "kind",
                "direction",
                "target_kind",
            }:
                raise CatabolicError("invalid relation selector")
            alias = selector.get("alias")
            if (
                not isinstance(alias, str)
                or not re.fullmatch(r"[a-z][a-z0-9_]*", alias)
                or alias in aliases | {"file", "association"}
            ):
                raise CatabolicError("relation aliases must be unique lowercase names")
            if (
                not isinstance(selector.get("from", "item"), str)
                or selector.get("from", "item") not in aliases
                or selector.get("direction", "outgoing") not in ("outgoing", "incoming")
            ):
                raise CatabolicError(
                    "relation selector needs an earlier source alias and a valid direction"
                )
            if not isinstance(selector.get("kind"), str):
                raise CatabolicError("relation selector requires kind")
            vocabulary(selector["kind"], RELATIONS, "relationship kind")
            if "target_kind" in selector:
                if not isinstance(selector["target_kind"], str):
                    raise CatabolicError("target_kind must be a media kind string")
                media_kind(selector["target_kind"])
            aliases.add(alias)
        for _, field, _, _ in _parts(rule["path"]):
            if field and field.split(".")[0] not in aliases | {"file", "association"}:
                raise CatabolicError(f"unknown template alias: {field}")
    encode(definition)  # Refuse nonfinite JSON before persisting any configuration.
    return definition


def _component(value):
    value = unicodedata.normalize("NFC", str(value))
    value = re.sub(r'[\\/:*?"<>|\x00-\x1f\x7f]', "_", value).strip().rstrip(".")
    if not value or value in (".", ".."):
        raise CatabolicError("metadata produced an empty or invalid path component")
    return value


def render_path(template, context):
    output = []
    for literal, field, spec, _ in _parts(template):
        output.append(literal)
        if field is None:
            continue
        value = context
        for key in field.split("."):
            if not isinstance(value, dict) or key not in value or value[key] is None:
                raise CatabolicError(f"missing template field: {field}")
            value = value[key]
        if type(value) not in (str, int, float, bool):
            raise CatabolicError(f"template field must be scalar: {field}")
        if spec:
            if type(value) is not int:
                raise CatabolicError(f"integer required for {field}:{spec}")
            value = format(value, spec)
        if field == "file.path":
            value = "/".join(
                _component(part) for part in PurePosixPath(relative_path(value)).parts
            )
        elif field == "file.extension" and value == "":
            pass
        else:
            value = _component(value)
        output.append(value)
    path = relative_path("".join(output))
    if len(path.encode()) > 4096 or any(
        len(part.encode()) > 255 for part in path.split("/")
    ):
        raise CatabolicError("generated path exceeds portable filesystem length limits")
    return path


# Bound decoded graph data independently of the complete desired plan. SQL
# chunks also stay below SQLite's historical 999-variable default.
_LAYOUT_BATCH_SIZE = 1000
_SQL_BATCH_SIZE = 500
_RELATION_ERROR = object()


def _batches(values, size):
    for start in range(0, len(values), size):
        yield values[start : start + size]


def _load_items(store, identifiers):
    items = {}
    for batch in _batches(sorted(set(identifiers)), _SQL_BATCH_SIZE):
        marks = ",".join("?" for _ in batch)
        for row in store.db.execute(
            f"SELECT id,kind,metadata FROM items WHERE id IN ({marks})", batch
        ):
            metadata = json.loads(row["metadata"])
            items[row["id"]] = {
                "id": row["id"],
                "kind": row["kind"],
                "title": metadata.get("title"),
                "year": metadata.get("year"),
                "metadata": metadata,
            }
    return items


def _matching_rule(rules, item, association):
    for rule in rules:
        when = rule.get("when", {})
        if ("kinds" in when and item["kind"] not in when["kinds"]) or (
            "roles" in when and association["role"] not in when["roles"]
        ):
            continue
        if any(item["metadata"].get(key) is None for key in when.get("has", [])):
            continue
        if any(
            key not in item["metadata"]
            or encode(item["metadata"][key]) != encode(value)
            for key, value in when.get("metadata", {}).items()
        ):
            continue
        return rule
    return None


def _resolve_relation(store, selector, contexts):
    source_alias = selector.get("from", "item")
    origins = sorted({context[source_alias]["id"] for context in contexts})
    origin, target = (
        ("source_id", "target_id")
        if selector.get("direction", "outgoing") == "outgoing"
        else ("target_id", "source_id")
    )
    matches = {}
    for batch in _batches(origins, _SQL_BATCH_SIZE):
        marks = ",".join("?" for _ in batch)
        join, condition = "", ""
        args = [*batch, selector["kind"]]
        if "target_kind" in selector:
            join = f" JOIN items i ON i.id=r.{target}"
            condition = " AND i.kind=?"
            args.append(selector["target_kind"])
        # Aggregate before fetching metadata: a high-degree or ambiguous origin
        # returns one count, not an unbounded Python list of related items.
        for row in store.db.execute(
            f"SELECT r.{origin} AS origin, count(*) AS matches, "
            f"min(r.{target}) AS item_id, min(r.position) AS position "
            f"FROM item_relationships r{join} WHERE r.{origin} IN ({marks}) "
            f"AND r.active=1 AND r.kind=?{condition} GROUP BY r.{origin}",
            args,
        ):
            matches[row["origin"]] = row
    targets = _load_items(
        store, [row["item_id"] for row in matches.values() if row["matches"] == 1]
    )
    for context in contexts:
        match = matches.get(context[source_alias]["id"])
        count = match["matches"] if match else 0
        if count != 1:
            context[_RELATION_ERROR] = (
                f"relation {selector['alias']} must match exactly one item; found {count}"
            )
        else:
            context[selector["alias"]] = {
                **targets[match["item_id"]],
                "position": match["position"],
            }


def _layout_contexts(store, associations, rules):
    for batch in _batches(associations, _LAYOUT_BATCH_SIZE):
        items = _load_items(store, [a["item_id"] for a in batch])
        entries, by_rule = [], {}
        for association in batch:
            item = items[association["item_id"]]
            selected = _matching_rule(rules, item, association)
            context = {"item": item}
            entries.append((association, selected, context))
            if selected is not None:
                by_rule.setdefault(selected["name"], []).append(context)
        for rule in rules:
            contexts = by_rule.get(rule["name"], [])
            for selector in rule.get("relations", []):
                contexts = [
                    context for context in contexts if _RELATION_ERROR not in context
                ]
                if not contexts:
                    break
                _resolve_relation(store, selector, contexts)
        # Preserve association order, including blockers and first-match rules.
        yield from entries


class Layouts:
    def __init__(self, app):
        self.app, self.store = app, app.store

    def _read(self, key):
        row = self.store.db.execute(
            "SELECT value FROM meta WHERE key=?", (key,)
        ).fetchone()
        return json.loads(row[0]) if row else None

    def get(self, identifier):
        name(identifier)
        definition = self._read("layout:" + identifier)
        if definition is None:
            raise CatabolicError(f"unknown layout: {identifier}")
        return {"name": identifier, "definition": validate_layout(definition)}

    def list(self):
        return {
            "layouts": [
                {"name": row["key"][7:], "definition": json.loads(row["value"])}
                for row in self.store.rows(
                    "SELECT key,value FROM meta WHERE key LIKE 'layout:%' ORDER BY key"
                )
            ]
        }

    def put(self, identifier, definition):
        name(identifier)
        validate_layout(definition)
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO meta(key,value) VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                ("layout:" + identifier, encode(definition)),
            )
        return self.get(identifier)

    def _plan(self, identifier, catalog, replace_layout, allow_empty=False):
        definition = self.get(identifier)["definition"]
        if not self.store.db.execute(
            "SELECT 1 FROM catalogs WHERE id=?", (catalog,)
        ).fetchone():
            raise CatabolicError(f"unknown catalog: {catalog}")
        self.app.require_recovered()
        owner = self._read("layout-catalog:" + catalog)
        if owner and owner["layout"] != identifier and not replace_layout:
            raise CatabolicError(
                f"catalog is managed by layout {owner['layout']}; use --replace-layout explicitly"
            )
        owned = set(owner["mapping_ids"] if owner else [])
        current = {
            r["id"]: r
            for r in self.store.rows(
                "SELECT * FROM mappings WHERE catalog=?", (catalog,)
            )
        }
        blockers, desired, skipped = [], {}, 0
        if owned - current.keys():
            raise CatabolicError(
                "layout ownership references missing mappings; refusing to discard its history"
            )
        selection_report = None
        if "selection" in definition:
            associations, selection_report = selected_associations(
                self.store, definition["selection"]
            )
        else:
            associations = self.store.rows(
                "SELECT a.*,f.path,f.location FROM item_files a JOIN files f ON f.id=a.file_id WHERE a.active=1 ORDER BY a.id LIMIT 100001"
            )
        if len(associations) > 100000:
            raise CatabolicError(
                "layout planning supports at most 100000 active identifications"
            )
        for association, selected, context in _layout_contexts(
            self.store, associations, definition["rules"]
        ):
            item = context["item"]
            if selected is None:
                skipped += 1
                continue
            try:
                path = PurePosixPath(association["path"])
                context.update(
                    {
                        "association": {
                            **association,
                            "metadata": json.loads(association["metadata"]),
                        },
                        "file": {
                            "id": association["file_id"],
                            "name": path.name,
                            "stem": path.stem,
                            "extension": path.suffix,
                            "path": str(path),
                            "location": association["location"],
                        },
                    }
                )
                if _RELATION_ERROR in context:
                    raise CatabolicError(context[_RELATION_ERROR])
                destination = render_path(selected["path"], context)
                mapping_id = self.app.mapping_id(
                    catalog, association["file_id"], item["id"], destination
                )
                if mapping_id in current and mapping_id not in owned:
                    raise CatabolicError(
                        "generated destination belongs to an explicit mapping; refusing to adopt it"
                    )
                desired[mapping_id] = {
                    "id": mapping_id,
                    "catalog": catalog,
                    "file_id": association["file_id"],
                    "item_id": item["id"],
                    "path": destination,
                    "active": 1,
                }
            except CatabolicError as exc:
                blockers.append(
                    {
                        "association": association["id"],
                        "rule": selected["name"],
                        "reason": str(exc),
                    }
                )
        # Compare complete generated and explicit scopes, including portable
        # case/Unicode aliases and parent/child paths, before recording anything.
        paths = {}
        for row in [
            *desired.values(),
            *(r for key, r in current.items() if key not in owned and r["active"]),
        ]:
            key = unicodedata.normalize("NFC", row["path"]).casefold()
            if key in paths and paths[key]["id"] != row["id"]:
                blockers.append(
                    {"path": row["path"], "reason": "destination collision"}
                )
            paths[key] = row
        for key, row in paths.items():
            parts = key.split("/")
            if any("/".join(parts[:index]) in paths for index in range(1, len(parts))):
                blockers.append(
                    {
                        "path": row["path"],
                        "reason": "destination has a conflicting file parent",
                    }
                )
        removals = [key for key in owned - desired.keys() if current[key]["active"]]
        changes = [{"action": "disable", **current[key]} for key in sorted(removals)]
        changes += [
            {"action": "enable" if key in current else "create", **row}
            for key, row in desired.items()
            if key not in current or not current[key]["active"]
        ]
        if (
            selection_report is not None
            and not desired
            and removals
            and not allow_empty
        ):
            blockers.append(
                {
                    "reason": "query layout would clear all generated mappings; inspect the selection and use --allow-empty explicitly"
                }
            )
        plan = {
            "layout": identifier,
            "catalog": catalog,
            "definition_sha256": hashlib.sha256(
                encode(definition).encode()
            ).hexdigest(),
            "safe": not blockers,
            "blockers": blockers,
            "changes": changes,
            "desired_count": len(desired),
            "unchanged_count": len(desired)
            - sum(c["action"] != "disable" for c in changes),
            "skipped_associations": skipped,
        }
        if selection_report is not None:
            plan["selection"] = selection_report
        return plan, desired, removals, owned

    def run(
        self,
        identifier,
        catalog="global",
        *,
        apply=False,
        replace_layout=False,
        allow_empty=False,
        limit=100,
    ):
        if type(limit) is not int or not 1 <= limit <= 1000:
            raise CatabolicError("limit must be between 1 and 1000")
        if apply:
            with self.store.transaction() as db:
                plan, desired, removals, owned = self._plan(
                    identifier, catalog, replace_layout, allow_empty
                )
                if plan["safe"]:
                    db.executemany(
                        "UPDATE mappings SET active=0 WHERE id=?",
                        [(key,) for key in removals],
                    )
                    db.executemany(
                        "INSERT INTO mappings(id,catalog,file_id,item_id,path,active) VALUES (?,?,?,?,?,1) ON CONFLICT(id) DO UPDATE SET active=1",
                        [
                            (r["id"], catalog, r["file_id"], r["item_id"], r["path"])
                            for r in desired.values()
                        ],
                    )
                    state = {
                        "layout": identifier,
                        "definition_sha256": plan["definition_sha256"],
                        "mapping_ids": sorted(owned | desired.keys()),
                    }
                    db.execute(
                        "INSERT INTO meta(key,value) VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                        ("layout-catalog:" + catalog, encode(state)),
                    )
                plan["applied"] = plan["safe"]
        else:
            plan, _, _, _ = self._plan(identifier, catalog, replace_layout, allow_empty)
            plan["applied"] = False
        plan["change_count"], plan["blocker_count"] = (
            len(plan["changes"]),
            len(plan["blockers"]),
        )
        plan["details_truncated"] = (
            max(plan["change_count"], plan["blocker_count"]) > limit
        )
        plan["changes"], plan["blockers"] = (
            plan["changes"][:limit],
            plan["blockers"][:limit],
        )
        return plan
