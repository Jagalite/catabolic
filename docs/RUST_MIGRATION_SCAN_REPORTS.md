# Ordinary native scan report-shape completion

Base: `2c123ced74bbf45dc1b0635e65ec6c60a4262b94`.
Frozen oracle: `453fca983222c6665a48775eef67c489c96b527e`, Python 3.14.6.
This records the initial opt-in Linux report-shape checkpoint, **not full scan,
maintenance or watch parity**. Its subsequent narrow
[ordinary accounting follow-on](RUST_MIGRATION_SCAN_ACCOUNTING.md) changes real
batch scheduling/accounting. Neither changes the default engine, M6 gates,
publication revalidation/fences, filesystem safety policy, freshness eligibility,
installed/release qualification or the status of either blocked broad review.

## Native facts now reported

The existing observation owner returns the following ordinary fields, duplicated
consistently under `scans` and each observation's `report`:

- `media_headers.invalid`: at most 100 actual invalid rows joined to file paths,
  sorted by path; `invalid_truncated` follows the full native invalid count
- `progress.committed_bytes`, `last_progress_at` and `batches`: SQL aggregates of
  the actual committed `observation_batches` for that job
- `temporary_bytes`, `temporary_free_bytes` and `elapsed_seconds`: values sampled
  by the existing resource checks, retaining their existing order and thresholds;
  a measurement not reached is null rather than invented
- `suggested_actions`: no action for a complete scan; otherwise the actual oldest
  active request for continuation and the observed source's scan-policy operation
- Observation `coverage`: persisted source, exclusions, execution and dirty
  generation, with the method corresponding to that execution
- `retry_state`: actual job state and the owner's recorded deferred result
- `reuse`: provenance attached at the branch that created, reused, joined,
  deferred or resumed the job, including readmission; cancelled requests retain
  the frozen omission of this field

`public_scan` captures a real request-time `after` value once before its source
loop. The owner still computes its existing stricter admission-time floor. No
new admission rule, claim, batch, scope completion, publication or fence was added
by report enrichment itself. The later accounting change creates real owner
batches; the report module still only reads the catalog. The existing finish transaction
persists the enriched report; report-only fields do not enter compatibility keys.

## Explicit remaining differences

The ordinary one-file JSON/machine diagnostics continue to fail, and required
discovery continues to count known-nonparity witnesses separately:

1. The ordinary no-callback accounting follow-on at `ccf130e` now records the
   frozen schedule's three actual batches for the one-directory fixture, including
   its real final empty flush. Both report counters read those durable rows
2. That follow-on also includes actual UTF-8 directory-path and encoded scope
   payload bytes in checkpoint thresholds and durable committed bytes. Scope
   details contain actual elapsed seconds, whose encoded lengths can differ
   naturally between executions. Totals are checked against each engine's own
   durable payloads, never replaced with the oracle's count. See
   [ordinary scan accounting](RUST_MIGRATION_SCAN_ACCOUNTING.md) for boundaries,
   exact ledger comparisons and the separately retained callback/generated limits
3. Frozen `minimum_start` equals the public `after` boundary; native
   `minimum_start` remains its later admission/started time. Recording `after`
   removes the old zero placeholder without relaxing the existing native floor

Execution identities, worker PID, real wall/monotonic times and sampled free
space also naturally differ. The full diagnostic compares both reports without
normalizing the remaining freshness or actual timing-dependent differences into equality. It
retains untouched stdout/stderr. Exact empty/preflight/text and shell comparisons
remain separate checks; they do not establish full nonempty-report parity.

Pending/unavailable/error reports, all continuation/barrier edge cases and
external publisher callbacks are not qualified by this checkpoint. This does
not claim every possible report contains every ordinary completed-scan field.

## Separate narrow resume forwarding fix

The new read-only resume test exposed an inherited public adapter error:
`public_scan` manufactured `exclude: []`, then the resume owner correctly rejected
that present argument as an attempted execution amendment. After the public
adapter's existing checks reject actual nonempty exclusions and other amendments,
it now removes only that normalized empty default before forwarding the resume.
The owner admission and mutation policy is unchanged. This correction is separate
from report enrichment and has paired frozen-oracle JSON/machine resume checks,
exact text/JSON/machine invalid-exclusion precedence checks, and whole-catalog
before/after snapshots. The original failed test is retained in the evidence.

## Verification

`tests/scan_report_support.py` checks complete field sets and exact newly supplied
semantic fields for paired ordinary JSON/machine maintenance/watch reports, plus
SQL-backed progress, durable report equality, positive actual resource samples,
actual scope-byte accounting and the remaining freshness difference. Watch progress lines must
carry the same owner report as the final cycle.

`crates/catabolic-runtime/tests/scan_reports.rs` checks actual resource/durable
values; unchanged disposable source bytes; no mappings, journal, processing or
watch-revision effects; read-only resume and cancelled-read behavior; completed
reuse without extra batches; invalid headers sorted and capped at 100 from 101
real checks; deferred continuation guidance and repeat reuse without new batches.
Existing maintenance/watch tests retain output/refresh refusal and state checks.
All sources and catalogs are disposable. A test-compilation correction (owned
SQL string parameters), the inherited resume failure, and an initial unqualified
nonempty scan-text comparison are retained in their initial logs. The latter
compares the entire dynamic report, including the real batch/byte differences;
it remains an explicitly failing diagnostic, not a passing test with fields
removed. Exact empty scan text remains a separate passing check.

Reproduce with the pinned local environment, a private target, offline Cargo,
debug info/incremental compilation disabled and two build jobs:

```
cargo build --offline -p catabolic-cli --bin catabolic-legacy-native
cargo test --offline -p catabolic-runtime --test scan_reports --test maintenance_orchestration
cargo test --offline -p catabolic-cli --bin catabolic-legacy-native
cargo clippy --offline -p catabolic-cli --bin catabolic-legacy-native -- -D warnings
CATABOLIC_LEGACY_BINARY=PRIVATE_TARGET/debug/catabolic-legacy-native \
  python -m unittest tests.test_rust_scan_reports_cli tests.test_rust_maintenance_cli tests.test_rust_scan_watch_cli -v
```

Run the separately failing JSON and machine diagnostics exactly as described in
[RUST_MIGRATION_MAINTENANCE.md](RUST_MIGRATION_MAINTENANCE.md) and
[RUST_MIGRATION_SCAN_WATCH.md](RUST_MIGRATION_SCAN_WATCH.md). The source-bound
[development receipt](../tests/parity/native-scan-report-shapes-checkpoint.json)
records terminal outcomes, precise hashes and limits; it is not aggregate or
release qualification.

For the separately failing direct scan text diagnostic:

```
CATABOLIC_SCAN_DIAGNOSTIC_DIR=EVIDENCE_DIR \
  python -m tests.test_rust_scan_reports_cli --diagnose-full-scan-text
```

This retains exact text stdout/stderr and exits 1. No text normalization is used
beyond replacing the fixture catalog paths. JSON/machine diagnostics retain
their original limited execution-identity/timing normalization and still fail.

## Initial report-shape terminal result (2026-10-12, Linux x86_64)

- Required combined Python discovery: 28 checks pass, comprising 24 other checks
  and four explicit known-nonparity witnesses; no skips or expected failures
- Runtime: three report checks and four existing maintenance checks pass
- Legacy CLI unit tests: 12 pass; scoped CLI Clippy denies warnings and passes
- Rust format, scoped Ruff check/format, ledger freshness and whitespace pass
- All five separately executed full-report diagnostics fail with exit 1, as
  documented; four JSON/machine comparisons have zero structural omissions
- Candidate source, oracle source and executable hashes rechecked unchanged
- Independent narrow review is pending; the first fresh reviewer could not be
  admitted because the concurrent agent limit had been reached
