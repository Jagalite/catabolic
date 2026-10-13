# Preserved top-level watch: partial native adapter

Base: `ce3095f49153e9e8cbe7d2e1da56882ca4c12fea`.
Frozen Python oracle: `453fca983222c6665a48775eef67c489c96b527e`.
This opt-in Linux development checkpoint does not qualify the full CLI leaf,
installed artifacts, any milestone, underlying effect owners, macOS or release.
Python remains the default. The blocked broad filesystem/observation/watcher
review has not been retried or reopened.

Report-shape follow-up: [native scan reports](RUST_MIGRATION_SCAN_REPORTS.md)
completes the ordinary missing fields described below from actual native facts.
The original evidence sections remain historical; batch-count, byte-accounting
and freshness-floor differences are still explicit nonparity.

## Ownership and ordinary behavior

`catabolic-legacy-native watch` forwards one polling cycle to the existing native
`public_scan` and `public_processing` owners. It shares the maintenance metadata
helper's stable-revision protocol, adding a bounded `stable_page_with_limit`
entrypoint; the original maintenance entrypoint retains its 1,000-row default.
The new adapter does not implement traversal, processing, filesystem publication,
recovery, or a named watcher engine in Python or Rust.

Loop interval/cycle admission precedes Store opening. Profile, settle and batch
admission precede scan. Processing worker/retry validation retains its later
position after scan and stable-file enqueue. All stable files are enqueued in
bounded pages; `--batch` limits each metadata transaction and processing run.
The processing run includes pre-existing queued jobs, unlike maintenance's
cycle-local selection. Cached failed jobs retain the frozen watch completeness
behavior; the adapter does not reinterpret owner reports.

A completed cycle closes its Store, emits and flushes one JSON progress line to
stderr, then either returns the frozen text/JSON/machine result or waits without
a writer. `--cycles 0` continues until interrupted; incomplete cycle results do
not themselves end a bounded/continuous watch. SIGINT reaches SQLite checks,
scan callbacks and native processing/tool cancellation. The adapter joins its
cancellation bridge. Its per-cycle cleanup token is separate from the external
signal flag, so ordinary Processing cleanup cannot impersonate an interrupt.
SIGINT returns 130; selected ordinary validation errors return 2.

## Explicit automatic-refresh refusal

Frozen watch allows Processing to finish with automatic catalog refresh. This
adapter does **not** qualify that dependent output path. It runs Processing with
`_refresh=false`, checks enabled pending refresh work both before and after the
run, and invokes the existing public Reconciler publication gate inside a
metadata transaction when such work exists. This refuses before entering the
refresh owner or staging its desired mappings, journal or output mutations.
Even work currently in refresh backoff is refused; it is still pending work.

This is an explicit compatibility gap, not automatic-refresh parity. Refusal
returns exit 2 with the public gate error and no completed-cycle progress line.
Inventory and enqueue metadata may already have committed. If refresh work
arrives during a detached processing wait, the processing completion may also
have committed before the post-run refusal. No fixture bypass is forwarded.
An empty/disabled refresh queue leaves ordinary scanning and processing available.
The underlying refresh, filesystem, observation and named-watcher owners are
unchanged.

## Inherited scan-report nonparity

The forwarded `scan` value has the same known missing fields as documented for
[maintenance](RUST_MIGRATION_MAINTENANCE.md): observation `coverage`, `reuse` and
`retry_state`; header `invalid` and `invalid_truncated`; `suggested_actions`;
and progress `temporary_free_bytes`, `last_progress_at`, `committed_bytes`,
`temporary_bytes` and `elapsed_seconds`. The ordinary one-file fixture reports
two native batches versus three frozen batches, including progress batches, and
observation `guarantees.after` is 0.0 rather than the frozen freshness timestamp.
These omissions remain visible in final reports and every progress line.
The adapter neither synthesizes missing fields nor silently discards scan data.

Required tests separately assert two known-nonparity witnesses. They are not
full-report parity passes. Shell comparisons exclude only the explicitly named
forwarded scan payload, require that payload to remain present, and compare the
remaining cycle/result values. Separate full-result JSON and machine diagnostics
are expected to fail and retain the untouched stdout/stderr of both processes.
No expectedFailure/skip is used to convert a missing dependency into success.

## Evidence scope and fixture corrections

`tests/test_rust_scan_watch_cli.py` covers exact empty text/JSON/machine results,
bounded cycles, admission order, selected profiles/locations, writer contention,
bounded whole-backlog progress, stable revision hashes, settling, cached failure,
late processing validation, idle and real tool-wait SIGINT, tool reaping,
writer/lease release, immediate restart, and pre-/post-processing refresh refusal.
All inputs, catalogs, source files and output roots are disposable.
The blocked tool is an explicit local fixture executable, not a live service.

The initial fixture diagnostics are retained rather than overwritten:

- The first unknown-location diagnostic exposed the scan owner's different
  message. The new adapter now performs the frozen location-existence admission
  check after settle/batch validation, without changing the scan owner
- Intermediate per-file backlog states differed because both existing owners
  sort by independently generated second-resolution `created_at` and job UUID.
  The corrected test fixes the pre-existing job timestamp, checks exact bounded
  counts each tick and compares final per-file states after draining. It does not
  claim identical tie-break choices among independently generated new job IDs
- Frozen interrupted Processing retains a running job row and an expired claim
  row (`lease_until=0`). The corrected test checks absence of live claims, actual
  tool reaping and successful immediate restart, rather than requiring all rows
  to be deleted. The original row-count failures remain in the evidence logs
- Switching the fixture's ffprobe identity at restart correctly yielded a
  `changed` result. The corrected fixture preserves the same executable bytes
  and version, using a disposable readiness file to permit the resumed work
- A tool readiness marker can precede its parent's detached wait. The wait
  fixture now requires observable writer release within three seconds while the
  tool remains blocked; it does not assume the marker proves the parent has
  detached. Idle release after the flushed cycle line is still checked immediately
- Early refresh fixtures stopped at missing-layout and unsafe-plan diagnostics.
  The final adapter's explicit public gate is checked before either dependency
  path, with unchanged mappings/journal/output state

The additional metadata test covers invalid page limits, page boundaries,
settling, failed-page rollback, nested transaction rollback and the unchanged
maintenance entrypoint. Two CLI unit tests cover signal forwarding and ordinary
owner-error provenance. Existing maintenance and CLI presentation tests remain
required regressions. Exact hashes and terminal outcomes are recorded in the
local development evidence receipt; these are not installed-wheel or aggregate
qualification.

## Recorded final local result (2026-10-12, Linux x86_64)

- Each Python 3.14.6/3.11.15 harness run: 13 required checks pass, comprising
  nine paired ordinary checks, two native refresh refusals and two explicit
  known-nonparity witnesses, with no skips or expected failures. Both harnesses
  launch the pinned Python 3.14.6 oracle
- Maintenance/legacy-gate regression: 12 checks pass, including its two existing
  known scan-report nonparity witnesses
- Runtime metadata/maintenance: four checks pass; legacy CLI unit tests: eleven
  pass; owner-ledger source tests: three pass
- Offline build, scoped CLI Clippy with warnings denied, Rust formatting,
  scoped Ruff check/format, ledger freshness and patch whitespace checks pass
- Full-result JSON and machine diagnostics each fail with exit 1; untouched
  oracle/native stdout/stderr and the original failed attempts remain retained

The immutable tested executable SHA-256 is
`fb3022040499fd61c4d29b744276b14d6e448db2ff3c6bde825e3c151f60094f`.
The [development receipt](../tests/parity/native-scan-watch-checkpoint.json)
records exact source/oracle/artifact/log hashes and qualification limits.

## Reproduce

Use the pinned local environment, an isolated target, offline Cargo, debug info
and incremental compilation disabled, and two build jobs:

```
cargo build --offline -p catabolic-cli --bin catabolic-legacy-native
cargo test --offline -p catabolic-runtime --test maintenance_orchestration
cargo test --offline -p catabolic-cli --bin catabolic-legacy-native
cargo clippy --offline -p catabolic-cli --bin catabolic-legacy-native -- -D warnings
CATABOLIC_LEGACY_BINARY=PRIVATE_TARGET/debug/catabolic-legacy-native \
  python -m unittest tests.test_rust_scan_watch_cli tests.test_rust_maintenance_cli -v
python scripts/legacy_cli_owners.py --check
```

Run full-report diagnostics separately from required discovery, retaining their
exit-1 reports as nonparity evidence:

```
CATABOLIC_WATCH_DIAGNOSTIC_DIR=EVIDENCE_DIR \
  python -m tests.test_rust_scan_watch_cli --diagnose-full-scan-parity --json
CATABOLIC_WATCH_DIAGNOSTIC_DIR=EVIDENCE_DIR \
  python -m tests.test_rust_scan_watch_cli --diagnose-full-scan-parity --machine
```

Not covered: every scan/processing cancellation window, every OS/SQLite exception
class, scan continuation/barriers, external-tool/platform combinations, named
watcher safety, publication/recovery correctness, exact nonempty scan reports,
or complete performance/resource qualification.

## Later selected SIGTERM boundary checkpoint

The historical signal evidence above is retained. The later
[selected legacy SIGTERM checkpoint](RUST_MIGRATION_LEGACY_SIGTERM.md) aligns
standalone TERM exit/no-new-output presentation for rule `--worker`, fallback
worker and top-level watch only after existing native cleanup finishes. Private
Python callable and shared HTTP signal behavior remain unchanged. This does not
close the other documented gaps or promote any leaf or migration gate.
