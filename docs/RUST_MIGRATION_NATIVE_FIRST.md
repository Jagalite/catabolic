# Native-first completion and Python retirement

**October 16 continuation:** The user selected strict unknown-profile reads and
structured mapping-runtime errors without tracebacks. See the
[approved retirement decisions](NATIVE_RETIREMENT_DECISIONS_2026_10_16.md) for the
bounded replacement contracts and remaining default-switch gates, and the
[macOS resume evidence](NATIVE_TAKEOVER_MACOS_RESUME.md) for the failed aggregate
and separately validated fixture repairs.

**Current direction, October 13:** Design the reusable native catalog now and
continue implementation on `codex/rust-migration-m4-plus`. The
[native catalog plan](CATABOLIC_NATIVE_CATALOG_DESIGN.md) governs new semantics,
explicit schema upgrade and the next implementation sequence. Python remains a
thin-wrapper-last target. This changes sequencing, not safety or evidence gates.

**Current qualification boundary, October 15:** the
[catalog checkpoint](RUST_MIGRATION_CATALOG_CHECKPOINT_2026_10_15.md) separates
the complete Linux native and installed checks at `00950e7` from its interrupted
dynamic run and the subsequently adopted, focused-qualified diagnostic repairs.
No later commit inherits the earlier aggregate or wheel qualification.

**Historical macOS qualification:** [resumed on October 13](RUST_MIGRATION_MACOS_RESUMED_2026_10_13.md).
The `f8c2759` native CLI lane passes all 28 selected methods and both lifecycle
and playback models pass. Its completed full aggregate has failures. The
`cd70ac7` fixture and CI portability repairs pass the selected Python and shell
checks, Rust formatting, strict Clippy and test compilation. Its complete native
replay discovers and executes all 53 artifacts: **579 tests pass, five fail and
six are ignored**. Five available mandatory witnesses pass; OCR is blocked by
missing Tesseract, and the Linux entry-budget witness is excluded on macOS.
All 11 before/after identity checks pass, but the aggregate remains failed.
A separate unchanged-artifact diagnostic passes both notification cases and the
regex case but repeats the fallback PID-file failure. Test-only commit
`5e912f1` separates anchor semantics from host timing and increases the sleeping
fallback fixture's timeout while retaining its strict outcome and cleanup checks;
formatting, strict Clippy, both test compilations and all four focused tests pass
with unchanged source and artifact identities. Existing distribution CI run
`38338151813` completed successfully for `cd70ac7`, with all 11 jobs passing.
Its reference tests retain their documented skips and deferred native checks.
That hosted result does not qualify the failed native aggregate or the later
test-only commit.
The [October 13 stop handoff](RUST_MIGRATION_STOP_2026_10_13.md)
is preserved as the historical boundary before this resumption.

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

1. **Prove native hosting (R0).** Define the small supported lifecycle/error
   contract and real scan/cancellation/reopen with native helper identity. The
   October14 Catabolic-first amendment defers Motion dependency adoption/testing;
   standard Rust1.99 in the current native workspace is explicitly authorized.
2. **Prove catalog semantics before schema changes (R1).** Exercise the chosen
   version-item composition through actual component/fallback owners, stable
   timelines, alternative multipart representations and revision-pinned intervals.
3. **Implement the versioned catalog foundation (R2–R4).** Add explicit schema
   upgrades, atomic command receipts, contributions/pins, reviewed merge/split,
   host scope admission, ranked search and restore-aware invalidation.
4. **Qualify execution and cutover (R5–R6).** Account for actual worker lifetimes;
   rehearse resumable identity-preserving import and Catabolic-side one-owner
   restart admission. Actual Motion reference reconciliation and deployment remain
   deferred; no live cutover is implied by this plan.
5. **Complete native product and wrapper gates (R7).** Close the still-required
   behavior, safety/recovery, HTTP, standalone artifact and platform gates below.
   Finish the thin Python wrapper and default transition last.

See the catalog plan for dependencies and acceptance cases. Its package gates
qualify catalog adoption; they do not replace full migration/release qualification.

## Retained native completion work

The former ordered list is retained as work scope and historical evidence. Its
ordering is superseded by the catalog critical path above.

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
content protection; the catalog extensions and upgrade in R0–R6; final native-default distribution transition. No full parity
claim follows from the current 242-route owner ledger.

**Test infrastructure:** three stale source/CI assertions have reviewed fixes;
the idle-watch fixture now establishes a genuine production tick before testing
idle SIGINT. Two further test-only startup/regex corrections are committed as
`5e912f1`, with all four focused Rust tests passing. Historical failures remain
failures. A frozen-reference fallback
fixture also returned `stale_or_missing_resolution_plan` in an earlier full
reference run; its original cause is not established. A later diagnostic pass
must not erase it or imply a native product defect was fixed.

**External/qualification gates:** the two denied review lanes, actual macOS and
mounted-filesystem acceptance, approved real-service environments, complete
resource/performance/rollback evidence, and remaining prerequisite gates.
Official pinned development tools and locked dependencies were subsequently
authorized and staged in isolated environments. Historical Linux wheel/client
cohorts are source-specific; the final integrated source still needs its own
artifact/installed qualification.

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
