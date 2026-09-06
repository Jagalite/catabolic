# Catabolic

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Catabolic is an independent Python application for media inventory and symlink or hardlink
catalogs. Its application core owns catalog decisions, SQLite state, source scans,
and recoverable link synchronization. The CLI is an adapter over that core.

The 4dlink skill informed the requirements. Catabolic does not import, invoke,
or depend on 4dlink, and its database is a separate format.

## Current scope

Version 0.1.0 implements the first complete workflow: register source and output
directories, scan files, record explicit media identities and catalog destinations,
preview changes, synchronize links, and independently verify them. It supports
machine profiles, multiple catalogs, searchable and paginated catalog queries, interruption
recovery, declarative output layouts, read-only SQL and GraphQL, and explicit,
backed-up database upgrades.

Schema 6 also adds lightweight signature inspection, optional FFmpeg probing,
resumable processing, checksum integrity reports, persisted identification proposals,
sidecar discovery, preferred-copy policies, completeness reports, content search,
periodic watching, and a Jellyfin refresh adapter. See [ENRICHMENT.md](ENRICHMENT.md)
or `catabolic docs enrichment` for commands and the implemented scope. Automatic
identification remains an explicit review workflow; migration from 4dlink is future work.

## Install and run

Requires Python 3.11 or later on macOS or Linux. Runtime dependencies include
`graphql-core` for GraphQL and Pydantic for the manifest contract.
macOS is tested locally; Linux support has not yet been validated.

```sh
python3 -m venv .venv
.venv/bin/python -m pip install -e '.[dev]'
.venv/bin/catabolic --help
```

After installing dependencies, running directly from the checkout also works:

```sh
PYTHONPATH=src python3 -m catabolic --help
```

## Offline documentation

The installed CLI bundles guides, query examples and output specifications:

```sh
catabolic docs
catabolic docs schema
catabolic docs query
catabolic docs native
catabolic docs --search 'manifest version'
catabolic --json docs graphql
```

No database or checkout is required. See [SCHEMAS.md](SCHEMAS.md) for schema
discovery commands and the documentation JSON interface for agents.

## A first catalog

Create a directory for application state and an empty directory for custom
generated links. Source directories and explicit output roots must already exist.
Keep state outside both trees.

For default output folders, omit `--root` when binding a catalog:
`catalog bind plex` creates `./catabolic/plex/`, and `catalog bind jellyfin`
creates `./catabolic/jellyfin/`. `global` defaults to `./catabolic/global/`.
Existing bindings are reused. The example below selects a custom output instead.

```sh
mkdir -p ./library-state ./plex
export CATABOLIC_DB="$PWD/library-state/catalog.sqlite3"

.venv/bin/catabolic init
.venv/bin/catabolic location bind media --root /Volumes/Media
.venv/bin/catabolic catalog bind global --root "$PWD/plex"
.venv/bin/catabolic scan
.venv/bin/catabolic --json files --unmapped
```

Use the returned file ID to record a catalog decision. The identity namespace
should distinguish media types when a provider uses overlapping numeric IDs.

```sh
.venv/bin/catabolic --json item put \
  --kind movie \
  --identity tmdb.movie=438631 \
  --metadata '{"title":"Dune","year":2021}'

.venv/bin/catabolic mapping put \
  --file FILE_ID \
  --item ITEM_ID \
  --path 'Movies/Dune (2021)/Dune (2021).mkv'

.venv/bin/catabolic sync --dry-run
.venv/bin/catabolic sync
.venv/bin/catabolic verify
```

`FILE_ID` and `ITEM_ID` are placeholders for IDs returned by previous commands.
An item may have multiple identity keys. Reusing a key returns the same item;
conflicting identities fail. An identical mapping request returns the same mapping.
Built-in kinds cover movies/TV, music, books, audiobooks, podcasts, comics,
photos, documents, and collections. `item types` lists them; `custom:name` extends
the vocabulary. See [MEDIA_MODEL.md](MEDIA_MODEL.md) for identification, roles,
ordered parts, and relationships.
To change an existing destination's selected file, disable the old mapping and
record the replacement before synchronizing. An existing owned link is then
retargeted atomically.

Global options (`--db`, `--profile`, `--json`) go before the command. No database is
chosen implicitly: provide `--db` or `CATABOLIC_DB`. `CATABOLIC_PROFILE` defaults to
`default`. `--json` writes structured results to stdout and application errors to
stderr. Exit codes are 0 for success, 2 for input/operational errors, 3 for an
incomplete scan, blocked layout/sync, or unhealthy verification, and 130 for interruption.
GraphQL always emits JSON; query errors appear in its stdout `errors` array and exit 2.
Argument-parser errors use argparse's standard text output and exit code 2.

## Generated layouts and GraphQL

[Application compatibility](COMPATIBILITY.md) covers 20 targets: 17 folder
presets plus calibre, Calibre-Web and Immich import adapters. Run `catabolic
target list` or `catabolic docs compatibility`. `export` generates XSPF playlists,
OPDS 2.0 catalogs and NFO metadata, optionally in a fresh symlink bundle. Application
scan compatibility is reported separately from Catabolic fixture validation.

[OUTPUT_LAYOUTS.md](OUTPUT_LAYOUTS.md) describes custom naming templates, relationship
selectors, Catabolic/Plex/flat presets, and safe mapping ownership. Save a layout, preview and
apply its mappings, then use the existing sync and verification commands. One
library can feed several output catalogs with different layouts.

The [native Catabolic layout](NATIVE_LAYOUT.md) uses
`{kind}/{item-id}/{role}/{file-id}/{source-filename}` for all media types, with
portable filename sanitization. Select it with `layout put native --preset catabolic`;
`catalog bind native` defaults to `./catabolic/native`. Its naming profile is
versioned separately from the metadata manifest.
[QUERY_FOLDERS.md](QUERY_FOLDERS.md) shows how saved SQL or GraphQL queries select
membership for those folders, with complete-result checks and explicit refresh.

[GRAPHQL.md](GRAPHQL.md) documents read-only, nested catalog queries with JSON
variables, query files/stdin, schema discovery, pagination, and execution limits.
For example:

```sh
catabolic --db catalog.sqlite3 graphql '{ items(first: 10) { nodes { id title kind } pageInfo { hasNextPage endCursor } } }'
```

## Hardlink outputs

Symlinks remain the default. `catalog bind NAME --link-mode hardlink` opts a
catalog into same-filesystem hardlinks. Cross-filesystem operations are refused;
final-reference removal is blocked, and other retired links are retained without
automatic deletion. Read [HARDLINKS.md](HARDLINKS.md) or `catabolic docs hardlinks`.

## Tags and curation

[TAGGING.md](TAGGING.md) and `catabolic docs tags` cover namespaced tags, aliases,
acyclic parent relationships, explicit item/file tagging, provenance, bulk
assignment, all/any/none filters, SQL/GraphQL and tag-driven output folders.
Tagging preserves sources; generated manifest v3 exports carry the tagging data.
Existing manifest v1 documents remain readable.

## Metadata manifests

[OPEN_CATALOG.md](OPEN_CATALOG.md) describes the typed interchange contract,
generated JSON Schema and field reference, frozen versions, and lossless JSON-value
round trips. `catabolic spec schema`, `spec docs`, `spec check`, and
`spec validate --file catalog.json` work without a database. Manifest exports use
the same models for validation.

[MANIFESTS.md](MANIFESTS.md) documents versioned JSON exports containing item
metadata, identities, file roles, relationships, source/output paths, and saved
layout/query provenance. Export to stdout, a separate file, or the synchronized
catalog's reserved metadata file:

```sh
catabolic --db catalog.sqlite3 manifest --catalog global --in-catalog
```

## Inventory and profiles

Locations identify logical source trees. Profiles bind those locations to concrete
directories on each machine. File occurrences and media identities are shared;
observed availability is specific to the selected profile.

```sh
.venv/bin/catabolic profile add laptop
.venv/bin/catabolic --profile laptop location bind media --root /mnt/media
.venv/bin/catabolic --profile laptop catalog bind global --root /srv/plex
.venv/bin/catabolic --profile laptop scan
```

Bindings record directory device and inode identity. If a source or output root
is replaced, scanning or synchronization stops until the binding is deliberately
updated. Bind source roots at the actual mounted media filesystem. Nested
filesystem boundaries make a scan incomplete; register each mount separately.

Only a complete scan publishes observations or marks unseen occurrences missing.
Errors, detected directory changes, and inaccessible roots leave the previous
inventory intact. Source symlinks are never followed. A scan is not a filesystem
snapshot: avoid modifying directory structure while it runs.

For source volumes containing protected operating-system directories, exclude
those paths explicitly. `--exclude` accepts an exact source-relative file or
subtree and can be repeated; it does not interpret wildcard characters.

```sh
.venv/bin/catabolic scan \
  --exclude .DocumentRevisions-V100 \
  --exclude .Spotlight-V100 \
  --exclude .TemporaryItems \
  --exclude .Trashes \
  --exclude .fseventsd \
  --exclude .DS_Store
```

Each scan records and reports its exclusion scope. Excluded entries are not
traversed, and their previous observations are preserved rather than marked
missing. Other hidden files remain in scope. Repeat the exclusions on subsequent
scans; a permission failure without an exclusion still makes the scan incomplete.

`files --limit N` returns at most N entries and a `next_cursor`. Pass it to
`files --cursor TOKEN` with the same profile, catalog, and filters. The default
limit is 100 and the maximum is 1000. `--unmapped` is scoped to `global` unless
`--catalog NAME` selects another catalog. Pagination is stable for unchanged
inventory; restart enumeration after a concurrent scan.

## Searching the catalog

Queries read a database snapshot and do not scan drives, synchronize links, or
upgrade the schema. They work with offline sources. `present`, `missing`, and
`unknown` describe the selected profile's **recorded observations**; they are not
live availability checks. `scan_id` and `observed_at` identify the supporting scan.
Use `scan` to refresh inventory and `verify` for current link health.

With `CATABOLIC_DB` set as above:

```sh
# Find items by title, media kind, year, or an exact provider identity.
.venv/bin/catabolic --json item list --search Dune --kind movie --year 2021
.venv/bin/catabolic --json item list --identity tmdb.movie=438631

# Match stored top-level metadata; repeated conditions are combined with AND.
.venv/bin/catabolic --json item list --metadata 'genre="Science Fiction"'

# See an item's identities, associated source copies, and catalog destinations.
.venv/bin/catabolic --json item show --identity tmdb.movie=438631
.venv/bin/catabolic --json item show ITEM_ID --catalog global

# Find source paths and inventory requiring attention.
.venv/bin/catabolic --json files --search dune --location media --sort path
.venv/bin/catabolic --json files --status missing
.venv/bin/catabolic --profile laptop --json files --status unknown
.venv/bin/catabolic --json files --unidentified
.venv/bin/catabolic --json files --unmapped --catalog global

# Follow associations or inspect disabled decisions.
.venv/bin/catabolic --json files --item ITEM_ID --catalog global
.venv/bin/catabolic --json mapping list --all-catalogs --item ITEM_ID
.venv/bin/catabolic --json mapping list --active disabled
```

Search is a literal substring match with Unicode case folding. `%`, `_`, and
quotes are ordinary characters, not wildcards or SQL. `item list --search` searches
the stored `title`; `files --search` searches source-relative paths, and
`mapping list --search` searches destination paths. Provider identity namespaces
and values match exactly. `--year` matches an integer `year` between 1 and 9999;
the string `"2021"` does not match `--year 2021`.

`item list --metadata KEY=JSON_VALUE` matches an existing top-level field exactly,
including its JSON type. A missing field does not match `null`. Strings need JSON
quotes inside shell quoting, as in the example. Arbitrary nested-path expressions,
range filters, fuzzy search, and full-text indexing are not implemented.

`files --unmapped` means no active mapping in the selected catalog.
`--unidentified` means no active file identification in any catalog. Disabling a
mapping preserves identification; removing identification is a separate explicit
action. `files --item`, `--kind`, `--year`, and `--identity` filter independent active
identifications across catalogs. Combining these filters uses AND and never
duplicates a file row. `--catalog` scopes `--unmapped`, not identification filters.
Identification describes an association, not metadata quality or confidence.

Items are shared across profiles. `item list --catalog NAME` restricts them to
active membership in that catalog. `item show` returns one occurrence per mapping
across all catalogs by default, so the same source can appear in several rows.
Use `--include-disabled` to include historical disabled decisions. Source and
output paths use the selected profile's stored bindings; unbound roots yield
`null`. `recorded_link_target` is the last owned target at that destination, which
does not establish that the link exists or matches the displayed mapping today.
An item with no mappings still has a detail record with an empty occurrence list.
`item show` also includes independently paginated `file_associations` and
`relationships`; use `association list` and `relationship list` to follow their
respective cursors.

All three listing commands now default to **100 rows**, with a maximum `--limit`
of 1000, and return `next_cursor`. This changes the previously unbounded item and
mapping listings: scripts must follow cursors to consume all results. `item show`
also paginates its occurrences. An empty search result succeeds with an empty
list; an unknown ID or identity passed to `item show` is an error.

| Command | `--sort` choices | Default |
| --- | --- | --- |
| `files` | `id`, `path`, `size`, `mtime` | `id` |
| `item list` | `id`, `title`, `year` | `id` |
| `mapping list` | `id`, `path`, `catalog` | `path` |

Use `--descending` to reverse ordering. Title sorting uses case folding; paths
sort by their stored text. Missing titles sort as empty strings, and unknown
sizes/timestamps or invalid years sort before known values in ascending order.
An ID breaks sorting ties. Pass each `next_cursor` back with the same database,
profile, filters, and ordering; the page size may change. Cursors are opaque and
do not keep a snapshot alive between commands. Restart pagination after catalog
changes or an application upgrade. Each returned page is internally consistent.

Queries use schema 7. Search and metadata filters can scan matching
tables; there is no full-text index. Very large catalogs
may need additional indexes or a separate search index after measurement.

## SQL and agent access

Use `query` for joins, aggregation, and questions beyond the convenience filters.
Agents can discover the SQL interface and run parameterized queries from the CLI:

```sh
.venv/bin/catabolic --json query --schema
.venv/bin/catabolic --json query \
  'SELECT location,count(*) AS missing_files FROM catalog_files
   WHERE profile=:profile AND status=:status GROUP BY location ORDER BY location' \
  --params '{"status":"missing"}'
.venv/bin/catabolic --json query --file report.sql
```

The documented views are `catalog_items`, `catalog_identities`, `catalog_files`,
`catalog_entries`, `catalog_item_files`, and `catalog_relationships`. These views
exist only on the query connection; the underlying media model requires schema 7.
File, entry, and association views contain every profile; `:profile` is bound to
the selected CLI profile for explicit filtering. Items and relationships are
shared across profiles. SQLite enforces read-only execution.

JSON results include `columns`, row arrays, and `complete`/`truncated` flags.
The default output cap is 1000 rows and the SQL execution timeout is 5 seconds.
Truncation returns exit code 3; errors return 2 without partial stdout. `--file -`
accepts explicit stdin; otherwise the command never prompts for SQL.
See [QUERYING.md](QUERYING.md) for the full agent contract, limits, examples,
pagination, and view semantics.

## Synchronization and recovery

`sync --dry-run` is read-only, including database state. It does not scan sources,
create directories, claim outputs, or save an executable plan. `sync` recomputes
and revalidates current state. Use `--catalog NAME` or `--all-catalogs` for other
output scopes; the default is `global`.

Only an empty output can be claimed. Its ownership marker identifies the database,
profile, and catalog. Correct links remain untouched. Conflicting entries and
externally changed owned paths block synchronization. Generated symlink targets
are relative. Empty directories are retained after their last link is removed.

When a source disappears without a complete confirming scan, synchronization
blocks. After a complete scan confirms absence, synchronization may remove its
owned link but preserves its active mapping. A later complete scan and sync
restore the link when the source returns. Empty files and recognized download
suffixes are also excluded. Verification reports these mappings as unhealthy
even when link reconciliation completed safely.

Every filesystem mutation is journaled before execution. An interrupted command
can have made partial progress. Inspect `status`, then recover the selected scope:

```sh
.venv/bin/catabolic status
.venv/bin/catabolic recover --all-catalogs
.venv/bin/catabolic sync --all-catalogs --dry-run
.venv/bin/catabolic sync --all-catalogs
.venv/bin/catabolic verify --all-catalogs
```

Recovery observes what actually happened, finishes accounting for completed
operations, and revalidates unapplied operations. A successful scan can make an
unapplied operation obsolete; recovery then cancels that intent. An unavailable
root or external collision remains blocked. Desired mappings and bindings cannot
change while operations are pending. Scans remain available to refresh evidence.

## Database upgrades

Application releases and database schema versions are separate. New databases
start at schema 7. Schema 1 through 6 databases require an explicit upgrade; ordinary
commands never migrate them automatically.

```sh
.venv/bin/catabolic --db ./library-state/catalog.sqlite3 db status
.venv/bin/catabolic --db ./library-state/catalog.sqlite3 db upgrade --dry-run
.venv/bin/catabolic --db ./library-state/catalog.sqlite3 db upgrade
```

The runner applies every pending numbered SQL file in order. Before changing the
live database, it creates and verifies a SQLite backup, then rehearses the upgrade
on a disposable copy. All pending steps commit together. Existing catalog values,
identities, bindings, and ownership records must survive unchanged; migrations do
not edit source media or generated links. Schema 2 adds migration history and file
checksums. Schema 3 adds independent file identification and typed relationships,
backfilling identification from active and disabled mappings while preserving all
existing records and link ownership.

Backups and a manifest are retained under `DATABASE.backups/`. Use
`db upgrade --backup-dir /path/to/backups` to choose another location outside
registered source and output trees. Backups are never automatically pruned.
`--dry-run` validates the existing database and lists pending steps without
creating a backup or running the migration SQL. Repeating a completed upgrade
does nothing and creates no further backup.

Pending link operations in any profile block an upgrade. Finish their recovery
first; this release supports `recover` against schemas 1 through 6. Unknown schemas,
inconsistent migration history, and databases from newer versions are refused.
Downgrades and automatic backup restoration are not implemented.

See [MIGRATIONS.md](MIGRATIONS.md) for failure handling, backup recovery guidance,
and the workflow for adding the next numbered SQL migration.

## Operating assumptions and limits

Keep the database on a local filesystem. Catabolic serializes its writers with a
separate advisory lock, and readers use a consistent SQLite snapshot. Do not edit
the database directly or delete its lock file while a process is using it.

Catabolic owns generated outputs exclusively. Directory-relative, no-follow
operations prevent traversal through symbolic-link parents, and existing targets
are checked before replacement or removal. Portable POSIX APIs do not provide an
atomic compare-and-delete primitive: simultaneous external renames or writes to
the output tree are outside the supported concurrency model. This is not a sandbox
against a malicious process running as the same user.

Scanning stages inventory in disposable SQLite storage with 500-row Python
buffers. Synchronization preflights the full selected scope, then revalidates the
affected source and destination before each mutation. Synthetic scale benchmarks
and representative-media acceptance are available; results depend on storage and
workload. Tests cover process interruption; power-loss durability on
every supported filesystem has not been established. Keep source and output paths
in the same relative relationship in Plex's filesystem namespace.

For release acceptance, see [RELEASE_TESTING.md](RELEASE_TESTING.md) or
`catabolic docs testing`. Schema 7 adds durable processing attempt history and
opt-in transient retries. `sync --max-removals N --max-removal-percent P` checks
bulk output removals before any filesystem changes.

## Development

```sh
.venv/bin/python -m unittest discover -s tests -v
.venv/bin/ruff check src tests
.venv/bin/ruff format --check src tests
.venv/bin/python scripts/sync_docs.py --check
```

Root Markdown guides are authoritative. After editing one, run
`python scripts/sync_docs.py` and include its updated package copies in the change.
Tests reject stale bundled guides so installed documentation matches the checkout.

The tests use temporary fixture trees and databases, including a subprocess CLI
workflow. They never use a real media library. See [DESIGN.md](DESIGN.md) for the
model, ownership boundaries, and subsequent milestones.

### Fake-library scenarios and performance

A deterministic fixture generator creates two fake source drives containing
movies, episodes, subtitles, Unicode names, empty files, partial downloads,
excluded directories, and symlink traps. Media payloads are 1 KiB of synthetic
data. Every run requires fresh directories and retains its files for inspection.

Run the small lifecycle check as an ordinary integration test:

```sh
.venv/bin/python -m unittest tests.test_synthetic_library -v
```

Run larger fixtures and save timing reports:

```sh
.venv/bin/python -m tests.synthetic_benchmark --sizes 100 1000 5000
```

The runner prints its artifact path under `.local-tests/`. Supply `--root NEW_PATH`
to choose a new directory; existing paths are refused. Each `files-N/` contains
`source-1/`, `source-2/`, `plex/`, a database, and `report.json`. The top-level
`report.json` compares stages and records the platform and process memory peak.

Each fixture checks pagination, read-only preview, correct relative links,
unchanged repeat sync, missing/restored files, unavailable source roots, external
collisions, empty/partial sources, and journal recovery after an actual child
process exits between filesystem work and database accounting. It finishes with
the requested number of healthy links and matching source-content hashes.

Timings cover generation, scanning, recording decisions, preview, initial sync,
verification, repeat sync, and failure scenarios separately. These measure local
filesystem and catalog overhead with small files. They do not measure media
decoding, large-file reads, or NAS performance. Unit tests assert behavior without
hardware-dependent wall-clock thresholds.

### Higher-cardinality tests

[SCALE_BENCHMARKS.md](SCALE_BENCHMARKS.md) describes the isolated-process benchmark
for 10k/100k/1m database fixtures and 10k/100k real synthetic file/link trees. It
records peak RSS, existing application limits, repeat/change behavior, and crash
recovery separately from fixture-generation costs.

## License

Catabolic's code and documentation are licensed under [MIT](LICENSE), copyright
2026 The Catabolic Contributors. Files carry SPDX notices; JSON, existing SQL
migrations, and frozen specification artifacts use adjacent `.license` files so
that their content and checksums stay intact. Keep these files together when
redistributing them. Dependencies retain their own licenses, and external media
and provider data remain subject to their respective rights and terms.
