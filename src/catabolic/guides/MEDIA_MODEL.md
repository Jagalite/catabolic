# Media beyond movies and TV

Schema 3 separates four concepts: inventoried files, identified content items,
relationships between items, and catalog destinations. The same scanning,
read-only SQL, recoverable synchronization, and verification work across media
types. Source files are never rewritten or decoded by these commands.

## Discover the model

```sh
catabolic --json item types
catabolic --db /path/to/catalog.sqlite3 --json query --schema
```

`item types` needs no database and returns the media kinds, file roles, directed
relationship kinds, and permitted built-in parent types for agents.

| Family | Built-in item kinds |
| --- | --- |
| Film and video | `movie`, `series`, `season`, `episode`, `video`, `music_video` |
| Music | `artist`, `album`, `recording`, `track` |
| Books and audio | `book`, `book_edition`, `audiobook`, `audiobook_chapter` |
| Podcasts | `podcast`, `podcast_episode` |
| Comics | `comic_series`, `comic_volume`, `comic_issue` |
| Photos | `photo`, `photo_album` |
| General | `person`, `collection`, `document`, `other` |

Custom kinds, file roles, and relationships use `custom:lowercase_name`. Names
after the colon start with a letter and can contain lowercase letters, digits,
dots, underscores, and hyphens, up to 63 characters. Metadata is an extensible
JSON object: unknown fields round-trip without being discarded. An existing item's
kind cannot be changed implicitly by reusing an identity.

## Identification independent of placement

With `CATABOLIC_DB` set, identify scanned files before deciding where to put them:

```sh
catabolic item put --id book-1 --kind book --metadata '{"title":"Example Book"}'
catabolic item put --id edition-1 --kind book_edition \
  --metadata '{"title":"Example Book","year":2026,"publisher":"Example"}'
catabolic relationship put --source edition-1 --target book-1 --kind edition_of

catabolic association put --file EPUB_FILE_ID --item edition-1 \
  --metadata '{"format":"epub"}'
catabolic association put --file PDF_FILE_ID --item edition-1 \
  --metadata '{"format":"pdf"}'
catabolic --json item show edition-1
```

An item can have many files. A file can have several explicitly recorded
associations, such as a shared cover or ambiguous legacy identifications. The CLI
does not infer which association is authoritative. Provider IDs remain attached
to items through `item put --identity NAMESPACE=VALUE`.

`association put` identifies one `(file, item, role)` tuple and reuses its ID on
retry. `--role` defaults to `primary`; other built-ins are `cover`, `subtitle`,
`transcript`, `lyrics`, `thumbnail`, `extra`, and `source`. Association metadata
can record format, language, or evidence, independently from item metadata.

```sh
catabolic association put --file SUBTITLE_FILE_ID --item movie-1 --role subtitle \
  --metadata '{"language":"en"}'
catabolic association put --file COVER_FILE_ID --item album-1 --role cover
catabolic --json association list --item edition-1
catabolic --json association list --file FILE_ID --active all
```

`--part N` orders multiple files within one item and role, starting at 1. Active
part numbers are unique within that scope. Alternate complete formats can leave
part null; several unordered primary files are allowed. Omitted part/metadata
options preserve existing values on retry. Use `--clear-part` explicitly to remove
ordering; supplying a metadata object replaces the association's metadata object.

Disable an incorrect identification with `association disable ASSOCIATION_ID`.
Records are retained, and `put` reactivates the same tuple. The last identification
for a file/item pair cannot be disabled while an active catalog mapping uses it.
Disable those mappings first or retain another active association for that pair.
Identification and relationship mutations refuse pending link operations.

## Relationships and ordered media

Relationships point from source to target: child to parent, edition to work,
or content to contributor.

```sh
catabolic item put --id album-1 --kind album --metadata '{"title":"Example Album"}'
catabolic item put --id track-1 --kind track --metadata '{"title":"Opening Track"}'
catabolic relationship put --source track-1 --target album-1 --kind part_of --position 1
catabolic association put --file TRACK_FILE_ID --item track-1

catabolic item put --id audio-1 --kind audiobook --metadata '{"title":"Example Book"}'
catabolic relationship put --source audio-1 --target book-1 --kind edition_of
catabolic item put --id chapter-1 --kind audiobook_chapter --metadata '{"title":"Chapter 1"}'
catabolic relationship put --source chapter-1 --target audio-1 --kind part_of --position 1
catabolic association put --file CHAPTER_FILE_ID --item chapter-1
```

`part_of` supports seasons/episodes, album tracks, audiobook chapters, podcast
episodes, comic volumes/issues, and photos/videos in albums. Any kind can belong
to a general `collection`. `edition_of` supports book editions and audiobooks
pointing to books, and film editions pointing to another movie item.
`recording_of` links a track to a recording. `created_by` and `performed_by` point
to a `person` or `artist`; `narrated_by` applies to audiobooks and their chapters.
`derived_from` connects originals and derivatives, such as RAW and edited photos.

Only `part_of` uses `--position`. A parent's active ordered children cannot share
a position. Disc-local track numbers can be stored as metadata; the relationship
position gives a single playback order across the whole release. Omitted
position/metadata preserve previous values; `--clear-position` removes ordering.
Metadata objects replace rather than merge previous objects when explicitly given.

Self-links are rejected. Structural cycles are rejected across `part_of`,
`edition_of`, `recording_of`, and `derived_from`. Built-in endpoint kinds are
validated. Custom relationship names support caller-defined semantics and do not
automatically participate in structural-cycle checks.

```sh
catabolic --json relationship list --item album-1 --direction incoming --kind part_of
catabolic --json relationship list --item track-1 --direction outgoing
catabolic relationship disable RELATIONSHIP_ID
```

Both listing commands return 100 rows by default, accept `--limit` up to 1000,
and return `next_cursor`. Repeat the same filters with `--cursor`. Associations
sort by part and ID; relationships sort by position and ID, with unordered records
first. `item show` includes the first page of each under `file_associations` and
`relationships`, alongside existing catalog occurrences. Follow those nested
cursors with their corresponding list commands and the same filters.

## Catalog projections and SQL

Identification does not create output links. Choose an explicit destination:

```sh
catabolic mapping put --file TRACK_FILE_ID --item track-1 \
  --path 'Music/Artist/Album/01 - Opening Track.flac'
catabolic sync --dry-run
catabolic sync
catabolic verify
```

Legacy `mapping put` still identifies a file if it has no active association with
that item, atomically with placement. If an association already exists, its role
is preserved. Disabling a mapping now leaves identification intact. `files
--unidentified` filters independent associations; `files --unmapped --catalog
NAME` filters placement. File `--item`, `--kind`, `--year`, and `--identity` filters
use active identification across catalogs.

SQL adds `catalog_item_files` and `catalog_relationships`; existing views retain
their columns and meanings. For example, enumerate identified album tracks even
before assigning destinations:

```sql
SELECT r.position, a.title, a.source_path, a.status
FROM catalog_relationships r
JOIN catalog_item_files a ON a.item_id=r.source_id
WHERE r.target_id=:album AND r.kind='part_of' AND r.active=1
  AND a.active=1 AND a.role='primary' AND a.profile=:profile
ORDER BY r.position, a.association_id;
```

See [QUERYING.md](QUERYING.md) for CLI parameter binding and row semantics.
See [OUTPUT_LAYOUTS.md](OUTPUT_LAYOUTS.md) for generated paths using item/file
metadata and relationship selectors, and [GRAPHQL.md](GRAPHQL.md) for nested queries.
Optional probing, decoding checks, TMDB movie candidates, and reviewed metadata
updates are available through the enrichment workflow. These preserve supplied
curation and do not imply consumer-specific library recognition. See
[ENRICHMENT.md](ENRICHMENT.md).

## Upgrade and preservation

Run `db upgrade --dry-run`, then `db upgrade` for an existing schema-1 or schema-2
database. The runner verifies a backup, rehearses the migration, and commits the
whole chain atomically. New databases initialize at schema 6.

Migration 003 adds tables and indexes. Every distinct historical file/item pair,
including disabled mappings, becomes an active `primary` identification with
origin `migration`. This preserves evidence without guessing more specific roles
or resolving conflicting legacy identifications. Existing mappings, active flags,
IDs, metadata, profile bindings, and owned links remain unchanged. Fresh
associations record origin `explicit` or `mapping`. New relationships are empty
until explicitly supplied. Link-journal recovery remains supported for schemas
1 and 2 before upgrading.

## Tagging

See [TAGGING.md](TAGGING.md) (`catabolic docs tags`) for schema 4 tag tables,
CLI commands, SQL views, GraphQL fields, boolean/descendant filters and generated
tag-selected folders. Tags apply explicitly to items or files and are shared
across profiles. Manifest v2 carries their vocabulary and attributed assertions.
