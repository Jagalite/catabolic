# Native upgrade rehearsal

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

This covers Track B of the
[native retirement work plan](NATIVE_RETIREMENT_WORK_PLAN.md): the operator
commands for the explicit schema-32 to schema-39 upgrade, the identity
checker, the rehearsal harness, offline journal conversion, the recorded
post-upgrade limitations, and native backup with controlled restore and
incarnation rotation. These are development tools and tests. Passing them
does not qualify the upgrade or restore, close a retirement gate or authorize
using them on a real catalog.

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
| `db convert-journal --to delete --assert-quiescent ...` | Offline conversion of a WAL catalog to the DELETE rollback journal. |
| `db native-backup [--backup-dir D]` | Verified snapshot of a native catalog with a manifest (see below). |
| `db native-restore --backup DIR --assert-quiescent ...` | Controlled restore of a native backup at the live path under a fresh incarnation. |

For `db native-upgrade`, `db convert-journal` and `db native-restore`,
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
catabolic-native --db catalog.sqlite3 db convert-journal --to delete \
  --assert-quiescent admission-stopped \
  --assert-quiescent readers-and-workers-stopped \
  --assert-quiescent exclusive-operational-ownership   # only if WAL
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
- the sequence of every AUTOINCREMENT table the schema declares, recorded as
  null when SQLite has no sequence row for it;
- the `identities` rows, and the schema-34 `catalog_legacy_identity_evidence`
  baseline when present.

`diff` first validates both manifests: the exact field set and types, a
canonical database ID, history contiguous up to the schema, a native catalog
identity exactly for native schemas, exactly the declared schema's tables with
their key columns, exactly its AUTOINCREMENT tables in `sequences` (null only
for an empty table), the schema-34 baseline exactly from schema 34, and the
fingerprint. The expected tables and keys come from the frozen schema signature. An
incomplete, mistyped or unsigned manifest is reported as `invalid_manifest`
and nothing else is compared. It then reports each changed `database_id`,
`catalog_id`, history prefix, table key set, sequence state (including a
sequence appearing or changing on an empty table) and legacy evidence entry,
plus schema downgrades. New tables, sequences and history rows are allowed. Incarnation and
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
   It then checks schema 39, history, preservation and the identity diffs
   against both the schema-32 and the original manifests.
7. Restores the committed backup, and the interrupted attempt's backup, to
   new paths. It verifies the manifest SHA-256 before and after the copy, and
   that `inspect` reports schema 32 with the original manifest fingerprint.

The boundaries are the migration checkpoints, with the version passed at each.
The matrix has 34 entries:

| Boundary | Versions | Schema after exit | Receipt status | Rehearsal clone left |
| --- | --- | --- | --- | --- |
| `upgrade:after_backup` | 32 | 32 | `backup_verified` | no |
| `rehearsal:before_migration`, `rehearsal:after_migration` | 33–39 | 32 | `backup_verified` | yes |
| `rehearsal:before_commit`, `rehearsal:after_commit` | 39 | 32 | `backup_verified` | yes |
| `upgrade:after_rehearsal` | 32 | 32 | `rehearsed` | no |
| `upgrade:before_migration`, `upgrade:after_migration` | 33–39 | 32 | `rehearsed` | no |
| `upgrade:before_commit` | 39 | 32 | `rehearsed` | no |
| `upgrade:after_commit` | 39 | 39 | `rehearsed` | no |

Two results are worth noting. An exit after the live commit leaves the
receipt at `rehearsed`, although the catalog is at schema 39; the schema
version and history are authoritative. A killed rehearsal leaves its
`rehearsal-*` clone inside the backup directory for the operator to remove.

`tests/test_native_upgrade_rollback.py` checks the frozen Python side. It
upgrades a frozen-initialized catalog, and the retained schema-32 fixture when
present, with the native CLI. It restores the verified backup to a new path,
and frozen Python must read the restored copy with `db status` and `status`
without changing its bytes. Frozen Python must then refuse the schema-39 file
with `unsupported database schema 39` and leave it unchanged. The test needs
`CATABOLIC_PYTHON_ORACLE`, `CATABOLIC_PYTHON_REFERENCE` and
`CATABOLIC_NATIVE_CLI`, and skips with the missing names otherwise.

## Journal conversion

Native upgrade refuses WAL catalogs: it needs a real SQLite EXCLUSIVE lock,
and WAL mode does not exclude readers. `db convert-journal --to delete`
requires the same three quiescence assertions as native upgrade and refuses
without them before it takes a lock or opens SQLite. It then takes the native
lifecycle lease and the Catabolic writer lock. It refuses if
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

The checks cannot see everything, which is why the assertions are required:

- A process that has opened the catalog but not yet read it, such as a
  legacy Python process between connecting and its first query, holds no
  SQLite or WAL-index lock. Neither the probe nor SQLite detects it, and
  conversion proceeds. In the recorded test, such a connection's first later
  use reads the converted header and works in DELETE mode. That is SQLite's
  behavior for this case, not a guarantee that every idle user is safe.
- The probe and SQLite's locks cover processes on the same host only, not
  users on another host of a network filesystem.

## Native backup and controlled restore

`db native-backup` copies a native catalog with SQLite's backup API inside one
read transaction, so it does not need writers to stop. It validates the copy,
requires its identity manifest to equal the live catalog's, fsyncs it and
renames it into a new private directory (mode 0700) under `--backup-dir`,
which must exist, or under `<catalog>.backups`. Its `manifest.json`
(`catabolic-native-backup-v1`) binds the snapshot's SHA-256, database and
catalog IDs, incarnation, generation, schema and full identity manifest.
Legacy catalogs are refused; their backups come from `db upgrade` and are
restored to a new path by hand. Like restore, backup runs every file
operation through held directory descriptors: `mkdirat`, `openat(O_EXCL)`,
`renameat` and `fsync`, with the manifest written through the held backup
directory. SQLite reaches the new snapshot by name only between checks that
the names still resolve to the held descriptors and to the inode created
through them. Any fsync failure is an error.

`db native-restore --backup DIR` publishes such a snapshot at the live path.
It is fail-closed. Before anything in the backup is read, it requires the
three quiescence assertions and takes lifecycle exclusion: one restore per
process, the native lifecycle lease and the Catabolic writer lock. It then
opens the catalog's directory and the backup directory once, with
`O_DIRECTORY|O_NOFOLLOW`, and holds them. The backup manifest and snapshot
are opened through the held backup descriptor without following links. Each
must be a regular file with one link that is not the live catalog, one of
its sidecars or a hard link to either.

Existing restore records are read and classified next (see below), still
before any SQLite access to the live path. Restore then takes an exclusive
SQLite lock on the live catalog and checks rollback-journal mode and the
database and catalog IDs. It creates and holds the staging directory
`.<name>.restore-<id>/` beside the catalog and passes these checkpoints, in
order:

| Checkpoint | Work before it | After a crash here |
| --- | --- | --- |
| `staged` | Copies the snapshot from the held backup descriptor into a private `catalog.sqlite3` created through the held staging descriptor, and checks the manifest's SHA-256 against the copied bytes. Only this copy is verified or published from now on; backup paths are never reopened by name. | Live catalog unchanged; staging holds the copy |
| `verified` | Fully validates the copy and requires its schema, IDs, incarnation and identity manifest to match the backup manifest | Live catalog unchanged |
| `retained` | Creates `previous.sqlite3` through the held staging descriptor and keeps that descriptor, fills it with SQLite's backup API through the locked live connection, rechecks that the name is still that inode (and not the live catalog or a sidecar), and hashes it only through the held descriptor | Live catalog unchanged |
| `rotated` | Changes the staged copy's incarnation through the schema's restore-scoped permit, to a fresh value different from both the live and the snapshot incarnation. Checks that only `catalog_state` changed, that identities are preserved and that no worker ownership is left unresolved (see below). Records the complete state of the rotated file and writes the staging `manifest.json` record (`catabolic-native-restore-v1`, status `staged`). | Live catalog unchanged; record says `staged` |
| `protected` | Takes an exclusive SQLite lock on the staged copy. Under that lock, runs complete native validation (schema signature, history checksums, integrity, foreign keys, native state) and requires its complete state to equal the rotated file's: table digests, every schema object, full migration history rows, `sqlite_sequence` and the identity manifest | Live catalog unchanged |
| `publishing` | Nothing new; the final checks follow it. Restore rechecks the held directories, the staged entry, the live entry, the lease and the absence of `-journal`, `-wal` and `-shm` files, then writes the record with status `publishing`. A failed rename closes both connections and removes only this run's staging directory. | Live catalog unchanged; record says `staged` |
| `renamed` | `renameat` from the held staging descriptor into the held catalog directory, then requires the live entry to be the locked staged inode; on a mismatch nothing is touched and the record says `substitution_detected`. This rename is the single commit point. | Restore in effect; durability not yet confirmed; record says `publishing` |
| `published` | Fsyncs the held catalog directory | Restore in effect and durable |
| `finalizing` | Prepares the final record | Restore in effect; record still says `publishing` |
| `finalized` | Writes the record with status `published` and durability `durable` | Restore in effect; record final |

### Anchored file operations

Every create, open, rename, unlink and fsync in restore runs through a held
directory descriptor (`openat`, `mkdirat`, `renameat`, `unlinkat`, `fsync`),
never through a pathname that could have been swapped. Directory listings,
of restore records and of a staging directory being removed, are read with
`fdopendir`/`readdir` on a duplicate of the held descriptor. Records are written
as a private temporary file through the staging descriptor, fsynced, renamed
within it and followed by a directory fsync. Before each rename restore
checks that the held directories are still the entries it created. SQLite
opens files by name, so restore only points it at the live catalog and at
files in the held staging directory. Each such open is preceded and followed
by checks that the directory pathnames still resolve to the held descriptors
and that the staged or retained entry is still the inode created through
them. Writing the retained copy is the one place where SQLite opens a second
file by name while the live catalog is locked. A same-user swap of that name
to the live catalog in the instant between the check and SQLite's open is
the accepted residual risk of using the backup API. The bundled SQLite
offers no page-level copy that avoids the open, and serializing a whole
catalog into memory is not bounded. Anything
that fails before the rename leaves the live catalog's bytes unchanged and
removes the staging directory entry by entry through its descriptor; a crash
before the rename leaves it for the operator.

The exclusive locks mean that no SQLite user can read or write the live
catalog between verification and the rename, or the staged copy between its
final verification and the rename. A changed or replaced staged file is
refused. No descriptor is ever opened and closed on the live or staged file
while a connection holds it, because on POSIX closing any descriptor drops
every lock the process holds on that file. The descriptor of the private copy
stays open until after the staged connection closes and pins the inode the
rename must publish. One restore per process keeps a second restore from
opening and closing descriptors that could cancel the first one's locks. A
test probes the exclusion from a separate process; a probe in the same
process would be masked.

### Failures after the rename

After the rename the restore is committed. Any later failure, including a
failed directory fsync or record write, is an error that begins `restore
committed` and names the published incarnation. It is never reported as
success with a warning, and the operator should simply rerun. A rerun with
the same backup finds the record whose incarnation is the live one and
returns `already_published: true` without rotating again. It first retries
the directory fsync while holding the exclusive locks. If the fsync
succeeds, it reports and records `durable`. Otherwise it records `uncertain`
and fails with `restore committed` again.

### Restore records

If any existing restore record, its staging directory or the catalog's
directory cannot be opened or read, or a record is not valid, restore
refuses with `restore record reconciliation required` and changes nothing.
A valid record is valid JSON with every required field and type: contract,
status, database path, database and catalog IDs, both incarnations, backup
path and SHA-256, retained path and SHA-256, the live, staged and retained
file identities, and an optional durability. Because the staging prefix
names the catalog, a record whose database path or database ID differs is
also refused rather than skipped. An unreadable record could describe a
published restore, and treating it as absent would rotate the catalog again
and discard later writes. The operator inspects and repairs or removes the
record by hand. A staging directory without a record never reached rotation,
so it is ignored. A rerun after a crash before the rename performs the
restore. Successful restores keep their staging directory, with
`previous.sqlite3`, for the operator to inspect and remove. Idle, unused
descriptors and users on other hosts remain undetectable, as for journal
conversion, so the assertions are required.

Records identify files by the volume UUID of the catalog's directory and the
inode. They never store `st_dev`, which can change across reboots on macOS;
the device and inode pair is compared only within one run. Restore refuses
when the filesystem reports no volume UUID.

A rerun classifies a record left at `publishing` by the live file's identity. If
the live file is still the previous one, the rename never happened and the
record becomes `abandoned`. If it is the staged file, the restore was
published. Any other file needs reconciliation.

### Threat model and detected substitution

The staging directory has mode 0700, so only processes running as the same
user, and root, can touch the staged file. The design defends against crashes
and against same-user processes acting outside the quiescence assertions. It
does not defend against root.

A pathname check always leaves a window before the rename. So the rename is
also bound to the inode that was verified and locked: immediately after it,
restore compares the live path's identity with the held staged descriptor. On
a mismatch the published file is a substitution. Restore does not touch the
live path or try to roll back. It leaves the unexpected live file and any
sidecars where they are, leaves `previous.sqlite3` in the staging directory,
sets the record to `substitution_detected`, and fails with `hostile
substitution detected` and `restore record reconciliation required`. Reruns
refuse in the same way while that record exists. Records are read and
classified before any SQLite access to the live path, because opening it
would replay or checkpoint a hot journal or WAL left with the unexpected
file and destroy that evidence.

To recover by hand:

1. Quiesce every user of the catalog.
2. Check the live path for `-journal`, `-wal` and `-shm` sidecars and keep
   them with the unexpected file for investigation.
3. Verify `previous.sqlite3` against the record's `previous_sha256` (its
   whole-file SHA-256) and `retained_identity`.
4. Copy the unexpected live file aside, then restore `previous.sqlite3` over
   the live path by hand.
5. Validate the result with `db native-inspect --full`, then resolve or
   remove the record before the next restore.

`rename_window` is a checkpoint used only by these tests.

### Test hooks

The observation hooks (`restore_observed`, `backup_observed`,
`convert_observed`, the checkpoint and hook name constants, and every hook
call) exist only with the catabolic-store `failpoints` feature. That feature
is off by default and is required by the `native_restore` and
`journal_conversion` test targets; build them with `--features
failpoints`. Without it every hook call compiles to `Ok(())`, and the
production `restore`, `backup` and `convert` take no observer.
catabolic-cli and the wheel do not enable the feature, and a CLI test asserts
`catabolic_store::FAILPOINTS_ENABLED` is false in the shipped build.

Restore refuses backups with retained worker ownership. Cleanup witnesses for
completed processing jobs and observation companions name the incarnation
that sealed them, and the schema's guards forbid rebinding them. Rewriting
them under the new incarnation would mint evidence that no runtime owner
produced. So the rotated copy's worker inventory must admit new work. A
backup that holds completed but witnessed workers, or unfinished ones, is
refused before publication. Take the backup when no worker ownership is
retained. A future evidence-carrying reconciliation would need its own
reviewed schema contract.

Rotating the incarnation is what fences the old catalog's users. A live native
session holds shared admission, so a restore is refused while one exists.
After a restore, a permit for the old incarnation fails with
`catalog_stale_incarnation`. A command admitted under the old incarnation whose
receipt is not in the backup is reported as `outcome_unknown_after_restore`
and is not executed again, and one whose receipt is in the backup replays its
original result. Existing native handles also check the catalog's file
identity and incarnation before use and require a reopen.

`crates/catabolic-store/tests/native_restore.rs` covers:

- the backup, a backup directory swapped for a symbolic link before the
  manifest is written, and a restore round trip;
- the stale-incarnation refusals above;
- refused backups: missing assertions, another catalog's backup, a changed
  snapshot, an edited manifest, a legacy backup, a WAL live catalog, and
  completed or unfinished worker ownership;
- exclusion of another process from the live and staged catalogs until
  publication;
- refused staged mutations and replacements, and refused staged schema,
  migration-history and sequence changes;
- injected failures at the rename and after it, reported as committed;
- a staging directory swapped for a symbolic link at `finalizing`, a snapshot
  swapped for a symbolic link to a locked catalog, and a backup directory
  swapped after it was opened;
- a replacement at `publishing`, refused before the rename;
- a substitution in the rename window, detected and preserved for
  reconciliation;
- rerun durability after an uncertain publication;
- unreadable or corrupt restore records, and a directory swapped in during
  record enumeration;
- volume-UUID file identities in records;
- a rerun after a detected substitution that leaves a hot journal or WAL
  byte-identical;
- a process exit at every checkpoint, followed by a rerun.

Each refusal leaves the catalog unchanged. A restore returns the database to
the backup point only; it does not undo filesystem or network effects made
since.

## Post-upgrade write limitations

These are asserted as specific refusals by the harness, the CLI tests and the
frozen Python check. None of them changes the upgraded catalog.

| Limitation | Refusal |
| --- | --- |
| Legacy recovery admission on schema 39 (`Store::open` for recovery) | `native catalog recovery requires separately qualified recovery admission` |
| Legacy upgrade path on a native catalog | `native catalogs cannot be downgraded by the legacy upgrade path` |
| Frozen Python on schema 39 (`db status`, `status`) | `unsupported database schema 39` |
| Native schemas newer than 39 | `unsupported native schema target 40` |
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
`--test journal_conversion --features failpoints`, `--test native_restore
--features failpoints`, and, separately and without that feature, `-p
catabolic-cli --test native_db_commands`).
Then run the compiled executables directly, with `TMPDIR` on a physical
volume. Add `--ignored scale_database_rehearsal` for the scale fixture.
