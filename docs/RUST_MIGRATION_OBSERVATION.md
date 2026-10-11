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

The runtime observation route now detaches the caller's Store during traversal.
The descriptor-relative walker owns a bounded private SQLite queue and opens a
short writer session for each fenced batch; connection-local scope filters are
reinstalled on every writer. The immutable store entry remains available to
internal synchronous fixtures, while executable/wheel runtime scans use the
mutable detached entry. This does not qualify real mounted/unmounted filesystem
latency or final large-catalog performance.

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
- Full maintenance command/native filesystem event monitoring, all long-running
  supervisor platform acceptance, and every late-result stateful
  trace/fuzz/shrink/replay requirement
- Complete Python `regex.VERSION0` dialect qualification beyond the explicit
  bounded common/lookaround/backreference subset implemented here
- Full scan report/detail parity, projection statistics and resource/performance
  acceptance at the plan's large-catalog scales
- Watcher fallback/projection evaluation and reactions use their shared native
  owners. Projection publication remains restricted to explicitly authorized
  disposable debug calls; real-output and release publication remain gated.
  Broader render-preset, source-lineage and external-service acceptance remains
  separate
- Host timezone recurrence/search-path portability and release packaging/default
  switching remain unqualified


## Detached scheduler extension

The opt-in runtime now includes `observation`, `watchers` and `catalog_refresh`
owners. This is another bounded Linux-container implementation checkpoint, not
final M5/M6 acceptance or permission to enable native real-output mutation.

### Observation and caller custody

- Admission, durable request identity and execution claims still use schema 32.
  One job guard remains owned across detached traversal, publication and finish
- Compatible in-flight/queued work is shared by distinct immutable callers.
  Capacity stops new workers while retaining ordinary queued caller demand;
  caller cancellation never revokes another caller's useful shared work
- Changed dirty generations or bindings re-admit existing immutable requests
  without rewriting their guarantees. Policy changes still require new requests;
  stale claims and changed catalog/source identities cannot publish a late batch
- Native isolated workers use the explicitly registered companion executable.
  A requester timeout or terminated watcher waiter does not kill the observation
  worker. No Python business-logic subprocess is used
- Generated scans snapshot only ready artifact registry paths, require strict
  generated-root ownership and publish explicit missing rows. Registry completion
  is separate from directory absence proof; unregistered generated files are not
  invented as inventory
- Nonempty include/exclude filters normalize bounded lists and use pinned
  `fancy-regex` with bounded backtracking, compile sizes, per-match time checks
  and scope-finalization budget. Strict Python `\Z` is translated to strict
  native `\z`. Unsupported regex-module dialect constructs fail explicitly;
  this is not a claim of every Python regex extension's parity

### Watcher and refresh orchestration

- Native supervisor ticks bound concurrent child processes, child lifetime and
  collected output, wait without a catalog session, and confirm child death
  before recovery. Worker acquisition uses bounded contention retries
- Analysis/render reactions pin all resolved file/item/revision inputs before
  detached tool capability checks, revalidate the run and input revisions before
  admission, and preserve newer requested generations on old completion
- HTTP reactions validate immutable definitions without executing their report
  at definition time. Mapped delivery admission, baseline and pending-reaction
  state commit together. Admission does not claim remote delivery acknowledgment
- Refresh durable settings, queue selection, generation-conditional acknowledgment
  and retry backoff live in `catabolic-store::catalog_refresh`. Runtime plans and
  applies through the shared layout and filesystem owners, rolling staged desired
  mappings back on an unsafe preview. Fallback refresh uses the shared projection
  owner and retains pending demand when the publication gate refuses the call
- Layout refresh/recovery suppresses only legacy refresh-dirty/event generation,
  preserving any pre-existing dirty row. Frozen fallback projection application
  uses a separate default reconciler and retains legacy notifications. Modern publication/consumer generations
  and verification remain active. A single post-batch consumer callback drains
  through the normal detached owner without changing the frozen refresh report
- No destructive-leaf gate, journal record, remount trust rule or hardlink safety
  protocol was relaxed by this work

### Extension qualification

Reproduction inputs and copied executables are under
`.local-tests/rust-migration/m5-scheduler/`. The focused Python modules are
`test_rust_observation_scheduler`, `test_rust_watcher_runtime` and
`test_rust_catalog_refresh`, alongside the prior observation/watcher/remount
regressions. They use disposable catalogs and the frozen Python source only for
fixture setup and independent reference comparisons.

The new scenarios exercise writer progress during actual traversal and tool
waits, shared caller cancellation, dirty/root/catalog fences, immutable request
re-admission, registered-only generated inventory, nonempty regex filtering,
parallel supervisor demand, shared observation surviving supervisor timeout,
processing admission dedupe/failure, atomic unsent HTTP mapping admission and
refresh rollback/backoff/legacy-notification suppression. Native unit tests
exercise deterministic regex exhaustion and stale refresh acknowledgment/backoff.
Full trace/fuzz/shrink/replay and real Linux/macOS mount acceptance remain open.

The extension checkpoint passed **66** focused Python cases in 44.711 seconds,
six observation unit tests and one stale refresh acknowledgment/backoff unit
test. The automatic-consumer regression uses a loopback protocol server that
opens an independent writable Store during each request; it verifies one
acknowledged refresh without adding fields to the frozen refresh report. No
runtime SQL remains in these three new orchestration modules.

Copied debug executable SHA-256:
`f45ec9cee03d7b44b9d82960f3c219d842c969adbf46e2462d8c1fe9458e1f2b`.
Logs are `focused-final.log`, `observation-unit-final.log`,
`refresh-unit-final.log` and `owner-clippy.log` under the checkpoint directory.
The owner Clippy run uses `-A dead-code -D clippy::all` while a separate layout
owner has an unreferenced integration seam; this is not an aggregate strict
workspace-Clippy qualification. The three Python modules pass Ruff.

```sh
source .local-tests/rust-migration/env.sh
cargo build -p catabolic-cli --offline
cp target/debug/catabolic-native .local-tests/rust-migration/m5-scheduler/catabolic-native
CATABOLIC_NATIVE_BINARY="$PWD/.local-tests/rust-migration/m5-scheduler/catabolic-native" \
PYTHONPATH="$PWD/.local-tests/rust-migration/reference/src:$PWD" \
../.local-tools/catabolic-reference-3.14.6/bin/python -W ignore::ResourceWarning -m unittest \
  tests.test_rust_observation_scheduler tests.test_rust_watcher_runtime \
  tests.test_rust_catalog_refresh tests.test_rust_watchers \
  tests.test_rust_observation_filesystem tests.test_rust_remount
cargo test -p catabolic-store observation::tests --offline
cargo test -p catabolic-store catalog_refresh::tests --offline
```

### Fallback/projection orchestration extension

Fallback watchers now resolve through one bounded native Evaluation, preserve
their previous semantic baseline and health namespace, and commit no baseline
for incomplete evidence. Projection report/event watchers evaluate without
publishing. Projection reactions retain the exact typed prepared plan under the
same publication guard across baseline admission and apply; they do not replan
after acknowledging the evaluation. Existing watcher generation fences still
leave newer caller demand pending.

Projection automation ownership follows the frozen explicit-transfer rules.
Enabling an owner disables legacy automation without deleting queued refresh
work; conflicting owners refuse. Pinned projection binding digests are checked
before definition/run admission and after detached preparation.

An ordinary projection-reaction run is refused before creating a watcher claim.
Only a debug call explicitly carrying `disposable_fixture: true` may publish;
the supervisor forwards this permission only in that call's child payload, never
as stored watcher configuration. The next ordinary tick remains gated. Automatic
producer finish does not infer this permission, so fallback refresh remains queued
with a retry error unless an explicit disposable test call performs it. No
journal format, destructive-leaf protocol or release-output gate was changed.

`tests.test_rust_watcher_projection` independently compares frozen normalized
definitions, fallback event baselines/failover and projection report baselines.
It also checks incomplete evidence, stale binding refusal, ownership transfer,
pre-claim publication refusal, per-tick fixture propagation, exact prepared-plan
publication, resolved/unresolved processing reactions and gated refresh recovery.
A separate real disposable Python/native differential verifies the fallback
legacy-notification distinction described above. These are bounded fixture checks, not
complete M5/M6 state-space or platform qualification.

The combined extension qualification passed **79 tests in 62.680 seconds**:
the prior 66-case set, ten new watcher/projection cases and three existing plan
cases. The tested copied executable is
`.local-tests/rust-migration/m5-scheduler/catabolic-projection`, SHA-256
`019be25f9d90fc50de6efdce993369df9bd8b84e7cf6145740c92329e0377afc`.
See `projection-focused-final.log` and `fallback-refresh-notify.log` in that
directory. Add `tests.test_rust_watcher_projection tests.test_rust_plans` to
the preceding reproduction command and select this executable. Ruff/rustfmt and
diff checks pass. Strict runtime all-target Clippy currently stops at the shared
filesystem owner's new, not-yet-connected `published_mut`/`verify_mut` wrappers;
no suppression or removal of those integration seams was made here.
