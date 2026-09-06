# Output layouts

Layouts turn active file identifications into desired catalog paths. An optional
saved SQL or GraphQL selection limits membership; see [QUERY_FOLDERS.md](QUERY_FOLDERS.md). They are
versioned JSON definitions, independent of machine profiles and output directory
bindings. A library can project the same sources into several catalogs with
different naming conventions. Layouts do not infer media identities or fetch
metadata. An agent or user supplies those decisions through the catalog commands.

## Workflow

Bind an existing, empty output directory, identify files, then choose a layout:

```sh
catabolic --db catalog.sqlite3 catalog bind plex --root /absolute/path/to/plex
catabolic --db catalog.sqlite3 association put --file FILE_ID --item ITEM_ID
catabolic --db catalog.sqlite3 layout put plex --preset plex
catabolic --db catalog.sqlite3 --json layout preview plex --catalog plex
catabolic --db catalog.sqlite3 --json layout apply plex --catalog plex
catabolic --db catalog.sqlite3 --json sync --catalog plex --dry-run
catabolic --db catalog.sqlite3 --json sync --catalog plex
catabolic --db catalog.sqlite3 --json verify --catalog plex
```

`FILE_ID` and `ITEM_ID` are existing catalog identifiers. The Plex preset includes
movie primary files and language-tagged subtitles, episodes linked to seasons and
series, and tracks/artwork linked to albums and artists. Required metadata must
already exist. These are starter naming rules, not a guarantee that Plex will
recognize every media variant. Multi-disc, multiple movie editions, and other
application conventions should use custom rules and appropriate metadata.

`layout presets` lists the complete `plex` and `flat` definitions without opening
a database. `flat` covers all media kinds and roles using
`{item.kind}/{item.id}/{file.name}`. Duplicate basenames for the same item can still
collide; preview reports that conflict rather than choosing a source arbitrarily.

`layout list` and `layout show NAME` inspect saved definitions. To save a custom
layout, use `layout put NAME --file layout.json`; `--file -` reads stdin. Definition
files are capped at 1 MiB. Replacing a saved definition does not change mappings
until `layout apply` runs. Layouts and mappings are shared across profiles;
source availability and bound output paths remain profile-specific.

## Custom definition

```json
{
  "version": 1,
  "rules": [
    {
      "name": "album-tracks",
      "when": {"kinds": ["track"], "roles": ["primary"]},
      "relations": [
        {"alias": "album", "kind": "part_of", "target_kind": "album"},
        {"alias": "artist", "from": "album", "kind": "performed_by", "target_kind": "artist"}
      ],
      "path": "Audio/{artist.title}/{album.title}/{album.position:02d} - {item.title}{file.extension}"
    },
    {
      "name": "everything-else",
      "path": "Archive/{item.kind}/{item.id}/{file.name}"
    }
  ]
}
```

Rules run in array order. The first matching rule handles an association. An
association without a matching rule is counted as skipped. Missing template data
in a matching rule blocks the plan; it does not fall through to another rule.
Several associations producing the same file/item/path mapping are coalesced.

`when` supports the following optional filters, combined with AND:

| Field | Meaning |
| --- | --- |
| `kinds` | Nonempty array of built-in or `custom:name` item kinds |
| `roles` | Nonempty array of file roles |
| `has` | Nonempty array of top-level item metadata keys whose values are not null |
| `metadata` | Object of exact top-level item metadata values to match |

A relation selector starts at `item` by default, or at an earlier alias using
`from`. It specifies `kind`, optional `target_kind`, and `direction` (`outgoing`
by default, or `incoming`). `target_kind` always filters the related item, including
for incoming edges. Exactly one active relationship must match. Its related item
becomes the alias; `alias.position` is the relationship's ordering position.
In the example, `album.position` is the track's number within the album.

## Template fields

| Field | Value |
| --- | --- |
| `item.id`, `item.kind` | Stable item identifier and media kind |
| `item.title`, `item.year` | Item metadata title and year |
| `item.metadata.KEY` | Scalar value under a metadata key; dotted nested objects supported |
| `file.id`, `file.location` | File and source-location identifiers |
| `file.name`, `file.stem` | Original basename, or basename without its final extension |
| `file.extension` | Final extension including its dot; empty for extensionless files |
| `file.path` | Source-relative path, preserving source subdirectories |
| `association.role`, `association.part` | File role and optional part number |
| `association.metadata.KEY` | Association metadata such as language, disc, or format |
| `ALIAS.title`, `ALIAS.metadata.KEY`, `ALIAS.position` | Selected relationship item data and edge order |

Templates use `{field}` placeholders, optional small integer formats such as
`{association.part:02d}`, and `{{`/`}}` for literal braces. Field access traverses
JSON dictionaries only. It cannot execute Python, access object attributes,
index arrays, call functions, or run shell commands. Keys with dots in their
names cannot be addressed literally; dots mean nested dictionary traversal.

Dynamic values are normalized to NFC. Slashes, backslashes, control characters,
and common filesystem-reserved punctuation become underscores; leading/trailing
whitespace and trailing dots are removed. `file.path` sanitizes each component
while preserving its directory separators. Empty components, null/missing values,
non-scalar values, absolute paths, traversal, and `.catabolic` reserved components
are rejected. Generated paths are capped at 4096 UTF-8 bytes and 255 bytes per
component. Case/Unicode aliases and file-versus-directory collisions block the
plan conservatively even on case-sensitive filesystems.

## Ownership, application, and recovery

Preview is read-only and evaluates recorded catalog state; it does not inspect
output directories. Apply recomputes the plan under the catalog writer lock and
updates mappings in one SQLite transaction. Any blocker leaves all mappings and
layout ownership unchanged. Apply only records desired state. `sync --dry-run`
then checks current source availability, output ownership, and filesystem conflicts;
`sync` performs the existing journaled, recoverable link operations.

A catalog has at most one managing layout. Explicit mappings can coexist where
paths do not conflict. Layout apply never adopts explicit mappings, including an
identical disabled mapping. It disables only mappings that the layout previously
owned and no longer generates; historical records and source identifications are
retained. Repeated application is idempotent. To switch the managing layout, pass
`--replace-layout` to preview and apply. This explicitly transfers management of
the catalog's generated mappings while retaining their history.

Editing item metadata or relationships does not automatically regenerate paths.
Run preview/apply again. Removing an association requires disabling its active
mappings first, as with the existing manual workflow. Pending link operations
must be recovered before layout planning/application. Source files are never
renamed, moved, or deleted by layouts or synchronization.

JSON plans include `safe`, `applied`, desired/skipped/unchanged counts, total
change and blocker counts, and detailed changes/blockers. `--limit` (1–1000,
default 100) only limits displayed details, not the applied plan;
`details_truncated` reports clipping. Exit 3 means a blocked plan, exit 2 an input
or operational error, and exit 0 a successful preview/application. Always inspect
`skipped_associations` when using selective rules.

Definitions support at most 100 rules and eight relation selectors per rule.
A plan supports up to 100,000 active associations; query selections have additional
complete-result limits documented in QUERY_FOLDERS.md. Definitions and managed mapping
IDs use namespaced records in the existing `meta` table, so this feature does not
require a new database migration. The layout definition format has its own version;
unsupported versions are rejected. Database backups include definitions and ownership.

Planning evaluates SQL/GraphQL selection and association limits before loading
item metadata. It processes metadata in batches of 1,000 associations and resolves
only relationships requested by matching rules, including incoming and chained
selectors. Ambiguous matches are counted in SQL before related metadata is loaded.
All reads use the caller's database snapshot; apply retains its existing transaction.

Destination and ownership checks still cover the complete output catalog, including
conflicts across batches. The selected associations, desired mappings, and conflict
state remain in memory, so batching bounds graph loading rather than the entire
plan. Existing limits, CLI commands, and definition formats are unchanged.

## Metadata alongside the links

After synchronization, `manifest --catalog NAME --in-catalog` writes a versioned
JSON metadata snapshot beside the links. It includes source/item metadata and
the saved layout/query definition. Use `--replace` for a changed export.
See [MANIFESTS.md](MANIFESTS.md) for contents and refresh semantics.
