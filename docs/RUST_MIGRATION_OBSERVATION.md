# Native observation and output checkpoint

This is a bounded implementation and Linux-container qualification checkpoint,
not completion of M5 or M6. Python remains the production default. All filesystem
fixtures are disposable. No real user catalog or source media was used.

## Implemented owners

- `catabolic-store::observation`: binding, strict/path/per-check trust histories,
  scan policies, descriptor-relative guarded scans, durable request/job claims,
  source/DB/root pins, dirty-generation fences, bounded SQLite traversal queues,
  durable batches and scope-specific absence, fresh barriers, completed-evidence
  reuse, explicit continuation and interrupted scan resumption, caller-local
  cancellation and recovery
- `catabolic-store::remount`: exact reviewed plan digests, explicit named source
  replacement, stable-volume renumbering, atomic binding/consumer/publication
  updates and audit records, inventory-only repair and automatic device-number
  recovery without legacy adoption
- `catabolic-store::filesystem_mutation`: no-follow descriptor ownership checks,
  empty-output marker claims, symlink preview/apply/recovery, exact frozen journal
  rows, creates-before-removals across catalogs, removal budgets, source-health
  and currentness checks, publication generation/refresh updates, and integration
  with the separately owned native hardlink implementation
- `catabolic-store::watcher`: immutable definitions, numeric cron/manual/interval
  schedules, demand and debounce admission, claims and generation fences,
  observation-only and query report/event runs, baselines, retries and dead-owner
  recovery. Typed query evaluation uses the shared native plans owner
- `catabolic-core::coordination`: pure production claim generation, resume,
  lease/fence and caller-local cancellation decisions. The actual observation
  and watcher transactions call this reducer before applying accepted effects

The current scan orchestration still owns one synchronous writer session while
traversing, although batches commit independently. It is not the detached,
short-writer multiworker observation runtime required for final M5 acceptance.

## Open destructive-leaf safety gate

The frozen symlink protocol checks an existing leaf and then calls `renameat` or
`unlinkat`. Those are not conditional-on-inode operations. Deterministic hooks
inside the production effect path reproduced both races: an external regular
file swapped after the final check can be overwritten or removed. Characterizing
that behavior is not safety qualification.

Native preview/apply/recover therefore reject symlink `replace` and `remove`
unless running a debug disposable-fixture test. The `disposable_fixture: true`
exception is compiled out of optimized release builds. A normal debug request
without that explicit flag also fails closed. Release recovery preserves the
journal and refuses those effects. Create/unchanged/forget and the separately
qualified hardlink owner are not enabled by this exception.

No native-only retained-leaf journal state was introduced. A safe compatible
protocol, or an explicitly coordinated reference/schema change, remains an open
M6 requirement. Cross-language destructive symlink crash tests below only apply
to the debug disposable-fixture protocol, not a release safety claim.

The broader same-user adversary that replaces private marker-temporary names
between a type check and cleanup also remains outside qualification. The current
release gate addresses the two reproduced published-leaf effects; it is not a
claim that every marker/temp cleanup race has been eliminated.

## Evidence and reproduction

The final focused debug-artifact run passed 49 Python differential/scenario tests.
Native checks passed 5 observation, 2 remount, 6 filesystem and 3 coordination
unit tests; one filesystem check deliberately characterizes the open race rather
than claiming safety. The optimized release artifact passed all 10 fail-closed
cases, including attempts to set the disposable-fixture flag. Its SHA-256 is
`186c9d30cc1fe2bf3db20709819c705a2de43527e6c9007a1fb5dd17004f6047`.
The same gate script rejects the debug opt-in artifact as a negative control.

Evidence, copied artifacts and the focused source/scenario register are retained
under `.local-tests/rust-migration/m5-m6-native/`. Ruff passes for the three test
modules and release-gate script. Strict Clippy reports no diagnostics in these
owners; the aggregate store check was blocked by a sibling artifact-owner lint,
so this checkpoint does not claim the full workspace Clippy gate.

`tests/test_rust_observation_filesystem.py` uses actual native subprocesses and
Python reference fixtures. It includes unchanged reference reconciliation
assertions, exact persisted observation/header comparisons, failed/deferred and
excluded scope retention, root replacement and dirty fences, selected/full
continuation, shared caller cancellation, Python-to-native and native-to-Python
journal recovery, actual process-exit scan resumption, forged journal identity
refusal, and fail-closed default destructive-leaf admission.

`tests/test_rust_watchers.py` compares normalized definition digests, history,
manual/scheduled admission, debouncing/retry/lease rules, baseline/event behavior,
shared inventory reuse, observation-only runs, dead-owner recovery and numeric
cron/DST behavior against the Python reference.

`tests/test_rust_remount.py` compares exact source-only preview digests, reviewed
apply results and persisted state; stale plans and unknown replacements preserve
observations. Automatic recovery respects skipped device checks and cannot
adopt an unknown volume. Injected volume-identity Rust tests cover stable UUID
renumbering. These are not real mount/unmount qualification.

The production coordination reducer's bounded independent-ledger graph covers
52 states and 536 transitions with no skipped transitions. It checks issued
proofs after expiry/takeover and caller cancellation preserving shared work.
This is a unit-level graph, not the final Statelessness trace/fuzz/shrink/replay
acceptance artifact.

Run focused qualification with a copied binary to avoid concurrent Cargo
relinking of the executable:

```sh
cargo build -p catabolic-cli --offline
cp target/debug/catabolic-native /tmp/catabolic-observation-checkpoint
CATABOLIC_NATIVE_BINARY=/tmp/catabolic-observation-checkpoint \
  .venv/bin/python -m unittest tests.test_rust_observation_filesystem \
  tests.test_rust_watchers tests.test_rust_remount
cargo test -p catabolic-core coordination --offline -- --nocapture
cargo test -p catabolic-store characterize_unresolved_external_writer_leaf_race \
  --offline -- --nocapture
cargo build --release -p catabolic-cli --offline
.venv/bin/python scripts/check_native_output_gate.py target/release/catabolic-native
```

The last command checks both opt-in values against release preview/apply and
recovery, preserving the original leaf, journal and source bytes. It emits the
actual tested binary's SHA-256.

## Timezone dataset boundary

This container's Python `ZoneInfo` reads host IANA data 2026b; pinned `chrono-tz`
contains 2025b. Native schedules therefore read the same bounded host TZif files
instead of silently using older political rules. Differential cases include
America/Asuncion, America/Coyhaique, Africa/Casablanca, New York's spring gap and
fall fold, London and Kathmandu. Repeated fold occurrences are skipped, and
restricted day-of-month/day-of-week fields use AND semantics.

Recurring dates beyond the host TZif explicit transition horizon fail closed
when host and bundled dataset versions differ. Full POSIX recurrence, alternate
host timezone search paths and macOS timezone qualification remain open.

## Remaining gates

- Safe compatible symlink destructive-leaf protocol, real mounted/unmounted,
  permission/full-disk and adversarial filesystem acceptance on Linux and macOS
- Detached short-writer scans, cross-process concurrent caller scheduling,
  complete watcher supervisor/maintenance orchestration and every late-result
  stateful trace/fuzz/shrink/replay requirement
- Generated-artifact inventory and bounded Python-compatible nonempty scan regex
- Automatic re-admission after changed binding/dirty generation for an existing
  immutable request; current native API explicitly requires a new request
- Full scan report/detail parity, projection statistics and resource/performance
  acceptance at the plan's large-catalog scales
- Watcher fallback/projection evaluation and processing/render/HTTP/projection
  reaction families remain explicitly refused. Runtime render capability waits
  need a mutable detached Store boundary before admission can be shared safely
- Host timezone recurrence/search-path portability and release packaging/default
  switching remain unqualified
