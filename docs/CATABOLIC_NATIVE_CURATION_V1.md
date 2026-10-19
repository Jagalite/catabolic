# Native curation contract v1 (schema 34)

Development integration for design package R3. This document does not close the
blocked independent schema review, the M0–M11 ledger, or product qualification.
The R2 checkpoint and its source-bound evidence remain separate.

## Additive admission and migration

Ordinary initialization/upgrade still target schema 32. Explicit native admission
supports frozen schemas 33 and 34 with distinct exact signatures; schema35 adds
derived search storage without changing the curation contract below.
`initialize_native_at(..., 33)` retains an explicit compatibility fixture target;
opening an existing native catalog never upgrades it implicitly. New curation
commands require schema 34; existing identity and item-put commands retain their
schema-33 contract. Known receipts are resolved before new-operation admission.

Migration 034 adds tables, triggers and views without rewriting migrations
001–033 or their schema definitions. Existing records remain byte-identical.
The one deliberately versioned preservation exception is the append-only
invalidation-index baseline for newly snapshotted `legacy_import` identity
triples. Every prior index row remains identical; the added rows must exactly
match the complete preserved identities, with revision 1 and deleted 0. No
other index append or update is allowed by that exception. Catalog generation
is unchanged during this baseline import. The existing backup, rehearsal,
exclusive admission, exact-plan and rollback owner remains responsible.

## Trusted authority and durable receipts

`execute_with_authority` shares the existing top-level transaction and receipt
engine. `CatalogAuthority` holds separate host-established metadata, tag and pin
grants; these grants are not request JSON. Mutation admission, source CAS,
preview binding, projection updates and receipt insertion occur atomically.
Failures roll back all mutation effects. New owners do not perform filesystem or
network work in the transaction.

`TrustedCommandAuthority.receipt_visibility = FullResult` is a separate current
trusted read capability for the entire retained receipt body. It must be refreshed
by the embedding host, including every affected subject. Membership in a host
namespace does not establish that capability. Revoking mutation permission alone
does not revoke an independently granted receipt-read capability. If any original
member is no longer readable, the host must request `CommittedStatusOnly`; known
receipts then disclose no body. Receipt replay intentionally does not re-run old
mutation preconditions, parse the mutation again, or require current write grants.
R4 owned host leases remain a separate prerequisite for application admission.

## Contribution metadata

Metadata v1 is item-scoped and initialized with an explicit immutable field
schema. Initialization snapshots the exact raw legacy metadata, preserving it as
legacy evidence rather than inferring provider or manual provenance. Source
records use exact subject, namespace, record ID and kind; every update supplies
an expected source revision. Empty source heads remain revisioned. History is
immutable and contributions retain evidence and provenance.

Pure resolution distinguishes local value, local explicit null, absent field and
override removal. It attributes all agreeing normalized providers, preserves
conflicts, and uses scan values only as fallback. Legacy effective values remain
until named reconciliation. Policies explicitly choose field types and
normalization. Bounded complete-source resolution and materialized items.metadata
updates share one transaction. Legacy item-put cannot overwrite managed metadata;
an equal value preserves existing raw JSON bytes.

Public reads take a real admitted Store, reject ambient transactions, validate
lifetime and final catalog stamps, and return bounded provenance. Source revisions
and the generic entity invalidation counter are distinct contracts.

## Tags and identity pins

Native effective tag views apply revisioned local suppression by canonical tag ID
without removing source assertions. Unsuppression reveals active assertions.
Vocabulary merge requires a complete authorized preview, generation/incarnation
fence, source/target revisions, a bound digest and explicit collision evidence
choices. Immutable history retains affected identities and mappings. Alias changes
do not erase suppression; retired logical IDs are never silently replaced by a
survivor grant.

Identity pins are explicit decisions with expected revisions, retained removals,
immutable history and unique active namespace/value ownership. Provider refreshes
cannot overwrite them. Raw legacy identities remain independently preserved
evidence; they are not relabeled as accepted manual decisions. Preview and public
reads validate Store lifetime, refuse ambient transactions and recheck generation
and incarnation. Apply uses the existing writer transaction and revalidates the
preview digest. Retired logical subjects reject new direct decisions while
historical evidence stays addressed by its original ID.

## Reviewed logical merge and split

Logical replacement preserves old timeline/version/binding snapshots and explicit
historical ownership. A complete bounded preview supplies native context heads,
affected membership, generation/incarnation, full reference mappings and a digest.
New timelines, versions and association owners are explicit replacements; file
extents must exactly partition the old bindings. Retiring selected versions first
requires validating their untouched persisted context, then rebuilding and
validating the complete new membership in the same command transaction. No public
skip-validation flag or arbitrary mutation callback exists.

Merge retains an unambiguous retired-item alias. Split retains its historical ID
and returns all reviewed children, never an arbitrary redirect. Local/provider
metadata, pins and legacy evidence stay on their original historical IDs; this
contract does not silently copy or transfer them. Direct new decisions on retired
subjects fail. Retired association owners cannot be revived through legacy writes.
Old mapping rows remain pinned/readable and cannot be reactivated or relabeled.

This first bounded replacement contract refuses preexisting active relationships,
publication mappings and expected sets on retiring identities/owners when no
explicit relocation policy is defined. That refusal is a stated limitation, not
successful migration of those references. New writes targeting retired IDs are
also guarded. A later explicit relocation contract is required to handle those
populated cases. Public preview/resolution require admitted Store lifetime,
reject ambient transactions and recheck the final catalog stamp; mutation uses a
private transaction-safe path. Late failures roll back new items, associations,
retirements, aliases, history and the receipt together.

## Qualification status

The R3 privileged public read helpers currently require an autocommit Store.
They fail closed on the owned retained transaction of a read-only Store, as well
as on caller-owned transactions. R4 must add an owner-controlled fresh-read
snapshot boundary and qualify read-only Session use before these APIs serve
application reads. Existing legacy retained-snapshot behavior is unchanged.

The frozen bounded curation source passed 871 tests with zero failures and nine
opt-in entries across 64 core/store/runtime harnesses. Source hashes matched
before and after the run. All seven applicable opt-in cases passed separately;
the two fork helpers ran through their passing subprocess wrappers. Strict
Clippy, formatting, Ruff, 49 Python schema/audit tests and both schema generators
passed. Exact evidence and corrected initial failures are recorded in
`tests/parity/native-catalog-r3-checkpoint.json`. The aggregate used Python3.12.14;
three supplemental differential cases used the pinned Python3.14.6 environment.
The real helper fixture uses legacy schema32, not native34.

These results qualify this bounded slice, not all R3 relocation requirements or
M0–M11 gates. Independent schema review remains incomplete. No Motion edits or
tests are part of this stage; external composition remains deferred. Native
restore and destructive publication remain fail-closed. The Python wrapper is last.
