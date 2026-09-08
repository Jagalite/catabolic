# Query-driven symlink folders

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Query folders are the legacy embedded-selection form of a [projection](PROGRAMMABLE_CATALOG.md). New `query save` and `projection put` commands let multiple outputs share one immutable query and reuse layout templates; the commands below remain supported.


A layout can save a SQL or GraphQL selection. The query decides membership; the
layout names the selected files; a catalog binding chooses the output folder.
This supports folders such as recent films, a reading list, an artist's recordings,
or files matching custom metadata without duplicating the source media.

## SQL example

Save `recent-movies.sql`:

```sql
SELECT item_id
FROM catalog_items
WHERE kind = 'movie' AND year >= :since
```

Bind an existing output directory and save the query with a naming layout:

```sh
catabolic --db catalog.sqlite3 catalog bind recent --root /absolute/path/to/Recent
catabolic --db catalog.sqlite3 layout put recent --preset flat \
  --select-sql recent-movies.sql --params '{"since":2020}'
catabolic --db catalog.sqlite3 --json layout preview recent --catalog recent
catabolic --db catalog.sqlite3 --json layout apply recent --catalog recent
catabolic --db catalog.sqlite3 --json sync --catalog recent --dry-run
catabolic --db catalog.sqlite3 --json sync --catalog recent
catabolic --db catalog.sqlite3 --json verify --catalog recent
```

`--preset plex` can be used when the selected media has the required Plex naming
metadata. `--file layout.json` uses custom naming rules instead. Query input
supports `--select-sql -` or `--select-graphql -` for stdin. A command can consume
stdin for only one input. Query text and parameters are saved inside the layout;
subsequent edits to the input file take effect only after another `layout put`.

SQL must return exactly one named column containing nonempty string IDs:

| Column | Projection |
| --- | --- |
| `item_id` | All active file associations for those items |
| `file_id` | All active identifications of those files |
| `association_id` | Exactly those active associations, preserving item/role selection |

Naming-rule filters still apply after selection. Duplicate IDs are coalesced;
unknown IDs and disabled association IDs fail the entire refresh. Items or files
without active associations produce no mappings. Queries do not invent identities.
To choose particular roles, file formats, or recorded availability, select
association IDs:

```sql
SELECT association_id
FROM catalog_item_files
WHERE profile = :profile AND active = 1
  AND kind = 'movie' AND role = 'primary' AND status = 'present'
```

Queries have the existing read-only SQL restrictions and support joins, CTEs,
metadata expressions, and named scalar parameters. `:profile` is reserved and
bound automatically. The profile is saved with the selection using
`--selection-profile NAME`, defaulting to `default`. It is independent of the
profile used to refresh or synchronize the output. Views contain all profiles,
so queries using profile-dependent views should explicitly filter `:profile`.

## GraphQL example

Save `tracks.graphql`:

```graphql
query Tracks($after: String, $kind: String!) {
  chosen: items(kind: $kind, first: 100, after: $after) {
    nodes { id }
    pageInfo { hasNextPage endCursor }
  }
}
```

```sh
catabolic --db catalog.sqlite3 layout put tracks --preset flat \
  --select-graphql tracks.graphql --variables '{"kind":"track"}'
```

Bind a catalog and preview/apply/sync it as above. The selector automatically
follows every page in the same database snapshot. `first` is the batch size,
not a limit on folder membership. The query must declare `$after: String`, pass
`after: $after`, and return one root `items`, `files`, or `associations` collection.
An alias for that root is optional. Return exactly `nodes { id }` and
`pageInfo { hasNextPage endCursor }`, without nested aliases or directives.
Fragments and multiple operations are excluded from the saved selection contract;
the ordinary GraphQL query command retains its broader query support.
`after` is reserved and cannot be supplied in saved variables.

## Full definition

Selections can also be embedded in a JSON layout:

```json
{
  "version": 1,
  "selection": {
    "language": "sql",
    "query": "SELECT item_id FROM catalog_items WHERE kind = :kind",
    "params": {"kind": "book_edition"},
    "profile": "default",
    "timeout_ms": 5000
  },
  "rules": [
    {"name": "books", "path": "{item.title}/{file.name}"}
  ]
}
```

For GraphQL, use `"language": "graphql"`, a pageable query document, and
`"variables"` instead of `"params"`. Layout `show`/`list` return the saved definition.
Selection is optional; existing layouts continue to use every active association.

## Refresh and preservation

A refresh is explicit: run layout preview/apply, then sync preview/apply and
verify. Scans, metadata edits, and query-file edits do not automatically change
links. Apply recomputes the query and naming plan while holding the writer lock,
in the same transaction snapshot used for mapping changes. SQL remains protected
by its read-only authorizer and `query_only` mode even during that transaction.

New matches create or re-enable desired mappings. Results that no longer match
disable only mappings owned by this layout. Sync later removes those owned links;
it does not remove source files, identifications, historical mappings, or manually
placed mappings. Existing filesystem ownership and recovery checks still apply.
A changed selection can legitimately retire many links: review the plan counts.
Avoid queries that depend on this same output's generated mappings (such as its
own unmapped files), because changing that output can change the next query result.

A query error, invalid ID, timeout, incomplete GraphQL page, or truncated SQL
result aborts the whole refresh. None is treated as an empty selection.
A valid query-driven plan that would retire every active generated mapping is
blocked unless `--allow-empty` is passed to preview/apply. This also covers a
nonempty selection whose naming rules match nothing. The override permits a
complete empty result; it does not override query errors or collision blockers.
An initially empty folder is allowed without that flag.

Plans add a `selection` report with language, fixed profile, selected ID/association
counts, page/row counts, and completeness. `--limit` still limits displayed plan
details only. Query errors exit 2 with the CLI error envelope; blocked empty plans
and naming collisions exit 3 with `safe: false`. Successful apply records mappings;
filesystem checks still happen in `sync`.

Selection execution accepts at most 10,000 returned SQL rows or GraphQL nodes
before deduplication, expanding to at most 100,000 active associations. SQL and
GraphQL retain their input/output and per-query limits. `timeout_ms` defaults to
5000 (1–60000); for GraphQL it covers the entire pagination loop. These are
cooperative execution limits, not hard process-kill deadlines. Large collections
can be split into separately managed catalogs. No database migration is needed;
query definitions are included in existing layout metadata and database backups.

## Metadata alongside the links

After synchronization, `manifest --catalog NAME --in-catalog` writes a versioned
JSON metadata snapshot beside the links. It includes source/item metadata and
the saved layout/query definition. Use `--replace` for a changed export.
See [MANIFESTS.md](MANIFESTS.md) for contents and refresh semantics.

## Tagging

See [TAGGING.md](TAGGING.md) (`catabolic docs tags`) for schema 4 tag tables,
CLI commands, SQL views, GraphQL fields, boolean/descendant filters and generated
tag-selected folders. Tags apply explicitly to items or files and are shared
across profiles. Manifest v2 carries their vocabulary and attributed assertions.
