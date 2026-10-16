# Native upgrade rehearsal

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

This covers Track B items 1–5 of the
[native retirement work plan](NATIVE_RETIREMENT_WORK_PLAN.md): the operator
commands for the explicit schema-32 to schema-38 upgrade, the identity
checker, the rehearsal harness, offline journal conversion and the recorded
post-upgrade limitations. These are development tools and tests. Passing them
does not qualify the upgrade, close a retirement gate or authorize upgrading a
real catalog. Controlled restore with incarnation rotation (item 6) is not
implemented here.

## Commands

All commands belong to the `catabolic-native` executable and print one JSON
value. Global options come first, as in `catabolic-native --db PATH db ...`.
A refusal prints `catabolic: <reason>` on stderr and exits with status 2.
Only the legacy `db upgrade` implements `--dry-run`; every other command
refuses it before doing anything. Plan and manifest files are fsynced, with
their directory, before success is reported.

| Command | Purpose |
| --- | --- |
| `db native-inspect [--full]` | Read-only inspection: schema, pending native steps, pending work in every profile, journal mode. `quiescence_verified` is always `false`. |
| `db native-plan --out PLAN.json` | Writes a plan bound to the exact input: file identity, history, table digests, sequences, pending work and migration checksums. The file is created with mode 0600 and is never replaced. |
| `db native-upgrade --plan PLAN.json --assert-quiescent ... [--backup-dir D] [--migration-failpoint STAGE:VERSION]` | Applies the unchanged plan through the existing migration owner, with a verified backup and a rehearsal on a private clone. |
| `db identity-manifest [--out FILE]` | Read-only identity manifest (see below). |
| `db identity-diff --before A --after B` | Compares two manifests and refuses if any identity changed. |
| `db convert-journal --to delete` | Offline conversion of a WAL catalog to the DELETE rollback journal. |

`--assert-quiescent` must be repeated with each of `admission-stopped`,
`readers-and-workers-stopped` and `exclusive-operational-ownership`. These are
operator statements. Locks cannot show that old Python processes, idle
descriptors or external workers have stopped, so the commands never infer
them. Missing assertions, unknown assertion names, options given to the wrong
command, a changed or tampered plan, pending work, WAL mode, a live Catabolic
writer and active native sessions are each refused before a backup is made.

A typical sequence, with every writer, reader and worker stopped:

```sh
catabolic-native --db catalog.sqlite3 db native-inspect --full
catabolic-native --db catalog.sqlite3 db convert-journal --to delete   # only if WAL
catabolic-native --db catalog.sqlite3 db identity-manifest --out before.json
catabolic-native --db catalog.sqlite3 db native-plan --out plan.json
catabolic-native --db catalog.sqlite3 db native-upgrade --plan plan.json \
  --assert-quiescent admission-stopped \
  --assert-quiescent readers-and-workers-stopped \
  --assert-quiescent exclusive-operational-ownership
catabolic-native --db catalog.sqlite3 db identity-manifest --out after.json
catabolic-native --db catalog.sqlite3 db identity-diff --before before.json --after after.json
```

Catalogs older than schema 32 are first upgraded with the existing
`db upgrade`, which keeps its own backup.

## Identity manifest

`catabolic_store::identity_manifest` records:

- the database ID (canonical and as stored in `meta`) and the native
  `catalog_id`, incarnation and generation;
- the complete migration history;
- a digest of every table's primary-key set (rowid for tables without one);
- every AUTOINCREMENT sequence;
- the `identities` rows, and the schema-34 `catalog_legacy_identity_evidence`
  baseline when present.

`diff` first validates both manifests: the exact field set and types, a
canonical database ID, history contiguous up to the schema, a native catalog
identity exactly for native schemas, well-formed digests for the core tables,
the schema-34 baseline exactly from schema 34, and the fingerprint. An
incomplete, mistyped or unsigned manifest is reported as `invalid_manifest`
and nothing else is compared. It then reports each changed `database_id`, `catalog_id`, history prefix,
table key set, sequence and legacy evidence entry, plus schema downgrades.
New tables, sequences and history rows are allowed. Incarnation and
generation may change. The key digests do not look at non-key values; the
migration owner's whole-row preservation check and the harness's table
snapshots cover those. The schema-34 entity index gains its legacy baseline
rows in that migration. The migration owner checks those rows individually,
so `diff` skips that one table across the schema-34 boundary.

## Rehearsal harness

`crates/catabolic-store/tests/upgrade_rehearsal.rs` runs on the retained
fixtures under `.local-tests/rust-migration/m0-completion`, or under
`CATABOLIC_REHEARSAL_FIXTURES`. Fixtures are only read; each case works on a
private copy. The harness covers legacy schemas 01, 16 and 32. The 10,000-item
scale catalog runs only on request, as the ignored test
`scale_database_rehearsal`; `CATABOLIC_REHEARSAL_SCALE_FIXTURE` may select a
different catalog.

For each fixture the harness:

1. Captures a whole-row snapshot and an identity manifest of the original.
2. Upgrades to schema 32 with the legacy owner and checks preservation, the
   identity diff and the legacy backup.
3. For each boundary below, copies the schema-32 catalog, writes a plan file,
   and runs the native upgrade in a child process that exits with status 86
   at that boundary.
4. Reopens the copy writable, so that SQLite rolls back a hot journal, and
   checks the schema, full validation, table preservation and identities. A
   catalog still at schema 32 must have the same manifest fingerprint.
5. Checks the backup receipt status and the SHA-256 of the backup snapshot.
   The receipt must embed the approved plan.
6. Resumes with a fresh plan. After a committed exit the old plan is refused.
   It then checks schema 38, history, preservation and the identity diffs
   against both the schema-32 and the original manifests.
7. Restores the committed backup, and the interrupted attempt's backup, to
   new paths. It verifies the manifest SHA-256 before and after the copy, and
   that `inspect` reports schema 32 with the original manifest fingerprint.

The boundaries are the migration checkpoints, with the version passed at each.
The matrix has 30 entries:

| Boundary | Versions | Schema after exit | Receipt status | Rehearsal clone left |
| --- | --- | --- | --- | --- |
| `upgrade:after_backup` | 32 | 32 | `backup_verified` | no |
| `rehearsal:before_migration`, `rehearsal:after_migration` | 33–38 | 32 | `backup_verified` | yes |
| `rehearsal:before_commit`, `rehearsal:after_commit` | 38 | 32 | `backup_verified` | yes |
| `upgrade:after_rehearsal` | 32 | 32 | `rehearsed` | no |
| `upgrade:before_migration`, `upgrade:after_migration` | 33–38 | 32 | `rehearsed` | no |
| `upgrade:before_commit` | 38 | 32 | `rehearsed` | no |
| `upgrade:after_commit` | 38 | 38 | `rehearsed` | no |

Two results are worth noting. An exit after the live commit leaves the
receipt at `rehearsed`, although the catalog is at schema 38; the schema
version and history are authoritative. A killed rehearsal leaves its
`rehearsal-*` clone inside the backup directory for the operator to remove.

`tests/test_native_upgrade_rollback.py` checks the frozen Python side. It
upgrades a frozen-initialized catalog, and the retained schema-32 fixture when
present, with the native CLI. It restores the verified backup to a new path,
and frozen Python must read the restored copy with `db status` and `status`
without changing its bytes. Frozen Python must then refuse the schema-38 file
with `unsupported database schema 38` and leave it unchanged. The test needs
`CATABOLIC_PYTHON_ORACLE`, `CATABOLIC_PYTHON_REFERENCE` and
`CATABOLIC_NATIVE_CLI`, and skips with the missing names otherwise.

## Journal conversion

Native upgrade refuses WAL catalogs: it needs a real SQLite EXCLUSIVE lock,
and WAL mode does not exclude readers. `db convert-journal --to delete` takes
the native lifecycle lease and the Catabolic writer lock. It refuses if
another process holds any WAL-index lock in the `-shm` file. It then opens
the catalog in SQLite's EXCLUSIVE locking mode and takes the database lock
before reading anything, and holds it until verification ends, so a process
arriving after the probe is refused rather than able to commit. Under that
lock it validates and digests the catalog, checkpoints with `TRUNCATE` and switches to
DELETE without waiting on locks. Finally it requires identical contents and
no remaining `-wal` file. Committed WAL frames left by a killed writer are
kept. A catalog already in rollback-journal mode is reported unchanged.
Converting to WAL is refused. A plan made before conversion no longer
matches, so a fresh plan is required.

The lock probe and SQLite's locks cover processes on the same host only. They
do not cover users on another host of a network filesystem. The operator's offline statement still applies.

## Post-upgrade write limitations

These are asserted as specific refusals by the harness, the CLI tests and the
frozen Python check. None of them changes the upgraded catalog.

| Limitation | Refusal |
| --- | --- |
| Legacy recovery admission on schema 38 (`Store::open` for recovery) | `native catalog recovery requires separately qualified recovery admission` |
| Legacy upgrade path on a native catalog | `native catalogs cannot be downgraded by the legacy upgrade path` |
| Frozen Python on schema 38 (`db status`, `status`) | `unsupported database schema 38` |
| Native schemas newer than 38 | `unsupported native schema target 39` |
| Reusing a consumed or stale plan | `native upgrade plan or inputs changed; inspect and approve a fresh native plan` |
| Native upgrade of a WAL catalog | `native upgrade requires rollback-journal mode for a real SQLite exclusive lock; found wal` |
| Converting a catalog to WAL | `unsupported journal conversion target "wal"` |

A repeated native upgrade with a fresh plan is an acknowledged no-op
(`upgraded: false`, no backup), and ordinary native admission still opens
the catalog. After the upgrade, recovery is native-only. A backup restores
only the pre-upgrade point. It is restored to a new path and does not undo
later writes or filesystem and network effects.

## Running

Build the targets through Kyoto with `cargo test --offline --locked -p
catabolic-store --test upgrade_rehearsal` (and `--test identity_manifest`,
`--test journal_conversion`, and `-p catabolic-cli --test native_db_commands`).
Then run the compiled executables directly, with `TMPDIR` on a physical
volume. Add `--ignored scale_database_rehearsal` for the scale fixture.
