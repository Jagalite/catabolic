# Native host v1: initial R0 implementation

This checkpoint extends the existing Rust owners. It does not close R0, R1–R7,
or any M0–M11 release gate. No schema migration, native default transition or
Python-wrapper change is included.

## Supported initial surface

`catabolic_runtime::host::HostSession` is a **privileged operator handle**. Its
possession is host authority; it is not an application-user authorization API.
Keep it behind trusted host code. R4 must supply scoped admission before exposing
catalog requests to Motion users. Profile IDs still identify machine profiles.

The synchronous facade owns one existing `embedding::Session`, uses saved query
IDs rather than raw SQL, and supports:

- Explicit contract-version admission; open never creates or upgrades a catalog
- Catalog identity and schema inspection without disclosing its filesystem path
- Complete saved-selection reads with deadline, ID and result-byte budgets
- Cooperative cancellation, checked before admission and before read delivery
- Bounded legacy-v1 item batches on the same writer and transaction, with rollback
  of the whole batch on validation/conflict/cancellation and recovery refusal
- Explicit helper path plus SHA-256 registration and one supervised native scan
- Pending status, demand-only cancellation, drain, close and reopen

This item-batch API is an adapter to existing curation, not the R2 durable command
protocol or R3 contribution/identity resolver. Do not retry a failed or uncertain
mutation under a new operation identity. Durable replay, retention, restore
incarnation and new catalog semantics remain later work.

Every call blocks. Run it on a host-owned blocking executor, never the application's
async event-loop thread. Rust's mutable borrow serializes session calls. There is
no public transaction handle, Store escape hatch, or transaction across an await.

## Read freshness and errors

Read-only sessions retain their SQLite snapshot until close/reopen. In WAL mode,
a concurrent writer can commit while that snapshot remains stable. In rollback
journal mode, the retained reader can delay a writer. This initial contract offers
no live cache invalidation promise; generation/scope/incarnation cursors are R2/R4.

Machine-readable classifications originate at the refusal site. Store admission
classifies unsupported schema, invalid catalog, writer contention and internal
failure using explicit state and OS/SQLite codes. Evaluations classify missing
saved query, timeout, cancellation, unsupported mode and ID/byte/work exhaustion.
Item writes distinguish invalid input and identity/kind conflicts. Legacy APIs
retain their original error diagnostics.

The host returns stable codes, static safe details and retry guidance. Unclassified
owner failures remain `internal_failure` with reconciliation guidance, never a
classification guessed from diagnostic text. The underlying error source is for
trusted operator logs and must not be serialized into an application response.
A completed mutation stays successful if cancellation arrives after commit.

## Worker ownership

`start_scan` transfers a caller-supplied resource permit into the existing
observation worker's supervisor. Pending responses retain it. Only a verified
OS wait proving worker exit releases it, including failed workers. No unowned
reaper is created on the new path; existing legacy entrypoints remain unchanged.

The session is detached while a worker runs, so a retained writer cannot prevent
its progress. Demand cancellation briefly reconnects and cancels that request
only; it does not terminate shared work required by another consumer. A supervised
helper receives the expected catalog identity and checks it immediately after
open, before observation admission.

After any post-spawn reconnect failure, the host retains the worker and permits
bounded drain. `close` refuses while a worker is live. `drain` stops new admission,
waits up to the caller's bound, and closes once exit is established even when the
work failed. Reconnect can add the existing five-second writer-lock bound.

`Drop` closes the Store before a blocking join fallback. It is not bounded shutdown
or evidence of cancellation. Hosts must explicitly drain, retain pending handles,
and reconcile durable work after a process crash. On an unprovable OS wait failure,
the low-level supervisor deliberately retains/leaks the permit rather than falsely
admitting replacement work. Crash, orphan, descendant and cross-platform behavior
remain R5/R7 qualification obligations.

## Compatibility disposition for this slice

| Surface | Disposition | Version/migration rule | Evidence and remaining obligation |
| --- | --- | --- | --- |
| Existing Store, Session, SQL, GraphQL and item mutation calls | `preserve` | Existing signatures, diagnostics and schema stay supported; typed entrypoints are additive | Original regressions plus typed-admission/evaluation tests; all M0–M11 obligations remain |
| Native host facade | `versioned_change` | New host contract v1, no automatic catalog migration and no claim of Python parity | Host unit/integration and combined Motion fixture; scoped admission and durable command semantics remain R2/R4 |
| Supervised scan entrypoint | `adapt` | New handle retains the existing observation owner and durable request identity; legacy entrypoint unchanged | Pending/exit/identity/reconnect fixtures and real-helper scan; crash/platform completion remains R5/R7 |
| Schema, timeline, contribution, receipt and cutover contracts | Not implemented by this slice | No schema bytes, historical checksums or public Python/HTTP contract changed | R1–R7 requirements are still required; no release gate is waived |

The frozen compatibility ledger is not rewritten to label these new contracts as
legacy differential parity. The complete revised ledger remains an R7 deliverable.

## Qualification entrypoints

Use the repository's pinned Rust 1.99.0. The user authorized standard Cargo rather
than Kyoto for the 2026-10-14 cloud run. Catabolic's declared minimum is 1.99, so
this is its minimum-toolchain run; it does not validate Motion on its old 1.98
minimum after introducing Catabolic.

Focused tests:

- `catabolic-store --test open_admission`: typed admission and unchanged legacy errors
- `catabolic-runtime --lib host::tests`: post-insert cancellation rollback and recovery refusal
- `catabolic-runtime --test native_host_contract`: admission, reads, mutations,
  retained WAL snapshot, in-flight cancellation, explicit errors and close/reopen
- `catabolic-runtime --test observation_supervision`: pending permits, failed
  workers, reconnect ownership, identity fencing and replacement refusal
- Explicit ignored native-host test `real_native_helper_scan_cancel_drain_and_reopen`,
  with `CATABOLIC_HOST_TEST_HELPER` naming a separately built native binary whose
  digest is registered by the fixture

The real-helper fixture uses 400 disposable files, cancels and drains a request,
reopens and completes a scan, then checks failed-work drain. Test doubles do not
stand in for this fixture. The normal test command intentionally marks it ignored;
run it explicitly and record its outcome separately.

## Motion dependency boundary

The pinned Motion graph has SQLx 0.8.6/libsqlite3-sys 0.30.1. Catabolic has rusqlite
0.40.2/libsqlite3-sys 0.38.2. The unmodified graph fails `links=sqlite3` resolution.
An isolated probe changing only SQLx SQLite's dependency requirement to `=0.38.2`
compiled and linked the real Motion library with Catabolic. It applied 62 Motion
migrations, initialized Catabolic schema 32, compared SQLite source identities,
checked build flags and reopened both databases. This is Linux viability evidence,
not upstream support or authorization to alter Motion's production dependencies.

A supported Motion alignment decision and full combined host lifecycle evidence
remain external integration obligations. The user's 2026-10-14 sequencing
amendment defers Motion edits and testing and permits Catabolic R1–R7 to proceed
from locally qualified contracts. The isolated probe does not satisfy the deferred
production composition gate. See the native catalog design's sequencing amendment.

## 2026-10-14 Linux checkpoint

The frozen-source store/runtime command completed with **553 passing executions,
zero failures and seven ignored entries**. The real-helper host entry was then
executed explicitly and passed; the other six legacy ignored obligations remain
open. Focused contract tests comprise 37 passing executions and overlap that
aggregate; they are not an additional independent total. Formatting, strict
store/runtime all-target Clippy and the expanded combined Motion fixture passed.

The exact [checkpoint receipt](../tests/parity/native-catalog-r0-checkpoint.json)
and [source manifest](../tests/parity/native-catalog-r0-source-manifest.json) retain
source/helper identities, run scopes, ignored cases, oracle provenance and hashes
of failed development/setup runs. The source manifest is
`9c670fd16d98fa1747dbaa461c54d407faa4747428622d3e039eef2932671f6c`;
all 700 files matched before and after qualification. The stripped debug helper
SHA-256 is `6830631b4274c9fb651c5c7d52072ac3cbac17e3200e0f8e3504143f36f0a2fa`.
This is a test helper, not a standalone distribution candidate.

Historical fixture setup used the exact frozen oracle and all 518 inventoried
resource hashes, with Linux Python 3.12.14 rather than the release Python matrix.
All 32 regenerated fixtures passed their frozen upgrade/preservation checks and
remained unchanged after testing. Earlier missing-fixture, missing-oracle and
lint failures are retained, not relabeled successful.

The [combined dependency fixture](../tests/native-host/motion-dependency-probe/README.md)
and [production options](../tests/native-host/motion-dependency-probe/DEPENDENCY_OPTIONS.md)
are reproducible. Its SQLx patch is qualification-only; Motion's production tree
was not edited. At this historical checkpoint R0 remained open pending supported
dependency adoption and broader qualification. The subsequent sequencing amendment
permits Catabolic work to continue with external integration explicitly deferred.
No M0–M11 gate closure is implied by these results.
