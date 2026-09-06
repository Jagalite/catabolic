# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Manifest v3 records the output mode and regular-file ownership separately."""

from typing import Literal

from pydantic import Field

from . import v1, v2
from .v1 import FORMAT as FORMAT
from .v1 import Id, Nonnegative, Record, SizeText

VERSION = 3
SCHEMA_ID = "urn:catabolic:open-catalog:manifest:3"


def label(description):
    return Field(description=description, json_schema_extra={"x-since": 3})


class Catalog(v1.Catalog):
    """An explicitly selected link mechanism shared across profiles."""

    link_mode: Literal["symlink", "hardlink"] = label(
        "Desired link mechanism; symlink is the default for existing catalogs."
    )


class Hardlink(Record):
    """Recorded ownership, not proof of live existence or current source identity."""

    path: Id = label(
        "Relative regular-file output path; retained records use reserved internal paths."
    )
    file_id: Id = label(
        "Original source occurrence ID, possibly outside this active projection."
    )
    location: Id = label("Source location at link creation.")
    source_path: Id = label("Source-relative path at link creation.")
    device: SizeText = label(
        "Recorded filesystem device as decimal text; machine-specific."
    )
    inode: SizeText = label(
        "Recorded inode as decimal text; machine-specific and not content identity."
    )


class RetainedHardlink(Hardlink):
    """Retired output data kept outside the visible library; never automatically purged."""

    id: Id = label("Retirement operation ID.")
    original_path: Id = label("Previous output path.")
    retained_at: str = label("Time the retirement was committed to the database.")


class Counts(v2.Counts):
    """Exact lengths of all twelve collections."""

    hardlinks: Nonnegative = label("Number of recorded regular-file outputs.")
    retained_hardlinks: Nonnegative = label(
        "Number of recorded retained regular-file outputs."
    )


class Content(v2.Content):
    """Desired catalog with independently typed symlink and hardlink ownership."""

    catalog: Catalog = label("Output catalog and explicit link mode.")
    hardlinks: list[Hardlink] = label(
        "Recorded hardlink outputs, including obsolete paths awaiting retirement."
    )
    retained_hardlinks: list[RetainedHardlink] = label(
        "Recorded retirement locations; no live filesystem verification is implied."
    )
    counts: Counts = label("Exact lengths of all twelve collections.")


class Document(v2.Document):
    """Manifest v3; v1 and v2 remain frozen and readable."""

    format_version: Literal[3] = label("Wire-format version; no implicit downgrade.")
    content: Content = label(
        "Checksummed catalog snapshot, tagging and typed output ownership."
    )
