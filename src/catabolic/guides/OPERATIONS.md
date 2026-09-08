# Inventory, synchronization and recovery

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

This guide covers ongoing operation after the [first catalog walkthrough](GETTING_STARTED.md). Examples assume an installed CLI and an initialized database selected by `CATABOLIC_DB`. Source paths are examples; bind your own existing directories.

## Inventory and profiles

For the recommended order of operations, start with the
[workflow guide](WORKFLOW.md), also available as `catabolic docs workflow`.
For a single operational cycle with backlog counts, use
`catabolic maintenance --all-catalogs`; see the [maintenance guide](MAINTENANCE.md)
or `catabolic docs maintenance` for its zero-removal default and safety boundaries.

Locations identify logical source trees. Profiles bind those locations to concrete
directories on each machine. File occurrences and media identities are shared;
observed availability is specific to the selected profile.

```sh
catabolic profile add laptop
catabolic --profile laptop location bind media --root /mnt/media
catabolic --profile laptop catalog bind global --root /srv/plex
catabolic --profile laptop scan
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
catabolic scan \
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

Set `CATABOLIC_DB` to the absolute path of your initialized database before running these examples:

```sh
# Find items by title, media kind, year, or an exact provider identity.
catabolic --json item list --search Dune --kind movie --year 2021
catabolic --json item list --identity tmdb.movie=438631

# Match stored top-level metadata; repeated conditions are combined with AND.
catabolic --json item list --metadata 'genre="Science Fiction"'

# See an item's identities, associated source copies, and catalog destinations.
catabolic --json item show --identity tmdb.movie=438631
catabolic --json item show ITEM_ID --catalog global

# Find source paths and inventory requiring attention.
catabolic --json files --search dune --location media --sort path
catabolic --json files --status missing
catabolic --profile laptop --json files --status unknown
catabolic --json files --unidentified
catabolic --json files --unmapped --catalog global

# Follow associations or inspect disabled decisions.
catabolic --json files --item ITEM_ID --catalog global
catabolic --json mapping list --all-catalogs --item ITEM_ID
catabolic --json mapping list --active disabled
```

Search is a literal substring match with Unicode case folding. `%`, `_`, and
quotes are ordinary characters, not wildcards or SQL. `item list --search` searches
the stored `title`; `files --search` searches source-relative paths, and
`mapping list --search` searches destination paths. Provider identity namespaces
and values match exactly. `--year` matches an integer `year` between 1 and 9999;
the string `"2021"` does not match `--year 2021`.

`item list --metadata KEY=JSON_VALUE` matches an existing top-level field exactly,
including its JSON type. A missing field does not match `null`. Strings need JSON
quotes inside shell quoting, as in the example. These convenience filters do not
provide nested expressions or ranges; use SQL for those. Separately, `content
search` searches text extracted by processing jobs; see [enrichment](ENRICHMENT.md).

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

Queries use schema 7. Item-title and metadata filters can scan matching tables.
Extracted-content search is a separate processing feature. Measure representative
queries before adding indexes or increasing timeouts.

## SQL and agent access

Use `query` for joins, aggregation, and questions beyond the convenience filters.
Agents can discover the SQL interface and run parameterized queries from the CLI:

```sh
catabolic --json query --schema
catabolic --json query \
  'SELECT location,count(*) AS missing_files FROM catalog_files
   WHERE profile=:profile AND status=:status GROUP BY location ORDER BY location' \
  --params '{"status":"missing"}'
catabolic --json query --file report.sql
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
catabolic status
catabolic recover --all-catalogs
catabolic sync --all-catalogs --dry-run
catabolic sync --all-catalogs
catabolic verify --all-catalogs
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
catabolic --db ./library-state/catalog.sqlite3 db status
catabolic --db ./library-state/catalog.sqlite3 db upgrade --dry-run
catabolic --db ./library-state/catalog.sqlite3 db upgrade
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
first; this release supports `recover` against schemas 1 through 16. Unknown schemas,
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
