# GraphQL querying

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

`catabolic graphql` executes read-only GraphQL documents locally against one
SQLite snapshot. It returns JSON for agents and scripts without requiring a
server, network port, or service process. SQL remains available through `query`
for joins, aggregation, and ad hoc analysis. GraphQL provides typed, nested access
to the catalog model.

```sh
catabolic --db catalog.sqlite3 graphql '{ items(kind: "movie", first: 20) { nodes { id title year } pageInfo { hasNextPage endCursor } } }'
catabolic graphql --schema
```

`--schema` returns the SDL schema, interface version, and limits without opening
a database. Standard GraphQL introspection is also supported during queries.
Parsing, validation, fragments, variables, aliases, and operation selection use
[graphql-core](https://graphql-core-3.readthedocs.io/en/stable/intro.html), pinned
in the Python package dependencies. No mutation or subscription schema is exposed.

To generate a symlink folder from a saved, fully paginated selection, see
[QUERY_FOLDERS.md](QUERY_FOLDERS.md).

## Query files and variables

Save the following as `album.graphql`:

```graphql
query Album($id: ID!, $after: String) {
  item(id: $id) {
    id
    title
    relationships(kind: "part_of", direction: INCOMING, first: 50, after: $after) {
      nodes {
        position
        source {
          id
          title
          associations(role: "primary", first: 20) {
            nodes {
              file { id path sourcePath status size mtimeNs }
            }
            pageInfo { hasNextPage endCursor }
          }
        }
      }
      pageInfo { hasNextPage endCursor }
    }
  }
}
```

```sh
catabolic --db catalog.sqlite3 --profile default graphql \
  --file album.graphql --variables '{"id":"ALBUM_ID"}' --operation-name Album
cat album.graphql | catabolic --db catalog.sqlite3 graphql \
  --file - --variables '{"id":"ALBUM_ID"}'
```

`ALBUM_ID` is an existing item identifier. Supply exactly one inline document or
`--file` (`-` reads stdin). Variables must be a JSON object. Select
`--operation-name` when a document contains several named operations.
Global database/profile options go before `graphql`.

## Data and filtering

Items expose `workflow`, `worklog`, and `workflowChecks`. Filter items with
`curationStatus` (`PENDING`, `IN_PROGRESS`, `COMPLETE`, `DEFERRED`, `IGNORED`,
`NEEDS_ATTENTION`). Readiness is evaluated from recorded evidence. See
[Entry worklog](WORKLOG.md) for completion rules and examples.

Root fields are `items`, `item`, `files`, `file`, `associations`, `relationships`,
`mappings`, `catalogs`, `profile`, `schemaVersion`, and `mediaTypes`. Items expose
identities, metadata, file associations, relationships, and output mappings.
Relationships expose their source and target items; associations and mappings
expose both their file and item. Files expose recorded availability and source
paths; catalogs and mappings expose profile-specific output paths.

Generated media also exposes `artifact`, `artifacts`, `recipe`, and `recipes`.
`rendition` and `renditions` cover generated and externally registered renditions;
`outputDefinition` and `outputDefinitions` expose their immutable catalog policies.
Single-record fields take `id`; lists use `first`/`after` and return `EvidencePage`
with JSON `nodes`. `renditions(file:ID)` filters by source file ID. Renditions and
artifacts are profile-scoped; recipe and output definitions are catalog-wide.
See [Generated media](ARTIFACTS.md) for provenance semantics and examples.

List filters mirror the existing CLI query layer:

| Query | Filters and ordering |
| --- | --- |
| `items` | `search`, `kind`, `year`, `identity`, `metadata`, `catalog`; sort `ID`, `TITLE`, `YEAR` |
| `files` | `search`, `location`, `status`, `unidentified`, `unmapped`, `catalog`, `item`, `kind`, `year`, `identity`; sort `ID`, `PATH`, `SIZE`, `MTIME` |
| `associations` | `item`, `file`, `role`, `active`; ordered by part then ID |
| `relationships` | `item`, `direction`, `kind`, `active`; ordered by position then ID |
| `mappings` | `catalog`, `allCatalogs`, `item`, `file`, `active`, `search`; sort `ID`, `PATH`, `CATALOG` |

Search is a literal, case-insensitive substring. Identity filters use
`"namespace=value"`. Metadata filters are strings such as `"language=\"en\""`,
with the right side parsed as JSON, matching the CLI's `--metadata KEY=JSON_VALUE`.
`item` requires exactly one `id` or `identity`; an absent match returns null.
`file(id: ...)` likewise returns null when absent.

`Active` values are `ACTIVE`, `DISABLED`, and `ALL`. Associations and relationships
default to active records; mappings default to all records in `global`. Pass
`allCatalogs: true` for mappings across catalogs. Relationships support
`INCOMING`, `OUTGOING`, and `BOTH` (default). Custom kinds and roles remain strings
so new vocabularies do not require a GraphQL schema release.

Pages contain `nodes` and `pageInfo { hasNextPage endCursor }`. Pass the returned
cursor as `after` with the same filters, sort, database, and profile. Cursors are
context-bound keyset cursors; changing the query scope rejects the cursor.
`first` defaults to 100 and accepts 1–1000. Every nested collection has its own
page and cursor. Individual CLI invocations have a consistent snapshot; pages
fetched by later invocations can reflect intervening catalog changes.

Status values (`PRESENT`, `MISSING`, `UNKNOWN`) come from recorded scans for the
selected profile. Reading does not probe, scan, or modify media. Unbound paths and
unobserved sizes/times are null. Byte sizes and nanosecond timestamps use `BigInt`,
serialized as decimal strings to avoid JavaScript precision loss. Arbitrary
metadata uses the `JSON` scalar. `title` and `year` convenience fields are null
when metadata has an incompatible type; original metadata remains available.

## Limits and errors

Documents and variables each have a 128 KiB input cap. Execution allows at most
4000 document tokens, 2000 expanded selections, depth 20, 10,000 returned records
(including repeated nested lookups and introspection lists), 20,000 resolved
fields, and an 8 MiB compact JSON output budget. Reused fragments and aliases
are included in the limits. Caches avoid repeated database lookups within a
request; repeated output still consumes the budget.

`--timeout-ms` defaults to 5000, accepts 1–60000, and is checked by resolvers and
SQLite's progress handler. This is a cooperative execution deadline, not a hard
process-kill timer. Use the caller's process timeout when a hard wall-clock limit
is required. Resource exhaustion discards partial data and returns
`LIMIT_EXCEEDED` when detected by the runtime budget checks. Syntax, schema,
fragment, and variable validation also reject unsuitable documents before access.

GraphQL results always go to stdout as JSON, with standard `data` and optional
`errors`. Successful execution includes `extensions` with interface version,
profile, and record count. Query/validation errors exit 2; resolver errors may
include partial data alongside errors. Input-file, database, and operational
errors use the CLI's JSON error envelope on stderr and exit 2. Argument parsing
uses argparse's usual text errors. Exit 0 means no GraphQL errors were reported.
Agents should check the exit code, `errors`, and every requested page's
`hasNextPage` before considering a result complete.

The public GraphQL interface starts at version 1 and is separate from SQLite's
schema version. Additive fields can preserve interface compatibility; future
breaking API changes require an explicit versioning decision. This feature
introduces no database migration.

## Tagging

See [TAGGING.md](TAGGING.md) (`catabolic docs tags`) for schema 4 tag tables,
CLI commands, SQL views, GraphQL fields, boolean/descendant filters and generated
tag-selected folders. Tags apply explicitly to items or files and are shared
across profiles. Manifest v2 carries their vocabulary and attributed assertions.

For output link modes, hardlink ownership, retained data and manifest v3, see
[HARDLINKS.md](HARDLINKS.md) (`catabolic docs hardlinks`).

## Enrichment

`File.facts` exposes recorded processing results. `jobs` and `proposals` return
context-paginated summary pages; `job(id:)` and `proposal(id:)` return full JSON
records. `contentSearch(text:,first:,after:)` returns word matches with locators.
These fields are read-only; see [ENRICHMENT.md](ENRICHMENT.md) for examples.
