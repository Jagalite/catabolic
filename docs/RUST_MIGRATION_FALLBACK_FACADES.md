# Selected public fallback facade checkpoint

This is a narrow forwarding, ownership and runtime-orchestration checkpoint. It
selects exactly 15 frozen `tests/parity/python-api.json` rows at commit
`453fca983222c6665a48775eef67c489c96b527e`: Resolver class/constructor/resolve/health/
capture (5); projection binding plus class/constructor/bind/plan/run/apply_prepared/
present (8); worker configure/tick (2). Neither callable coverage nor this document
claims exhaustive parity or closes another migration/release gate.

## Resume checkpoint: narrow lifetime and callback corrections

The 2026-10-11 stop preserved two retained-plan leaks and a cancellation callback
that could mask evaluation identity/epoch errors. On 2026-10-12, fresh diagnostics
against the restored debug extension reproduced all three: callback-time plan
dropping and post-prepare cancellation/exception each left capacity for only 255
plans; an invalid evaluation token invoked the callback and returned its exact
exception instead of the identity error.

The resumed correction adds a Store-local release-only native plan registry,
which can release a handle while the Session is checked out, detached or closed.
Its mutex never spans Python callbacks, Store access or publication effects;
poisoned registry access is refused. A native reply guard releases an undelivered
prepared token when cancellation or a latched Python exception prevents native
JSON delivery. Both inline and threaded execution perform the completion poll.
The existing identity/profile/epoch checks are reused before starting the live
callback observer. The thin Python request adapter no longer eagerly coerces the
cancellation object before those checks.

Fresh after-fix diagnostics restored all 256 retained-plan slots and reported
zero callback invocations for invalid evaluation identity. Exact callback
exception identity remains preserved. That first correction expanded the selected
Python gate to **22 required cases**, retaining all prior 18 and adding four lifetime/callback cases;
its 27 exact paired groups, 15 preserved rows and two separately counted
publication refusals are unchanged.

Final copied-debug replays passed all 22 cases separately on Python 3.14.6 and
3.11.15 (`final-python314.log` and `final-python311.log`).

Owner evidence is under `.local-tests/rust-migration/fallback-lifetimes/`:
`before-probe.jsonl`, `before-regression.log`, `after-probe.jsonl`,
`after-regression.log`, `rust-fallback.log`, `rust-release-poison.log`,
`rust-owner-regressions.log`, `source-contracts-final.log`, `clippy.log`, and
`owner-evidence.json`. Completed owner checks: 22 copied-debug Python cases,
14 runtime fallback tests, one native registry-poison test, seven Components,
six query-ownership and four embedding-unwind tests, seven source-contract tests,
and Store/runtime/PyO3 all-target Clippy with warnings denied. These are focused
owner diagnostics, **not source-bound installed-wheel or release qualification**.
That first correction's installed artifact cohort was captured before the
independent review correction below; it is not final qualification evidence.

### Independent prepared-plan delivery correction

Fresh independent fault injection reproduced a remaining leak after native JSON
serialization: JSON-delivery failure, wrapper-constructor failure and an exception
between decode and wrapper creation each left only 255 available slots. The
public `plan()` adapter now receives a native `PreparedReply` lease before it
decodes JSON or allocates the mapping. The successful mapping retains that lease;
native ownership never depends on a Python `__del__` method. A single
`BaseException` boundary explicitly releases failed handoffs while preserving the
original exception object, even when the caller retains its traceback and the
partially initialized mapping. Lease release is idempotent and works while the
Store is checked out, detached or closed.

This cleanup boundary covers exceptions inside the adapter's delivery body.
Arbitrary return-event or `RETURN_VALUE` instrumentation can retain the completed
mapping and lease through traceback locals outside that boundary. The reachable
owner releases normally when dropped or explicitly released; this is retained
ownership, not a lost-token leak.

The installed suite now requires **24 cases**, preserving the previous 22 and
adding retained-traceback handoff faults and detached/closed lease release. The
fault case injects exact `MemoryError` and `KeyboardInterrupt` objects at seven
decode/allocation/construction/delivery boundaries and requires all 256 slots
while failed frames remain live. The 27 exact pairs, 15 selected rows and two
separately counted publication refusals are unchanged. The generic private
`Store._call` / `NativeStore.call` JSON protocol still transfers raw handles to
internal callers; public `FallbackProjection.plan()` uses the owned lease method
instead. Raw protocol use is not part of the selected public facade contract.

Independent evidence is under `.local-tests/rust-migration/fallback-independent/`.
Separate probes also cover cyclic dropping, reentrant close/detach refusal plus
dropping, and deterministic inline completion cancellation/exception. The older
threaded post-prepare regression yields briefly to the empty native planner; it
is an observed capacity witness, not exhaustive scheduling qualification. Final
source-bound installed artifacts for the lease correction remain pending.
See the [paused handoff](RUST_MIGRATION_PAUSED_HANDOFF.md) for the preserved
historical stop record and unchanged broad-review restrictions.

## Qualification design

- `scripts/native_fallback_embedding_tests.py` owns 24 fixed, non-skipping installed
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
Ordinary idle dropping and dropping during a Store callback release the native
handle through the Store-local release-only registry. Detached/closed release and
poison refusal have focused owner witnesses above. Copying its mapping or
editing the plan does not recreate native authority. This deliberately narrower
contract does not claim compatibility with arbitrary caller-created Python plans.

Worker configure operates on the caller's Store. Tick is genuinely path based and
owns one bounded native session; disabled/empty ticks match the oracle. The suite
also checks watcher-owner skipping, native publication/recovery refusal, no journal
consumption, and retry backoff after a refused tick. Native tick accepts only
integer limits 0 through 1000; legacy negative/unbounded limits are an explicit
compatibility gap. Perpetual worker `run()` is not selected by this Python
facade gate. The separate [preserved CLI lifecycle adapter](RUST_MIGRATION_FALLBACK_WORKER.md)
forwards this tick while retaining all public publication/recovery refusals.

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
- Those counts describe historical preparation. The resumed owner evidence is
  recorded above. The installed artifact run remains pending the parent
  source-stable build; no installed success is claimed by these owner checks
