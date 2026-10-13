# Preserved fallback worker lifecycle checkpoint

Reference: `453fca983222c6665a48775eef67c489c96b527e`, Python 3.14.6.
This is a narrow ordinary CLI/worker transport checkpoint. Python remains the
default. The `fallback worker` leaf is **partial**; no M4–M11 hard gate closes.

## Existing ownership and added transport

The frozen `fallback_cli.run` delegates directly to `fallback_worker.run` for this
leaf. It exposes only `--once`, in addition to global database/profile/output
options. The run owner resolves the database, takes the database-wide
`<database>.fallback-worker.lock` singleton, and then calls a bounded tick. Tick
opens one writable Store, validates the profile, selects ten enabled due bindings
ordered by `next_attempt,catalog`, skips watcher-owned catalogs, attempts recovery
when a journal exists, runs automatic fallback projection application, records the
per-catalog result, and advances `next_attempt` by the configured interval.

The frozen run owner converts catalog writer/SQLite contention to
`{processed: 0, complete: false, blockers: [catalog_busy]}`. Singleton contention
is terminal and precedes profile validation. `--once` returns that report; the
perpetual worker sleeps one second after each tick and emits no intermediate
stdout, even for incomplete results. The catalog writer ends with the tick;
the singleton ends with the whole worker.

These native business owners already existed before this change:

- `catabolic_runtime::public_fallback::tick`: scheduler selection, watcher-owner
  skipping, short Store ownership, recovery/application admission, per-catalog
  result and retry scheduling
- `public_fallback::call(projection-run)` and native fallback/projection owners:
  planning/application entrypoints, with the existing public M6 refusal
- `catabolic_store::filesystem_mutation::execute_mut(recover)`: existing recovery
  entrypoint, with its existing public M6 refusal
- `catabolic_store::{database_path,writer_lock}`: path admission and owning locks
- `catabolic_http::interruptible`: installed native signal bridge

The added `catabolic_runtime::public_fallback::run` supplies only singleton and
loop lifecycle around that existing tick. `legacy::fallback_worker::run` connects
it to the preserved command dispatcher and signal presentation. No Python
business implementation, private fixture flag, helper registration or alternate
publication path is introduced. A cancellation is presented only after the
runtime owner has dropped its writer and singleton.

The CLI presents exact `processed,complete,projections` order and the existing
text/JSON/machine envelopes. Only this worker's exact singleton contention
message gains the frozen `CatalogBusy` type; ordinary command errors remain
`CatabolicError`. Incomplete reports use exit 3, ordinary failures 2, successful
empty/skipped reports 0 and handled interruption 130.

## Focused executable evidence

`tests/test_rust_legacy_fallback_worker.py` uses separate copies of disposable
catalogs seeded through the frozen owners. There are no source/output bindings,
media, real credentials or remote services. Ten test methods cover:

- Exact text/JSON/machine stdout, stderr and exits for empty work, unknown profile,
  missing database, database writer contention and singleton contention
- Contention precedence over unknown profile, SQLite exclusive-lock contention,
  release after an error and successful retry
- Disabled/future/watcher-owned skipping without rescheduling, ten-row selection,
  persistent retry timestamps, and reopening for remaining then empty work
- The unchanged native apply/recovery refusals, preserved pending journal and
  absence of output/link/history effects; the public parser refuses a private
  fixture override
- Real SIGINT after an observed tick/idle boundary in all three output modes,
  a simultaneous external writer, cross-profile singleton exclusion, released
  owners after exit, and restart without repeating the deferred work
- A separately asserted SIGTERM difference and owner release/restart

The frozen SIGINT helper traces the actual `time.sleep(interval)` line and waits
for a real signal. It never replaces a business function. Native readiness uses
the tick's committed retry timestamp and an independently acquired writer lock.
These are selected schedules, not exhaustive cancellation qualification.

Three new runtime tests cover once/reopen, contention admission order and
pre-tick cancellation release. All 14 existing public fallback owner tests and
nine legacy binary unit tests also pass. The five source-ledger tests and two
existing CLI fallback/error regressions pass. Scoped Clippy with warnings denied,
Rust formatting, Ruff and ledger regeneration checks pass.

The source/artifact receipt is
[`tests/parity/native-fallback-worker-checkpoint.json`](../tests/parity/native-fallback-worker-checkpoint.json).
Ignored logs and the copied debug binary are under
`.local-tests/fallback-worker/` in the owner worktree. The first Python replay
failed because the new diagnostic test assumed a frozen per-catalog error where
the real oracle returned incomplete empty states; the retained diagnostic and
corrected test explicitly record the actual difference. No expected-failure or
skipped test is counted as a pass.

## Explicit non-parity and remaining contract

1. **Public apply/recovery remains refused.** For the due empty projection seed,
   frozen Python increments generation and returns `complete:false,states:[]`.
   Native leaves generation unchanged and returns the existing M6 refusal as
   `error`. Both use exit 3 and schedule retry. This is a tested difference, not
   equivalent publication behavior. Recovery keeps the pending native journal.
2. **SIGTERM differs.** The shared native bridge yields exit 130 and the preserved
   KeyboardInterrupt envelope; frozen Python terminates with SIGTERM (process
   return code -15 in the fixture) and no output. SIGINT is compared exactly at
   the selected boundary.
3. Native Store errors currently flatten SQLite codes into text. The wrapper
   recognizes only exact known writer/SQLite contention diagnostics. Extended
   `SQLITE_LOCKED` messages and the complete frozen exception-class matrix are
   not qualified. No broad string or error-category redesign is included.
4. Runtime tick's existing integer limit range 0..1000 remains unchanged. The CLI
   exposes no limit and uses the frozen default ten. The documented native
   Python facade still selects configure/tick only; this CLI wrapper does not
   implement the preserved Python `fallback_worker.run` facade.
5. There is no installed-wheel, release-profile, macOS or full-leaf receipt here.
   Arbitrary interruption boundaries, database replacement/error races, live
   failover/failback, all component/rendition families, applied output publication
   and complete cross-platform recovery remain dependencies of wider migration
   qualification. The two blocked broad safety-review lanes were not retried.

Full worker equivalence now depends on those existing native owner gates and
qualification contracts, rather than absence of a worker CLI wrapper.

## Later selected SIGTERM boundary checkpoint

The historical signal evidence above is retained. The later
[selected legacy SIGTERM checkpoint](RUST_MIGRATION_LEGACY_SIGTERM.md) aligns
standalone TERM exit/no-new-output presentation for rule `--worker`, fallback
worker and top-level watch only after existing native cleanup finishes. Private
Python callable and shared HTTP signal behavior remain unchanged. This does not
close the other documented gaps or promote any leaf or migration gate.
