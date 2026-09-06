# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Interoperable metadata exports and new, non-overwriting projection bundles."""

import json
import mimetypes
import os
import re
from pathlib import Path, PurePosixPath
from urllib.parse import quote, urlsplit
from xml.etree import ElementTree as ET

from .domain import CatabolicError, relative_path
from .filesystem import open_directory, parent_handle, root_handle, source_stat
from .manifest import MAX_BYTES, path_key


def xml_text(value):
    value = str(value)
    if re.search(r"[\x00-\x08\x0b\x0c\x0e-\x1f\ud800-\udfff\ufffe\uffff]", value):
        raise CatabolicError("metadata contains characters forbidden by XML 1.0")
    return value


def xml(root):
    return ET.tostring(root, encoding="utf-8", xml_declaration=True).decode() + "\n"


def http_base(value):
    parsed = urlsplit(value or "")
    if (
        parsed.scheme not in ("https", "http")
        or not parsed.netloc
        or parsed.username
        or parsed.password
        or parsed.query
        or parsed.fragment
    ):
        raise CatabolicError(
            "base URL must be an HTTP(S) directory URL without credentials, query or fragment"
        )
    return value.rstrip("/") + "/"


def metadata_text(item, key, fallback=None):
    value = item["metadata"].get(key)
    if value is None or value == "":
        value = fallback
    if value is not None and not isinstance(value, str):
        raise CatabolicError(f"{key} must be text for metadata export: {item['id']}")
    return value


def build_export(document, format, base_url=None):
    content = document["content"]
    items = {item["id"]: item for item in content["items"]}
    associations = {value["id"]: value for value in content["associations"]}
    entries = [
        entry
        for entry in content["entries"]
        if any(
            associations[key]["role"] == "primary" for key in entry["association_ids"]
        )
    ]
    artifacts = {}
    if format == "xspf":
        base = http_base(base_url) if base_url else None
        playlist = ET.Element(
            "playlist", {"version": "1", "xmlns": "http://xspf.org/ns/0/"}
        )
        ET.SubElement(playlist, "title").text = xml_text(content["catalog"]["id"])
        tracks = ET.SubElement(playlist, "trackList")
        for entry in entries:
            item = items[entry["item_id"]]
            if item["kind"] not in (
                "movie",
                "episode",
                "video",
                "music_video",
                "track",
                "recording",
                "audiobook",
                "audiobook_chapter",
                "podcast_episode",
            ):
                continue
            track = ET.SubElement(tracks, "track")
            ET.SubElement(track, "location").text = (base or "") + quote(
                entry["path"], safe="/"
            )
            ET.SubElement(track, "title").text = xml_text(
                item["metadata"].get("title") or item["id"]
            )
        artifacts["catalog.xspf"] = xml(playlist)
    elif format == "opds":
        base = http_base(base_url)
        publications = {}
        for entry in entries:
            item = items[entry["item_id"]]
            if item["kind"] not in (
                "book",
                "book_edition",
                "comic_issue",
                "comic_volume",
            ):
                continue
            mime = (
                {
                    ".epub": "application/epub+zip",
                    ".pdf": "application/pdf",
                    ".cbz": "application/vnd.comicbook+zip",
                    ".cbr": "application/vnd.comicbook-rar",
                }.get(PurePosixPath(entry["path"]).suffix.lower())
                or mimetypes.guess_type(entry["path"])[0]
                or "application/octet-stream"
            )
            publication = publications.setdefault(
                item["id"],
                {
                    "metadata": {
                        "@type": "http://schema.org/Book",
                        "title": metadata_text(item, "title", item["id"]),
                        "identifier": "urn:catabolic:"
                        + content["database_id"]
                        + ":"
                        + quote(item["id"], safe=""),
                    },
                    "links": [],
                },
            )
            author = metadata_text(item, "author")
            if author:
                publication["metadata"]["author"] = [{"name": author}]
            publication["links"].append(
                {
                    "rel": "http://opds-spec.org/acquisition",
                    "href": base + quote(entry["path"], safe="/"),
                    "type": mime,
                }
            )
        artifacts["catalog.opds.json"] = (
            json.dumps(
                {
                    "metadata": {
                        "title": content["catalog"]["id"],
                        "numberOfItems": len(publications),
                    },
                    "links": [
                        {
                            "rel": "self",
                            "href": base + "catalog.opds.json",
                            "type": "application/opds+json",
                        }
                    ],
                    "publications": list(publications.values()),
                },
                ensure_ascii=False,
                allow_nan=False,
                indent=2,
            )
            + "\n"
        )
    elif format == "nfo":
        for entry in entries:
            item = items[entry["item_id"]]
            if item["kind"] not in ("movie", "episode"):
                continue
            root = ET.Element("movie" if item["kind"] == "movie" else "episodedetails")
            metadata = item["metadata"]
            ET.SubElement(root, "title").text = xml_text(
                metadata.get("title") or item["id"]
            )
            for field in ("year", "plot", "outline", "tagline", "premiered"):
                if metadata.get(field) is not None:
                    ET.SubElement(root, field).text = xml_text(metadata[field])
            for identity in item["identities"]:
                provider = identity["namespace"].split(".")[0]
                if provider in ("tmdb", "imdb", "tvdb"):
                    ET.SubElement(root, "uniqueid", {"type": provider}).text = xml_text(
                        identity["value"]
                    )
            if item["kind"] == "episode":
                parents = [
                    edge
                    for edge in content["relationships"]
                    if edge["source_id"] == item["id"]
                    and edge["kind"] == "part_of"
                    and items[edge["target_id"]]["kind"] == "season"
                ]
                if len(parents) != 1 or parents[0]["position"] is None:
                    raise CatabolicError(
                        "episode NFO requires one numbered season association"
                    )
                season = parents[0]
                series = [
                    edge
                    for edge in content["relationships"]
                    if edge["source_id"] == season["target_id"]
                    and edge["kind"] == "part_of"
                    and items[edge["target_id"]]["kind"] == "series"
                ]
                if len(series) != 1 or series[0]["position"] is None:
                    raise CatabolicError(
                        "episode NFO requires one numbered series association"
                    )
                ET.SubElement(root, "season").text = str(series[0]["position"])
                ET.SubElement(root, "episode").text = str(season["position"])
            path = str(PurePosixPath(entry["path"]).with_suffix(".nfo"))
            payload = xml(root)
            if path in artifacts and artifacts[path] != payload:
                raise CatabolicError(f"conflicting NFO metadata at {path}")
            artifacts[path] = payload
    else:
        raise CatabolicError("unknown export format")
    if sum(len(value.encode()) for value in artifacts.values()) > MAX_BYTES:
        raise CatabolicError("metadata export exceeds 128 MiB")
    return {
        "format": format,
        "format_version": 1,
        "catalog": content["catalog"]["id"],
        "files": [
            {"path": path, "content": value} for path, value in artifacts.items()
        ],
    }


def publish_bundle(app, document, export, output):
    """Create a fresh detached snapshot. Never replace or adopt an existing path."""
    if app.store.lock_fd is None:
        raise CatabolicError("bundle publication requires the catalog writer lock")
    destination = Path(output).absolute()
    app._validate_binding_path("output", "export", destination)
    entries = document["content"]["entries"]
    artifacts = export["files"] + [
        {
            "path": "catalog-manifest.json",
            "content": json.dumps(document, ensure_ascii=False, allow_nan=False) + "\n",
        }
    ]
    paths = [entry["path"] for entry in entries] + [
        entry["path"] for entry in artifacts
    ]
    keys = set()
    for path in paths:
        relative_path(path)
        if len(path.encode()) > 4096 or any(
            len(part.encode()) > 255 for part in path.split("/")
        ):
            raise CatabolicError(
                "export path exceeds portable filesystem length limits"
            )
        key = path_key(path)
        if key in keys:
            raise CatabolicError(f"export path collision: {path}")
        keys.add(key)
    for path in keys:
        parts = path.split("/")
        if any("/".join(parts[:index]) in keys for index in range(1, len(parts))):
            raise CatabolicError("export file conflicts with a parent directory")
    files = {file["id"]: file for file in document["content"]["files"]}
    sources = {}
    for file in files.values():
        binding = app.binding("source", file["location"])
        with root_handle(binding) as fd:
            source_stat(fd, file["path"])
        sources[file["id"]] = str(Path(binding["root"]) / file["path"])
    parent = open_directory(destination.parent)
    try:
        app._guard_default_parent(parent)
        os.mkdir(destination.name, mode=0o755, dir_fd=parent)
        os.fsync(parent)
        from .filesystem import DIRECTORY_FLAGS

        root = os.open(destination.name, DIRECTORY_FLAGS, dir_fd=parent)
        try:
            for entry in entries:
                source = sources[entry["file_id"]]
                with parent_handle(root, entry["path"], create=True) as (fd, leaf):
                    os.symlink(
                        os.path.relpath(
                            source, destination / PurePosixPath(entry["path"]).parent
                        ),
                        leaf,
                        dir_fd=fd,
                    )
                    os.fsync(fd)
            for artifact in artifacts:
                with parent_handle(root, artifact["path"], create=True) as (fd, leaf):
                    out = os.open(
                        leaf,
                        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                        0o644,
                        dir_fd=fd,
                    )
                    with os.fdopen(out, "w", encoding="utf-8") as stream:
                        stream.write(artifact["content"])
                        stream.flush()
                        os.fsync(stream.fileno())
                    os.fsync(fd)
        finally:
            os.close(root)
    finally:
        os.close(parent)
    return {
        "output": str(destination),
        "format": export["format"],
        "links": len(entries),
        "metadata_files": len(artifacts),
        "written": True,
    }
