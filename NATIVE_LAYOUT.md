# Catabolic native layout v1

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

The `catabolic` preset defines the `catabolic.native` output layout, version 1.
It applies to every supported media kind and file role, including custom values.
Ordinary filesystem symlinks point to source files; generating this output never
renames or modifies the sources.

## Naming contract

Each active selected file association generates this relative destination:

```text
{item.kind}/{item.id}/{association.role}/{file.id}/{file.name}
```

For example, with illustrative item and file IDs:

```text
catabolic/
  native/
    .catabolic-manifest.json
    movie/movie-1/primary/file-1/Arrival.mkv
    movie/movie-1/subtitle/file-2/Arrival.en.srt
    book_edition/edition-1/primary/file-3/Dune.epub
    book_edition/edition-1/primary/file-4/Dune.pdf
    track/track-1/primary/file-5/Song.flac
    album/album-1/cover/file-6/cover.jpg
    photo/photo-1/primary/file-7/photo.raw
    custom_research_notes/notes-1/primary/file-8/notes.txt
```

The final filename comes from the source basename, with the layout engine's
portable component normalization: NFC Unicode, replacement of slashes,
backslashes, control characters and `:*?"<>|` with underscores, removal of outer
whitespace and trailing dots. This applies to every dynamic component, so
`custom:research_notes` becomes `custom_research_notes`. The manifest retains the
original kind, role and source path; consumers must not reconstruct identity by
reversing filename normalization. Normalization is not byte-for-byte preservation.

IDs are used in full, without shortening. Distinct file IDs keep repeated source
basenames separate, and roles keep different uses of one file separate. Identical
associations yielding the same mapping coalesce. Other collisions, including
case or Unicode aliases, block the plan rather than overwrite anything. Existing
path traversal, reserved-name, component-length and path-length checks apply;
invalid names fail explicitly, without silent truncation or fallback naming.

Title, year, ordering and relationship changes do not affect these paths. Changing
an item's kind or an association's role can change them. File IDs currently
depend on logical source location and source-relative path; source renames can
produce new file IDs. This layout does not add rename detection. Items without
selected file associations do not receive empty directories. Related items and
ordering are represented in metadata, rather than nested album/series folders.

## Version and manifest contract

The executable definition lives in `src/catabolic/layouts.py` and is available
through `catabolic layout presets`. Its identifying field is:

```json
"profile": {"id": "catabolic.native", "version": 1}
```

The profile version identifies naming semantics. The definition's top-level
`version: 1` identifies the layout language; the manifest's `format_version: 3`
identifies the separate Open Catalog metadata contract. These versions evolve
independently. Native v1 rules must remain fixed; changes require a new profile
version and an explicit saved-layout update, preview, apply and synchronization.
Existing saved layouts are not replaced by installing a newer CLI.

The validator refuses unsupported profiles and altered native rules. Remove the
`profile` field to derive a custom layout. SQL or GraphQL selection may limit
membership without changing the native naming profile.

The manifest records the marker at
`content.layout.current_definition.profile`, alongside the current definition,
its hash and the last-applied hash. A marker describes the current saved rules:
consumers must check `definition_matches_last_apply` and entry-level
`managed_by_layout` before attributing entries to those rules. Explicit mappings
may coexist with a layout. The manifest describes desired state and is not a live
filesystem verification or a full database backup.

## CLI workflow

For an initialized database with inventoried, identified media:

```sh
catabolic --db catalog.sqlite3 catalog bind native
catabolic --db catalog.sqlite3 layout put native --preset catabolic
catabolic --db catalog.sqlite3 layout preview native --catalog native
catabolic --db catalog.sqlite3 layout apply native --catalog native
catabolic --db catalog.sqlite3 sync --catalog native --dry-run
catabolic --db catalog.sqlite3 sync --catalog native
catabolic --db catalog.sqlite3 verify --catalog native
catabolic --db catalog.sqlite3 manifest --catalog native --in-catalog
```

The first bind defaults to `./catabolic/native`; `--root` can select another
existing directory. `native` is the catalog name, while `catabolic` is the preset.
Selecting a preset does not rename or relocate another catalog. Plex and custom
projections can coexist beside this one using their own bindings.

Manifest generation is explicit. Repeat the final command with `--replace` after
updating and syncing a changed catalog. A directory without a manifest is still
a native symlink projection, but consumers will lack its rich metadata. See
[MANIFESTS.md](MANIFESTS.md) and [OPEN_CATALOG.md](OPEN_CATALOG.md) for export and
validation semantics. No per-item metadata files are emitted in v1.

The native naming layout also works with hardlink catalogs. Link mechanism is
configured separately with `catalog bind --link-mode`; see [HARDLINKS.md](HARDLINKS.md).
