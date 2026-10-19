# Ordinary native maintenance: partial implementation

Baseline: `721b80ff64719d2440cd678fbcd39fb4ddd0b0cf`.
Frozen Python oracle: `453fca983222c6665a48775eef67c489c96b527e`, Python 3.14.6.
This is an opt-in development checkpoint, **not full maintenance parity**.
Python remains the default. No milestone, publication, installed-wheel, platform,
release, or destructive recovery qualification is promoted by this change.

Report-shape follow-up: [native scan reports](RUST_MIGRATION_SCAN_REPORTS.md)
completes the ordinary missing fields described below from actual native facts.
The original evidence sections remain historical; batch-count, byte-accounting
and freshness-floor differences are still explicit nonparity.

## Ownership and implemented paths

The preserved `catabolic-legacy-native maintenance` route now owns one ordinary
cycle through `catabolic_runtime::maintenance::run`. It opens the ordinary writer
and validates the profile before maintenance admission. Bounded numeric arguments,
rule-option dependencies, exclusions, catalog selection, watcher ownership, pending
journal/artifact recovery, source existence and root admission retain their order.
Root checks, remount handling, inventory scanning, processing and rules delegate
to existing production native owners. Those underlying owners are unchanged.

The new store helper provides SQL aggregates over the full selected-profile
backlog and the stable-revision metadata protocol. Stable observations are paged
in groups of 1,000, revisions are transactionally recorded, only this cycle's
scan IDs qualify, and only the requested bounded job IDs are passed to Processing.
Unrelated queued jobs do not run. Processing uses `_refresh=false`; no implicit
catalog refresh or Python fallback is introduced. Existing nested transactions
own rollback. The ordinary signal bridge shares its cancellation token with scan,
processing and rule tool execution, with explicit cancellation checks between
stages. Cancellation escapes as exit 130, rather than an incomplete success.

Implemented transport includes ordinary text, JSON and machine-v1 envelopes,
exit 0/2/3/130 routing, flushed stage progress on stderr, exact preflight error
report order, SQL summary totals independent of detail limit, exclusions guidance,
analysis backlog text, and no-render rule reporting. Scan-owner reports are
forwarded unchanged: **their inherited nonparity is not hidden or synthesized**.

## Refused dependent paths

After successful inventory/optional analysis/rule enqueue, a cycle with output
catalogs stops at `planning` with `complete=false`, `mappings_committed=false`,
empty staged layouts, an explicit existing public Reconciler publication-gate
error, and exit 3. The gate executes inside a metadata transaction before any
layout staging. No desired mappings, publication journal, owned links, managed
output bytes or manifest are changed by that dependent stage. No private fixture
option is forwarded, and removal budgets cannot relax the gate.

`--rules --render-rules N` with N greater than zero stops at `rules` before rule
admission/rendering with an explicit M6 error. `--manifest` cannot be reached
without qualified output publication. Ordinary inventory/analysis metadata may
already have committed at earlier stages, exactly as the stage report indicates.
These intentional refusals differ from frozen Python output cycles.

## Explicit inherited scan-report blocker

Disposable one-file inventory JSON and machine reports have two named known-nonparity
witnesses in `tests/test_rust_maintenance_cli.py`. The existing `public_scan::execute`
owner returns the following differences relative to the frozen Python oracle:

- Each scan (also duplicated under observation `report`) lacks
  `media_headers.invalid`, `media_headers.invalid_truncated`, `suggested_actions`,
  and `progress.temporary_free_bytes`, `progress.last_progress_at`,
  `progress.committed_bytes`, `progress.temporary_bytes`, `progress.elapsed_seconds`
- The observation record lacks `coverage`, `reuse`, and `retry_state`
- The one-file fixture reports 2 batches rather than Python's 3, in both `batches`
  and `progress.batches`
- `guarantees.after` is 0.0 rather than the ordinary Python freshness timestamp

Execution UUIDs, worker PID and timing naturally differ as well; those do not
explain the structural omissions above. Missing fields are not present in the
returned native owner value. No coverage, timing, progress or retry value is
invented by this adapter. Fixing dependency semantics and reviewing its safety
are outside this narrow change. The ledger therefore keeps maintenance `partial`.

The passing report-shell differential excludes only the explicitly identified
scan/observation payloads and asserts they remain present. Full JSON/machine
comparisons are separately invoked diagnostics that fail and record both reports.
Required test discovery asserts the known omissions explicitly; it has no
expected failures and does not count these witnesses as passed parity. Closing
the dependency must update the witnesses and rerun the full diagnostic.

## Narrow evidence and limits

`tests/test_rust_maintenance_cli.py` exercises frozen-oracle argument/preflight
order in text/JSON/machine modes; inventory/exclusion text and durable rows;
summary status counts beyond detail limits; settling; scan-scoped exclusions;
bounded repeat sniff analysis; unrelated-job preservation; cached failure stops;
no-enabled-rule reporting; bounded enabled-analysis-rule enqueue without execution;
output/rule-render refusal; and pending recovery.
`crates/catabolic-runtime/tests/maintenance_orchestration.rs` exercises cancellation
between stages, writer release, read-session refusal, and whole-page/nested
stable-revision metadata rollback. All catalogs and source/output roots are
disposable, and no live service, external package or credential is used.

Not qualified here: render/external rule mixes, external analysis tools, every cancellation
window or OS/SQLite exception subclass, scan continuation/barrier guarantees,
remount repair edge cases, all layout/reconciliation/publication/recovery behavior,
installed artifacts, macOS, release builds, Python default switching or full CLI
leaf parity. This is not a broad review of existing effect owners.

Reproduce with the pinned local environment, a private Cargo target, offline Cargo,
debug info disabled, incremental compilation disabled, and two build jobs:

```
cargo build --offline -p catabolic-cli --bin catabolic-legacy-native
cargo test --offline -p catabolic-runtime --test maintenance_orchestration
cargo test --offline -p catabolic-cli --bin catabolic-legacy-native presentation
CATABOLIC_LEGACY_BINARY=PRIVATE_TARGET/debug/catabolic-legacy-native \
  python -m unittest tests.test_rust_maintenance_cli \
  tests.test_rust_legacy_cli.NativeLegacyCliTest.test_maintenance_preflight_and_http_gate_are_explicit -v
python scripts/legacy_cli_owners.py --check
```

Run the intentionally failing full-result diagnostics separately from required
native discovery, with an explicit output directory for both untouched reports:

```
CATABOLIC_MAINTENANCE_DIAGNOSTIC_DIR=EVIDENCE_DIR \
  python -m tests.test_rust_maintenance_cli --diagnose-full-scan-parity --json
CATABOLIC_MAINTENANCE_DIAGNOSTIC_DIR=EVIDENCE_DIR \
  python -m tests.test_rust_maintenance_cli --diagnose-full-scan-parity --machine
```

Each currently exits 1. Those failures are dependency nonparity evidence, not
waived qualification success. Required discovery's two positive nonparity
witnesses must be counted separately from the passing parity scenarios.

## Recorded local result (2026-10-12, Linux x86_64)

- Required narrow Python discovery: 12 tests pass, comprising 10 transport/state/
  refusal checks and two **known-nonparity witnesses**, with no expected failures
- New runtime integration: three tests pass (stage cancellation, read admission,
  and stable-revision rollback); report-detail bounding unit: one test passes
- Existing presentation regressions: five tests pass
- Offline build, scoped CLI Clippy with warnings denied, Rust format, Python
  Ruff check/format, owner-ledger freshness and patch whitespace checks pass
- Separately executed full JSON and machine diagnostics: both **fail**, exit 1,
  with untouched oracle/native reports retained in the local evidence directory

The private build target, tested executable hash, changed-source hashes, frozen
oracle hashes, commands and copied logs are in the local development evidence
receipt. None of these results constitutes aggregate/release/M6 qualification.
