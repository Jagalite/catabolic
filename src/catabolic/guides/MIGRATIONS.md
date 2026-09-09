# Database migrations

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Migrations 15–16 introduce reusable query revisions, projection bindings and generalized operation rules. Existing IDs, definition JSON, historical jobs and requirements are preserved; see [the exact mapping](PROGRAMMABLE_CATALOG.md#compatibility-evidence-and-limits).


Catabolic uses ordered SQL files and one Python runner. Users can skip application
releases: upgrading schema 1 to schema 16 applies migrations 2 through 16 in order.
There is no separate script for every possible pair of versions. The shipped
schema is currently **20**. Migration `004_tags.sql` adds tag vocabulary, aliases,
hierarchy and attributed item/file assertions without changing existing rows.
Migration `005_hardlinks.sql` adds catalog link modes, hardlink ownership and
retention records. The later shipped migrations are described below.

Migration `012_rendition_publication.sql` adds catalog-specific rendition policies
and decisions, accepted external receipts with delivery evidence, and semantic
rule requirements/evaluations. It changes no existing rows, recipe definitions,
association activation or historical job requirements.

Migration `013_processor_workers.sql` adds immutable HTTP processor definitions,
durable dispatch jobs and fenced worker-attempt history. Existing rows and the
schema-12 migration remain unchanged. No endpoints or remote work are created by
an upgrade. See [network processors](PROCESSORS.md).

Migration `014_catalog_refresh.sql` adds opt-in catalog settings and a coalescing
refresh queue. It preserves existing data and enables no automatic work during
upgrade. See [automatic catalog link updates](CATALOG_REFRESH.md).

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

Schema 11 (`011_processing_rules.sql`) adds immutable processing rule revisions,
maintenance enablement and rule/job provenance. Existing items, recipes, jobs,
artifacts and worklogs remain unchanged. No rules are created or enabled during
upgrade. Estimates are computed from current evidence rather than stored as
promises about output size. See [processing rules](RULES.md).

## Repair after a reboot or remount

Schema 18 records stable filesystem volume UUIDs separately from machine device
numbers. New bindings capture UUIDs when the filesystem supports them. Migration
does not adopt whatever happens to be mounted at existing paths. Device numbers
remain checked during ordinary work; a changed number requires verified repair.
Enrolled bindings additionally check volume identity, including when a replacement
volume reuses the old device number. Logical/serialized binding contracts remain
unchanged; volume evidence is local and is not transferred in program bundles.

On macOS, UUIDs come from `fgetattrlist` on an open directory descriptor
(`ATTR_VOL_UUID`). Linux uses bounded `findmnt --json --target ... --output UUID`;
filesystems without a reported UUID retain legacy checks and cannot use this
repair. A UUID is identity evidence, not a cryptographic guarantee against a cloned
volume. Directory inode, ownership and source/link checks remain required.

Preview repair of all bindings in the selected profile:

```sh
catabolic --db catalog.db --json remount
# For old bindings without UUID evidence, explicitly authorize first enrollment:
catabolic --db catalog.db --json remount --adopt-existing
# Review roots, before/after device IDs, UUIDs, and verification totals.
catabolic --db catalog.db --json remount --adopt-existing \
  --expected-plan PLAN_ID_FROM_PREVIEW --apply
catabolic --db catalog.db --json verify
catabolic --db catalog.db consumer run --limit 10
```

Preview is read-only. Apply rechecks the exact plan under the catalog writer lock,
then updates binding identities and the device fields of verified published source
observations in one transaction. It checks every active mapping, owned symlink,
resolved source's size/mtime/inode and output ownership marker. It verifies again
before commit. Missing/replaced roots, wrong volume UUIDs, changed media, modified
links, pending filesystem recovery and active consumer leases block repair.
Initially this path supports symlink outputs only; hardlink and retained-hardlink
ownership require separate recovery support.

Repair does not rewrite links, edit source files, replay processing, or send scans.
Consumer/publication references are updated together; pending generations and
acknowledgements are preserved. Binding revisions fence stale delivery responses.
The observation publication trigger is suppressed only inside the repair
transaction so device renumbering cannot masquerade as new media. An interruption
rolls back all repair records, including the trigger guard. The repair audit stores
the verified plan. Other inventory observations remain unchanged until a normal
scan; no full source-tree traversal is performed by repair.

## Trust replacement sources explicitly

Schema 19 adds an optional identity policy per source and local profile. Strict
identity checking remains the default. Neither upgrading nor importing a program
bundle enables path trust. Output ownership checks are not configurable here.

For a one-time replacement at a source's existing path:

```sh
catabolic --db catalog.db --json remount --trust-source seed1
# Review the preview, including the old/new roots, UUIDs and checked file counts.
catabolic --db catalog.db --json remount --trust-source seed1 \
  --expected-plan PLAN_ID_FROM_PREVIEW --apply
```

Repeat `--trust-source` to select several sources. This explicitly accepts the new
source root UUID/device/inode and treats media at the recorded relative paths as
the selected media. It verifies that every published source exists as a healthy
regular file and that the output marker and symlinks still match. It refreshes
size, modification time and file inode/device observations for those published
sources, recording the repair plan. New file versions produce normal pending
publication generations; a trusted replacement is not suppressed as a harmless
remount. Scan the source normally to refresh the rest of its inventory. This
operation does not prove byte-for-byte equivalence and does not change the
persistent policy. Generated artifact sources and hardlink repair remain excluded.

For an ongoing setting that trusts whatever is mounted at one source path:

```sh
catabolic --db catalog.db location trust seed1 --identity path
catabolic --db catalog.db location trust seed1 --identity path --apply
catabolic --db catalog.db --json location list
catabolic --db catalog.db scan seed1
```

`path` disables UUID, device and root-inode comparison for that source only. It
still requires an accessible directory with no symlink traversal. Scan completion
checks that the root did not change during traversal. Publication still checks
source-file versions against observations, so changed files need a completed scan
before they can be published. Missing sources, unhealthy media, output ownership,
changed output roots, and filesystem journals retain their existing checks.
Credentials, consumer identities and other sources' policies are unaffected.
Configuring a policy does not scan, rewrite links or request a Plex scan.

Restore strict checking with:

```sh
catabolic --db catalog.db location trust seed1 --identity strict --apply
```

This restores checking against the saved identity; it does not silently adopt a
replacement. If storage changed while path trust was enabled, use the explicit
one-time repair before continuing in strict mode. The persistent choice is stored
in `source_identity_policies`, separately from exported logical catalog programs.

## Schema 20: trust policy evidence

Adds individual source identity settings, policy revisions and change history. Existing schema-19 policies retain their behavior; migration records a baseline without inventing past evidence. See [Trust policies](TRUST_POLICIES.md). No local override is enabled by upgrade.
