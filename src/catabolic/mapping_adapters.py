# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Destination-specific wire formats behind the mapping snapshot contract."""

import re
from urllib.parse import quote, urlencode

from .consumer_adapters import path, text
from .destination_mappings import fail
from .metadata_publication import value as metadata_value
from .plex_metadata import FIELDS, KINDS
from .store import encode


class PlexMapping:
    validate_value = staticmethod(metadata_value)

    capabilities = {**{f: "scalar" for f in FIELDS}, "genres": "set", "labels": "set"}
    sets = {"genres": "genre", "labels": "label"}

    def __init__(self, transport):
        self.transport = transport

    def read(self, library, target, fields):
        key = target["id"]
        if not re.fullmatch(r"[0-9]{1,20}", key):
            fail("invalid_plex_id")
        root = self.transport.xml("/library/metadata/" + key, {"includeGuids": 1})
        if root.tag != "MediaContainer" or len(root) != 1:
            fail("invalid_item_response")
        node = root[0]
        kind = node.get("type")
        if (
            node.get("ratingKey") != key
            or node.get("librarySectionID", root.get("librarySectionID"))
            != library["id"]
            or kind not in KINDS
            or KINDS[kind][0] != library["type"]
        ):
            fail("remote_identity_mismatch")
        allowed = KINDS[kind][2] | (
            set(self.sets) if kind in ("movie", "episode") else set()
        )
        if set(fields) - allowed:
            fail("unsupported_fields_for_kind")
        paths = sorted({path(p.get("file")) for p in node.findall("Media/Part")})
        if path(target["path"]) not in paths or len(paths) > 100:
            fail("remote_path_mismatch")
        locks = {}
        for f in node.findall("Field"):
            if f.get("locked") not in ("0", "1") or f.get("name") in locks:
                fail("invalid_locks")
            locks[f.get("name")] = f.get("locked") == "1"
        values = {}
        selected_locks = {}
        for field in fields:
            wire = self.sets[field] if field in self.sets else FIELDS[field]
            selected_locks[field] = locks.get(wire, False)
            if field in self.sets:
                values[field] = sorted(
                    {text(t.get("tag"), 255) for t in node.findall(wire.title())}
                )
            elif field == "year":
                raw = node.get(wire)
                if raw and not re.fullmatch(r"[0-9]{1,4}", raw):
                    fail("invalid_remote_year")
                values[field] = int(raw) if raw else None
            else:
                values[field] = node.get(wire, "")
        return {
            "identity": {"guid": text(node.get("guid")), "paths": paths, "kind": kind},
            "values": values,
            "locks": selected_locks,
        }

    def parameters(self, row):
        params = {
            "id": row["target"]["id"],
            "type": KINDS[row["before"]["identity"]["kind"]][1],
        }
        for field, value in row["after"].items():
            if value == row["before"]["values"][field]:
                continue
            wire = self.sets[field] if field in self.sets else FIELDS[field]
            params[wire + ".locked"] = int(row["before"]["locks"][field])
            if field in self.sets:
                # Full final set plus explicit removals (including the last tag).
                for i, tag in enumerate(value):
                    params[f"{wire}[{i}].tag.tag"] = tag
                removed = set(row["before"]["values"][field]) - set(value)
                if removed:
                    params[wire + "[].tag.tag-"] = ",".join(
                        quote(t, safe="") for t in sorted(removed)
                    )
            else:
                params[wire + ".value"] = value if value is not None else ""
        return params

    def validate(self, library, row):
        if len(urlencode(self.parameters(row)).encode()) > 16384:
            fail("request_too_large")

    def write(self, library, row):
        self.transport.call(
            "/library/sections/" + quote(library["id"], safe="") + "/all",
            "PUT",
            self.parameters(row),
        )

    @staticmethod
    def matches(observed, before, after):
        return observed == {**before, "values": after}


class JellyfinMapping:
    validate_value = staticmethod(metadata_value)

    fields = {
        "title": "Name",
        "sort_title": "ForcedSortName",
        "original_title": "OriginalTitle",
        "summary": "Overview",
        "year": "ProductionYear",
        "content_rating": "OfficialRating",
        "genres": "Genres",
        "tags": "Tags",
    }
    capabilities = {f: "set" if f in ("genres", "tags") else "scalar" for f in fields}
    # UpdateItem is a whole metadata DTO operation on supported older servers.
    # Round-trip every mutable property, and verify untouched ones too.
    editable = set(
        "Name ForcedSortName OriginalTitle CriticRating CommunityRating IndexNumber ParentIndexNumber Overview Genres AirsAfterSeasonNumber AirsBeforeEpisodeNumber AirsBeforeSeasonNumber Height Taglines Studios DateCreated EndDate PremiereDate ProductionYear OfficialRating CustomRating Tags ProductionLocations PreferredMetadataCountryCode PreferredMetadataLanguage DisplayOrder AspectRatio LockData LockedFields RunTimeTicks ProviderIds Video3DFormat AlbumArtists ArtistItems Album AirDays AirTime Status".split()
    )

    def __init__(self, transport):
        self.transport = transport

    def read(self, library, target, fields):
        key = target["id"]
        if not re.fullmatch(r"[A-Fa-f0-9-]{32,36}", key):
            fail("invalid_jellyfin_id")
        response = self.transport.data(
            "/Items?"
            + urlencode(
                {
                    "Ids": key,
                    "Limit": 2,
                    "Recursive": "true",
                    "EnableUserData": "false",
                    "Fields": "Path,Genres,Tags,ProviderIds,Settings,Overview,OriginalTitle,SortName,CustomRating,DateCreated,ProductionLocations,Studios,Taglines,SpecialEpisodeNumbers,MediaSources",
                }
            )
        )
        if (
            not isinstance(response, dict)
            or response.get("TotalRecordCount") != 1
            or len(response.get("Items", [])) != 1
        ):
            fail("invalid_item_response")
        dto = response["Items"][0]
        if (
            not isinstance(dto, dict)
            or dto.get("Id") != key
            or dto.get("Type") not in ("Movie", "Episode")
        ):
            fail("remote_identity_mismatch")
        if path(dto.get("Path")) != path(target["path"]):
            fail("remote_path_mismatch")
        ancestors = self.transport.data("/Items/" + key + "/Ancestors")
        if not isinstance(ancestors, list) or not any(
            isinstance(a, dict) and a.get("Id") == library["id"] for a in ancestors
        ):
            fail("wrong_library")
        if not {
            "Name",
            "Genres",
            "Tags",
            "ProviderIds",
            "LockData",
            "LockedFields",
        } <= set(dto):
            fail("incomplete_metadata_dto")
        if (
            not isinstance(dto["ProviderIds"], dict)
            or type(dto["LockData"]) is not bool
            or not isinstance(dto["LockedFields"], list)
        ):
            fail("invalid_metadata_dto")
        payload = {k: dto[k] for k in self.editable if k in dto}
        values = {}
        for field in fields:
            wire = self.fields[field]
            v = dto.get(wire)
            if self.capabilities[field] == "set":
                if not isinstance(v, list) or len(v) > 1000:
                    fail("invalid_remote_set")
                v = sorted({text(t, 255) for t in v})
            elif field != "year":
                v = v or ""
                if not isinstance(v, str):
                    fail("invalid_remote_scalar")
            elif v is not None and type(v) is not int:
                fail("invalid_remote_year")
            values[field] = v
        return {
            "identity": {
                "path": dto["Path"],
                "kind": dto["Type"],
                "providers": dto["ProviderIds"],
            },
            "values": values,
            "payload": payload,
        }

    def payload(self, row):
        return {
            **row["before"]["payload"],
            **{self.fields[k]: v for k, v in row["after"].items()},
        }

    def validate(self, library, row):
        if len(encode(self.payload(row)).encode()) > 1024 * 1024:
            fail("request_too_large")

    def write(self, library, row):
        self.transport.call(
            "/Items/" + row["target"]["id"],
            "POST",
            body=encode(self.payload(row)).encode(),
        )

    def matches(self, observed, before, after):
        changed = {self.fields[f] for f in after}
        return (
            observed["identity"] == before["identity"]
            and observed["values"] == after
            and {k: v for k, v in observed["payload"].items() if k not in changed}
            == {k: v for k, v in before["payload"].items() if k not in changed}
        )


ADAPTERS = {"plex": PlexMapping, "jellyfin": JellyfinMapping}


def destination_adapter(application, transport, collections=None):
    if application not in ADAPTERS:
        fail("unsupported_destination")
    base = ADAPTERS[application](transport)
    return (
        CollectionMapping(base, application, collections)
        if collections is not None
        else base
    )


def capabilities():
    result = {
        name: {
            "fields": {
                **cls.capabilities,
                "collection_ids": "set (requires destination.collections)",
            },
            "missing_rows": "retain",
            "set_policy": "owned_merge",
            "scalar_policy": "replace_with_conflict_detection",
            "collections": "explicit_existing_ids",
        }
        for name, cls in ADAPTERS.items()
    }

    from .publication_mappings import capabilities as publication_capabilities

    for name, operations in publication_capabilities().items():
        result.setdefault(name, {})["operations"] = operations
    for name in ADAPTERS:
        result[name].setdefault("operations", {})["metadata"] = {
            "query": "rows",
            "definition_version": 1,
        }
    result["http"] = {
        "operations": {
            "openapi": {
                "query": ["rows", "document"],
                "definition_version": 3,
                "delivery": "durable_json_http",
                "acknowledgment": "http_2xx",
            }
        }
    }
    return result


class CollectionMapping:
    """Explicit adoption of existing collection IDs, never name-based creation."""

    def __init__(self, base, application, collections):
        if not isinstance(collections, list) or not 1 <= len(collections) <= 20:
            fail("invalid_collection_scope")
        pattern = r"[0-9]{1,20}" if application == "plex" else r"[a-f0-9]{32}"
        if any(
            not isinstance(k, str) or not re.fullmatch(pattern, k) for k in collections
        ) or len(set(collections)) != len(collections):
            fail("invalid_collection_id")
        self.base = base
        self.validate_value = base.validate_value
        self.transport = base.transport
        self.application = application
        self.collections = sorted(collections)
        self.capabilities = {**base.capabilities, "collection_ids": "set"}

    def read(self, library, target, fields):
        if set(fields) != {"collection_ids"}:
            fail("collection_mapping_requires_separate_definition")
        snapshot = self.base.read(library, target, {})
        identities = {}
        memberships = []
        for key in self.collections:
            if self.application == "plex":
                root = self.transport.xml("/library/metadata/" + key)
                if len(root) != 1:
                    fail("invalid_collection")
                node = root[0]
                if (
                    node.get("ratingKey") != key
                    or node.get("type") != "collection"
                    or node.get("smart", "0") != "0"
                    or node.get("librarySectionID") != library["id"]
                    or node.get("subtype") != snapshot["identity"]["kind"]
                ):
                    fail("unsupported_collection")
                identities[key] = text(node.get("guid"))
                children = self.transport.xml(
                    "/library/metadata/" + key + "/children",
                    {"X-Plex-Container-Start": 0, "X-Plex-Container-Size": 1000},
                )
                if (
                    children.get("offset", "0") != "0"
                    or children.get("totalSize", children.get("size"))
                    != str(len(children))
                    or len(children) >= 1000
                    or any(not n.get("ratingKey") for n in children)
                ):
                    fail("incomplete_collection")
                members = [n.get("ratingKey") for n in children]
            else:
                response = self.transport.data(
                    "/Items?"
                    + urlencode({"Ids": key, "Limit": 2, "Fields": "DateCreated,Path"})
                )
                if (
                    not isinstance(response, dict)
                    or response.get("TotalRecordCount") != 1
                    or len(response.get("Items", [])) != 1
                ):
                    fail("invalid_collection")
                node = response["Items"][0]
                if node.get("Id") != key or node.get("Type") != "BoxSet":
                    fail("unsupported_collection")
                identities[key] = {
                    "created": text(node.get("DateCreated")),
                    "path": path(node.get("Path")),
                }
                children = self.transport.data(
                    "/Items?"
                    + urlencode({"ParentId": key, "Recursive": "false", "Limit": 1000})
                )
                if (
                    not isinstance(children, dict)
                    or not isinstance(children.get("Items"), list)
                    or children.get("TotalRecordCount") != len(children["Items"])
                    or len(children["Items"]) >= 1000
                    or any(
                        not isinstance(n, dict) or not n.get("Id")
                        for n in children["Items"]
                    )
                ):
                    fail("incomplete_collection")
                members = [n["Id"] for n in children["Items"]]
            if target["id"] in members:
                memberships.append(key)
        snapshot["identity"]["collections"] = identities
        snapshot["values"] = {"collection_ids": memberships}
        return snapshot

    def validate(self, library, row):
        if set(row["after"]["collection_ids"]) - set(self.collections):
            fail("collection_outside_adopted_scope")

    def write(self, library, row):
        before = set(row["before"]["values"]["collection_ids"])
        after = set(row["after"]["collection_ids"])
        for key in sorted(before ^ after):
            # Recheck identity/membership before each operation in this item.
            observed = self.read(library, row["target"], row["after"])
            if (
                observed["identity"] != row["before"]["identity"]
                or set(observed["values"]["collection_ids"]) != before
            ):
                fail("collection_changed")
            adding = key in after
            if self.application == "plex":
                route = "/library/collections/" + key + "/items"
                if adding:
                    server = self.transport.connection["server_id"]
                    uri = (
                        "server://"
                        + server
                        + "/com.plexapp.plugins.library/library/metadata/"
                        + row["target"]["id"]
                    )
                    self.transport.call(route, "PUT", {"uri": uri})
                else:
                    self.transport.call(route + "/" + row["target"]["id"], "DELETE")
            else:
                self.transport.call(
                    "/Collections/" + key + "/Items",
                    "POST" if adding else "DELETE",
                    {"ids": row["target"]["id"]},
                )
            if adding:
                before.add(key)
            else:
                before.remove(key)

    def matches(self, observed, before, after):
        return observed == {**before, "values": after}
