# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Versioned application output profiles and documented compatibility scope."""

from copy import deepcopy

from .domain import CatabolicError


def rule(name, kinds, roles, path, relations=None, has=None):
    value = {"name": name, "when": {"kinds": kinds, "roles": roles}, "path": path}
    if relations:
        value["relations"] = relations
    if has:
        value["when"]["has"] = has
    return value


def relation(alias, kind, target_kind, source="item"):
    return {"alias": alias, "kind": kind, "target_kind": target_kind, "from": source}


TV = [
    relation("season", "part_of", "season"),
    relation("series", "part_of", "series", "season"),
]
MUSIC = [
    relation("album", "part_of", "album"),
    relation("artist", "performed_by", "artist", "album"),
]


def video_rules(target):
    movie = "{item.title} ({item.year})"
    show = "TV/{series.title}/Season {series.position:02d}/{series.title} - S{series.position:02d}E{season.position:02d}"
    rules = []
    # Different copies require explicit version names, never a random source pick.
    for has, suffix, label in (
        (
            ["edition"],
            " {{edition-{item.metadata.edition}}}"
            if target == "plex"
            else " - {item.metadata.edition}",
            "edition",
        ),
        ([], "", "standard"),
    ):
        base = f"Movies/{movie}{suffix}/{movie}{suffix}"
        rules.extend(
            [
                rule(
                    "movie-" + label,
                    ["movie"],
                    ["primary"],
                    base + "{file.extension}",
                    has=has,
                ),
                rule(
                    "subtitle-" + label,
                    ["movie"],
                    ["subtitle"],
                    base + ".{association.metadata.language}{file.extension}",
                    has=has,
                ),
                rule(
                    "poster-" + label,
                    ["movie"],
                    ["cover"],
                    f"Movies/{movie}{suffix}/poster{{file.extension}}",
                    has=has,
                ),
                rule(
                    "extra-" + label,
                    ["movie"],
                    ["extra"],
                    f"Movies/{movie}{suffix}/Extras/{{file.name}}",
                    has=has,
                ),
            ]
        )
    rules.extend(
        [
            rule("episodes", ["episode"], ["primary"], show + "{file.extension}", TV),
            rule(
                "episode-subtitles",
                ["episode"],
                ["subtitle"],
                show + ".{association.metadata.language}{file.extension}",
                TV,
            ),
        ]
    )
    return rules


def music_rules():
    base = "Music/{artist.title}/{album.title}"
    return [
        rule(
            "tracks",
            ["track"],
            ["primary"],
            base + "/{album.position:03d} - {item.title}{file.extension}",
            MUSIC,
        ),
        rule(
            "lyrics",
            ["track"],
            ["lyrics"],
            base + "/{album.position:03d} - {item.title}{file.extension}",
            MUSIC,
        ),
        rule(
            "covers",
            ["album"],
            ["cover"],
            "Music/{artist.title}/{item.title}/cover{file.extension}",
            [relation("artist", "performed_by", "artist")],
        ),
    ]


def book_rules():
    series = [relation("series", "part_of", "comic_series")]
    return [
        rule(
            "books",
            ["book", "book_edition"],
            ["primary"],
            "Books/{item.metadata.author}/{item.title}/{item.title}{file.extension}",
        ),
        rule(
            "book-covers",
            ["book", "book_edition"],
            ["cover"],
            "Books/{item.metadata.author}/{item.title}/cover{file.extension}",
        ),
        rule(
            "comics",
            ["comic_issue", "comic_volume"],
            ["primary"],
            "Comics/{series.title}/{series.title} #{series.position:03d}{file.extension}",
            series,
        ),
    ]


def audio_rules():
    chapters = [relation("book", "part_of", "audiobook")]
    return [
        rule(
            "books",
            ["audiobook"],
            ["primary"],
            "Audiobooks/{item.metadata.author}/{item.title}/{file.name}",
        ),
        rule(
            "covers",
            ["audiobook"],
            ["cover"],
            "Audiobooks/{item.metadata.author}/{item.title}/cover{file.extension}",
        ),
        rule(
            "chapters",
            ["audiobook_chapter"],
            ["primary"],
            "Audiobooks/{book.metadata.author}/{book.title}/{book.position:03d} - {item.title}{file.extension}",
            chapters,
        ),
        rule(
            "podcasts",
            ["podcast_episode"],
            ["primary"],
            "Podcasts/{show.title}/{show.position:04d} - {item.title}{file.extension}",
            [relation("show", "part_of", "podcast")],
        ),
    ]


def photo_rules():
    return [
        rule(
            "albums",
            ["photo", "video"],
            ["primary"],
            "Photos/{item.metadata.album}/{file.id}-{file.name}",
            has=["album"],
        ),
        rule("photos", ["photo", "video"], ["primary"], "Photos/{item.id}/{file.name}"),
    ]


SOURCES = {
    "plex": "https://support.plex.tv/articles/naming-and-organizing-your-movie-media-files/",
    "jellyfin": "https://jellyfin.org/docs/general/server/media/movies/",
    "emby": "https://emby.media/support/articles/Movie-Naming.html",
    "kodi": "https://kodi.wiki/view/Naming_video_files/Movies",
    "infuse": "https://support.firecore.com/hc/en-us/articles/215090947-Metadata-101",
    "navidrome": "https://www.navidrome.org/docs/usage/library/tagging/",
    "mpd": "https://mpd.readthedocs.io/en/stable/user.html",
    "gonic": "https://github.com/sentriz/gonic/blob/master/README.md",
    "airsonic": "https://airsonic.github.io/docs/first-start/",
    "audiobookshelf": "https://audiobookshelf.org/docs/documentation/libraries/book-library/directory-structure/",
    "komga": "https://komga.org/es/docs/guides/scan-analysis-refresh/",
    "kavita": "https://wiki.kavitareader.com/guides/scanner/managefiles/",
    "ubooquity": "https://vaemendis.net/ubooquity/",
    "stump": "https://github.com/stumpapp/stump",
    "photoprism": "https://docs.photoprism.app/user-guide/library/index.html",
    "photoview": "https://github.com/photoview/photoview",
    "piwigo": "https://doc.piwigo.org/self-hosting-piwigo/importing-and-synchronizing-ftp-photos/",
    "calibre": "https://manual.calibre-ebook.com/generated/en/calibredb.html",
    "calibre-web": "https://github.com/janeczku/calibre-web",
    "immich": "https://docs.immich.app/features/command-line-interface/",
}


def definitions():
    values = {}
    for target in SOURCES:
        if target in ("calibre", "calibre-web", "immich"):
            continue
        if target in ("plex", "jellyfin", "emby", "kodi", "infuse"):
            rules = video_rules(target) + (music_rules() if target != "infuse" else [])
        elif target in ("navidrome", "mpd", "gonic", "airsonic"):
            rules = music_rules()
        elif target == "audiobookshelf":
            rules = audio_rules()
        elif target in ("komga", "kavita", "ubooquity", "stump"):
            rules = book_rules()
        else:
            rules = photo_rules()
        for entry in rules:
            kinds, roles = entry["when"]["kinds"], entry["when"]["roles"]
            if roles == ["cover"]:
                extensions = [".jpg", ".jpeg", ".png"]
            elif roles == ["subtitle"]:
                extensions = [".srt", ".ass", ".ssa", ".vtt", ".sub", ".idx"]
            elif roles == ["lyrics"]:
                extensions = [".lrc", ".txt"]
            elif "photo" in kinds:
                extensions = [".jpg", ".jpeg", ".png", ".gif", ".webp", ".mp4", ".mov"]
            elif "book" in kinds or "comic_issue" in kinds:
                extensions = (
                    [".epub", ".pdf"] if "book" in kinds else [".cbz", ".cbr", ".pdf"]
                )
            elif any(
                kind in kinds
                for kind in (
                    "track",
                    "audiobook",
                    "audiobook_chapter",
                    "podcast_episode",
                )
            ):
                extensions = [
                    ".mp3",
                    ".flac",
                    ".m4a",
                    ".m4b",
                    ".ogg",
                    ".opus",
                    ".wav",
                    ".aac",
                ]
            else:
                extensions = [
                    ".mkv",
                    ".mp4",
                    ".m4v",
                    ".avi",
                    ".mov",
                    ".webm",
                    ".mpg",
                    ".mpeg",
                    ".ts",
                ]
            entry["when"]["extensions"] = extensions
        values[target] = {
            "version": 1,
            "profile": {"id": "catabolic.target." + target, "version": 1},
            "rules": rules,
        }
        if target == "piwigo":
            values[target]["normalization"] = "ascii"
    return values


def describe(target=None):
    if target is not None and target not in SOURCES:
        raise CatabolicError(f"unknown target: {target}; use target list")
    profiles = definitions()
    entries = []
    for identifier, source in SOURCES.items():
        if target is not None and target != identifier:
            continue
        definition = profiles.get(identifier)
        entries.append(
            {
                "id": identifier,
                "version": 1,
                "mechanism": "layout" if definition else "import",
                "preset": ("plex-v1" if identifier == "plex" else identifier)
                if definition
                else None,
                "definition": deepcopy(definition),
                "supported_kinds": sorted(
                    {
                        kind
                        for rule in definition["rules"]
                        for kind in rule["when"]["kinds"]
                    }
                )
                if definition
                else (
                    ["photo", "video"]
                    if identifier == "immich"
                    else ["book", "book_edition"]
                ),
                "documentation": source,
                "supported_roles": sorted(
                    {
                        role
                        for rule in definition["rules"]
                        for role in rule["when"]["roles"]
                    }
                )
                if definition
                else ["primary"],
                "consumer_api": {
                    "available": identifier in ("plex", "jellyfin"),
                    "command": "consumer"
                    if identifier in ("plex", "jellyfin")
                    else None,
                    "evidence": "protocol fixtures; live scanner acceptance separate"
                    if identifier in ("plex", "jellyfin")
                    else "naming/export support only",
                },
                "tested_application_versions": [],
                "validation": "Catabolic fixtures; application scan unverified",
                "notes": {
                    "navidrome": "Embedded audio tags remain authoritative; paths cannot repair tags.",
                    "immich": "Uploads selected originals via the official Immich CLI; does not create an external symlink library.",
                    "calibre-web": "Uses calibredb to import into the local calibre library used by Calibre-Web.",
                    "gonic": "One album per folder; multi-disc releases need globally unique positions or custom rules.",
                    "piwigo": "Strict ASCII names and physical-album synchronization; RAW derivatives are outside v1.",
                }.get(
                    identifier,
                    "Only listed kinds and roles are projected; review skipped associations and conflicts.",
                ),
            }
        )
    return {"interface_version": 1, "targets": entries}
