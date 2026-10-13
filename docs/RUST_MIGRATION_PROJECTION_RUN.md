# Preserved projection rollback-only preview

Local adapter scope from base `02fcfc6541554d2391d7c693b22e92d63fd4401f`.
This is ordinary new orchestration over the existing production owners, not an
independent review of layout, selection, source access, or filesystem safety.
Python remains the default. No M6, platform, milestone, or release gate changes.

## Supported composition

`catabolic_runtime::projection::run` opens the ordinary catalog writer for
`projection preview`. `catabolic_store::projection_run::preview` preserves limit,
fallback selection, writer/outside-transaction, projection lookup and saved-layout
admission, then forwards the existing sealed `layout_mutation::prepare` owner.
A writer-owned rollback-only transaction forwards `layout_mutation::apply`, the
public `public_reconcile::call(preview)` read owner and frozen report composition.
No caller-supplied display JSON becomes an applied mapping plan.

All desired mappings, layout ownership, selection statistics, epoch changes and
other transaction effects roll back before a report returns. An internal typed
Report/Failure result uses Store's existing transaction cleanup; an error-message
sentinel is not used. Cleanup failure cannot turn into a successful report.
Interrupt guards are destroyed inside the transaction closure before rollback,
including on callback unwind. Preview never commits staged desired mappings.

The full reconciliation action list determines safety and blocked-source count.
Only the returned action and blocked-source details are truncated. Ordinary
layout detail truncation stays with the layout owner. A zero-denominator removal
percentage retains frozen Python's integer-zero scalar kind. The terminal
adapter restores known report-field insertion order and forwards writer contention
as CatalogBusy. Both leaves use the existing joined CLI signal supervisor.

Preparation uses the existing separate bounded read snapshot, followed by its
existing identity/epoch seal at staging admission. This is an explicit native
adaptation, not identical frozen transaction placement: noncooperating writes
between preparation and application may produce a stale-plan refusal. The writer
lock remains held throughout. No new persistence state or refresh queue is used.

## Refusals and remaining contracts

- `projection execute` always refuses before opening the database, resolving a
  catalog, evaluating a layout, staging metadata or planning outputs. This is an
  intentionally early nonparity boundary. `public_reconcile::call(apply/recover)`
  remains unconditionally gated; the frozen commit/apply/verify/report contract
  cannot be advertised while that dependency is unavailable
- Fallback-bound preview refuses before definition/layout planning or mutable
  fallback probing. The ordinary wrapper does not substitute a fixed read owner
  or a refresh queue for `FallbackProjection.run`'s actual mutable-owner contract
- Private `disposable_fixture` options never reach reconciliation and cannot
  relax either refusal. Existing fixed-owner, destructive-action and release
  gates remain unchanged
- Frozen Python may convert a SIGINT arriving inside SQL into `CatabolicError`
  (`SQL query failed: interrupted`, exit 2); the native joined supervisor emits
  KeyboardInterrupt/exit 130. Both outcomes must roll back and release the writer.
  The fixture records the exact observed outcomes as nonparity evidence
- Preview is partial. Underlying owner edge cases, arbitrary nested data ordering,
  every cancellation/race schedule, historical schemas and platform coverage
  are not qualified by these fixtures

No broad previously blocked review was retried, rephrased or delegated. No real
catalog, source media, managed output, credential, external service, publication,
recovery, push, merge, release or default-engine switch is part of this work.

## Narrow evidence and independent rerun

`tests/test_rust_legacy_projection_run.py` has nine mandatory cases, with no
skips or expected failures. It invokes independent frozen Python 3.14.6 and native
processes against separate disposable databases. Fixtures contain file/item and
confirmed-missing observation metadata, plus empty temporary source/output roots.
There is no fixture filesystem publication. SQL dumps of all tables and output
contents are checked before and after operations.

Cases cover full and truncated populated previews, repeated safe-empty previews,
empty-selection guard/allow-empty disable rollback, unsafe-layout short circuit,
admission and post-staging budget failure, writer contention/release, and explicit
fallback and execute refusal/nonparity. The frozen signal branch uses a test-only
SQLite trace/progress wrapper: readiness is emitted only from a progress callback
for the uniquely tagged selection query, after constructor/migration validation.
It forwards the original deadline callback and its return value. An in-memory
control verifies unrelated SQL cannot signal and callback execution/clearing is
preserved. The native branch remains synchronized only to writer admission plus
0.1 seconds; it does not establish a particular native SQL instruction. The
recorded signal outcomes identify both readiness boundaries and the instrumented
oracle-program digest. Narrow SQL-interruption exit nonparity remains explicit. Adapter-only Rust tests add
sealed-plan stale/tamper refusal, stage-write errors, deterministic cancellation
and callback unwind after actual layout ownership writes, full table rollback,
interrupt-handler cleanup and same-owner reuse. Runtime tests prove execute
refuses even absent database paths and private fixture options.

Run with the provided local offline environment, overriding its target:

    source /path/to/main/.local-tests/rust-migration/env.sh
    export CARGO_TARGET_DIR="$PWD/target" CARGO_BUILD_JOBS=2
    export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
    export PYTHONPATH="$PWD"
    python scripts/qualify_projection_run.py --report .local-tests/projection-run/proof.json

The runner rebuilds this worktree's legacy binary locked/offline, runs the focused
Rust tests and fixed Python cases, verifies the frozen reference commit and clean
tracked content, and records source, binary and harness digests. It rechecks those
identities afterward and writes no passing receipt for failed, skipped, optimized
or changed-source execution. The harness fence includes the shared executable
oracle/selection helper and test-package initializer. Loaded test/helper module
origins, binary/reference/interpreter paths and the executable oracle-string
digest are checked before and after the suite. Interpreter identity preserves
the venv path rather than resolving it to a possibly shared base binary. Five
mandatory controls check mutation and misdirection refusal, including a symlink
alias for the same interpreter executable. This is local source/artifact evidence, not hermetic
build provenance or installed/release qualification.

Executed results and artifact identities are reported separately with the final
local commit. Fresh independent narrow review remains required.

## Earlier artifact with incomplete helper fence, 2026-10-12

The `e2670f2` runner passed 9/9 mandatory cases, six store adapter tests and
one runtime refusal test. It checked the source/artifact/harness identities it
recorded again after execution. Independent review subsequently found that this
earlier receipt omitted the imported `tests/test_rust_legacy_review.py` helper
and did not check the loaded subprocess selections against the recorded paths.
This is an incomplete harness-fence snapshot, not the final corrected receipt.
The original receipt and logs are preserved under
`.local-tests/projection-run/e267-incomplete-helper-fence/`. Ten existing legacy CLI unit tests and all-target clippy
for store, runtime and CLI with warnings denied passed. Formatting, Ruff, ledger
regeneration/check and whitespace checks passed. These are local debug artifacts,
not installed-wheel or release receipts.

Relative to the isolated `catabolic-projection-run-orchestration` worktree:

- Receipt: `.local-tests/projection-run/proof.json`
- Receipt SHA-256: `d65f9ff4228973e7dd007574b526703e351c513c55fb619e0e6947236cb4ff2c`
- Source-content SHA-256: `908265d97c55aa9e738200ae66de9e96e69fa1dd931fb3a6e1310aa40575b72e`
- Binary: `target/debug/catabolic-legacy-native`
- Binary SHA-256: `295101bb1cfd795b7921f95945bc4feeedc27c6c530f23dc7f3ca9416ed671e6`

The final signal receipt records exit 130 in all three modes for both processes.
The preceding failed qualification captured the frozen SQL CatabolicError/exit-2
branch; that evidence remains under `.local-tests/projection-run/first-failed/`.
The same run caught fixture observations placed before a source bind invalidated
them. The fixture now seeds them after the bind. The next failed run, retained
under `second-failed/`, caught incorrect disable-action field order; the adapter
now retains the observed frozen order. Neither failure is treated as a passing
receipt or removed from the history.

The existing 13-case HTTP-v3 regression run was **11/13**, not an aggregate pass.
Its new-adapter diagnostic expectation used `unknown catalog: missing` instead of
the observed `unknown catalog`; that test-only expectation is corrected. The
other failure occurred in the frozen Python process: SIGINT reached importlib's
weakref callback, which logged an ignored KeyboardInterrupt; the query then
returned its timeout diagnostic with exit 2. Native comparison was not reached
for that failing branch. The strict signal assertion was not relaxed. Both the
corrected closed-route case and a fresh strict signal rerun then passed (2/2).

- Original aggregate: `.local-tests/projection-run/http3-regression.log`
- Separate strict rerun: `.local-tests/projection-run/http3-focused-recheck.log`

The fresh pass does not erase the reference timing failure or establish arbitrary
signal-schedule parity. The source and binary digests above still matched after
all these checks. The prior HTTP-v3 test's only source change here updates the
preview/execute availability diagnostics; its signal test is unchanged.

## Corrected receipt fence

The narrow correction adds the shared review helper, test-package initializer and
new qualification-control module to the before/after harness content fence. It
verifies both loaded modules' origins and their actual `BINARY`, `REFERENCE` and
`PYTHON` selections against the recorded artifacts, preserving the interpreter's
venv path. The executable `ORACLE` string must agree between the modules; its
SHA-256 is recorded and compared again after the suite. Source or loaded-selection
drift fails before a successful receipt can be written.

Five mandatory controls passed: helper-file mutation; matching selection capture;
every binary/reference/interpreter misdirection in either loaded module; a venv
path alias resolving to the same interpreter binary; and wrong module origins or
changed oracle programs. These controls use disposable files and call the actual
receipt validation functions. They do not alter real helpers or redirect real
qualification subprocesses.

The corrected source-bound replay passed those 5/5 controls, all 9/9 paired/refusal
cases, six store adapter tests and the runtime gate test, without skips or expected
failures. Source and binary digests remain those recorded above; no owner/runtime
semantic code changed in this correction.

- Corrected receipt: `.local-tests/projection-run/corrected-proof.json`
- SHA-256: `6ab23abde87429dd03a534e92beb5dec841540421ab5f85f9fe28c7b9508cdf1`
- Run log: `.local-tests/projection-run/corrected-qualification.log`

This supersedes the incomplete e267 helper-fence receipt for the selected local
adapter evidence. It does not change the retained HTTP-v3 aggregate failure,
publication refusals, migration gates or release status. Independent re-review
of the correction remains required.

## Signal-readiness correction after merged-source testing

The helper-fence correction passed independent re-review at `532a901`: controls
5/5, cases 9/9, store tests 6/6 and runtime 1/1. That independent receipt is
`.local-tests/projection-run/independent-corrected/proof.json`, SHA-256
`39e031e5f2e45be05d0cdc66fa84f29b2ecaa12bd4355bb5adb518e46e5fdff9`.
It retained a plain-mode frozen SQL-interruption/2 versus native/130 outcome.

A later merged-source run at `bc5da09` demonstrated that writer-lock observation
plus 0.1 seconds still allowed the frozen signal to arrive during constructor
migration validation, producing `migration 023_fallback_resolution.sql failed:
not authorized`. Its failed aggregate log remains in the integration worktree at
`.local-tests/cli-next/combined-bc5da09/projection.log`. This new phase is not an
accepted cancellation outcome and is not hidden by broadening exit-2 assertions.

The corrected fixture adds an observation-only SQLite connection subclass in the
frozen subprocess. A trace callback recognizes the uniquely tagged recursive
selection statement. Only its subsequent SQLite progress callback writes a
readiness marker through an inherited pipe. The parent waits for that marker
before sending SIGINT; constructor/migration validation has therefore finished.
The wrapper forwards the owner's progress callback and return value, and honors
callback clearing. An in-memory negative/control check proves unrelated SQL emits
no marker, the tagged query does, and normal callback behavior remains intact.
No reference files or product cancellation code are changed.

The native executable remains uninstrumented. Its fixture readiness is still
writer-lock observation plus 0.1 seconds, without an exact native SQL-phase
claim. Existing native tests retain deterministic interruption after actual
mapping insertion and after layout ownership writes. The two signal phases are
not claimed equivalent; the receipt records their exact boundaries and the
instrumented oracle program digest.

The synchronized local replay passed 9/9 cases, five receipt controls, six store
tests and the runtime test. All three modes observed the exact frozen
`SQL query failed: interrupted` CatabolicError/exit-2 branch versus native
KeyboardInterrupt/exit 130; all catalog/output state checks and lock-release
checks passed. Arbitrary migration errors, weakref diagnostics and timeouts still
fail the assertions. This is a bounded instrumented-signal witness, not general
cancellation-schedule or identical query-phase parity.

- Receipt: `.local-tests/projection-run/signal-synchronized/proof.json`
- SHA-256: `ec6cb80285cae9b37f320e15151a75b7c204ebeffe9f1f6b16a1be0f500dbc5d`
- Log: `.local-tests/projection-run/signal-synchronized.log`

Existing private debug artifacts were reused; source and binary hashes remain
unchanged. Earlier receipts and failed logs are retained. This fixture-only
correction requires independent review and fresh merged-source qualification.
