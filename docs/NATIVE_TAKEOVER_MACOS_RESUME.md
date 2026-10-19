# Native takeover: first macOS qualification and fixture repairs

Development resumed from publication commit
`df868920dc277d16562c3f6168b9f8afd6b89084` on
`codex/rust-migration-m4-plus`. The original takeover and publication documents
remain historical receipts. This continuation changes test fixtures only; it
does not enable a native default, migrate a catalog, activate the workflow
proposal, or qualify a release.

## Source and local evidence

The unmodified publication snapshot was tested in the isolated worktree
`/Volumes/seed1/Projects/catabolic-takeover-macos`. Repairs remain on the original
feature branch. Baseline and repair builds use separate external-volume target
directories, with Rust 1.99.0 on `aarch64-apple-darwin`, locked offline
dependencies, debug information disabled, and incremental compilation disabled.
Fixtures use physical temporary directories on that volume.

Kyoto compiled the tests; its shared policy added `--no-run`. Test execution
was performed separately. Compilation success is not counted as test success.

| Cohort | Result | Kyoto compilation job |
| --- | --- | --- |
| Baseline query setup ownership | Four passed | `315991165be546568086811c3689a17d` |
| Baseline whole workspace | Compilation passed; execution failed, see below | `20c97734eba34446b78e5ddd55504b5d` |
| Initial two-fixture repair, runtime library | 143 passed, one failed, two ignored | `efa2cea043d54928a8fd2de9bebe93b6` |
| Final three-fixture repair, runtime library | 144 passed, zero failed, two ignored | `db293d09d90c42b2ad3c16719b53ecc9` |
| Fourth fixture repair, execution admission | Uncompiled/unexecuted; queued job cancelled after persistent OS memory pressure | `4af1d4d2124346ceb8001bc5bdb4d96a` |
| October 16 execution-admission retry | Eight passed, zero failed/ignored | `9b95cab62afb425e9deec928b67840ac` |

The unmodified workspace command terminated with exit 101 after 1,485.96 seconds.
Its 105 libtest summaries total 1,386 passed, 16 failed and 34 ignored executions,
with seven failed Cargo targets. These are execution counts, not unique product
contracts or complete platform coverage. The source remained clean at the exact
publication commit. The full log SHA-256 is
`56ed59157a7444a9da996309e07bb57cd14fd7e0f3b0385a27d1570f98841efc`.

Raw local evidence directories, which are not shipped repository artifacts:

- `/Volumes/seed1/Projects/catabolic-builds/evidence/takeover-df86892-macos`
- `/Volumes/seed1/Projects/catabolic-builds/evidence/takeover-macos-fixture-repair`
- `/Volumes/seed1/Projects/catabolic-builds/evidence/takeover-macos-fixture-repair-02`

The final runtime receipt records patch SHA-256
`e5f30e2700bd1fbbafd075e169029e9c0d05362c42415f4f0124c7217901d611`
and executable SHA-256
`1b732e60acc40df60330ae611c8e1d9f0b9f631c9da5a7771da669a3b390372e`.
The patch is relative to the publication commit and covers the three runtime
source files only. Earlier failed runs remain retained separately.

## Fixture corrections

1. The no-child-scope test assumed `/bin/true`, absent on macOS. It now uses
   `/bin/sh -c 'exit 0'` and asserts successful exit after the guard is dropped.
   The refused command and absent-side-effect assertions remain intact.
2. The descendant-retirement test used a 250 ms command deadline. During the
   parallel baseline run, it expired before the first heartbeat existed; the
   unchanged case passed alone. The fixture now allows five seconds for startup
   and execution. It still requires the retained-pipe timeout, successful exit
   when pipes are redirected, and a stopped heartbeat after cleanup.
3. The worker-cancellation fixture cancelled after a fixed 200 ms and could
   collect zero child PIDs under load. It now waits for two child-created
   readiness files before cancellation. Startup has a ten-second bound and
   cancels even on failure; post-cancellation cleanup retains its five-second
   bound, two-PID absence checks, and released-claim assertion.
4. The execution-admission integration test had the same `/bin/true` assumption.
   It now starts `/bin/sh -c 'exit 0'`, retaining its actual child wait, PID absence,
   reconciliation refusal, and unchanged-database assertions.

All corrections are within existing tests. Runtime behavior, production
deadlines, worker admission, and cleanup authority are unchanged. Formatting
and whitespace checks passed.

## Baseline failures and prerequisite limits

- The baseline runtime library failed the first two fixtures described above.
  The first repair's runtime replay then exposed the fixed-delay cancellation
  fixture; the final runtime-library execution passed all 144 ordinary tests.
- Four notification wire comparisons failed because the initial workspace run
  omitted `CATABOLIC_PYTHON_ORACLE` and `CATABOLIC_PYTHON_REFERENCE`. A separate
  replay of the same baseline executable with Python 3.14.6 and the exact frozen
  `453fca983222c6665a48775eef67c489c96b527e` reference passed all four. This does
  not relabel the initial aggregate as passed.
- The fixed-rendering-presets test failed because the installed FFmpeg lacks
  `libwebp`. Its required-preset assertion was not relaxed.
- The acyclic component-lineage depth case returned `ScopeError { code: Timeout }`
  in the parallel baseline and passed in isolation on the same executable. The
  store library finished with 499 passed, one failed and four ignored. This
  remains an aggregate qualification issue; no catalog implementation was changed.
- The composition fallback case failed its `real probes must run` assertion in
  the parallel baseline and passed alone on the same executable. The composition
  target finished with 14 passed and one failed. Its aggregate result remains
  failed, and no resolver or budget behavior was changed.
- The baseline execution-admission target finished with seven passed and one
  failed because `/bin/true` was absent, addressed by fixture correction four.
- Six storage-fault tests failed because the scratch checkout lacked the retained
  schema 1–32 `before.sqlite3` inputs under
  `.local-tests/rust-migration/m0-completion/legacy-fixtures`. All 32 inputs were
  subsequently copied from the original checkout, with byte equality and SHA-256
  recorded in `storage-fixture-copies.json`. These are disposable historical
  test catalogs, not a migration of current user data. The separate replay then
  passed all seven storage-fault tests in 119.23 seconds on the same baseline
  executable; its log is `storage-with-fixtures.log`.
- Tesseract is unavailable. Mandatory OCR, ignored helper/oracle witnesses,
  installed wheels, full canonical/reference suites, and broader platform,
  performance, service, rollback and release gates are not qualified here.

## Next continuation

The execution-admission repair was subsequently compiled through Kyoto and all
eight tests executed successfully. Its separate receipt is
`/Volumes/seed1/Projects/catabolic-builds/evidence/retirement-20261016-admission/result.json`.
The original cancelled attempt remains unqualified. The retry used:

```sh
CARGO_TARGET_DIR=/Volumes/seed1/Projects/catabolic-builds/takeover-macos-repair-target \
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
python3 /Volumes/seed1/Projects/kyoto/kyoto.py submit -- \
  cargo test --offline --locked -p catabolic-store --test native_execution_admission
```

Inspect the effective command and execute the compiled test separately if Kyoto
again applies compilation-only policy. The cancelled job never started; it has
no compilation or test credit. No qualification job remains running from this
continuation.

Prepare a complete isolated tool environment, including the required FFmpeg
encoders and Tesseract, before rerunning their acceptance lanes. Even an ordinary
workspace test invocation needs the explicit pinned oracle variables for the
notification comparisons and the retained historical schema fixtures for storage
faults. Keep a physical `TMPDIR` and separate new evidence
directories. Investigate the catalog-lineage aggregate failure without changing
production budgets merely to obtain a passing test.

The user resolved the unknown-profile and mapping-runtime-v2 choices on October
16; see [the decision record](NATIVE_RETIREMENT_DECISIONS_2026_10_16.md).
Its composed-source continuation repairs the lineage, composition and further
macOS fixture failures. A third aggregate failed only the FFmpeg `libwebp` lane.
Previously declined review lanes, production effects, default switching, and the
inactive workflow proposal retain their existing boundaries.
