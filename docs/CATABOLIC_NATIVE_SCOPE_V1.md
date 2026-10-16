# Native scoped host reads v1

Status: the item-only owner and shared budget are integrated for qualification.
This is an additive trusted-host Rust API, not a new HTTP wire contract or R4
completion. Existing operator Session and HTTP authorization remain unchanged.

## Authority and ownership

`HostAuthorization` represents a permission snapshot established by the trusted
embedding host. It is not a catalog grant. Its owned, non-cloneable lease remains
held through read delivery or mutation commit. Revocation closes new admission
first and acknowledges completion only after all admitted leases have drained.
Timeout leaves admission closed. Equal namespace/revision strings on a fresh gate
cannot revive an old capability.

`ScopedHostSession` privately owns `ScopedReadSession`; neither exposes Store,
Connection, legacy Session or a database callback. Trusted grants name immutable
saved selections and explicit field masks. Reports require independent approved
saved references. Request IDs narrow admitted membership and never grant access.
Host constructors must not be bound directly to untrusted request JSON.

The opaque scope binds the private session, catalog ID/incarnation/generation,
host gate identity and revision, complete selection/dependency closure and masks.
Each read uses a fresh owner-controlled snapshot, checks the exact scope,
recomputes bounded complete membership and materializes masked values before
public filtering, aggregation or pagination. Final catalog state is checked again
outside the snapshot. Cleanup failure poisons the private owner. Existing legacy
read-only Store/Session retained-snapshot behavior is unchanged.

## Initial supported surface

- Item lookup and bounded item pages
- Explicitly approved saved SQL reports against masked temporary item relations
- SQL or GraphQL saved selections through the existing complete Evaluation owner
- Identical hidden/missing lookup outcomes
- Owned result delivery that holds its lease through callback completion/unwind

Public SQL denies all main-table access, including view dependencies, CTE name
spoofing and aggregate-only reads. Materialization precedes public query work;
the existing broad operator SQL authorizer is not reused as a scope fence.
Public GraphQL and relation-specific reads are not exposed by this first slice.

## Shared budget

One operation owns an absolute deadline and cumulative item/value/byte/work
counters. Membership, definitions, materialization, mask expansion, SQL/GraphQL
VM execution and output borrow that same budget. Nested entry cannot substitute
a fresh budget. Cancellation and sticky typed refusal propagate without parsing
diagnostic strings. Existing SQL/GraphQL owners keep their own VM hooks; the
shared budget installs no competing hook. The temporary thread-local bridge is
thread-affine, while the persistent Store/Session/host owners remain Send.

A private writer must separately establish exclusive VM-hook ownership and hold
the same budget through source writes, derived index flush and COMMIT. Public
Store APIs cannot infer that an unknown progress handler may be replaced.

## Remaining qualification and adapters

Signed cursor pagination, scoped effective tags/pins/provenance, public GraphQL,
search and independently authorized writes are subsequent increments. Unsupported
surfaces stay closed. Original preparation evidence used an earlier schema34
snapshot; only integrated results against the frozen final schema qualify the
live source. No M0–M11, independent schema-review or platform gate is closed by
this document. External Motion composition remains deferred by user instruction.
