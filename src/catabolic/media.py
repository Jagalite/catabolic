# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Media vocabulary and relationship validation shared by CLI and application."""

import re

from .domain import CatabolicError

KINDS = {
    "movie": "Film or film edition",
    "series": "Television series",
    "season": "Television season",
    "episode": "Television episode",
    "video": "Personal or general video",
    "music_video": "Music video",
    "artist": "Artist or musical group",
    "person": "Author, narrator, or contributor",
    "album": "Music release",
    "recording": "Musical recording",
    "track": "Track on a release",
    "audiobook": "Audiobook edition",
    "audiobook_chapter": "Chapter of an audiobook",
    "podcast": "Podcast show",
    "podcast_episode": "Podcast episode",
    "book": "Written work",
    "book_edition": "Edition of a written work",
    "comic_series": "Comic series",
    "comic_volume": "Collected comic volume",
    "comic_issue": "Comic issue",
    "photo": "Photograph",
    "photo_album": "Photo or video album",
    "collection": "General collection",
    "document": "General document",
    "other": "Unclassified media",
}
ROLES = (
    "primary",
    "cover",
    "subtitle",
    "transcript",
    "lyrics",
    "thumbnail",
    "extra",
    "source",
)
RELATIONS = {
    "part_of": "Child belongs to parent; position optionally orders siblings",
    "edition_of": "Edition represents a work",
    "recording_of": "Track represents a recording",
    "created_by": "Item was created by a person or artist",
    "performed_by": "Item was performed by a person or artist",
    "narrated_by": "Audiobook or chapter was narrated by a person or artist",
    "derived_from": "Item derives from an original item",
}
STRUCTURAL = ("part_of", "edition_of", "derived_from", "recording_of")
PARENTS = {
    "season": ("series",),
    "episode": ("series", "season"),
    "track": ("album",),
    "audiobook_chapter": ("audiobook",),
    "podcast_episode": ("podcast",),
    "comic_volume": ("comic_series",),
    "comic_issue": ("comic_series", "comic_volume"),
    "photo": ("photo_album",),
    "video": ("photo_album",),
}


def vocabulary(value, allowed, label):
    if (
        not isinstance(value, str)
        or value not in allowed
        and not re.fullmatch(r"custom:[a-z][a-z0-9_.-]{0,62}", value)
    ):
        raise CatabolicError(
            f"unknown {label}: {value}; use a built-in name or custom:lowercase_name"
        )
    return value


def media_kind(value):
    return vocabulary(value, KINDS, "media kind")


def ordinal(value, label):
    if value is not None and (type(value) is not int or not 1 <= value <= 2147483647):
        raise CatabolicError(f"{label} must be an integer between 1 and 2147483647")


def relationship(kind, source, target, position):
    vocabulary(kind, RELATIONS, "relationship kind")
    ordinal(position, "position")
    if position is not None and kind != "part_of":
        raise CatabolicError("only part_of relationships have an ordered position")
    if kind.startswith("custom:") or kind == "derived_from":
        return
    if kind == "part_of":
        valid = target == "collection" or target in PARENTS.get(source, ())
    elif kind == "edition_of":
        valid = (source, target) in (
            ("book_edition", "book"),
            ("audiobook", "book"),
            ("movie", "movie"),
        )
    elif kind == "recording_of":
        valid = source == "track" and target == "recording"
    else:
        valid = target in ("person", "artist")
        if kind == "narrated_by":
            valid = valid and source in ("audiobook", "audiobook_chapter")
    if not valid:
        raise CatabolicError(f"invalid relationship: {source} {kind} {target}")


def describe_types():
    return {
        "kinds": KINDS,
        "file_roles": ROLES,
        "relationships": RELATIONS,
        "part_of_parents": PARENTS,
        "custom_names": "custom:lowercase_name",
        "relationship_direction": "source (child/edition/content) to target (parent/work/contributor)",
    }
