<!-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors -->
<!-- SPDX-License-Identifier: MIT -->

# Native catalog schema 33: source, reproduction, and contract

Status: schema-only packaging and regression evidence. The prepared schema is
integrated as additive migration 033; this document and its Python tests do not
qualify Rust transaction owners, installed packages, release readiness, or the
full R2 milestone. The independent schema review remains **incomplete** following
its platform error. Its exact error/status is preserved without retry,
reassignment, or a substitute review in
[`schema-review-blocked.json`](../tests/parity/native-catalog-schema/schema-review-blocked.json).
The original absolute target in that historical record is evidence only, not a
runtime dependency.

## Reproduce and test

Run from the repository root with Python 3.11 or newer and its standard-library
SQLite module; no application import, Rust helper, or Python package dependency is
required:

```sh
python3 scripts/native_catalog_schema.py --check
python3 -m unittest tests.test_native_catalog_schema -v
python3 -m unittest tests.test_native_catalog_schema_generation -v
```

The first command checks all four generated artifacts without writing files.
The next two commands run the 37 original schema regressions and seven separate
packaging/reproducibility checks. Both modules participate in ordinary unittest
discovery and are outside the frozen legacy parity ledger. Passing these checks
is schema-only evidence, not Rust-owner or release qualification.

To regenerate a candidate tree for comparison:

```sh
python3 scripts/native_catalog_schema.py --output-dir /tmp/catabolic-schema-candidate
```

The output directory uses the same relative paths as the checkout. SQL and the
native signature must match the integrated files byte-for-byte; no frozen
migration or historical reference/signature is rewritten. The generator's
inputs resolve relative to its own repository, so invoking it from another
working directory or copying the relevant repository tree does not depend on a
preparation workspace. Migration discovery explicitly requires exactly one file
for each version **001–032**, ignoring 033 and later versions.

The generator has no write-on-import behavior. Its default is the same read-only
check as `--check`. `--output-dir` emits only these four files:

- `crates/catabolic-store/resources/migrations/033_native_catalog.sql`
- `crates/catabolic-store/resources/native-schema-33.json`
- `tests/parity/native-catalog-schema/generation-revision-inventory.json`
- `tests/parity/native-catalog-schema/preparation-manifest.json`

The SQL source is
`crates/catabolic-store/resources/native-catalog/033_native_catalog.base.sql`;
the generated trigger logic is in `scripts/native_catalog_schema.py`.
The separate normalized signature is an ordered `[type, name, sql]` array,
independent of every historical `reference.json` signature.

The inventory covers all 147 tables, keys, generation/entity-revision coverage
and exclusions, pre-existing fallback triggers, and statically discovered Rust
mutation-owner files. The file scan is a discovery aid; it cannot prove every
dynamic SQL statement or indirect entrypoint. Rust source changes can change
this inventory without changing generated SQL or the native signature.
Reproduce it against the final integrated source before recording final evidence.

[`source-provenance.json`](../tests/parity/native-catalog-schema/source-provenance.json)
retains the original preparation hashes, exact 37 test names, legacy migration
hashes, and the blocked review record hash. The hand-authored base, integrated
SQL, and native signature remain byte-identical to those original artifacts.
The tests retain the original fixtures/assertions, with repository-relative
paths and the explicit legacy migration boundary replacing temporary paths.
The Python UUID/query/digest UDFs are schema-test stand-ins, not production-owner
qualification. Migration tests use SQLite's complete-statement parser rather
than `executescript`, which would implicitly commit an enclosing transaction.

The migration adds 21 tables and two read-only views. The preserved regression
suite checks that populated schema-32 row values and pre-existing object SQL
survive an additive upgrade, including `meta`, `fallback_epoch`, association IDs,
and raw metadata. It also covers rollback, scoped permits, immutable history,
replacement collisions, generation/revision exhaustion and tombstones,
receipt retention, typed associations, and required connection settings.
The following sections record the prepared SQL/API contract; production owner
conformance needs its own evidence.

## 1. State and identity

`catalog_state(id, catalog_id, incarnation, generation, contract_version)`:

- Exactly one row, `id=1`; `catalog_id` identifies the existing database
- `incarnation` is exactly 32 lowercase hexadecimal characters from 16 random
  bytes, generated on migration or initialization
- `generation` is an SQLite INTEGER in `0..=i64::MAX`, initially 0
- `contract_version=1`
- Row deletion, identity edits, generation decreases and REPLACE are refused
- Ordinary open/reconnect preserves incarnation and verifies the live state
- Incarnation changes need an explicit restore-scoped transaction permit;
  this SQL facility does not implement or authorize restore publication
- Controlled restore must quiesce cross-process admission and all live owners;
  filesystem restore remains unavailable until that owner is qualified

Read `SELECT catalog_id,incarnation,generation,contract_version FROM catalog_state
WHERE id=1`. The migration-only deterministic `catabolic_catalog_id_v1(value)` function
normalizes the new catalog_id copy through the existing UUID parser. The runner
installs/removes it for step33 only; no persistent trigger uses it. Legacy meta
bytes are untouched, including accepted uppercase/simple/braced/URN spellings.

## 2. Generation and entity revisions

137 source tables receive INSERT, DELETE and changed-value UPDATE triggers.
Triggers compare each declared source column with `IS NOT`; a no-op UPDATE does
not change the native generation or entity revision. Every changed source row
advances generation. Generation is an opaque monotonic invalidation token, **not
an exactly-one-per-transaction counter**. Compound commits can advance it several
times. The final value is stable once the mutation/projection writes finish.

Ten exclusions are explicit:

- `schema_migrations`: lifecycle metadata
- `fallback_epoch`: derived legacy epoch already covered at its sources
- `fallback_probe_slots`, `remount_guard`: internal temporary guards
- `catalog_state`, `catalog_entity_revisions`: counters themselves
- `catalog_commit_barrier`, `catalog_mutation_permits`: transaction mechanics
- `catalog_command_receipts`, `catalog_receipt_holds`: receipt bookkeeping,
  excluded so the receipt's committed generation is the actual final mutation
  generation

The coverage is deliberately conservative, including job/availability/control
changes that may cause harmless additional invalidation. Audit inventory found
25 old tables with no fallback-epoch trigger and three component tables with
partial event coverage. Merely mirroring old epoch changes is insufficient.

`catalog_entity_revisions(entity_kind, entity_key, revision, deleted)` retains a
monotonic current revision per source row. `entity_kind` is the exact source table
name. `entity_key` is SQLite `json_array` of primary-key values in PK order.
Backfilled live rows start at revision1; a delete sets a tombstone and advances
revision; a reinsertion advances the existing tombstone rather than resetting to
1. PK changes retire the old key and start/advance the new key. This table is an
internal revision index, not a new public query dialect.

All counters have integer checks plus explicit BEFORE-advancement exhaustion
checks using `RAISE(ABORT)`. This is essential: an outer OR IGNORE/OR FAIL conflict
policy can suppress a CHECK or preserve earlier statement changes. Explicit ABORT
refuses the complete source statement even under IGNORE, FAIL or REPLACE. The
command owner must still roll back its whole transaction on any error.
No wrapping, saturation, counter reset or key recycling is allowed. Rows in
immutable native history tables carry their own typed revision number; the
internal source-row counter is a separate invalidation/reconciliation aid.

## 3. Unclassified legacy backfill

`catalog_legacy_associations` retains each original association ID, original
file/item owner IDs and full original row as JSON, including metadata as the
original raw string. Every migrated record starts `unclassified`.

`catalog_legacy_item_metadata` retains raw effective item metadata as a
`legacy_import` snapshot. It does not overwrite independent known legacy evidence
or fabricate manual/verified provenance.

These immutable audit tables intentionally have no FKs to live legacy rows.
Deleting an unclaimed legacy association or item remains supported. The
`catalog_legacy_association_classification` view derives `claimed` from a typed
binding; it also reports whether the original live association is still present.
The raw historical audit mapping is never rewritten when classification occurs.

Migration creates no timeline, version item, exact revision, binding, duration,
segmentation, equivalence or declared/verified evidence. Later classification
must be one reviewed typed command whose immutable receipt contains original IDs
and owners, exact evidence, new IDs/revisions and explicit reference mappings.
Any transformation of historical rows needs its own preservation rule; the
existing whole-row migration preservation check remains unchanged.

## 4. Typed identity and histories

- `catalog_timelines`: stable timeline ID, ordinary edition item ID, current
  revision; `catalog_timeline_revisions` retains provisional/established state
  and optional measured interval
- `catalog_versions`: stable version ID, unique dedicated association item,
  explicit segmentation context, current revision
- `catalog_version_revisions`: exact timeline revision, draft/ready state,
  optional/required declared coverage, optional declared/verified equivalence
  evidence, command key and timestamp
- `catalog_bindings`: stable binding ID, immutable version owner and unique
  association ID; real FK to `item_files` prevents historical typed claims from
  disappearing through a legacy delete
- `catalog_binding_revisions`: revision, profile/file/exact file revision,
  integer part, whole-file or slice extent, placement, optional normalization
  evidence and immutable command reference
- `catalog_version_bindings`: immutable whole-version snapshot membership,
  linking each version revision to exact binding revisions
- `catalog_exact_file_revisions`: retained profile/file/revision and snapshot;
  capture must use the existing Components revision contract, not a new digest
  rule. The v1 Components revision excludes ctime; capture must account for that
  before deciding that a previously retained snapshot is inconsistent
- `catalog_identity_evidence`: immutable declared/verified/legacy_import records;
  equivalence FKs accept only declared/verified evidence, never legacy_import

All stable identifiers are bounded by 256 UTF-8 bytes in these native tables.
Core identifier validation remains responsible for whitespace/control-character
rules. All part/revision/time coordinates have INTEGER checks. Parts are bounded
by u32::MAX. Intervals are nonnegative, nonempty, half-open; the initial placement
must preserve duration. Unknown whole-file duration stores all source/placement
coordinates NULL and never becomes a fabricated known interval.

Head updates advance exactly one revision and preserve stable ownership fields.
Revision rows append consecutively. History rows and snapshot membership cannot
be updated or deleted. Their command FK is deferred until commit, so no typed
revision can commit without its receipt. Once the receipt exists, old snapshot
membership and history cannot be extended. Explicit BEFORE INSERT guards close
INSERT OR REPLACE bypasses even if recursive_triggers is disabled. New heads and
immutable history must use INSERT, never UPSERT/REPLACE; head advancement uses an
expected-revision UPDATE.

Binding identities do not have an independently writable active revision pointer:
the active version's complete membership is the authority. This avoids two heads
that can disagree about which binding revision belongs to a ready version.

## 5. Segmentation is explicit and contextual

`catalog_segmentation_contexts` has a stable ID plus unique trusted
`(authority_namespace, context_key)` and revision. The API resolves/authorizes
this editorial context; an untrusted caller does not bypass overlap rules by
inventing an ID or namespace.

`catalog_segmentation_sets` is unique by `(context_id,profile,file_id,file_revision)`.
Renaming the set cannot split one exact revision's overlapping uses inside the
same context. `catalog_segmentation_revisions` and `catalog_segment_members`
retain complete active context snapshots. SQL validates a segment's exact
profile/file/revision, source_kind=slice, version context and nonoverlap within
the context/set snapshot. Different approved editorial contexts can deliberately
reuse the same file; that is not an inferred global equivalence.

The typed mutation owner must load the complete existing context membership,
including unchanged versions, before applying a proposed change. It must
reconcile context membership with active version heads, not only trust submitted
members. A context revision is a complete snapshot, not a partial patch.

## 6. Transaction-bound owner permits

`catalog_mutation_permits(scope_kind,scope_id,incarnation,barrier_id)` is private
to the typed command implementation. Scope kinds: `version`, `timeline`,
`context`, `restore`. `barrier_id=1` references the deliberately empty
`catalog_commit_barrier` with an INITIALLY DEFERRED FK. A trigger forbids populating
the barrier. Permit insert verifies the current incarnation.

Protocol:

1. Enter the existing borrowed Store writer transaction; verify FK enforcement,
   current file identity/incarnation, authority, receipt replay decision,
   expected generation and all entity/membership preconditions
2. Insert narrow permits for the affected version/timeline/context IDs
3. Apply all typed and losslessly adapted legacy writes on this same connection
4. Validate complete final versions, timelines, exact revisions, memberships and
   segmentation contexts using existing core identity/component owners
5. Delete permits and verify no permits remain; recheck lifetime/incarnation
6. Read final generation, construct/persist the immutable result receipt last
7. Commit; on any error or panic roll back the entire transaction

A forgotten permit causes COMMIT to fail with a deferred FK. SQLite leaves that
failed transaction active, so the owner must explicitly roll back or use its
RAII transaction wrapper. ROLLBACK and savepoint rollback remove permits with all
other changes. SQLite permits only one writer, and production connections must
set and verify `foreign_keys=ON`, `recursive_triggers=ON`, and
`read_uncommitted=OFF` on every native schema33 connection, including migration
validation/snapshot/reopen connections. Schema32 connection behavior is unchanged.

The recursive-triggers requirement is a native connection contract, not SQL-only
protection. SQLite otherwise suppresses DELETE triggers for rows displaced by
INSERT/UPDATE OR REPLACE, leaving stale per-key revision tombstones after a
secondary UNIQUE collision. With it enabled the displaced key is retired
correctly. Targeted tests cover both collision forms and the existing
job→request→event/notification/reservation trigger chain, including last-insert
identity behavior. Disabling any prerequisite invalidates native admission.

Legacy `item_files` INSERT/UPDATE/DELETE checks both before/after claimed
association IDs and dedicated version owners. A scoped permit allows legitimate
typed writes; an unrelated version permit does not. Dedicated owner item edits
check both OLD and NEW IDs, including UPDATE OR REPLACE collisions; typed
head/history writes have corresponding guards. Unclaimed legacy writes
continue through the old owners. Claimed association deletion still fails its
history FK even with a permit; retain/inactivate it instead.

This is application invariant protection, not raw-SQL authorization. A privileged
SQL connection can bypass application guarantees; it is not a mutation API. The
permit is never exposed as a public capability or trusted from request JSON.

## 7. Receipts, expiry and storage

`catalog_command_receipts` primary key is exactly
`(catalog_id,host_namespace,operation_id)`. It retains contract/canonicalization
version, payload digest, admission incarnation, expected generation, committed
generation, committed/retention timestamps and either the original full result
or an expired tombstone. Digest canonicalization and max1MiB payload/max4MiB
result validation come from the staged core `catalog_command` owner. SQL bounds
result bytes to 4MiB; core validates depth, node count, finite JSON, canonical
bytes and envelope semantics. SQL does not recanonicalize or hash JSON.

New receipts are full, match current incarnation and final generation. Updates
allow only full-to-expired compaction after retain_until_us and with no active
hold. Key/digest/envelope/result identity never changes. Deletion, REPLACE,
restoring a compacted result and recycling an operation key are refused.

`catalog_receipt_holds` keeps immutable acquire identity/time and one-way release
time; released hold rows remain for audit. It can only attach to a full receipt.
Holds and compaction run under a writer transaction. The trusted compaction API
uses actual current time, not a client-claimed expiry timestamp.

Receipt lookup has no incarnation filter and happens before stale precondition
checks: identical retries return the original body under current authorization,
conflicting payloads fail, tombstones return receipt_expired, and a missing
receipt from an old incarnation returns outcome_unknown_after_restore. A current
status-only authority gets redacted committed status rather than old body data.
No automatic new operation ID or retry of uncertain work.

The storage-admission API must apply its explicit configured retention/storage
budget before new commands; when exhausted it refuses new commands rather than
recycling permanent keys. No background compactor, quota policy, or automatic
hold scheduler is installed by this schema package.

## 8. Required owner checks not replaced by SQL

The SQL design deliberately does not duplicate the complete core validator in
hundreds of ad hoc SQL constraints. Before a typed command commits, its owner
must validate:

- Full active association sets and their exact owner/file/part/current revision
- Ready versions: consecutive 1..N, unique files/associations, exact complete
  coverage, one timeline, no overlap, and declared coverage matching timeline
- Unknown encodes: distinct provisional timelines unless declared/verified
  evidence explicitly supports membership; no duration/digest/title inference
- Measured evidence and snapshot hashes captured/revalidated through existing
  owners; exact-file table SQL alone cannot prove a supplied snapshot or hash
- Whole context membership, including unchanged parts, and no replacement of
  current context with a caller-selected incomplete snapshot
- Explicit permission for context/equivalence/provenance decisions and all
  reviewed identity/reference mappings in the immutable receipt
- No unsupported transformation/rehome/deletion of historical legacy rows
- Final FK integrity, no permit remnants, admission incarnation and source file
  descriptor identity, then receipt-last atomic commit

Until this owner exists and is qualified, schema33 must not expose direct typed
writes as a public API. Preparation tests demonstrate schema safeguards only.

## 9. Legacy read and effect admission

Mutation guards alone cannot prevent a legacy reader from treating a slice as
the entire backing file. Add explicit adapter gates before admitting typed use:

- Fast claim lookup: `catalog_bindings.association_id` UNIQUE index
- Dedicated owner lookup: `catalog_versions.association_item_id` UNIQUE index
- Read-only `catalog_active_binding_revisions` joins active complete version
  membership, exact binding/source/placement, edition/timeline/context identities
- Legacy fallback/component/effect planning must refuse claimed slices and any
  typed semantics it cannot represent losslessly, with explicit unsupported
  contract outcomes
- The new typed resolver must load its facts from these authoritative records,
  pass through the existing validated version/component/fallback owners and keep
  slice publication/playback effects blocked until an interval-aware adapter is
  separately qualified

A typed match or read does not make publication ready, grant authority or permit
execution of whole backing files. The legacy SQL/JSON/Python contract remains
unchanged for schema32 and losslessly representable schema33 paths.

## 10. Outstanding review and qualification gates

Independent schema review remains incomplete after the recorded platform block.
Do not retry or reroute that review, or treat these ordinary regression tests as
clearance. The blocked record is preserved byte-for-byte, and the generation
manifest explicitly labels this evidence as schema-only.

The prepared regression cases include the already reported new-owner
UPDATE OR REPLACE guard, explicit exhaustion ABORT, and displaced-key tombstones
under required native connection settings. Packaging these tests does not
constitute another independent review of the schema.

Rust migration-owner qualification requires separate evidence for exact backup,
rehearsal and plan digests; interruption before/after COMMIT; exclusive admission
and SQLite ownership; checksums; schema-33 reopen/reconnect; and historical row
preservation. Command-owner evidence must cover duplicates/conflicts and
concurrent calls, receipt-last atomicity, held expiry, storage exhaustion, and
restore-after-commit uncertainty. Typed ownership also requires full
owner/read/effect tests. Findings and pass counts in those suites must be stated
separately from this schema-only package.

Preserve all old SQL/resources/signatures and schema-32 defaults. None of these
artifacts closes a frozen ledger gate or authorizes Motion integration, external
actions, commits, pushes, or publication. Final integrated source hashes are
recorded by the integration receipt, after all owners finish their changes.

## 11. Schema 38 review guards

Migration `038_native_review_guards.sql` (native signature
`native-schema-38.json`) closes schema-review findings SC1–SC5 of e8f0c52.
Migrations 033–037 are released, checksummed in `schema_migrations` and pinned by
their per-version signatures, so schema 38 is purely additive: new triggers, one
new table, no rebuilt or rewritten historical DDL, no existing-row mutation and
no generation change. `python3 scripts/native_catalog_review_guards_schema.py
--check` reproduces the signature and
`tests/parity/native-review-guards/schema-manifest.json`. Native catalogs reach
schema 38 only through the explicit `plan_native`/`upgrade_native` path (from any
native 33–37 or legacy 32 catalog); frozen Python still refuses every schema
above 32.

- **SC1, suppression subjects.** Item/file tag-suppression heads cascade with
  their subject, but suppression history is retained. As with identity pins, a
  subject that has a suppression head is now managed until an explicit
  retirement owner exists: deleting it, `REPLACE` (a delete under the required
  `recursive_triggers=ON`) and rekeying abort with
  `catalog_tag_suppression_subject_retirement_required`. A subject whose heads
  were already cascaded away before the upgrade cannot be recreated (or rekeyed
  into) under the same ID (`catalog_tag_suppression_subject_history_retained`),
  because its next decision would restart at revision 1 and collide with the
  retained history. Upserts of existing subjects are unaffected.
- **SC2, metadata provenance receipts.** SQLite cannot add a foreign key to the
  existing `catalog_metadata_subjects`/`catalog_metadata_history` tables without
  a rebuild. Insert triggers instead record each distinct command key once in
  the immutable `catalog_metadata_receipt_obligations` table, whose composite
  `DEFERRABLE INITIALLY DEFERRED` foreign key must resolve to
  `catalog_command_receipts` at COMMIT. Receipts are permanent, so a satisfied
  obligation stays satisfied. The migration backfills obligations from existing
  rows; an orphaned key makes the migration's full foreign-key check fail and
  the upgrade roll back. Obligations do not advance the catalog generation.
- **SC3, receipt sealing.** The schema-33 `catalog_*_revisions_uncommitted`
  rule now also covers metadata subjects/history, identity-pin revisions,
  tag-suppression revisions, tag merges, and logical and version retirements.
  Each refuses an INSERT whose command key already has a receipt
  (`catalog_committed_history_sealed`); receipt-last owners are unaffected.
- **SC4, execution cleanup witness.** Schema 36's terminal-state CHECK evaluates
  NULL, which SQLite accepts, for a quiescent row with a NULL `cleanup_kind`.
  NULL-safe INSERT/UPDATE triggers require the complete witness
  (`cleanup_kind IS 'builtin_no_child_v1'`, zero obligations and retirements,
  released capacity and `sealed_at`) for every non-owned row. Full validation of
  every native catalog at schema 36 or later rejects any persisted terminal row
  without that witness. This blocks native planning, upgrade and backup
  verification of a damaged catalog.
- **SC5, backup durability.** Native and legacy upgrades now create each missing
  backup ancestor individually (mode 0700) and fsync the directory containing
  each new entry before writing the verified backup.
