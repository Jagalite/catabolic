# Native-first completion and Python retirement

**Current status:** [paused at the October 13 stop checkpoint](RUST_MIGRATION_STOP_2026_10_13.md);
no implementation or qualification is running.

Priority revised by the user on **2026-10-12 at 22:45 UTC**: retire the Python
implementation, retain only a thin compatibility wrapper, and work on that
wrapper last. This changes sequencing, not the safety floor or evidence standard.

## Target and current architecture

The final standalone Rust engine and CLI must not require Python, a Python
Catabolic installation, or a Python provider helper. The old engine may remain
as a frozen differential reference during migration; it is not the shipped
implementation or an automatic runtime fallback. Preserve the existing wrapper
work, then finish its thin argument/value/exception boundary after the native
engine is qualified. Do not remove the reference or old implementation before
replacement and rollback evidence exist.

Rust already owns SQLite/migrations/transactions, catalog and definition
mutations, SQL/GraphQL, scans/watchers, filesystem journals, media/processors,
artifacts/renditions/components/fallback, consumer/delivery/playback owners,
legacy CLI dispatch and experimental HTTP. That is real Rust implementation,
not delegation to the original Python business engine. However, owner/adapter
presence is not complete behavior or safety qualification.

Two current distinctions matter:

- The existing default `catabolic` distribution and CLI are still Python. The
  native companion is opt-in. Changing the shipped default is a final gate,
  not an assertion that it has already happened
- The embedded Python notification adapter has now been replaced by
  [native supervised providers](RUST_MIGRATION_NATIVE_NOTIFICATIONS.md).
  Their inherited option gaps and explicit fail-closed differences remain
  documented; that bounded replacement is not full provider parity

## Ordered native critical path

1. **Eliminate the Python runtime provider path.** The reviewed native slice implements the allowlisted
   notification destinations in Rust: ntfy, Gotify, Discord, Telegram and SMTP,
   including their secure variants. Reuse the native notification state/retry
   owner. Compare parsing, requests, severity, static messages, delivery outcomes,
   redaction and bounds against the pinned provider through owned fixtures.
   Publish a precise option matrix; unsupported options remain functional gaps,
   not silently ignored inputs. Filesystem/remote templates, TLS-verification
   bypass and redirect enablement are outside the first fail-closed slice.
   Both actual CLI helper paths pass local supervision/drain fixtures; the
   integrated standalone release candidate passed its exact eight-test replay
   in [native checkpoint 11](RUST_MIGRATION_NATIVE_CHECKPOINT_11.md), without a
   full runtime/provider-parity claim
2. **Close named native behavior gaps.** Checkpoint 11 integrated notification
   JSON wire escaping, skipped-object GraphQL completion and two location
   diagnostics. [Checkpoint 12](RUST_MIGRATION_NATIVE_CHECKPOINT_12.md) adds the
   reviewed parser/document cases and native grouped runtime/introspection
   completion, including exact arbitrary-width opaque JSON. Its Rust/model and artifact lanes passed,
   while the aggregate and HTTP journey retain explicit failures. A larger parser-diagnostic slice, legacy
   file-admission correction and standalone CI wiring are queued separately.
   Remaining field-conflict/parser/CLI diagnostics, provider options, source
   regex/report contracts and processing/consumer lifecycle differences stay
   explicit. Each next slice must close a named contract rather than add adapters
3. **Complete production safety and recovery.** Destructive output/recovery,
   v2 apply/recovery and dependent publication remain disabled. The compatible
   safe protocol, durable-boundary matrix and both Python/Rust handover directions
   still need implementation/qualification. Production HTTP/content protection
   also has unresolved gates. The reviewed
   [recognizable orphaned WAL/SHM refusal](RUST_MIGRATION_PRIVATE_SIDECAR_HEADERS.md)
   protects both descriptor callbacks but leaves unrecognizable sidecars open.
   Previously denied broad definition/filesystem
   reviews stay blocked; new prioritization does not authorize bypassing them
4. **Qualify the standalone Rust artifact.** Prepare local native executables,
   source/toolchain/artifact manifests and outside-checkout smoke with no Python
   runtime dependency. Use Rust unit/integration/model checks and exact native
   CLI/HTTP comparisons to the frozen reference. Linux evidence does not replace
   macOS, real supported integrations, resource/performance or rollback evidence
5. **Finish the thin wrapper last.** Existing wheel/facade work and completed
   receipts are retained. In-flight checks may finish as bounded evidence, but
   new paired-wheel builds, exact public-Python contracts and optional Python
   HTTP dependencies are no longer prerequisites for the next native checkpoint.
   Complete the thin compatibility/import/pip boundary only after native owners
   and standalone behavior meet their gates

## Separate blocker lists

**Product work:** remaining provider options and named native
behavior/lifecycle contracts; safe real publication/recovery; production HTTP
content protection; final native-default distribution transition. No full parity
claim follows from the current 242-route owner ledger.

**Test infrastructure:** three stale source/CI assertions have reviewed fixes;
the idle-watch fixture now establishes a genuine production tick before testing
idle SIGINT. Historical failures remain failures. A frozen-reference fallback
fixture also returned `stale_or_missing_resolution_plan` in the latest full
reference run; its original cause is not established. A later diagnostic pass
must not erase it or imply a native product defect was fixed.

**External/qualification gates:** the two denied review lanes, actual macOS and
mounted-filesystem acceptance, approved real-service environments, complete
resource/performance/rollback evidence, and remaining prerequisite gates.
Approval for the exact optional PyPI test wheels is still pending, but that
blocks a deferred Python/ASGI lane, not native Rust progress.

## Next checkpoint acceptance

Freeze one explicit native source snapshot after its bounded changes and reviews.
Require unchanged source/artifact identities, native-crate formatting/lint/tests,
required native lifecycle and fail-closed regressions, standalone release-binary
smoke and focused frozen/native behavior replay. Keep independent tests and
remaining limitations attached to the exact artifacts. Record any blocked or
failed acceptance separately; do not count it as skipped success.

No new wrapper work is required to declare that *bounded native checkpoint*
ready. It still does not close M4–M11, authorize publication, enable dangerous
paths, or retire Python before the replacement is verified. Full completion has
no defensible date while the safety/review/platform gates remain unresolved.
