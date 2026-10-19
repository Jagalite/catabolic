# Ordinary native scan accounting and batch scheduling

Base: `ccf130e`. Frozen reference: `453fca983222c6665a48775eef67c489c96b527e`,
Python 3.14.6. This is a Linux opt-in development slice, not M5 closure,
aggregate qualification, installed/release evidence or a default-engine switch.

## Changed ordinary contract

The real native `public_scan` owner now uses the frozen ordinary schedule:

- Every buffered scope adds the UTF-8 path length plus its actual canonical
  encoded detail length; replacing a buffered path still counts the submitted
  payload bytes, as the standalone Publisher already does
- Scope records share `batch_records` with file entries. Record/byte thresholds
  are inclusive, and time checkpoints continue to use the real monotonic clock
- Running-directory notifications flush immediately. Other scope notifications
  checkpoint, so child pending/excluded scopes may share a batch with entries
- Each enumerated directory explicitly flushes after its final notification,
  even if that notification just triggered a checkpoint
- A successful ordinary traversal explicitly flushes once more, even when empty
  The empty batch still goes through the existing writer, fence and transaction
- A stopped traversal retains committed batches and discards its buffered work;
  it does not acquire a synthetic successful final batch

The public report continues to read actual `observation_batches` rows. No report
counter, duration or payload is fabricated. A one-directory/one-file fixture now
has three real batches in both engines rather than frozen three/native two.
Byte totals now include scopes in both engines. Actual directory `seconds`
values can have different JSON lengths, so total byte equality across separate
executions is not a valid universal claim. The retained full-report diagnostics
still fail, including the deliberately unchanged stricter native freshness floor.

## Explicit limits and preserved owners

The admission-time `minimum_start`, source trust/ownership/admission checks,
filesystem publication/revalidation transaction, writer reacquisition, claim
fences, journal/M6 restrictions and default engine are unchanged. The existing
post-completion interruption hook remains after a real successful flush.

Callback-backed scans and generated-registry scans keep their prior eager scope
schedule and entry-only bytes. The public standalone Publisher keeps its prior
scope-accounting/checkpoint behavior. Filtering is still performed once by the
traversal and is separately applied for standalone instances. The existing
Publisher gate now measures ordinary and callback ledgers separately instead of
claiming their schedules are identical: its bounded two-child fixture records
11 ordinary versus eight callback-backed batches, each checked against SQL.
Generated-registry equivalence is source-reviewed only in this slice.

No broad definitions, filesystem/hardlink/source-access/observation/remount or
watcher safety review is reopened or closed. No known unsafe path is reproduced.
No real catalog, live service, managed output or source-file mutation is used.

## Verification contracts

`tests/test_rust_scan_accounting.py` runs eight bounded paired cases (14 fixture
configurations) through the real frozen CLI and native public scan:

- Empty bound root and one-file root, including the real final empty batch
- Nested Unicode directory/file names and an excluded directory
- Filtered entries excluded from record/byte accounting
- Combined entry/scope record thresholds and scope-only thresholds
- UTF-8 scope byte limits just below, exactly at and above the encoded payload
- A total-entry stop after a committed entry, with exact paired durable batches

The test reconstructs the frozen record/byte schedule from each run's actual
persisted scope details and file/header values. It compares every ordered
`(sequence, observed, bytes)` tuple, SQL aggregates, stored report and returned
report. Across engines it compares sequence/record layout, entry bytes, scope
paths/states, and the exact difference contributed by real scope payload sizes.
It retains actual elapsed values and verifies the original source bytes and
unrelated effect tables. The matrix sets `batch_seconds=120` and verifies elapsed
stays below it, isolating byte/record thresholds without changing production time.
Existing standalone Publisher scenarios cover time checkpoints and replacement
accounting; these are bounded copied-debug tests, not installed qualification.

The existing report/maintenance/watch checks now verify ordinary scope bytes and
three real batches while keeping the four known-nonparity witnesses. Five full
report diagnostics retain raw stdout/stderr and must still fail separately.

Initial test mistakes are retained in ignored local evidence: an audit-trigger
fixture was correctly rejected by schema validation and was replaced with a
read-only payload reconstruction; the incomplete-scan CLI test first expected
exit 0 instead of the documented exit 3. Neither failure changed production
validation or reporting behavior.

## Reproduction

Use the pinned local environment, a reused private target, `CARGO_BUILD_JOBS=1`,
`CARGO_INCREMENTAL=0`, debug info disabled and offline Cargo:

```sh
cargo build --offline -p catabolic-cli --bin catabolic-legacy-native
cargo build --offline -p catabolic-python
cargo test --offline -p catabolic-runtime --test scan_reports --test maintenance_orchestration
cargo clippy --offline -p catabolic-cli --bin catabolic-legacy-native -- -D warnings
CATABOLIC_LEGACY_BINARY=PRIVATE_TARGET/debug/catabolic-legacy-native \
  python -m unittest tests.test_rust_scan_accounting tests.test_rust_scan_reports_cli \
  tests.test_rust_maintenance_cli tests.test_rust_scan_watch_cli -v
```

Use the full diagnostic commands in the report/maintenance/watch documents.
The source-bound development receipt records final hashes, terminal checks and
remaining limits. Fresh independent narrow review is required; this file does
not imply that a platform-blocked review has been completed.

## Recorded development result (2026-10-12)

- Combined Python replay: 38 checks pass, including four explicit known-nonparity
  witnesses; no skips or expected failures. After strengthening the scope check
  to compare every non-timing detail field, all eight accounting checks pass again
- Rust: three report tests, five existing maintenance tests and 12 CLI unit tests
  pass; scoped CLI Clippy denies warnings and passes
- Copied-debug Publisher: two selected runtime cases, three source contracts and
  24 exact type-aware frozen paired groups pass
- All five full-report diagnostics still exit 1; their raw output is retained
- Rustfmt, scoped Ruff, owner-ledger freshness, whitespace and source/artifact
  fence rechecks pass. The frozen source checkout is clean
- Independent narrow accounting review is pending at this checkpoint; no broader
  review or milestone status changes

## Independent review and dual-interpreter replay

A fresh independent narrow review of `2784bd4` found no actionable defect.
It replayed 21 named checks and all 24 exact type-aware Publisher groups,
including the equal-payload replacement control (124 real buffered/committed
bytes on each side). It independently verified source, artifact, wrapper and
original evidence hashes and retained a fresh failing scan-text diagnostic.
This closes only that narrow accounting review, not any blocked safety lane.

The eight accounting cases/14 fixture configurations also pass with the full
pinned Python 3.11.15 oracle environment, using the same native executable tested
with Python 3.14.6. Runtime, test-source and executable fences were checked before
and after; the original source/oracle/artifact receipt was reverified afterward.
See the [review/replay receipt](../tests/parity/native-scan-byte-accounting-review.json).
No implementation or test source changed after the reviewed commit.
