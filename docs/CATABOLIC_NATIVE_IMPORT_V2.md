# Bounded native import V2

See `CATABOLIC_NATIVE_IMPORT_PARTITIONS.md` for additive empty-manifest v3,
trusted cross-partition endpoint grants, tag/relationship/logical-alias owner
mappings, and aggregate restart admission.

V2 extends `CATABOLIC_NATIVE_IMPORT_V1.md` through the existing Store command
transaction and typed owners. It adds no schema, queue, job owner, identity engine,
resolver, filesystem publisher or external application integration. V1 remains
accepted with unchanged canonical serialized shapes. V2 is selected by the
snapshot manifest's `contract_version: 2`; optional V2 fields are absent from V1
serialization. The reserved operation-ID namespace remains unchanged, so changing
the version or data under a used import ID is a conflict, never a second import.

## Admission and durable state

The host supplies independent `ImportAuthority` and `TypedAuthority`; neither is
serialized into the snapshot. `CatalogAuthority::with_import_typed` provides the
existing per-subject metadata grants, pin grant, exact physical file read grants,
and editorial identity grant. A payload cannot declare its own authority. Public
entry points remain privileged embedding APIs, not untrusted HTTP endpoints.

Prepare and stage retain immutable bounded input in existing command receipts,
with holds inserted in that same transaction. Limits remain 16 batches and 64 KiB
canonical JSON per batch; each typed batch has 1..=64 top-level owner records.
The existing identity mutation validator imposes its own nested structure limits.
The existing receipt count/byte/hold budget applies to every step and result.
There is no eviction, automatic hold release or reuse of expired import IDs.

Validate invokes the actual curation, metadata, pin and editorial identity owners
inside a nested savepoint, then deliberately rolls back that entire rehearsal.
Only the validation receipt and proposed mappings survive. Items, projections,
source/pin histories, associations, native identity rows, search invalidations,
and generation do not become visible. A failed owner or failed rollback is a
failed validation. This is database-only work, with no user callback or external
side effect in the rehearsal.

Activate runs the same owners again under the existing command transaction. It
requires the exact final generation, mapping digest, trusted source/reference
audit acknowledgment and the same closed/drained HostAuthorization gate as V1.
It checks the full actual typed mapping against the validated mapping before
committing. The normal search flush, generation, final receipt and hold commit
atomically. Each composed owner's internal command envelope retains the one
outer receipt key and incarnation; it describes that owner's exact payload and
the generation after preceding composition steps. It does not admit another
command or create another receipt.

## Supported typed mappings

- Logical source IDs map to explicit preserved destination IDs. Destination and
  cross-batch collisions refuse the whole import.
- Metadata initialization and local/provider/scan contributions use
  `catalog_metadata_store`. Each source needs the exact independent namespace,
  record ID and source-kind grant. Source snapshot revision is retained in evidence
  and explicitly mapped to new Catabolic source revision 1; missing prior source
  history is not fabricated. Existing legacy retention and conflict projection
  rules continue to apply.
- Explicit ordered identity-pin decision history uses the existing preview/CAS
  and mutation owner. Every supplied source revision maps to its actual new local
  revision. Initial removal without an evidenced earlier set is refused. A later
  set followed by removal retains both historical decisions.
- Physical occurrence mapping reads existing source/path/status/exact-revision
  evidence through the occurrence owner. It does not scan, register a source,
  deduplicate equal content, infer a rename, or invent a new identity scheme.
  Activation checks the exact evidence again. Two equal-content files remain
  separate. Availability remains `unknown`; an observed `present` status is not
  live-source availability. A stored `missing` status is preserved as evidence.
- Associations use the existing curation owner and migration origin. Source
  association IDs map explicitly to owner-generated Catabolic IDs. Every imported
  association must be consumed once by an exact-revision typed binding, preventing
  an unpinned legacy association from silently rebinding to a replacement.
- Contexts, timelines, versions, exact-revision bindings, segmentation and declared
  equivalence use `catalog_identity_mutation`, its grants and its validators.
  They create new heads only, retain supplied native IDs, and return the owner's
  exact new revision mappings. Edition/association item IDs must be newly imported
  items; source association references are replaced only through the explicit
  validated map. Existing identity updates require their separate normal owner
  operation and are not an import shortcut. Interval fields here are database
  identity facts, not permission to run interval media processing or publication.

Unresolved historical/application references can still be retained in staged
receipts. Their presence prevents activation. Resolving an ambiguity by guessing,
selecting one duplicate, dropping a historical decision, or treating unavailable
coverage as successfully empty is not supported.

## Freeze, restart and runtime integration

Use the common HostAuthorization object for catalog calls, private writes and
scanner admission. Close it, retain all already-admitted leases through actual
worker lifetime and writer delivery, and drain it. Persist the external owner's
stable snapshot/reference audit acknowledgment before constructing
`TrustedImportActivation::after_drain`. Same strings on a different gate do not
satisfy exact gate identity. Activation never reopens the old gate.

The Catabolic receipt and external owner's durable prepared/validated/activated
checkpoint are reconciled by `reconcile_activation`. Both must agree on catalog,
incarnation, import, input, mapping and reference acknowledgment. A committed
Catabolic receipt with only a validated external checkpoint requests completion
of the external activation; it does not admit catalog writes. Only both activated
permits creation of a fresh Catabolic-only gate. A lost reply replays the original
receipt before current mutation grants, without duplicating histories or bindings.

Runtime HostSession and the schema34–36 private writer now accept the same exact
gate. Their owner-level contracts and V2 are qualified together in
`tests/parity/native-catalog-r6-checkpoint.json`; public cross-owner composition
has a separate fixture. This module does not invent a competing host gate or claim
that a synthetic gate proves integrated application cutover.

## Qualification boundary

The preparation tests explicitly cover native34, native35 and native36 database
fixtures for typed positive activation, complete rollback, and real process exits
before commit and after commit before reply. The native identity positive fixture
has deliberately pre-seeded quiescent occurrence rows with real file stat values;
this is stated fixture evidence, not a claim that an arbitrary scan has cleaned up.
A separate real observation-owner fixture exercises equal-content duplicates,
rename, same-path replacement and an unavailable source. It preserves retained
observation worker PIDs and never clears them to make activation succeed.

Activation reuses the migration owner's native pending-work inventory, including
schema36 owned execution lifetimes, and requires rollback-journal mode. Retained
observation/watcher ownership still refuses. A later exact companion cleanup
witness must be qualified through its actual child/process protocol before that
refusal can be relaxed; no PID liveness guess, expiry or manual nulling substitutes.

Actual Motion export/reference updates, both-owner deployment, real WAL cutover,
controlled restore, destructive publication, and interval-processing effects are
not qualified by these fixtures. They remain deferred or fail closed. There was
no independent schema/definition/filesystem safety review in this preparation.

Additional bounded import-path qualification for multipart alternatives, distinct
cuts, declared/unknown equivalence, local/scan contributions, explicit null versus
absent fields, and retained legacy precedence is described in
[`CATABOLIC_NATIVE_IMPORT_REPRESENTATIONS_V2.md`](CATABOLIC_NATIVE_IMPORT_REPRESENTATIONS_V2.md).
That test-only addendum uses existing owners and preserves the representation and
external-integration limits above.
