# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Deterministic fake media. Every path is created beneath a fresh fixture root."""

from __future__ import annotations

import hashlib
import json
from dataclasses import dataclass
from pathlib import Path

from catabolic.app import Application
from catabolic.store import Store

MARKER = ".catabolic-synthetic-fixture.json"
EXCLUSIONS = [".fixture-system"]


@dataclass(frozen=True)
class Media:
    location: str
    relative: str
    destination: str
    kind: str
    identity: str


@dataclass
class SyntheticLibrary:
    root: Path
    sources: dict[str, Path]
    entries: list[Media]

    @property
    def database(self) -> Path:
        return self.root / "catalog.sqlite3"

    @property
    def output(self) -> Path:
        return self.root / "plex"

    @classmethod
    def create(cls, root: Path, count: int) -> SyntheticLibrary:
        if count < 10:
            raise ValueError("use at least 10 fixture media files")
        # Existing directories are never reused, even when they look empty.
        root = root.absolute()
        root.mkdir()
        root = root.resolve()
        sources = {f"drive-{number}": root / f"source-{number}" for number in (1, 2)}
        for source in sources.values():
            source.mkdir()
        (root / "plex").mkdir()
        outside = root / "outside"
        outside.mkdir()
        (outside / "must-not-be-scanned.mkv").write_bytes(b"OUTSIDE THE SOURCE TREE")
        entries = []
        for index in range(count):
            location = f"drive-{index % 2 + 1}"
            if index % 5 == 0:
                title = f"Café 宇宙 {index:06d}"
                relative = f"releases/movies/{title} (2025)/feature.mp4"
                destination = f"Movies/{title} (2025)/{title} (2025).mp4"
                kind, identity = "movie", f"movie-{index}"
            else:
                series = index // 20
                extension = "srt" if index % 10 == 9 else "mkv"
                episode = index % 20 if extension == "srt" else index % 20 + 1
                relative = f"releases/series-{series:05d}/season-01/S01E{episode:02d}.{extension}"
                destination = f"TV Shows/Series {series:05d}/Season 01/Series {series:05d} - S01E{episode:02d}.{extension}"
                kind, identity = "series", f"series-{series}"
            media = Media(location, relative, destination, kind, identity)
            path = sources[location] / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            payload = f"FAKE MEDIA {index:08d}\n".encode()
            path.write_bytes((payload * (1024 // len(payload) + 1))[:1024])
            entries.append(media)
        for source in sources.values():
            (source / "empty.mkv").write_bytes(b"")
            (source / "download.mkv.PART").write_bytes(b"INCOMPLETE DOWNLOAD")
            (source / "external-link.mkv").symlink_to(
                outside / "must-not-be-scanned.mkv"
            )
            (source / "loop").symlink_to(source, target_is_directory=True)
            system = source / ".fixture-system"
            system.mkdir()
            (system / "excluded.mkv").write_bytes(b"EXCLUDED FROM SCAN")
        initialized = Store.initialize(root / "catalog.sqlite3")
        (root / MARKER).write_text(
            json.dumps(
                {
                    "format": 1,
                    "database_id": initialized["database_id"],
                    "media_files": count,
                },
                indent=2,
            )
        )
        library = cls(root, sources, entries)
        with Store(library.database, writable=True) as store:
            app = Application(store)
            for location, source in sources.items():
                app.bind("source", location, str(source))
            app.bind("output", "global", str(library.output))
        return library

    def source(self, entry: Media) -> Path:
        return self.sources[entry.location] / entry.relative

    def hashes(self) -> dict[str, str]:
        return {
            str(self.source(entry).relative_to(self.root)): hashlib.sha256(
                self.source(entry).read_bytes()
            ).hexdigest()
            for entry in self.entries
        }

    def inventory(self, app: Application) -> dict[tuple[str, str], dict]:
        files = {}
        cursor = None
        while True:
            page = app.files(limit=257, cursor=cursor)
            for row in page["files"]:
                key = row["location"], row["path"]
                if key in files:
                    raise AssertionError("duplicate occurrence while paginating")
                files[key] = row
            cursor = page["next_cursor"]
            if cursor is None:
                return files

    def seed_decisions(self, app: Application, files: dict, progress=None) -> dict:
        items = {}
        mappings = {}
        for index, entry in enumerate(self.entries, 1):
            if entry.identity not in items:
                items[entry.identity] = app.put_item(
                    entry.kind,
                    {f"synthetic.{entry.kind}": entry.identity},
                    {"title": entry.identity},
                )["id"]
            row = files[entry.location, entry.relative]
            mappings[entry.destination] = app.put_mapping(
                "global", row["id"], items[entry.identity], entry.destination
            )
            if progress and index % 1000 == 0:
                progress(index)
        return mappings
