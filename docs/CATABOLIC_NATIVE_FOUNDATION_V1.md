# Native catalog foundation contract v1

Status: R2 development integration. Independent schema review remains incomplete;
this document does not qualify release, cutover or restore. The frozen R1 source
is commit `555b9455864070646a20aad401e8f0cab460b884`.

## Compatibility and explicit admission

Legacy initialization and upgrade still target schema 32. Native initialization,
planning and upgrade explicitly target schema 33 through the existing migration
owner. Opening a catalog never upgrades it. Historical migration SQL, signatures
and Python behavior are retained. Native schema uses a separate signature.

Native connections require foreign keys and recursive triggers enabled and
read-uncommitted disabled. These prerequisites apply only to schema 33; imposing
them on legacy connections would be an unqualified behavior change. New native
history and generation guards depend on these prerequisites, which are checked at
admission and transaction boundaries. Raw external SQLite writers are unsupported
unless they establish the same contract.

Native generation is a monotonic invalidation token, not a transaction count.
Every changed source row may advance it, including conservative control-state
changes. No-op updates do not advance it. Counter exhaustion refuses changes;
keys and tombstones are never recycled. A new incarnation is created at native
initialization or upgrade. Ordinary reopen does not rotate it.

Legacy association and raw-metadata backfill preserves original identities and
values as unclassified history. It does not infer duration, move ownership,
assert timeline equivalence or manufacture accepted identity. Native typed claims
must be created through the typed command owner.

## Upgrade and lifecycle

The native upgrade plan binds database identity, path device/inode, table data,
full migration history, autoincrement sequence, pending work, journal mode,
migration resource checksums and native schema signature. Upgrade requires exact
plan equality, verified backup, rehearsal and a real SQLite exclusive application
transaction in addition to writer and native lifecycle admission locks.

The operator must explicitly attest that admission, readers and workers have
stopped and operational ownership is exclusive. Cooperative locks do not prove
that an unsupported process has stopped. Cross-profile pending work and expired
claims conservatively block upgrade. Native WAL cutover is currently refused
without changing journal mode; legacy WAL support remains unchanged.

Native sessions retain catalog incarnation and file identity. Native admission
sidecars use shared reader leases and exclusive lifecycle leases; inherited
handles cannot silently become fresh admissions. Detached operations must
revalidate admission and catalog identity before acknowledging an external result.
A failed reopen leaves the owner unusable. Session reconnect invalidates native
plans and evaluations while preserving handle counters and shared plan-owner
identity. These checks do not claim protection from arbitrary hostile filesystem
races.

There is no native restore publisher in this slice. SQL restore-scoped permits
are transaction primitives, not permission or an implemented recovery workflow.
Existing recovery admission does not expand to schema 33.

## Durable command boundary

Versioned core envelopes use bounded strict JSON parsing, canonical payloads and
scoped operation keys. Digests bind contract version, key, incarnation, generation
and payload. Known receipts are reconciled before stale generation checks under
current trusted host authority. An absent receipt from a previous incarnation
returns uncertainty and cannot run again automatically.

The store command owner owns the top-level transaction and admits typed
operations only. Mutations and their immutable result receipt commit together;
network and filesystem effects do not belong in this transaction. Explicit
retention and storage limits are host policy. Full receipts can compact only
after their deadline and active holds end. Permanent expired tombstones prevent
re-execution. Storage exhaustion refuses new commands instead of evicting keys.
Privileged host authority is not an authentication mechanism and must never be
deserialized directly from a caller's request.

## Persistent identity and legacy adapters

A native identity command is scoped to one trusted editorial context and appends
complete version/binding snapshots over existing dedicated association owners.
It never implicitly rehomes associations. Revisioned equivalence evidence binds
exact timeline/version/file revisions. Reads load the complete persisted context
before using the existing whole-version composition owner; caller-supplied
manifests are not authoritative for schema 33.

Legacy paths may represent only lossless whole-file decisions. Slice, multipart,
draft, stale or otherwise ambiguous typed bindings must fail closed rather than
silently return an unrestricted file. Interval-aware effects remain unsupported
in this slice; native read diagnostics do not authorize packaging or publication.

## Evidence and remaining gates

The frozen development checkpoint uses the user-approved standard Rust 1.99
Linux runner, not Kyoto. The core/store/runtime aggregate passed **718 tests,
zero failures**, with nine ordinary ignored entries across 59 harnesses. The
rebuilt real-helper test passed separately, and two ignored fork helpers ran
through their passing wrappers. Six other opt-in cases remain unrun. Strict
Clippy, Rust formatting, 46 Python schema/audit tests and pinned Ruff checks passed.
All 732 frozen source/resource/input files remained unchanged before and after.
See [`native-catalog-r2-checkpoint.json`](../tests/parity/native-catalog-r2-checkpoint.json)
for exact commands, source/helper identities, failed-run evidence and limitations.
This is bounded local foundation evidence, not full-workspace or release closure.

The independent staged schema review was blocked on 2026-10-14 and remains
incomplete. Its exact result and scope are retained in qualification evidence;
it has not been retried or reassigned. Ordinary implementation and targeted tests
do not substitute for that review. Native WAL cutover, controlled restore,
platform qualification and the remaining product milestones remain open. The
recorded aggregate qualifies only its exact source and declared test scope. External Motion testing
and changes are deferred by the user's Catabolic-first direction. All existing
M0–M11 obligations remain unless an explicit versioned replacement is approved
and recorded in the contract ledger.
