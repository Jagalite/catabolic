# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Small application adapters. No account discovery, global mutations or hooks."""

import json
import re
import time
from dataclasses import asdict, dataclass
from email.utils import parsedate_to_datetime
from pathlib import PurePosixPath
from typing import Protocol
from urllib.parse import quote, urlencode, urlsplit
from xml.etree import ElementTree as ET

from .domain import CatabolicError
from .network_adapters import request, token


class ConsumerError(CatabolicError):
    def __init__(self, code, *, retry_after=None):
        self.code = code
        self.safe_to_retry = code in (
            "temporarily_failed",
            "rate_limited",
            "unavailable",
            "busy",
            "publication_unhealthy",
            "publication_changed",
        )
        try:
            self.retry_after = min(86400, max(0, int(retry_after or 0)))
        except (TypeError, ValueError):
            try:
                self.retry_after = min(
                    86400,
                    max(
                        0, parsedate_to_datetime(retry_after).timestamp() - time.time()
                    ),
                )
            except (TypeError, ValueError, OverflowError):
                self.retry_after = 60
        super().__init__(
            "consumer "
            + code
            + (
                "; retry delivery"
                if self.safe_to_retry
                else "; inspect and repair configuration"
            )
        )

    def result(self):
        return {
            "code": self.code,
            "safe_to_retry": self.safe_to_retry,
            "message": str(self),
        }


@dataclass(frozen=True)
class Capabilities:
    identity: bool = True
    libraries: bool = True
    create_library: bool = False
    section_scan: bool = True
    targeted_scan: bool = False
    scan_activity: bool = False
    indexed_paths: bool = False


class Adapter(Protocol):
    capabilities: Capabilities

    def inspect(self) -> dict: ...
    def libraries(self) -> list[dict]: ...
    def scan(self, library: dict) -> None: ...
    def choices(self, kind: str) -> dict: ...
    def create(self, spec: dict) -> None: ...
    def indexed(self, library: dict, expected: list[str], limit: int) -> dict: ...


def text(value, maximum=4096):
    if (
        not isinstance(value, str)
        or not value
        or len(value) > maximum
        or any(ord(c) < 32 for c in value)
    ):
        raise ConsumerError("invalid_configuration")
    return value


def segment(value):
    text(value, 255)
    if value in (".", "..") or "/" in value or "\\" in value:
        raise ConsumerError("invalid_response")
    return value


def endpoint(value):
    text(value)
    u = urlsplit(value)
    if (
        u.scheme not in ("http", "https")
        or not u.hostname
        or u.username
        or u.password
        or u.query
        or u.fragment
    ):
        raise ConsumerError("invalid_configuration")
    return value.rstrip("/")


def consumer_credential_ref(value):
    if isinstance(value, str) and value.startswith("file:"):
        path(value[5:])
        return value
    return credential_ref(value)


def credential_ref(value):
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", value):
        raise ConsumerError("invalid_configuration")
    return value


def path(value, *, relative=False):
    if relative and value == "":
        return ""
    text(value)
    if (
        "\\" in value
        or any(p in (".", "..", "") for p in value.strip("/").split("/"))
        or value.startswith("//")
    ):
        if not (value == "/" and not relative):
            raise ConsumerError("invalid_path")
    p = PurePosixPath(value)
    if p.is_absolute() == relative:
        raise ConsumerError("invalid_path")
    return str(p)


def within(child, parent):
    return PurePosixPath(child).is_relative_to(PurePosixPath(parent or "."))


def translate(binding, relative):
    relative = path(relative, relative=True)
    if not within(relative, binding["subtree"]):
        raise ConsumerError("outside_binding_scope")
    suffix = PurePosixPath(relative).relative_to(
        PurePosixPath(binding["subtree"] or ".")
    )
    return str(PurePosixPath(binding["remote_root"]) / suffix)


class HTTPAdapter:
    def __init__(self, connection):
        self.connection = connection
        self.base = endpoint(connection["endpoint"])

    def call(self, route, method="GET", params=None):
        try:
            reference = self.connection["credential_env"]
            client_id = None
            if reference.startswith("file:"):
                from .plex_login import credential

                secret, client_id = credential(reference)
            else:
                secret = token(reference)
        except CatabolicError:
            raise ConsumerError("unauthorized") from None
        headers = {
            "X-Plex-Token"
            if self.connection["application"] == "plex"
            else "X-Emby-Token": secret,
            "Accept": "application/xml"
            if self.connection["application"] == "plex"
            else "application/json",
        }
        if client_id:
            headers["X-Plex-Client-Identifier"] = client_id
            headers["X-Plex-Product"] = "Catabolic"
        try:
            return request(
                self.base + route + ("?" + urlencode(params) if params else ""),
                method=method,
                headers=headers,
                maximum=8 * 1024 * 1024,
                timeout=15,
                structured_errors=True,
            )
        except ConsumerError:
            raise
        except CatabolicError:
            raise ConsumerError("temporarily_failed") from None

    def create(self, spec):
        raise ConsumerError("unsupported")

    def choices(self, kind):
        raise ConsumerError("unsupported")

    def indexed(self, library, expected, limit):
        raise ConsumerError("unsupported")


class Plex(HTTPAdapter):
    capabilities = Capabilities(
        create_library=True, scan_activity=True, indexed_paths=True
    )
    types = {"movie": 1, "show": 2, "artist": 8, "photo": 13}

    def xml(self, route, params=None):
        raw = self.call(route, params=params)
        if b"<!DOCTYPE" in raw.upper() or b"<!ENTITY" in raw.upper():
            raise ConsumerError("invalid_response")
        try:
            return ET.fromstring(raw)
        except ET.ParseError:
            raise ConsumerError("invalid_response") from None

    def inspect(self):
        root = self.xml("/")
        return {
            "server_id": text(root.get("machineIdentifier")),
            "version": text(root.get("version")),
            "permissions": {
                "discovery": "authorized",
                "mutations": "unknown; checked on request",
                "read_only_libraries": root.get("readOnlyLibraries"),
            },
            "capabilities": asdict(self.capabilities),
        }

    def libraries(self):
        root = self.xml("/library/sections")
        rows = root.findall("Directory")
        if len(rows) > 1000:
            raise ConsumerError("response_limit")
        result = []
        for r in rows:
            result.append(
                {
                    "id": text(r.get("key"), 128),
                    "uuid": text(r.get("uuid"), 256),
                    "name": text(r.get("title")),
                    "type": text(r.get("type"), 64),
                    "roots": sorted(path(x.get("path")) for x in r.findall("Location")),
                    "scanner": r.get("scanner"),
                    "agent": r.get("agent"),
                    "created_at": r.get("createdAt"),
                    "scanning": r.get("refreshing") == "1"
                    if r.get("refreshing") in ("0", "1")
                    else None,
                }
            )
        return result

    def choices(self, kind):
        if kind not in self.types:
            raise ConsumerError("unsupported")
        # Ask the configured server; never invent modern agent/scanner defaults.
        scanners = self.xml(f"/system/scanners/{self.types[kind]}")
        agents = self.xml("/system/agents", {"mediaType": self.types[kind]})
        return {
            "type": kind,
            "scanners": sorted({text(x.get("name")) for x in scanners.iter("Scanner")}),
            "agents": sorted({text(x.get("identifier")) for x in agents.iter("Agent")}),
        }

    def create(self, spec):
        self.call(
            "/library/sections",
            "POST",
            {
                "name": spec["name"],
                "type": spec["type"],  # Library kind, not a numeric media/search type.
                "location": spec["root"],
                "scanner": spec["scanner"],
                "agent": spec["agent"],
                "language": spec["language"],
            },
        )

    def scan(self, library):
        # Normal section scan. No force/metadata refresh, trash or deletion API.
        self.call(
            "/library/sections/" + quote(library["id"], safe="") + "/refresh", "GET"
        )

    def indexed(self, library, expected, limit):
        if not 1 <= limit <= 1000 or len(expected) > limit:
            raise ConsumerError("response_limit")
        media_type = {"movie": 1, "show": 4, "artist": 10, "photo": 13}.get(
            library["type"]
        )
        if media_type is None:
            raise ConsumerError("unsupported")
        root = self.xml(
            "/library/sections/" + quote(library["id"], safe="") + "/all",
            {
                "type": media_type,
                "X-Plex-Container-Start": 0,
                "X-Plex-Container-Size": limit,
            },
        )
        found = {p.get("file") for p in root.iter("Part")}
        matches = [p for p in expected if p in found]
        return {
            "status": "indexed"
            if len(matches) == len(expected) and expected
            else "inconclusive",
            "expected": len(expected),
            "matched": len(matches),
            "matched_paths": matches,
            "identities": [
                {
                    "path": part.get("file"),
                    "rating_key": item.get("ratingKey"),
                    "guid": item.get("guid"),
                }
                for item in root
                for part in item.iter("Part")
                if part.get("file") in matches
            ],
            "complete": len(matches) == len(expected) and bool(expected),
            "coverage": "bounded first page; exact server-reported file paths, not playback/readability or generation causality",
        }


class Jellyfin(HTTPAdapter):
    capabilities = Capabilities()

    def data(self, route):
        try:
            return json.loads(self.call(route))
        except (ValueError, TypeError):
            raise ConsumerError("invalid_response") from None

    def inspect(self):
        r = self.data("/System/Info")
        if not isinstance(r, dict):
            raise ConsumerError("invalid_response")
        return {
            "server_id": text(r.get("Id")),
            "version": text(r.get("Version")),
            "permissions": {
                "discovery": "authorized",
                "mutations": "unknown; checked on request",
            },
            "capabilities": asdict(self.capabilities),
        }

    def libraries(self):
        rows = self.data("/Library/VirtualFolders")
        if (
            not isinstance(rows, list)
            or len(rows) > 1000
            or any(
                not isinstance(r, dict) or not isinstance(r.get("Locations", []), list)
                for r in rows
            )
        ):
            raise ConsumerError("invalid_response")
        return [
            {
                "id": segment(r.get("ItemId")),
                "uuid": segment(r.get("ItemId")),
                "name": text(r.get("Name")),
                "type": text(r.get("CollectionType") or "mixed"),
                "roots": sorted(path(p) for p in r.get("Locations", [])),
                "scanning": None,
            }
            for r in rows
        ]

    def scan(self, library):
        self.call(
            "/Items/" + quote(library["id"], safe="") + "/Refresh",
            "POST",
            {
                "Recursive": "true",
                "MetadataRefreshMode": "Default",
                "ImageRefreshMode": "Default",
                "ReplaceAllMetadata": "false",
                "ReplaceAllImages": "false",
            },
        )


def adapter(connection) -> Adapter:
    return {"plex": Plex, "jellyfin": Jellyfin}[connection["application"]](connection)


def validate_remote(connection, library=None):
    a = adapter(connection)
    identity = a.inspect()
    if identity["server_id"] != connection["server_id"]:
        raise ConsumerError("server_identity_changed")
    if library is None:
        return a, identity
    matches = [r for r in a.libraries() if r["id"] == library["id"]]
    if len(matches) != 1 or any(
        matches[0].get(k) != library.get(k)
        for k in ("uuid", "type", "roots", "created_at")
    ):
        raise ConsumerError("library_identity_changed")
    return a, matches[0]
