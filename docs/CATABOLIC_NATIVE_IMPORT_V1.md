# Native import v1: bounded logical cutover

Status: a bounded Catabolic-only integrated slice, qualified together with V2 in
`tests/parity/native-catalog-r6-checkpoint.json`. This contract does not complete R6,
qualify external Motion integration, authorize production cutover, or close any
R0–R7/M0–M11 gate. No Motion source or tests are used.

## One owner, two bounded stages

`catalog_import` extends `catalog_commands::execute_with_authority`. It does not
create a job engine, schema, writable secondary catalog or filesystem protocol.
Every import action uses the existing top-level command writer transaction,
canonical payload digest, immutable result, generation/incarnation checks,
retention accounting and permanent expired-key tombstone.

A v1 `SnapshotManifest` names an import ID, source namespace and source snapshot
ID. Ordered batch descriptors contain the canonical SHA-256, byte count, logical
item count and unresolved-evidence count. Its `manifest_digest` binds that whole
input, including batching/order. Digests use the existing command canonical JSON
rules, not original export whitespace or a new hash interpretation. Preserve the
original authoritative export separately.

Bounds are 16 batches, 64 KiB per batch, 64 logical items and 64 evidence records
per batch. No empty batch or empty manifest is admitted. IDs are exact bounded
UTF-8, with no normalization/trim-based merging. Unknown request fields refuse.
The outer command owner's JSON depth/node/body limits still apply.

1. `Prepare` creates an immutable manifest receipt
2. `Stage` checks an exact indexed batch against that manifest and retains it
3. `Validate` verifies complete coverage, distinct source/destination/evidence IDs,
   supported logical item syntax and no current destination collisions; it returns
   proposed explicit source-to-item mappings, their digest and the validation
   generation
4. `Activate` can atomically apply the qualified logical-only subset through the
   existing curation `item_put_typed` owner, and commits the activated mapping,
   reference acknowledgment and import receipt in that same transaction

No item, file, identity, availability, selection or search document is created by
steps 1–3. Before activation, the only authoritative staged storage is immutable
command results. Every import receipt and its existing receipt hold commit
atomically. Its hold uses holder namespace `catabolic.import.v1` and hold ID
SHA-256(import ID). All bytes/counts are charged to the same explicit host receipt
policy; exhaustion rolls back the entire action. There is no expiry window between
staging and acquiring its hold. The host must retain those holds until migration
reconciliation is complete. Explicit release/compaction preserves permanent keys;
an expired prerequisite refuses resume rather than reimporting.

The command key is deterministic: `catalog-import-v1/<SHA-256(import ID)>/<step>`,
with `prepare`, `batch/<index>`, `validate`, or `activate`. The scope remains the
existing catalog ID and host namespace. The dispatcher reserves this prefix.
Different input or preconditions under an already used key conflict. Retrying an
operation requires its original host-persisted envelope, not a newly constructed
one with updated generation/incarnation. A known receipt replays before stale
checks and requires the current independent receipt-read capability. An absent
old-incarnation intent returns `outcome_unknown_after_restore`.

## Activation admission and one-authority handshake

Activation needs both the exact `ImportAuthority` and a separate
`TrustedImportActivation`. Neither is deserializable from application input.

The host first stops new catalog/scanner admission using its existing
`HostAuthorization` gate, retaining each admitted worker's lease until its actual
exit/recovery boundary. `TrustedImportActivation::after_drain` takes the exact
`Revocation` for that gate and refuses if any lease remains or an unrelated gate
is supplied, even if its namespace/revision strings are equal. The closed gate
stays owned through command COMMIT. Dropping it cannot reopen admission.

This is an explicit trusted-host contract, not a discovery mechanism: the host
must ensure all relevant writers/workers participate and must hold the source
snapshot frozen. It must persist its complete external reference audit and the
acknowledgment before constructing `ActivationBinding`. The binding names the
catalog/incarnation, import ID/input digest, source snapshot, validation generation,
reference acknowledgment and full proposed-map digest. The activation payload
also binds the acknowledgment and mapping digest. A request field or an opaque
ack ID alone cannot establish those facts.

The command re-reads all staged batches and checks collisions, the complete mapping
and the unchanged validation generation while holding its writer transaction.
An unrelated catalog write after validation is conservatively stale; this slice
does not silently refresh a reviewed mapping or recycle the validate key.
It reuses the existing native-upgrade pending-work inventory and refuses retained
observation/watcher worker pointers, even in terminal rows. It does not infer exit
from a PID, terminal status or expired lease. Rollback-journal admission reuses the
existing owner; WAL activation is explicitly refused. No worker is adopted, killed
or recovered, and no publication or restore action is invoked.

For a clean logical-only snapshot, all requested IDs are created with the existing
curation owner. Existing IDs are collisions, never overwritten/auto-merged. Raw
metadata stays legacy/unknown-provenance metadata; it is not recast as a provider
contribution, local curation or accepted identity. The original import/source
provenance is retained in the protected snapshot/command receipts. Native35 search
maintenance remains the normal dispatcher flush in this same transaction.
All items, generation, search changes, result and retention hold commit or roll back
together. No transaction crosses an await/network/helper/filesystem action.

An activated receipt is only Catabolic's half of cutover. The trusted host reads
its own durable checkpoint and the original receipt. `reconcile_activation` uses
these production decisions:

- Missing, redacted, expired, mismatched or wrong-incarnation receipt: keep admission closed
- Catabolic activated, matching host validated: finish the host's idempotent activation
- Both activated with identical catalog/incarnation/input/ack/map: a fresh Catabolic-only admission gate may be opened
- Any other state: keep admission closed

The helper never opens a gate, edits another database or rolls back imported data.
The old gate is permanently closed. The host must never resume its previous
catalog/scanner authority after activation. Known lost replies replay the actual
activated mapping without a new freeze; the host must still reconcile its half
before resuming admission. Subsequent catalog writes require forward recovery,
not returning to an old snapshot.

## Explicitly unresolved semantics

V1 stages contribution, accepted-decision, physical-occurrence, timeline/version
and application-reference records as bounded unresolved evidence. These are
preserved as canonical JSON values and explicit evidence IDs. They are never
interpreted as a path to open, successful empty source coverage, a physical
mapping, rename/replacement equivalence, timeline equivalence or reference audit.
Any unresolved evidence prevents activation, including with a valid trusted ack.

The rename, same-path replacement, equal-content duplicate and unavailable-source
fixtures qualify evidence preservation and refusal only. They do not qualify
physical import. Full R6 still needs supported adapters through the existing
source/observation/file-revision, identity/binding, contribution and pin owners,
including exact-reference preservation and rejection of unrepresentable cases.
No additional authoritative identity mapping engine should be introduced.

## Evidence and boundaries

Preparation tests use real disposable SQLite catalogs with explicit native34/35
targets and the production command/curation/search owners. They cover interrupted
staging, concurrent duplicate batches, missing/changed batches, collision and
phantom detection, current authority/redaction, retention/expiry, final commit
failures, item/search/generation rollback, actual process exits during staging and
activation and after commit/before reply, drained-gate identity, trusted-ack binding
and restart reconciliation. Process-exit helpers are intentionally ignored as
standalone tests and executed by their passing wrappers.

The incarnation-change fixture is a no-live-owner simulation. It is not real
backup restore/publication qualification. WAL has refusal coverage only. No Motion
changes/tests, combined dependency proof, actual application-reference migration,
macOS/other-platform qualification, destructive publication or interval activation
is included. Independent schema, definition and filesystem safety reviews remain
incomplete and were not retried or reassigned.

Native `HostSession::open_authorized` now requires the caller's actual common gate,
and its scan supervisor retains the admission lease until exact child exit.
The same-source receipt qualifies those owners and the import contracts. The
separate public cross-owner composition fixture and positive cleanup for actual
scanned-source activation still require their own integrated evidence.
External host freeze/reference persistence and both-owner crash recovery remain
separate deferred evidence. The bounded logical fixtures do not prove them.
