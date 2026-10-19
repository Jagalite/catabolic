# Shared rendition and fallback checkpoint

This is a bounded, disposable Linux checkpoint against frozen Python commit
`453fca983222c6665a48775eef67c489c96b527e`, using Python 3.14.6. It does not
complete M7/M8, qualify a release, or change the Python default.

## Production ownership

- `catabolic-core::rendition` is the pure demand decision function consumed by
  `catabolic-store::rendition_mutation`. Existing schema-32 triggers retain
  physical-job fan-out. Caller cancellation changes only that caller's demand;
  even cancelling every caller does not grant cancellation of shared work.
  The old M1 cancellation model is not this production contract.
- Admission owns body/principal idempotency, current source and operation
  authorization, shared physical reuse, reservations, requests and callbacks in
  one transaction. Pending caps include cancelled demands whose physical jobs
  remain pending. Retry keeps the concrete source/job identity and refreshes
  caller token/callback authority. A stale queued source takes precedence over a
  disabled operation and releases the reservation through the existing triggers.
- `runtime::renditions::{tick_store,tick_with_cancel}` orchestrates the existing
  artifact/receipt owner. Store alone selects and mutates durable work. No native
  operation falls back to Python business logic.
- `core::resolution`, `store::fallback_resolution`, `fallback_projection` and
  `fallback_publication` own ordered candidates, rank/ties, bounded evidence,
  lineage, live failover, stable/manual failback, logical generation admission
  and metadata revalidation. Current-source and explicitly accepted historical
  derivative lineage are distinct; originals never acquire historical mode.
- `runtime::fallback` supervises the isolated native probe helper and transfers
  one owned evaluation through detached I/O. A captured catalog identity and
  fallback epoch fence each reopen. Durable PID slots remain occupied until the
  helper is dead and reaped. Paths travel in bounded stdin, not command arguments.
- Prepared projection application consumes the exact evaluated plan, under one
  publication guard. Watcher/refresh callers can persist their comparison and
  pending reaction before applying that same plan. The common filesystem owner
  performs fresh metadata capture and detached live checks at preview, intent
  validation and final verification; a preview is never publication evidence.
  Standalone completion drains consumers once after leaving the publication
  guard; batch callers retain their separate completion boundary.
- `Store::transaction` and `Access::transaction` share the existing transaction
  implementation, including nested savepoints, detached/read-only refusal and
  rollback after failed commit. There is no second request writer or raw-BEGIN
  replacement transaction implementation.

Component selection/packaging and HTTP session ownership are documented in
[RUST_MIGRATION_COMPONENT_SELECTION.md](RUST_MIGRATION_COMPONENT_SELECTION.md)
and [RUST_MIGRATION_HTTP.md](RUST_MIGRATION_HTTP.md).

## HTTP selection compatibility

Ordinary approved fallback policies evaluate globally complete IDs through a
sealed immutable policy/action proof, then apply caller content authority.
Package SQL requires the caller's separate `sql:read` operator permission.
Package GraphQL evaluates scoped views in actual leaf traversal order. Complete
child caches, consumed budgets and scope state survive short query sessions;
only an actual detached probe retires the temporary SQLite scope.

This preserves two frozen quirks rather than broadening access:

- Scoped GraphQL masks source identity for non-metadata operators. A required
  primary occurrence is stale and blocks; stale requirement candidates are
  skipped, which can leave a package unresolved.
- Reinstalling a GraphQL authorization context before detaching raises a frozen
  SQLite table-exists error. Native HTTP preserves the sanitized
  `catalog_error`/500 result, without exposing table names. SQL after scoped
  GraphQL retains the frozen restricted-query denial. This is known-defect
  parity, not a newly recommended query pattern.

Final token reauthentication remains separate from the epoch fence. Revocation
that changes the epoch fails with generic `catalog_error`/409; natural expiry
while detached, without a database change, fails with `unauthorized`/401.

## Executable evidence

The final immutable-artifact Python gate uses frozen source **and frozen fixture
modules**, selecting only the named native test classes. It excludes imported
oracle-only test classes and records every unique scenario and executable hash.
The run passed **75/75 unique native scenarios**, with no skips, in 153.29
seconds. The retained receipt
`.local-tests/rust-migration/shared-final/report.json` records `complete: true`
and unchanged before/after executable hashes:

- Eight shared rendition scenarios
- Seven base/component-failure resolver scenarios
- Fifteen projection and historical-lineage scenarios
- Twenty native component selection/render/reuse scenarios
- Four isolated helper scenarios
- Twenty-one actual-loopback HTTP package/privacy scenarios

The copied debug executables were built on 2026-10-11 at 20:15:44 UTC:

- CLI SHA-256: `0408a4c849da2a20ee874dd5193b93aa2e74a0c8559726e019fff64019bcedbd`
- HTTP SHA-256: `3f11f5e0988cf1e6e8b65222bff860180a5467c70ed3e860ddead40acf130695`

The immutable copies, explicit native-class runner and detailed results are in
`.local-tests/rust-migration/shared-final/`. This receipt qualifies those
executable bytes against the frozen oracle; it is not a claim that the shared
working tree remained immutable while other implementation lanes continued.

Separate production-owner Rust checks passed in the integrated source checkout:

- `rendition_ledger`: 37,448 actual SQLite edges through depth five, comparing an
  independent input/event ledger to jobs, caller states and reservations. An
  injected shared-cancellation fault shrinks to two events and fails again in a
  fresh process. Three test entry points pass.
- `projection_ledger`: 1,554 traces through depth four (5,910 event applications)
  through real plan/admission/completion SQL. An independent availability and
  publication ledger detects premature generation acknowledgment, shrinks it to
  three events and reproduces it in a fresh process. Three entry points pass.
- `fallback_probe`: three real ownership tests cover occupied live PIDs,
  deadline/kill/reap of a sleeping helper, and a catalog mutation that acquires
  the writer during detached I/O and invalidates the returned receipt.
- `review_shared_resolution`: four independent regressions cover stale-attempt
  receipts, manual/stable failback, malformed/oversized/failed helper responses
  and resource release.
- The four shared Store/Access transaction tests and six production demand/
  resolution reducer tests passed again after integration.
- Strict all-target runtime Clippy passed with `--locked -- -D warnings`. Focused
  Python formatting/lint checks passed. These source-level checks do not claim
  that their test executables share the final CLI/HTTP executable identity.

The Python scenarios include real media rendering and accepted-lineage reuse,
whole-result frozen comparisons, exact sidecar plan signatures, both directions
of unfinished journal handover, every existing journal failpoint, stale and
incomplete evidence, source-byte preservation, named/bulk reconciliation and
actual-loopback authority changes during a blocked native probe.

## Remaining gates

Fallback publication remains explicitly restricted to debug builds and requests
with `disposable_fixture: true`. Release and nonfixture admission stay closed
until the independent M6 leaf-race/recovery qualification succeeds. These checks
neither waive that gate nor qualify all filesystem races.

The Linux fixture results do not establish the full Linux/macOS installed-wheel,
helper supervision, all scheduler/resource combinations, optional integrations,
public Python interface, performance or release/handover matrix. The separately
tracked analysis scheduling and integration gates remain authoritative in
[RUST_MIGRATION_PLAN.md](RUST_MIGRATION_PLAN.md) and
[RUST_MIGRATION_MEDIA.md](RUST_MIGRATION_MEDIA.md).
