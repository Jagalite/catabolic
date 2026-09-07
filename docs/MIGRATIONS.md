# Database migrations

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Catabolic uses ordered SQL files and one Python runner. Users can skip application
releases: upgrading schema 1 to schema 10 applies migrations 2 through 10 in order.
There is no separate script for every possible pair of versions. The shipped
schema is currently **6**. Migration `004_tags.sql` adds tag vocabulary, aliases,
hierarchy and attributed item/file assertions without changing existing rows.
Migration `005_hardlinks.sql` adds catalog link modes, hardlink ownership and
retention records. Higher versions below are authoring examples.

## Operating an upgrade

```sh
catabolic --db /path/to/catalog.sqlite3 db status
catabolic --db /path/to/catalog.sqlite3 db upgrade --dry-run
catabolic --db /path/to/catalog.sqlite3 db upgrade
```

Global options precede `db`. JSON output is available with `--json`.
`db status` checks the known schema and migration history. `--dry-run` also checks
SQLite integrity and foreign keys; it lists pending migrations but does not
execute them or create backup or rehearsal files. An outstanding link journal
with a pending upgrade returns `safe: false` and exit code 3.

Actual upgrades perform these steps:

1. Acquire the same advisory writer lock used by normal Catabolic writers.
2. Validate the schema, migration checksums, database identity, integrity, and
   foreign keys. Refuse pending link operations across all profiles.
3. Capture a consistent SQLite backup. Verify its schema, identity, integrity,
   foreign keys, and a digest of every existing catalog table's values.
4. Rehearse all pending migrations on a private copy of that verified backup.
5. Reserve the live SQLite writer and confirm its version and data still match
   the backup. If an external writer changed them, stop and require a new run.
6. Run every pending migration in one transaction. After each step, validate the
   expected schema, integrity, foreign keys, and preservation of existing values.
   Record the SQL filename and SHA-256 checksum with the schema version.
7. Commit the entire chain, then update the backup manifest.

Source media and generated links are outside this operation. Their database IDs,
bindings, mappings, opaque metadata, and ownership records are preserved. The
database stays at its original path. New databases run the same SQL sequence,
recording history with origin `initialized`. A schema-1 upgrade records migration
1 as `baseline` and subsequent migrations as `migrated`.

Schema 1, 2, 3 and 4 recovery is explicitly supported by this release because schemas
2 through 4 add tables without changing the existing symlink journal. Schema 5
adds separately named hardlink operation kinds; legacy recovery still uses only
the symlink protocol. Schema 3 backfills
independent identification from both active and disabled mappings; it does not
change the original mappings or infer new provider identities. If an upgrade reports unfinished link operations, recover
each affected profile before retrying:

```sh
catabolic --db /path/to/catalog.sqlite3 --profile default recover --all-catalogs
catabolic --db /path/to/catalog.sqlite3 db upgrade
```

Use the appropriate profile name for other machines. Recovery may require their
source and output bindings to be accessible.

## Backups and failures

The default backup parent is `/path/to/catalog.sqlite3.backups/`. Each attempt
that reaches backup creation gets a unique timestamped directory containing
`snapshot.sqlite3` and `manifest.json`. The manifest records the original database
identity and path, source and target schema versions, pending SQL checksums,
backup SHA-256, and pre-upgrade table counts and digests. Completed backup files
have mode 0600 and new backup directories have mode 0700.

`db upgrade --backup-dir /another/location` selects a different parent. The actual
backup directory cannot overlap any registered source or output tree. Keep ample
free space for a backup, a rehearsal copy, and SQLite's transaction storage;
required space depends on the migration. Backups are retained, including after
failed attempts; there is no automatic pruning.

A backup or rehearsal failure leaves the live schema untouched. A migration
failure before commit rolls the whole chain back. Process-interruption tests cover
exits before and after commit, and SQLite-full tests cover rollback during live
DDL. These tests do not establish power-loss durability on every filesystem.

After an interrupted run, rerun `db upgrade`. SQLite can recover an interrupted
transaction when the database is opened writable; a read-only status command may
fail while that recovery is needed. A committed upgrade is a no-op on retry. The
database version and migration history are authoritative: an interrupted process
may leave a backup manifest at an earlier phase even though commit succeeded.
If only the final manifest update fails, the CLI reports the committed upgrade
with a warning.

There is no automatic restore or downgrade. To recover manually, stop Catabolic
writers, retain the current database using SQLite's backup facility, and restore
the verified snapshot to a **new database path**. Verify its manifest checksum and
inspect it with `db status` before choosing it as the active database. It retains
the original schema version and may need the matching application version or a
fresh upgrade. Never operate both copies against the same generated output.
Restoring loses catalog changes made after the snapshot; filesystem changes made
since then also require reconciliation. Do not overwrite a running SQLite file or
copy only its main file when WAL data may exist.

The implementation uses Python's
[SQLite backup API](https://docs.python.org/3/library/sqlite3.html#sqlite3.Connection.backup)
to include committed WAL data. It executes statements individually because
[`executescript()` can commit an existing transaction](https://docs.python.org/3/library/sqlite3.html#sqlite3.Connection.executescript).
The runner controls [SQLite transaction boundaries](https://www.sqlite.org/lang_transaction.html)
and performs a separate
[foreign-key check](https://www.sqlite.org/pragma.html#pragma_foreign_key_check).

## Adding a migration

The shipped resources are:

```text
src/catabolic/migrations/
  001_initial.sql
  002_migration_history.sql
  003_media_model.sql
  004_tags.sql
  005_hardlinks.sql
```

For the next schema change:

1. Add `006_descriptive_name.sql`. Versions must be contiguous and filenames must
   use the existing numbered, lowercase convention.
2. Increment `SCHEMA_VERSION` in `src/catabolic/migration.py` to the next version.
3. Add populated fixtures and tests for the oldest supported schema, the previous
   schema, skipped releases, fresh initialization, failure, and interruption.
4. Build and install a wheel in isolation to verify the SQL resources ship.

Never edit, rename, or remove a released migration. The runner compares recorded
filenames and SHA-256 checksums and refuses a mismatch. Fix a released change with
a new migration. Application package versions and schema versions are independent;
only a schema change requires a new SQL file.

For example, an additive migration could contain:

```sql
ALTER TABLE items ADD COLUMN review_note TEXT;
```

SQL files must also work when the application initializes an empty database by
running the full sequence. End SQL statements with semicolons. Triggers and
semicolons inside quoted strings are supported. Do not include `BEGIN`, `COMMIT`,
`ROLLBACK`, savepoints, `PRAGMA`, `ATTACH`, `DETACH`, or changes to the history ledger.
The runner owns those operations and rejects attempts from migration SQL.

The current preservation policy is deliberately conservative: all existing tables,
columns, row counts, and projected values must survive exactly. Adding columns or
tables is supported. Rewriting existing metadata, dropping a column, or renaming
an existing table will fail preservation checks. Such transformations need a
reviewed extension with explicit before/after invariants and populated historical
fixtures; do not disable the checks to make a migration pass. Python migration
hooks and automatic downgrade scripts are not implemented.

`tests/fixtures/schema_v1.sql` is an independent frozen copy of the old schema.
Do not regenerate historical fixtures from current migrations, since that would
hide compatibility regressions. Future reconciliation changes must also revisit
the narrowly permitted legacy-recovery path in `Store`.

```sh
.venv/bin/python -m unittest discover -s tests -v
.venv/bin/ruff check src tests
.venv/bin/ruff format --check src tests
```

Schema 6 (`006_enrichment.sql`) adds proposals, decision events, expected sets, copy
policies, processing jobs/results, checksum baselines, text indexes, refresh state,
and watch stability records. It preserves all existing rows and ownership tables.

Schema 7 (`007_processing_attempts.sql`) adds processing attempt history and indexed
retry eligibility. Existing jobs and results are preserved. Historical attempts
are not invented; only newly completed attempts get history rows. Existing
migrations 1 through 6 remain unchanged.

Schema 8 (`008_processing_artifacts.sql`) adds immutable recipes, generated locations
and recoverable output records. Jobs gain an optional recipe reference; attempts
gain start/end timestamps and structured results. Existing columns and rows are
preserved; historical recipe references and attempt details remain null. See
[Generated media](ARTIFACTS.md) for publication and recovery.

Schema 9 (`009_output_definitions.sql`) adds reusable output definitions and a
rendition registry shared by generated and externally registered media. Recipes
gain a nullable output-definition reference. Existing recipe definitions, digests,
jobs and artifact rows remain unchanged. Older queued jobs retain their preset's
same-item semantics when run or recovered. Historical completed outputs are not
backfilled with invented definitions or provenance. New recipes reference explicit
immutable definitions. New item/relationship/rendition registration commits with
artifact publication; a failed transaction leaves no partial catalog item behind.

Schema 10 (`010_item_workflow.sql`) adds item workflow state, required completion
checks, and append-only worklog events. Existing items default to `pending` at
read time, with revision 0 and no fabricated history. Artifacts gain a nullable
publication snapshot; existing rows remain unchanged. See [Entry worklog](WORKLOG.md)
for completion checks, evidence freshness, and requirements for older artifacts.
