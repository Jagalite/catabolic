# Ordinary native Publisher adapter

## Scope and ownership

This local slice adds `catabolic_native.scan_execution.Publisher` with the seven
preserved class/method rows from the frozen Python inventory. It remains opt-in.
Python remains the default, and no M5, M6, M10 or release gate is closed here.

The adapter is connected to real `Application.scan`, including ordinary
`Publisher.directory`, `checkpoint` and `_flush` monkeypatches. The instance
retains the exact caller Application/Store and native binding presentation.
Python contains argument/value conversion and callback dispatch only. It does
not implement publication SQL, claims, source checks or a second publisher.

`catabolic-store::observation::Publisher` now holds the production publication
state extracted from `Scan`: execution/identity context, batch sequence, entries,
scopes, byte accounting, root identity and the existing transaction/fence/refresh
operations. The scanner and the public adapter share this owner. Callback-backed and generated
scans retain their existing per-scope flush schedule and byte accounting; public
standalone instances buffer ordered scopes into one atomic transaction using the
frozen record/byte/time checkpoint behavior. Successful flush clears the buffer;
a failed transaction retains it for an explicit retry.

Construction reads the caller's existing Store, including an uncommitted
snapshot. The copied initial-scope snapshot is a native temporary SQLite owner,
read through bounded 256-row pages. A Publisher's state can survive a callback
or caller Store lifetime without borrowing a Python SQLite connection. A native
scan still owns its ordinary execution guard until completion or failure.

Python callbacks run without holding a Publisher mutex or borrowing its mutable
state. The outer Store is checked out and detached during traversal; reentrant
calls through that same Store refuse, while independent readers/writers can see
completed batches. Native flush errors carry private exception provenance so
they return through native outcome handling. User callback exceptions, including
wrapped OSError/sqlite3 exceptions, retain the original Python object after
claim cleanup and Store reacquisition. Factory-delivery failures use the same
cleanup boundary.

## Deliberate limits

- Explicit `status="missing"` entries for non-generated sources refuse before
  buffering or database effects with: `native public Publisher explicit missing
  entries for non-generated sources are unqualified`. Frozen Publisher re-stats
  such entries; the existing native traversal emits them only for generated
  sources. This slice neither duplicates that source policy nor silently
  exposes unverified absence. The refusal applies to direct and retained real
  scanner handles and is a refusal witness, not paired parity
- Callback-backed batch scheduling differs from frozen Python's complete scan
  report and remains unchanged. Ordinary no-callback scheduling/accounting is
  covered separately by [scan accounting](RUST_MIGRATION_SCAN_ACCOUNTING.md).
  This gate checks each path's actual ledger and retains their count difference;
  it does not establish full report equivalence
- Buffer/options properties are read-only value snapshots. Direct mutations of
  the frozen instance's lists/dicts, its raw SQLite `snapshot` connection, and
  manual `last` timestamp manipulation are not supported. Retained native
  scanner handles retain their readable snapshot until the owner is dropped;
  that is not an exact claim about frozen internal snapshot-close timing
- Legacy Python Store sharing, exhaustive malformed values/exception diagnostics,
  generated-registry combinations, all interruption schedules, platform parity,
  performance and specialized scan failure reports remain unqualified
- No broad filesystem/source-access/remount/watcher safety review, managed-output
  publication/recovery, hardlink, source-identity policy or journal redesign is
  part of this work. Existing safety gates remain in place

## Selected evidence

On Linux, the copied debug extension passed **eleven required cases and 24 exact,
type-aware frozen paired groups on Python 3.14.6 and 3.11.15**. The reference is
commit `453fca983222c6665a48775eef67c489c96b527e` on pinned Python 3.14.6. All seven
selected inventory rows are signature-checked and executed. Evidence includes:

- Constructor identity, same-owner uncommitted snapshot, snapshot paging and
  stability across rollback/close
- Filtered entries, record/byte accounting, record/scope/time checkpoints,
  repeated scope replacement, stop and successful clearing
- One atomic multi-scope batch, real SQLite constraint rollback and explicit retry
- Real scan reader visibility and directory/_flush consumer hooks
- Original callback/factory exception identity, retained owners, claim cleanup,
  reentrant refusal and subsequent Store/scan reuse
- Separately measured ordinary versus Python-transport scheduling and the explicit-missing refusal

Eighteen source/CI/receipt-boundary methods pass separately on both Python
versions. Targeted Rust Clippy and Python formatting/lint pass. The first paired
fixture saved live reference lists, which later flushes cleared; it was corrected
to capture both engines by value. The original mismatch and failed receipt-test
attempt are retained as development evidence, not relabeled passes.

These are source-bound **copied-debug development results**, not installed-wheel
or release qualification. The strict installed gate is implemented and required
by the Linux workflow, but has not been run for this branch. It requires the
same-source release wheel/helper/both external CLIs, immutable installed payload,
frozen oracle, fixed no-skip case/group counts and before/after fences. Run it
outside the checkout after the unified release build:

```sh
/path/to/installed/python -I -B /checkout/scripts/native_publisher_embedding_tests.py \
  --wheel /artifacts/catabolic_native.whl --build-manifest /artifacts/build-manifest.json \
  --source-root /checkout --inventory /checkout/tests/parity/python-api.json \
  --reference-root /frozen-reference --reference-python /pinned-3.14.6/python \
  --report /artifacts/native-publisher-embedding.json
```

The first independent narrow review reproduced a child-directory callback error
whose native NOT NULL message was replaced by a generic callback diagnostic.
The correction propagates that error before ordinary per-file aggregation. The
new deterministic regression fails against the earlier debug extension and also
checks child-boundary RuntimeError, KeyboardInterrupt, OSError and sqlite3
exception identity, claim cleanup and Store reuse. The earlier artifact and
before/after repro logs are retained separately.

Fresh review of that correction and integrated-source replay remain required. No push, package publication, release, merge or default switch occurred.
