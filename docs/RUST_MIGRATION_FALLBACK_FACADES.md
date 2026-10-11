# Selected public fallback facade checkpoint

This is a narrow forwarding, ownership and runtime-orchestration checkpoint. It
selects exactly 15 frozen `tests/parity/python-api.json` rows at commit
`453fca983222c6665a48775eef67c489c96b527e`: Resolver class/constructor/resolve/health/
capture (5); projection binding plus class/constructor/bind/plan/run/apply_prepared/
present (8); worker configure/tick (2). Neither callable coverage nor this document
claims exhaustive parity or closes another migration/release gate.

## Stop-point warning

Implementation stopped on 2026-10-11 before the first artifact-bound installed
fallback gate. Independent diagnostic probes found **two retained-plan leaks**:
dropping a plan during an in-flight Store callback can lose its release, and
cancellation after native preparation can discard the only returned plan token.
Each probe left capacity for only 255 new plans instead of 256. A third probe
showed a cancellation callback can run and raise before an invalid evaluation
token is rejected. These are unresolved adapter defects. The probes used a
disposable copied package and existing debug extension, not an installed,
source-bound qualification artifact. See the [paused handoff](RUST_MIGRATION_PAUSED_HANDOFF.md)
for evidence and proposed resume steps.

## Qualification design

- `scripts/native_fallback_embedding_tests.py` owns 18 fixed, non-skipping installed
  cases and 27 fixed exact paired value groups. The gate traces every selected row
  in both the installed suite and native paired scenario and verifies all frozen
  signatures. Class rows are witnessed by constructor calls
- One disposable initialized and seeded database is copied for both engines. This
  preserves database UUIDs, query/policy IDs, epoch and canonical plan identities
- Installed pairs use real native empty candidate queries, unresolved plans and
  direct captures. No monkeypatched Python engine, probe callback, filesystem
  publication or external service is needed. Native Rust owner tests separately
  cover probe state; they are not substituted for installed paired evidence
- Fresh/prior health clock values are range-checked and normalized; other paired
  values include full plans, digests, captures, binding/configure returns,
  transaction rollback and empty/disabled worker results
- Two publication refusal witnesses are recorded separately in the report's
  `non_parity_refusal_evidence`, never counted as equal paired groups. The oracle
  rejects a missing expected digest or deliberately stale epoch. Native public
  run/apply refusal is a stronger, intentionally different safety boundary
- Development mode verifies a helperless installed wheel, exact payload hashes,
  source-before/source-after build identity and extension hash. Strict `--wheel`
  mode uses the shared release artifact verifier including packaged helper and
  matching external CLI binaries. Both modes preserve frozen source/import
  provenance, pinned Python 3.14.6 interpreter identity, installed module identity,
  fixed counts and source/file fences before reporting success

## Tested boundary and explicit gaps

The installed suite is designed to cover lazy persistent evaluation, mutable primary choices
and cumulative check counters, session byte/deadline/cancellation/drop and epoch
fences, foreign native session rejection, same-owner transaction reads and nested
rollback, read snapshots, detached/closed refusal, full native prepared plans and
presentation, immutable native plan comparison, wrong owner/profile/spoofed or
expired plan token rejection, and bounded retained-plan ownership.

Native prepared plan handles are process-local and capped at 256 retained plans.
Ordinary idle dropping releases the handle; dropping during a Store callback has
the unresolved leak described above. Copying its mapping or
editing the plan does not recreate native authority. This deliberately narrower
contract does not claim compatibility with arbitrary caller-created Python plans.

Worker configure operates on the caller's Store. Tick is genuinely path based and
owns one bounded native session; disabled/empty ticks match the oracle. The suite
also checks watcher-owner skipping, native publication/recovery refusal, no journal
consumption, and retry backoff after a refused tick. Native tick accepts only
integer limits 0 through 1000; legacy negative/unbounded limits are an explicit
compatibility gap. Perpetual worker `run()` is not selected.

Foreign Python Access contexts and `capture(check=Python_callback)` are explicitly
refused. Resolved package/rendition/layout combinations, all probe/failback timing
boundaries, competing writers, arbitrary non-JSON values and exact invalid-type
exception categories remain unqualified by this installed facade slice.

Public run/apply/recovery retains existing safety gates and exposes no fixture
override. The gate tests the distinct existing run/recovery and prepared-apply
refusal diagnostics. This work does not authorize filesystem publication or retry,
rephrase, reassign or close blocked broad definitions/curation/programs/plans/
interchange/manifest/layout or filesystem/source-access/observation reviews.

## Evidence at preparation

- Focused runtime adapter suite: 12 tests passed; preserved Components (7), query
  ownership (6) and embedding unwind (4) regressions passed
- Store/runtime/PyO3 Clippy with warnings denied, targeted formatting and Ruff passed
- Source-only facade contract suite: 7 tests passed
- Frozen Python 3.14.6 scenario preparation: all 27 groups completed and both oracle
  refusal diagnostics matched their expected values
- Installed artifact run remains pending the parent source-stable build. No
  installed success is claimed by these preparation checks
